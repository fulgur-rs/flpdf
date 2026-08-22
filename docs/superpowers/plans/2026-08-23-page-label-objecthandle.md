# PageLabel ObjectHandle Cutover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (\`- [ ]\`) syntax for tracking.

**Goal:** Move the remaining page-label mutation consumer from the legacy materialized \`Object\`/\`NumberTree\` route to qpdf 11.9.0's live \`QPDFObjectHandle\` route, while deleting the no-qpdf page-shifting API.

**Architecture:** The lower stacked PR exposes the existing canonical \`NNTree\` handle mutation through \`HandleNumberTree\`. The upper stacked PR rewires \`PageLabelDocumentHelper\` mutation/rebuild operations to that facade and removes \`insert_pages\`/\`remove_pages\`, which qpdf does not own.

**Tech Stack:** Rust workspace, \`ObjectHandle\`, \`NNTree\`, \`HandleNumberTree\`, pinned qpdf 11.9.0, cargo, GitHub stacked PRs.

**Spec:** \`docs/superpowers/specs/2026-07-25-qpdf-component-bottom-up-refactor-design.md\`, correspondence rows 405, 410, and 411.

## Global Constraints

- qpdf 11.9.0 source and observed output are authoritative.
- Do not add a legacy bridge, compatibility adapter, or sentinel route.
- Do not preserve \`insert_pages\`/\`remove_pages\); they have no qpdf counterpart and no production caller.
- Write RED tests before production changes and verify the expected failure.
- PR bodies must not contain the prohibited merge-warning sentence.
- Create Draft PRs only; mark Ready only after every required CI check, including patch coverage, is green; do not merge.
- Rebase the final stack onto the latest \`origin/main\` immediately before pushing PRs.

---

### Task 1: Specify the lower HandleNumberTree contract

**Files:**
- Modify: \`crates/flpdf/src/nntree.rs\` test module.

**Interfaces:**
- Consumes: existing \`HandleNumberTree::new\`, read methods, and internal \`NNTree::insert_handle\`/\`remove_handle\`.
- Produces: \`new_empty\`, \`root_handle\`, \`insert\`, and \`remove\`.

- [ ] **Step 1: Write the failing test**

Add \`handle_number_tree_mutation_preserves_live_handle_identity\`. It must create an empty tree, insert an indirect label dictionary, assert lookup returns the same \`ObjectHandle\` identity, remove it, and assert the removed handle retains the same object reference. Also cover duplicate-key replacement and direct-root promotion.

~~~rust
let mut tree =
    HandleNumberTree::new_empty(&mut pdf, DEFAULT_MAX_TREE_DEPTH).unwrap();
let value = pdf.make_indirect_from_object_handle(label_handle()).unwrap();
let value_ref = value.object_ref().unwrap();
tree.insert(&mut pdf, 0, value.clone()).unwrap();
let found = tree.find_object_at_or_below(&mut pdf, 0).unwrap().unwrap().0;
assert!(found.is_same_object_as(&value));
let removed = tree.remove(&mut pdf, 0).unwrap().unwrap();
assert!(removed.is_same_object_as(&value));
assert_eq!(removed.object_ref(), Some(value_ref));
~~~

- [ ] **Step 2: Verify RED**

~~~bash
cargo test -p flpdf --lib nntree::tests::handle_number_tree_mutation_preserves_live_handle_identity
~~~

Expected: compilation failure because the wished-for mutation facade is absent.

- [ ] **Step 3: Commit the RED test**

~~~bash
git add crates/flpdf/src/nntree.rs
git commit -m "test: specify handle number-tree mutation contract"
~~~

### Task 2: Implement the lower HandleNumberTree facade

**Files:**
- Modify: \`crates/flpdf/src/nntree.rs\`.

**Interfaces:**
- Consumes: qpdf \`NNTree\` mutation and existing \`ObjectHandle\` ownership/dirty propagation.
- Produces:
  - \`pub(crate) fn new_empty<R: Read + Seek>(pdf: &mut Pdf<R>, max_depth: usize) -> Result<Self>\`
  - \`pub(crate) fn root_handle(&self) -> ObjectHandle\`
  - \`pub(crate) fn insert<R: Read + Seek>(&mut self, pdf: &mut Pdf<R>, key: i64, value: ObjectHandle) -> Result<()> \`
  - \`pub(crate) fn remove<R: Read + Seek>(&mut self, pdf: &mut Pdf<R>, key: i64) -> Result<Option<ObjectHandle>>\`

- [ ] **Step 1: Implement the smallest canonical facade**

Store an \`NNTree<NumberKey>\` initialized with \`NNTree::from_handle\`, use \`pdf.unique_id()\` for document identity, preserve the configured depth, and delegate to \`insert_handle\`/\`remove_handle\`. Return the canonical root handle after mutation. Do not materialize the root or values.

- [ ] **Step 2: Verify GREEN**

~~~bash
cargo test -p flpdf --lib nntree::tests::handle_number_tree_mutation_preserves_live_handle_identity
cargo test -p flpdf --lib nntree
~~~

Expected: the new identity test and all existing NNTree tests pass.

- [ ] **Step 3: Commit the lower slice**

~~~bash
git add crates/flpdf/src/nntree.rs
git commit -m "feat: expose canonical handle number-tree mutation"
~~~

### Task 3: Add the failing PageLabel consumer regression

**Files:**
- Modify: \`crates/flpdf/src/page_label_document_helper.rs\` tests.
- Test: \`crates/flpdf/tests/page_label_document_helper_tests.rs\` when public behavior is required.

**Interfaces:**
- Consumes: the Task 2 facade and current \`PageLabelDocumentHelper\` APIs.
- Produces: a regression for live catalog/root and label-value identity.

- [ ] **Step 1: Write the failing test**

Hold the catalog's \`/PageLabels\` handle and an existing label value handle, call \`set_range\` and \`remove_range\`, then assert aliases observe the live replacement/removal. Add a production source census that rejects \`NumberTree\`, \`resolve_borrowed\`, and \`Pdf::set_object\` in the page-label implementation.

- [ ] **Step 2: Verify RED**

~~~bash
cargo test -p flpdf --test page_label_document_helper_tests set_range_preserves_live_pagelabels_handle_identity
~~~

Expected: failure because the current consumer clones through the legacy raw route and writes back with \`Pdf::set_object\`.

### Task 4: Migrate PageLabelDocumentHelper and delete the debt route

**Files:**
- Modify: \`crates/flpdf/src/page_label_document_helper.rs\`.
- Modify: \`crates/flpdf/src/nntree.rs\`.
- Modify: focused page-label, extract, and merge tests.
- Modify: \`crates/flpdf/src/lib.rs\` only if stale exports remain.

**Interfaces:**
- Consumes: \`HandleNumberTree::{new_empty,root_handle,insert,remove}\`, \`ObjectHandle::replace_key/remove_key\`, and \`Pdf::mark_object_handle_dirty\`.
- Produces: \`set_range\`, \`remove_range\`, \`write_labels\`, and \`rebuild\` without raw \`Object\` mutation.

- [ ] **Step 1: Replace raw label construction**

Keep \`LabelRange\` as a typed projection, but use \`page_label_dict\`/\`reconstructed_label_handle\` for live output. Make raw \`to_dict\` test-only or remove it after updating tests.

- [ ] **Step 2: Rewrite set/remove**

Resolve the catalog handle, obtain an existing or empty \`HandleNumberTree\`, mutate it with an \`ObjectHandle\` value, replace \`/PageLabels\` with its root handle, and mark the catalog handle dirty. Preserve qpdf null-root, malformed-tree, duplicate-key, and warning behavior.

- [ ] **Step 3: Rewrite write/rebuild**

Validate typed inputs, build handle entries, insert them through the canonical number-tree facade, and remove \`/PageLabels\` with \`ObjectHandle::remove_key\` for empty input. Do not add a second tree algorithm.

- [ ] **Step 4: Delete non-qpdf page-shifting APIs**

Remove \`insert_pages\`, \`remove_pages\`, their \`qpdf-deviation\` block, and tests that exist only for those methods. Confirm no production caller before deletion.

- [ ] **Step 5: Verify GREEN**

~~~bash
cargo test -p flpdf --test page_label_document_helper_tests
cargo test -p flpdf --test page_extract_tests
cargo test -p flpdf --test page_merge_tests
cargo test -p flpdf --lib page_label_document_helper
~~~

Expected: focused tests pass and the page-label production census finds no legacy NumberTree or write-back route.

- [ ] **Step 6: Commit the upper slice**

~~~bash
git add crates/flpdf/src/nntree.rs crates/flpdf/src/page_label_document_helper.rs crates/flpdf/tests/page_label_document_helper_tests.rs crates/flpdf/tests/page_extract_tests.rs crates/flpdf/tests/page_merge_tests.rs crates/flpdf/src/lib.rs
git commit -m "refactor: migrate page labels to canonical ObjectHandle"
~~~

### Task 5: Stack and final verification

**Files:**
- PR metadata only; never include the prohibited merge-warning sentence in a PR body.

- [ ] **Step 1: Initialize and extend the stack**

~~~bash
gh stack init --base main feature/flpdf-egzr-3-2-6-number-tree-handle
gh stack add feature/flpdf-egzr-3-2-6-page-labels
~~~

- [ ] **Step 2: Run focused checks**

~~~bash
cargo fmt --all -- --check
cargo test -p flpdf --lib nntree
cargo test -p flpdf --test page_label_document_helper_tests
cargo test -p flpdf --test page_extract_tests
cargo test -p flpdf --test page_merge_tests
~~~

- [ ] **Step 3: Rebase the final stack**

~~~bash
git fetch origin main --prune
gh stack rebase --remote origin
gh stack view --json
~~~

Resolve only at the qpdf responsibility boundary, rerun focused tests, and do not preserve a legacy route to avoid conflicts.

- [ ] **Step 4: Run all required gates**

Run all-features clippy, strict private rustdoc, workspace tests, qpdf module/deviation checks, and fresh per-PR patch coverage using each PR's actual base.

- [ ] **Step 5: Push Draft PRs and wait for CI**

~~~bash
gh stack submit --auto --remote origin
~~~

Keep both PRs Draft until all required checks, including patch coverage, are green. Then use \`gh pr ready\` for each PR; do not merge.

- [ ] **Step 6: Record Beads evidence**

Append implementation, PR URLs, commit heads, CI results, \`bd dep cycles\`, and \`bd dolt push\` output to \`flpdf-egzr.3.2.6\`. Keep the aggregate issue open until remaining page-group routes are complete.

---

## Self-review checklist

- The plan maps qpdf's four PageLabelDocumentHelper methods and NNTree mutation to one canonical route.
- It explicitly deletes the no-qpdf page-shifting methods instead of preserving them for compatibility.
- It separates the lower primitive PR from the upper consumer PR.
- It does not claim the aggregate page-group issue is complete after this bounded slice.
- Every production change has a RED test before implementation and a focused GREEN verification.

