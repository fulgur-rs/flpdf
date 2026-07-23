# First-page ObjStm Container Ordering Parity Design

**Issue:** flpdf-19ac  
**Date:** 2026-07-23  
**Oracle:** qpdf 11.9.0

## Goal

Make linearized `--object-streams=generate` and
`--object-streams=preserve` order multiple first-page object-stream
containers exactly as qpdf does: first-page-private containers, then
first-page-shared containers, then first-page outline containers when
`/PageMode /UseOutlines` places outlines in the first-page section.

Success requires byte-identical output against qpdf 11.9.0 in both modes.
The same ordered object-user classification is canonical for all
linearization parts, so this change also includes its necessary adjacent
effect: a non-first-page object with a thumbnail user is qpdf Part 9, not
Part 7.

## qpdf Behavior

qpdf establishes object-stream membership before calculating linearization
parts. `filterCompressedObjects` replaces each compressed member in the
object-user maps with its containing ObjStm, taking the union of every
member's users. `calculateLinearizationData` then classifies the container
itself using this precedence:

1. root;
2. outlines;
3. open-document;
4. first-page-private;
5. first-page-shared;
6. other-page-private, other-page-shared, thumbnail, or other.

A container is first-page-private only when its union has a first-page user
and has no document-other, non-first-page, or thumbnail users. Every other
first-page container is first-page-shared.

For a non-first-page object or container, qpdf's
`lc_other_page_private` predicate likewise requires `thumbs == 0`.
Therefore a union with exactly one non-first-page user but any thumbnail user
belongs to Part 9 rather than Part 7. This is classification, not a change to
the Part 7/8/9 placement mechanism or within-part ordering.

qpdf emits the first page dictionary, the
`lc_first_page_private` set, the `lc_first_page_shared` set, and finally
first-page outlines. Each set is ordered by container `QPDFObjGen`.
Generate assigns fresh container numbers in global even-split order.
Preserve retains source container numbers. Therefore stable buckets in those
respective input orders reproduce qpdf's within-category ordering.

## Current Gap

`route_objstm_containers` correctly unions member users to select a broad
linearization part, but collapses first-page-private and first-page-shared
containers into `ContainerPart::FirstPage`.

Both Generate and Preserve append all non-outline first-page containers to a
single `part3_regular` vector. If an earlier/lower-numbered container is
shared while a later/higher-numbered container is private, flpdf keeps the
shared container first. qpdf moves the private container before it. This
changes renumbered object IDs, `/O`, hint data, offsets, and output bytes.

## Design

### Canonical route

Refine `ContainerPart` so the router retains qpdf's first-page subsection:

- `FirstPagePrivate`;
- `FirstPageShared`;
- `FirstPageOutlines`.

Keep `OpenDocument`, `OtherPagePrivate`, `OtherPageShared`, and `Rest`.
The route remains the single source of truth used by Generate and Preserve.
No planner-side reclassification or first-member heuristic is added.

### User signals

Build one qpdf-style ordered object-user map and reuse it in both the classic
partition and `route_objstm_containers`. For each page, the traversal has one
shared `visited` set, visits dictionary keys lexically, recursively descends
direct arrays and dictionaries, and switches from `ou_page` to `ou_thumb`
while descending the leaf page's `/Thumb` value. Thus indirect descendants of
a direct `/Thumb` receive thumbnail users, and the first edge to an indirect
object wins that page's user. Ordinary page and thumbnail membership must
come from this same traversal; independently computed closures plus post-hoc
subtraction cannot reproduce qpdf's order.

`LinearizationPlan::from_pdf` first runs the qpdf-compatible inherited-attribute
push over the catalog's real `/Pages` → `/Kids` tree. The ordered-user walk
therefore consumes the already materialized leaf dictionary and always skips
the leaf's `/Parent`. It must not attempt a second inheritance walk through that
entry: a detached, cyclic, non-dictionary, or arbitrarily deep bogus `/Parent`
is not part of qpdf's page-user traversal, and attributes found only there must
not become page users.

Continue computing open-document, document-other, and outline membership
through their existing routes.

