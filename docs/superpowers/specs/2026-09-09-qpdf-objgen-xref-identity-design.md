# qpdf QPDFObjGen xref identity design

## Goal

Make flpdf's canonical xref reader accept and register classic xref generations
with the same responsibility boundaries as qpdf 11.9.0, so
`decode-parameters.test` case 1 reaches the unknown `/DecodeParms` fixture
without an artificial xref-recovery warning.

## Oracle facts

- `QPDF::parse_xrefEntry` parses the ten-byte offset and five-byte generation
  fields separately, converting both to integer values without a u16 range
  check (`libqpdf/QPDF.cc:770-842`).
- `QPDF::read_xrefTable` passes the parsed generation directly into
  `QPDFObjGen` for live and free rows (`libqpdf/QPDF.cc:846-895`).
- `QPDFObjGen` stores signed `int` object and generation values, and
  `isIndirect()` depends only on `obj != 0`
  (`include/qpdf/QPDFObjGen.hh:29-86`). Free rows are later reduced to an
  object-number-wide tombstone by `insertFreeXrefEntry`
  (`libqpdf/QPDF.cc:1149-1208`).
- Parsed indirect references have a separate validity boundary: qpdf converts
  the two integers to an indirect object only when `id >= 1`, `gen >= 0`, and
  `gen < 65535`; otherwise it adds null
  (`libqpdf/QPDFParser.cc:157-178`).
- The live qpdf probe is `qpdf --check
  vendor/qpdf-qtest/qpdf/fax-decode-parms.pdf`, which exits 0 without warnings.
  The flpdf baseline fails before the unknown `/DecodeParms` is reached because
  the fixture contains `0000000000 65536 f` and flpdf reports `invalid
  fixed-width u16`.

## Current mismatch

`ObjectRef` currently stores a `u16` generation and is used by
`xref.rs::XrefRegistration`, public indirect handles, and linearization's
synthetic container identity. `parse_xref_table` therefore collapses qpdf's raw
`QPDFObjGen` into a valid-reference representation before qpdf's free-row and
indirect-reference decisions have occurred.

The fix must not widen `ObjectRef` and introduce another sentinel, skip only
the `65536` row in the parser, or modify the qtest fixture. Those approaches
would preserve the mixed responsibility that caused the failure.

## Design

Add an internal qpdf-shaped `QpdfObjGen` value with signed integer object and
generation fields. It is the key for raw xref parsing and registration while a
classic/xref-stream section is being interpreted.

`QpdfObjGen` owns:

- signed object/generation identity and qpdf ordering/equality;
- `is_indirect()` with qpdf's `object_number != 0` rule;
- conversion to `ObjectRef` only at the valid indirect-reference boundary
  (`object >= 1`, `0 <= generation < 65535`, and representable object number).

`xref.rs` changes its raw parsing structures (`ParsedXrefEntry` and
`XrefRegistration`) to use `QpdfObjGen`. Free rows are converted to the
object-number-wide `deleted_objects` tombstone before the effective live table
is exposed. Valid live rows convert to `ObjectRef` at the canonical resolver
handoff. An invalid live generation remains a raw xref fact during registration
but cannot become an indirect handle, matching qpdf's parser boundary.

The public `ObjectRef` contract and the existing linearization synthetic identity
are not silently repurposed. Their separate cleanup remains tracked by the
existing ObjectRef invariant work; this slice only prevents those concepts from
contaminating qpdf's raw xref registration.

The `QPDFObjGen` correspondence row in `docs/qpdf-correspondence.md` will be
updated to point to the new raw xref primitive and to distinguish it from
`ObjectRef`'s valid indirect-reference surface.

## Data flow

```text
classic/xref-stream bytes
        |
        v
  QpdfObjGen (raw int identity)
        |
        +-- free row -> deleted object-number tombstone
        |
        +-- valid indirect generation -> ObjectRef -> canonical resolver/cache
        |
        `-- invalid indirect generation -> no ObjectHandle, qpdf null boundary
```

## Testing

- Add unit coverage for `QpdfObjGen` ordering, `is_indirect`, valid conversion,
  and rejection of invalid indirect generations.
- Add a canonical xref regression whose free object 0 row uses generation
  `65536`; it must open without recovery warnings and retain the same effective
  live entries as qpdf.
- Add coverage for a live row with an invalid indirect generation and for
  object-header references at the `65535` boundary, proving the raw xref and
  parser gates are distinct.
- Run the focused Rust xref/reader tests and qpdf 11.9.0 probes before the
  production change (RED), then rerun them after the change (GREEN).
- Run the focused `decode-parameters.test` and finally the full
  `QTEST_FULL=1` corpus with the same-run `harness.log` and
  `qtest-results.xml` pair.

## Non-goals

- Do not vendor or edit qpdf-qtest fixtures in flpdf.
- Do not change stream filter semantics, DecodeParms retention, writer filter
  policy, or runtime filter registration in this slice.
- Do not replace the linearization sentinel in the same change; keep that
  separate from raw xref identity.
- Do not preserve a backward-compatible adapter solely for the old mixed xref
  representation.
