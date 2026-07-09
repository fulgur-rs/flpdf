# flpdf-9hc.16.12 — CLI overlay/underlay --verbose progress (qpdf-parity) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** flpdf-cli の rewrite 経路で `--verbose` を渡したとき、qpdf の
overlay/underlay 進捗ブロック (`qpdf: processing underlay/overlay` + per-page
mapping) と書き出しメッセージ (`qpdf: wrote file <path>`) をバイト等価に出力し、
qtest form-xobject の uo-1/2/3/4/5/7 runtest (test 19/21/23/25/29/31) を
PASS に転じる。

**Architecture:** `crates/flpdf/src/overlay.rs` に (a) `spec_page_sources` から
range-math を切り出した `resolve_spec_pairs`、(b) `apply_overlays_to_page` の
underlay/overlay 分離を抜き出した `kind_stable_partition` helper、(c) 新規
公開 API `overlay_verbose_report` を追加。CLI 側 (`crates/flpdf-cli/src/main.rs`)
は `verbose: bool` の `requires="list_attachments"` を外し、`run_rewrite` に
verbose を通して report を print する。printing はすべて stderr、prefix
`flpdf:`（flpdf-qtest shim が `qpdf:` に正規化）。

**Tech Stack:** Rust workspace (flpdf lib + flpdf-cli), cargo test, clap,
flpdf-qtest ハーネス (`~/flpdf-qtest/scripts/run.sh`).

**Design source:** beads issue `flpdf-9hc.16.12` の `design` フィールド
(`bd show flpdf-9hc.16.12` で参照)。詳細な qpdf 出力仕様・却下案・oracle
戦略はそちらに記載。

---

## Task 1: Refactor — split `resolve_spec_pairs` out of `spec_page_sources`

**Rationale:** report と apply が同じ pair 生成ロジックを共有すること
を構造的に強制する。この Task は挙動を変えない。

**Files:**
- Modify: `crates/flpdf/src/overlay.rs:408-472` (`spec_page_sources`)

**Step 1: baseline overlay byte tests を控えておく**

```bash
cd /home/ubuntu/flpdf/.worktrees/flpdf-9hc-16-12-overlay-verbose
cargo test -p flpdf --lib overlay --features qpdf-zlib-compat 2>&1 | tail -5
```
Expected: `test result: ok. <N> passed;` を控える（Task 3 完了まで数値不変）。

**Step 2: `resolve_spec_pairs` に前半を切り出す**

`spec_page_sources` の pair 生成 (page_refs → PageRange::resolve → pairing) を
以下シグネチャで `pub(crate) fn resolve_spec_pairs<RS: Read + Seek>(
source: &mut Pdf<RS>, from: &PageRange, to: &PageRange, repeat: Option<&PageRange>,
n_dest: u32) -> Result<Vec<(u32, u32)>>` として抽出。`spec_page_sources` は
これを呼び、`distinct_sources` の dedup 以降 (`import_page_as_form_xobject`,
`OverlaySource` 生成) を継続する形にする。

`dest` 参照を渡さない設計にすることで `overlay_verbose_report` から呼びやすくする
（dest はページ数取得と xobject import で必要だが、pair 計算自体には不要）。
`n_dest` は呼び出し側で `page_refs(dest)?.len()` から作る。

**Step 3: overlay ライブラリテストが全通ることを確認**

```bash
cargo test -p flpdf --lib overlay --features qpdf-zlib-compat 2>&1 | tail -5
```
Expected: Step 1 と同数の pass。**failure が 1 でも出たら refactor 誤り**、
diff を戻して原因究明。

**Step 4: `cargo fmt`**

```bash
cargo fmt
```

**Step 5: Commit**

