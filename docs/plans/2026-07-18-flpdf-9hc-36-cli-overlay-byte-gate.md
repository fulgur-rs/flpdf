# CLI overlay/underlay byte-identity gate Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** library の `overlay::byte_gate` 相当 19 シナリオを `flpdf` CLI binary 経由で qpdf golden と byte-identical に一致することを検証する gated test 群を追加し、argv 解析・`WriteOptions` 組立・CLI defaults の wiring 起因の乖離を捕捉する。

**Architecture:** 新規テストファイル `crates/flpdf-cli/tests/cli_byte_identical_overlay.rs` を `qpdf-zlib-compat` feature gate 下に追加。既存 goldens (`tests/golden/references/overlay/`) と fixtures (`tests/fixtures/compat/`) を再利用し、`assert_cmd` で `flpdf rewrite --static-id [--qdf --no-original-object-ids] DEST --overlay/--underlay SRC [...] -- OUT` を実行して golden と bytes-compare。CI (`.github/workflows/ci.yml`) の bytes-identical zlib compat step に明示列挙。library `overlay.rs:1148-1152` の deferral コメントを実装済み表記に更新。

**Tech Stack:** Rust (2021), `assert_cmd`, `tempfile`, `flpdf` の既存 `overlay::byte_gate` を範型に。既存 `crates/flpdf-cli/tests/cli_byte_identical.rs` の linearize gate と同構造。

**Beads issue:** flpdf-9hc.36

**Worktree:** `.worktrees/flpdf-9hc-36-cli-overlay-byte-gate` / branch `feat/flpdf-9hc-36-cli-overlay-byte-gate`

---

## 前提の確認

**CLI defaults の確認 (実装前に確認済み):**
- `flpdf rewrite` は overlay presence で `needs_mutation ⇒ options.full_rewrite = true` (crates/flpdf-cli/src/main.rs:3082-3091)。
- `NewlineBeforeEndstream::Never` は CLI default (flpdf-9hc.33 完了、`bd recall flpdf-cli-newline-before-endstream-default-is-now`)。
- したがって `flpdf rewrite --static-id DEST --overlay SRC ... OUT` は library `write_static_id`(`full_rewrite=true, static_id=true, newline=Never`) と同じ `WriteOptions` を生成する。
- `--qdf --no-original-object-ids` 追加で `write_qdf_nooid` と一致。

**Golden 対応:** `tests/golden/references/overlay/*.pdf` は `qpdf --static-id --warning-exit-0 [...]` で生成 (`tests/golden/regenerate.sh:1943-2054` plain + `:2055-2085` QDF)。

---

### Task 1: 新規テストファイル scaffold + smoke test

**Files:**
- Create: `crates/flpdf-cli/tests/cli_byte_identical_overlay.rs`

**Step 1: 新規ファイル作成 (helpers + Test #1)**

`crates/flpdf-cli/tests/cli_byte_identical.rs` を範型とし、overlay 用 helper と 1 つ目の test (`cli_three_page_overlay_one_page_is_byte_identical`) を書く。

```rust
//! End-to-end byte-identity: the `flpdf` CLI binary + `--overlay`/`--underlay`
//! == qpdf 11.9.0 output.
//!
//! Mirrors the library-layer `overlay::byte_gate` (in `crates/flpdf/src/overlay.rs`)
//! but runs the actual `flpdf` binary through `rewrite --static-id [--qdf
//! --no-original-object-ids] DEST --overlay SRC [--from=..] [--to=..] [--repeat=..]
//! -- OUT`. This exercises the whole CLI path — raw-argv pre-split (`extract_overlay_groups`),
//! `WriteOptions` assembly (incl. the `overlay-presence ⇒ full_rewrite=true` promotion),
//! CLI defaults (`NewlineBeforeEndstream::Never`), and the write pipeline — so a
//! divergence introduced by the CLI layer (not just the library) is caught.
//!
//! Gated on `qpdf-zlib-compat`: byte identity requires flpdf's DEFLATE to match
//! qpdf's classic-zlib output. The default (miniz_oxide) build compiles these
//! out — the only sanctioned byte deviation per the project's mimicry policy.

#![cfg(feature = "qpdf-zlib-compat")]

use assert_cmd::Command;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(name)
}

fn overlay_golden(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references/overlay")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read golden {path:?}: {e}"))
}

