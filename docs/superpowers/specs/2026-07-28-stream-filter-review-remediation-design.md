# Stream Filter Review Remediation Design

## Goal

Address both actionable review threads on PR #576 without changing the public
stream-decoding API:

1. preserve qpdf-style truncated-Flate warnings instead of discarding them; and
2. avoid copying the complete encoded stream before the first filter runs.

The change remains limited to the stream-filter cutover introduced by
`flpdf-qynx.5.1`.

## Warning handling

The shared decode implementation will accept an internal warning callback.
`PlFlate` warnings will flow through that callback instead of terminating at
`ignore_warning`.

The two existing consumer classes will apply different policies:

- `decode_stream_data` and `decode_stream_data_with_limits` have no warning
  channel. They will convert a truncated-Flate warning into
  `Error::Unsupported`, preserving the documented contract that malformed
  codec input is an error.
- The `--check` content-stream walk already owns a `Diagnostics` collection.
  It will collect the warning, emit a warning `Diagnostic` that names the page
  and content stream, and keep the report valid when no independent error is
  present.

This matches qpdf 11.9.0's distinction: `Pl_Flate` reports `Z_BUF_ERROR` through
its warning callback, and `QPDF_Stream` routes the callback to the document
warning channel.

No new public warning-aware API will be added.

## Borrowed first-stage input

The filter-chain loop will hold its current input as `Cow<'_, [u8]>`, initially
borrowing the caller's `stream_data`. The first filter therefore receives the
original slice. Each filter result becomes owned storage and supplies the next
stage.

An empty filter chain still returns an owned `Vec<u8>`, as required by the
existing return type. Crypt and predictor behavior, filter ordering, output
limits, and the maximum chain length remain unchanged.

## Error handling

- A Flate warning becomes an error only at entry points that cannot return
  warnings.
- A real codec failure remains an error in every path.
- The output-limit sentinel remains a warning in `--check` and an error in the
  decoding APIs.
- If a content stream emits both a warning and a later error, `--check` records
  both and the error keeps the report invalid.

## Test strategy

Implementation follows RED-GREEN-REFACTOR in two independent cycles.

1. Add a public decode regression test showing that the one-byte zlib prefix
   `0x78` no longer succeeds silently. Add a checker regression test showing
   that the same payload produces a warning diagnostic rather than an error.
2. Add a test-only stream filter probe that records the pointer passed to its
   first decode stage. Assert that it equals the original input slice pointer;
   the current eager `to_vec` implementation must fail this test.

After the focused tests pass, run formatting, Clippy, the workspace test suite,
qpdf module documentation validation, and fresh patch coverage against
`origin/main`.

## Non-goals

- Adding a public warning collection API.
- Changing qpdf's warning text or `PlFlate` lifecycle.
- Refactoring predictor allocation or later filter-stage buffers.
- Replying to or resolving GitHub review threads.
