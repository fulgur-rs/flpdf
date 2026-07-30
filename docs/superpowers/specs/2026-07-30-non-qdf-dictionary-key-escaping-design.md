# Non-QDF Dictionary-Key Escaping Design

**Issue:** `flpdf-n9t0.7`

## Goal

Make every generic non-QDF dictionary serializer emit decoded dictionary-key
bytes as one valid PDF name token, matching qpdf 11.9.0. A key such as the raw
bytes `A B#C/D\x80E` must serialize as `/A#20B#23C#2fD#80E`, never as raw
delimiter, whitespace, or non-ASCII bytes.

## Oracle

qpdf 11.9.0 routes dictionary and trailer keys through
`QPDF_Name::normalizeName`:

- `libqpdf/QPDFWriter.cc:1177` normalizes keys in `writeTrailer`.
- `libqpdf/QPDFWriter.cc:1495` normalizes keys in generic dictionary output.
- `libqpdf/QPDF_Name.cc:17-43` defines the escaping rules.

flpdf already implements those rules in
`crate::object::write_name_escaped`, including qpdf's lowercase hex and the NUL
sentinel behavior. QDF dictionary serializers and `Object::Name` already use
that helper.

## Scope

Replace raw key writes with `write_name_escaped` in these four non-QDF
serializers:

1. `Dictionary::write_pdf`
2. `Dictionary::write_pdf_with_id_writer`
3. `Dictionary::write_pdf_stream`
4. `Dictionary::write_pdf_trailer`

The Bead named the first three paths. Inspection of current `main` found the
fourth production path with the same raw-key write; leaving it unchanged would
retain the bug in classic trailer output.

The change must not alter:

- `BTreeMap` key ordering;
- `/ID` value substitution or trailer-last placement;
- `/Length`, `/Filter`, or `/DecodeParms` stream ordering;
- QDF layout;
- the bytes of keys that require no escaping.

No new escaping helper or serializer abstraction is needed.

## Test Strategy

Follow RED-GREEN-REFACTOR:

1. Add exact-output unit tests for all four serializers using a key containing
   whitespace, `#`, `/`, and a non-ASCII byte. Run them before production edits
   and confirm that each fails because raw key bytes are emitted.
2. Apply the four minimal call-site changes and rerun the focused tests.
3. Use a synthetic PDF fixture and qpdf 11.9.0 to verify observable ordinary
   dictionary, stream dictionary, and trailer output. Where a private helper
   has no distinct end-to-end route, its exact-output unit test is authoritative
   for the helper contract.
4. Run existing qpdf byte-identity goldens with `qpdf-zlib-compat`, workspace
   formatting, tests, clippy, and fresh 100% changed executable-line coverage.

## Acceptance Criteria

- All four serializers escape keys through `write_name_escaped`.
- Regression tests prove each changed call site fails before and passes after
  the fix.
- qpdf 11.9.0 probes agree for every observable output route.
- Existing plain, stream, trailer, QDF, and byte-identity tests remain green.
- Changed executable-line coverage is 100%.
