# qpdf LZW and PNG Predictor Pipeline Cutover Design

**Issue:** `flpdf-qynx.5.3`<br>
**Date:** 2026-07-28<br>
**Oracle:** qpdf 11.9.0 (`v11.9.0`, commit `3b97c9bd`)<br>
**Oracle source:** `scripts/fetch-qpdf-source.sh --print-path`

## Problem

flpdf decodes LZW streams and PNG-predicted streams, and encodes PNG-predicted
streams, through whole-buffer helpers written from the PDF specification rather
than from qpdf's incremental components:

- `filters.rs::lzw_decode` is a self-contained LZW decoder;
- `filters.rs::decode_png_predictor` / `encode_png_predictor` /
  `png_filter_byte` are whole-buffer PNG predictor helpers;
- `writer/serialize.rs::png_up_predict` is a second, independent PNG "Up"
  encoder used by the cross-reference stream writer;
- `filters.rs::extract_predictor_params` parses `/DecodeParms` outside the
  `StreamFilter` boundary, and `LZWDecode` never reaches `stream_filter_for`
  at all.

qpdf 11.9.0 has one component for each responsibility, and one stream filter
that owns both:

- `libqpdf/Pl_LZWDecoder.cc` — incremental LZW decoder;
- `libqpdf/Pl_PNGFilter.cc` — incremental PNG predictor, both directions;
- `libqpdf/SF_FlateLzwDecode.cc` — the `QPDFStreamFilter` that parses
  `/Predictor`, `/Columns`, `/Colors`, `/BitsPerComponent`, and `/EarlyChange`,
  and then chains `Pl_LZWDecoder` or `Pl_Flate` in front of `Pl_PNGFilter`.

The split produces material behavior differences from the oracle. Observed
qpdf 11.9.0 behavior that flpdf does not currently reproduce:

- `Pl_PNGFilter` decode **ignores** an unknown row filter byte (`> 4`) and
  passes the row through; flpdf raises `unsupported PNG predictor filter`.
- `Pl_PNGFilter::finish` emits a **zero-padded full row** for a truncated final
  row; flpdf raises `corrupt PNG predictor stream` and decodes nothing.
- `Pl_PNGFilter` encode hard-codes the **Up** filter and does not receive the
  predictor number at all, so `/Predictor 11` encodes as Up rows in qpdf;
  flpdf implements per-predictor filters and a libpng minimum-sum heuristic
  for `/Predictor 15`.
- `Pl_PNGFilter`'s constructor rejects `samples_per_pixel < 1`, rejects
  `bits_per_sample` outside `{1, 2, 4, 8, 16}`, and rejects a zero computed
  row width; flpdf accepts any `/BitsPerComponent`.
- `Pl_LZWDecoder` **throws** `LZWDecoder: table full` when the table would
  reach 4096 entries; flpdf silently stops adding entries and keeps decoding.
- `Pl_LZWDecoder` produces seven distinct diagnostic strings that flpdf does
  not use.
- `SF_FlateLzwDecode::setDecodeParms` accepts `/Predictor 1`, `2`, and
  `10..15`, treats every other key as ignorable, requires a non-zero
  `/Columns` only when `/Predictor > 1`, and honors `/EarlyChange` **only for
  LZW** and **only when the value is exactly `0` or `1`**; flpdf reads
  `/EarlyChange` as `value != 0` on any filter and never validates it.
- `SF_FlateLzwDecode` defers negative `/Columns`, `/Colors`, and
  `/BitsPerComponent` to `QIntC::to_uint` at pipeline-construction time rather
  than rejecting them while reading parameters.

The issue is therefore a production cutover, not an additive implementation.
When it is complete, no whole-buffer LZW decoder and no duplicate PNG predictor
implementation remains on any production route.

## Goals

1. Mirror the qpdf 11.9.0 `Pl_LZWDecoder` and `Pl_PNGFilter` state machines as
   crate-private `Pipeline` stages.
2. Replace `FlateStreamFilter` with a single `SF_FlateLzwDecode`-shaped adapter
   that owns decode parameters for both `FlateDecode` and `LZWDecode` and
   constructs the qpdf pipeline chain.
3. Route production LZW decoding and production PNG-predictor decoding through
   that adapter.
4. Route production PNG-predictor encoding — both `encode_stream_data` and the
   cross-reference stream writer — through the same component in encode mode.
5. Preserve exact qpdf byte output, chunk boundaries, finish behavior, and
   error text for the supported component boundary.
