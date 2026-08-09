# xref bootstrap registration view design

**Date:** 2026-08-09

**Status:** Approved for implementation

## Goal

Make xref bootstrap resolution observe qpdf's one cumulative xref
registration without cloning that registration for every `/Prev` hop. Preserve
the distinct visibility rules for active/previous sections and reconstruction.

## Oracle boundary

qpdf 11.9.0 is authoritative:

- `QPDF::read_xref` walks `/Prev` sections while sharing `m->xref_table`
  (`libqpdf/QPDF.cc:626-708`).
- `insertXrefEntry` and `insertFreeXrefEntry` apply first-wins registration and
  object-number tombstones (`libqpdf/QPDF.cc:1149-1209`).
- Reconstruction removes and rebuilds type-1 entries, then re-enters
  `read_xref` with the reconstructed table (`libqpdf/QPDF.cc:516-623`).
- Bootstrap resolution uses lookup, cache, recursion guard, and null fallback
  semantics (`libqpdf/QPDF.cc:1700-1753`).

The qpdf-shaped canonical route already exists in
`crates/flpdf/src/xref.rs`; this change is not a consumer migration or a
legacy-bridge repair.

## Chosen design

Replace `XrefReadContext`'s owned `BTreeMap` with a private lookup view:

- `ActiveSection` and `PreviousSection` borrow the cumulative
  `XrefRegistration.entries` map.
- `Reconstruction` borrows both the line-scan map and the re-entry
  registration. The line-scan map wins for an exact `ObjectRef`; if it has a
  free entry, that tombstone remains authoritative. Registration is consulted
  only when the line-scan map has no exact key.
- Free entries remain absent from the normal registration map and resolve to
  null when explicitly present as free in a reconstruction map, matching the
  current filtered behavior.
- `resolve_reference`, its cache, recursion guard, diagnostics, and repair
  trigger remain unchanged apart from reading through the view.

The context borrow must end before `parse_xref_stream` mutates the cumulative
registration. The stream object and decoded entries will therefore be
collected in an inner context scope; registration insertion and final
`LoadedXref` snapshots happen after that scope. Final snapshots are retained
because `LoadedXref` owns post-bootstrap state.

## Invariants

- `/Prev` and hybrid `/XRefStm` resolution sees all registrations accumulated
  before the context is constructed.
- Reconstruction preserves line-scan exact-key precedence, including free
  tombstones, over re-entry stream entries.
- Exact object generations remain distinct.
- No new bridge, sentinel, fallback resolver, or qpdf-incompatible error path
  is introduced.
- The existing qpdf differential and xref behavior tests remain the acceptance
  authority; the new unit coverage only protects the view's ownership and
  precedence contract.

## Verification

1. Add a focused RED test for the active/previous context selecting a borrowed
   registration view rather than an owned snapshot.
2. Run the focused test and record the expected failure before implementation.
3. Implement the view and scope split, then run the focused test and all
   `xref_tests`.
4. Run formatting, clippy-relevant checks, and the qpdf compatibility test
   where available.

## Non-goals

- Do not alter `ResolverCore` or ordinary post-bootstrap `Pdf::resolve`.
- Do not remove final `XrefRegistration::snapshot()` calls used to construct
  owned `LoadedXref` values.
- Do not broaden the change to unrelated xref parsing, recovery, or writer
  behavior.
