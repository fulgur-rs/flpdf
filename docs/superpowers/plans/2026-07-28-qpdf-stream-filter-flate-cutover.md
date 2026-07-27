# QPDFStreamFilter Driver and PlFlate Cutover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Route every `filters.rs` Flate decode and encode through the existing streaming `pipeline::flate::Flate`, while adding the qpdf-shaped `/Filter` and `/DecodeParms` driver boundary needed by later codec cutovers.

**Architecture:** A new crate-private `stream_filter.rs` owns filter-name normalization, qpdf-compatible `/DecodeParms` alignment, and the production PlFlate adapter. `filters.rs` retains the public whole-buffer API and the not-yet-migrated LZW/ASCII/RunLength/predictor implementations, but delegates all filter-chain interpretation to the new driver and has no direct `flate2` route.

**Tech Stack:** Rust 2021; existing `Pipeline`, `Buffer`, and `Flate` stages; qpdf 11.9.0 pinned source and `/usr/bin/qpdf` oracle; Cargo tests, Clippy, strict rustdoc, `cargo llvm-cov`, and `scripts/patch-coverage.sh`.

## Global Constraints

- Resolve the read-only oracle with `scripts/fetch-qpdf-source.sh --print-path`; do not edit or re-clone it.
- Preserve the existing public `decode_stream_data`, `decode_stream_data_with_limits`, `encode_stream_data`, `DecodeLimits`, and error-category contracts.
- `QPDFStreamFilter` is a crate-private responsibility boundary. Do not expose new public API.
- Preserve the intentional maximum decode-chain length of 16 even though qpdf has no equivalent cap.
- Preserve the current consumer-wait decisions: LZW, PNG predictor, ASCII85, ASCIIHex, and RunLength remain one-shot until `flpdf-qynx.5.2`/`.5.3`; DCT and TIFF Predictor 2 remain unsupported/passthrough as already documented.
- The new driver must align scalar, array, empty-array, and absent `/DecodeParms` the way qpdf 11.9.0 `QPDF_Stream::filterable` does.
- Flate warning callbacks, chunking, finish behavior, malformed-input timing, and compression bytes come from `pipeline::flate::Flate`; do not introduce a second zlib state machine.
- The per-Flate-stage `DecodeLimits::max_output` check must abort while output is streamed, before an unbounded allocation, and retain the existing `Error::Unsupported` sentinel.
- `filters.rs` must contain no production `flate2`, `ZlibDecoder`, or `ZlibEncoder` reference at completion.
- Every behavior change follows RED→GREEN→REFACTOR. Each test must name the production mutation it catches and use literal or independently generated expectations.
- Every changed executable line must have fresh 100% patch coverage against `origin/main`.

## Delivery Boundary

This is one PR from `feature/flpdf-qynx-5-1-stream-filter` to `main`. The work does not modify the concurrent RC4, tokenizer, JSON, optimization, or writer-only output-pipeline slices.

---

### Task 1: Add the QPDFStreamFilter filter/DecodeParms driver

**Files:**
- Create: `crates/flpdf/src/stream_filter.rs`
- Modify: `crates/flpdf/src/lib.rs`
- Test: `crates/flpdf/src/stream_filter.rs`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/include/qpdf/QPDFStreamFilter.hh`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/QPDFStreamFilter.cc`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/QPDF_Stream.cc:380-480`

**Interfaces:**
- Produces `pub(crate) struct FilterSpec<'a> { name: &'a [u8], decode_params: Option<&'a Object> }`.
- Produces `FilterSpec::normalized_name(&self) -> &[u8]`, expanding qpdf abbreviations without allocating.
- Produces `pub(crate) fn decode_filter_specs<'a>(filter: Option<&'a Object>, decode_params: Option<&'a Object>) -> Result<Vec<FilterSpec<'a>>>`.
- Consumes the existing `Object`, `Error`, and `Result` types only; it does not know codec implementations.

- [x] **Step 1: Write failing driver tests**

Add unit tests that call the wished-for `decode_filter_specs` API and assert:

