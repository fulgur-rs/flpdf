# Drop ObjStm Content Recovery During Xref Reconstruction

**Status:** Approved from the `flpdf-4zt3` primitive-readiness audit

## Goal

Make damaged-xref reconstruction follow qpdf 11.9.0: recover only raw
`N G obj` type-1 entries and leave objects that are mentioned only inside a
recovered `/Type /ObjStm` unresolved. Explicit type-2 entries read from an xref
table or xref stream remain a separate supported responsibility.

## Oracle and responsibility boundary

qpdf `QPDF::reconstruct_xref` scans each line for an integer/integer/`obj`
header and calls `insertReconstructedXrefEntry`, which creates a type-1 entry
(`libqpdf/QPDF.cc:532-575,1194-1210`). Its closing comment explicitly declines
to inspect stream contents during reconstruction (`QPDF.cc:618-623`).

Type-2 entries are created only by xref table/stream parsing and are consumed
by `resolveObjectsInStream` after the current xref table confirms the type-2
mapping (`QPDFXRefEntry.hh:28-61`; `QPDF.cc:1716-1831`). Unknown objects resolve
to null (`QPDF.cc:1745-1749`).

## Design

1. Keep `recover_xref_entries` as the qpdf-style line scan in
   `crates/flpdf/src/xref.rs`. It continues to produce only
   `XrefEntry::Uncompressed` entries.
2. Remove the flpdf-only `recover_objstm_compressed_entries` call from
   `recover_xref_from_linear_scan` and from
   `ResolverCore::reconstruct_xref_and_retry`. Remove the helper and its
   private fallback machinery when no other consumer remains.
3. Preserve xref table/stream parsing, `XrefRegistration` free tombstones,
   `XrefEntry::Compressed`, and the existing explicit ObjStm member parser.
   No compatibility bridge or replacement synthetic entry is introduced.
4. Let a packed object absent from the reconstructed xref follow the existing
   missing-object path. Public resolution must observe qpdf's null result;
   the canonical resolver must not manufacture an `Unsupported` result merely
   because the removed helper used to create a synthetic type-2 entry.

## Tests and acceptance criteria

- Best-effort recovery fixtures with a recovered ObjStm assert the ObjStm
  container is recovered as type-1 while packed members are absent, including
  indirect `/Length` and in-stream-header cases.
- Public resolution of a packed member that has no explicit xref type-2 entry
  resolves to null rather than using the legacy ObjStm fallback.
- Existing explicit type-2 xref parsing/resolution and generation/provenance
  tests remain intact.
- Helper-only tests for synthetic ObjStm entries, fallback budgets, and
  gap-filler tombstone workarounds are removed or rewritten to assert the
  qpdf boundary; unrelated xref candidate and generation tests stay in scope.
- Run the focused xref and resolver tests, then the flpdf crate tests and
  workspace quality gates before handoff.

## Non-goals

- Do not implement a new canonical type-2 resolver in this issue.
- Do not change xref-stream candidate discovery, trailer selection, or writer
  ObjStm eligibility.
- Do not close or reprioritize related Beads issues such as `flpdf-1om0`.