6. Differentially verify both components against the pinned qpdf 11.9.0 source
   **before** hardening the Rust implementation.
7. Delete the four superseded helpers and prove their absence by repository
   search.

## Non-goals

- **TIFF Predictor 2.** `Pl_TIFFPredictor` is a separate component and a
  separate issue. `/Predictor 2` remains an explicit declared deviation
  (see "Declared deviations").
- Adding an LZW **encoder**. qpdf has none; flpdf writes Flate only.
- Introducing qpdf's "not filterable means pass the stream through encoded"
  semantics. flpdf's boundary maps a rejected `set_decode_params` to
  `Error::Unsupported`, and that pre-existing mapping is shared with the
  `flpdf-qynx.5.2` adapters. It is unchanged here.
- Changing the public signatures of `decode_stream_data`,
  `decode_stream_data_with_limits`, or `encode_stream_data`.
- Migrating the xref-stream **Flate** producer. That is `flpdf-qynx.5.4`.
- Changing ASCII85, ASCIIHex, RunLength, Crypt, or passthrough behavior.

## Component layout

Add two modules under `crates/flpdf/src/pipeline/`:

```text
pipeline/
├── lzw.rs         → libqpdf/Pl_LZWDecoder.cc
└── png_filter.rs  → libqpdf/Pl_PNGFilter.cc
```

`pipeline.rs` declares both. Each type borrows its downstream `Pipeline`,
retains only qpdf-equivalent state, implements `identifier`, `write`, and
`finish`, and maps qpdf's `runtime_error` / `logic_error` categories onto
`PipelineError::Runtime` / `PipelineError::Logic`.

```rust
pub(crate) struct LzwDecoder<'a> { /* qpdf state */ }

pub(crate) enum PngFilterAction {
    Decode,
    Encode,
}

pub(crate) struct PngFilter<'a> { /* action and qpdf state */ }
```

`PngFilter::new` is fallible, mirroring the four constructor throw sites.

## LZW decoder semantics

`LzwDecoder` mirrors qpdf's three-byte rotating input buffer (`buf`, `next`,
`byte_pos`, `bit_pos`, `bits_available`), its `code_size`, its
`code_change_delta` flag, its `eod` flag, its `last_code`, and its table of
owned byte strings.

At construction `code_size` is `9`, `last_code` is `256`, `eod` is false, the
buffer is zeroed, and the table is empty.

### Bit extraction

`write` processes one input byte at a time. Each byte is stored at `buf[next]`,
`next` advances modulo 3, `bits_available` increases by 8, and **at most one
code is emitted per input byte** — qpdf calls `sendNextCode` once when
`bits_available >= code_size`. Leftover bits accumulate in the rotating buffer.
This per-byte cadence is part of the contract and is reproduced exactly rather
than replaced by a wide bit accumulator.

`sendNextCode` reproduces qpdf's high/med/low mask arithmetic verbatim,
including the `bit_pos == 8` normalization, and then calls `handle_code`.

### Code handling

`handle_code` returns immediately once `eod` is set. `eod` is never cleared, so
after code `257` all later input is consumed and discarded while `finish` still
reaches downstream.

- Code `256` clears the table and resets `code_size` to `9`.
- Code `257` sets `eod`.
- Otherwise, when `last_code != 256`, a table entry is appended:
  - `code < 256` contributes `code` as the next byte;
  - `code > 257` with `idx > table.len()` raises
    `LZWDecoder: bad code received`;
  - `idx == table.len()` is the self-referential case and takes the first
    character of `last_code`;
  - `new_idx == 4096` raises `LZWDecoder: table full`;
  - the code width increases when `new_idx + code_change_delta` equals `511`,
    `1023`, or `2047`.
- Output is then written downstream: a single byte for `code < 256`, otherwise
  the whole table entry in one downstream `write`.
- `last_code` is assigned last, on every path that reaches the end.

`getFirstChar` and `addToTable` reproduce their own overflow and
invalid-code diagnostics.

The seven diagnostic strings adopted verbatim are:

```text
LZWDecoder: bad code received
LZWDecoder: table full
Pl_LZWDecoder::getFirstChar: table overflow
Pl_LZWDecoder::getFirstChar called with invalid code (N)
Pl_LZWDecoder::addToTable: table overflow
Pl_LZWDecoder::addToTable called with invalid code (N)
Pl_LZWDecoder::handleCode: table overflow
```

### Finish

