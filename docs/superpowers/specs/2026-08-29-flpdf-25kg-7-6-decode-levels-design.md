# qtest decode-levels parity design

## Goal

Make the vendored qpdf 11.9.0 `decode-levels.test` pass all 14 cases through
the Rust CLI and the bounded Rust `qpdf-ctest` process adapter. The result must
preserve qpdf's decode-level ordering, all-or-nothing stream filterability,
warning/error channel, output-file behavior, and object inspection behavior.

## Current evidence

The current qtest run on flpdf `main` records 14 failures:

- cases 1, 3, 5, 7, and 12 fail because the top-level CLI rejects
  `--decode-level`;
- cases 2, 4, 6, 8, and 13 are downstream failures because `a.pdf` was not
  produced;
- case 9 reaches the existing adapter, which supports test 19 but not
  `qpdf-ctest 20`;
- cases 11 and 14 reach DCT decoding, but expose the Rust-only `DCT decode:`
  prefix in a warning that qpdf does not emit.

The qpdf 11.9.0 source is authoritative:

- `include/qpdf/Constants.h:150-158` defines the ordered levels
  `none < generalized < specialized < all`;
- `libqpdf/QPDFJob_config.cc:719-727` parses the four CLI values;
- `libqpdf/QPDFJob.cc:2847-2875` applies stream-data, compression, and explicit
  decode-level settings in that order;
- `libqpdf/QPDFWriter.cc:1239-1314` decides whether a stream is filtered and
  passes the selected level to `QPDFObjectHandle::pipeStreamData`;
- `libqpdf/QPDF_Stream.cc:504-512` rejects a complete chain when a specialized
  or lossy filter is above the selected level;
- `libqpdf/Pl_DCT.cc:83-141,298-326` emits raw libjpeg diagnostics without the
  pipeline identifier;
- `qpdf/qpdf-ctest.c:445-455` defines test 20's setter sequence.

## Architecture

### CLI and writer state

Add one `CliDecodeLevel` value enum to both the qpdf-shaped top-level `Cli` and
the native `RewriteCommand`. Store the value plus an explicit-set bit in the
CLI-owned `WriterOptions`. `writer_configuration` will replay settings in
qpdf's fixed order: `stream-data`, `compress-streams`, then explicit
`decode-level`. The explicit-set bit is required so QDF can still raise the
decode level to generalized when no explicit level was supplied, while
`--qdf --decode-level=none` remains an explicit override.

The JSON job consumer will use the same CLI value, defaulting to generalized,
so accepting `--decode-level` never creates a silently ignored JSON option.
The existing `WriterConfiguration::set_decode_level` and
`WriterSettings::decode_level_set` remain the canonical state owners; no new
writer-side compatibility adapter will be introduced.

### Stream filter gate and DCT diagnostics

Extend the existing writer-side `filter_chain_is_decodable` decision with the
qpdf lossy rule: `/DCTDecode` and `/DCT` are filterable only at
`DecodeLevel::All`. The existing generalized and specialized rules remain
unchanged, and the full chain remains all-or-nothing. Successful DCT decoding
continues through the existing registered `DctStreamFilter` and `PlDct`
pipeline; writer passthrough is not restored for an in-level valid DCT stream.

Keep `PlDct`'s identifier for pipeline introspection, but map codec and
stage-local runtime errors to the underlying qpdf diagnostic text without
prepending `DCT decode:`. Downstream pipeline errors remain propagated from the
downstream owner. Update source-near unit expectations and CLI regression tests
to assert qpdf's public diagnostic contract.

### qpdf-ctest process adapter

Extend `crates/flpdf-qtest-tools/src/bin/qpdf_ctest.rs` with test 20. It will
open the input once with the existing C-API-like password boundary, then call
the canonical `PdfWriter` setters in exactly this order:

1. static ID;
2. static AES IV;
3. compression disabled;
4. specialized decode level;
5. write and report errors;
6. print `C test 20 done`.

This is a bounded process adapter for portable PDF behavior, not a C ABI
implementation. It will not modify vendored qtest files.

## Verification

The implementation will use RED/GREEN cycles for:

1. CLI parsing and writer configuration, including each decode level, explicit
   `none`, QDF default/override, and JSON propagation;
2. DCT all-level emission and below-all preservation, plus exact malformed-DCT
   warning text and exit status;
3. qpdf-ctest test 20's argument, output, error, and setter behavior.

After the focused Rust tests pass, run `decode-levels.test` against a disposable
qtest data copy and retain its same-run `harness.log` and `qtest-results.xml`.
Then run the full qtest corpus with `QTEST_FULL=1`, validate the paired
artifacts with `verify-parity-manifest.py`, promote only rows proven by that
run, and add the complete `decode-levels` suite to the qtest allowlist.

The flpdf worktree will run `cargo fmt --all -- --check`, focused package tests,
workspace tests, and the repository's full quality commands before handoff.

## Non-goals

- no C/C++ ABI or libqpdf linkage;
- no edits to `vendor/qpdf-qtest` or `vendor/qtest`;
- no broad qpdf-ctest test migration beyond test 20;
- no change to specialized image codec passthrough policy below the selected
  decode level;
- no change to the sole allowed DEFLATE byte-output deviation.