```rust
#[test]
fn scalar_decode_parms_are_reused_for_each_filter() {
    let filter = Object::Array(vec![
        Object::Name(b"FlateDecode".to_vec()),
        Object::Name(b"ASCII85Decode".to_vec()),
    ]);
    let params = Object::Dictionary(Dictionary::new());
    let specs = decode_filter_specs(Some(&filter), Some(&params)).unwrap();
    assert_eq!(specs.len(), 2);
    assert!(std::ptr::eq(specs[0].decode_params.unwrap(), &params));
    assert!(std::ptr::eq(specs[1].decode_params.unwrap(), &params));
}

#[test]
fn decode_parms_array_must_align_with_filter_array() {
    let filter = Object::Array(vec![
        Object::Name(b"FlateDecode".to_vec()),
        Object::Name(b"ASCII85Decode".to_vec()),
    ]);
    let params = Object::Array(vec![Object::Null]);
    let error = decode_filter_specs(Some(&filter), Some(&params)).unwrap_err();
    assert!(matches!(error, Error::Unsupported(_)));
    assert_eq!(
        error.to_string(),
        "unsupported PDF feature: stream /DecodeParms length is inconsistent with filters"
    );
}

#[test]
fn empty_decode_parms_array_is_null_and_filter_abbreviations_expand() {
    let filter = Object::Name(b"Fl".to_vec());
    let params = Object::Array(Vec::new());
    let specs = decode_filter_specs(Some(&filter), Some(&params)).unwrap();
    assert_eq!(specs[0].normalized_name(), b"FlateDecode");
    assert!(specs[0].decode_params.is_none());
}
```

Also cover no filter ignoring malformed `/DecodeParms`, a non-name filter item, name form with a one-element params array, and all standard abbreviations already recognized by qpdf (`/Fl`, `/LZW`, `/A85`, `/AHx`, `/RL`, `/CCF`, `/DCT`).

- [x] **Step 2: Run the new tests and verify RED**

Run:

```bash
cargo test -p flpdf stream_filter::tests -- --nocapture
```

Expected: compilation failure because `stream_filter` and `decode_filter_specs` do not exist.

- [x] **Step 3: Implement the minimal driver**

Create `stream_filter.rs` with this structure:

```rust
//! qpdf correspondence: QPDFStreamFilter.cc and QPDF_Stream.cc filter-name,
//! DecodeParms-alignment, and decode-pipeline construction responsibilities.

use crate::{Error, Object, Result};

#[derive(Clone, Copy, Debug)]
pub(crate) struct FilterSpec<'a> {
    pub(crate) name: &'a [u8],
    pub(crate) decode_params: Option<&'a Object>,
}

impl FilterSpec<'_> {
    pub(crate) fn normalized_name(&self) -> &[u8] {
        match self.name {
            b"Fl" => b"FlateDecode",
            b"LZW" => b"LZWDecode",
            b"A85" => b"ASCII85Decode",
            b"AHx" => b"ASCIIHexDecode",
            b"RL" => b"RunLengthDecode",
            b"CCF" => b"CCITTFaxDecode",
            b"DCT" => b"DCTDecode",
            name => name,
        }
    }
}

pub(crate) fn decode_filter_specs<'a>(
    filter: Option<&'a Object>,
    decode_params: Option<&'a Object>,
) -> Result<Vec<FilterSpec<'a>>> {
    let names: Vec<&[u8]> = match filter {
        None | Some(Object::Null) => return Ok(Vec::new()),
        Some(Object::Name(name)) => vec![name],
        Some(Object::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_name().ok_or_else(|| {
                    Error::Unsupported(
                        "stream filter type is not name or array".to_string(),
                    )
                })
            })
            .collect::<Result<_>>()?,
        Some(_) => {
            return Err(Error::Unsupported(
                "stream filter type is not name or array".to_string(),
            ))
        }
    };

    if names.is_empty() {
        return Ok(Vec::new());
    }

    let params = match decode_params {
        None | Some(Object::Null) => vec![None; names.len()],
        Some(Object::Array(items)) if items.is_empty() => vec![None; names.len()],
        Some(Object::Array(items)) => {
            if items.len() != names.len() {
                return Err(Error::Unsupported(
                    "stream /DecodeParms length is inconsistent with filters".to_string(),
                ));
            }
            items
                .iter()
                .map(|item| (!matches!(item, Object::Null)).then_some(item))
                .collect()
        }
        Some(item) => vec![Some(item); names.len()],
    };

    Ok(names
        .into_iter()
        .zip(params)
        .map(|(name, decode_params)| FilterSpec {
            name,
            decode_params,
        })
        .collect())
}
```