/// Run `flpdf rewrite --static-id [extra_head...] <dest> <argv...> <out>` and
/// return the written bytes. `argv` should terminate each overlay/underlay
/// group with `--`, mirroring the qpdf CLI shape captured in
/// `tests/golden/regenerate.sh`.
fn run_cli(extra_head: &[&str], dest: &str, argv: &[&str]) -> Vec<u8> {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = tmp.path().join("out.pdf");
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.env("FLPDF_STATIC_ID_QUIET", "1");
    cmd.arg("rewrite").arg("--static-id");
    for a in extra_head {
        cmd.arg(a);
    }
    cmd.arg(fixture(dest));
    for a in argv {
        cmd.arg(a);
    }
    cmd.arg(&out);
    cmd.assert().success();
    std::fs::read(&out).unwrap_or_else(|e| panic!("read out: {e}"))
}

fn assert_bytes(actual: &[u8], golden_name: &str) {
    let expected = overlay_golden(golden_name);
    if actual == expected {
        return;
    }
    let common = actual.len().min(expected.len());
    let off = (0..common).find(|&i| actual[i] != expected[i]).unwrap_or(common);
    let lo = off.saturating_sub(24);
    panic!(
        "{golden_name}: CLI overlay output diverged from qpdf golden \
         (flpdf={} bytes, golden={} bytes, first diff at byte {off})\n\
         flpdf : {:?}\ngolden: {:?}",
        actual.len(),
        expected.len(),
        String::from_utf8_lossy(&actual[lo..(off + 24).min(actual.len())]),
        String::from_utf8_lossy(&expected[lo..(off + 24).min(expected.len())]),
    );
}

// ── Plain static-id: three-page dest × one-page source (identity cm) ─────────

#[test]
fn cli_three_page_overlay_one_page_is_byte_identical() {
    let src = fixture("one-page.pdf");
    let bytes = run_cli(&[], "three-page.pdf", &["--overlay", src.to_str().unwrap(), "--"]);
    assert_bytes(&bytes, "three-page-overlay-one-page.pdf");
}
```

**Step 2: baseline としてこの test を実行 (PASS 期待)**

Run:
```bash
cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_byte_identical_overlay -- --nocapture
```
Expected: `1 passed`. FAIL の場合は CLI-vs-library の wiring 乖離が実在するサイン。次 task 前に切り分けが必要 (advisor 呼ぶ)。

**Step 3: Commit**

```bash
git add crates/flpdf-cli/tests/cli_byte_identical_overlay.rs
git commit -m "test(flpdf-cli): scaffold CLI overlay byte-identity gate with first case

Mirrors library overlay::byte_gate through the flpdf binary to catch CLI-layer
divergences (argv parsing, WriteOptions assembly, defaults). First case:
three-page dest + one-page overlay + --static-id.

Refs: flpdf-9hc.36"
```

---

### Task 2: Plain static-id batch A (page-range 系: tests #2-8)

**Files:**
- Modify: `crates/flpdf-cli/tests/cli_byte_identical_overlay.rs` (append 7 tests)

**Step 1: 7 tests 追加** (`three-page` dest ベース、`--from`/`--to`/`--repeat` 変種 + 複数 overlay 合成 + overlay+underlay 合成)

```rust
#[test]
fn cli_three_page_overlay_two_page_is_byte_identical() {
    let src = fixture("two-page.pdf");
    let bytes = run_cli(&[], "three-page.pdf", &["--overlay", src.to_str().unwrap(), "--"]);
    assert_bytes(&bytes, "three-page-overlay-two-page.pdf");
}

#[test]
fn cli_three_page_overlay_one_page_repeat1_is_byte_identical() {
    let src = fixture("one-page.pdf");
    let bytes = run_cli(
        &[],
        "three-page.pdf",
        &["--overlay", src.to_str().unwrap(), "--repeat=1", "--"],
    );
    assert_bytes(&bytes, "three-page-overlay-one-page-repeat1.pdf");
}

