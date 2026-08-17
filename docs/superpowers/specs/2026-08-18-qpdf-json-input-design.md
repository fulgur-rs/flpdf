# qpdf 11.9.0 JSON input boundary design

## Scope

This design ports the input side of qpdf 11.9.0 `QPDF_json.cc:1-833` to
flpdf. It covers the canonical `ObjectHandle` document boundary for complete
JSON creation, partial document updates, incremental `JSONReactor` dispatch,
deferred stream data, qpdf warnings, and the qpdf v2 value grammar. The CLI
`--json-input` and `--update-from-json` option wiring remains the dependent
`flpdf-3yn9.16` task.

The pinned qpdf source and observed `/usr/bin/qpdf` behavior are authoritative.
The existing `json/parser.rs` Reactor event contract and the already merged
canonical ObjectHandle, stream-provider, replacement, and page-tree primitives
are the implementation substrate.

## Responsibility boundary

The new importer owns the responsibility represented by qpdf's
`QPDF::JSONReactor`, `createFromJSON`, `updateFromJSON`, and `importJSON`:

- validators for qpdf object keys, indirect references, PDF names, Unicode
  strings, binary strings, and JSON metadata;
- state transitions for `top`, `qpdf`, metadata, objects, trailer, object,
  and stream containers;
- canonical reservation, replacement, trailer mutation, object descriptions,
  and end-of-document dangling-reference normalization;
- deferred inline base64 and `datafile` providers; and
- complete/update validation, warnings, and error aggregation.

The importer does not modify the legacy `Pdf::set_object` bridge and does not
add new callers to it. Existing output serialization remains in
`document_json.rs`. CLI job ordering remains downstream.

## Data flow

1. Complete creation opens qpdf's exact rootless `JSON_PDF` seed
   (`%PDF-1.3`, empty xref, `/Size 1`) through the normal `Pdf` parser. It
   never calls `Pdf::empty()` and does not add cleanup special cases.
2. The JSON input is owned by a shared, seekable source. The parser consumes
   it incrementally through `json::parse_reader`; the importer retains only
   the source handle and scalar state needed by the current Reactor frame.
3. `make_object` converts each JSON value directly to a document-owned
   `ObjectHandle`. Container values are consumed by the Reactor so the parser
   does not retain a second complete JSON tree.
4. Object keys first obtain the canonical handle for their exact
   `ObjectRef`. A value replacement retains that identity. A stream replacement
   uses the canonical `new_stream`/dictionary/provider path. An update never
   removes objects omitted from the JSON document.
5. Inline stream data records the string's exclusive source offsets and
   registers a provider. At pipe time the provider seeks to the range, feeds
   chunks through the existing base64 decoder, and never stores decoded stream
   bytes in a `Vec`. `datafile` opens and streams its named file only when
   invoked.
6. After the root container closes, complete/update validation and qpdf's
   reserved-reference normalization run in the same order as
   `QPDF_json.cc:359-429`. Update metadata delegates only to canonical page
   helpers and only when the JSON flag is true.

## Stacked PR boundaries

The implementation is split into dependent Beads/branches so every PR has a
single qpdf responsibility and its own tests.

### PR 1: JSON value factory and reservation

Add the qpdf validators and canonical value conversion for null, booleans,
integers, reals, scientific notation, arrays, dictionaries, indirect
references, PDF names, Unicode strings, and binary strings. Add the exact
canonical reservation helper needed by the importer without changing the
legacy bridge. Tests cover valid values, malformed values, descriptions, and
identity reuse.

### PR 2: Deferred stream input providers

Add the importer-owned source-backed providers for inline base64 and
`datafile`. Tests use instrumented seekable readers and provider call counts to
prove registration is lazy, offsets are exclusive of JSON quotes, retries are
stable, and read/seek/decode failures cross the qpdf warning/error boundary.

### PR 3: JSONReactor state machine

Implement the complete qpdf state machine and validation, including unknown-key
forward compatibility, object/trailer/stream construction, replacement,
object descriptions, warning attribution, and dangling normalization. Tests
cover complete and update mode, both/neither value/stream and data/datafile
errors, missing metadata, malformed object keys, and preserved omitted objects.

### PR 4: Public document boundary and parity fixtures

Expose create/update/import entry points, wire the rootless seed and error
aggregation, delegate the two update page flags, add flpdf-authored JSON/PDF/
QDF round-trip fixtures, and update `docs/qpdf-correspondence.md`. The
dependent CLI task consumes only this completed boundary.

## Error and warning policy

Parser failures retain the parser's exact offset and are wrapped once with the
input description, matching `QPDF::importJSON`. Semantic failures are recorded
through the document warning sink with the current object key and JSON value
offset; parsing continues where qpdf continues so multiple semantic errors are
reported before the final `errors found in JSON` result. Provider failures are
reported only when the stream is piped, not at registration.

## Verification

Each stacked PR must pass its focused Rust tests and the qpdf 11.9.0 oracle
probes for the behavior it owns. Before a PR is marked ready, run formatting,
all-features clippy, focused tests, workspace tests, strict private rustdoc,
and per-PR patch coverage. Use only flpdf-authored fixtures; qpdf-qtest
fixtures remain in the separate qpdf test repository.

The final PR must prove every acceptance criterion of `flpdf-3yn9.15` and
leave `flpdf-3yn9.16` as the sole downstream CLI dependency.
