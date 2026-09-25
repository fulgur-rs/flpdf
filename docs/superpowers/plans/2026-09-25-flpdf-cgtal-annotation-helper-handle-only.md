# AnnotationObjectHelper Handle-Only Cutover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Match qpdf 11.9.0's annotation-helper ownership shape by making `AnnotationObjectHelper` hold only an `ObjectHandle`, with no exclusive `Pdf` borrow in its public type or constructors.

**Architecture:** `QPDFAnnotationObjectHelper` receives one `QPDFObjectHandle` through its base `QPDFObjectHelper`. flpdf's canonical `ObjectHandle` already owns the value and resolves through its weak resolver, returning `Result` if the document has been dropped. The cutover removes the unused `_pdf` field, lifetime/type parameters, and Pdf-coupled constructors, then migrates callers to pass canonical handles.

**Tech Stack:** Rust workspace, `ObjectHandle`, pinned qpdf 11.9.0, Beads, Cargo tests/Clippy/Rustdoc, qpdf route and coverage scripts.

---

### Task 1: Specify handle-only simultaneous helpers with a RED test

**Files:**
- Modify: `crates/flpdf/tests/annotation_object_helper_tests.rs`
- Reference: `include/qpdf/QPDFObjectHelper.hh:34-59`
- Reference: `include/qpdf/QPDFAnnotationObjectHelper.hh:27-32`

- [x] Add a test that builds one PDF with two annotation dictionaries, calls `pdf.get_object_handle(ObjectRef::new(4, 0))` and `pdf.get_object_handle(ObjectRef::new(5, 0))`, constructs `AnnotationObjectHelper::new(first_handle)` and `AnnotationObjectHelper::new(second_handle)`, reads both subtypes, then uses `pdf` for a page-tree query while both helpers still exist.
- [x] Use this concrete test body after the existing `open` and `build_pdf` helpers:

```rust
#[test]
fn annotation_helpers_from_handles_can_coexist_with_pdf_access() {
    let bytes = build_pdf(vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 100 100] >>".to_vec(),
        ),
        (3, b"<< /Type /Page /Parent 2 0 R /Annots [4 0 R 5 0 R] >>".to_vec()),
        (4, b"<< /Type /Annot /Subtype /Text >>".to_vec()),
        (5, b"<< /Type /Annot /Subtype /Link >>".to_vec()),
    ]);
    let mut pdf = open(bytes);
    let mut text = AnnotationObjectHelper::new(pdf.get_object_handle(ObjectRef::new(4, 0)));
    let mut link = AnnotationObjectHelper::new(pdf.get_object_handle(ObjectRef::new(5, 0)));

    assert_eq!(text.get_subtype().expect("text subtype"), b"Text".to_vec());
    assert_eq!(link.get_subtype().expect("link subtype"), b"Link".to_vec());
    assert_eq!(
        flpdf::pages::page_refs(&mut pdf).expect("Pdf remains independently usable").len(),
        1
    );
}
```
- [x] Run `cargo test -p flpdf --test annotation_object_helper_tests annotation_helpers_from_handles_can_coexist_with_pdf_access`.
- [x] Confirm RED is the expected compile error at `AnnotationObjectHelper::new`: the current constructor requires `(ObjectRef, &mut Pdf)`, so it cannot accept a canonical handle.

### Task 2: Implement the handle-only helper boundary

**Files:**
- Modify: `crates/flpdf/src/annotation_object_helper.rs`

- [x] Change the type and constructor to this handle-only form:

```rust
pub struct AnnotationObjectHelper {
    annot: ObjectHandle,
}

impl AnnotationObjectHelper {
    pub fn new(annot: ObjectHandle) -> Self {
        Self { annot }
    }
}
```

- [x] Keep the existing accessor methods in the same implementation block, without a reader type or lifetime parameter.
- [x] Remove `_pdf`, the `route-hygiene-allow` marker, the `ObjectRef + Pdf` constructor, and the `from_object_handle(ObjectHandle, Pdf)` alias.
- [x] Keep every accessor on the same `ObjectHandle` resolving path and preserve its current defaults, errors, and output behavior.
- [x] Rewrite module/type/method examples to construct from canonical handles; remove imports used only by the old borrow-gated signatures.
- [x] Change the in-module struct-literal test to call `AnnotationObjectHelper::new(annot)`.

### Task 3: Migrate production and test consumers atomically with the public API