```bash
git add crates/flpdf/src/overlay.rs
git commit -m "refactor(flpdf): split resolve_spec_pairs out of spec_page_sources

Prepares for overlay_verbose_report (flpdf-9hc.16.12): the report needs
the (dest, src) page pairing that spec_page_sources computes internally,
without the downstream xobject import. Extract the range-math half into
a pub(crate) helper; spec_page_sources now calls it and imports as
before. Byte-parity path unchanged."
```

---

## Task 2: Refactor — extract `kind_stable_partition` helper

**Rationale:** advisor #3: underlay/overlay の順序を apply と report で
独立に組むと drift 危険。共通 helper で構造的に一致させる。

**Files:**
- Modify: `crates/flpdf/src/overlay.rs:235-298` (`apply_overlays_to_page`)

**Step 1: helper を追加**

```rust
/// Stable-partition a slice into (underlays first, overlays second),
/// preserving original relative order within each group. qpdf orders
/// overlay/underlay sources this way for both painting (see
/// [`apply_overlays_to_page`]) and verbose reporting.
pub(crate) fn kind_stable_partition<T, F>(entries: Vec<T>, kind_of: F) -> Vec<T>
where
    F: Fn(&T) -> OverlayKind,
{
    let (underlays, overlays): (Vec<T>, Vec<T>) =
        entries.into_iter().partition(|e| matches!(kind_of(e), OverlayKind::Underlay));
    let mut out = underlays;
    out.extend(overlays);
    out
}
```

**Step 2: unit test を追加**

```rust
#[test]
fn kind_stable_partition_underlays_first_stable_within_group() {
    #[derive(Debug, PartialEq)]
    struct E(u32, OverlayKind);
    let out = kind_stable_partition(
        vec![
            E(1, OverlayKind::Overlay),
            E(2, OverlayKind::Underlay),
            E(3, OverlayKind::Overlay),
            E(4, OverlayKind::Underlay),
        ],
        |e| e.1,
    );
    assert_eq!(
        out,
        vec![
            E(2, OverlayKind::Underlay),
            E(4, OverlayKind::Underlay),
            E(1, OverlayKind::Overlay),
            E(3, OverlayKind::Overlay),
        ],
        "underlays first, then overlays, stable within each"
    );
}
```

**Step 3: `apply_overlays_to_page` を helper 呼び出しに置き換える**

現行 line 241-248 の 2 段 Vec 構築を、`kind_stable_partition(sources.to_vec(),
|s| s.kind)` の結果を保持し、そこから `.iter().filter(|s| s.kind ==
Underlay)` / Overlay で `underlays: Vec<ObjectRef>` / `overlays: Vec<ObjectRef>`
を派生させる形に書き換える。あるいは、helper が返した順序付き Vec を直接
使い、その後の naming/painting は元と同じロジックを踏襲する。

**重要:** paint 結果 (contents stream バイト列, xobject naming) は 1 バイトも
変わってはいけない。

**Step 4: overlay ライブラリテスト＋ helper unit test 実行**

```bash
cargo test -p flpdf --lib overlay --features qpdf-zlib-compat 2>&1 | tail -10
cargo test -p flpdf --lib overlay::tests::kind_stable_partition 2>&1 | tail -5
```
Expected: 既存テスト全通 + 新規 helper test 1 pass。**失敗したら refactor
誤り**、原因究明まで先へ進まない。

**Step 5: `cargo fmt` + Commit**

```bash
cargo fmt
git add crates/flpdf/src/overlay.rs
git commit -m "refactor(flpdf): extract kind_stable_partition helper

apply_overlays_to_page's underlay/overlay grouping is about to be
reused by overlay_verbose_report (flpdf-9hc.16.12). Extract the
grouping into a shared pub(crate) helper so painting and reporting
cannot drift. Paint output unchanged."
```

---

## Task 3: Library — `overlay_verbose_report` public API (TDD)

**Files:**
- Modify: `crates/flpdf/src/overlay.rs` (add public types + fn)
- Modify: `crates/flpdf/src/lib.rs` (re-export if needed)

**Step 1: 失敗テストを書く (drift 検出テスト)**

