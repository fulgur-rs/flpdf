# Embedded-files ObjectHandle Cutover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the five production legacy resolver consumers in `embedded_files.rs` while preserving qpdf 11.9.0 name-tree and attachment behavior.

**Architecture:** Reuse `EmbeddedFileDocumentHelper` and `HandleNameTree` as the single live ObjectHandle route. Keep only the already-recorded raw projection at `collect_embedded_file_pairs_raw` until the final legacy-route removal issue; do not add a new adapter or compatibility path.

**Tech Stack:** Rust workspace, `ObjectHandle`, `HandleNameTree`, qpdf 11.9.0 `/usr/bin/qpdf`, cargo tests, rustdoc, clippy, llvm-cov/LCOV.

---

### Task 1: Add the retained-handle RED regression

**Files:**
- Modify: `crates/flpdf/src/embedded_files.rs` in the existing `#[cfg(test)] mod tests`

- [ ] **Step 1: Write the failing test**

Add a unit test using `indirect_names_pdf_bytes()` that resolves and retains
the canonical `/Names` and `/EmbeddedFiles` handles, calls the public
`insert_embedded_file` wrapper, and asserts the retained `/Names` array now
contains the inserted pair. The test must exercise the legacy wrapper, not
call `EmbeddedFileDocumentHelper` directly.

- [ ] **Step 2: Run the RED test**

Run:

```bash
cargo test -p flpdf --lib embedded_files::tests::legacy_insert_updates_retained_embedded_files_root -- --exact
```

Expected result: FAIL because the current wrapper clones the catalog/name tree
through the legacy raw route and the retained canonical tree handle does not
observe the insertion.

### Task 2: Add the handle-native traversal option

**Files:**
- Modify: `crates/flpdf/src/nntree.rs` near `HandleNameTree`
- Modify: `crates/flpdf/src/embedded_files.rs` near the existing helper-tree setup

- [ ] **Step 1: Write the minimal depth-preserving support**

Expose an internal `HandleNameTree::set_max_depth` that forwards to the shared
`NNTree` depth guard. Add a private `embedded_files_tree_with_options` helper
that resolves `/Root`, `/Names`, and `/EmbeddedFiles` as handles, creates a
`HandleNameTree`, and optionally applies the existing explicit list depth.

- [ ] **Step 2: Run focused tests**

Run:

```bash
cargo test -p flpdf --test embedded_files_tests
```

Expected result: existing behavior remains green before the five consumers are
rewired.

### Task 3: Cut over list, raw projection, insert, and delete consumers

**Files:**
- Modify: `crates/flpdf/src/embedded_files.rs:430-778`

- [ ] **Step 1: Implement live `/AF` mutation**

Replace the cloned `Dictionary`/`Vec<Object>` path in
`remove_ref_from_af_in_dict` with canonical catalog/page and array handles.
Filter array child handles by their indirect identity, call
`set_array_items`, mark the mutated handle dirty, and remove/mark the parent
`/AF` key only when the filtered array is empty.

- [ ] **Step 2: Implement handle-native list and raw projection**

Use the shared handle-tree helper for `list_embedded_files_with_max_depth` and
`collect_embedded_file_pairs_raw`. Filter list values by their object identity;
at the raw boundary project indirect values as `Object::Reference` and direct
values through the existing bounded materialization API.

- [ ] **Step 3: Cut over insert/remove wrappers without changing ownership**

Make `insert_embedded_file` obtain the canonical filespec handle and call
`replace_embedded_file`. Make `delete_embedded_file` use a
`HandleNameTree` removal and preserve its existing raw detach contract; do
not call `remove_embedded_file` there because that qpdf helper nulls an
indirect filespec, while the attachment cleanup owner must preserve a
filespec still referenced by another live name tree. Keep the qpdf nulling
behavior in `EmbeddedFileDocumentHelper::remove_embedded_file`.

- [ ] **Step 4: Run the GREEN regression and focused suite**

Run:

```bash
cargo test -p flpdf --lib embedded_files::tests::legacy_insert_updates_retained_embedded_files_root -- --exact
cargo test -p flpdf --test embedded_files_tests
cargo test -p flpdf --lib embedded_files
```

Expected result: the retained-handle regression and all embedded-file tests
pass with no production `resolve_borrowed` calls remaining at the five scoped
sites.

### Task 4: Run repository quality gates

**Files:**
- Inspect: `crates/flpdf/src/embedded_files.rs`, `crates/flpdf/src/nntree.rs`, and the generated diff

- [ ] **Step 1: Run format and diff checks**

```bash
cargo fmt --all -- --check
git diff --check
```

- [ ] **Step 2: Run strict rustdoc and all-features clippy**

```bash
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

- [ ] **Step 3: Run workspace tests**

```bash
cargo test --workspace
```

- [ ] **Step 4: Run qpdf module-doc checks**

```bash
python3 scripts/qpdf-module-docs.py --check
```

### Task 5: Rebase, coverage, Draft PR, and Beads handoff

**Files:**
- Inspect: `git diff origin/main...HEAD`, PR checks, and Beads readback

- [ ] **Step 1: Commit the tested implementation before coverage**

The patch-coverage script rejects a dirty tree. After the focused and quality
tests pass, commit the design, plan, implementation, and tests, then verify
`git status --short` is empty before measuring coverage.

- [ ] **Step 2: Run fresh patch coverage against latest main**

Run the same commands as `.github/workflows/ci.yml:355-364`:

```bash
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path target/patch-cov.lcov
scripts/patch-coverage.sh --base origin/main --lcov target/patch-cov.lcov
```

Expected result: the gate reports zero uncovered changed executable lines in
`crates/flpdf/src`.

- [ ] **Step 3: Rebase and push**

Fetch the latest `origin/main`, rebase the feature branch, rerun focused gates
and patch coverage, then push the branch.

- [ ] **Step 4: Create Draft PR**

Create a Draft PR documenting the qpdf source mapping, the live probe, the
RED→GREEN result, all local gates, and the patch-coverage result. Do not merge.

- [ ] **Step 5: Verify CI and mark ready only after all checks are green**

Read back every CI check, including coverage/patch coverage. Run `gh pr ready`
only after all required checks are green; otherwise leave the PR Draft.

- [ ] **Step 6: Persist Beads evidence**

Append implementation, verification, PR, and CI evidence to `flpdf-3yn9.23`,
run `bd dep cycles`, then run `bd dolt push` and confirm the exact `Push complete.`
message. Keep the issue open if the PR is not merged, as required by the user.
