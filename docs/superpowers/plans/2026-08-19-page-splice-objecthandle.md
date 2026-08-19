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
- [x] Indirect `/Kids` identity is retained while its live array is mutated,
  `/Count` is updated canonically, and both count mismatches and duplicate
  insertions have regression coverage.
- [x] Focused tests, formatting, rustdoc, strict clippy, relevant qpdf
  differential tests, workspace tests, and fresh patch coverage pass.
- [x] A source-first qpdf review finds no remaining scoped legacy bridge.
- [ ] The PR is rebased onto current `main`, all CI checks pass, and it is
  changed from Draft to Ready.

The `cov:ignore` markers cover only invariant-impossible branches: the
`ObjectHandle` ownership edge for `/Kids` has only same-`Pdf` children, a fresh
object allocated by `make_indirect_object_handle` always has an indirect
identity, and `usize` page-count overflow cannot be constructed by a finite
PDF object tree. Direct `/Kids`, missing `/Kids`, leaf-count failures, and the
private count-mismatch edge are covered by focused tests.

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
before recursing, then mutate an existing live array in place (preserving its
indirect identity) and update `/Count` on the live `/Pages` handle. A missing
or non-array `/Kids` value may be replaced with a new array. The oracle and
tests determine the exact resolution order and error boundary.

**[/provisional]**
