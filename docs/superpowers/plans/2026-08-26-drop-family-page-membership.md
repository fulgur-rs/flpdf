# Drop Family Page Membership Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `/Pg` and `/P` drop decisions depend on `RebuildResult::removed_pages`, exactly identifying original page-tree leaves removed by extraction.

**Architecture:** Preserve each module's existing walk, chain resolution, remap, and mutation ownership. Pass the explicit removed-page set alongside the surviving-page map, drop only set members, and leave unknown/non-page/orphan-page targets untouched.

**Tech Stack:** Rust workspace, `RebuildResult`, qpdf 11.9.0 source/live probes, page-extraction and full-rewrite differential tests.

---

### Task 1: Add RED regression tests

**Files:**
- Modify: `crates/flpdf/src/struct_tree_pg.rs` test module
- Modify: `crates/flpdf/src/thread_bead_p.rs` test module
- Modify: `crates/flpdf/src/objr_obj_annot_p.rs` test module

- [x] **Step 1: Add structure `/Pg` preservation tests.**

Use the existing `base_objs()` and `keep_3_and_5()` helpers. Add one test with
`StructElem 20` carrying `/Pg 30 0 R` and object 30 defined as
`<< /Type /Whatever >>`; add another with object 30 defined as an orphan
`<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>`. The result must
contain `removed_pages: [ObjectRef::new(4, 0)]`, and both `/Pg 30 0 R` values
must remain after `drop_struct_elem_dangling_pg`. Update the test helper's
`RebuildResult` to set that exact removed-page set while retaining its current
`ref_map`.

- [x] **Step 2: Add orphan-page preservation tests for thread and OBJR `/P`.**

In each module, add an object 30 with the orphan page dictionary above. For
the thread test, use the existing `p_resolving_to_non_page_left_unchanged`
fixture shape but set bead `/P 30 0 R`; for the OBJR test, use the existing
`p_resolving_to_non_page_left_unchanged` shape but set annotation `/P 30 0 R`.
Populate `removed_pages` with page 4. Assert that `/P 30 0 R` remains after
the pass. Existing removed-page and surviving-page tests must remain unchanged
in their observable assertions.

- [x] **Step 3: Run the new tests and verify RED.**

```bash
cargo test -p flpdf --lib struct_tree_pg::tests::pg_resolving_to_non_page_left_unchanged
cargo test -p flpdf --lib struct_tree_pg::tests::pg_resolving_to_orphan_page_left_unchanged
cargo test -p flpdf --lib thread_bead_p::tests::p_resolving_to_orphan_page_left_unchanged
cargo test -p flpdf --lib objr_obj_annot_p::tests::p_resolving_to_orphan_page_left_unchanged
```

Expected: the new tests fail because `/Pg` uses `ref_map` absence and the
thread/OBJR tests use `/Type /Page` rather than original page membership. Fix
test-builder errors only; do not change production code before these semantic
failures are observed. Commit the regression tests:

```bash
git add crates/flpdf/src/struct_tree_pg.rs crates/flpdf/src/thread_bead_p.rs crates/flpdf/src/objr_obj_annot_p.rs
git commit -m "test: expose drop family page membership gap"
```

### Task 2: Implement removed-page membership in all three passes

**Files:**
- Modify: `crates/flpdf/src/struct_tree_pg.rs`
- Modify: `crates/flpdf/src/thread_bead_p.rs`
- Modify: `crates/flpdf/src/objr_obj_annot_p.rs`

- [x] **Step 1: Change structure `/Pg` to use `removed_pages`.**

Thread `&result.removed_pages` through `walk_kids`, `walk_kid_ref`, and
`process_elem_dict`. Keep this surviving remap branch:

```rust
match surviving.get(pg) {
    Some(&new) => {
        if new != *pg {
            dict.insert("Pg", Object::Reference(new));
            changed = true;
        }
    }
    None if removed_pages.contains(pg) => {
        dict.remove("Pg");
        changed = true;
    }
    None => {}
}
```

Update all calls and comments to describe page-tree membership rather than
`ref_map` absence. Do not change `/Obj` collection or `/K` traversal.

- [x] **Step 2: Change thread bead `/P` to use `removed_pages`.**

Pass `&result.removed_pages` into `remap_or_drop_bead_p`. After the existing
`resolve_to_terminal_ref`, use membership before the remap/drop match:

```rust
if !surviving.contains_key(&page_ref) && !removed_pages.contains(&page_ref) {
    return Ok(());
}
match surviving.get(&page_ref) {
    Some(&new) if new != page_ref => {
        bead.replace_key(b"/P", pdf.get_object_handle(new))?;
        pdf.mark_object_handle_dirty(bead)?;
        Ok(())
    }
    Some(_) => Ok(()),
    None => {
        bead.remove_key(b"/P");
        pdf.mark_object_handle_dirty(bead)?;
        Ok(())
    }
}
```

