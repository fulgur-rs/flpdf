# xref Repair Diagnostics — qpdf Warning Sequence Parity (flpdf-ny1f)

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** xref repair 時の診断を qpdf 11.9.0 の実出力（3警告シーケンス）に揃え、「linear scan が走っていないのに linear object scan と表示する」バグ（flpdf-ny1f）を解消する。

**Architecture:** `format_repair_diagnostic`（単一合成メッセージ）を廃止し、qpdf の `reconstruct_xref`（`QPDF_objects.cc`）と同じ3警告 `file is damaged` / `<トリガーエラー>` / `Attempting to reconstruct cross-reference table` を個別 `Diagnostic` として両復旧パス（offset-0 再パース成功 / linear scan）で emit する。qpdf は復旧手段を区別しないので、パスを区別しないことが parity 的に正しい。あわせて `missing startxref` を qpdf 文言 `can't find startxref` に変更する。

**Tech Stack:** Rust / cargo。golden は実機 qpdf 11.9.0（`/usr/bin/qpdf`）の stderr:

```
WARNING: <file>: file is damaged
WARNING: <file>: can't find startxref
WARNING: <file>: Attempting to reconstruct cross-reference table
```

（exit=3。`WARNING: <file>: ` プレフィクスは Stage 2 = flpdf-tc3e のスコープ。本 plan は message 本文のみ。）

**Worktree:** `/home/ubuntu/flpdf/.worktrees/flpdf-ny1f`（branch `fix/flpdf-ny1f-xref-repair-diag-qpdf-parity`）

**beads:** flpdf-ny1f（design/acceptance はそちらを参照）。後続: flpdf-tc3e（CLI 形式）、flpdf-ud7r（全復旧パス監査）。

---

### Task 1: `missing startxref` → `can't find startxref`

**Files:**
- Modify: `crates/flpdf/src/xref.rs:672`
- Test: `crates/flpdf/tests/xref_tests.rs:682-684, 708, 753, 757`

**Step 1: テスト assertion を新文言に更新（failing にする）**

`crates/flpdf/tests/xref_tests.rs` の2箇所の assertion を変更:

- :708（`repair_diagnostic_aggregates_multiple_errors` 内）:
```rust
    assert!(
        message.contains("can't find startxref"),
        "expected first parse error, got {message}"
    );
```
- :757（`with_repair_appends_diagnostic_when_stream_parse_succeeds` 内）:
```rust
    assert!(
        message.contains("can't find startxref"),
        "expected the missing-startxref clause, got {message}"
    );
```
- doc コメント :682 と :753 の `"missing startxref"` 文字列も `"can't find startxref"` に更新。

**Step 2: 失敗確認**

Run: `cargo test -p flpdf --test xref_tests repair -- --nocapture 2>&1 | tail -20`
Expected: 上記2テストが FAIL（メッセージはまだ `missing startxref`）。

**Step 3: src を変更**

`crates/flpdf/src/xref.rs:672`:
```rust
        return Err(Error::parse(bytes.len(), "can't find startxref"));
```

**Step 4: パス確認**

Run: `cargo test -p flpdf --test xref_tests && cargo test -p flpdf --test writer_tests`
Expected: 全パス（writer_tests の `missing startxref` はテストヘルパーの panic 文言で無関係、変更しない）。

**Step 5: Commit**

```bash
git add crates/flpdf/src/xref.rs crates/flpdf/tests/xref_tests.rs
git commit -m "fix(flpdf): align startxref-missing parse error wording with qpdf (flpdf-ny1f)"
```

---

### Task 2: qpdf 3警告シーケンスへの置き換え

**Files:**
- Modify: `crates/flpdf/src/xref.rs:118-123, 199-222, 332-351`
- Test: `crates/flpdf/tests/xref_tests.rs`（4テスト + コメント :1401-1403）、`crates/flpdf/tests/check_tests.rs:131-142`

**Step 1: `with_repair_appends_diagnostic_when_stream_parse_succeeds` を新シーケンス仕様に書き換え（failing にする）**

xref_tests.rs:719-771 のテストを以下に変更（fixture 構築部分はそのまま、doc コメントと assertion を差し替え）:

```rust
/// When `startxref` is absent but the FIRST indirect object in the file is
/// itself a valid xref stream with no `/Prev`, repair pushes a single "can't
/// find startxref" error and resets the retry offset to 0. `parse_xref_from_start`
/// then skips the `%PDF-` header comment and parses that xref stream
/// successfully, so the accumulated-error warning arm runs. The emitted
/// diagnostics are the same three-warning sequence qpdf produces for this
/// input (qpdf does not distinguish recovery methods), and in particular must
/// NOT claim a linear object scan ran: the stream parse keeps
/// `XrefForm::Stream`, whereas a linear scan would force `XrefForm::Table`.
#[test]
fn with_repair_appends_diagnostic_when_stream_parse_succeeds() {
    // ...（fixture 構築は現状のまま）...

    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes)).unwrap();

    // The xref STREAM parse succeeded (not a linear scan, which sets Table).
    assert_eq!(loaded.last_xref_form, XrefForm::Stream);

    // The qpdf-compatible warning sequence, one diagnostic per line.
    let messages: Vec<&str> = loaded
        .repair_diagnostics
        .entries()
        .iter()
        .map(|entry| entry.message.as_str())
        .collect();
    assert_eq!(
        messages,
        [
            "file is damaged",
            "can't find startxref",
            "Attempting to reconstruct cross-reference table",
        ],
        "expected the qpdf warning sequence"
    );
    assert!(
        !messages.iter().any(|m| m.contains("linear object scan")),
        "must not claim a linear scan ran: {messages:?}"
    );

    // The stream's own entries are present (e.g. object 1 at its offset).
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefOffset::Offset(xref_offset))
    );
    assert_eq!(loaded.trailer.get_ref("Root"), Some(ObjectRef::new(1, 0)));
}
```

**Step 2: 失敗確認**

Run: `cargo test -p flpdf --test xref_tests with_repair_appends_diagnostic_when_stream_parse_succeeds`
Expected: FAIL（現状は単一合成メッセージ）。

**Step 3: src 実装**

`crates/flpdf/src/xref.rs` — `format_repair_diagnostic`（:332-351）を削除し、以下に置き換え:

```rust
/// Push the qpdf-compatible repair warning sequence onto `diagnostics`.
///
/// qpdf (`reconstruct_xref` in `QPDF_objects.cc`, observed with qpdf 11.9.0)
/// emits the same three warnings regardless of how the damaged
/// cross-reference data is ultimately recovered: `file is damaged`, the error
/// that triggered recovery, and `Attempting to reconstruct cross-reference
/// table`. Only the first accumulated error is reported: qpdf has no retry-at-
/// offset-0 detour, so follow-up failures from that path have no qpdf
/// counterpart on stderr.
fn push_repair_diagnostics(
    diagnostics: &mut Diagnostics,
    parse_errors: &[Error],
    startxref: u64,
) {
    diagnostics.push(Diagnostic::warning("file is damaged", Some(startxref)));
    if let Some(error) = parse_errors.first() {
        let message = match error {
            Error::Parse { message, .. } => message.clone(),
            other => other.to_string(),
        };
        diagnostics.push(Diagnostic::warning(message, Some(startxref)));
    }
    diagnostics.push(Diagnostic::warning(
        "Attempting to reconstruct cross-reference table",
        Some(startxref),
    ));
}
```

呼び出し元1（:118-123、offset-0 再パース成功パス）:
```rust
    if !parse_errors.is_empty() {
        push_repair_diagnostics(&mut loaded.repair_diagnostics, &parse_errors, startxref);
    }
```

呼び出し元2（`recover_xref_from_linear_scan`、:208-212）:
```rust
    let mut repair_diagnostics = Diagnostics::default();
    push_repair_diagnostics(&mut repair_diagnostics, &parse_errors, startxref);