#[test]
fn cli_three_page_overlay_two_page_to_2_3_is_byte_identical() {
    let src = fixture("two-page.pdf");
    let bytes = run_cli(
        &[],
        "three-page.pdf",
        &["--overlay", src.to_str().unwrap(), "--to=2-3", "--"],
    );
    assert_bytes(&bytes, "three-page-overlay-two-page-to2-3.pdf");
}

#[test]
fn cli_three_page_two_overlays_compose_is_byte_identical() {
    let a = fixture("one-page.pdf");
    let b = fixture("two-page.pdf");
    let bytes = run_cli(
        &[],
        "three-page.pdf",
        &[
            "--overlay", a.to_str().unwrap(), "--",
            "--overlay", b.to_str().unwrap(), "--",
        ],
    );
    assert_bytes(&bytes, "three-page-two-overlays.pdf");
}

#[test]
fn cli_three_page_overlay_and_underlay_compose_is_byte_identical() {
    let a = fixture("one-page.pdf");
    let b = fixture("two-page.pdf");
    let bytes = run_cli(
        &[],
        "three-page.pdf",
        &[
            "--overlay", a.to_str().unwrap(), "--",
            "--underlay", b.to_str().unwrap(), "--",
        ],
    );
    assert_bytes(&bytes, "three-page-overlay-and-underlay.pdf");
}

#[test]
fn cli_three_page_overlay_two_page_from2_is_byte_identical() {
    let src = fixture("two-page.pdf");
    let bytes = run_cli(
        &[],
        "three-page.pdf",
        &["--overlay", src.to_str().unwrap(), "--from=2", "--"],
    );
    assert_bytes(&bytes, "three-page-overlay-two-page-from2.pdf");
}

#[test]
fn cli_three_page_overlay_two_page_from_empty_repeat2_is_byte_identical() {
    let src = fixture("two-page.pdf");
    let bytes = run_cli(
        &[],
        "three-page.pdf",
        &["--overlay", src.to_str().unwrap(), "--from=", "--repeat=2", "--"],
    );
    assert_bytes(&bytes, "three-page-overlay-two-page-from-empty-repeat2.pdf");
}
```

**Step 2: 全 8 tests を実行 (PASS 期待)**

Run:
```bash
cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_byte_identical_overlay
```
Expected: `8 passed`.

**Step 3: Commit**

```bash
git add crates/flpdf-cli/tests/cli_byte_identical_overlay.rs
git commit -m "test(flpdf-cli): add page-range and composition CLI overlay byte gates

Covers --from/--to/--repeat variants, two --overlay composition, and
overlay+underlay composition end-to-end through the flpdf binary.

Refs: flpdf-9hc.36"
```

---

### Task 3: Plain static-id batch B (fixture 系: tests #9-16)

**Files:**
- Modify: `crates/flpdf-cli/tests/cli_byte_identical_overlay.rs` (append 8 tests)

**Step 1: 8 tests 追加** (underlay 単独 + rotated source + `--to --repeat` 合成 + multi-stream/userunit/rotated-dest/swapped-box/swapped-box-r90 各 fixture)

```rust
#[test]
fn cli_three_page_underlay_two_page_is_byte_identical() {
    let src = fixture("two-page.pdf");
    let bytes = run_cli(
        &[],
        "three-page.pdf",
        &["--underlay", src.to_str().unwrap(), "--"],
    );
    assert_bytes(&bytes, "three-page-underlay-two-page.pdf");
}

#[test]
fn cli_three_page_overlay_rotated_source_is_byte_identical() {
    let src = fixture("one-page-r90.pdf");
    let bytes = run_cli(&[], "three-page.pdf", &["--overlay", src.to_str().unwrap(), "--"]);
    assert_bytes(&bytes, "three-page-overlay-rotated.pdf");
}

#[test]
fn cli_three_page_overlay_to_1_3_repeat1_is_byte_identical() {
    let src = fixture("one-page.pdf");
    let bytes = run_cli(
        &[],
        "three-page.pdf",
        &["--overlay", src.to_str().unwrap(), "--to=1-3", "--repeat=1", "--"],
    );
    assert_bytes(&bytes, "three-page-overlay-to-repeat.pdf");
}

#[test]
fn cli_three_page_overlay_multi_stream_source_is_byte_identical() {
    let src = fixture("multi-stream-one-page.pdf");
    let bytes = run_cli(&[], "three-page.pdf", &["--overlay", src.to_str().unwrap(), "--"]);
    assert_bytes(&bytes, "three-page-overlay-multi-stream.pdf");
}

