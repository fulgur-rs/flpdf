# Overlay Destination Page Handle Resolution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the active overlay destination-page rewrite use qpdf-shaped live ObjectHandles and preserve `/Annots` without a raw page snapshot.

**Architecture:** Keep overlay mapping, placement, and canonical annotation copying unchanged. Replace only the final raw page-dictionary reconstruction with a validated live page handle, ObjectHandle-built `/Resources` and `/Contents` values, and one dirty propagation call.

**Tech Stack:** Rust workspace, `ObjectHandle`, `PageObjectHelper`, qpdf 11.9.0 source/live probes, qpdf-zlib-compatible byte goldens.

---

### Task 1: Add RED route and destination-annotation behavior tests

**Files:**
- Modify: `crates/flpdf/tests/legacy_route_cutover_tests.rs`
- Modify: `crates/flpdf/src/job/overlay.rs` test module
- Modify: `tests/golden/regenerate.sh`
- Create: `tests/golden/references/overlay/overlay-destination-existing-annotation.pdf`

- [ ] **Step 1: Add the route-contract assertion before production edits.**

Read `crates/flpdf/src/job/overlay.rs`, isolate the production body of
`apply_overlays_to_page_with_sources` from its function marker to the next
`#[cfg(test)]`, then assert that the final page-rewrite slice contains
`get_object_handle`, `resolve(`, `replace_key`, and
`mark_object_handle_dirty`, and contains none of `resolve_borrowed`,
`resolve_object`, `live_annots`, or `page_dictionary`.

- [ ] **Step 2: Add the qpdf destination-annotation golden recipe and test.**

Use `link-annot-no-acroform.pdf` as the destination and `one-page.pdf` as the
content-only overlay. Add this command to `tests/golden/regenerate.sh`:

```bash
qpdf --qdf --static-id --no-original-object-ids \
  tests/fixtures/compat/link-annot-no-acroform.pdf \
  --overlay tests/fixtures/compat/one-page.pdf --to=1 -- \
  tests/golden/references/overlay/overlay-destination-existing-annotation.pdf
```

Add a `job::overlay::byte_gate` test that writes the same pair with the
existing static-id helper and compares the output to that golden. The test
must additionally inspect the destination page and assert that its annotation
reference remains present after the overlay operation.

- [ ] **Step 3: Run only the new route and behavior tests to verify RED.**

Run:

```bash
cargo test -p flpdf --test legacy_route_cutover_tests overlay_destination_page_uses_live_handle
cargo test -p flpdf --lib job::overlay::byte_gate::overlay_destination_existing_annotation_is_byte_identical --features qpdf-zlib-compat
```

Expected result: the route test fails because the current implementation still
contains `resolve_borrowed`/`page_dictionary`, and the behavior test either
fails to compile until its fixture hook is complete or fails on the existing
raw rewrite. Correct test setup errors, but do not change production code before
the route failure is observed. Commit the tests and golden recipe:

```bash
git add crates/flpdf/tests/legacy_route_cutover_tests.rs crates/flpdf/src/job/overlay.rs tests/golden/regenerate.sh tests/golden/references/overlay/overlay-destination-existing-annotation.pdf
git commit -m "test: expose overlay destination page raw route"
```

### Task 2: Replace the final overlay page snapshot with live handles

**Files:**
- Modify: `crates/flpdf/src/job/overlay.rs:178-352,1033-1042`
- Modify: `crates/flpdf/src/job/overlay.rs` test module around the old `page_dictionary` test

- [ ] **Step 1: Add the canonical destination-page resolver.**

Implement a private helper with the following behavior:

```rust
fn overlay_page_handle<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<ObjectHandle> {
    let page = pdf.get_object_handle(page_ref);
    pdf.resolve(&page)?;
    if page.try_as_dictionary()?.is_none() {
        return Err(Error::Unsupported(format!(
            "page {page_ref} is not a dictionary"
        )));
    }
    Ok(page)
}
```

This preserves the current error boundary without materializing a raw
dictionary. Add a unit test using an integer object that expects the same
`Error::Unsupported` message.

- [ ] **Step 2: Build the resource dictionary from destination-owned handles.**

Replace the raw `Dictionary`/`Object::Reference` construction for the page's
XObject map with a `Vec<(Vec<u8>, ObjectHandle)>`. The first entry is
`(b"/Fx0".to_vec(), dest.get_object_handle(fx0_ref))`; each underlay and
overlay entry uses its existing name and `dest.get_object_handle(xref)`. Build:

```rust
let resources = ObjectHandle::dictionary(vec![
    (
        b"/XObject".to_vec(),
        ObjectHandle::dictionary(xobject_entries),
    ),
]);
```

After `dest.set_object(contents_ref, Object::Stream(contents_stream))`, obtain
`let contents = dest.get_object_handle(contents_ref)` and call
`overlay_page.replace_key(b"/Resources", resources)?` and
`overlay_page.replace_key(b"/Contents", contents)?`.

