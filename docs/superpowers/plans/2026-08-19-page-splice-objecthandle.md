# Page-tree splice ObjectHandle migration

## Scope

Migrate the production mutation route in `crates/flpdf/src/page_splice.rs` to
the canonical `ObjectHandle` graph. Test-only PDF builders and legacy
assertion helpers remain at the fixture boundary for this bounded slice.

The qpdf responsibility boundary is the page-tree owner and its document
helper: `QPDF_pages.cc:36-42` owns the page cache, `:154-188` flattens and
normalizes page trees, `:203-253` inserts pages while maintaining `/Kids`,
`/Count`, and `/Parent`, and `:254-304` removes pages. The public helper
delegates to that owner in `QPDFPageDocumentHelper.cc:37-52`.

## Acceptance checklist

- [x] `leaf_count_of`, `set_page_parent`, and `splice_subtree` use only
  `ObjectHandle` resolution/accessors and canonical mutation/write-back.
- [x] Existing flat, nested, boundary, invalid-range, empty-result, and
  depth-limit behavior remains unchanged.
- [x] Indirect `/Kids` and `/Count` are covered by a regression test, and
  mutations are marked dirty through the canonical document route.
- [x] Focused tests, formatting, rustdoc, strict clippy, relevant qpdf
  differential tests, workspace tests, and fresh patch coverage pass.
- [x] A source-first qpdf review finds no remaining scoped legacy bridge.
- [ ] The PR is rebased onto current `main`, all CI checks pass, and it is
  changed from Draft to Ready. Merge is handled by the integration session.

The one `cov:ignore` marker is on the `ObjectHandle::replace_key` error edge
for `/Kids`: every child handle is minted by the same owning `Pdf`, so qpdf's
ownership check cannot fail on this route; the normal success path is covered
by every splice mutation test.

## Implementation sequence

1. Add the indirect page-tree regression and run the focused tests.
2. Replace legacy snapshots with canonical handles, preserving error messages
   and the existing depth-first splice semantics.
3. Run focused and workspace quality gates, then compare output on the existing
   splice fixtures.
4. Review the diff against the pinned qpdf source, create the stacked PR, wait
   for all CI checks, and mark it Ready only after the final head is green.

**[provisional — settled by TDD, not by this document]**

The implementation may snapshot child handles from a resolved `/Kids` array
before recursing, then replace `/Kids` and `/Count` on the live `/Pages`
handle. The oracle and tests determine the exact resolution order and error
boundary.

**[/provisional]**
