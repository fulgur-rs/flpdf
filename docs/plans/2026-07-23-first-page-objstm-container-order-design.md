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

`route_objstm_containers` already computes first-page reach, non-first-page
reach, open-document membership, document-other membership, and outline
membership.

Extract the existing classic-path thumbnail-user calculation into a private
helper and reuse it in container routing. The helper must preserve qpdf's
special rule that a page's `/Thumb` traversal does not add `ou_thumb` to an
object already visited through that same page's ordinary page closure.

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
- Generate and Preserve retain their existing within-container member order.
- Empty preserved containers remain omitted after eligibility filtering.

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
5. Implement the canonical route and shared thumbnail-user helper.
6. Verify strict byte identity and structural parity for both new cases.
7. Run formatting, focused tests, both crate suites, workspace tests,
   all-feature clippy, qpdf validation, and committed-HEAD patch coverage.
   Changed-line coverage must be 100%.

## Scope

This change covers linearized Generate and Preserve because qpdf applies the
same filtered-container classification contract to both.

It does not change non-linearized object-stream writing, compression policy,
object eligibility, even-split membership, preserved source membership,
classic non-ObjStm ordering, or second-half container ordering.