Remove `is_page_dict` and its now-unused tests/comments. Keep malformed
non-reference and chain-resolution behavior unchanged.

- [x] **Step 3: Change OBJR annotation `/P` to use `removed_pages`.**

Pass `&result.removed_pages` into `remap_or_drop_annot_p`. After resolving the
terminal ref, return unchanged unless it is in `surviving` or
`removed_pages`; then use the existing surviving remap and removed drop arms.
Remove the `is_page_dict` helper and its type-based gate. Keep the annotation
object resolution and `set_object` write-back boundary unchanged for this
semantic slice.

- [x] **Step 4: Run GREEN focused tests.**

```bash
cargo test -p flpdf --lib struct_tree_pg
cargo test -p flpdf --lib thread_bead_p
cargo test -p flpdf --lib objr_obj_annot_p
cargo test -p flpdf --test page_extract_structtree_pg_tests
```

Expected: removed-page references still drop, surviving references still
remap, and non-page/orphan-page references remain. Commit the implementation:

```bash
git add crates/flpdf/src/struct_tree_pg.rs crates/flpdf/src/thread_bead_p.rs crates/flpdf/src/objr_obj_annot_p.rs
git commit -m "fix: restrict drop family to removed page leaves"
```

### Task 3: Verify qpdf parity and repository gates

**Files:**
- Verify: `crates/flpdf/src/struct_tree_pg.rs`
- Verify: `crates/flpdf/src/thread_bead_p.rs`
- Verify: `crates/flpdf/src/objr_obj_annot_p.rs`
- Verify: `crates/flpdf/tests/page_extract_structtree_pg_tests.rs`

- [ ] **Step 1: Run the qpdf differential and page-operation tests.**

```bash
cargo test -p flpdf --test page_extract_structtree_pg_tests
cargo test -p flpdf --test page_extract_tests
cargo test -p flpdf --test page_merge_tests
cargo test -p flpdf-cli --test compat_matrix_tests
scripts/qpdf-tokenizer-diff.sh
```

Expected: qpdf-generated output and all page/drop-family assertions pass; no
merge_documents behavior is changed by this issue.

- [ ] **Step 2: Run static quality gates.**

```bash
cargo fmt --all -- --check
RUSTDOCFLAGS='-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags' cargo doc --workspace --no-deps --document-private-items
cargo clippy --workspace --all-targets --all-features -- -D warnings
python3 -m unittest scripts/tests/test_qpdf_module_docs.py
python3 scripts/qpdf-module-docs.py --check
python3 -m unittest scripts/tests/test_qpdf_deviation_markers.py
python3 scripts/check-qpdf-deviation-markers.py --check
```

- [ ] **Step 3: Run workspace and changed-line coverage.**

```bash
cargo test --workspace --all-features
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path /tmp/flpdf-hn1g-12-drop-membership.lcov
scripts/patch-coverage.sh --base origin/main --lcov /tmp/flpdf-hn1g-12-drop-membership.lcov
```

Expected: all tests pass and `flpdf` reports zero uncovered changed
executable lines.

### Task 4: Rebase, publish, and record the bounded slice

**Files:**
- Modify: `docs/superpowers/specs/2026-08-26-drop-family-page-membership-design.md`
- Modify: `docs/superpowers/plans/2026-08-26-drop-family-page-membership.md`
- Modify: Beads issue `flpdf-hn1g.12`

- [ ] **Step 1: Rebase onto current `origin/main` and rerun Task 3.**

```bash
git fetch --prune origin
git rebase origin/main
git status --short --branch
```

- [ ] **Step 2: Push and create a Draft PR.**

```bash
git push --set-upstream origin feature/flpdf-hn1g-12-drop-membership
gh pr create --draft --base main --head feature/flpdf-hn1g-12-drop-membership --title "fix: restrict drop family to removed page leaves" --body-file /tmp/flpdf-hn1g-12-pr.md
```

- [ ] **Step 3: Wait for all checks, then mark ready.**

Run `gh pr checks <number>` until Quality, Coverage/patch coverage, Fuzz,
CodeQL, all OS tests, labels, and release gates pass. Review API findings
against qpdf before changing anything. Then run `gh pr ready <number>` and
read back the PR as open, ready, clean, and unmerged.

- [ ] **Step 4: Append Beads evidence without closing the issue.**

Record the qpdf source/live probe, RED→GREEN tests, focused/full gates,
rebase, PR URL/state, and `bd dep cycles`. Run `bd dolt push` and confirm
`Push complete.`. Keep the issue open or in progress if the integration session
owns final closeout; do not merge the PR.
