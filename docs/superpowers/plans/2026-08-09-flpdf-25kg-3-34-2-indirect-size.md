# flpdf-25kg.3.34.2 Indirect `/Size` Recovery Warning Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make both ordinary xref loading and candidate-xref recovery resolve an indirect trailer `/Size` through the qpdf-equivalent active or reconstructed xref context before post-chain validation.

**Architecture:** Keep `append_xref_size_warning_for` as the shared warning formatter. Obtain the resolved `/Size` from `XrefReadContext` at each qpdf-equivalent post-chain call site: the completed active registration for ordinary loading and the reconstructed line-scan plus re-entry registration state for candidate recovery. No late `Pdf::resolve` bridge or raw-reference special case is added.

**Tech Stack:** Rust workspace, `crates/flpdf/src/xref.rs`, `crates/flpdf/tests/xref_tests.rs`, qpdf 11.9.0 source and executable oracle.

## Global Constraints

- qpdf 11.9.0 source and observed output are the semantic oracle.
- Preserve qpdf's post-chain warning order and exact warning text.
- Limit production changes to qpdf's post-chain `/Size` validation paths: ordinary loading and candidate recovery.
- Add matching and mismatching indirect `/Size` regressions before production code.
- Do not change bootstrap resolver, strict-mode policy, dependency graph, or unrelated xref behavior.

---

### Task 1: Add failing regressions for candidate recovery and ordinary loading

**Files:**
- Modify: `crates/flpdf/tests/xref_tests.rs` near `xref_stream_document` and `candidate_recovery_warns_when_xref_size_is_not_one_plus_highest_object`

**Interfaces:**
- Consumes: existing xref-stream byte builders and `load_xref_and_trailer_best_effort`.
- Produces: a fixture with a candidate stream `/Size 3 0 R`, a line-scanned object `3 0 obj`, and a corrupt `startxref`; two tests assert qpdf-compatible matching and mismatching diagnostics.

- [x] **Step 1: Add a fixture builder for an indirect candidate `/Size`**

Append an `xref_stream_document_with_indirect_size(size_value: i64)` helper beside `xref_stream_document`. Build the existing candidate stream with `/Size 3 0 R` and `/Index [0 3]`, append `3 0 obj\n{size_value}\nendobj\n` after the stream, then replace the final `startxref` with `999999` so recovery must use the reconstructed line-scan table. The stream entry payload remains three entries for objects 0, 1, and 2; object 3 is found only by reconstruction.

- [x] **Step 2: Add the matching regression**

```rust
#[test]
fn candidate_recovery_resolves_matching_indirect_xref_size() {
    let bytes = xref_stream_document_with_indirect_size(4);
    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes))
        .expect("candidate xref-stream recovery should succeed");

    assert!(!loaded.repair_diagnostics.entries().iter().any(|diagnostic| {
        diagnostic.message.contains("reported number of objects")
    }));
}
```

- [x] **Step 3: Add the mismatching regression**

```rust
#[test]
fn candidate_recovery_warns_for_mismatching_indirect_xref_size() {
    let bytes = xref_stream_document_with_indirect_size(3);
    let loaded = load_xref_and_trailer_best_effort(&mut Cursor::new(bytes))
        .expect("candidate xref-stream recovery should succeed");

    assert!(loaded.repair_diagnostics.entries().iter().any(|diagnostic| {
        diagnostic.message
            == "reported number of objects (3) is not one plus the highest object number (3)"
            && diagnostic.offset.is_none()
    }));
}
```

- [x] **Step 4: Run the new tests before changing production code**

Run:

```bash
cargo test -p flpdf --test xref_tests candidate_recovery_
```

Expected: the tests compile and the mismatching test fails because the current raw `Object::Reference` match returns without warning; the matching test must not fail for an unrelated fixture error.

- [x] **Step 5: Add the ordinary-loading regression**

Append a `classic_xref_document_with_indirect_size(size_value: i64)` helper whose classic xref table registers `3 0 R` before the final post-chain check. Add `normal_xref_warns_for_mismatching_indirect_xref_size`, using `/Size 3 0 R` resolving to integer `3` while the highest live object is `3`. The current raw `loaded.trailer.get("Size")` path must fail to emit qpdf's warning; the active-context fix must make it emit the exact same diagnostic as candidate recovery.

### Task 2: Resolve ordinary and candidate trailer values through the bootstrap context

**Files:**
- Modify: `crates/flpdf/src/xref.rs` at the ordinary and candidate post-chain validation call sites

**Interfaces:**
- Consumes: `XrefReadContext::new`, `XrefReadContextSpec::ActiveSection`, `XrefReadContextSpec::Reconstruction`, the completed active registration, merged candidate `entries`, and `reentry_registration`.
- Produces: the same `append_xref_size_warning_for` warning behavior with a resolved integer `/Size` for ordinary and candidate loading.

- [x] **Step 1: Replace the candidate call's raw trailer value with a resolved validation dictionary/value**

After candidate entries and `/Prev` state are merged, construct `XrefReadContext::new(bytes, XrefReadContextSpec::Reconstruction { line_scan_entries: entries }, &reentry_registration, options)`. Resolve the candidate trailer's `Size` key with `resolve_dictionary_value`. Feed that resolved integer into the existing size comparison without changing the helper's warning text, maximum-object calculation, or diagnostic ordering. Preserve the context diagnostics and reconstruction-trigger handling exactly as the existing bootstrap context contract requires.

- [x] **Step 2: Run the two regressions**

Run the command from Task 1. Expected: both tests pass, with the mismatching indirect value producing one exact warning and the matching value producing none.

- [x] **Step 3: Resolve the ordinary trailer value through the active context**

After the ordinary xref chain is complete and `registration.snapshot()` has been assigned, construct an `XrefReadContext` with `XrefReadContextSpec::ActiveSection` and resolve the loaded trailer's `Size` key. Append resolver diagnostics before the shared warning formatter, and keep `registration.deleted_objects` available until the warning completes, matching qpdf's `read_xref` order.

- [x] **Step 4: Run the focused xref suite**

```bash
cargo test -p flpdf --test xref_tests
```

Expected: all xref tests pass and no existing warning-order or strict-mode assertions change.

### Task 3: Verify against the qpdf oracle and repository gates

**Files:**
- Inspect only: qpdf 11.9.0 source under `/home/ubuntu/.cache/flpdf/qpdf-11.9.0`

- [x] **Step 1: Run qpdf source/version checks**

Verify `QPDF.cc:626-704` still performs `m->trailer.getKey("/Size").getIntValueAsInt()` after the xref chain and that `QPDFObjectHandle` integer accessors dereference references. Run `qpdf --version` and require `qpdf version 11.9.0`.

- [x] **Step 2: Run the matching and mismatching real-PDF probes for both routes**

Run qpdf repair/check on the ordinary and candidate indirect-size fixtures and capture that each matching case has no size-mismatch warning while each mismatching case contains:

```text
reported number of objects (3) is not one plus the highest object number (3)
```

- [x] **Step 3: Run final Rust quality gates**

```bash
cargo fmt -- --check
cargo test -p flpdf --test xref_tests
cargo test -p flpdf
cargo test --workspace
```

- [x] **Step 4: Review the diff and persist the implementation state**

Run `git diff --check`, inspect `git diff`, update the Bead with implementation and verification evidence, run `bd dep cycles`, run `bd dolt push`, commit the focused changes, and push the implementation branch. Do not close or merge the issue without an explicit request.