For each container, union the signals of all surviving members and apply
qpdf's precedence exactly once. A first-page container is private only when
the union has:

- at least one first-page member;
- no document-other member;
- no member reached by a non-first page;
- no member with a thumbnail user.

Otherwise it is first-page-shared.

### Generate ordering

Continue using the qpdf-compatible global even split. Iterate containers in
that split order and append them to separate stable buckets:

1. open-document;
2. first-page-private;
3. first-page-shared;
4. first-page-outlines;
5. existing second-half part buckets.

Concatenate the three first-page buckets in qpdf order. Sorting members within
each generated container by source object number remains unchanged.
The second-half bucket placement mechanism and within-part ordering remain
unchanged; only the canonical ordered-user predicate may classify a
non-first-page thumbnail union as Part 9 instead of Part 7.

### Preserve ordering

Continue reconstructing source ObjStm groups in ascending source-container
number and retaining source member-index order. Route every surviving source
container once from the union of its member users. Append first-page
containers to the same private/shared/outline stable buckets used by
Generate.

Preserve does not split or repack a source container. Its source-container
number order within each bucket matches qpdf's `std::set<QPDFObjGen>` order.

### Errors and invariants

This change adds no public API or new recoverable error case.

- Every non-empty container has exactly one canonical route.
- Outline and open-document precedence remains above first-page
  private/shared classification.
- First-page-private means the complete union satisfies qpdf's private
  predicate; a single shared member makes the whole container shared.
- Other-page-private means the complete union has exactly one non-first-page
  user and no document-other or thumbnail user; a thumbnail user makes it
  Part 9.
- Generate and Preserve retain their existing within-container member order.
- Empty preserved containers remain omitted after eligibility filtering.
- The page-tree depth bound applies to the real `/Kids` traversal that pushes
  inherited attributes, not to an unrelated leaf `/Parent` chain.

## Test Strategy

Follow red-green-refactor.

1. Add a flpdf-authored fixture with more than 100 compressible first-page
   objects. Arrange for the first generated container to contain a
   document-shared first-page member while a later container contains only
   first-page-private members.
2. Generate a qpdf 11.9.0 linearized Generate golden and demonstrate that the
   current flpdf output differs because it emits the shared container first.
3. Derive an ObjStm-bearing input from the authored fixture, then generate a
   qpdf 11.9.0 linearized Preserve golden. Demonstrate the same current-order
   failure in Preserve.
4. Add focused unit tests for private/shared/outline routing and stable
   private-before-shared batch ordering.
5. Add focused RED cases for a direct `/Thumb` descendant and for lexical
   first-edge-wins when `/Thumb` precedes another edge to the same object.
6. Add a deep bogus leaf-`/Parent` acceptance case and a detached-parent
   attribute case. Confirm qpdf 11.9.0 linearizes the former, and ensure the
   latter attribute never becomes a page user.
7. Add a focused synthetic container whose union has exactly one non-first-page
   ordinary user plus one thumbnail user, and require `Rest` (Part 9). Add an
   authored >100-member fixture so this signal alone moves a real container,
   with strict Generate and Preserve qpdf 11.9.0 goldens.
8. Implement the canonical route and shared ordered page/thumbnail user map,
   including the qpdf Part 7 `thumbs == 0` predicate.
9. Verify strict byte identity and structural parity for Generate and Preserve
   in the ordering and thumbnail-user cases.
10. Run formatting, focused tests, both crate suites, workspace tests,
   all-feature clippy, qpdf validation, and committed-HEAD patch coverage.
   Changed-line coverage must be 100%.

## Scope

This change covers linearized Generate and Preserve because qpdf applies the
same filtered-container classification contract to both.

It does not change non-linearized object-stream writing, compression policy,
object eligibility, even-split membership, preserved source membership,
or any part's placement mechanism or within-part ordering.

The user explicitly approved including the adjacent qpdf-canonical
classification implied by the same ordered user contract: non-first-page
thumbnail objects and containers that previously satisfied the broad
one-page route move from Part 7 to Part 9. This is the only second-half
classification change in scope; it does not reorder objects within Part 7,
Part 8, or Part 9.