`crates/flpdf/src/overlay.rs` の `#[cfg(test)]` セクション末尾に:

```rust
#[test]
fn overlay_verbose_report_orders_underlays_then_overlays_across_specs() {
    // 4 specs on the same simple 1-page dest, each targeting page 1
    // via --to=1 in declaration order:
    //   spec 0: overlay  source A
    //   spec 1: overlay  source B
    //   spec 2: underlay source C
    //   spec 3: underlay source D
    // Expected page-1 sources order (spec_index): [2, 3, 0, 1]
    let (mut dest, [src_a, src_b, src_c, src_d]) = mk_1page_dest_and_4_sources();
    let mut specs = vec![
        OverlaySpec { source: src_a, kind: OverlayKind::Overlay,
                      from: PageRange::parse("").unwrap(), to: PageRange::parse("1").unwrap(),
                      repeat: None },
        OverlaySpec { source: src_b, kind: OverlayKind::Overlay,
                      from: PageRange::parse("").unwrap(), to: PageRange::parse("1").unwrap(),
                      repeat: None },
        OverlaySpec { source: src_c, kind: OverlayKind::Underlay,
                      from: PageRange::parse("").unwrap(), to: PageRange::parse("1").unwrap(),
                      repeat: None },
        OverlaySpec { source: src_d, kind: OverlayKind::Underlay,
                      from: PageRange::parse("").unwrap(), to: PageRange::parse("1").unwrap(),
                      repeat: None },
    ];
    let report = overlay_verbose_report(&mut dest, &mut specs).unwrap();
    assert_eq!(report.len(), 1);
    assert_eq!(report[0].dest_page, 1);
    let idx: Vec<usize> = report[0].sources.iter().map(|s| s.spec_index).collect();
    assert_eq!(idx, vec![2, 3, 0, 1], "underlays first (specs 2,3), then overlays (0,1)");
}
```

`mk_1page_dest_and_4_sources` は既存 test helper (`build_pdf`,
`open_from_bytes` 等) を利用して 1-page destination + 4 個の 1-page source
`Pdf` を返す。

Run: `cargo test -p flpdf --lib overlay::tests::overlay_verbose_report_orders 2>&1 | tail -20`
Expected: FAIL with "cannot find function `overlay_verbose_report`".

**Step 2: 型と関数を実装**

```rust
/// A single source line in the [`overlay_verbose_report`] output.
pub struct OverlayVerboseSource {
    pub spec_index: usize,
    pub kind: OverlayKind,
    pub src_page: u32,
}

/// One destination page in the [`overlay_verbose_report`] output.
pub struct OverlayVerbosePage {
    pub dest_page: u32,
    pub sources: Vec<OverlayVerboseSource>,
}

/// Read-only inspection returning the per-destination-page overlay/underlay
/// progress plan matching qpdf `--verbose`'s "processing underlay/overlay"
/// block. Covers `1..=n_dest` in ascending order; per-page sources are
/// ordered underlays-first (declaration order across specs), overlays after.
///
/// Does not import xobjects and does not mutate the destination graph.
/// Safe to call before [`apply_overlay_specs`] on the same `&mut` slice.
///
/// # Errors
///
/// - [`Error::Unsupported`] when a page number resolves outside its
///   document.
/// - Any error propagated from page-range resolution.
pub fn overlay_verbose_report<RS, RT>(
    dest: &mut Pdf<RT>,
    specs: &mut [OverlaySpec<RS>],
) -> Result<Vec<OverlayVerbosePage>>
where
    RS: Read + Seek,
    RT: Read + Seek,
{
    let n_dest = u32_len(page_refs(dest)?.len());
    // Flatten across specs into (dest_page, spec_index, kind, src_page).
    let mut flat: Vec<(u32, OverlayVerboseSource)> = Vec::new();
    for (spec_index, spec) in specs.iter_mut().enumerate() {
        let pairs = resolve_spec_pairs(&mut spec.source, &spec.from, &spec.to,
                                       spec.repeat.as_ref(), n_dest)?;
        for (dest_page, src_page) in pairs {
            flat.push((dest_page, OverlayVerboseSource {
                spec_index, kind: spec.kind, src_page,
            }));
        }
    }
    // Bucket by dest_page (BTreeMap keeps ascending page order).
    let mut by_page: BTreeMap<u32, Vec<OverlayVerboseSource>> = BTreeMap::new();
    for (dest_page, src) in flat {
        by_page.entry(dest_page).or_default().push(src);
    }
    // Emit page 1..=n_dest, ordering each bucket with the shared helper.
    let mut out = Vec::with_capacity(n_dest as usize);
    for dest_page in 1..=n_dest {
        let sources = by_page.remove(&dest_page).unwrap_or_default();
        let sources = kind_stable_partition(sources, |s| s.kind);
        out.push(OverlayVerbosePage { dest_page, sources });
    }
    Ok(out)
}
```