`finish` calls downstream `finish` and **resets nothing**. Trailing bits are
discarded, no implicit EOD is synthesized, a truncated final code produces no
output and no error, and a second `finish` reaches downstream `finish` again.
A write after `finish` continues from the retained state.

## PNG filter semantics

`PngFilter` mirrors qpdf's two row buffers of `bytes_per_row + 1`, the
`cur_row` / `prev_row` pointers, `pos`, and `incoming`.

### Construction

In declaration order:

1. `samples_per_pixel < 1` raises
   `PNGFilter created with invalid samples_per_pixel`;
2. `bits_per_sample` outside `{1, 2, 4, 8, 16}` raises
   `PNGFilter created with invalid bits_per_sample not 1, 2, 4, 8, or 16`;
3. `bytes_per_pixel = ((bits_per_sample * samples_per_pixel) + 7) / 8`;
4. `bytes_per_row` is computed with **32-bit wrapping arithmetic**, because
   qpdf evaluates `((columns * bits_per_sample * samples_per_pixel) + 7) / 8`
   in `unsigned int` before widening the result to `unsigned long long`;
5. a zero row width raises `PNGFilter created with invalid columns value`.

Consequently the `bpr > UINT_MAX - 1` guard in qpdf is unreachable, and a
`/Columns` large enough to wrap the 32-bit product reaches the zero-width
error instead. The oracle probe pins this rather than the source reading.

`incoming` is `bytes_per_row` for encode and `bytes_per_row + 1` for decode.
Both buffers start zeroed; `cur_row` is `buf1` and `prev_row` is `buf2`, so the
**first row is filtered against a zeroed previous row** rather than passed
through.

### Row accumulation

`write` fills `cur_row` from `pos` until `incoming` bytes are present, processes
the row, swaps the buffers, zeroes the new `cur_row`, and repeats while the
remaining input still completes a row. A trailing partial row is buffered.

The swap reproduces qpdf's `cur_row = t ? t : buf2` fallback, which only
matters after `finish` has set `prev_row` to null.

### Decode

`decodeRow` reads the filter byte at `cur_row[0]` and applies Sub, Up, Average,
or Paeth to `cur_row[1..]` **only when `prev_row` is non-null**. Filter bytes
above `4` are ignored, and the row is emitted unchanged. The row body is then
written downstream in one `write` of exactly `bytes_per_row` bytes.

Average uses `(left + up) / 2` in signed `int` arithmetic before the byte
truncation, and Paeth uses qpdf's `abs_diff` tie-breaking order.

### Encode

`encodeRow` writes the constant filter byte `2` in its own downstream `write`,
then either `bytes_per_row` **separate one-byte writes** of `cur_row[i] -
prev_row[i]` when `prev_row` is non-null, or a single `bytes_per_row` write of
the raw row when it is null. The predictor number never reaches the component.

### Finish

`finish` processes a buffered partial row — emitting a full, zero-padded row —
then sets `prev_row` to null, resets `cur_row` to `buf1`, zeroes it, resets
`pos`, and calls downstream `finish`. A subsequent write therefore starts a
fresh unfiltered first row, and a second `finish` with no buffered data calls
downstream `finish` again without emitting a row.

## StreamFilter adapter

Replace `FlateStreamFilter` with `FlateLzwStreamFilter`, mirroring
`SF_FlateLzwDecode` including its two factories:

```rust
struct FlateLzwStreamFilter {
    lzw: bool,
    predictor: i32,
    columns: i32,
    colors: i32,
    bits_per_component: i32,
    early_code_change: bool,
}
```

Defaults are the PDF defaults qpdf uses: `predictor = 1`, `columns = 1`,
`colors = 1`, `bits_per_component = 8`, `early_code_change = true`.

`stream_filter_for` registers `FlateDecode` with `lzw = false` and `LZWDecode`
with `lzw = true`, after the existing abbreviation normalization.

### set_decode_params

The adapter overrides the trait default, which rejects all non-null parameters.
Mirroring qpdf:

- a null or absent parameter object is accepted with no state change;
- a non-dictionary object yields no keys and is accepted unchanged;
- `/Predictor` must be an integer, and its value must be `1`, `2`, or
  `10..=15`; anything else is not filterable;
- `/Columns`, `/Colors`, and `/BitsPerComponent` must be integers and are
  stored without range validation, including negative values;
- `/EarlyChange` is examined **only when `lzw`**; it must be an integer, and
  `early_code_change` becomes `value == 1`; a value other than `0` or `1` is
  not filterable;
