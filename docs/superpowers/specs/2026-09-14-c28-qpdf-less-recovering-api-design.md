# C28 qpdf-less recovering filter API removal

## Goal

Remove flpdf's public recovering stream-decoder API and its legacy internal
implementation. These APIs expose partial output/events and opt-in resource
limits that have no qpdf 11.9.0 counterpart. Public API compatibility is not
a retention constraint for this pre-v1 qpdf parity project.

## Evidence and route classification

- Route: C28, currently classified `bridge` in the stream route matrix.
- Public API in `crates/flpdf/src/filters.rs`: `StreamDecodeWarning`,
  `StreamDecodeEvent`, `StreamDecodeOutcome`, `DecodeLimits`,
  `decode_stream_data_recovering`, and
  `decode_stream_data_recovering_with_limits`.
- Current external callers are only the TIFF hardening integration tests;
  qtest test 0/1 was already migrated to canonical ObjectHandle pipe by
  `.48.93`. No ordinary library or CLI consumer enters this route.
- qpdf 11.9.0 exposes `getStreamData` as a complete decoded-buffer result
  and `pipeStreamData` as a sink operation with a bool/exception boundary
  (`include/qpdf/QPDFObjectHandle.hh:990-1068`,
  `libqpdf/QPDF_Stream.cc:344-360,488-638`). It does not expose partial
  `StreamDecodeEvent`/`StreamDecodeOutcome` values or `DecodeLimits`.
- qpdf's TIFF predictor and DCT stages have qpdf-shaped constructors and
  streaming pipeline behavior, but no flpdf-style output or memory cap.

## Design

1. Add source-near RED assertions that the public `filters` module, the
   recovering API names, and recovery-only stream-filter/DCT/TIFF limit seams
   are absent.
2. Delete `filters.rs`, its public module declaration, and the standalone TIFF
   hardening tests. Do not replace them with a compatibility facade.
3. Remove only recovery-only machinery from `stream_filter.rs`: filter-spec
   copy parsing, partial-result/error/event types, recovering pipe methods,
   output sink limits, and C28-only warning/phase plumbing.
4. Keep the canonical StreamFilter factory, decode-pipeline construction,
   warning callback, DCT, Flate/LZW, ASCII, RunLength, Crypt, and writer
   `encode_flate` paths unchanged.
5. Remove the qpdf-less max-output and TIFF-memory extensions from `PlDct`
   and `TiffPredictor`; make normal qpdf-shaped constructors the only path.
6. Update ObjectHandle comments, route matrix classifications, aggregate
   counts, and tracked symbols. C9's dead recovery spec reader becomes
   `canonical/absent` through `prepare_stream_filter_plan`.

## Invariants

- `ObjectHandle::pipe_stream_data`, `get_stream_data`, and canonical
  `prepare_stream_filter_plan` remain the ordinary decoder paths.
- DCT decoding/compression, TIFF geometry and output bytes, raw passthrough,
  warning order, encryption, and writer behavior remain unchanged.
- No qpdf-less public API, adapter, sentinel, output rewrite, or hardening cap
  is reintroduced.
- Canonical qpdf oracle fixtures remain; only tests solely for deleted C28
  hardening are removed.

## Verification

- New route/API assertions fail before deletion and pass afterward.
- Run format, focused route tests, stream/DCT/TIFF tests, full flpdf and
  qtest-tools tests, all-features clippy, qpdf probes/differentials, route
  citation checks, deviation checks, and caller-zero checks.