**Files:**
- Modify: `crates/flpdf/src/page_annotation_flatten.rs`
- Modify: `crates/flpdf/src/form_field_object_helper/rendering.rs`
- Modify: `crates/flpdf/src/job/json_sections.rs`
- Modify: `crates/flpdf-qtest-tools/src/driver/test_42_49.rs`
- Modify: `crates/flpdf/tests/annotation_object_helper_tests.rs`
- Modify: `crates/flpdf/tests/annotation_object_helper_error_tests.rs`
- Modify: `crates/flpdf-cli/tests/cli_tests.rs`
- Modify: `crates/flpdf-cli/tests/cli_acroform_transforms.rs`

- [x] Replace `from_object_handle(handle, pdf)` with `AnnotationObjectHelper::new(handle)` wherever the callsite already owns a canonical annotation handle.
- [x] At `ObjectRef` callsites, create the canonical handle with `pdf.get_object_handle(object_ref)` and pass that handle to `new`.
- [x] For example, change `AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf)` to `AnnotationObjectHelper::new(pdf.get_object_handle(ObjectRef::new(4, 0)))` and change `AnnotationObjectHelper::from_object_handle(annotation.clone(), pdf)` to `AnnotationObjectHelper::new(annotation.clone())`.
- [x] Migrate qtest-tools case 42-49's five annotation-helper calls; they already receive canonical `ObjectHandle`s and should pass those directly.
- [x] Preserve direct annotation dictionaries by using the existing `PageObjectHelper::get_annotation_handles(None)` output where the caller enumerates all page annotations.
- [x] Search the workspace and confirm no `AnnotationObjectHelper::from_object_handle`, no old two-argument `AnnotationObjectHelper::new`, and no `AnnotationObjectHelper<'_, R>` remain.
- [x] Run `cargo fmt --all` and focused flpdf annotation-helper and CLI annotation/AcroForm tests.

### Task 4: Verify the qpdf-facing contract and workspace gates

**Files:**
- No additional production files unless a qpdf-verified regression requires a correction.

- [x] Run `cargo fmt --all -- --check`.
- [x] Run `cargo test -p flpdf --test annotation_object_helper_tests` and `cargo test -p flpdf --test annotation_object_helper_error_tests`.
- [x] Run `cargo test -p flpdf-qtest-tools` to cover the qtest driver consumers.
- [x] Run `cargo test -p flpdf-cli --test cli_tests` and `cargo test -p flpdf-cli --test cli_acroform_transforms`.
- [x] Run `cargo test --workspace`.
- [x] Run `RUSTDOCFLAGS='-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags' cargo doc --workspace --no-deps --document-private-items`.
- [x] Run `cargo clippy --workspace --all-targets --all-features -- -D warnings`.
- [x] Run `python3 scripts/qpdf-module-docs.py --check`.
- [x] Run `python3 scripts/check-qpdf-route-matrix.py --check`.
- [x] Run `python3 scripts/check-qpdf-deviation-markers.py --check`.
- [ ] After committing, run `cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path target/patch-cov.lcov` and `scripts/patch-coverage.sh --base origin/main --lcov target/patch-cov.lcov`; require zero uncovered executable lines.

### Task 5: Hand off through a Draft PR

**Files:**
- Update: Beads issue `flpdf-cgtal`.
- Create: Draft PR for branch `fix/flpdf-cgtal-annotation-handle-only`.

- [ ] Stage the explicit implementation, test, and plan files; commit with `git commit -m 'refactor(annotation): use handle-only helper'`.
- [ ] Run `git fetch origin main`, rebase onto `origin/main`, and confirm `git merge-base --is-ancestor origin/main HEAD` succeeds.
- [ ] Re-run `cargo fmt --all -- --check`, the workspace tests, strict private-item Rustdoc, all-features Clippy, qpdf module/route/deviation checks, and committed patch coverage on the rebased head.
- [ ] Push `fix/flpdf-cgtal-annotation-handle-only` and create a Draft PR against `main` with `gh pr create --draft --base main --head fix/flpdf-cgtal-annotation-handle-only` and a source-backed body.
- [ ] Keep the PR Draft until `gh pr checks` reports every exact-head check green, including patch coverage and Windows.
- [ ] Freshly read back the PR head, base, state, and checks; then run `gh pr ready`.
- [ ] Append the final RED/GREEN, verification, and PR handoff to Beads; run `bd dep cycles`; close `flpdf-cgtal` per the user's handoff rule; read back the close and run `bd dolt push` until it reports `Push complete.`.
