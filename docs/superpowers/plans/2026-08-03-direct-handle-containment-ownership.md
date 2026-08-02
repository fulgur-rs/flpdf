# Direct Handle Containment Ownership Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make direct `ObjectHandle` mutations dirty their known indirect owners without resolving unrelated objects.

**Architecture:** Direct slots retain the indirect object references that contain them. Resolution of an indirect handle and direct dictionary insertion propagate that containment relation through direct descendants, stopping at indirect boundaries. `Pdf` consumes those references to invalidate and schedule owners for incremental output.

**Tech Stack:** Rust workspace; `ObjectHandle`, `Pdf`, `FileSpec`; pinned qpdf 11.9.0 source.

## Global Constraints

- qpdf 11.9.0 `replaceKey` mutates shared object state and applies `checkOwnership` at insertion, with no document-wide owner scan.
- Do not preserve the legacy owner-scan API or add a helper-only adapter.
- Every behavior change starts with a focused RED test and ends with focused Rust tests.

---

### Task 1: Direct containment metadata

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs`
- Test: `crates/flpdf/src/object_handle.rs`

**Interfaces:**
- Produces: crate-private direct-owner propagation and lookup APIs used by `Pdf`.

- [ ] **Step 1: Write the failing test**

Add a unit test that inserts a direct child into an indirect dictionary and asserts the child reports that indirect object's reference as its owner.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p flpdf direct_owner --lib`
Expected: FAIL because direct handles do not retain containment owners.

- [ ] **Step 3: Write minimal implementation**

Add owner references to direct slots. Propagate an indirect owner's reference to direct children when resolving an indirect value and when inserting into an already-contained dictionary; stop propagation at indirect children.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p flpdf direct_owner --lib`
Expected: PASS.

### Task 2: Dirty mutation and Filespec regression

**Files:**
- Modify: `crates/flpdf/src/reader.rs`
- Modify: `crates/flpdf/tests/filespec_helper_tests.rs`

**Interfaces:**
- Consumes: direct-owner lookup from Task 1.
- Produces: direct helper mutations schedule known owners without a global resolve scan.

- [ ] **Step 1: Write the failing integration test**

Change the malformed-unrelated-xref Filespec test to require `set_description` success, default incremental write, and persisted `/Desc` on reopen.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p flpdf --test filespec_helper_tests filespec_direct_setter_persists_without_resolving_unrelated_object`
Expected: FAIL because the legacy scan resolves malformed object 7.

- [ ] **Step 3: Write minimal implementation**

Replace `mark_object_handle_dirty`'s document-wide traversal with canonical-handle validation for indirect values and direct-slot owner references for direct values.

- [ ] **Step 4: Run tests to verify it passes**

Run: `cargo test -p flpdf --test filespec_helper_tests`
Expected: PASS.

### Task 3: Verification

**Files:**
- Modify: only files changed by Tasks 1-2.

- [ ] **Step 1: Format and lint**

Run: `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --all-features -- -D warnings`.

- [ ] **Step 2: Run relevant suites and changed-line coverage**

Run: `cargo test -p flpdf --test filespec_helper_tests`, `cargo test -p flpdf`, `cargo test -p flpdf-cli --test cli_tests attachment`, and `scripts/patch-coverage.sh --base origin/main`.

- [ ] **Step 3: Commit implementation**

Commit the focused source and regression-test changes only after all verification commands pass.
