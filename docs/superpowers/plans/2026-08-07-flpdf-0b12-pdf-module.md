# Pdf Module Extraction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move the canonical `Pdf<R>` document container, its teardown, and its direct document-state accessors from `reader.rs` into a new public `pdf.rs` module without changing PDF behavior or output bytes.

**Architecture:** `pdf.rs` owns the single `Pdf<R>` type corresponding to qpdf's single `QPDF::Members` container. Existing reader/resolve implementation blocks remain where they are and continue to implement methods on that type through crate-visible fields and narrow crate-visible primitives; `reader.rs` keeps only a private import for those implementation blocks, while the canonical public paths are `flpdf::Pdf` and `flpdf::pdf::Pdf`.

**Tech Stack:** Rust workspace, `cargo test`, `cargo clippy`, qpdf 11.9.0 correspondence documentation, patch coverage.

## Global Constraints

- Base this branch on PR #658 only; PR #657 is not a prerequisite.
- Preserve one `Pdf<R>` type; do not introduce Document/Engine wrapper types.
- Move code without changing parsing, resolution, teardown, diagnostics, or serialized bytes.
- Expose the canonical type through `flpdf::Pdf` and `flpdf::pdf::Pdf`; do not retain a `reader::Pdf` compatibility alias.
- Use the pinned qpdf 11.9.0 source as the responsibility oracle.

---

### Task 1: Pin the new public module contract

**Files:**
- Create: `crates/flpdf/tests/pdf_module_tests.rs`

**Interfaces:**
- Consumes: existing `Pdf::open`, `Pdf::version`, `Pdf::root_ref`, and `Pdf::trailer` behavior.
- Produces: a compile-time and runtime contract for `flpdf::pdf::Pdf` being the same canonical type as `flpdf::Pdf`.

- [ ] **Step 1: Write the failing integration test**

```rust
use std::io::Cursor;

#[test]
fn pdf_module_exposes_the_canonical_document_type() {
    let bytes = include_bytes!("../../../tests/fixtures/minimal.pdf").to_vec();
    let mut from_module = flpdf::pdf::Pdf::open(Cursor::new(bytes)).unwrap();
    assert_eq!(from_module.version(), "1.7");
    assert_eq!(from_module.root_ref(), Some(flpdf::ObjectRef::new(1, 0)));
    assert_eq!(from_module.trailer().get_ref("Root"), from_module.root_ref());

    let _: &mut flpdf::Pdf<_> = &mut from_module;
}
```

- [ ] **Step 2: Run the test and verify RED**

Run: `cargo test -p flpdf --test pdf_module_tests`

Expected: compilation fails because `flpdf::pdf` does not exist.

### Task 2: Extract the document container and accessors

**Files:**
- Create: `crates/flpdf/src/pdf.rs`
- Modify: `crates/flpdf/src/reader.rs`
- Modify: `crates/flpdf/src/reader/resolver.rs`
- Modify: `crates/flpdf/src/lib.rs`

**Interfaces:**
- Consumes: `reader::resolver::ResolverHandle<R>`, `reader::EncryptionState`, the existing resolve/lift primitives, and all existing `impl Pdf<R>` blocks.
- Produces: `pdf::Pdf<R>` and the crate-root `Pdf` re-export.

- [ ] **Step 1: Move the `Pdf<R>` declaration and `Drop` implementation into `pdf.rs`**

Copy the existing documentation and fields verbatim. Mark fields `pub(crate)` because the remaining inherent implementations live in sibling modules. Import `ResolverHandle` and `EncryptionState` through `crate::reader`.

- [ ] **Step 2: Move the eight selected accessors into `pdf.rs`**

Move `version`, `trailer`, `trailer_handle`, `trailer_key_handle`, `root_ref`, `adobe_extension_level`, `ever_called_get_all_pages`, and `mark_get_all_pages_called` without changing their bodies. Move the single-use `resolve_object_value` helper with `adobe_extension_level`.

- [ ] **Step 3: Expose only the crate-internal seams required by sibling implementation modules**

Change `EncryptionState` and `ResolverHandle::encryption_parameters` to `pub(crate)`. Change `Pdf::lift` and `Pdf::lift_to_handle_bounded` to `pub(crate)`. Do not expose these publicly outside the crate.

- [ ] **Step 4: Move consumers to the canonical module path**

Add `pub mod pdf;` to `lib.rs`, export `pub use pdf::Pdf`, remove `Pdf` from the grouped reader export, and migrate crate-internal `crate::reader::Pdf` consumers to `crate::Pdf`. Keep only a private `use crate::pdf::Pdf` in `reader.rs` for the implementation blocks that have not moved yet.

- [ ] **Step 5: Run the focused test and verify GREEN**

Run: `cargo test -p flpdf --test pdf_module_tests`

Expected: one test passes.

### Task 3: Update qpdf responsibility correspondence

**Files:**
- Modify: `docs/qpdf-correspondence.md`

**Interfaces:**
- Consumes: pinned `QPDF.hh` `Members` fields and `QPDF.cc` teardown/accessor locations.
- Produces: a correspondence row that identifies `pdf.rs` as the `Pdf<R>` container while leaving parsing/resolution responsibility in `reader.rs` and `reader/resolver.rs`.

- [ ] **Step 1: Amend the `QPDF.cc` correspondence row**

Add `pdf.rs` for `Pdf<R>`, `Drop`, and the direct document-state accessors; do not reclassify reader/resolver logic that this issue does not move.

### Task 4: Verify, commit, and publish the stacked layer

**Files:**
- Test: entire workspace and changed-line coverage

**Interfaces:**
- Consumes: completed extraction.
- Produces: a single reviewable PR whose base is PR #658's branch.

- [ ] **Step 1: Run formatting and focused/full checks**

Run, in order:

```bash
cargo fmt --all -- --check
cargo test -p flpdf --test pdf_module_tests
cargo test -p flpdf
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test
```

- [ ] **Step 2: Run changed-line coverage**

Generate the workspace LCOV data with the repository's normal `cargo llvm-cov` flow, then run `scripts/patch-coverage.sh --base docs/reader-rs-pdf-engine-resolve-split-design --lcov <path>` and require 100% changed executable-line coverage.

- [ ] **Step 3: Commit and publish**

Stage only the issue's files, commit with `refactor(pdf): extract canonical Pdf container`, push the branch, and use `gh stack link 658 refactor/flpdf-0b12-pdf-module` so the new PR targets PR #658's branch.

- [ ] **Step 4: Persist task state**

Read back the PR base/head and checks, append the implementation evidence to `flpdf-0b12`, close it only when the repository workflow permits, run `bd dolt push`, and report the pushed git/Beads state.