Register `pub(crate) mod stream_filter;` in `lib.rs`.

- [x] **Step 4: Run focused tests and verify GREEN**

Run:

```bash
cargo test -p flpdf stream_filter::tests -- --nocapture
```

Expected: all driver tests pass.

- [x] **Step 5: Refactor and re-run**

Remove only duplication revealed by the tests. Keep parser and codec execution separate. Run the focused tests again.

- [x] **Step 6: Commit the driver**

```bash
git add crates/flpdf/src/lib.rs crates/flpdf/src/stream_filter.rs
git commit -m "feat: add qpdf stream filter driver"
```

---

### Task 2: Add production PlFlate decode/encode adapters

**Files:**
- Modify: `crates/flpdf/src/stream_filter.rs`
- Test: `crates/flpdf/src/stream_filter.rs`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/Pl_Flate.cc`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/SF_FlateLzwDecode.cc`

**Interfaces:**
- Consumes `pipeline::flate::{Flate, FlateAction, DEFAULT_OUT_BUFFER_SIZE}` and `Pipeline`.
- Produces `pub(crate) fn decode_flate(...) -> Result<Vec<u8>>`.
- Produces `pub(crate) fn encode_flate(data: &[u8]) -> Result<Vec<u8>>`.
- Keeps an internal chunked/warning-aware helper so tests can prove qpdf lifecycle behavior while production passes one complete slice.

- [x] **Step 1: Write failing adapter tests**

Add tests for these observable behaviors:

```rust
#[test]
fn flate_decode_is_invariant_across_input_chunks() {
    let encoded = encode_flate(b"chunk boundaries must not matter").unwrap();
    let whole = decode_flate_chunks(
        [&encoded[..]],
        None,
        &mut |_, _| Ok(()),
    )
    .unwrap();
    let split = decode_flate_chunks(
        encoded.chunks(1),
        None,
        &mut |_, _| Ok(()),
    )
    .unwrap();
    assert_eq!(whole, b"chunk boundaries must not matter");
    assert_eq!(split, whole);
}

#[test]
fn flate_limit_rejects_one_byte_over_but_accepts_exact_boundary() {
    let encoded = encode_flate(&vec![b'A'; 2_000]).unwrap();
    let error = decode_flate(&encoded, Some(1_999)).unwrap_err();
    assert_eq!(
        error.to_string(),
        "decoded output exceeds configured limit of 1999 bytes"
    );
    assert_eq!(decode_flate(&encoded, Some(2_000)).unwrap().len(), 2_000);
}

#[test]
fn incomplete_input_reports_qpdf_warning_before_downstream_finish() {
    let warnings = RefCell::new(Vec::new());
    let decoded = decode_flate_chunks(
        [b"\x78".as_slice()],
        None,
        &mut |message, code| {
            warnings.borrow_mut().push((message.to_string(), code));
            Ok(())
        },
    )
    .unwrap();
    assert!(decoded.is_empty());
    assert_eq!(
        warnings.into_inner(),
        vec![(
            "input stream is complete but output may still be valid".to_string(),
            -5,
        )]
    );
}
```

Also assert:

- empty raw input emits no wrapper because qpdf's `Pl_Flate` does not initialize
  zlib before the first non-empty write;
- malformed zlib header returns the exact `stream inflate: inflate: data: incorrect header check` message;
- an output-limit failure is `Error::Unsupported`, not `Error::System`;

The mutation each test catches is respectively: buffering the entire input instead of forwarding chunks, checking the limit after allocation, dropping the warning callback, changing the qpdf error identifier, or bypassing the existing `Flate` stage.

- [x] **Step 2: Run adapter tests and verify RED**

Run:

```bash
cargo test -p flpdf stream_filter::tests::flate -- --nocapture
```

Expected: compilation failure because the adapter functions do not exist.

- [x] **Step 3: Implement a streaming limited sink**

Add an internal sink:

