# Filespec helper handle rework

**Issue:** flpdf-d9sq
**Oracle:** qpdf 11.9.0 `QPDFFileSpecObjectHelper` and `QPDFEFStreamObjectHelper`

## Decision

Replace the D1 helper's `ObjectRef` plus copied `Stream` representation with
the crate's shared `ObjectHandle` representation.  qpdf helpers are wrappers
over `QPDFObjectHandle`, so indirect-reference transparency and in-place
dictionary mutation are helper responsibilities, not caller conveniences.

`FileSpec::new` is handle-first. `FileSpec::from_ref` remains a Rust
document-ownership convenience, while `ObjectRef` is no longer the helper's
only constructor contract. The public qpdf-shaped getters use qpdf's
empty-string or zero defaults; lower-level optional inspection remains a
separately named API and does not duplicate qpdf lookup rules.

## Boundaries

- `FileSpec` holds a Filespec `ObjectHandle`; `EmbeddedFileStream` holds an
  EmbeddedFile stream `ObjectHandle`.
- A helper resolves a handle before inspecting a value.  Therefore `/EF`,
  `/Params`, every scalar parameter, and `/Subtype` accept indirect references
  exactly as qpdf's `QPDFObjectHandle` accessors do.
- A setter modifies the live dictionary handle.  It marks the mutated indirect
  object dirty and invalidates its materialized memo, but never reconstructs a
  stream merely to alter metadata.
- `setParam` changes an existing dictionary reached via `/Params`; an absent or
  non-dictionary `/Params` is replaced on the stream dictionary with a fresh
  direct dictionary, matching qpdf's `replaceKeyAndGetNew` path.
- Factory and convenience-builder operations create indirect handles where a
  serialized object identity is required.  They are compositions of the two
  helpers, not a second dictionary implementation.

## Rust mapping

qpdf's `std::string` result becomes `Vec<u8>` because the qpdf UTF-8 view may
contain invalid bytes.  Missing or wrongly typed qpdf string values become an
empty vector, and missing/wrong `/Size` becomes zero.  `ObjectHandle::null()`
is the Rust result for qpdf's null-object return from embedded-stream lookup.
`Result` remains for PDF resolution, parsing, and I/O failures.

## Tests

Tests first prove indirect `/Subtype` and each indirect `/Params` scalar,
including a holder chain.  Mutation tests prove direct and indirect `/Params`
write to the same locations qpdf writes, survive a writer round trip, and do
not require an `EmbeddedFileStream` payload snapshot.  Consumer tests retain
attachment-list behavior using the qpdf-shaped empty values.
