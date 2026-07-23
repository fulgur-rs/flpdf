# Linearization Object-User Snapshot Design

**Issue:** flpdf-dpff
**Date:** 2026-07-23
**Oracle:** qpdf 11.9.0

## Goal

Make linearized object-stream routing consume the same object-user analysis as
the classic linearization partition. Build the analysis once while constructing
`LinearizationPlan`, retain the routing-relevant results, and remove the second
PDF-wide analysis currently performed by `route_objstm_containers`.

The primary outcome is to eliminate repeated xref scans, page-tree enumeration,
`resurrectable_null_refs` traversal, and page/thumbnail user traversal. The
design also makes one qpdf-derived classification snapshot authoritative for
classic partitioning and Generate/Preserve ObjStm routing, preventing the two
paths from drifting.

Existing qpdf 11.9.0 byte-identical output must remain unchanged unless a new
fixture proves that the shared object-user contract currently differs from
qpdf. A proven difference in that contract is fixed in this issue; unrelated
parity gaps are recorded separately.

## qpdf Model

qpdf builds object-user maps during `optimize()`. For linearized object streams,
`filterCompressedObjects()` replaces compressed members in those maps with
their ObjStm container, thereby folding the union of the members' users onto
the container. `calculateLinearizationData()` then classifies objects and
containers from those already-built maps.

The relevant property for this change is that qpdf does not rediscover page,
thumbnail, outline, open-document, and document-other users while routing each
ObjStm container. Object-user discovery precedes classification and remains the
single source of truth.

## Current State

Recent linearization parity work already provides the difficult behavioral
pieces:

- `page_object_users` reproduces qpdf's ordered per-page traversal with a fresh
  shared `visited` set per page.
- Page and thumbnail users share that traversal, including direct `/Thumb`
  descendants and lexical first-edge-wins behavior.
- `route_objstm_containers` folds member-user signals into one canonical
  container route.
- Generate and Preserve retain the route through batching, renumbering, and
  writer placement.

However, the results are not retained. `LinearizationPlan::from_pdf` computes
the user sets for classic partitioning, while `route_objstm_containers`
subsequently recomputes page refs, live refs, resurrectable refs, page and
thumbnail users, outlines, open-document users, and document-other users.

## Design

### Construction boundary

After `push_inherited_attributes_to_pages`, `LinearizationPlan::from_pdf`
collects these shared structural inputs once:

- page references;
- live object references;
- resurrectable null references;
- page-tree node references.

The existing user traversals consume those inputs. Traversals for different
users retain their independent `visited` semantics; the refactor shares
document-wide inputs and results but does not merge traversals whose qpdf
semantics are distinct.

The ordered page/thumbnail traversal produces:

- ordinary page users for every page;
- thumbnail users for every page;
- the existing `all_referenced_pages` inverse map;
- the first-page object set;
- the union of all thumbnail objects.

Outline, open-document, and document-other traversals produce their existing
sets. The outline first-page predicate is evaluated at the same construction
boundary.

### Retained routing snapshot

`LinearizationPlan` gains a private routing snapshot:

```rust
struct LinearizationRoutingUsers {
    first_page: BTreeSet<ObjectRef>,
    thumbnails: BTreeSet<ObjectRef>,
    outlines: BTreeSet<ObjectRef>,
    outlines_in_first_page: bool,
    open_document: BTreeSet<ObjectRef>,
    document_other: BTreeSet<ObjectRef>,
}
```

The existing public `LinearizationPlan::all_referenced_pages` remains the
authoritative object-to-page inverse map. The snapshot does not duplicate that
potentially large map. `PageObjectUsers` remains an intermediate construction
value and is not retained after the inverse map, first-page set, and thumbnail
union have been derived.

The private snapshot is optional only to support `LinearizationPlan::default()`
and internal hand-built hint tests. Every plan returned by `from_pdf` contains
a complete snapshot.

### Pure container routing

`route_objstm_containers` becomes a pure classifier over retained analysis:

```rust
fn route_objstm_containers(
    users: &LinearizationRoutingUsers,
    referenced_pages: &BTreeMap<ObjectRef, BTreeSet<u32>>,
    containers: &[Vec<ObjectRef>],
) -> Vec<ContainerPart>
```

It no longer accepts or reads a `Pdf`. It resolves no objects, enumerates no
pages, scans no xref entries, and creates no page or document user sets.

For each container it preserves the existing qpdf precedence:

1. outlines;
2. open-document;
3. first-page private or shared;
4. other-page private;
5. other-page shared;
6. rest.

First-page private requires no non-first-page, document-other, or thumbnail
user in the full member union. Other-page private requires exactly one
non-first-page user and no document-other or thumbnail user. Generate and
Preserve call the same pure classifier.

ObjStm membership, eligibility, source-container reconstruction, and
within-container member ordering still read the `Pdf` and remain unchanged.

### Defaults and invariant failures

`LinearizationPlan::default()` contains no routing snapshot. Disable mode and
tests that only consume hand-built hint data continue to work.

Generate or Preserve batching from a hand-built plan without a snapshot returns
`Error::Unsupported("linearization plan: missing object-user routing snapshot")`
rather than silently routing every container to Part 9. Production plans from
`from_pdf` cannot reach this error. A focused unit test fixes this invariant
failure and its message.

Reader and malformed-input errors from user discovery now arise while
constructing the plan. The pure routing stage introduces no new reader error.
Existing helper errors propagate without being rewritten.

## Invariants

- Every plan returned by `LinearizationPlan::from_pdf` has a complete routing
  snapshot.
- `first_page` equals the objects whose `all_referenced_pages` entry contains
  page index 0.
- `thumbnails` is the union of all exact qpdf-style `ou_thumb` users.
- Classic partitioning and ObjStm routing consume results from the same user
  discovery pass.
- Outline, open-document, and page-user precedence remains unchanged.
- Every non-empty ObjStm container receives exactly one route derived from the
  union of all surviving member users.
- Generate and Preserve use the same routing snapshot.
- Writer and renumbering code retain the selected route and do not reclassify
  the container.

## Test Strategy

Follow red-green-refactor while preserving the existing strict parity suite.

1. Convert focused routing tests to call `route_objstm_containers` without a
   `Pdf`. Its type signature makes PDF-wide recomputation structurally
   impossible.
2. Add snapshot consistency tests asserting that the first-page set agrees
   with `all_referenced_pages` and that thumbnail users remain distinct from
   ordinary page users.
3. Retain focused routing coverage for:
   - direct `/Thumb` descendants;
   - lexical `/Thumb` first-edge-wins;
   - first-page private/shared containers;
   - one other page plus a thumbnail user routing to Part 9;
   - one other page plus a document-other user routing to Part 9;
   - outline and open-document precedence;
   - mixed-member ObjStm user unions.
4. Verify that Generate and Preserve classify equivalent member unions from the
   same retained snapshot.
5. Run strict qpdf 11.9.0 byte comparisons for linearized Generate and Preserve,
   including thumbnail and container-order fixtures.
6. Run `qpdf --check-linearization` on affected parity outputs.

If an existing golden changes, do not regenerate it automatically. First
compare the result with qpdf 11.9.0. Only a demonstrated difference within this
object-user contract permits a behavior change and a new or updated golden.

## Verification

The completed implementation must pass:

- `cargo fmt --all -- --check`;
- focused linearization plan and ObjStm tests;
- `cargo test -p flpdf --test cmp_linearize_objstm_tests`;
- `cargo test -p flpdf --test linearize_objstm_generate_tests`;
- `cargo test -p flpdf`;
- `cargo test`;
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
- the repository patch-coverage gate from the final committed `HEAD`, with
  changed-line coverage at 100%.

## Scope

This change covers the object-user analysis shared by classic linearization and
linearized Generate/Preserve ObjStm routing.

It does not change:

- non-linearized object-stream writing;
- ObjStm eligibility or compression policy;
- Generate even-split membership;
- Preserve source-container reconstruction;
- writer compression or stream encoding;
- general PDF reader behavior;
- unrelated qpdf parity gaps.
