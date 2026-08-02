# Direct Page-Handle Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Translate qpdf 11.9.0's direct `QPDFObjectHandle` page-tree behavior into Rust for root correction, inherited attributes, and bounded rebuilds.

**Architecture:** Keep `PageTreeRoot` as the mutation-owner result, but introduce an internal ref-or-direct dictionary cursor for read traversal. `prepare_for_optimization_with_max_depth` owns qpdf `getAllPages()` repair; existing public preparation remains its default-bound wrapper. All inherited resolvers consume the same cursor semantics so a direct `/Parent` is not silently treated as absent.

**Tech Stack:** Rust, flpdf unit/integration tests, qpdf 11.9.0 source oracle.

---

### Task 1: Root correction follows direct catalog page values

**Files:**
- Modify: `crates/flpdf/src/pages/repair.rs`
- Test: `crates/flpdf/tests/page_document_helper_tests.rs`

- [ ] **Step 1: Write a failing helper test.** Construct a catalog with a direct `/Pages` value that is a `/Page` dictionary whose `/Parent` is the real indirect `/Pages` root containing two leaf children. Assert `PageDocumentHelper::get_all_pages()` returns both children and catalog `/Pages` becomes that real indirect reference.
- [ ] **Step 2: Run the focused test.**

```text
cargo test -p flpdf --test page_document_helper_tests direct_catalog_page_follows_parent_to_real_root -- --exact
```

Expected: failure because `prepare_for_optimization` skips its parent climb for direct values.

- [ ] **Step 3: Implement the minimum root cursor.** Replace the `direct_root.is_none()` gate with a cursor that can read `/Parent` from either a reference-resolved dictionary or an owned direct dictionary. If correction reaches an indirect root, write that reference to catalog `/Pages`; if it ends at a direct dictionary, write that direct value back. Call `repair_page_tree` or `repair_direct_page_tree` according to the final cursor.
- [ ] **Step 4: Re-run the focused test and `pages::repair` tests.**

```text
cargo test -p flpdf --test page_document_helper_tests direct_catalog_page_follows_parent_to_real_root -- --exact
cargo test -p flpdf pages::repair::tests
```

Expected: both pass.

### Task 2: Inherited attributes traverse direct parents

**Files:**
- Modify: `crates/flpdf/src/pages.rs`
- Modify: `crates/flpdf/src/page_rotate.rs`
- Modify: `crates/flpdf/src/page_tree_rebuild.rs`
- Test: `crates/flpdf/tests/page_document_helper_tests.rs`

- [ ] **Step 1: Write failing public regressions.** Use a direct catalog `/Pages` root with `/Resources` and `/Rotate 90`, and leaf pages whose `/Parent` is the direct root. Assert helper resource pruning retains a used resource and `add_page`/non-final `remove_page` preserve inherited rotation and resources on rebuilt leaves.
- [ ] **Step 2: Run the focused tests.**

```text
cargo test -p flpdf --test page_document_helper_tests direct_parent -- --nocapture
```

Expected: resource lookup returns `None` and rebuild materializes default rotation or loses inherited values.

- [ ] **Step 3: Implement shared direct-parent traversal semantics.** In `resolve_inherited_resources_with_max_depth`, `resolve_inherited_rotate_with_max_depth`, and `resolve_inherited_raw`, advance from a direct dictionary `/Parent` without requiring an `ObjectRef`; retain ref-cycle tracking and use the caller depth limit for every advance. Preserve the existing null-as-absent behavior.
- [ ] **Step 4: Re-run focused tests.**

```text
cargo test -p flpdf --test page_document_helper_tests direct_parent -- --nocapture
cargo test -p flpdf page_tree_rebuild::tests
```

Expected: all pass.

### Task 3: Thread the caller depth through repair

**Files:**
- Modify: `crates/flpdf/src/pages/repair.rs`
- Modify: `crates/flpdf/src/page_tree_rebuild.rs`
- Test: `crates/flpdf/src/page_tree_rebuild.rs`

- [ ] **Step 1: Write a failing unit test.** Create a `/Pages` tree deeper than a supplied `max_depth` but shallower than `DEFAULT_MAX_PAGE_TREE_DEPTH`. Call `rebuild_page_tree_with_max_depth` and assert it returns `Error::Unsupported` without changing `/Kids`.
- [ ] **Step 2: Run the exact test.**

```text
cargo test -p flpdf page_tree_rebuild::tests::rebuild_honors_repair_depth_limit -- --exact
```

Expected: it incorrectly succeeds because repair uses the default limit.

- [ ] **Step 3: Add `prepare_for_optimization_with_max_depth`.** Make the existing `prepare_for_optimization` wrapper call it with `DEFAULT_MAX_PAGE_TREE_DEPTH`; thread the supplied bound through direct and indirect repair helpers. Make `rebuild_page_tree_with_max_depth` call the bounded entry point.
- [ ] **Step 4: Re-run exact and focused tests.**

```text
cargo test -p flpdf page_tree_rebuild::tests::rebuild_honors_repair_depth_limit -- --exact
cargo test -p flpdf page_tree_rebuild::tests
```

Expected: all pass.

### Task 4: Verify and publish the review fixes

**Files:**
- Modify: `docs/superpowers/specs/2026-08-02-qpdf-page-document-helper-boundary-design.md`
- Create: this plan

- [ ] **Step 1: Run quality gates.**

```text
cargo fmt -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p flpdf --test page_document_helper_tests
cargo test -p flpdf page_tree_rebuild::tests
cargo test --workspace
```

- [ ] **Step 2: Regenerate changed-line coverage and inspect the diff.**

```text
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path target/patch-cov.lcov
scripts/patch-coverage.sh --base origin/main --lcov target/patch-cov.lcov
git diff --check origin/main...HEAD
git status --short
```

- [ ] **Step 3: Commit and publish.** Commit only the listed source, tests, and design/plan files; push the existing PR branch without force; reply and resolve the three review threads with qpdf citations and the focused verification output.
