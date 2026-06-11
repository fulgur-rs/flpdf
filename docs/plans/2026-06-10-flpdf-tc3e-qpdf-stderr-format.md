# CLI stderr diagnostic format parity with qpdf (flpdf-tc3e) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** flpdf CLI の stderr 診断を qpdf 11.9.0 の形式（`WARNING: <file>: <msg>` / `<progname>: <file>: <msg>` / 末尾 `operation succeeded with warnings`）に揃える。

**Architecture:** library 層（`crates/flpdf/src/xref.rs`）は repair 3 警告の offset 付与を qpdf に合わせる 1 箇所のみ。CLI 層（`crates/flpdf-cli/src/main.rs`）に `progname()` / `diagnostic_location()` ヘルパーを導入し、`run_check` と `open_pdf` の診断レンダリングを置換。プレフィクスは env var `FLPDF_PROGNAME`（qtest shim 用、未設定なら `flpdf`）。文言 parity は Stage 3（flpdf-ud7r）スコープ外。

**Tech Stack:** Rust, assert_cmd + predicates（CLI 統合テスト）, tempfile。

**Design:** beads issue `flpdf-tc3e` の design フィールド参照。

**qpdf 11.9.0 golden（実機観測済み）:**

```text
# 修復警告（exit 3）— トリガー警告のみ (offset N) 付き
WARNING: broken-startxref.pdf: file is damaged
WARNING: broken-startxref.pdf (offset 999): xref not found
WARNING: broken-startxref.pdf: Attempting to reconstruct cross-reference table
<stdout: checking ... ブロック>
qpdf: operation succeeded with warnings

# check 中のエラー（/Root 欠落、exit 2）— 単一行、"PDF check failed" 相当行なし
qpdf: noroot.pdf: unable to find /Root dictionary

# 致命的 open エラー（exit 2）
qpdf: notpdf.pdf: unable to find trailer dictionary while recovering damaged file
```

**観測による設計確定事項（bd design からの更新）:** check 中の Error severity 診断（形式2）は `<file>: <msg>` ではなく **`<progname>: <file>{ (offset N)}: <msg>`** とし、`flpdf: PDF check failed` の末尾行は**削除**する（qpdf は出さない）。

---

## Task 1: library — repair 3 警告の offset を qpdf に合わせる

qpdf は `file is damaged`（#1）と `Attempting to reconstruct...`（#3）に offset を表示しない（内部 offset 0 = 非表示）。flpdf は現在 3 警告すべてに `Some(startxref)` を付けている。#1/#3 を `None` に変更する。

**Files:**
- Modify: `crates/flpdf/tests/xref_tests.rs`（3 つの既存テスト: `repair_reports_*` 2 件 + `with_repair_appends_diagnostic_when_stream_parse_succeeds`、690〜840 行付近）
- Modify: `crates/flpdf/src/xref.rs:330-352`（`push_repair_diagnostics` とその doc コメント）

**Step 1: 失敗するテストを書く**

`crates/flpdf/tests/xref_tests.rs` の 3 テストそれぞれで、トリガー警告 offset の既存 assert の直後に追加:

```rust
    // qpdf prints no offset for the surrounding warnings (#1 and #3); only
    // the trigger warning carries one.
    assert_eq!(loaded.repair_diagnostics.entries()[0].offset, None);
    assert_eq!(loaded.repair_diagnostics.entries()[2].offset, None);
```

挿入位置（3 箇所とも「トリガー警告の offset を assert している `assert_eq!(...entries()[1].offset, ...)` の直後」）:
1. `repair_emits_qpdf_warning_sequence_for_missing_startxref`（`Some(file_len)` assert の後、~720 行）
2. `repair_reports_non_parse_trigger_error_via_display`（`Some(xref_offset)` assert の後、~770 行）
3. `with_repair_appends_diagnostic_when_stream_parse_succeeds` には offset assert がないので、`messages` assert の後に entries[0]/[1]/[2] の offset assert を 3 行とも追加（[1] は `None` ではなく値があることだけ確認: `assert!(loaded.repair_diagnostics.entries()[1].offset.is_some());`）

**Step 2: テストが失敗することを確認**

Run: `cargo test -p flpdf --test xref_tests repair -- --nocapture` および `cargo test -p flpdf --test xref_tests with_repair_appends`
Expected: FAIL（`entries()[0].offset` が `Some(...)` で `None` と不一致）

**Step 3: 最小実装**

`crates/flpdf/src/xref.rs` の `push_repair_diagnostics`:

```rust
fn push_repair_diagnostics(diagnostics: &mut Diagnostics, trigger_error: &Error, startxref: u64) {
    diagnostics.push(Diagnostic::warning("file is damaged", None));
    let (message, offset) = match trigger_error {
        Error::Parse { offset, message } => (message.clone(), Some(*offset as u64)),
        other => (other.to_string(), Some(startxref)),
    };
    diagnostics.push(Diagnostic::warning(message, offset));
    diagnostics.push(Diagnostic::warning(
        "Attempting to reconstruct cross-reference table",
        None,
    ));
}
```

doc コメント末尾の「the surrounding warnings carry the `startxref` offset.」を更新:

```text
/// offset when available; the surrounding warnings carry no offset, matching
/// qpdf, which reports them at offset 0 and suppresses the display.
```

**Step 4: テストが通ることを確認**

Run: `cargo test -p flpdf --test xref_tests`
Expected: PASS（全件）

**Step 5: library 全テスト + commit**

Run: `cargo test -p flpdf -q 2>&1 | grep -E "test result|FAILED"`
Expected: 全 suite ok

```bash
git add crates/flpdf/src/xref.rs crates/flpdf/tests/xref_tests.rs
git commit -m "fix(flpdf): omit offset on surrounding xref-repair warnings (flpdf-tc3e)"
```

---

## Task 2: CLI — progname()/diagnostic_location() + run_check 警告レンダリング + 末尾サマリ行

**Files:**
- Modify: `crates/flpdf-cli/tests/cli_check_exitcodes.rs`
- Modify: `crates/flpdf-cli/src/main.rs`（`run_check` ~2002 行、ヘルパーは `actionable_password_error` 付近 ~3939 行に追加）

**Step 1: 失敗するテストを書く**

`cli_check_exitcodes.rs` の既存 exit-3 テスト 2 件（`check_warnings_only_pdf_exits_3` / `check_subcommand_warnings_only_pdf_exits_3`）の `.stderr(predicate::str::contains("warning"))` を qpdf 形式の assert に置換し、新規テストを追加:

```rust
#[test]
fn check_warnings_use_qpdf_stderr_format() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&warnings_only_corrupt_xref_bytes()).unwrap();
    let path = f.path().to_str().unwrap().to_string();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--check", "--repair", &path])
        .assert()
        .code(3)
        .stdout(predicate::str::contains("PDF check succeeded"))
        // qpdf shape: WARNING: <file>: <msg>, surrounding warnings without
        // offset, then the trailing summary line.
        .stderr(predicate::str::contains(format!(
            "WARNING: {path}: file is damaged\n"
        )))
        .stderr(predicate::str::contains(
            "Attempting to reconstruct cross-reference table\n",
        ))
        .stderr(predicate::str::contains(
            "flpdf: operation succeeded with warnings\n",
        ))
        // The old lowercase `warning: <msg>` prefix must be gone.
        .stderr(predicate::str::contains("warning: ").not());
}

/// The trigger warning (and only the trigger warning) carries `(offset N)`.
#[test]
fn check_trigger_warning_carries_offset() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&warnings_only_corrupt_xref_bytes()).unwrap();
    let path = f.path().to_str().unwrap().to_string();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--check", "--repair", &path])
        .assert()
        .code(3)
        .stderr(predicate::str::is_match(format!(
            "WARNING: {} \\(offset \\d+\\): ",
            regex::escape(&path)
        )).unwrap())
        .stderr(predicate::str::contains(format!(
            "WARNING: {path} (offset"
        )).count(1));
}
```

既存 exit-3 テスト 2 件は `.stderr(predicate::str::contains("WARNING: "))` に変更。

注: `regex::escape` を使うため `crates/flpdf-cli/Cargo.toml` の `[dev-dependencies]` に `regex` が無ければ追加（`predicates` の `is_match` は内部 regex なので `regex = "1"` を dev-dep に追加するだけ）。すでにあれば不要。tempfile のパスに regex メタ文字はまず入らないが、エスケープしておくのが安全。

**Step 2: テストが失敗することを確認**

Run: `cargo test -p flpdf-cli --test cli_check_exitcodes`
Expected: FAIL（現行は `warning: <msg>` 形式・末尾行なし）

**Step 3: 実装**

`crates/flpdf-cli/src/main.rs` にヘルパー追加（`actionable_password_error` の直前あたり）:

```rust
/// Program name used in qpdf-parity diagnostic prefixes.
///
/// `FLPDF_PROGNAME` overrides the default so the qpdf qtest harness shim can
/// present flpdf as `qpdf` (the shim exports `FLPDF_PROGNAME=qpdf`); unset
/// or empty, the prefix is always `flpdf`.
fn progname() -> String {
    std::env::var("FLPDF_PROGNAME")
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "flpdf".to_string())
}

/// Render the `<file>` / `<file> (offset N)` location part shared by the
/// qpdf-shaped warning and error diagnostic lines (qpdf 11.9.0 observed
/// format; qpdf suppresses the offset display when it is unknown).
fn diagnostic_location(input: &Path, offset: Option<u64>) -> String {
    match offset {
        Some(offset) => format!("{} (offset {offset})", input.display()),
        None => input.display().to_string(),
    }
}
```

（`use std::path::Path;` が無ければ既存 import に追加。`PathBuf` は既に import 済みのはず。）

`run_check` の診断ループを置換（エラー側は Task 3 で完成させるため、この時点では警告アームのみ qpdf 形式、エラーアームは現状の `error: <msg>` のまま残してよい — ただし match 構造は先に入れる）:

```rust
    for diagnostic in report.diagnostics.entries() {
        let location = diagnostic_location(&input, diagnostic.offset);
        match diagnostic.severity {
            Severity::Warning => eprintln!("WARNING: {location}: {}", diagnostic.message),
            Severity::Error => eprintln!("error: {}", diagnostic.message),
        }
    }
```

exit-3 パス（`println!("PDF check succeeded");` の直後、`return Err(...)` の前）に追加:

```rust
        eprintln!("{}: operation succeeded with warnings", progname());
```

**Step 4: テストが通ることを確認**

Run: `cargo test -p flpdf-cli --test cli_check_exitcodes`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/flpdf-cli/src/main.rs crates/flpdf-cli/tests/cli_check_exitcodes.rs crates/flpdf-cli/Cargo.toml
git commit -m "feat(cli): qpdf-format WARNING lines and trailing summary in check (flpdf-tc3e)"
```

---

## Task 3: CLI — check 中の Error severity を qpdf 形式に + "PDF check failed" 行の削除

qpdf golden（/Root 欠落）: `qpdf: noroot.pdf: unable to find /Root dictionary` のみ、exit 2。

**Files:**
- Modify: `crates/flpdf-cli/tests/cli_check_exitcodes.rs`
- Modify: `crates/flpdf-cli/src/main.rs`（`run_check` のエラーアームと exit-2 パス）

**Step 1: 失敗するテストを書く**

`cli_check_exitcodes.rs` にフィクスチャとテストを追加:

```rust
/// Valid xref but the trailer lacks /Root — opens fine, check reports an
/// error-severity diagnostic → exit 2.
fn missing_root_pdf_bytes() -> Vec<u8> {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n");
    let off1 = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let off2 = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");
    let xref_start = pdf.len();
    pdf.extend_from_slice(
        format!("xref\n0 3\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n")
            .as_bytes(),
    );
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 3 >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    pdf
}

/// qpdf prints check errors as a single `<progname>: <file>: <msg>` line and
/// no extra "check failed" summary (observed with qpdf 11.9.0 on the same
/// fixture: `qpdf: noroot.pdf: unable to find /Root dictionary`).
#[test]
fn check_error_diagnostics_use_qpdf_stderr_format() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&missing_root_pdf_bytes()).unwrap();
    let path = f.path().to_str().unwrap().to_string();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--check", &path])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(format!(
            "flpdf: {path}: trailer is missing /Root\n"
        )))
        .stderr(predicate::str::contains("PDF check failed").not())
        .stderr(predicate::str::contains("error: ").not());
}
```

**Step 2: テストが失敗することを確認**

Run: `cargo test -p flpdf-cli --test cli_check_exitcodes check_error_diagnostics`
Expected: FAIL（現行は `error: trailer is missing /Root` + `flpdf: PDF check failed`）

**Step 3: 実装**

`run_check` のエラーアームを置換:

```rust
            Severity::Error => {
                eprintln!("{}: {location}: {}", progname(), diagnostic.message)
            }
```

exit-2 パスの `CliExitError` を空メッセージに（診断行が既に qpdf 形式で出ているため、main は追加の行を出さない）:

```rust
    if !report.valid {
        // Errors found — exit 2.  The error diagnostics above are already in
        // qpdf shape; qpdf prints no extra summary line in this case.
        return Err(Box::new(CliExitError {
            code: ExitCode::Errors,
            message: String::new(),
        }));
    }