- [ ] **Step 3: Remove the raw snapshot and mark the live page dirty.**

Delete the `page_dictionary` call and `live_annots` block. Obtain
`let overlay_page = overlay_page_handle(dest, dest_page_ref)?;`, replace only
the two keys, call `dest.mark_object_handle_dirty(&overlay_page)?`, and leave
`/Annots` untouched. Delete the obsolete `page_dictionary` helper and migrate
its test to `overlay_page_handle_rejects_non_dict`.

- [ ] **Step 4: Run the route and behavior tests to verify GREEN.**

Run:

```bash
cargo test -p flpdf --test legacy_route_cutover_tests overlay_destination_page_uses_live_handle
cargo test -p flpdf --lib job::overlay::byte_gate --features qpdf-zlib-compat
cargo test -p flpdf --lib job::overlay
```

Expected: the route contract passes, the destination annotation golden is
byte-identical, and all overlay tests pass. Commit the production cutover:

```bash
git add crates/flpdf/src/job/overlay.rs crates/flpdf/tests/legacy_route_cutover_tests.rs
git commit -m "refactor: route overlay destination rewrite through handles"
```

### Task 3: Run the complete affected verification matrix

**Files:**
- Verify: `crates/flpdf/src/job/overlay.rs`
- Verify: `crates/flpdf/tests/legacy_route_cutover_tests.rs`
- Verify: `tests/golden/regenerate.sh`

- [ ] **Step 1: Run qpdf differential and CLI overlay tests.**

```bash
cargo test -p flpdf --lib job::overlay --features qpdf-zlib-compat
cargo test -p flpdf --test legacy_route_cutover_tests
cargo test -p flpdf-cli --test compat_matrix_tests
```

Expected: all overlay annotation-copy, malformed-page, multi-source, and CLI
compatibility tests pass.

- [ ] **Step 2: Run formatting and static quality gates.**

```bash
cargo fmt --all -- --check
RUSTDOCFLAGS='-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags' cargo doc --workspace --no-deps --document-private-items
cargo clippy --workspace --all-targets --all-features -- -D warnings
python3 -m unittest scripts/tests/test_qpdf_module_docs.py
python3 scripts/qpdf-module-docs.py --check
python3 -m unittest scripts/tests/test_qpdf_deviation_markers.py
python3 scripts/check-qpdf-deviation-markers.py --check
```

- [ ] **Step 3: Run workspace and changed-line coverage gates.**

```bash
cargo test --workspace
scripts/qpdf-tokenizer-diff.sh
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path /tmp/flpdf-egzr-3-2-8-17-overlay.lcov
scripts/patch-coverage.sh --base origin/main --lcov /tmp/flpdf-egzr-3-2-8-17-overlay.lcov
```

Expected: workspace tests pass and `flpdf` reports zero uncovered changed
executable lines. Add a focused test before proceeding if coverage is below
100%.

### Task 4: Review, rebase, publish, and record evidence

**Files:**
- Modify: `docs/superpowers/specs/2026-08-26-overlay-page-handle-resolution-design.md`
- Modify: `docs/superpowers/plans/2026-08-26-overlay-page-handle-resolution.md`
- Modify: Beads issue `flpdf-egzr.3.2.8.17`

- [ ] **Step 1: Self-review the diff against qpdf.**

Confirm the production route has no raw page snapshot, qpdf citations match,
the old `overlay_annotations.rs` scope was not pulled in, and no
`canonical_*`/overlay naming cleanup slipped into this issue.

- [ ] **Step 2: Rebase and rerun all gates.**

```bash
git fetch --prune origin
git rebase origin/main
git status --short --branch
```

After a clean rebase, rerun Task 3 and patch coverage against the rebased
`origin/main`.

- [ ] **Step 3: Push and create a Draft PR.**

```bash
git push --set-upstream origin feature/flpdf-egzr-3-2-8-17-overlay-page-handle
gh pr create --draft --base main --head feature/flpdf-egzr-3-2-8-17-overlay-page-handle --title "refactor: route overlay destination rewrite through handles" --body-file /tmp/flpdf-egzr-3-2-8-17-pr.md
```

- [ ] **Step 4: Wait for all CI, then mark ready.**

Run `gh pr checks <number>` until Quality, Coverage/patch coverage, Fuzz,
CodeQL, all OS tests, labels, and release gates are pass. Read review APIs and
validate any finding against qpdf source/live behavior. Only after every check
is green run `gh pr ready <number>` and read back the PR as open, ready, clean,
and unmerged.

- [ ] **Step 5: Record Beads evidence without closing the issue.**

Append implementation, RED→GREEN, qpdf source/live, focused/full verification,
rebase, PR URL/state, and CI results to `flpdf-egzr.3.2.8.17`. Keep it
`IN_PROGRESS` because integration owns review and merge. Run:

```bash
bd show flpdf-egzr.3.2.8.17
bd dep cycles
bd dolt push
```

Confirm the Beads push prints `Push complete.` and do not merge the PR.
