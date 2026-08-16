# ASCII85 decoder route cutover design

## Status

Approved by the user on 2026-08-16. Implementation is pending spec review.

## Goal

Remove the legacy flpdf-specific ASCII85 encoder route and make the qpdf-shaped
streaming decoder route explicit as `pipeline/ascii85_decoder.rs`.

The final production design must contain an ASCII85 decoder, but no production
ASCII85 encoder. Requests to encode a stream through `/ASCII85Decode` will
return `Error::Unsupported`, matching qpdf's responsibility boundary. Test
fixtures that need an ASCII85-wrapped payload may use fixed pre-encoded bytes
or a test-only fixture helper.

## Oracle and responsibility boundary

Pinned qpdf 11.9.0 is authoritative:

- `libqpdf/Pl_ASCII85Decoder.cc:7-108` owns incremental ASCII85 state,
  whitespace and EOD handling, partial-group flushing, downstream writes, and
  finish propagation.
- `libqpdf/qpdf/SF_ASCII85Decode.hh:14-18` constructs one
  `Pl_ASCII85Decoder` for the stream-filter stage.
- `libqpdf/QPDF_Stream.cc:86-94` registers the decoder factory and
  `:537-568` constructs registered filter stages in reverse order. The output
  side adds only Flate compression and content normalization; it does not add
  an ASCII85 encoder.

The current Rust route inventory is:

| Route | Current location | Responsibility | Decision |
| --- | --- | --- | --- |
| Canonical decoder | `crates/flpdf/src/pipeline/ascii85.rs` plus `stream_filter.rs` | qpdf-shaped streaming decode stage and factory wiring | Rename the module file to `ascii85_decoder.rs`; preserve behavior |
| Legacy encoder | `crates/flpdf/src/ascii85.rs` plus `filters.rs` | flpdf-only whole-buffer ASCII85 encoding used by encode helpers and test fixture construction | Delete production implementation and callers |

The legacy route is not repaired or moved into the decoder module. Moving it
would preserve a qpdf-nonexistent responsibility and would mix encode and
decode ownership in one qpdf-correspondence module.

## Architecture and migration

1. Rename `crates/flpdf/src/pipeline/ascii85.rs` to
   `crates/flpdf/src/pipeline/ascii85_decoder.rs`.
2. Change the pipeline module declaration and all imports, including
   `stream_filter.rs` and the live codec-oracle tests, to
   `pipeline::ascii85_decoder::Ascii85Decoder`.
3. Delete `crates/flpdf/src/ascii85.rs` and its root module declaration.
4. Remove the `ASCII85Decode` branch from
   `filters::apply_single_filter_encode`. The existing public
   `filters::encode_stream_data` and crate-internal handle variant retain their
   API shape, but return a clear `Unsupported` error for an ASCII85 encode
   request.
5. Keep the production decode path unchanged: `Ascii85StreamFilter` continues
   to construct one streaming decoder and the existing downstream/error
   contracts remain authoritative.
6. Replace production-test fixture construction that currently calls the
   encoder with fixed encoded payloads or test-only helpers. No test helper may
   be imported by production code.
7. Update qpdf correspondence/module-index documentation so the old encoder
   mapping is removed and the decoder mapping points to the new filename.

This is one bounded cutover: the canonical decoder consumer is already wired,
so no bridge caller remains after the rename. The only intentional behavior
change is that encode requests for ASCII85 become unsupported.

## Error and compatibility behavior

- Decoding keeps the current qpdf-compatible streaming behavior: PDF
  whitespace, `z` only at a group boundary, `~>` EOD, partial final groups,
  downstream write/finish propagation, and qpdf error messages.
- Encoding `/FlateDecode`, `/ASCIIHexDecode`, `/RunLengthDecode`, and existing
  predictor paths is outside this cutover and remains unchanged.
- Encoding `/ASCII85Decode` returns `Error::Unsupported` with a message that
  identifies ASCII85 encoding as unsupported and explains that qpdf only
  provides the decoder-side pipeline.
- Writer paths that decode a source stream and emit raw or Flate-compressed
  output are unchanged. They do not need the removed encoder.
- Existing public API compatibility for successful ASCII85 encoding is not a
  requirement for this pre-1.0 qpdf-parity cutover; the API remains callable
  so the failure is explicit rather than a compile-time disappearance.

## Testing strategy

The first implementation test is RED and exercises the canonical public encode
boundary:

- Add a focused test proving `encode_stream_data` rejects an ASCII85 encode
  request with `Error::Unsupported`.
- Run it before deleting the encoder and confirm it fails because the old
  implementation still succeeds.

Then migrate and verify:

- Move the existing decoder unit tests with the module and retain their
  coverage of qpdf success/error cases, split writes, EOD, partial groups, and
  downstream failure propagation.
- Run the focused pipeline, stream-filter, and filter tests, including the
  ASCII85/Flate and ASCII85/LZW decode-chain tests.
- Keep CLI and page tests that consume ASCII85 PDFs by supplying pre-encoded
  fixture bytes from test-only code.
- Search the final tree for stale production references to
  `crate::ascii85`, `ascii85::encode`, and the old module path.
- Run `cargo fmt --all -- --check`, focused package tests, both package test
  suites, and the workspace tests. Run the repository's all-features clippy
  and strict private rustdoc gates before handoff.

## Scope exclusions

- Do not remove or redesign the analogous ASCIIHex encoder in this change.
- Do not rewrite the existing streaming decoder algorithm; the rename and
  route cleanup must preserve its qpdf-observed semantics.
- Do not change unrelated stream-filter, writer, or compression policy.
- Do not add a compatibility adapter or a new production ASCII85 encoder.
