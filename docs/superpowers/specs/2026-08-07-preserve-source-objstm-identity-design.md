# Preserve Source ObjStm Identity Design

## Status

Approved for `flpdf-um4z` on 2026-08-08. This document records the primitive
boundary approved by the readiness audit and written design review.

## Goal

Make the plain-writer object-stream plan retain the real source ObjStm identity
in Preserve mode. A reconstructed Preserve container must be one placement of
that source object, while a Generate container remains synthetic and has no
source identity.

This primitive unblocks `flpdf-rmp1`, which will separately make body emission
and xref allocation honor the resulting placement plan.

## Oracle and responsibility boundary

Pinned qpdf 11.9.0 is authoritative:

- `libqpdf/QPDF.cc:2381-2389` derives a compressed member's source container
  from its type-2 xref entry;
- `libqpdf/QPDFWriter.cc:1939-1967` intersects Preserve membership with the
  compressible-object set;
- `libqpdf/QPDFWriter.hh:668-680` stores both member-to-container and
  container-to-ordered-members maps, with members held in a `std::set`;
- `libqpdf/QPDFWriter.cc:1057-1118` numbers the container before its members
  whether traversal encounters a member or the container first;
- `libqpdf/QPDFWriter.cc:1998-2003` creates a new indirect null handle only for
  a Generate container;
- `libqpdf/QPDFWriter.cc:1621-1740` reconstructs a Preserve container from its
  source handle and copies only an indirect `/Extends` edge from the original
  ObjStm dictionary.

Therefore the planner/renumber boundary owns source-container identity,
container-first numbering, member numbering, and source-container reachability.
It does not own serialized ObjStm bytes or xref entries.

## Data model

Add one explicit group enum at the plain-writer planning boundary:

```rust
pub(crate) enum ObjectStreamGroup {
    SourceBacked {
        source: ObjectRef,
        members: Vec<ObjectRef>,
    },
    Synthetic {
        members: Vec<ObjectRef>,
    },
}
```

The source reference and members travel together. There is no sentinel source,
parallel source vector, or inference from group position.

`plan_qpdf_preserve_object_streams` returns source-backed groups and the existing
`removed_refs`. Its source reference is the generation-zero ObjStm reference
identified by each compressed xref entry, matching qpdf's `QPDFObjGen` mapping.
Members are filtered as today and normalized by ascending full `ObjectRef`,
matching qpdf's `std::set` order. Empty filtered groups are omitted.

The legacy `PackingPlan` used by the retained writer remains batch-based. The
new source-aware group representation is limited to the qpdf-shaped plain
writer, so this issue does not migrate or change legacy writer behavior.

Generate converts its existing even-split batches into `Synthetic` groups at
the plain-writer boundary. Preserve and Generate then use the same renumber and
placement path without erasing their different container identities.

## Renumbering and reachability

Replace the Generate-only renumber abstraction with an object-stream-aware
renumber abstraction that accepts `ObjectStreamGroup` values.

For a source-backed group, the container's new reference is also the
`old_to_new` mapping for its source ObjStm. For a synthetic group, the container
has no `old_to_new` entry. In both cases the container receives the next output
number before all members, and members receive consecutive output numbers in
ascending source-reference order.

A source-backed container is activated when traversal first encounters either
its source reference or any member. Activation is idempotent, so member-first
and container-first traversal produce one identical container placement.

When reachability processes the source container role, it inspects the original
ObjStm only for an indirect `/Extends` value stored directly in its dictionary
and enqueues that reference.
It does not perform the ordinary stream-dictionary walk for this role. Member
objects still undergo the ordinary reference walk. This preserves qpdf's
reconstructed-container boundary: unrelated original ObjStm dictionary entries
cannot make output objects reachable.

An `/Extends` target activates its own source-backed group when that group
retains members. If it has no retained group, it is numbered and placed as an
ordinary source object. Likewise, a source ObjStm whose members were all
filtered out remains an ordinary source placement when another surviving edge
reaches it.

> **[provisional — settled by TDD, not by this document]**
>
> *(implementation-detail sketch)*
>
> The traversal queue may distinguish ordinary source work from source-backed
> container work. Group activation records the container number, records the
> source mapping only for `SourceBacked`, then records and queues each member
> before queueing the special container work. This preserves qpdf's order in
> which writing members enqueues their child references before the reconstructed
> dictionary enqueues `/Extends`. The special work resolves the original
> stream dictionary and conditionally enqueues its raw indirect `/Extends`
> reference; ordinary work continues through the existing qpdf-shaped enqueue
> reference collector.
>
> **[/provisional]**

## Plain placement and validation

Carry the group origin into `PlannedIndirectObject::ObjectStream`. The origin is
available to the later body consumer, including the original source reference
needed by qpdf-compatible `/Extends` serialization, but this issue does not add
that serialization.

For a source-backed object-stream placement, validation treats the source
container as a placed source in addition to validating each member. It requires
the container source to map to the container output and applies the existing
unique-source rule. A plan containing both `Source { source: C, .. }` and a
source-backed ObjectStream for `C` therefore returns `Error::Unsupported`
instead of panicking. Synthetic containers do not participate in source
uniqueness or `old_to_new` completeness.

The placement builder excludes source-backed container sources from ordinary
`Source` placements. All existing output uniqueness, contiguous output-number,
member generation, removed-source, root, trailer, and PDF-version checks remain
in force.

## Error behavior

The implementation introduces no panic, sentinel, fallback duplication, or
silent role conversion. Invalid duplicate group membership or conflicting
source-container roles return `Error::Unsupported` at the earliest boundary
where they can be identified. PDF resolution errors continue to propagate.

## Acceptance criteria and test strategy

Use RED to GREEN TDD and pin these behaviors independently:

1. Preserve planning retains the exact source ObjStm for every non-empty group
   and orders members by ascending source `ObjectRef`.
2. Preserve groups are `SourceBacked`; Generate groups are `Synthetic`.
3. Source-backed container-first and member-first fixtures produce one
   container placement and identical qpdf-compatible numbering.
4. The source container maps to that container output and never also appears as
   an ordinary `Source` placement.
5. A source-backed container follows an indirect `/Extends` target but ignores
   an unrelated indirect dictionary value.
6. An `/Extends`-only target becomes a source-backed reconstructed container
   when it retains members, otherwise an ordinary source placement.
7. A reachable source ObjStm with no retained members remains eligible for an
   ordinary source placement.
8. The planned ObjectStream retains its original source reference for the body
   consumer.
9. Validation rejects duplicate Source/ObjectStream placement for one source
   container without panic.
10. Existing Preserve and Generate planner, renumber, and plain-plan tests stay
    green.

Focused tests belong beside `writer/object_streams.rs`, `rewrite_renumber.rs`,
and `writer/plain/plan.rs`. Verification then expands to formatting, the
`flpdf` crate test suite, and the workspace suite.

## Non-goals

- Removing the structural-container skip in `writer/plain/body.rs`.
- Changing xref allocation or emitting a source structural stream body.
- Serializing the source-backed container's `/Extends` entry.
- Migrating linearized, QDF, encryption, or legacy writer paths.
- Adding a compatibility alias for the Generate-only renumber abstraction.