```

**Step 4: テストが通ることを確認**

Run: `cargo test -p flpdf-cli --test cli_check_exitcodes`
Expected: PASS（既存の exit-2 テスト 2 件は exit code のみの assert なので影響なし）

**Step 5: Commit**

```bash
git add crates/flpdf-cli/src/main.rs crates/flpdf-cli/tests/cli_check_exitcodes.rs
git commit -m "feat(cli): qpdf-format error lines in check, drop 'PDF check failed' (flpdf-tc3e)"
```

---

## Task 4: CLI — 致命的 open エラーへのファイル名挿入 + main() の progname + FLPDF_PROGNAME 切替

qpdf golden: `qpdf: notpdf.pdf: unable to find trailer dictionary while recovering damaged file`。現行 flpdf: `flpdf: parse error at byte 999: ...`（ファイル名なし）。

**Files:**
- Modify: `crates/flpdf-cli/tests/cli_check_exitcodes.rs`
- Modify: `crates/flpdf-cli/src/main.rs`（`run_check` の open エラー、`open_pdf` の open エラー、main() の結果ハンドラ 1544-1570 行）

**Step 1: 失敗するテストを書く**

```rust
/// Fatal open errors carry the input path: `<progname>: <file>: <msg>`
/// (observed qpdf shape: `qpdf: notpdf.pdf: unable to find trailer
/// dictionary while recovering damaged file`).
#[test]
fn fatal_open_error_includes_filename() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&corrupt_pdf_bytes()).unwrap();
    let path = f.path().to_str().unwrap().to_string();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--check", &path])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(format!("flpdf: {path}: ")));
}

/// FLPDF_PROGNAME swaps the program-name prefix (qtest harness shim sets
/// FLPDF_PROGNAME=qpdf); diagnostics are otherwise identical.
#[test]
fn flpdf_progname_env_swaps_prefix() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&warnings_only_corrupt_xref_bytes()).unwrap();
    let path = f.path().to_str().unwrap().to_string();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.env("FLPDF_PROGNAME", "qpdf")
        .args(["--check", "--repair", &path])
        .assert()
        .code(3)
        .stderr(predicate::str::contains(
            "qpdf: operation succeeded with warnings\n",
        ))
        .stderr(predicate::str::contains("flpdf:").not());
}
```

**Step 2: テストが失敗することを確認**

Run: `cargo test -p flpdf-cli --test cli_check_exitcodes fatal_open_error flpdf_progname`
Expected: FAIL

**Step 3: 実装**

ヘルパー追加（`diagnostic_location` の隣）:

```rust
/// Prefix a fatal error with the input path so main() renders the observed
/// qpdf shape `<progname>: <file>: <msg>` for open failures.
fn error_with_file(input: &Path, error: Box<dyn std::error::Error>) -> Box<dyn std::error::Error> {
    format!("{}: {error}", input.display()).into()
}
```

`run_check` の open:

```rust
    let report = check_reader_with_options(BufReader::new(file), options)
        .map_err(|error| error_with_file(&input, actionable_password_error(error)))?;
```

同様に `File::open(input)?` も `.map_err(...)` でファイル名付与（`File::open(&input).map_err(|e| error_with_file(&input, e.into()))?`）。

`open_pdf` の open も同形（`Pdf::open_with_options(...).map_err(|error| error_with_file(input, actionable_password_error(error)))?` と `File::open(input).map_err(|e| error_with_file(input, e.into()))?`）。
**注意:** `open_pdf` のラップは open エラーのみ。`Error::Signed` は書き込みパス（`run_rewrite` 以降）で発生するため main() の downcast には影響しない。

main() 結果ハンドラの 3 箇所（1553 / 1565 / 1568 行付近）の固定 `flpdf:` を `progname()` に:

```rust
                eprintln!("{}: {}", progname(), exit_err.message);
...
            eprintln!("{}: {message}", progname());
...
        eprintln!("{}: {error}", progname());
```

（dispatch 前の usage エラー `flpdf: --qdf and --linearize cannot be used together` 等はスコープ外、変更しない。）

**Step 4: テストが通ることを確認**

Run: `cargo test -p flpdf-cli --test cli_check_exitcodes`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/flpdf-cli/src/main.rs crates/flpdf-cli/tests/cli_check_exitcodes.rs
git commit -m "feat(cli): filename in fatal open errors, FLPDF_PROGNAME prefix override (flpdf-tc3e)"
```

---

## Task 5: CLI — open_pdf（rewrite 等の全サブコマンド）の警告を qpdf 形式に