`crates/flpdf/src/lib.rs` の `pub use overlay::{...}` 節（既存 OverlaySpec,
OverlayKind, apply_overlay_specs 等が並ぶ場所を grep で特定）に
`OverlayVerbosePage`, `OverlayVerboseSource`, `overlay_verbose_report` を追加。

**Step 3: Task 3 Step 1 のテストを再実行**

```bash
cargo test -p flpdf --lib overlay::tests::overlay_verbose_report_orders 2>&1 | tail -10
```
Expected: PASS。

**Step 4: 残 4 テストを追加（順に TDD）**

`overlay_verbose_report_includes_dest_pages_with_no_sources`:
- 3-page dest, 2-page source, `--overlay --to=1-2` の 1 spec
- 期待: `report.len() == 3`, `report[2].sources.is_empty()`

`overlay_verbose_report_pins_source_page_under_repeat`:
- 5-page dest, 2-page source, `--overlay --repeat=1-2` の 1 spec
- 期待: sources[0..=4].src_page == [1, 2, 1, 2, 1]

`overlay_verbose_report_empty_to_yields_all_empty_entries`:
- 3-page dest, 2-page source, `--to=""` 明示（`PageRange::empty()`）
- 期待: 全 3 page で sources 空

`overlay_verbose_report_does_not_mutate_dest`:
- 3-page dest, 1 spec、report 呼出前後で `page_refs(&mut dest)?.len()` 不変、
  page 1 の dict `Contents`, `Resources` オブジェクト参照が同一

各テストを追加するたび `cargo test -p flpdf --lib overlay::tests::<name>` で
red→green を確認。

**Step 5: 既存 overlay テスト全走で回帰なし確認**

```bash
cargo test -p flpdf --lib overlay --features qpdf-zlib-compat 2>&1 | tail -10
```
Expected: 既存 + 新規 5 テストすべて pass。

**Step 6: `cargo fmt` + Commit**

```bash
cargo fmt
git add crates/flpdf/src/overlay.rs crates/flpdf/src/lib.rs
git commit -m "feat(flpdf): add overlay_verbose_report inspection API

New pub fn that returns per-destination-page overlay/underlay plan
matching qpdf --verbose's 'processing underlay/overlay' ordering:
underlays first across specs in declaration order, then overlays. Uses
the shared kind_stable_partition helper so the report and the paint
path (apply_overlays_to_page) cannot drift. Read-only; does not import
xobjects. Feeds flpdf-9hc.16.12 (CLI verbose progress)."
```

---

