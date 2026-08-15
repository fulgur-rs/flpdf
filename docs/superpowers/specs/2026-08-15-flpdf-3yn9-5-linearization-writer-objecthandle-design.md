# flpdf-3yn9.5 Linearization Writer ObjectHandle Design

**Date:** 2026-08-15

**Status:** implementation-approved for the active qpdf parity goal

## Goal

Translate the remaining `QPDFWriter::writeLinearized` consumer path in
`linearization/{writer,part1,back_patch,renumber}.rs` to the existing canonical
`ObjectHandle`/writer emission path, while preserving qpdf 11.9.0's physical
layout, two-pass hint construction, xref/trailer semantics, encryption rules,
and back-patch offsets.

## Evidence and route classification

The pinned qpdf 11.9.0 source assigns one responsibility to
`QPDFWriter`: shared object traversal/emission (`QPDFWriter.cc:1072-1809`),
write dispatch and its two passes (`2191-2210`), xref/trailer output
(`2335-2495`), and linearized orchestration (`2537-2904`).

The canonical flpdf primitives already exist:

- `ObjectHandle::unparse_object_with_ref_map_and_removed` for live dictionary,
  array, scalar, and reference emission;
- `ObjectHandle::unparse_stream_body_with_ref_map_and_removed` and
  `ObjectHandle::pipe_stream_data` for stream dictionaries and payloads;
- handle-based ObjStm body/wrapper functions in `writer/object_streams.rs`;
- `ObjectHandle` trailer/dictionary emission in `object_handle.rs`;
- the canonical body and stream framing path in `writer/plain/body.rs`.

The current linearized route is `mixed`/`migration-needed`: it still
materializes legacy `Object` values, calls `resolve_borrowed`, uses legacy
filter helpers, and writes raw dictionaries in the same responsibility that
qpdf assigns to `QPDFWriter`. The layout planner, hint tables, and byte
back-patch machinery are valid layout responsibilities and are retained.

## Alternatives

1. **Bulk rewrite of the entire linearization module.** This could remove the
   old type quickly, but combines layout, stream, ObjStm, xref, and encryption
   changes and makes qpdf byte regressions difficult to localize.
2. **Add a linearization-specific ObjectHandle adapter/writer.** This would
   preserve the current call shape but duplicate the canonical writer and
   leave responsibility split from the standard path. It is rejected.
3. **Recommended: staged canonical emission cutover.** Keep the proven raw
   layout and offset code, move one qpdf emission responsibility at a time to
   the shared handle APIs, and make every branch a stacked, independently
   tested PR.

## Architecture and stack

The branch stack is rooted at PR #841, which supplies the preceding
linearization consumer cutover:

1. **Body/stream slice:** replace `resolve`-to-`Object` body and stream
   emission with live handles and the canonical ref-map/pipeline APIs.
2. **ObjStm/xref/trailer slice:** use handle-based ObjStm member emission and
   canonical trailer/xref dictionary serialization, preserving linearization's
   first-page/main xref split and `/Prev` chain.
3. **Two-pass cleanup slice:** route ID, encryption, metadata, hint framing,
   and final-pass state through the canonical writer contracts, then remove
   the obsolete legacy helpers and imports.

`part1.rs`, `back_patch.rs`, and `renumber.rs` remain byte-layout and mapping
modules unless a test demonstrates that a qpdf writer emission responsibility
has leaked into them. No new caller may be added to a legacy bridge.

## Data-flow invariants

- The same live `Pdf` graph is used for planning and both emission passes.
- `RenumberMap` remains the only source-to-output reference mapping.
- Raw `Vec<u8>` operations are limited to PDF framing, fixed-width padding,
  offsets, xref records, and qpdf-required back-patch regions.
- ObjectHandle emission must preserve qpdf's null visibility: null dictionary
  values are suppressed by `unparseObject`, array nulls remain, and trailer
  null behavior remains the separate `writeTrailer` contract.
- Stream payloads are piped/re-encoded/encrypted according to the existing
  writer policy; `/Length`, `/Filter`, `/DecodeParms`, metadata identity, and
  endstream framing are derived from the final on-disk payload.
- The hint object is framed and encrypted once after pass 1 and inserted
  byte-for-byte into pass 2. No second IV or second filter traversal is added.
- The first-page xref `/Prev`, the main xref `startxref`, `/H`, `/E`, `/T`,
  and deterministic `/ID` remain byte-offset stable.

## Testing and acceptance

Each slice starts with a canonical-route RED test and is then checked against
the pinned qpdf 11.9.0 executable. The focused corpus includes one-page,
three-page, existing-ObjStm, null-visibility, multi-filter stream, metadata,
deterministic-ID, and encrypted fixtures in disable/generate/preserve modes.

Required checks per PR:

- focused Rust test fails before the production change and passes after it;
- qpdf `--check-linearization` succeeds for every emitted fixture;
- qpdf-zlib-compat byte comparison is equal except for explicitly documented
  non-zlib environments;
- `cargo fmt --all -- --check`, all-features clippy, focused tests, workspace
  tests, and patch coverage pass;
- production `linearization/{writer,part1,back_patch,renumber}.rs` contains no
  remaining `resolve_borrowed`, legacy filter decode/encode calls, or
  legacy `Object::` emission in the migrated responsibility;
- each PR body names its qpdf source ranges, remaining bridge callers, test
  evidence, and Beads issue; no merge-delegation sentence is included.

## Beads and integration

`flpdf-3yn9.5` keeps its existing dependencies on `flpdf-3yn9.4`,
`flpdf-egzr.3.2.5`, and parent `flpdf-3yn9`; no lower primitive dependency is
invented. The implementation slices are child issues of `flpdf-3yn9.5`,
ordered body/stream -> ObjStm/xref/trailer -> two-pass cleanup. Each child is
closed only after its PR is green, reviewed, and ready. The parent remains
open until all child PRs and the upstream #841 lifecycle are complete.

Merge is performed by the separate integration worker; this session creates,
tests, reviews, and readies the stack but does not merge it.
