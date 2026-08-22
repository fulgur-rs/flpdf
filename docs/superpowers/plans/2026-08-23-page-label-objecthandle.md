# PageLabel Canonical Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:test-driven-development and superpowers:verification-before-completion. Steps use checkbox (\`- [ ]\`) syntax for tracking.

**Goal:** Remove the remaining qpdf-unowned PageLabel mutation APIs and leave the qpdf 11.9.0 read/reconstruction consumer on the canonical ObjectHandle route.

**Architecture:** \`PageLabelDocumentHelper\` keeps qpdf's \`hasPageLabels\`, \`pageLabelDict\`, \`getLabelForPage\`, and \`getLabelsForPageRange\` responsibilities. Its flpdf-only \`set_range\`, \`remove_range\`, and \`write_labels\` APIs have no qpdf counterpart and have no production callers, so they are deleted with their legacy parity tests. No compatibility adapter or second number-tree route is introduced.

**Tech Stack:** Rust workspace, \`ObjectHandle\`, \`HandleNumberTree\`, pinned qpdf 11.9.0, cargo and qpdf differential tests.

**Spec:** \`docs/superpowers/specs/2026-07-25-qpdf-component-bottom-up-refactor-design.md\`, correspondence rows 405, 410, and 411.

## Global Constraints

- qpdf 11.9.0 source and observed behavior are authoritative.
- qpdf-deviation is technical debt; do not preserve it for compatibility.
- Keep the aggregate \`flpdf-egzr.3.2.6\` open after this bounded slice.
- Write and run the RED census before deleting production routes.
- Rebase onto the latest \`origin/main\` before creating the Draft PR.
- Do not put the prohibited merge-warning sentence in the PR body and do not merge.

---

### Task 1: Specify and verify the legacy-route regression

**Files:**
- Modify: \`crates/flpdf/tests/page_label_document_helper_tests.rs\`.

- [ ] **Step 1: Add the production-route census**

The test must inspect only the production portion of \`page_label_document_helper.rs\` and reject \`crate::NumberTree::\`, \`resolve_borrowed(\`, and \`.set_object(\`. The current main must fail because those routes still exist.

~~~bash
cargo test -p flpdf --test page_label_document_helper_tests page_label_mutations_use_only_the_canonical_handle_route
~~~

Expected: FAIL on the existing legacy route.

- [ ] **Step 2: Commit the RED test**

~~~bash
git add crates/flpdf/tests/page_label_document_helper_tests.rs
git commit -m "test: reject legacy page-label mutation route"
~~~

### Task 2: Remove qpdf-unowned mutation APIs

**Files:**
- Modify: \`crates/flpdf/src/page_label_document_helper.rs\`.
- Modify: \`crates/flpdf/tests/page_label_document_helper_tests.rs\`.
- Modify: \`crates/flpdf/tests/helper_api_tests.rs\`.
- Modify: \`crates/flpdf/src/nntree.rs\` only for now-unused raw helper cleanup.

- [ ] **Step 1: Delete \`set_range\`, \`remove_range\`, and \`write_labels\`**

They are not QPDFPageLabelDocumentHelper APIs and have no production callers. Keep qpdf-owned read/reconstruction methods and \`write_reconstructed_labels\`, which models QPDFJob's direct flat \`/Nums\` output.

- [ ] **Step 2: Delete their tests and manual raw comparison helpers**

Remove tests whose only contract is the old flpdf-specific mutation shape. Retain qpdf differential tests for raw label dictionaries, selection reconstruction, direct/indirect roots, malformed trees, and warning behavior.

- [ ] **Step 3: Verify GREEN**

~~~bash
cargo fmt --all -- --check
cargo test -p flpdf --lib nntree
cargo test -p flpdf --lib page_label_document_helper
cargo test -p flpdf --test page_label_document_helper_tests
cargo test -p flpdf --test helper_api_tests
cargo test -p flpdf --test page_extract_tests
cargo test -p flpdf --test page_merge_tests
~~~

Expected: all focused tests pass and the production census is green.

### Task 3: Full verification and Draft PR

- [ ] **Step 1: Run all-features workspace tests and strict quality gates**

~~~bash
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
python3 scripts/check-qpdf-deviation-markers.py --check
python3 scripts/qpdf-module-docs.py --check
~~~

- [ ] **Step 2: Rebase onto latest origin/main and verify the diff**

~~~bash
git fetch origin main --prune
git rebase origin/main
git status --short
git diff --check
~~~

- [ ] **Step 3: Run fresh patch coverage against the actual PR base**

~~~bash
scripts/patch-coverage.sh --base origin/main
~~~

Expected: green with zero uncovered changed executable lines.

- [ ] **Step 4: Push and create a Draft PR**

Use \`gh pr create --draft --base main\` with a body containing qpdf citations, focused tests, full gates, and patch coverage. Do not include the prohibited merge-warning sentence.

- [ ] **Step 5: Wait for all CI, then Ready**

Use \`gh pr checks <number> --watch\`. Only after every required check, including Coverage and codecov/patch, is successful, run \`gh pr ready <number>\`. Do not merge.

- [ ] **Step 6: Update Beads**

Append the implementation commit, PR URL, verification evidence, \`bd dep cycles\`, and \`bd dolt push\` result to \`flpdf-egzr.3.2.6\`. Keep the aggregate open.
