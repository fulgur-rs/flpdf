# Effective Xref Table Free-Entry Cutover Design

## Status

Approved in-session on 2026-08-08.

Bead: `flpdf-25kg.3.30`.

## Goal

Make the reader's effective cross-reference table match qpdf 11.9.0:
uncompressed and compressed entries are retained, while free entries are
represented only by a construction-scoped deleted-object set. The writer's
output free-entry bookkeeping remains separate and unchanged.

## qpdf boundaries

The pinned qpdf 11.9.0 implementation is the behavior oracle:

- `QPDF::insertXrefEntry` rejects an object number already in
  `deleted_objects`, then inserts the exact `QPDFObjGen` only if it is not
  already present.
- `QPDF::insertFreeXrefEntry` records the object number in
  `deleted_objects` only when the exact object generation is not already in
  the effective table; it never inserts a free entry into `xref_table`.
- A classic table defers its free rows until after the optional `/XRefStm` is
  read. The classic live rows are registered before that stream.
- An xref-stream type-0 row uses object generation zero for the deleted-object
  check; its generation field is ignored.
- `/Size` is checked against the highest object number in the effective table
  and deleted-object set. A short `/Size` produces qpdf's warning.

The highest-generation reconstruction tail is not part of this Bead. ObjStm
resolution and type-2 object resolution remain owned by their existing
prerequisite Beads.

## Implementation shape

The xref loader will use a local construction state containing the effective
`BTreeMap<ObjectRef, XrefEntry>` and `BTreeSet<u32>` deleted-object set. Shared
registration helpers will be used by classic tables, xref streams, hybrid
merges, `/Prev` merges, and the recovery result where applicable. The deleted
set is not exposed after construction.

The public `LoadedXref.entries` therefore contains no `XrefEntry::Free` rows.
Reader/cache consumers will stop filtering Free entries as an input-table
concern. `XrefEntry::Free` remains available for writer-side output assembly
and explicit object deletion.

## Verification

RED tests will cover:

1. a newest free row suppressing an older live row;
2. classic-free versus hybrid-live ordering;
3. xref-stream type-0 rows with a generation wider than `u16`;
4. exact-generation first-wins registration;
5. `/Size` warnings using both live and deleted object numbers; and
6. the invariant that an effective reader table contains no Free entries.

Focused xref tests, reader/cache tests, qpdf differential probes, formatting,
changed-line coverage, and the workspace quality gates will be run before
handoff.
