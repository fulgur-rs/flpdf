# flpdf-5p6h.1: xref tombstone lifetime and replacement parity

## Goal

Align flpdf's deleted-object bookkeeping with qpdf 11.9.0 for the boundary
between xref loading/recovery and in-memory object replacement. The change must
cover both the legacy `Pdf::set_object` route and the canonical
`replace_object_handle` route without adding another compatibility bridge.

## Oracle boundary

The pinned qpdf 11.9.0 source establishes two separate responsibilities:

- `QPDF::insertFreeXrefEntry` records a number-wide `deleted_objects` entry
  only while xref sections are being registered (`libqpdf/QPDF.cc:1187-1192`).
- `QPDF::insertReconstructedXrefEntry` consults that set while a damaged file
  is being rescanned (`libqpdf/QPDF.cc:1194-1210`).
- `QPDF::reconstruct_xref` clears the set after its scan
  (`libqpdf/QPDF.cc:516-575`), and `QPDF::read_xref` clears it after xref
  consistency checking (`libqpdf/QPDF.cc:686-708`). The set is therefore not
  a persistent mutation tombstone.
- `QPDF::replaceObject` validates that the replacement is direct and delegates
  to `updateCache`; it does not clear or add `deleted_objects`
  (`libqpdf/QPDF.cc:1843-1858,1980-1993`). `QPDF::removeObject` erases the
  xref/cache entry and nullifies outstanding handles without adding a
  persistent deleted-number entry (`libqpdf/QPDF.cc:1995-2004`).

## Current flpdf gap

`ResolverCore::deleted_object_numbers` currently survives beyond xref
registration (`crates/flpdf/src/reader/resolver.rs:273-278`). Both
`remove_object_preserving_handle` and `mark_deleted_object_number` add
mutation-time entries (`reader/resolver.rs:1257-1289`), and
`reconstruct_xref_and_retry` filters every recovered row against that retained
set (`reader/resolver.rs:1398-1410`). The public mutation routes then clear the
set from `Pdf::set_object` and `replace_object_handle`
(`reader.rs:1203-1211,1741-1749`). This makes different-generation replacement
dependent on whether a prior removal left a number-wide tombstone and permits
stale source rows to be reintroduced after the clear.

## Design

1. Keep number-wide free-row suppression local to xref registration and the
   recovery operation that produced it. A free row suppresses later live rows
   in the same registration/recovery merge, matching qpdf's number-wide
   `deleted_objects` behavior; exact live `(object number, generation)`
   collision handling remains unchanged.
2. Do not carry the xref loader's deleted-number set into the long-lived
   `ResolverCore` mutation state. Clear it at the same ownership boundary where
   qpdf clears `m->deleted_objects`; existing xref snapshots and diagnostics
   retain the result already computed while the set was active.
3. Remove mutation-time insertion and replacement-time clearing from the
   canonical and legacy object-removal/replacement routes. Those routes update
   the canonical cache/xref state and `qpdf_removed_refs` as their own
   responsibilities; they do not emulate qpdf's transient xref parser state.
4. Preserve the existing `XrefRegistration` local set and all free-row
   filtering needed before `/Size` resolution, candidate xref-stream re-entry,
   and initial resolver construction. Do not broaden the change into writer or
   consumer cutover work.
5. Update the correspondence documentation to state that flpdf's deleted
   number set is xref/recovery-local and that `replaceObject` does not mutate
   it.

## Alternatives considered

### Persistent number-wide tombstones

This prevents stale source bodies from returning after an in-memory removal,
but it is a flpdf hardening policy rather than qpdf's `replaceObject` and
`removeObject` contract. It also makes a later generation reuse depend on a
legacy removal history that qpdf does not retain.

### Split persistent tombstones by generation

This would allow a new generation while suppressing an old one, but introduces
a second mutation-specific identity policy. qpdf's free-row set is number-wide
only during xref registration, so this hybrid is more complex without being a
faithful responsibility boundary.

## Test-first acceptance

Add RED-to-GREEN coverage before production edits for:

- a free xref row suppressing later rows during the same xref registration;
- a malformed-header recovery after removing an object, recording the real
  qpdf-vs-flpdf outcome rather than assuming persistent suppression;
- replacing generation `3 0` with `3 1`, then forcing recovery, and asserting
  `get_xref_table`/`get_all_objects` and resolver minting match the qpdf probe;
- both `Pdf::set_object` and canonical `replace_object_handle`, including
  same-generation replacement and different-generation replacement;
- the existing free-row, compressed-entry, `/Size`, and xref-stream candidate
  regressions remaining green.

Verification must include the focused resolver/xref tests, the full flpdf and
workspace suites, clippy with `-D warnings`, strict rustdoc, qpdf differential
probes, and fresh changed-line coverage at 100%. The PR remains Draft until
all required CI checks pass; it is not merged by this work.
