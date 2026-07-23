# qpdf ObjStm Container Routing Parity Design

**Issue:** flpdf-5mv0
**Date:** 2026-07-23
**Oracle:** qpdf 11.9.0

## Goal

Make the linearized object-stream layout use the same classification model as
qpdf: collect every compressed member's object users onto its ObjStm container,
classify that container once, and carry the resulting part through batching,
renumbering, and physical placement.

Success requires both byte parity and internal classification parity. The
existing `objstm-lin-otherpage-shared-docother` fixture is byte-identical to
qpdf even though flpdf currently classifies one container as Part 8 while
computing batches and Part 9 while computing its anchor. This change must remove
that latent drift without changing the fixture's bytes.

## qpdf Behavior

qpdf establishes object-stream membership before calculating linearization
parts:

1. `generateObjectStreams` or `preserveObjectStreams` creates the
   object-to-container mapping.
2. Linearized output removes page dictionaries and the document catalog from
   that mapping.
3. `filterCompressedObjects` replaces each compressed member in the object-user
   maps with its container, thereby taking the union of all member users.
4. `calculateLinearizationData` classifies the container exactly once as
   `lc_open_document`, `lc_first_page_*`, `lc_other_page_private`,
   `lc_other_page_shared`, or `lc_other`.
5. The resulting part controls both numbering and output order.

The same classification contract applies to generated and preserved object
streams. Preserve keeps the surviving source-container membership; it does not
split a source container according to each member's independently calculated
part.

## Current Gap

The generate path calls `route_objstm_containers`, which mirrors qpdf's
container-user union and part classification. It then discards the route after
using it to order the batches.

`second_half_container_anchors` later re-derives each container's part from
`LinearizationPlan`'s classic per-object partitions. Those partitions describe
members, not their qpdf container-user union, so the second classifier can
disagree with `route_objstm_containers`.

The preserve path reconstructs source membership but then partitions eligible
members by their individual first/second-half and Part-7/8/9 ownership. A source
container with mixed member users can therefore be split even though qpdf
classifies and places the surviving container as a unit.

## Design

### Routed batch representation

Add an internal routed-batch value containing `members: Vec<ObjectRef>` and
`route: ContainerPart`, and use it for every batch whose placement depends on
the qpdf container category. `ObjStmBatchPlan` retains routed batches rather
than maintaining separate member and route arrays, so filtering or reordering
cannot misalign them.

`ContainerPart::OtherPagePrivate` identifies Part 7 but not its page number.
Only the anchor calculation needs that finer position. Once the canonical route
has established that the container is Part 7, recover the one non-first-page
index from its members and the plan's per-page private sets. No other part is
re-derived from classic partitions.

### Generate mode

Continue using the existing global even split and
`route_objstm_containers`. Pair every surviving container with its route, sort
the pairs into qpdf part order, and retain the route with the batch.

This keeps the established within-part ordering:

- open-document and first-page containers remain in the first half;
- second-half containers remain ordered as Part 7, Part 8, then Part 9;
- ordering within a part remains the qpdf-compatible even-split/container
  object-number order already pinned by the golden suite.

### Preserve mode

Reconstruct surviving eligible members per source ObjStm in source container
number and member-index order. Remove only objects qpdf removes or cannot retain
in an ObjStm, including page dictionaries, the catalog, and ineligible members.

Run `route_objstm_containers` on each surviving source container. Place the
entire surviving batch according to that union route:

- `OpenDocument` into the open-document first-half batches;
- `FirstPage` into the first-page batches;
- `OtherPagePrivate`, `OtherPageShared`, and `Rest` into routed second-half
  batches.

Do not split a source container by an individual member's classic part. Members
without a valid renumber slot remain excluded with an explicit invariant check;
the change must not turn malformed or unreachable references into writer
panics.

### Writer and renumbering

Writer-level filtering operates on the routed-batch value and preserves its
route when members remain. If filtering empties a batch, drop the whole value.

`second_half_container_anchors` consumes the retained route:

- Part 7: anchor after the last plain object of the recovered owner page;
- Part 8: anchor after the last plain Part-8 object;
- Part 9: anchor after the last eligible pre-container Part-9 object, with the
  existing post-container exclusions unchanged.

The renumbering algorithm, compressed-last constraints, xref construction, and
ObjStm emission order remain unchanged.

## Error Handling and Invariants

- Every non-empty second-half batch has exactly one retained route.
- A retained first-half route never appears in the second-half batch list.
- A Part-7 route has exactly one recoverable non-first-page owner.
- Writer filtering removes a route only when it removes the corresponding
  entire batch.
- Missing renumber entries continue to fail at the planner boundary rather than
  being silently synthesized.
- Empty source containers after qpdf-compatible filtering are omitted.

Violations are surfaced as explicit internal errors or assertions at the
boundary where the inconsistent plan is created.

## Test Strategy

Follow red-green-refactor:

1. Add a focused anchor test for the existing Part-8/Part-9 drift. Pass the
   canonical `OtherPageShared` route and assert that the Part-8 anchor is used.
   The current signature/classifier must fail this test before implementation.
2. Add a preserve fixture containing a source ObjStm whose members have mixed
   individual ownership but whose qpdf union has one canonical container part.
   Generate its golden with qpdf 11.9.0 and demonstrate the current flpdf output
   differs.
3. Implement routed batches and preserve-union routing.
4. Verify the focused tests turn green.
5. Run the existing structural and strict byte tests, including
   `objstm-lin-otherpage-shared-docother` and preserve-mode golden coverage.
6. Run formatting, crate tests, workspace tests, all-feature clippy, and the
   repository's committed-HEAD patch-coverage gate. Changed-line coverage must
   be 100%.

## Scope

This change covers linearized `--object-streams=generate` and
`--object-streams=preserve`, because qpdf applies the same container-user union
contract to both modes.

It does not change non-linearized object-stream writing, compression policy,
object eligibility, hint-table semantics, or general classic-part
classification except where required to carry an already-computed container
route.