## Task 4: CLI — promote `--verbose` to general flag

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs:450-456` (verbose 定義)

**Step 1: `requires = "list_attachments"` を削除**

```rust
#[arg(
    long = "verbose",
    help = "Print verbose progress and diagnostic messages \
            (mirrors qpdf --verbose)"
)]
verbose: bool,
```

**Step 2: 既存 `--list-attachments --verbose` の統合テストを確認**

`grep -rn "list_attachments" crates/flpdf-cli/tests/` で該当テストを見つけ、
その挙動が変わらないことを確認 (verbose を list_attachments に渡す call site
`run_list_attachments(..., args.verbose)` は無変更)。

```bash
cargo test -p flpdf-cli list_attachments 2>&1 | tail -10
```
Expected: 既存 pass 数維持。

**Step 3: fmt + Commit**

```bash
cargo fmt
git add crates/flpdf-cli/src/main.rs
git commit -m "feat(flpdf-cli): promote --verbose to a general rewrite-path flag

Removes requires=\"list_attachments\" so --verbose can gate progress
output on the rewrite path (upcoming: overlay/underlay progress and
'wrote file' messages for flpdf-9hc.16.12). --list-attachments --verbose
behavior unchanged."
```

---

## Task 5: CLI — thread verbose into `run_rewrite`

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs` (`run_rewrite` 関数と 3 箇所の
  呼び出し元)

**Step 1: 呼び出し元列挙**

```bash
grep -n "run_rewrite\|fn run_rewrite" crates/flpdf-cli/src/main.rs | head -10
```
呼び出し元 (line 1609, 1770, 2249 相当) を控える。

**Step 2: シグネチャに `verbose: bool` 追加**

`run_rewrite(input, output, repair, password, linearize, remove_restrictions,
decrypt, normalize_content, coalesce_contents, remove_unref,
generate_appearances, flatten_annotations_mode, flatten_rotation,
overlay_specs, options)` に `verbose: bool` 引数を追加（末尾 or `options` の
直前に置く）。

各呼び出し元で `args.verbose` を渡す（`Commands::Rewrite(cmd)` 分岐では
`cmd.verbose`; ただし RewriteCommand には verbose がまだ無い場合、`args.verbose`
の値を親から受け継ぐ形にする — 具体箇所は grep で確認）。

**Step 3: `cargo build -p flpdf-cli` でコンパイルが通ることを確認**

```bash
cargo build -p flpdf-cli 2>&1 | tail -20
```
Expected: no errors.

**Step 4: Commit**

```bash
git add crates/flpdf-cli/src/main.rs
git commit -m "feat(flpdf-cli): thread verbose flag through run_rewrite

Plumbing only; consumed in the next commits (overlay progress + wrote
file lines for flpdf-9hc.16.12). No user-visible behavior change yet."
```

---

## Task 6: CLI — emit overlay/underlay progress block (TDD)

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs` (`run_rewrite` 内 line 3118 付近)
- Create: `crates/flpdf-cli/tests/verbose_overlay_progress.rs`

**Step 1: 失敗する統合テストを書く**

```rust
// crates/flpdf-cli/tests/verbose_overlay_progress.rs

