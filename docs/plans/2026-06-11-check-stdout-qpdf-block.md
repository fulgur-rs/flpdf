# `flpdf check` stdout → qpdf checking block (flpdf-l3jx)

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Migrate `flpdf check` / `flpdf --check` stdout from the bespoke
`PDF check succeeded` line to qpdf 11.9.0's `checking` block
(`checking <file>` / `PDF Version` / encryption status / linearization status /
trailing reassurance line), preserving the existing exit codes and stderr format.

**Architecture:** Surface the document summary the CLI needs (version, encrypted,
linearized) on `CheckReport` so the CLI does not re-open the file. The CLI prints
the block only when `report.valid` (matching qpdf, which prints nothing to stdout
when document init fails on exit 2). Encrypted files get an interim `File is
encrypted` line; the detailed `R = / P =` block is deferred to flpdf-oox1.

**Tech Stack:** Rust workspace (`crates/flpdf` library, `crates/flpdf-cli` binary),
`assert_cmd` / `predicates` for CLI tests.

**Observed qpdf 11.9.0 stdout (ground truth):**
- exit 0 (clean): `checking <f>` / `PDF Version: X.Y` / `File is not encrypted` /
  `File is not linearized` / `No syntax or stream encoding errors found; the file may still contain` / `errors that qpdf cannot detect`
- exit 3 (warnings): same block WITHOUT the trailing two lines (warnings → stderr)
- exit 2 (unrecoverable open OR valid-xref-but-missing-/Root): stdout EMPTY
- encrypted: `File is not encrypted` is replaced by an `R = / P = / permissions / methods` block (deferred)
- linearized: `File is linearized`

`<file>` is echoed verbatim as passed on the CLI. flpdf swaps the trailing-line
subject `qpdf` → `progname()` (byte-identical under `FLPDF_PROGNAME=qpdf`).

---

### Task 1: Extend `CheckReport` with a document summary (library)

**Files:**
- Modify: `crates/flpdf/src/check.rs` (struct `CheckReport` ~line 15; both
  `CheckReport { .. }` constructors ~line 111 and ~line 138)
- Modify: `crates/flpdf/src/lib.rs` (re-export `CheckSummary` alongside `CheckReport`)

**Step 1: Write failing unit tests in `check.rs` (`#[cfg(test)]`)**

```rust
#[test]
fn summary_present_for_clean_document() {
    let report = check_reader_strict(Cursor::new(minimal_clean_pdf())).unwrap();
    let s = report.summary.expect("summary present when document opens");
    assert_eq!(s.version, "1.7");
    assert!(!s.encrypted);
    assert!(!s.linearized);
}

#[test]
fn summary_none_when_open_fails() {
    // Garbage with a header but no recoverable structure.
    let report = check_reader(Cursor::new(b"%PDF-1.4\nnot a pdf\n%%EOF\n".to_vec())).unwrap();
    assert!(!report.valid);
    assert!(report.summary.is_none());
}
```
(Reuse an existing in-crate minimal-PDF helper if present; otherwise inline bytes
with a `%PDF-1.7` header. Check existing `check.rs` tests for a helper first.)

**Step 2: Run — expect FAIL** (`summary` field does not exist).
Run: `cargo test -p flpdf --lib check::`

**Step 3: Implement**

Add the public type + field:
```rust
/// Document-level summary captured by [`check_reader`] when the input opened.
///
/// Backs a `qpdf --check`-style banner (header version, encryption and
/// linearization status) without re-opening the document.
#[derive(Debug, Clone)]
pub struct CheckSummary {
    /// PDF version from the file header, e.g. `"1.7"` (no `%PDF-` prefix).
    pub version: String,
    /// Whether the document authenticated an `/Encrypt` dictionary on open.
    pub encrypted: bool,
    /// Whether the document carries a linearization hint object.
    pub linearized: bool,
}
```
Add to `CheckReport`:
```rust
    /// Document summary, or `None` when the open path failed before a document
    /// object existed (e.g. an unrecoverable parse error).
    pub summary: Option<CheckSummary>,
```
Open-failure early return → `summary: None`. Success path: capture the
`is_linearized_pdf` bool once, reuse it for both the warning push and the summary:
```rust
    let linearized = is_linearized_pdf(&mut pdf)?;
    if linearized { /* existing warning push */ }
    let summary = CheckSummary {
        version: pdf.version().to_string(),
        encrypted: pdf.is_encrypted(),
        linearized,
    };
    Ok(CheckReport { valid: !diagnostics.has_errors(), diagnostics, summary: Some(summary) })
```
Re-export in `lib.rs`: add `CheckSummary` next to the existing `CheckReport` export.

**Step 4: Run — expect PASS.** Run: `cargo test -p flpdf --lib check::`
Also `cargo build -p flpdf` to confirm no other `CheckReport { .. }` literal broke.

**Step 5: Commit** `git commit -m "feat(flpdf): CheckReport carries document summary (version/encrypted/linearized) (flpdf-l3jx)"`

---

### Task 2: Emit the qpdf block from `run_check` (CLI)

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs` `run_check` (~lines 2004-2058); add a
  private `print_check_block` helper near it.

**Step 1: Write failing CLI tests** in `crates/flpdf-cli/tests/cli_check_exitcodes.rs`
(new tests — do not yet touch existing assertions):
```rust
#[test]
fn check_clean_pdf_emits_qpdf_block() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&clean_pdf_bytes()).unwrap();
    let path = f.path().to_str().unwrap().to_string();
    Command::cargo_bin("flpdf").unwrap()
        .args(["--check", &path]).assert().code(0)
        .stdout(predicate::str::contains(format!("checking {path}\n")))
        .stdout(predicate::str::contains("PDF Version: "))
        .stdout(predicate::str::contains("File is not encrypted\n"))
        .stdout(predicate::str::contains("File is not linearized\n"))
        .stdout(predicate::str::contains(
            "No syntax or stream encoding errors found; the file may still contain\nerrors that flpdf cannot detect\n"))
        .stdout(predicate::str::contains("PDF check succeeded").not());
}