```
（`parse_errors: Vec<Error>` 引数は `&parse_errors` で渡すだけ。シグネチャ変更不要。）

注意（レビュー規約）: `Error::Parse { message, .. }` の `message.clone()` は借用元 `&[Error]` から所有 String を得るために必要なクローン（許容）。

**Step 4: 対象テストのパス確認**

Run: `cargo test -p flpdf --test xref_tests with_repair_appends_diagnostic_when_stream_parse_succeeds`
Expected: PASS

**Step 5: 残りの旧文言テストを更新**

1. `repair_diagnostic_aggregates_multiple_errors`（xref_tests.rs:684-717）→ 「先頭エラーのみ報告」仕様に改名・書き換え（fixture と entries/trailer assert は維持）:

```rust
/// When `startxref` is absent, repair pushes a "can't find startxref" error
/// and retries `parse_xref_from_start` at offset 0, which fails at the header
/// and pushes a second error. Only the first (triggering) error appears in the
/// warning sequence: qpdf has no offset-0 retry, so the follow-up failure has
/// no counterpart in qpdf's stderr for the same input.
#[test]
fn repair_diagnostics_report_only_the_triggering_error() {
    // ...（fixture 構築は現状のまま）...

    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes)).unwrap();

    let messages: Vec<&str> = loaded
        .repair_diagnostics
        .entries()
        .iter()
        .map(|entry| entry.message.as_str())
        .collect();
    assert_eq!(
        messages,
        [
            "file is damaged",
            "can't find startxref",
            "Attempting to reconstruct cross-reference table",
        ],
        "expected the qpdf warning sequence with only the first error"
    );

    // Recovery still produced usable entries and a trailer.
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(&XrefOffset::Offset(9))
    );
    assert_eq!(loaded.trailer.get_ref("Root"), Some(ObjectRef::new(1, 0)));
}
```

2. `best_effort_recovers_from_corrupt_xref_data`（xref_tests.rs:256-）:
```rust
    assert_eq!(loaded.repair_diagnostics.entries().len(), 3);
    assert!(loaded
        .repair_diagnostics
        .entries()
        .iter()
        .any(|entry| entry.message == "Attempting to reconstruct cross-reference table"));
```

3. merge-fallback テスト（xref_tests.rs:838-858 付近）— contains 差し替え:
```rust
    // Best-effort records the error and recovers via the linear object scan,
    // emitting the qpdf-compatible warning sequence.
    ...
    assert!(
        loaded
            .repair_diagnostics
            .entries()
            .iter()
            .any(|entry| entry.message == "Attempting to reconstruct cross-reference table"),
        "expected the qpdf reconstruction warning"
    );
```

4. `check_tests.rs:131-142` `check_reports_repaired_xref_warning`:
```rust
        .any(|entry| entry.severity == Severity::Warning
            && entry.message == "Attempting to reconstruct cross-reference table")));
```

5. xref_tests.rs:1401-1403 のコメント中 `format_repair_diagnostic` 参照を `push_repair_diagnostics` に更新。

**Step 6: テスト全体のパス確認**

Run: `cargo test -p flpdf --test xref_tests && cargo test -p flpdf --test check_tests`
Expected: 全パス。さらに `grep -rn "linear object scan\|missing startxref" crates/flpdf/src/` が 0 件であること。

**Step 7: Commit**

```bash
git add crates/flpdf/src/xref.rs crates/flpdf/tests/xref_tests.rs crates/flpdf/tests/check_tests.rs
git commit -m "fix(flpdf): emit qpdf-compatible warning sequence for xref repair (flpdf-ny1f)"
```

---

### Task 3: 実機 qpdf との golden 比較・仕上げ

**Files:** なし（検証のみ。差分があれば修正）

**Step 1: 実機比較**

```bash
cd /tmp && printf '%%PDF-1.7\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\ntrailer\n<< /Size 4 /Root 1 0 R >>\n%%%%EOF\n' > no_startxref.pdf
qpdf --check no_startxref.pdf 2>&1 | head -3
cd /home/ubuntu/flpdf/.worktrees/flpdf-ny1f && cargo run -p flpdf-cli -- --check /tmp/no_startxref.pdf 2>&1 | head -5
```

Expected: qpdf の3警告の **message 本文** と flpdf の `warning: <msg>` 3行の本文が一致（`WARNING: <file>: ` プレフィクス差は flpdf-tc3e のスコープ）。flpdf の exit code は qpdf と同じ 3。

**Step 2: 品質ゲート**

Run: `cargo fmt --check && cargo test --workspace 2>&1 | grep -cE "test result: ok" && cargo test --workspace 2>&1 | grep -E "FAILED|[1-9][0-9]* failed" | head`
Expected: fmt クリーン、80 スイート ok、failed 行なし。

**Step 3: doc 整合確認**

`crates/flpdf/src/xref.rs` の公開 doc（`load_xref_and_trailer*` の `# Errors` 等）に旧文言依存がないこと、新規 doc が英語・issue ID なしであることを `.claude/rules/pdf-rust-doc-review-patterns.md` の grep で確認:

```bash
grep -rnE '(///|//!).*flpdf-[0-9a-z.]+' crates/flpdf/src/xref.rs
```
Expected: 0 件。

**Step 4: Commit（差分があれば）**

```bash
git add -A && git commit -m "test(flpdf): golden-verify xref repair warnings against qpdf 11.9.0 (flpdf-ny1f)"
```