```rust
struct OutputBuffer {
    data: Vec<u8>,
    max_output: Option<usize>,
}

impl OutputBuffer {
    fn new(max_output: Option<usize>) -> Self {
        Self {
            data: Vec::new(),
            max_output,
        }
    }
}

impl Pipeline for OutputBuffer {
    fn identifier(&self) -> &str {
        "stream data buffer"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        let next_len = self.data.len().checked_add(data.len()).ok_or_else(|| {
            PipelineError::runtime("decoded output length overflow")
        })?;
        if self.max_output.is_some_and(|limit| next_len > limit) {
            return Err(PipelineError::runtime(format!(
                "{DECODE_OUTPUT_LIMIT_PREFIX} {} bytes",
                self.max_output.expect("checked Some above")
            )));
        }
        self.data.extend_from_slice(data);
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}
```

Use a local constant with the same literal as `filters::DECODE_OUTPUT_LIMIT_PREFIX`; Task 3 will move the shared constant to `stream_filter.rs` when deleting the old Flate route.

- [x] **Step 4: Implement decode and encode through PlFlate**

Use the real stages:

```rust
fn map_pipeline_error(error: PipelineError) -> Error {
    Error::Unsupported(error.to_string())
}

fn decode_flate_chunks<'a>(
    chunks: impl IntoIterator<Item = &'a [u8]>,
    max_output: Option<usize>,
    warn: &mut dyn FnMut(&str, i32) -> PipelineResult<()>,
) -> Result<Vec<u8>> {
    let mut sink = OutputBuffer::new(max_output);
    {
        let mut flate = Flate::new(
            "stream inflate",
            &mut sink,
            FlateAction::Inflate,
            DEFAULT_OUT_BUFFER_SIZE,
        )
        .map_err(map_pipeline_error)?;
        flate.set_warn_callback(|message, code| warn(message, code));
        for chunk in chunks {
            flate.write(chunk).map_err(map_pipeline_error)?;
        }
        flate.finish().map_err(map_pipeline_error)?;
    }
    Ok(sink.data)
}

pub(crate) fn decode_flate(data: &[u8], max_output: Option<usize>) -> Result<Vec<u8>> {
    decode_flate_chunks([data], max_output, &mut |_, _| Ok(()))
}

pub(crate) fn encode_flate(data: &[u8]) -> Result<Vec<u8>> {
    let mut sink = Buffer::new("stream data buffer", None);
    {
        let mut flate = Flate::new(
            "compress stream",
            &mut sink,
            FlateAction::Deflate,
            DEFAULT_OUT_BUFFER_SIZE,
        )
        .map_err(map_pipeline_error)?;
        flate.write(data).map_err(map_pipeline_error)?;
        flate.finish().map_err(map_pipeline_error)?;
    }
    sink.take_buffer().map_err(map_pipeline_error)
}
```

If the borrow checker rejects the closure lifetime, make `decode_flate_chunks` generic over the warning callback rather than adding shared ownership.

- [x] **Step 5: Run focused tests and verify GREEN**

Run:

```bash
cargo test -p flpdf stream_filter::tests -- --nocapture
cargo test -p flpdf pipeline::flate::tests -- --nocapture
```

Expected: all driver, adapter, and existing PlFlate lifecycle tests pass.

- [x] **Step 6: Commit the adapter**

```bash
git add crates/flpdf/src/stream_filter.rs
git commit -m "feat: run stream flate through pipeline"
```

---

### Task 3: Cut the public filter APIs over and delete direct flate2 routes

**Files:**
- Modify: `crates/flpdf/src/filters.rs`
- Modify: `crates/flpdf/src/stream_filter.rs`
- Test: `crates/flpdf/src/filters.rs`
- Test: `crates/flpdf/tests/multi_filter_chain_tests.rs`
- Test: `crates/flpdf/tests/compress_streams_tests.rs`

**Interfaces:**
- Consumes `decode_filter_specs`, `decode_flate`, and `encode_flate`.
- Preserves every existing public filter function signature.
- Leaves `apply_single_filter_decode` and `apply_single_filter_encode` only for codecs owned by later Beads; neither function contains a Flate branch.

- [x] **Step 1: Add failing public-boundary tests**

Add tests through the public `decode_stream_data`/`encode_stream_data` APIs:

