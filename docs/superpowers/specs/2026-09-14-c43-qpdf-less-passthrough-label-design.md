# C43 qpdf-less passthrough label removal

## Goal

Remove the remaining `passthrough_codec_label` bridge from flpdf's stream
filter code. qpdf 11.9.0 has no equivalent label helper; unfilterable streams
use the generic filterability/error boundary.

This slice does not remove C28's public recovering/`DecodeLimits` API. C28 is
a separate qpdf-less public extension and will be tracked independently.

## Evidence and route classification

- Route: C43, classified `bridge` in
  `docs/qpdf-route-matrix/c-stream-pipeline-encryption.md`.
- Current entrypoint: `crates/flpdf/src/stream_filter.rs::passthrough_codec_label`.
- Current caller: `undecodable_filter_error` in the same module. The former
  public forwarding wrapper in `filters.rs` was already removed by `.48.91`.
- qpdf registers factories at `libqpdf/QPDF_Stream.cc:85-94`; DCT is
  registered, while CCITT/JBIG2/JPX have no factory entry. Unknown names are
  rejected by the lookup at `libqpdf/QPDF_Stream.cc:419-435`.
- qpdf's stream accessor reports the generic unfilterable boundary at
  `libqpdf/QPDF_Stream.cc:344-360`; it has no `passthrough codec ...` message.

## Design

1. Keep the canonical `stream_filter_for` registry and DCT pipeline unchanged.
2. Delete `passthrough_codec_label` and remove its dedicated branch from
   `undecodable_filter_error`. Unsupported names use the existing generic
   `unsupported stream filter: <name>` boundary in this compatibility path.
3. Update the source-near route contract test and add a unit assertion for the
   generic error boundary covering image/binary and arbitrary unknown names.
4. Update the C43 route row and both aggregate summaries only after a fresh
   zero-caller check. The bounded count changes are C canonical 36 / bridge 1,
   A-E canonical 101 / bridge 1 / mixed 58, and checker logical canonical 127 /
   bridge 7 / mixed 125. Do not relabel C28 or claim route-wide parity.

## Invariants

- `DCTDecode` registration, decoding, and raw writer passthrough are unchanged.
- No compatibility adapter, label table, or public API is introduced.
- C28 recovering APIs and TIFF hardening tests remain outside this slice.

## Verification

- New assertions fail before production deletion and pass afterward.
- Run format, focused stream-filter tests, route/deviation checks, and the
  qpdf 11.9.0 unfilterable-stream probe.
- Run `qpdf-route-callers.py --expect-zero` for the removed C43 symbol.