use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn verbose_overlay_prints_processing_and_per_page_mapping() {
    let dest = "tests/assets/overlay/3page-dest.pdf";
    let src = "tests/assets/overlay/1page-src.pdf";
    let out = tempfile::Builder::new().suffix(".pdf").tempfile().unwrap();
    Command::cargo_bin("flpdf").unwrap()
        .args([
            "--static-id", "--verbose", dest,
            out.path().to_str().unwrap(),
            "--overlay", src, "--"
        ])
        .assert()
        .success()
        .stderr(contains("flpdf: processing underlay/overlay\n"))
        .stderr(contains("  page 1\n    ").and(contains(" overlay 1\n")))
        .stderr(contains("  page 2\n"))
        .stderr(contains("  page 3\n"));
}
```

`3page-dest.pdf`, `1page-src.pdf` は既存 fixture を再利用 (grep で
`crates/flpdf-cli/tests/assets` を探す)。存在しなければ最小の 3-page /
1-page pdf を `qpdf` で生成しコミット。

```bash
cargo test -p flpdf-cli --test verbose_overlay_progress 2>&1 | tail -10
```
Expected: FAIL — 出力がない。

**Step 2: `run_rewrite` の overlay ブロック内に printing を追加**

`if !overlay_specs.is_empty()` の中、`apply_overlay_specs` の**直前**に:

```rust
if verbose {
    let report = flpdf::overlay_verbose_report(&mut pdf, &mut built)?;
    eprintln!("flpdf: processing underlay/overlay");
    for page in &report {
        eprintln!("  page {}", page.dest_page);
        for src in &page.sources {
            let file = &overlay_specs[src.spec_index].file;
            let kind_str = match src.kind {
                flpdf::OverlayKind::Underlay => "underlay",
                flpdf::OverlayKind::Overlay => "overlay",
            };
            eprintln!("    {} {} {}", file, kind_str, src.src_page);
        }
    }
}
```

`overlay_specs` は関数引数の `&[cli::OverlaySpec]`（CLI 側 struct）で、
`.file` フィールドで CLI 生文字列を取り出せる（main.rs:3404-3407 で確認済）。

**Step 3: テスト再実行**

```bash
cargo test -p flpdf-cli --test verbose_overlay_progress 2>&1 | tail -10
```
Expected: PASS。

**Step 4: Commit**

```bash
git add crates/flpdf-cli/src/main.rs crates/flpdf-cli/tests/verbose_overlay_progress.rs
# fixture を新規追加した場合はそれも
git commit -m "feat(flpdf-cli): emit --verbose overlay/underlay progress block

Prints 'flpdf: processing underlay/overlay' + per-destination-page
mapping to stderr, matching qpdf --verbose's byte-parity format. Uses
the new flpdf::overlay_verbose_report inspection API so the printed
ordering (underlays-first-then-overlays across specs) is source-shared
with the paint path. Prefix flpdf: is normalized to qpdf: by the
flpdf-qtest shim.

Refs flpdf-9hc.16.12."
```

---

## Task 7: CLI — emit `flpdf: wrote file <path>` (TDD)

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs` (`run_rewrite` の write 直後)
- Create: `crates/flpdf-cli/tests/verbose_wrote_file.rs`

**Step 1: 失敗テストを書く**

```rust
// crates/flpdf-cli/tests/verbose_wrote_file.rs

use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn verbose_prints_wrote_file_line() {
    let input = "tests/assets/overlay/3page-dest.pdf";  // 何でも良い最小 PDF
    let out = tempfile::Builder::new().suffix(".pdf").tempfile().unwrap();
    let out_path = out.path().to_str().unwrap();
    Command::cargo_bin("flpdf").unwrap()
        .args(["--static-id", "--verbose", input, out_path])
        .assert()
        .success()
        .stderr(contains(format!("flpdf: wrote file {}\n", out_path)));
}

#[test]
fn no_verbose_does_not_print_wrote_file() {
    let input = "tests/assets/overlay/3page-dest.pdf";
    let out = tempfile::Builder::new().suffix(".pdf").tempfile().unwrap();
    Command::cargo_bin("flpdf").unwrap()
        .args(["--static-id", input, out.path().to_str().unwrap()])
        .assert()
        .success()
        .stderr(predicates::str::is_empty().or(
            predicates::str::contains("wrote file").not()));
}
```

```bash
cargo test -p flpdf-cli --test verbose_wrote_file 2>&1 | tail -10
```
Expected: FAIL — 出力なし。

**Step 2: `run_rewrite` に emit を追加**

`write_pdf_with_options(&mut pdf, &mut out, &options)?;` の**直後**、
`if remove_restrictions && was_encrypted` の diagnostic より**前**に:

```rust
if verbose {
    eprintln!("flpdf: wrote file {}", output.display());
}
```

**Step 3: 両テスト pass 確認**

```bash
cargo test -p flpdf-cli --test verbose_wrote_file 2>&1 | tail -10
```
Expected: PASS。