- after the key loop, `predictor > 1 && columns == 0` is not filterable.

Integer values are clamped to `i32` exactly as `getIntValueAsInt` clamps.

### Chain construction timing

`QPDF_Stream::pipeStreamData` constructs every filter's decode pipeline —
walking the filters in reverse — before it writes the first byte. flpdf runs its
chain stage by stage over whole buffers, so construction is reproduced as a
separate `preflight_decode_pipeline` pass over the prepared chain, in the same
reverse order, before any stage decodes. A later stage whose geometry cannot
form a pipeline is therefore reported even when an earlier stage would fail
first, including when the decoded-output cap stops that earlier stage.

The preflight, `pipe_decode`, and the encode path share one geometry resolver so
all three reject exactly the same parameters.

### pipe_decode

Mirroring `getDecodePipeline`, the chain is built from the sink outward:

```text
input -> LzwDecoder | Flate -> PngFilter (when 10..=15) -> OutputBuffer
```

`/Predictor 2` raises the declared-deviation error at this point, which matches
qpdf's construction-time failure position for its other invalid-parameter
cases. Negative `/Columns`, `/Colors`, or `/BitsPerComponent` raise the
`QIntC::to_uint` range error here as well, before any codec write.

`Flate` continues to receive the existing warning callback; `LzwDecoder` and
`PngFilter` have no warning channel in qpdf.

### Output-limit semantics

`OutputBuffer` remains the sink, so `decode_stream_data_with_limits` now
enforces its cap on **post-predictor** bytes rather than on raw codec output.
For a PNG-predicted stream the final output is one byte per row smaller than
the codec output, so a stream near the cap may now succeed where it previously
failed. Memory stays bounded because `PngFilter` retains only two rows and
`LzwDecoder` only its table.

This is a deliberate, documented change. `check.rs`'s cap documentation and
`filters.rs`'s limit documentation are updated accordingly.

## Production cutover and deletion

### filters.rs

- `prepare_decode_filters` no longer calls `extract_predictor_params`; the
  `predictor` field leaves `PreparedDecodeFilter` and
  `apply_prepared_decode_params` is deleted.
- `validate_legacy_decode_filter` loses its `LZWDecode` arm, and
  `apply_single_filter_decode` loses its LZW branch, leaving only the
  passthrough label and the unsupported-name error.
- `apply_encode_params` is rebuilt on `PngFilter::Encode` over a `Buffer`
  sink; the predictor parameters it needs are read through the same adapter
  state so that decode and encode agree on validation.
- Delete `lzw_decode`, `decode_png_predictor`, `encode_png_predictor`, and
  `png_filter_byte`.

### writer/serialize.rs

Reduce `png_up_predict` to a `PngFilter::Encode` call. The name is kept as a
geometry adapter — it converts the writer's `/W` widths to the component's
`(columns, colors, bits_per_sample)` arguments, asserts the row-multiple
invariant, and collapses a `Result` that cannot fail for writer-controlled
geometry — but it no longer contains a predictor implementation.

The writer's row geometry is `columns = Σ /W`, `colors = 1`,
`bits_per_component = 8`, and `build_rows` always emits an exact multiple of the
row width, so the component never reaches its partial-row path. That invariant
is asserted, and the cutover must be byte-neutral: `deterministic_id_xref_stream_tests`,
`cmp_linearize_objstm_tests`, and the `qpdf-zlib-compat` `compat_baseline_*`
byte tests must stay green with no re-blessing.

### Deletion inventory

The final source search must prove:

- no `lzw_decode`, `decode_png_predictor`, `encode_png_predictor`, or
  `png_filter_byte` definition remains, and `png_up_predict` retains no
  predictor logic of its own;
- no production call reaches a predictor helper outside `pipeline::png_filter`;
- `LZWDecode` resolves through `stream_filter_for`;
- `extract_predictor_params` exists only as adapter state.

## Declared deviations

Recorded here, in the module docs, and in `docs/qpdf-correspondence.md`:

1. **`/Predictor 2` (TIFF)** — qpdf marks it filterable and builds
   `Pl_TIFFPredictor`. flpdf raises
   `/DecodeParms /Predictor 2 is not supported for this stream type` at
   pipeline-construction time. Scope boundary of `flpdf-qynx.5.3`; the
   component belongs to a separate issue.
2. **Non-filterable mapping** — qpdf leaves a non-filterable stream encoded,
   while flpdf returns `Error::Unsupported`. Pre-existing boundary behavior,
   shared with the `flpdf-qynx.5.2` adapters, unchanged by this issue.