**Files:**
- Modify: `crates/flpdf-cli/tests/cli_check_exitcodes.rs`（または既存の repair 警告を assert している統合テスト）
- Modify: `crates/flpdf-cli/src/main.rs`（`open_pdf` ~3887 行）

**Step 1: 失敗するテストを書く**

```rust
/// Repair warnings emitted while opening for any subcommand (here: rewrite)
/// use the same qpdf shape as check.
#[test]
fn rewrite_repair_warnings_use_qpdf_stderr_format() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&warnings_only_corrupt_xref_bytes()).unwrap();
    let path = f.path().to_str().unwrap().to_string();
    let out = tempfile::NamedTempFile::new().unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "rewrite",
        "--repair",
        &path,
        out.path().to_str().unwrap(),
    ])
    .assert()
    .success()
    .stderr(predicate::str::contains(format!(
        "WARNING: {path}: file is damaged\n"
    )))
    .stderr(predicate::str::contains("warning: ").not());
}
```

（`rewrite` の引数形は既存テスト `cli_tests.rs` / `cli_full_rewrite.rs` の呼び出し例に合わせて調整。）

**Step 2: テストが失敗することを確認**

Run: `cargo test -p flpdf-cli --test cli_check_exitcodes rewrite_repair_warnings`
Expected: FAIL（現行は `warning: <msg>`）

**Step 3: 実装**

`open_pdf` の警告ループと weak-crypto 警告を置換:

```rust
    for diagnostic in pdf.repair_diagnostics().entries() {
        eprintln!(
            "WARNING: {}: {}",
            diagnostic_location(input, diagnostic.offset),
            diagnostic.message
        );
    }
    if pdf.uses_weak_crypto() {
        eprintln!(
            "WARNING: {}: encrypted PDF uses weak crypto; processing because --allow-weak-crypto was supplied",
            input.display()
        );
    }
```

**Step 4: テストが通ることを確認 + 既存テストの巻き添え確認**

Run: `cargo test -p flpdf-cli -q 2>&1 | grep -E "test result|FAILED"`
Expected: 全 suite ok。失敗があれば旧 `warning: ` 形式を assert しているテストなので qpdf 形式に更新する（`grep -rn '"warning' crates/flpdf-cli/tests/` で残存を確認）。

**Step 5: Commit**

```bash
git add crates/flpdf-cli/src/main.rs crates/flpdf-cli/tests/
git commit -m "feat(cli): qpdf-format repair warnings in open_pdf path (flpdf-tc3e)"
```

---

## Task 6: 仕上げ — 全テスト・fmt・clippy・patch-coverage ゲート・qpdf 実機突き合わせ

**Step 1: qpdf 実機との形式突き合わせ（手動 smoke）**

```bash
cd /tmp && printf '%%PDF-1.4\n...' # 会話中の broken-startxref.pdf 再生成 or 既存を再利用
qpdf --check --repair /tmp/broken-startxref.pdf 2>&1 | head -5
FLPDF_PROGNAME=qpdf target/debug/flpdf --check --repair /tmp/broken-startxref.pdf 2>&1 | head -5
```

確認: プレフィクス（`WARNING: <path>`）・`(offset N)` の位置・行順序・末尾行が qpdf と同形（**文言の差は Stage 3 なので無視**）。

**Step 2: 品質ゲート**

```bash
cargo check --workspace
cargo test --workspace -q 2>&1 | grep -E "test result: FAILED|failures" ; echo "done"
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all && cargo fmt --all --check
```

Expected: check ok / テスト全件 ok / clippy 警告なし / fmt clean

**Step 3: commit してから patch-coverage ゲート**（dirty tree はエラーになる）

```bash
git status --short   # クリーンであること（Task 1-5 で commit 済み）
scripts/patch-coverage.sh --base origin/main
```

Expected: `flpdf` 変更行 100%（xref.rs の変更は Task 1 のテストでカバー）。`flpdf-cli` は報告のみ — 未カバー行が出たら可能な範囲でテスト追加を検討。

**Step 4: beads design フィールドの確定事項を反映 + issue note**

```bash
bd update flpdf-tc3e --notes "実装完了。観測により形式2を確定: Error severity は '<progname>: <file>{ (offset N)}: <msg>'、'PDF check failed' 行は削除（qpdf は出さない）。品質ゲート green。"
```

**Step 5: 最終 commit（残差分があれば）**

```bash
git status --short
git add -A && git commit -m "chore: format/cleanup (flpdf-tc3e)"  # 差分がある場合のみ
```