#[test]
fn check_warnings_emit_block_without_trailing_line() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&warnings_only_corrupt_xref_bytes()).unwrap();
    Command::cargo_bin("flpdf").unwrap()
        .args(["--check", "--repair", f.path().to_str().unwrap()]).assert().code(3)
        .stdout(predicate::str::contains("File is not encrypted\n"))
        .stdout(predicate::str::contains("No syntax or stream encoding errors found").not());
}
```

**Step 2: Run — expect FAIL** (still prints `PDF check succeeded`).
Run: `cargo test -p flpdf-cli --test cli_check_exitcodes check_clean_pdf_emits_qpdf_block`

**Step 3: Implement** — replace the two `println!("PDF check succeeded")` sites and
add the trailing lines + helper. Gate block on `report.summary` (present iff valid):
```rust
    if let Some(summary) = &report.summary {
        print_check_block(&input, summary);
    }
    if has_warnings {
        return Err(Box::new(CliExitError { code: ExitCode::Warnings, message: String::new() }));
    }
    println!("No syntax or stream encoding errors found; the file may still contain");
    println!("errors that {} cannot detect", progname());
    Ok(())
```
Helper (private; `<file>` verbatim via `Path::display`):
```rust
fn print_check_block(input: &Path, summary: &flpdf::CheckSummary) {
    println!("checking {}", input.display());
    println!("PDF Version: {}", summary.version);
    // Interim: encrypted files emit a single line; the detailed qpdf
    // R = / P = / permission block is tracked in flpdf-oox1.
    println!("{}", if summary.encrypted { "File is encrypted" } else { "File is not encrypted" });
    println!("{}", if summary.linearized { "File is linearized" } else { "File is not linearized" });
}
```
Leave the stderr diagnostics loop and the `!report.valid` early return untouched
(exit 2 already emits no stdout). Update the stale comment at ~line 1551 that
mentions `PDF check succeeded`.

**Step 4: Run — expect PASS.** Run the two new tests.

**Step 5: Commit** `git commit -m "feat(cli): qpdf --check stdout block (checking/version/encryption/linearization) (flpdf-l3jx)"`

---

### Task 3: Migrate existing assertions + strengthen exit-2 / encrypted

**Files (update `PDF check succeeded` → block lines):**
- `crates/flpdf-cli/tests/cli_check_exitcodes.rs`: 129, 142, 161, 174, 189
  (→ `File is not encrypted` etc.); **261** strengthen: `!report.valid` exit-2
  stdout must be empty → assert `.stdout(predicate::str::is_empty())` (or at least
  `checking`/`PDF Version` absent).
- `crates/flpdf-cli/tests/cli_password_hex_key_tests.rs`: 78 → assert
  `File is encrypted` (this fixture is an encrypted V5/R6 file opened with a key).
- `crates/flpdf-cli/tests/encrypted_rewrite_tests.rs`: 88, 220, 313 — these check
  the **decrypted** plaintext output → `File is not encrypted`.
- `crates/flpdf-cli/tests/cli_tests.rs`: 19, 32, 76, 114, 126, 141, 329, 775 —
  classify each fixture (clean plaintext expected) → `File is not encrypted`
  (use a representative line; inspect each test's fixture before editing).

**Step 1–2:** After editing, run the full CLI suite — expect any mis-classified
assertion to fail loudly: `cargo test -p flpdf-cli`.

**Step 3:** Fix classifications until green.

**Step 4: Run — expect PASS.** `cargo test -p flpdf-cli && cargo test -p flpdf`

**Step 5: Commit** `git commit -m "test(cli): migrate check-stdout assertions to qpdf block (flpdf-l3jx)"`

---

### Task 4: Quality gates + coverage

**Steps:**
1. `cargo fmt --all` then `cargo fmt --all --check` (CI quality gate).
2. `cargo clippy -p flpdf -p flpdf-cli --all-targets` — zero new warnings.
3. `cargo test -p flpdf -p flpdf-cli` — all green.
4. `cargo test --doc -p flpdf` — doc examples intact (CheckSummary added).
5. `cargo doc -p flpdf --no-deps` — no `broken_intra_doc_links`.
6. Commit, then `scripts/patch-coverage.sh --base main`:
   - `flpdf` changed lines must be 100%; `flpdf-cli` best-effort.
7. Qualitative check: confirm exit 0 / 3 / 2 and encrypted / linearized arms each
   have a real assertion (not just line execution).

**Commit** any fmt/coverage follow-ups.

---

## Notes / guardrails
- `pdf.version()` returns the header version verbatim (`"1.7"`), no `%PDF-` prefix
  (`crates/flpdf/src/xref.rs::parse_header`). qpdf reports the same header version.
- Do NOT touch the stderr format (`WARNING: <file>: <msg>` / `progname(): ...` /
  `operation succeeded with warnings`) — out of scope (flpdf-tc3e Stage 2).
- `CheckReport` is `pub` and not `#[non_exhaustive]`; adding `summary` is technically
  a breaking change — acceptable at 0.1.6, note it in the PR body.
- Public doc on `CheckSummary` / the new field: English only, rustdoc-conventional,
  no beads IDs (per `.claude/rules/pdf-rust-doc-review-patterns.md`). The flpdf-oox1
  reference lives only in a `//` comment in the CLI (private, unpublished).