#[test]
fn cli_r90_dest_overlay_one_page_is_byte_identical() {
    let src = fixture("one-page.pdf");
    let bytes = run_cli(&[], "one-page-r90.pdf", &["--overlay", src.to_str().unwrap(), "--"]);
    assert_bytes(&bytes, "r90-dest-overlay-one-page.pdf");
}

#[test]
fn cli_three_page_overlay_userunit_source_is_byte_identical() {
    let src = fixture("userunit-one-page.pdf");
    let bytes = run_cli(&[], "three-page.pdf", &["--overlay", src.to_str().unwrap(), "--"]);
    assert_bytes(&bytes, "three-page-overlay-userunit.pdf");
}

#[test]
fn cli_swapped_box_overlay_one_page_is_byte_identical() {
    let src = fixture("one-page.pdf");
    let bytes = run_cli(
        &[],
        "swapped-box-one-page.pdf",
        &["--overlay", src.to_str().unwrap(), "--"],
    );
    assert_bytes(&bytes, "swapped-box-overlay-one-page.pdf");
}

#[test]
fn cli_swapped_box_r90_overlay_self_is_byte_identical() {
    let src = fixture("swapped-box-r90-one-page.pdf");
    let bytes = run_cli(
        &[],
        "swapped-box-r90-one-page.pdf",
        &["--overlay", src.to_str().unwrap(), "--"],
    );
    assert_bytes(&bytes, "swapped-box-r90-overlay-self.pdf");
}
```

**Step 2: 全 16 tests を実行**

Run:
```bash
cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_byte_identical_overlay
```
Expected: `16 passed`.

**Step 3: Commit**

```bash
git add crates/flpdf-cli/tests/cli_byte_identical_overlay.rs
git commit -m "test(flpdf-cli): add fixture-variant CLI overlay byte gates

Covers underlay, rotated source, multi-stream source, r90 dest, /UserUnit
source, and reversed-page-box (swapped-box) fixtures end-to-end through the
flpdf binary.

Refs: flpdf-9hc.36"
```

---

### Task 4: QDF batch (tests #17-19)

**Files:**
- Modify: `crates/flpdf-cli/tests/cli_byte_identical_overlay.rs` (append 3 tests)

**Step 1: 3 tests 追加** (`--qdf --no-original-object-ids` を `extra_head` 経由で挿入)

```rust
// ── QDF variants: --qdf --no-original-object-ids ─────────────────────────────
//
// Mirrors the library `overlay::byte_gate::write_qdf_nooid` recipe. The
// full-rewrite promotion still comes from overlay-presence; --qdf adds the
// QDF writer path (uncompressed streams, object-number relaying, xref table
// form) and --no-original-object-ids reissues object numbers sequentially.

const QDF: &[&str] = &["--qdf", "--no-original-object-ids"];

#[test]
fn cli_three_page_overlay_one_page_qdf_is_byte_identical() {
    let src = fixture("one-page.pdf");
    let bytes = run_cli(QDF, "three-page.pdf", &["--overlay", src.to_str().unwrap(), "--"]);
    assert_bytes(&bytes, "three-page-overlay-one-page-qdf.pdf");
}

#[test]
fn cli_three_page_overlay_and_underlay_qdf_is_byte_identical() {
    let a = fixture("one-page.pdf");
    let b = fixture("two-page.pdf");
    let bytes = run_cli(
        QDF,
        "three-page.pdf",
        &[
            "--overlay", a.to_str().unwrap(), "--",
            "--underlay", b.to_str().unwrap(), "--",
        ],
    );
    assert_bytes(&bytes, "three-page-overlay-and-underlay-qdf.pdf");
}

#[test]
fn cli_three_page_two_overlays_qdf_is_byte_identical() {
    let a = fixture("one-page.pdf");
    let b = fixture("two-page.pdf");
    let bytes = run_cli(
        QDF,
        "three-page.pdf",
        &[
            "--overlay", a.to_str().unwrap(), "--",
            "--overlay", b.to_str().unwrap(), "--",
        ],
    );
    assert_bytes(&bytes, "three-page-two-overlays-qdf.pdf");
}
```

**Step 2: 全 19 tests を実行**

Run:
```bash
cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_byte_identical_overlay
```
Expected: `19 passed`.

**Step 3: Commit**

```bash
git add crates/flpdf-cli/tests/cli_byte_identical_overlay.rs
git commit -m "test(flpdf-cli): add --qdf --no-original-object-ids CLI overlay byte gates

