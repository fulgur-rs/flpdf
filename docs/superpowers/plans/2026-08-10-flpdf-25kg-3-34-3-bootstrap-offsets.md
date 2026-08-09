# Bootstrap Reference Diagnostic Offsets Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans or superpowers:subagent-driven-development to implement this plan task-by-task.

**Goal:** Make parse failures from referenced bootstrap objects report absolute source-file offsets in both diagnostic text and `Diagnostic::offset`.

**Architecture:** Keep `XrefReadContext` as the canonical qpdf-shaped bootstrap resolver. Rebase errors produced while parsing a source tail at the `read_file_object_for_reference` boundary, where both the tail and its absolute origin are known. In `resolve_reference`, derive the diagnostic offset from the rebased `Error::Parse` while preserving qpdf-style warning-and-null fallback and existing non-parse fallback behavior.

**Tech Stack:** Rust, Cargo unit tests, qpdf 11.9.0 source oracle.

---

### Task 1: Add the canonical bootstrap regression

**Files:**
- Modify: `crates/flpdf/src/xref.rs:2633-2655`

- [ ] **Step 1: Write the failing test**

Add `bootstrap_context_rebases_reference_parse_errors_to_source_offsets` next to the existing `bootstrap_context_reports_reference_read_errors` test. Build a referenced object at a nonzero offset with a malformed direct-object token, resolve it through `XrefReadContext::resolve_reference`, and assert that the warning text and stored offset use the object’s source position plus the body-relative parser position:

```rust
    #[test]
    fn bootstrap_context_rebases_reference_parse_errors_to_source_offsets() {
        let object_ref = ObjectRef::new(1, 0);
        let object_start = 7usize;
        let object = b"1 0 obj\n<0g>\nendobj\n";
        let bytes = [vec![b' '; object_start], object.to_vec()].concat();
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(
            object_ref,
            XrefEntry::Uncompressed {
                offset: object_start as u64,
            },
        );
        let mut context = XrefReadContext::new(
            &bytes,
            XrefReadContextSpec::ActiveSection,
            &registration,
            XrefLoadOptions::default(),
        );

        assert_eq!(context.resolve_reference(object_ref), Object::Null);

        let expected_offset = object_start + b"1 0 obj\n".len();
        let diagnostic = context
            .diagnostics
            .entries()
            .first()
            .expect("referenced parse failure warning");
        assert_eq!(diagnostic.offset, Some(expected_offset as u64));
        assert!(
            diagnostic
                .message
                .starts_with(&format!("parse error at byte {expected_offset}:")),
            "diagnostic = {diagnostic:?}"
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p flpdf --lib bootstrap_context_rebases_reference_parse_errors_to_source_offsets`

Expected: FAIL because the current bootstrap path reports the tail-relative parse offset and stores the object-start offset instead of the inner absolute offset.

### Task 2: Rebase canonical reference-read errors

**Files:**
- Modify: `crates/flpdf/src/xref.rs:288-335`
- Modify: `crates/flpdf/src/xref.rs:396-417`

- [ ] **Step 1: Rebase parser errors at the source-tail boundary**

In `read_file_object_for_reference`, convert the declared absolute offset once with `usize::try_from(...).unwrap_or(usize::MAX)`. Rebase errors from `parse_file_object_header` and `read_file_object` with the existing `Error::rebase_offset` primitive. Keep the explicit absolute offset for the object-ID-zero error, and make recovery header-mismatch errors carry the absolute object start rather than the synthetic zero offset.

- [ ] **Step 2: Preserve exact rebased offsets in diagnostics**

In `resolve_reference`, calculate the warning offset from `Error::Parse { offset, .. }` after the reference-read boundary has rebased it. Keep `Some(start as u64)` for non-parse errors. Continue to suppress warnings deferred to reconstruction and return `Object::Null` on every read failure.

- [ ] **Step 3: Run the focused regression**

Run: `cargo test -p flpdf --lib bootstrap_context_rebases_reference_parse_errors_to_source_offsets`

Expected: PASS with the warning text and `Diagnostic::offset` both pointing at the absolute malformed-token position.

### Task 3: Verify preserved bootstrap behavior

**Files:**
- No additional files.

- [ ] **Step 1: Run the bootstrap unit tests**

Run: `cargo test -p flpdf --lib xref::tests::bootstrap_context`

Expected: all bootstrap context tests pass, including zero-offset, beyond-EOF, successful resolution, indirect stream length, and cycle behavior.

- [ ] **Step 2: Run xref integration tests**

Run: `cargo test -p flpdf --test xref_tests`

Expected: all xref integration tests pass with zero failures.

- [ ] **Step 3: Run formatting and lint gates**

Run: `cargo fmt --all -- --check`

Expected: exit status 0 with no formatting changes required.

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings`

Expected: exit status 0 with no warnings.

- [ ] **Step 4: Inspect the final diff and status**

Run: `git diff --check` and `git status --short`

Expected: only the planned xref implementation, regression test, and this plan are changed; no generated or unrelated files are present.

- [ ] **Step 5: Commit the implementation**

Run:

```bash
git add crates/flpdf/src/xref.rs docs/superpowers/plans/2026-08-10-flpdf-25kg-3-34-3-bootstrap-offsets.md
git commit -m "fix: report absolute bootstrap parse offsets"
```

Expected: a commit containing only this issue’s plan, regression, and canonical xref fix.