**Step 4: Commit**

```bash
git add crates/flpdf-cli/src/main.rs crates/flpdf-cli/tests/verbose_wrote_file.rs
git commit -m "feat(flpdf-cli): emit --verbose 'wrote file <path>' after rewrite

Adds the second qpdf --verbose byte-parity line needed by qtest
form-xobject uo-1..uo-5, uo-7 (test 19/21/23/25/29/31). Emitted to
stderr with flpdf: prefix (shim normalizes to qpdf:), after the writer
returns and before any post-write diagnostics.

Refs flpdf-9hc.16.12."
```

---

## Task 8: Oracle — flpdf-qtest form-xobject 実行

**Rationale:** 真の byte-parity 判定は flpdf-qtest ハーネス経由の uo-*.out
比較。unit/integration test は補助。

**Step 1: flpdf-cli を release ビルド**

```bash
cd /home/ubuntu/flpdf/.worktrees/flpdf-9hc-16-12-overlay-verbose
cargo build --release -p flpdf-cli 2>&1 | tail -5
```

**Step 2: form-xobject test を走らせる**

```bash
FLPDF_CLI_BIN=/home/ubuntu/flpdf/.worktrees/flpdf-9hc-16-12-overlay-verbose/target/release/flpdf \
QTEST_TESTS=form-xobject \
bash /home/ubuntu/flpdf-qtest/scripts/run.sh 2>&1 | tail -40
```

**Step 3: 結果確認**

`/home/ubuntu/flpdf-qtest/qtest.log` を Read し、`overlay/underlay 1..8`
runtest について:
- test 19 (uo-1 runtest), 21 (uo-2), 23 (uo-3), 25 (uo-4), 29 (uo-5),
  31 (uo-7) が **PASS**
- test 27 (uo-6), 33 (uo-8) は `--pages` verbose 未実装のため FAIL のまま
  （follow-up 対象）
- test 20/22/24/26/28/30/32/34 (compare-files) は本 issue scope 外、状態不問

もし想定外の FAIL が出たら:
- `qtest.log` の diff セクションを Read
- CLI 出力と uo-*.out の差分を特定 (空白、改行、順序)
- 修正して commit 追加 (Task 6/7 修正 or 新 commit)

**Step 4: PASS 確定後、実行結果をコミットに追加しない**

qtest 実行結果 (harness.log, qtest.log) はコミットしない（flpdf-qtest 側の
アーティファクト）。本リポでの変更は無し、コミット追加不要。

---

## Task 9: 品質ゲート + follow-up bd issue 作成

**Step 1: fmt / clippy**

```bash
cd /home/ubuntu/flpdf/.worktrees/flpdf-9hc-16-12-overlay-verbose
cargo fmt --check
cargo clippy --workspace -- -D warnings 2>&1 | tail -10
```
Expected: 両方緑。失敗したら修正 commit。

**Step 2: フルテスト**

```bash
cargo test --workspace --features qpdf-zlib-compat 2>&1 | tail -20
```
Expected: 全 pass。

**Step 3: patch-coverage**

```bash
git add -A  # 未 commit のものがあれば commit
scripts/patch-coverage.sh --base main 2>&1 | tail -40
```

- `flpdf` は変更行 100% cover 必須。未カバー行が出れば cov:ignore で
  除外 (理由付き) するか、テスト追加。
- `flpdf-cli` は報告のみ (ブロックしない)。

**Step 4: follow-up issue を作成**

```bash
bd create \
  --title="CLI: --verbose --pages progress output (qpdf-parity) — unblocks uo-6/uo-8" \
  --description="qpdf --verbose emits per-file processing progress for --pages/--keep-open-files, which uo-6 and uo-8 (qtest form-xobject 27/33) need. Format:

  qpdf: selecting --keep-open-files=y
  qpdf: <file>: checking for shared resources
  qpdf: no shared resources found
  qpdf: removing unreferenced pages from primary input
  qpdf: adding pages from <file>

Scope: flpdf-cli/src/main.rs — thread verbose into the --pages path and emit these lines to stderr with flpdf: prefix (shim normalizes). Format byte-identical to qpdf.

Acceptance: qtest form-xobject 27/33 runtest steps PASS." \
  --type=feature \
  --priority=3 \
  --parent=flpdf-9hc.16
```

