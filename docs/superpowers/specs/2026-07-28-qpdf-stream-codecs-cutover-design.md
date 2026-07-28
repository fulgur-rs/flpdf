# qpdf ASCII and RunLength Pipeline Cutover Design

**Issue:** `flpdf-qynx.5.2`<br>
**Date:** 2026-07-28<br>
**Oracle:** qpdf 11.9.0 (`v11.9.0`, commit `3b97c9bd`)<br>
**Oracle source:** `scripts/fetch-qpdf-source.sh --print-path`

## Problem

flpdf already decodes ASCII85, ASCIIHex, and RunLength streams, and encodes
all three formats. Those implementations are whole-buffer helpers derived
primarily from the PDF specification. They are not faithful mirrors of qpdf's
incremental components:

- qpdf implements ASCII85 decoding in `Pl_ASCII85Decoder`;
- qpdf implements ASCIIHex decoding in `Pl_ASCIIHexDecoder`;
- qpdf implements both RunLength encoding and decoding in `Pl_RunLength`;
- `SF_ASCII85Decode`, `SF_ASCIIHexDecode`, and `SF_RunLengthDecode` construct
  those components through the `QPDFStreamFilter` boundary.

The current production decoder routes Flate through `StreamFilter`, but falls
back to direct calls to `ascii85::decode`, `ascii_hex::decode`, and
`run_length::decode`. The current encoder calls `run_length::encode` directly.
This split preserves output for ordinary inputs while hiding incremental state,
downstream write boundaries, finish ordering, and several malformed-input
behaviors.

Observed qpdf 11.9.0 behavior differs from the current helpers in material
ways:

- ASCII decoders ignore vertical tab but reject NUL;
- ASCII85 allows whitespace between `~` and `>`, flushes pending data on
  `finish` even after a bare `~`, accepts a final one-character group with no
  output, and reduces five-character arithmetic to the platform
  `unsigned long` result before emitting its low 32 bits;
- ASCIIHex may have emitted complete bytes before a later invalid character;
- RunLength decoding treats `0x80` as a no-op in the top state and continues,
  and silently tolerates truncated literal and repeat packets at `finish`;
- qpdf's RunLength encoder chooses a run for two equal bytes, so `AA` encodes
  as `ff 41 80`, not a two-byte literal packet;
- the three stream-filter adapters reject every non-null `/DecodeParms`, and
  RunLength is classified as specialized compression.

The issue is therefore a production cutover, not an additive alternative
implementation. Once complete, no whole-buffer decoder or duplicate
RunLength implementation remains.

## Goals

1. Mirror the qpdf 11.9.0 ASCII85, ASCIIHex, and RunLength state machines as
   crate-private `Pipeline` stages.
2. Route production decoding for all three filters through `StreamFilter`.
3. Route production RunLength encoding through the same stateful component in
   encode mode.
4. Preserve exact qpdf byte output, state transitions, downstream call
   ordering, finish behavior, and error text for the supported component
   boundary.
5. Reject non-null `/DecodeParms` before constructing or writing to a codec
   pipeline.
6. Differentially verify the components against the pinned qpdf 11.9.0 source.
7. Delete superseded decoder and RunLength helper paths and prove their
   absence by repository search.

## Non-goals

- Adding qpdf ASCII85 or ASCIIHex encoders. qpdf 11.9.0 has no corresponding
  Pipeline components, so flpdf's existing encoder helpers remain.
- Moving Predictor handling into a Pipeline component. That work belongs to
  `flpdf-qynx.5.3`.
- Changing the public signatures of `decode_stream_data`,
  `decode_stream_data_with_limits`, or `encode_stream_data`.
- Introducing a general streaming public API. The components are incremental,
  while the existing public API continues to collect final output in memory.
- Changing LZW, Flate, Crypt, or image/binary passthrough behavior.
- Adding new decoded-output limits or changing the existing limit semantics.
- Treating PDF specification conformance as a substitute for qpdf 11.9.0
  parity where the two observable behaviors differ.

## Component layout

Add three modules under `crates/flpdf/src/pipeline/`:

```text
pipeline/
├── ascii85.rs
├── ascii_hex.rs
└── run_length.rs
```

`pipeline.rs` declares the modules. Each type borrows its downstream
`Pipeline`, retains only qpdf-equivalent state, implements `identifier`,
`write`, and `finish`, and maps qpdf `runtime_error` and `logic_error`
categories to `PipelineError::Runtime` and `PipelineError::Logic`.

The components are:

```rust
pub(crate) struct Ascii85Decoder<'a> { /* qpdf state */ }
pub(crate) struct AsciiHexDecoder<'a> { /* qpdf state */ }

pub(crate) enum RunLengthAction {
    Encode,
    Decode,
}

pub(crate) struct RunLength<'a> { /* action and qpdf state */ }
```