```rust
#[test]
fn decode_stream_data_accepts_qpdf_flate_abbreviation() {
    let mut full = Dictionary::new();
    full.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
    let encoded = encode_stream_data(&full, b"abbreviated filter").unwrap();

    let mut abbreviated = Dictionary::new();
    abbreviated.insert("Filter", Object::Name(b"Fl".to_vec()));
    assert_eq!(
        decode_stream_data(&abbreviated, &encoded).unwrap(),
        b"abbreviated filter"
    );
}

#[test]
fn decode_stream_data_rejects_misaligned_decode_parms_before_codec_runs() {
    let mut dict = Dictionary::new();
    dict.insert(
        "Filter",
        Object::Array(vec![
            Object::Name(b"FlateDecode".to_vec()),
            Object::Name(b"ASCII85Decode".to_vec()),
        ]),
    );
    dict.insert("DecodeParms", Object::Array(vec![Object::Null]));
    let error = decode_stream_data(&dict, b"not zlib").unwrap_err();
    assert_eq!(
        error.to_string(),
        "unsupported PDF feature: stream /DecodeParms length is inconsistent with filters"
    );
}
```

Also exercise a Flate+predictor round trip, Flate in a multi-filter chain, a scalar params object repeated across a chain, exact output-limit behavior, empty input, and malformed-header timing. These tests must call public APIs, not the internal adapter.

- [x] **Step 2: Run public-boundary tests and verify RED**

Run:

```bash
cargo test -p flpdf filters::tests::decode_stream_data_accepts_qpdf_flate_abbreviation -- --nocapture
cargo test -p flpdf filters::tests::decode_stream_data_rejects_misaligned_decode_parms_before_codec_runs -- --nocapture
```

Expected: the abbreviation test fails as unsupported and the mismatch test reaches the codec instead of returning the alignment diagnostic.

- [x] **Step 3: Unify decode-chain dispatch**

Replace the separate name/array branches in `decode_stream_data_with_filters_and_crypt` with:

```rust
let specs = decode_filter_specs(filter, decode_params)?;
if specs.len() > MAX_FILTER_CHAIN_LEN {
    return Err(Error::Unsupported(format!(
        "filter chain length {} exceeds maximum of {MAX_FILTER_CHAIN_LEN}",
        specs.len()
    )));
}
let mut decoded = stream_data.to_vec();
for spec in specs {
    let name = spec.normalized_name();
    if name == b"Crypt" {
        decoded = decrypt_crypt(spec.decode_params, &decoded)?;
        continue;
    }
    decoded = if name == b"FlateDecode" {
        decode_flate(&decoded, limits.max_output)?
    } else {
        apply_single_filter_decode(name, &decoded, spec.decode_params, limits.max_output)
            .map_err(Error::Unsupported)?
    };
    decoded = apply_decode_params(spec.decode_params, &decoded)?;
}
Ok(decoded)
```

Keep the 16-stage cap based on `specs.len()`. Remove `get_decode_params` only after `rg` proves it has no caller.

- [x] **Step 4: Unify encode-chain dispatch**

Build the same specs and iterate in reverse. Apply predictor encoding before the codec, then:

```rust
encoded = if name == b"FlateDecode" {
    encode_flate(&after_predictor)?
} else {
    apply_single_filter_encode(name, &after_predictor).map_err(Error::Unsupported)?
};
```

Do not apply the decode-chain limit on encode. Preserve the current Crypt and unsupported-codec behavior.

- [x] **Step 5: Delete direct flate2 routes**

Remove:

```rust
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::io::{Read, Write};
```

Delete the `FlateDecode` branches from both one-shot helpers. Move `DECODE_OUTPUT_LIMIT_PREFIX` into `stream_filter.rs` and re-export it crate-privately to keep `is_decode_output_limit_error` using one source of truth.

Prove the production route is gone:

```bash
rg -n "flate2|ZlibDecoder|ZlibEncoder" crates/flpdf/src/filters.rs
```

Expected: no matches.

- [x] **Step 6: Run focused filter suites and verify GREEN**

Run:

```bash
cargo test -p flpdf filters::tests -- --nocapture
cargo test -p flpdf --test multi_filter_chain_tests -- --nocapture
cargo test -p flpdf --test compress_streams_tests -- --nocapture
cargo test -p flpdf --test reader_tests -- --nocapture
cargo test -p flpdf --test xref_tests -- --nocapture
cargo test -p flpdf --test check_tests -- --nocapture
cargo test -p flpdf --test writer_tests -- --nocapture
cargo test -p flpdf-cli --test cli_tests -- --nocapture
```

Expected: all pass. If an old expectation conflicts, compare qpdf 11.9.0 observed output before changing the test.

- [x] **Step 7: Refactor while green**