Mirrors the library write_qdf_nooid overlay byte_gate cases (single overlay,
overlay+underlay, two overlays) end-to-end through the flpdf binary to cover
the QDF writer path in combination with overlay/underlay.

Refs: flpdf-9hc.36"
```

---

### Task 5: Update `overlay.rs` deferral comment

**Files:**
- Modify: `crates/flpdf/src/overlay.rs:1148-1152`

**Step 1: Deferral コメント差し替え**

`overlay.rs:1148-1152` の現状:
```rust
// Explicit deferrals (NOT covered here, by design):
//   - CLI-level overlay byte-identity: these gates write through the library
//     entry points with `NewlineBeforeEndstream::Never` to keep the byte
//     comparison surgical. The CLI now also defaults to `Never` and can be
//     wired up separately for CLI-level overlay byte-identity coverage.
```

を以下に置き換え:
```rust
// CLI-level overlay byte-identity coverage lives in
// `crates/flpdf-cli/tests/cli_byte_identical_overlay.rs` (gated on
// `qpdf-zlib-compat`, same policy as the linearize `cli_byte_identical`
// gate). Those tests run the actual `flpdf` binary with `--static-id`
// [`--qdf --no-original-object-ids`] against the same overlay goldens
// used here, catching CLI-layer wiring divergences (argv parsing,
// `WriteOptions` assembly, defaults) that library-only gates cannot see.
```

**Step 2: overlay.rs が壊れていないことを確認 (compile + lib byte_gate 未変更)**

Run:
```bash
cargo test -p flpdf --features qpdf-zlib-compat --lib overlay::byte_gate -- --list 2>&1 | tail -5
```
Expected: 30+ test 一覧が壊れずに列挙される (コメント差し替えのみなので影響なし)。

**Step 3: Commit**

```bash
git add crates/flpdf/src/overlay.rs
git commit -m "docs(flpdf): update overlay.rs deferral comment to point at CLI byte gate

The CLI-level overlay byte-identity gate is now implemented in
crates/flpdf-cli/tests/cli_byte_identical_overlay.rs. Replace the
'deferred / can be wired up separately' note with a forward pointer.

Refs: flpdf-9hc.36"
```

---

### Task 6: CI wiring (`.github/workflows/ci.yml`)

**Files:**
- Modify: `.github/workflows/ci.yml:170` 直後

**Step 1: bytes-identical zlib compat step に 1 行追加**

現状 (line 165-170):
```yaml
          cargo test -p flpdf --features qpdf-zlib-compat --lib overlay::byte_gate
          # Pins the CLI binary's linearized output byte-identity vs qpdf goldens
          # end-to-end (argument parsing -> WriteOptions -> write path -> framing).
          # Gated behind qpdf-zlib-compat, so it is compiled out of the default
          # test workflow and must be run explicitly here.
          cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_byte_identical
```

末尾に append:
```yaml
          # Pins the CLI binary's overlay/underlay output byte-identity vs qpdf
          # goldens end-to-end (raw-argv pre-split -> WriteOptions -> write path).
          # Gated behind qpdf-zlib-compat; must be run explicitly here.
          cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_byte_identical_overlay
```

**Step 2: yaml 妥当性を軽く検証**

Run:
```bash
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml'))"
```
Expected: 例外なし。

**Step 3: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: run CLI overlay byte-identity gate in bytes-identical zlib compat step

Adds cli_byte_identical_overlay to the explicit test list so the new gate
runs in CI (bd remember flpdf-ci-bytes-identical-explicit-test-list).

Refs: flpdf-9hc.36"
```

---

### Task 7: Full-suite verification + patch-coverage gate + push + PR

**Files:** なし（検証と push のみ）

**Step 1: 影響範囲の full-test 実行**