Names should remain idiomatic Rust, while module headers cite the exact qpdf
11.9.0 counterparts:

- `libqpdf/Pl_ASCII85Decoder.cc`;
- `libqpdf/Pl_ASCIIHexDecoder.cc`;
- `libqpdf/Pl_RunLength.cc`.

No shared global "finished" guard is added. qpdf's three components do not use
one, and their post-`finish` and reuse behavior follows from their retained
state rather than from a common Pipeline policy.

## ASCII85 decoder semantics

`Ascii85Decoder` mirrors qpdf's `pos`, five-byte input buffer, and three EOD
states:

- `0`: normal input;
- `1`: `~` has been seen and `>` is expected;
- `2`: complete `~>` has been seen.

At construction, `pos` is zero, EOD is zero, and all five buffer bytes are
initialized to ASCII `u` (`117`).

### Input processing

For every input byte:

1. Ignore exactly space, form feed, vertical tab, tab, carriage return, and
   line feed. Whitespace is ignored even while waiting for `>` after `~`.
2. If EOD is complete, ignore the byte and the remainder of subsequent writes.
3. If waiting for `>`, accept only `>`; flush the pending group and enter the
   complete EOD state. Any other non-whitespace byte raises
   `PipelineError::Runtime("broken end-of-data sequence in base 85 data")`.
4. In normal state, `~` enters the waiting state.
5. `z` emits four zero bytes only when `pos == 0`; otherwise it raises
   `PipelineError::Runtime("unexpected z during base 85 decode")`.
6. Other bytes must be in the inclusive range `!` through `u`. An invalid byte
   raises
   `PipelineError::Runtime("character out of range during base 85 decode")`.
7. A fifth digit triggers a flush.

The five-digit accumulator follows qpdf's multiply/add sequence. The Rust
implementation deliberately retains the low 32 bits before byte extraction,
matching qpdf 11.9.0 for inputs such as `uuuuu` instead of introducing a
stricter overflow error.

### Flush and finish

A zero-length flush is a no-op. Otherwise the component:

1. pads the unfilled input positions with `u`;
2. computes four big-endian output bytes;
3. records `pos - 1` as the number of bytes to emit;
4. resets `pos` and the five-byte buffer;
5. calls the downstream `write`.

The reset happens before the downstream call. If that call fails, a later
write or finish observes the reset state, as it does in qpdf.

`finish` always calls `flush`, regardless of EOD state, then calls downstream
`finish`. Consequences that are intentionally retained include:

- a bare `~` at end of input flushes a pending group instead of reporting a
  broken EOD sequence;
- a one-digit final group makes an observable zero-length downstream `write`
  call and still clears its state;
- a second `finish` reaches downstream `finish` again;
- a completed `~>` causes later writes to be ignored, but does not suppress
  later downstream `finish` calls.

If `flush` fails, downstream `finish` is not called.

## ASCIIHex decoder semantics

`AsciiHexDecoder` mirrors qpdf's two-character input buffer, nibble position,
and EOD boolean. The unused nibble is initialized to ASCII `0`, which supplies
the low zero nibble for a partial final byte.

For every input byte:

1. uppercase it with the same byte-domain effect as qpdf for ASCII input;
2. ignore exactly space, form feed, vertical tab, tab, carriage return, and
   line feed;
3. on `>`, mark EOD, flush the pending nibble, and ignore the rest of this and
   all later writes;
4. accept `0`–`9` and `A`–`F`, flushing after two nibbles;
5. otherwise raise a runtime error whose text is
   `character out of range during base Hex decode: ` followed by the
   offending byte with qpdf's C-string behavior; specifically, a NUL byte
   appends no visible suffix.

Each complete pair is written downstream immediately. Therefore output
already emitted before a later invalid character remains observable.

Flush resets the nibble position and both input digits before calling
downstream `write`. `finish` flushes a pending high nibble and then calls
downstream `finish`. A downstream write failure therefore leaves the decoder
reset. Repeated `finish` calls repeat downstream finish calls. There is no
additional incomplete-input error.

## RunLength semantics

`RunLength` mirrors qpdf's action, state, length, and 128-byte buffer. Its
states are top, copying, and run.

### Decode mode

In top state:

- a byte below `128` starts a literal packet of `byte + 1` bytes;
- a byte above `128` starts a repeat packet of `257 - byte` copies;
- `128` is an EOD marker that leaves the component in top state.

Unlike a terminal stream flag, `128` does not prevent later bytes in the same
or subsequent writes from starting more packets.

In copying state, each input byte is passed downstream as a separate
one-byte `write`; the component returns to top state after the declared count.
In run state, the next input byte is passed downstream in `length` separate
one-byte writes, then the component returns to top state.

