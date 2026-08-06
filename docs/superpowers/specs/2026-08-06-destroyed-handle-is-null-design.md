# Destroyed Handle `is_null` Parity Design

**Beads issue:** `flpdf-nrp3`

## Goal

Make a surviving `ObjectHandle` from a dropped `Pdf` report `is_null() == false`,
matching qpdf 11.9.0's `QPDFObjectHandle::isNull()` for `ot_destroyed`.

## Scope

`ObjectHandle::is_null` will distinguish `IndirectState::Destroyed` from a
literal null and a missing indirect reference. `Missing` continues to report
null. No new error path, sentinel, state variant, or public API is introduced.

`with_value` keeps its current fallback for `Destroyed`. This preserves the
existing behavior of accessors that have no error channel, including the
documented `unparse_resolved` fallback. The fix is therefore limited to the
boolean accessor that corresponds to qpdf's `isNull`.

## qpdf boundary

- `QPDF::~QPDF` disconnects cached indirect objects and turns non-null ones
  into `QPDF_Destroyed` (`libqpdf/QPDF.cc:215-235`).
- `QPDFObjectHandle::isNull` returns true only for `ot_null`
  (`libqpdf/QPDFObjectHandle.cc:352-356`).
- A destroyed `/Filter` is neither null, a name, nor an array, so
  `QPDF_Stream::filterable` rejects it as an invalid filter type
  (`libqpdf/QPDF_Stream.cc:391-413`).

## Tests

Add red/green regression coverage for a real `Pdf` drop: the retained handle
has type code 14 and is not null. Add a stream-filter regression proving a
destroyed `/Filter` no longer becomes an empty filter list and reaches the
existing invalid-filter-type error path. Literal null and missing-reference
behavior remain covered by their existing tests.

## Non-goals

- Changing `with_value` or other accessor fallback behavior.
- Changing missing-reference semantics.
- Changing qpdf's warning-versus-flpdf error policy for invalid filters.