3. **Decoded-output cap** — flpdf enforces a caller-supplied output limit that
   qpdf does not have. Its enforcement point moves to the end of the chain.
4. **Lazy row-buffer allocation** — qpdf's `Pl_PNGFilter` constructor allocates
   both row buffers immediately. flpdf allocates them on the first byte written.
   This is a category (B) substitution: an unused stage never reads a row, so
   output bytes, downstream call boundaries, and error timing are unchanged, and
   the live differential covers empty writes in both directions. It keeps a
   stream that carries no data from allocating two buffers sized by an untrusted
   `/Columns`, which the deleted whole-buffer helpers guarded against.

## Tests

### Component unit tests

LZW cases cover: single-byte and multi-byte outputs; `EarlyChange` `1` and `0`
across each of the three width transitions; the self-referential code; an
intermediate clear code; a code immediately after a clear; table-full; every
invalid-code diagnostic; truncated trailing bits; data after EOD; reuse after
`finish`; repeated `finish`; downstream write failure at each output position.

PNG cases cover: each filter byte `0..=4` and an ignored byte above `4`; first
row against the zeroed previous row; multi-row Sub/Average/Paeth with
`bytes_per_pixel > 1`; row geometry for every legal `bits_per_sample`; every
constructor rejection; the 32-bit wrap to a zero row width; input split at
every position around a row boundary; a truncated final row at `finish`;
post-`finish` reuse in both directions; encode chunk boundaries; downstream
failures on the header write, on a body byte, and on `finish`.

### Adapter and public-path tests

`stream_filter.rs` and `filters.rs` tests cover `LZWDecode` and `Fl`/`LZW`
abbreviation registration; every `set_decode_params` acceptance and rejection
branch, including `/EarlyChange` on a Flate stream being ignored; the
`predictor > 1 && columns == 0` rule; construction-time range errors; the
`/Predictor 2` deviation; output-limit enforcement at the new boundary; mixed
filter chains; and public decode/encode round-trips.

Tests that asserted removed helper behavior — including the `filters.rs` LZW
malformed-code tests and any assertion pinning the minimum-sum encode
heuristic — are rewritten as qpdf parity tests. No assertion survives solely to
preserve a known divergence.

### Live qpdf 11.9.0 differential

Add `tests/oracle/qpdf_lzw_png_probe.cc` and `scripts/qpdf-lzw-png-diff.sh`,
modeled on the `flpdf-qynx.5.2` pair. The probe accepts a parameterized codec
selector — `lzw:EARLY` and `png-decode:COLUMNS,COLORS,BITS` /
`png-encode:COLUMNS,COLORS,BITS` — so constructor failures are themselves
comparable results. It reports output bytes, per-call downstream chunks,
finish counts, and exception category and exact message, under injected
downstream failures as well as clean runs.

The probe is written and run **first**, before the Rust components are
hardened, because the partial-row, ignored-filter-byte, table-full,
post-`finish` reuse, and 32-bit-wrap behaviors are ambiguous from source
reading alone.

The script fails closed if the pinned tree, compiler, headers, or link inputs
are unavailable, and the ignored Rust test never passes without executing the
external oracle.

## Documentation

- `docs/qpdf-correspondence.md`: add the two Pipeline modules and the
  `SF_FlateLzwDecode` adapter; remove correspondence claims from the deleted
  helpers; record the three declared deviations.
- Regenerate `docs/qpdf-module-doc-index.md` via `scripts/qpdf-module-docs.py`.
- `filters.rs` and `check.rs` rustdoc: state the new limit boundary and the
  `/Predictor 2` restriction in current-tense, externally verifiable terms.

## Completion gates

1. focused component, adapter, filters, writer, reader, and CLI tests;
2. `scripts/qpdf-lzw-png-diff.sh`;
3. `cargo fmt -- --check`;
4. `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
5. `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`;
6. qpdf correspondence and module-documentation checks;
7. `qpdf-zlib-compat` byte-identical regression tests, unblessed;
8. `cargo test --workspace`;
9. a fresh changed-executable-line coverage run at 100% patch coverage.

Any new `qpdf-zlib-compat`-gated byte test must be added to `ci.yml` by hand,
because gated tests are enumerated explicitly there.

The issue is complete only when production LZW decoding, PNG-predictor
decoding, and both PNG-predictor encoders use the new components, the four old
helpers are absent, the live oracle agrees, and every gate above passes.
