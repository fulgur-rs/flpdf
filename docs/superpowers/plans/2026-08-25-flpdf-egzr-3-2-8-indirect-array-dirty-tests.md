# Indirect Array Resource Fixture Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Migrate `qpdf_flatten_marks_an_indirect_array_category_dirty_after_merge` to the canonical ObjectHandle test route and repair the stale source-contract boundary left by the merged holder-redirect deviation deletion.

**Architecture:** Keep production behavior unchanged. Build the parsed fixture's mutable objects as live `ObjectHandle` values, replace indirect slots through `replace_object_handle`, resolve the shared `/ProcSet` handle before and after the qpdf-shaped resource merge, and assert the indirect array's identity and content through typed handle accessors. The route-contract test will use the next surviving test function as its boundary.

**Tech Stack:** Rust workspace, `cargo test`, `ObjectHandle`, qpdf 11.9.0 source/live oracle, Beads, GitHub stacked PR workflow.

---

### Task 1: Repair the stale route-contract boundary

**Files:**
- Modify: `crates/flpdf/tests/page_annotation_resource_skip_route_cutover_tests.rs:227-231`
- Test: `crates/flpdf/tests/page_annotation_resource_skip_route_cutover_tests.rs::indirect_scalar_resource_fixture_uses_live_handles`

- [ ] **Step 1: Change only the boundary marker**

Replace `fn qpdf_flatten_terminal_chases_a_holder_redirect_category_and_array_item` with `fn qpdf_flatten_marks_an_indirect_array_category_dirty_after_merge`.

- [ ] **Step 2: Run the focused route-contract test**

Run:

```bash
cargo test -p flpdf --test page_annotation_resource_skip_route_cutover_tests indirect_scalar_resource_fixture_uses_live_handles -- --exact
```

Expected: PASS for the existing indirect-scalar fixture, proving the failure was only the stale deleted-function boundary.

- [ ] **Step 3: Commit the isolated contract repair**

```bash
git add crates/flpdf/tests/page_annotation_resource_skip_route_cutover_tests.rs
git commit -m "test: update resource route contract boundary"
```

### Task 2: Add the indirect-array live-handle contract

**Files:**
- Modify: `crates/flpdf/tests/page_annotation_resource_skip_route_cutover_tests.rs`
- Test: `qpdf_flatten_marks_an_indirect_array_category_dirty_after_merge`

- [ ] **Step 1: Add a failing source-contract test**

Extract the target function through the next surviving `fn qpdf_flatten_keeps_an_earlier_indirect_array_merge_dirty_after_a_later_category_fails` boundary. Reject `resolve_object(`, `resolve_borrowed(`, `Object::`, `set_object(`, `materialize(`, and `lift_object_to_handle(`. Require `get_object_handle(`, `ObjectHandle::array(`, `ObjectHandle::dictionary(`, `ObjectHandle::stream(`, `replace_object_handle(`, `resolve(`, `as_array()`, and `object_ref()`.

- [ ] **Step 2: Run the target contract test**

Run:

```bash
cargo test -p flpdf --test page_annotation_resource_skip_route_cutover_tests indirect_array_category_dirty_fixture_uses_live_handles -- --exact
```

Expected: FAIL because the fixture still contains the raw route.

### Task 3: Migrate the fixture to ObjectHandle

**Files:**
- Modify: `crates/flpdf/src/page_annotation_flatten.rs:1475-1528`

- [ ] **Step 1: Replace raw setup with live handles**

Use `ObjectHandle::array`, `ObjectHandle::dictionary`, and `ObjectHandle::stream` for objects 9, 5, and 4; use `pdf.get_object_handle(ObjectRef::new(5, 0))` for `/AP /N`; use `pdf.replace_object_handle` for indirect slots; build DR as an `ObjectHandle::dictionary` containing an `ObjectHandle::array`.

- [ ] **Step 2: Replace the raw snapshot assertion**

Resolve the indirect appearance, `/Resources`, and `/ProcSet` handles with `pdf.resolve`; inspect `proc_set.as_array()`; assert `PDF`, `Text`, and the indirect owner identity through `object_ref()`.

- [ ] **Step 3: Run focused tests**

```bash
cargo test -p flpdf page_annotation_flatten::tests::qpdf_flatten_marks_an_indirect_array_category_dirty_after_merge --lib
cargo test -p flpdf --test page_annotation_resource_skip_route_cutover_tests
```

Expected: all selected unit and route-contract tests PASS.

- [ ] **Step 4: Commit the migration**

```bash
git add crates/flpdf/src/page_annotation_flatten.rs crates/flpdf/tests/page_annotation_resource_skip_route_cutover_tests.rs
git commit -m "refactor: migrate indirect array resource fixture to handles"
```

### Task 4: Verify and hand off

**Files:**
- Verify: all changed files and `origin/main...HEAD`

- [ ] **Step 1: Run formatting, lint, docs, tests, qpdf contracts, and coverage**

Run the repository CI-equivalent commands: `cargo fmt --all -- --check`, all-features clippy, strict private rustdoc, `cargo test --workspace`, qpdf module/deviation checks, patch-coverage contract, fresh qpdf-zlib LCOV, and `scripts/patch-coverage.sh --base origin/main --lcov target/patch-cov.lcov`.

- [ ] **Step 2: Push and create a Draft PR**

Push `feature/flpdf-egzr-3-2-8-indirect-array-dirty-tests`, create a Draft PR with base `main`, then read back base/head/body and monitor all CI checks.

- [ ] **Step 3: Mark Ready only after all checks pass**

Run `gh pr ready <number>` only after every required check, including patch coverage, is successful. Update `flpdf-egzr.3.2.8` with the PR and verification evidence, run `bd dep cycles`, and verify `bd dolt push` reports `Push complete.`. Do not merge the PR or close the aggregate issue.