Keep these boundaries:

- `stream_filter.rs`: interpretation/alignment and PlFlate execution;
- `filters.rs`: public API, predictor and later-codec compatibility dispatch;
- `pipeline/flate.rs`: the only zlib state machine.

Do not move LZW, predictor, ASCII, or RunLength code in this Bead.

- [x] **Step 8: Commit the production cutover**

```bash
git add crates/flpdf/src/filters.rs crates/flpdf/src/stream_filter.rs \
  crates/flpdf/tests/multi_filter_chain_tests.rs \
  crates/flpdf/tests/compress_streams_tests.rs
git commit -m "refactor: cut filters over to PlFlate"
```

---

### Task 4: Update correspondence and run completion gates

**Files:**
- Modify if generated correspondence requires it: `docs/qpdf-correspondence.md`
- Modify if the repository contract test requires annotations: affected module-level qpdf correspondence comments
- Tracking: Bead `flpdf-qynx.5.1`

**Interfaces:**
- Proves every production `decode_stream_data` consumer reaches the new route through the unchanged public entry point.
- Proves the old direct Flate route is absent.
- Produces verification evidence for the PR and Bead close.

- [x] **Step 1: Audit implementation and scope**

Run:

```bash
rg -n "decode_stream_data\\(" crates/flpdf/src crates/flpdf-cli/src
rg -n "flate2|ZlibDecoder|ZlibEncoder" crates/flpdf/src/filters.rs
rg -n "decode_filter_specs|decode_flate|encode_flate" crates/flpdf/src
git diff --check
```

Expected: existing consumers still use the public entry point; no direct flate2 route remains in `filters.rs`; only the new driver calls the PlFlate adapter.

- [x] **Step 2: Run formatting, Clippy, and correspondence gates**

Run:

```bash
cargo fmt --all
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
python3 scripts/qpdf-module-docs.py --check
```

Expected: all pass with no warnings.

- [x] **Step 3: Run focused and workspace tests**

Run:

```bash
cargo test -p flpdf filters::tests
cargo test -p flpdf pipeline::flate::tests
cargo test -p flpdf --test multi_filter_chain_tests
cargo test -p flpdf --test compress_streams_tests
cargo test -p flpdf --test reader_tests
cargo test -p flpdf --test xref_tests
cargo test -p flpdf --test check_tests
cargo test -p flpdf --test writer_tests
cargo test -p flpdf-cli --test cli_tests
cargo test --workspace --all-features
```

Expected: all pass.

- [x] **Step 4: Run qpdf-zlib compatibility gates**

Run the repository's existing zlib/byte-equivalence tests:

```bash
cargo test -p flpdf --features qpdf-zlib-compat --test zlib_compat_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests
```

Expected: all applicable tests pass or report only their documented environment skip.

- [x] **Step 5: Obtain fresh 100% patch coverage**

Run a fresh report:

```bash
cargo llvm-cov clean --workspace
cargo llvm-cov --workspace --all-features --lcov --output-path target/llvm-cov/lcov.info
scripts/patch-coverage.sh --base origin/main --lcov target/llvm-cov/lcov.info
```

Expected: every changed executable line is covered; patch coverage is exactly 100%.

- [x] **Step 6: Final self-review and commit**

Review:

```bash
git diff origin/main...HEAD
git status --short
```

Check for placeholders, duplicated codec logic, unintended public API, stale direct routes, and unrelated changes. Commit only files in this Bead:

```bash
git add crates/flpdf/src/lib.rs crates/flpdf/src/stream_filter.rs \
  crates/flpdf/src/filters.rs docs/qpdf-correspondence.md \
  docs/superpowers/plans/2026-07-28-qpdf-stream-filter-flate-cutover.md
git commit -m "docs: record stream filter cutover"
```

- [ ] **Step 7: Persist and publish**

After all verification succeeds:

```bash
bd close flpdf-qynx.5.1 --reason "QPDFStreamFilterのFilter/DecodeParms driverを追加し、production decode/encodeのFlateをPlFlateへ完全cutover。direct filters.rs flate2 routeを削除し、qpdf oracle、workspace gates、fresh patch coverage 100%を確認。"
bd dolt push
git pull --rebase
git push -u origin feature/flpdf-qynx-5-1-stream-filter
```

Do not describe the work as complete until both Beads and git pushes succeed.
