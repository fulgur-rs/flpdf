# Direct `/Pages` Root Mutation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `PageDocumentHelper` mutations preserve and update a direct catalog `/Pages` dictionary exactly as qpdf 11.9.0 does.

**Architecture:** Keep `PageTreeRoot::{Indirect, Direct { catalog }}` as the ownership boundary. The rebuild layer shares page materialization and duplicate allocation, then commits either an indirect root object or the catalog-owned direct dictionary. For a direct root, every flattened leaf receives a direct copy of the final root dictionary as `/Parent`; no root object is minted.

**Tech Stack:** Rust workspace; `flpdf` integration tests; qpdf 11.9.0 source oracle.

## Global Constraints

- Follow qpdf 11.9.0 `QPDF::flattenPagesTree`, `insertPage`, and `removePage`; do not materialize a direct root into an indirect object.
- Preserve the public `rebuild_page_tree` and `rebuild_page_tree_with_max_depth` APIs.
- Resolve inherited attributes before changing a leaf `/Parent`.
- Keep indirect-root output and duplicate-page behavior unchanged.

---

### Task 1: Pin direct-root helper mutations with integration tests

**Files:**
- Modify: `crates/flpdf/tests/page_document_helper_tests.rs`

**Interfaces:**
- Consumes: `PageDocumentHelper::{add_page, remove_page}` and `build_n_page_pdf`.
- Produces: failing public-API regressions for direct `/Pages` roots.

- [ ] **Step 1: Write the failing tests**

Add a local test helper that replaces catalog `1 0 R`'s `/Pages` with the direct dictionary from `2 0 R`. Add one test that calls `add_page(3 0 R, false)` on two pages and another that removes `4 0 R` from three pages. Each asserts catalog `/Pages` remains `Object::Dictionary`, has the expected `/Count` and `/Kids`, and each surviving leaf has a direct `Object::Dictionary` `/Parent`.

- [ ] **Step 2: Verify the tests are red**

Run `cargo test -p flpdf --test page_document_helper_tests direct_catalog_pages_root -- --nocapture`.

Expected: the two non-final mutation tests fail with `Error::Missing("/Pages")`, proving the current rebuild path assumes `catalog.get_ref("Pages")`.

### Task 2: Rebuild both root owners through one leaf pipeline

**Files:**
- Modify: `crates/flpdf/src/page_tree_rebuild.rs`
- Test: `crates/flpdf/tests/page_document_helper_tests.rs`

**Interfaces:**
- Consumes: `crate::pages::repair::{prepare_for_optimization, PageTreeRoot}`.
- Produces: unchanged public `rebuild_page_tree` APIs that accept direct catalog roots.

- [ ] **Step 3: Derive the repaired root and original page list**

After rejecting an empty selection, call `prepare_for_optimization(pdf)`. Convert `None` to the established missing-root error and use `prepared.pages` for `original_pages` rather than `page_refs_with_max_depth`.

- [ ] **Step 4: Stage leaves before setting their parents**

Retain inherited-value resolution and duplicate allocation, but collect each allocated target reference and materialized dictionary in `Vec<(ObjectRef, Dictionary)>`. Do not set `/Parent` in the selection loop because a direct parent must contain final `/Kids`.

- [ ] **Step 5: Commit the final root by ownership**

Build a root dictionary with `/Type /Pages`, ordered `/Kids`, `/Count`, and no `/Parent`. For `Indirect(root_ref)`, write it to `root_ref` and set each leaf parent to `Object::Reference(root_ref)`. For `Direct { catalog }`, replace catalog `/Pages` with `Object::Dictionary(root.clone())` and set each leaf parent to `Object::Dictionary(root.clone())`.

- [ ] **Step 6: Persist staged leaves and verify green**

After committing the root, set `/Parent` on every staged leaf and write it through `pdf.set_object`. Run:

```text
cargo test -p flpdf --test page_document_helper_tests direct_catalog_pages_root -- --nocapture
cargo test -p flpdf page_tree_rebuild::tests
cargo fmt -- --check
```

Expected: all commands exit 0; direct-root tests pass without changing existing indirect rebuild behavior.

- [ ] **Step 7: Commit the fixed boundary and tests**

Stage only the spec, this plan, `page_tree_rebuild.rs`, and `page_document_helper_tests.rs`; commit with `fix(flpdf): preserve direct pages root during mutations`.

### Task 3: Run the PR-quality verification set

**Files:**
- Verify only: working tree and PR branch

**Interfaces:**
- Consumes: completed Tasks 1 and 2.
- Produces: fresh evidence for pushing and replying to the review.

- [ ] **Step 8: Run complete relevant verification**

```text
cargo fmt -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p flpdf --test page_document_helper_tests
cargo test -p flpdf --test page_tree_rebuild
cargo test --workspace
```

- [ ] **Step 9: Inspect and publish the verified branch**

Run `git diff --check origin/main...HEAD`, `git status --short`, and `git log --oneline origin/main..HEAD`. If clean and verified, push `feat/flpdf-11hp-page-document-helper`, reply to the P2 review with qpdf source and regression evidence, read it back, and resolve the thread.