`finish` does not diagnose an incomplete literal or a missing repeat byte.
It leaves that packet state unchanged and simply finishes downstream. A later
write therefore continues the incomplete packet, and repeated `finish` calls
call downstream `finish` again.

When a downstream write fails, the operation stops at that exact one-byte
call. State changes retain qpdf ordering:

- copying mode decrements its remaining length only after a successful write;
- run mode returns to top only after all repeated writes succeed.

### Encode mode

The encoder follows qpdf's top/copying/run transitions rather than choosing
packets from a pre-scanned whole buffer:

- one buffered byte remains in top state;
- a following equal byte switches to run state;
- unequal buffered bytes move to copying state;
- copying flushes at 128 bytes;
- a repeated byte encountered while copying removes the previous equal byte
  from the literal packet, flushes that literal, and seeds a run with the two
  equal bytes;
- a run flushes when a different byte arrives or at 128 bytes.

`flush_encode` writes a run as two downstream calls—header, then one data
byte—and a literal as two downstream calls—header, then the literal slice.
It resets state only after those writes succeed, matching qpdf's exception
ordering. Internal state/length inconsistencies and impossible run lengths
are `PipelineError::Logic` with qpdf's messages.

`finish` flushes the pending packet, writes the `128` EOD byte, then calls
downstream `finish`. If any earlier downstream operation fails, later
operations are not attempted. The exact chunk boundaries are part of the
component contract and are tested.

## StreamFilter adapters

Add explicit adapters in `stream_filter.rs`:

- `Ascii85StreamFilter`;
- `AsciiHexStreamFilter`;
- `RunLengthStreamFilter`.

Each adapter constructs its matching decoder around `OutputBuffer`, writes
the supplied input slice, calls `finish`, and returns the collected bytes.
The public whole-buffer behavior is thus retained at the API boundary while
the codec itself uses the same incremental contract as qpdf.

All three adapters inherit the default `StreamFilter::set_decode_params`.
Only absent or null parameters are accepted. `filters.rs` already asks
`set_decode_params` before predictor extraction or `pipe_decode`; this
ordering must remain, so rejected parameters produce no codec writes and no
output.

`RunLengthStreamFilter::is_specialized_compression` returns `true`.
ASCII85 and ASCIIHex retain the default `false`. None is lossy.

`stream_filter_for` registers canonical names after existing abbreviation
normalization:

- `ASCII85Decode`;
- `ASCIIHexDecode`;
- `RunLengthDecode`.

The adapters use the existing `OutputBuffer`, so
`decode_stream_data_with_limits` continues to enforce its output cap at
downstream write boundaries without adding codec-specific allocation limits.

## Production cutover and deletion

In `filters.rs`, remove the direct ASCII85, ASCIIHex, and RunLength branches
from `apply_single_filter_decode`. These names must be handled only by
`stream_filter_for`.

Replace the RunLength branch in `apply_single_filter_encode` with a small
Pipeline collector path built around `RunLength::new(..., Encode)`. It writes
the input once, finishes the stage, and extracts the collector buffer.

The existing `ascii85.rs` and `ascii_hex.rs` modules retain only their
flpdf-specific encoder helpers and encoder tests. Their module and function
documentation must state that qpdf has no matching encoder component and must
not claim decoder correspondence after decoder removal.

Delete `crates/flpdf/src/run_length.rs` after its encoder and decoder
consumers and tests have moved. Do not retain compatibility wrappers or a
second packetization implementation.

The final source inventory must prove:

- no `ascii85::decode`, `ascii_hex::decode`, or `run_length::{decode, encode}`
  production call remains;
- no old decoder definitions remain;
- no `mod run_length` outside `pipeline` remains;
- ASCII85 and ASCIIHex encoder helpers remain used only on write paths;
- every supported decode name reaches a registered `StreamFilter`.

## Data flow

Production decoding remains filter-by-filter so existing chain order and
Predictor ownership are unchanged:

```text
PDF stream bytes
  -> decode_filter_specs
  -> normalize filter name
  -> stream_filter_for
  -> set_decode_params
  -> codec Pipeline stage
  -> OutputBuffer with existing max-output enforcement
  -> existing apply_decode_params
  -> next PDF filter
```

Production RunLength encoding becomes:

```text
unencoded stream bytes
  -> RunLength(Encode)
  -> Buffer
  -> encoded bytes including 0x80 EOD
```

ASCII85 and ASCIIHex encoding continues through the existing one-shot encoder
helpers because there is no qpdf Pipeline oracle for those directions.

## Tests

### Component unit tests

Always-on Rust tests cover every state transition and meaningful failure
boundary.

ASCII85 cases include:

- complete groups, partial groups of lengths one through four, and `z`;
- all six accepted whitespace bytes in normal and post-`~` states;
- NUL, out-of-range bytes, misplaced `z`, and broken EOD;
- `uuuuu` low-32-bit behavior;
- EOD and non-EOD chunk partitions, including a split `~>`;
- bare `~` finish, data after completed EOD, repeated finish, and reuse;
- downstream failures on full, partial, and zero-length-equivalent flushes,
  proving reset-before-write ordering and finish suppression after error.

ASCIIHex cases include:

- upper- and lowercase digits, even and odd nibble counts;
- all six accepted whitespace bytes and NUL rejection;
- `>` in each nibble state and ignored trailing data;
- every input split around a complete byte;
- output retained before a later invalid character;
- downstream write failures, reset-before-write ordering, repeated finish,
  and reuse.

RunLength decode cases include:

- literal and repeat packet lengths at minimum, typical, and maximum values;
- packet headers and payloads split across writes;
- truncated literal and repeat packets at finish;
- `0x80` followed by later packets in the same and later writes;
- repeated finish and writes after finish;
- downstream failure on each byte of literal and repeat output, proving exact
  state and call ordering.

RunLength encode cases include:

- empty, one-byte, two-equal-byte, two-distinct-byte, and mixed inputs;
- literal and run boundaries at 127, 128, and 129 bytes;
- the copying-to-run transition around every boundary;
- equivalence across all deterministic input chunk partitions;
- exact downstream header/data/EOD write boundaries;
- downstream failures during header, payload, EOD, and finish.

### StreamFilter and public-path tests

Tests in `stream_filter.rs` and `filters.rs` cover:

- canonical and abbreviated filter registration;
- null parameter acceptance and non-null parameter rejection before writes;
- RunLength specialized-compression classification;
- output-limit enforcement for all three decoders;
- public decode and encode behavior;
- mixed filter chains that include Flate and more than one new adapter;
- preservation of existing Predictor ownership and error timing.

Tests that encoded the former helper behavior are rewritten as qpdf parity
tests. No assertion remains solely to preserve a known divergence.

### Live qpdf 11.9.0 differential

Add `tests/oracle/qpdf_stream_codecs_probe.cc`. It directly constructs
`Pl_ASCII85Decoder`, `Pl_ASCIIHexDecoder`, and `Pl_RunLength` from the pinned
qpdf source and uses an instrumented downstream Pipeline. Deterministic cases
report:

- output bytes;
- success or qpdf exception category and exact message;
- individual downstream write chunks;
- downstream finish count;
- observable state after injected downstream failures followed by a defined
  continuation.

Add `scripts/qpdf-stream-codecs-diff.sh`. The script:

1. resolves the oracle through `scripts/fetch-qpdf-source.sh --print-path`;
2. verifies the source is clean and pinned to `3b97c9bd`;
3. compiles the probe and required qpdf sources in a private `mktemp`
   directory;
4. supplies the probe path to an ignored Rust differential test;
5. runs deterministic normal, malformed, chunked, finish/reuse, boundary, and
   fault-injection cases in both implementations;
6. compares bytes, error category and text, downstream chunks, and finish
   counts exactly;
7. verifies the oracle source remains clean and removes the temporary
   directory on exit.

The script fails closed if the pinned tree, compiler, headers, link inputs, or
runtime loading are unavailable. The ignored Rust test never silently passes
without executing the external oracle. The script is an explicit completion
gate even though it is not part of the ordinary offline test suite.

## Documentation

Update `docs/qpdf-correspondence.md` so the three new Pipeline modules point to
their qpdf 11.9.0 source components and the three StreamFilter adapters point
to their `SF_*` wrappers. Remove correspondence claims attached to deleted
whole-buffer decoders.

Regenerate `docs/qpdf-module-doc-index.md` through
`scripts/qpdf-module-docs.py`. The generated index must identify the new
modules without marking Predictor or unrelated Phase 2 components complete.

## Completion gates

Run the following against the final implementation from a clean test state:

1. focused component, StreamFilter, filters, reader, writer, and CLI tests;
2. `scripts/qpdf-stream-codecs-diff.sh`;
3. `cargo fmt -- --check`;
4. `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
5. `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`;
6. qpdf correspondence/module-documentation checks;
7. byte-oriented regression checks used by the repository;
8. `cargo test --workspace`;
9. a fresh changed-executable-line coverage run with 100% patch coverage.

Before completion, repeat the deletion inventory with `rg` and inspect the
final diff for accidental compatibility wrappers or unrelated changes.

The issue is complete only when production decoding for all three filters and
production RunLength encoding use the new Pipeline components, the old paths
are absent, the live qpdf oracle agrees, and every quality gate above passes.