Run:
```bash
cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_byte_identical_overlay
cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_byte_identical
cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_overlay
cargo test -p flpdf --features qpdf-zlib-compat --lib overlay::byte_gate
```
Expected: 全 test PASS。

**Step 2: fmt/clippy チェック**

Run:
```bash
cargo fmt --all -- --check
cargo clippy -p flpdf-cli --tests --features qpdf-zlib-compat -- -D warnings
```
Expected: 差分/警告なし。あれば `cargo fmt --all` で修正して amend しない (新規コミット)。`bd recall flpdf-ci-quality-fmt-check`。

**Step 3: patch-coverage gate**

Run:
```bash
scripts/patch-coverage.sh --base main
```
Expected: `flpdf` 変更行 (overlay.rs コメント差し替え) は non-executable なので 0/0、`flpdf-cli` は新 test file 全行 100% cover。EXIT 0。`bd recall llvm-cov-no-qpdf-zlib-compat`。

**Step 4: push + PR**

```bash
git pull --rebase origin main
git push -u origin feat/flpdf-9hc-36-cli-overlay-byte-gate
gh pr create --title "test(flpdf-cli): CLI overlay/underlay byte-identity gate (wire library goldens through CLI)" --body "$(cat <<'EOF'
## Summary

- Adds `crates/flpdf-cli/tests/cli_byte_identical_overlay.rs` — 19 byte-identity gates that run the `flpdf` binary end-to-end (`rewrite --static-id [--qdf --no-original-object-ids] DEST --overlay/--underlay SRC ... -- OUT`) and diff the output against the committed qpdf overlay goldens under `tests/golden/references/overlay/`.
- Mirrors the library-layer `overlay::byte_gate` scope: 16 plain `--static-id` gates (page-range variants, composition, fixture variants incl. rotated / multi-stream / userunit / swapped-box) + 3 `--qdf --no-original-object-ids` gates.
- Wires the new test binary into the CI `bytes-identical zlib compat (Linux amd64)` step so it runs alongside the existing library `overlay::byte_gate` and CLI `cli_byte_identical` (linearize) gates.
- Updates the deferral comment in `crates/flpdf/src/overlay.rs:1148-1152` to point at the new CLI gate.

The CLI-layer gate catches wiring divergences (argv parsing, `WriteOptions` assembly, defaults such as `NewlineBeforeEndstream::Never` and the overlay-presence ⇒ `full_rewrite=true` promotion) that library-only gates cannot see. It relies on the CLI default flip landed in flpdf-9hc.33 (`bd recall flpdf-cli-newline-before-endstream-default-is-now`).

Refs: flpdf-9hc.36

## Test plan

- [x] `cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_byte_identical_overlay` — 19 passed locally
- [x] `cargo test -p flpdf --features qpdf-zlib-compat --lib overlay::byte_gate` — unaffected (comment-only touch to overlay.rs)
- [x] `cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_byte_identical` — unaffected
- [x] `cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_overlay` — unaffected
- [x] `cargo fmt --all -- --check` — clean
- [x] `cargo clippy -p flpdf-cli --tests --features qpdf-zlib-compat -- -D warnings` — clean
- [x] `scripts/patch-coverage.sh --base main` — new test file 100% covered
EOF
)"
```

**Step 5: PR URL を報告して完了**

Report the PR URL to the user. Then blueprint:impl の Step 6 (verification-before-completion + finishing-a-development-branch + close issue confirmation) に戻る。

---

## リスク / トリアージ

- **Test #1 が FAIL**: CLI wiring 起因の乖離 (overlay presence ⇒ full_rewrite promotion か Newline default か)。advisor 呼んで切り分け前に他 test 追加を止める。
- **fixture 欠落**: `tests/fixtures/compat/` に `multi-stream-one-page.pdf` 等が無い場合 → regenerate.sh の recipe と付き合わせて存在確認 (下記コマンド)。
  ```bash
  for f in one-page two-page three-page one-page-r90 multi-stream-one-page userunit-one-page swapped-box-one-page swapped-box-r90-one-page; do
      test -f tests/fixtures/compat/$f.pdf && echo "OK $f" || echo "MISS $f"
  done
  ```
- **CI で fmt fail**: 各 commit 前に `cargo fmt --all` を回す (`bd recall flpdf-ci-quality-fmt-check`)。