新規 issue ID を控え、本 issue の close 時に follow-up 情報として PR body に
記載。

---

## Task 10: PR 作成

**Step 1: 変更概要を git log で確認**

```bash
git log --oneline main..HEAD
```
Expected: 6-7 commits (Task 1, 2, 3, 4, 5, 6, 7)。

**Step 2: push + PR 作成**

```bash
git push -u origin feat/flpdf-9hc-16-12-overlay-verbose
gh pr create --base main --title "feat(flpdf-cli): --verbose overlay/underlay progress + wrote file (qpdf-parity)" --body "$(cat <<'EOF'
## Summary

qpdf `--verbose` の rewrite 経路メッセージのうち以下 2 種を flpdf-cli で
バイト等価に emit する:

1. `qpdf: processing underlay/overlay` ヘッダ + per-destination-page
   overlay/underlay mapping
2. `qpdf: wrote file <path>` 書き出し完了行

## Scope note

本 issue (flpdf-9hc.16.12) の title は overlay progress のみだが、
acceptance 「odd runtest PASS」を成立させるため `wrote file` 行も同時に
含めた (advisor の recommendation)。qtest form-xobject 19/21/23/25/29/31
(uo-1/2/3/4/5/7) が本 PR で PASS に転じる。

uo-6/uo-8 (qtest 27/33) は追加で `--pages` verbose が必要で follow-up
(bd: <NEW-ID>) で対応する。

## Approach

- library (`crates/flpdf/src/overlay.rs`):
  - `spec_page_sources` から `resolve_spec_pairs` を切り出し、
    (dest, src) pair 計算を xobject import と分離
  - `apply_overlays_to_page` の underlay/overlay 分離を
    `kind_stable_partition` helper に共通化
  - 新規公開 API `overlay_verbose_report`: per-page 進捗 plan を返す。
    paint 経路と ordering ロジックを共有 (drift 予防)
- CLI (`crates/flpdf-cli/src/main.rs`):
  - `--verbose` の `requires=\"list_attachments\"` 撤廃
  - `run_rewrite` に verbose 引数、overlay/underlay progress と wrote
    file を stderr に emit (flpdf-qtest shim が prefix 正規化)

## Test plan

- [ ] `cargo test -p flpdf --lib overlay --features qpdf-zlib-compat`
      全通 (既存 + 新規 5 unit test)
- [ ] `cargo test -p flpdf-cli --test verbose_overlay_progress`
      と `--test verbose_wrote_file` 全通
- [ ] flpdf-qtest form-xobject の test 19/21/23/25/29/31 が PASS
      (uo-1/2/3/4/5/7)
- [ ] `cargo fmt --check`, `cargo clippy --workspace -- -D warnings`
- [ ] `scripts/patch-coverage.sh --base main` (flpdf は 100%, cli は報告のみ)

Refs flpdf-9hc.16.12.
EOF
)"
```

**Step 3: PR URL を控える**

出力の `https://github.com/.../pull/N` を保存し bd issue close 時の references
に記載。

---

## Notes

- 各 Task の commit 単位で `cargo fmt` を回すこと (flpdf CI Quality = fmt check
  memory 参照)。
- Task 8 の flpdf-qtest 実行が真の oracle。unit/integration test はサンドバッ
  グ; qtest が緑になるまで完了と見なさない。
- Task 6/7 の CLI テストで tempfile の path が verbose 出力に正しく表示
  されることも副次的に検証。
- 各 Task で新規 issue の必要が生じたら bd create し、design を差し込むこと。
