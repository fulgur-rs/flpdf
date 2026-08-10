# flpdf-25kg.4.2.1 DCT streaming primitive

**Date:** 2026-08-10
**Status:** Approved design; implementation pending written-spec review
**Beads issue:** `flpdf-25kg.4.2.1`

## Goal

Port the qpdf 11.9.0 `Pl_DCT` decode pipeline and `SF_DCTDecode` filter
factory into flpdf. The new primitive must be usable through the existing
`StreamFilter::decode_pipeline` stage-construction boundary and must emit the
same decoded component bytes as qpdf's default 8-bit libjpeg path for the
qpdf-required corpus.

This slice owns the reusable DCT decode primitive. It does not own stream
decode-level policy or the consumer cutover that will make
`QPDF_Stream::pipeStreamData` equivalent production code.

## Oracle responsibility and route inventory

Pinned qpdf 11.9.0 is authoritative:

- `Pipeline` and `Pl_Buffer` define the write/finish and buffered-input
  lifecycles (`include/qpdf/Pipeline.hh:19-33,46-60`,
  `include/qpdf/Pl_Buffer.hh:22-29,38-58`,
  `libqpdf/Pl_Buffer.cc:19-47`).
- `Pl_DCT::write` only appends encoded bytes. `finish` finalizes the buffer,
  special-cases empty input, decodes with libjpeg, writes one output scanline
  at a time, and finishes the downstream pipeline
  (`include/qpdf/Pl_DCT.hh:30-70`,
  `libqpdf/Pl_DCT.cc:83-141,196-326`).
- `SF_DCTDecode` constructs and owns a `Pl_DCT("DCT decode", next)` stage,
  accepts only null decode parameters through the base implementation, and
  classifies the filter as specialized and lossy
  (`libqpdf/qpdf/SF_DCTDecode.hh:8-40`,
  `include/qpdf/QPDFStreamFilter.hh:35-61`).
- `/DCT` is expanded to `/DCTDecode`, the factory is registered, and
  specialized/lossy classification is consumed at the `QPDF_Stream` boundary
  (`libqpdf/QPDF_Stream.cc:72-94,379-484,504-568`).
- qpdf is compiled for 8-bit samples; `Pl_DCT.cc:10-12` rejects other
  `BITS_IN_JSAMPLE` builds. The flpdf primitive therefore targets 8-bit JPEG
  decode and reports codec failure for unsupported precision.

The route classification is:

- **Canonical:** `StreamFilter::decode_pipeline` and the
  `stream_filter_for` registry. This slice adds the DCT factory and stage
  here. New tests exercise this route directly.
- **Bridge:** `pipe_decode_recovering` / `pipe_codec`, which still materialize
  one complete filter payload. Their semantics remain unchanged until
  `flpdf-3yn9.6` performs the bounded consumer migration.
- **Writer preservation:** existing DCT passthrough in `filters.rs` remains
  unchanged. Adding a decode stage must not turn writer recompression into a
  new policy decision.
- **Out of scope:** `json_inspect.rs` decode-level fallback, CLI behavior,
  test-driver migration, and multi-filter orchestration.

## Alternatives considered

1. **Extend the whole-buffer bridge.** This would be smaller mechanically, but
   it would place a qpdf-owned stage responsibility in the wrong route and
   would not prove stage construction, chunking, or downstream ownership.
2. **Add the canonical stage with the published Rust backend.** This preserves
   the qpdf-shaped boundary, keeps the default build Pure Rust, and uses the
   evaluated `libjpeg-turbo-rs = "=0.8.0"` implementation. This is selected.
3. **Make C libjpeg the default backend.** This would maximize dependence on
   the local qpdf ABI but would discard the approved Pure Rust default despite
   the published 0.8.0 corpus matching qpdf's required decode and encode
   cases. It remains an explicit compatibility backend instead.

## Selected architecture

### `Pl_DCT` pipeline stage

Add a crate-private `pipeline/dct.rs` module and register it from
`pipeline.rs`.

`PlDct<'a>` has one downstream `Pipeline` borrow and one qpdf-shaped buffered
input owner. Its `write` method only appends to the compressed JPEG buffer and
does not call downstream. Its `finish` method:

1. marks the buffer finished and takes its owned bytes;
2. calls downstream `finish` directly for empty input, including a repeated
   finish after the buffer has been consumed;
3. selects the configured backend and decodes with qpdf's default output
   settings for non-empty input;
4. forwards each decoded scanline as one downstream `write` call; and
5. calls downstream `finish` after the last scanline.

The Rust backend uses `libjpeg_turbo_rs::ScanlineDecoder` without output-format,
upsampling, DCT, smoothing, or color-space overrides. This preserves the
crate's default Gray/RGB/CMYK selection and qpdf's default libjpeg behavior.
Codec errors become `PipelineError::Runtime` with the codec message. A
downstream write or finish error is returned unchanged. The stage does not
invent a sentinel, panic on malformed JPEG, or finish downstream after a
non-empty decode error.

### `SF_DCTDecode` factory

Add a `DctStreamFilter` implementation in `stream_filter.rs`:

- `set_decode_params` uses the existing trait default, accepting absent/null
  parameters and rejecting a non-null dictionary;
- `decode_pipeline` returns `Some(Box<PlDct>)` around the supplied downstream
  pipeline;
- `is_specialized_compression` returns `true`; and
- `is_lossy_compression` returns `true`.

Add the `/DCTDecode` arm to `stream_filter_for`. Existing abbreviation
normalization already maps `/DCT` to `/DCTDecode`; no second abbreviation
table or consumer-side special case is added.

### Backend selection

The normal build adds the exact published dependency
`libjpeg-turbo-rs = "=0.8.0"` and uses it by default. The explicit
`qpdf-libjpeg-compat` feature selects a small C shim compiled against the
system `libjpeg` ABI. The shim exposes only a scanline decode callback and
error result, avoiding C struct definitions in Rust. It is used for strict
qpdf compatibility runs only when a DCT-attributable qtest mismatch requires
it; it is not selected by default and is distinct from the crate's
test-only `full-c-parity` feature.

The feature is forwarded to the core crate's compatibility test consumers in
the same way as `qpdf-zlib-compat`. No runtime backend switch or per-input
oracle is added.

The qpdf `Pl_DCT` compression constructor is not implemented in this slice.
The decision issue places compression consumers in `QPDFJob`/writer policy,
while this Bead owns the decode primitive and `SF_DCTDecode` integration.

## Verification design

Tests are written against the canonical factory/stage route before production
code is added.

Unit tests in the pipeline/filter modules cover:

- valid grayscale, RGB, CMYK/YCCK, progressive, restart, and sampling cases;
- the same JPEG delivered in one write and across multiple chunk boundaries;
- exact scanline bytes and exactly one downstream write per decoded row;
- empty input and repeated finish behavior;
- malformed and truncated JPEG errors;
- downstream write and finish failures with original error propagation;
- null versus non-null `/DecodeParms` construction behavior; and
- `DCTDecode` registration with specialized/lossy classification.

Oracle differential tests use the pinned qpdf 11.9.0 binary and real PDF
fixtures. For decode level `all`, they compare the stream bytes after qpdf
removes `/DCTDecode` with the bytes emitted by the canonical flpdf stage.
The probe records exit status, warnings, filter removal, and decoded bytes.
The qtest suite is then run with the Rust backend; any mismatch must be
classified before enabling the explicit C compatibility feature. A mismatch
attributable to another backend or consumer is not hidden by changing DCT
semantics.

Quality gates for the implementation are `cargo fmt --all -- --check`, focused
unit/oracle tests, workspace tests, all-features clippy, the qpdf compatibility
matrix, and fresh changed-line coverage with no unjustified coverage ignore.

## Scope boundary and completion conditions

The implementation is complete when the canonical DCT factory and pipeline
are qpdf-source-mapped, the Rust 0.8.0 backend passes the qpdf-required
8-bit corpus, the explicit C compatibility path compiles and is selectable,
writer passthrough remains byte-for-byte unchanged, and all tests/gates pass.

The following remain follow-up work: `QPDF_Stream::pipeStreamData` consumer
cutover, decode-level selection, JSON/CLI/test-driver migration, writer
recompression, and removal of the whole-buffer bridge after its final caller
is migrated.
