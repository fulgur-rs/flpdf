# qpdf ASCII and RunLength Pipeline Cutover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace flpdf's whole-buffer ASCII85, ASCIIHex, and RunLength decoder paths, plus its duplicate RunLength encoder, with qpdf 11.9.0-faithful incremental Pipeline components used by production.

**Architecture:** Three crate-private Pipeline stages mirror `Pl_ASCII85Decoder`, `Pl_ASCIIHexDecoder`, and both actions of `Pl_RunLength`. `StreamFilter` adapters collect decoder output for the existing public whole-buffer API, while RunLength encoding uses the same stateful component directly; ASCII85 and ASCIIHex retain only their flpdf-specific encoders.

**Tech Stack:** Rust workspace, crate-private `Pipeline`/`PipelineError`, qpdf 11.9.0 C++ oracle, Bash differential runner, Cargo tests, rustfmt, Clippy, rustdoc, llvm-cov patch coverage.

## Global Constraints

- The behavioral oracle is qpdf 11.9.0 at commit `3b97c9bd266b7c32ea36d3536e22dab77412886d`, resolved only by `scripts/fetch-qpdf-source.sh --print-path`.
- Preserve the public signatures and whole-buffer return types of `decode_stream_data`, `decode_stream_data_with_limits`, and `encode_stream_data`.
- Do not add a common finished guard; preserve each qpdf component's repeated-`finish` and post-`finish` state.
- Reject non-null `/DecodeParms` for ASCII85, ASCIIHex, and RunLength before codec construction or writes.
- Keep RunLength classified as specialized, non-lossy compression; ASCII85 and ASCIIHex remain non-specialized and non-lossy.
- Preserve the existing downstream `OutputBuffer` limit semantics; do not introduce codec-specific size caps.
- Retain the flpdf ASCII85 and ASCIIHex encoders because qpdf 11.9.0 has no matching encoder components.
- Delete the old ASCII decoder functions and the entire duplicate `crates/flpdf/src/run_length.rs`; no compatibility wrapper remains.
- Use RED → GREEN → REFACTOR for every production behavior change and make one reviewable commit per task.
- Finish with a fresh report showing 100% changed executable-line coverage.

---

## File map

**Create**

- `crates/flpdf/src/pipeline/test_support.rs` — test-only shared downstream trace and deterministic failure sink.
- `crates/flpdf/src/pipeline/ascii85.rs` — qpdf `Pl_ASCII85Decoder` state machine and component tests.
- `crates/flpdf/src/pipeline/ascii_hex.rs` — qpdf `Pl_ASCIIHexDecoder` state machine and component tests.
- `crates/flpdf/src/pipeline/run_length.rs` — qpdf `Pl_RunLength` encode/decode state machine and component tests.
- `crates/flpdf/src/pipeline/stream_codecs_oracle.rs` — ignored live differential plus normally run comparison/probe-boundary tests.
- `tests/oracle/qpdf_stream_codecs_probe.cc` — direct qpdf component trace probe.
- `scripts/qpdf-stream-codecs-diff.sh` — pinned-source, private-build, fail-closed differential runner.
- `scripts/tests/qpdf-stream-codecs-diff-contract.sh` — runner contract and failure-path regression test.

**Modify**

- `crates/flpdf/src/pipeline.rs` — declare the three production modules and two test-only support modules.
- `crates/flpdf/src/stream_filter.rs` — add decoder adapters, register factories, expose RunLength classification, and add `encode_run_length`.
- `crates/flpdf/src/filters.rs` — remove direct decoder branches, use Pipeline RunLength encoding, and update public-path regressions.
- `crates/flpdf/src/ascii85.rs` — retain only encoder implementation/tests and correct module documentation.
- `crates/flpdf/src/ascii_hex.rs` — retain only encoder implementation/tests and correct module documentation.
- `crates/flpdf/src/lib.rs` — remove the old root `run_length` module declaration.
- `crates/flpdf/tests/multi_filter_chain_tests.rs` — pin mixed-chain behavior after the factory cutover.
- `docs/qpdf-correspondence.md` — point correspondence rows at Pipeline components and StreamFilter adapters.
- `docs/qpdf-module-doc-index.md` — regenerate from source documentation.

**Delete**

- `crates/flpdf/src/run_length.rs` — superseded whole-buffer encoder/decoder and divergence-pinning tests.

---

### Task 1: Build the pinned qpdf stream-codec trace oracle

**Files:**

- Create: `tests/oracle/qpdf_stream_codecs_probe.cc`
- Create: `scripts/qpdf-stream-codecs-diff.sh`
- Create: `scripts/tests/qpdf-stream-codecs-diff-contract.sh`

**Interfaces:**

- Consumes: qpdf `Pipeline`, `Pl_ASCII85Decoder`, `Pl_ASCIIHexDecoder`, and `Pl_RunLength` from the pinned source.
- Produces: executable protocol
  `qpdf_stream_codecs_probe CODEC FAIL_WRITES FAIL_FINISHES OP...`, where
  `CODEC` is `ascii85`, `asciihex`, `runlength-decode`, or
  `runlength-encode`; failure lists are comma-separated one-based call
  indexes or `-`; and each operation is `w:HEX` or `f`.
- Produces: `scripts/qpdf-stream-codecs-diff.sh`, which exports
  `QPDF_STREAM_CODECS_PROBE` and selects exactly
  `pipeline::stream_codecs_oracle::qpdf_stream_codecs_differential`.

- [ ] **Step 1: Write the failing runner contract**

Create a shell contract that installs fake `git`, `mktemp`, `c++`, and `cargo`
commands in a private fixture. Its success case asserts that the runner:

```bash
required_cxx_args=(
  "-std=c++17"
  "-DQPDF_DISABLE_QTC"
  "-I${fixture_source}/include"
  "-I${fixture_source}/libqpdf"
  "${fixture_repo}/tests/oracle/qpdf_stream_codecs_probe.cc"
  "${fixture_source}/libqpdf/Pipeline.cc"
  "${fixture_source}/libqpdf/Pl_ASCII85Decoder.cc"
  "${fixture_source}/libqpdf/Pl_ASCIIHexDecoder.cc"
  "${fixture_source}/libqpdf/Pl_RunLength.cc"
)
required_cargo_args=(
  test -p flpdf --lib
  pipeline::stream_codecs_oracle::qpdf_stream_codecs_differential
  -- --ignored --exact
)
```

The fake cargo command must reject a missing/non-executable
`QPDF_STREAM_CODECS_PROBE`. Add separate invocations that require failure for:

```text
wrong source HEAD
dirty source before compile
compiler failure
dirty source after compile
dirty source after cargo
git status failure before compile
git status failure after cargo
mktemp leaf replaced by a symlink
```

Each failure case also asserts that a validated external build directory is
removed and a victim directory is untouched. A repository-local unsafe
`TMPDIR` is deliberately not a failure case: the runner must warn, continue
through a safe external directory, remove that directory after success, and
leave the victim untouched. The contract must assert the warning, external
build placement/cleanup, and victim preservation for this fallback.

- [ ] **Step 2: Run the contract to verify RED**

Run:

```bash
bash scripts/tests/qpdf-stream-codecs-diff-contract.sh
```

Expected: FAIL because `scripts/qpdf-stream-codecs-diff.sh` and the probe do
not exist.

- [ ] **Step 3: Implement the C++ trace probe**

Use a `RecordingPipeline` with this observable record:

```cpp
struct Call
{
    std::string kind;       // "write" or "finish"
    std::vector<unsigned char> data;
    bool failed;
};

class RecordingPipeline: public Pipeline
{
  public:
    RecordingPipeline(
        std::set<size_t> fail_writes,
        std::set<size_t> fail_finishes);
    void write(unsigned char const* data, size_t len) override;
    void finish() override;

    std::vector<Call> calls;
    std::vector<unsigned char> output;
};
```

Increment the applicable one-based call counter before deciding whether to
throw. A selected write throws `std::runtime_error("sink write failure N")`;
a selected finish throws
`std::runtime_error("sink finish failure N")`. Record failed attempts but add
only successful writes to `output`.

Construct the selected component with identifier `oracle codec` and execute
each operation independently. Catch `std::logic_error` and
`std::runtime_error` around each operation, record the exception category and
raw `what()` bytes, and continue to the next requested operation.

Emit a stable ASCII record using lowercase hex for every binary field:

```text
op\t0\tok\t
op\t1\truntime\t73696e6b207772697465206661696c7572652031
call\twrite\t0\t4\t00000000
call\tfinish\t0\t0\t
output\t00000000
```

For each `call` line, the third field is `1` for a failed call and `0` for a
successful call. Reject an unknown codec, malformed hex, malformed failure
list, or malformed operation with exit status 2 and a
`qpdf_stream_codecs_probe:`-prefixed diagnostic.

- [ ] **Step 4: Implement the hardened runner**

Use the existing hardened lifecycle from `scripts/qpdf-rc4-diff.sh` with these
codec-specific constants and compile command:

```bash
qpdf_commit="3b97c9bd266b7c32ea36d3536e22dab77412886d"
probe="${build_dir_fd_path}/qpdf_stream_codecs_probe"

c++ -std=c++17 -DQPDF_DISABLE_QTC \
  "-I${qpdf_source}/include" \
  "-I${qpdf_source}/libqpdf" \
  "${repo_root}/tests/oracle/qpdf_stream_codecs_probe.cc" \
  "${qpdf_source}/libqpdf/Pipeline.cc" \
  "${qpdf_source}/libqpdf/Pl_ASCII85Decoder.cc" \
  "${qpdf_source}/libqpdf/Pl_ASCIIHexDecoder.cc" \
  "${qpdf_source}/libqpdf/Pl_RunLength.cc" \
  -o "${probe}"

QPDF_STREAM_CODECS_PROBE="${probe}" \
  cargo test -p flpdf --lib \
  pipeline::stream_codecs_oracle::qpdf_stream_codecs_differential \
  -- --ignored --exact
```

Before and after compilation and after cargo, verify both the exact HEAD and
tracked-file cleanliness. Preserve the private external `mktemp` directory,
file-descriptor identity checks, safe cleanup, and fail-closed behavior from
the hardened runner.

- [ ] **Step 5: Run the contract and direct probe smoke**

Run:

```bash
bash scripts/tests/qpdf-stream-codecs-diff-contract.sh
qpdf_source="$(scripts/fetch-qpdf-source.sh --print-path)"
probe_dir="$(mktemp -d -p /tmp flpdf-qpdf-stream-codecs-smoke.XXXXXX)"
case "${probe_dir}" in
  /tmp/flpdf-qpdf-stream-codecs-smoke.*) ;;
  *) echo "unexpected probe directory: ${probe_dir}" >&2; exit 1 ;;
esac
probe_inode="$(stat -c '%d:%i' -- "${probe_dir}")"
cleanup_probe() {
  if [[ ! -L "${probe_dir}" && -d "${probe_dir}" ]] &&
    [[ "$(stat -c '%d:%i' -- "${probe_dir}")" == "${probe_inode}" ]]; then
    rm -rf -- "${probe_dir}"
  fi
}
trap cleanup_probe EXIT
c++ -std=c++17 -DQPDF_DISABLE_QTC \
  "-I${qpdf_source}/include" "-I${qpdf_source}/libqpdf" \
  tests/oracle/qpdf_stream_codecs_probe.cc \
  "${qpdf_source}/libqpdf/Pipeline.cc" \
  "${qpdf_source}/libqpdf/Pl_ASCII85Decoder.cc" \
  "${qpdf_source}/libqpdf/Pl_ASCIIHexDecoder.cc" \
  "${qpdf_source}/libqpdf/Pl_RunLength.cc" \
  -o "${probe_dir}/probe"
"${probe_dir}/probe" ascii85 - - w:7a f
"${probe_dir}/probe" asciihex - - w:343e f
"${probe_dir}/probe" runlength-encode - - w:4141 f
```

Expected:

- contract PASS;
- ASCII85 output line ends in `00000000`;
- ASCIIHex output line ends in `40`;
- RunLength encode output line ends in `ff4180`.

The EXIT trap removes only the validated prefixed `${probe_dir}`.

- [ ] **Step 6: Commit the oracle boundary**

```bash
git add \
  tests/oracle/qpdf_stream_codecs_probe.cc \
  scripts/qpdf-stream-codecs-diff.sh \
  scripts/tests/qpdf-stream-codecs-diff-contract.sh
git commit -m "test: add qpdf stream codec oracle"
```

---

### Task 2: Implement the qpdf ASCII85 Pipeline component

**Files:**

- Create: `crates/flpdf/src/pipeline/test_support.rs`
- Create: `crates/flpdf/src/pipeline/ascii85.rs`
- Modify: `crates/flpdf/src/pipeline.rs`

**Interfaces:**

- Consumes: `Pipeline` and `PipelineError`.
- Produces:

```rust
pub(crate) struct Ascii85Decoder<'a>;

impl<'a> Ascii85Decoder<'a> {
    pub(crate) fn new(identifier: impl Into<String>, next: &'a mut dyn Pipeline) -> Self;
}
```

- Produces test-only `RecordingSink`, `Trace`, and `shared_trace()` for all
  three component test modules.

- [ ] **Step 1: Add shared sink support and failing ASCII85 tests**

Declare the modules:

```rust
pub(crate) mod ascii85;

#[cfg(test)]
pub(crate) mod test_support;
```

The test sink records attempted calls through `Rc<RefCell<Trace>>` so tests can
inspect state while the stage still borrows the sink:

```rust
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Trace {
    pub(crate) calls: Vec<TraceCall>,
    pub(crate) output: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TraceCall {
    Write { data: Vec<u8>, failed: bool },
    Finish { failed: bool },
}
```

Start `pipeline/ascii85.rs` with:

```rust
//! qpdf correspondence: Pl_ASCII85Decoder.cc incremental decode state, output, error, and finish semantics.
```

Start `pipeline/test_support.rs` with:

```rust
//! qpdf correspondence: flpdf-only test instrumentation for observable Pipeline downstream calls and failures.
```

`RecordingSink::new(fail_writes, fail_finishes)` uses one-based attempt
indexes and error messages `sink write failure N` /
`sink finish failure N`.

Add table-driven ASCII85 assertions with these exact cases:

```rust
let success_cases = [
    (b"9jqo^".as_slice(), b"Man ".as_slice()),
    (b"z".as_slice(), &[0, 0, 0, 0]),
    (b"!".as_slice(), b""),
    (b"!!".as_slice(), &[0]),
    (b"!!!".as_slice(), &[0, 0]),
    (b"!!!!".as_slice(), &[0, 0, 0]),
    (b"uuuuu".as_slice(), &[0x08, 0x78, 0x0e, 0xc4]),
    (b"9jqo^~ \x0c\x0b\t\r\n>ignored".as_slice(), b"Man "),
];
```

Add exact runtime-error cases:

```rust
[
    (b"!\0".as_slice(), "character out of range during base 85 decode"),
    (b"!z".as_slice(), "unexpected z during base 85 decode"),
    (b"~X".as_slice(), "broken end-of-data sequence in base 85 data"),
]
```

Also assert:

- every split position of `b"9jqo^~>"` matches one write;
- `write(b"9jqo^~"); finish()` emits `Man ` and finishes once;
- a final one-character group records `Write { data: vec![], failed: false }`;
- data after completed `~>` is ignored;
- two finishes produce two finish calls;
- failure on a full or partial flush resets `pos`, proven by a later valid
  write producing a fresh group;
- a downstream failure while handling `>` leaves EOD waiting at state 1, so
  a later `>` completes EOD after the already-reset zero-length flush;
- flush failure suppresses that operation's downstream finish;
- without explicit EOD, `write; finish; write; finish` decodes both groups and
  records both finishes.

- [ ] **Step 2: Run the ASCII85 tests to verify RED**

Run:

```bash
cargo test -p flpdf --lib pipeline::ascii85::tests -- --nocapture
```

Expected: compile failure because `Ascii85Decoder` is not implemented.

- [ ] **Step 3: Implement the minimal qpdf state machine**

Use this state and qpdf ordering:

```rust
pub(crate) struct Ascii85Decoder<'a> {
    identifier: String,
    next: &'a mut dyn Pipeline,
    inbuf: [u8; 5],
    pos: usize,
    eod: u8,
}
```

`write` must:

```rust
if self.eod > 1 {
    return Ok(());
}
for &byte in data {
    if matches!(byte, b' ' | b'\x0c' | b'\x0b' | b'\t' | b'\r' | b'\n') {
        continue;
    }
    if self.eod > 1 {
        break;
    } else if self.eod == 1 {
        if byte == b'>' {
            self.flush()?;
            self.eod = 2;
        } else {
            return Err(PipelineError::runtime(
                "broken end-of-data sequence in base 85 data",
            ));
        }
    } else {
        match byte {
            b'~' => self.eod = 1,
            b'z' if self.pos == 0 => self.next.write(&[0; 4])?,
            b'z' => {
                return Err(PipelineError::runtime(
                    "unexpected z during base 85 decode",
                ));
            }
            b'!'..=b'u' => {
                self.inbuf[self.pos] = byte;
                self.pos += 1;
                if self.pos == 5 {
                    self.flush()?;
                }
            }
            _ => {
                return Err(PipelineError::runtime(
                    "character out of range during base 85 decode",
                ));
            }
        }
    }
}
Ok(())
```

In `flush`, compute with wrapping 32-bit arithmetic, save `pos - 1`, reset
`pos` and `inbuf` to `[b'u'; 5]` before calling downstream `write`, and do call
`write(&out[..0])` for a one-character group. `finish` is exactly:

```rust
self.flush()?;
self.next.finish()
```

- [ ] **Step 4: Run ASCII85 tests to verify GREEN**

Run:

```bash
cargo test -p flpdf --lib pipeline::ascii85::tests -- --nocapture
```

Expected: PASS.

- [ ] **Step 5: Refactor against the qpdf source and run local quality checks**

Compare branch-for-branch with
`libqpdf/Pl_ASCII85Decoder.cc`. Keep reset-before-downstream ordering and
remove helpers that only restate one expression.

Run:

```bash
cargo fmt --all -- --check
cargo clippy -p flpdf --lib --all-features -- -D warnings
```

Expected: PASS.

- [ ] **Step 6: Commit the ASCII85 component**

```bash
git add \
  crates/flpdf/src/pipeline.rs \
  crates/flpdf/src/pipeline/test_support.rs \
  crates/flpdf/src/pipeline/ascii85.rs
git commit -m "feat: add qpdf ASCII85 pipeline"
```

---

### Task 3: Implement the qpdf ASCIIHex Pipeline component

**Files:**

- Create: `crates/flpdf/src/pipeline/ascii_hex.rs`
- Modify: `crates/flpdf/src/pipeline.rs`

**Interfaces:**

- Consumes: `Pipeline`, `PipelineError`, and test support from Task 2.
- Produces:

```rust
pub(crate) struct AsciiHexDecoder<'a>;

impl<'a> AsciiHexDecoder<'a> {
    pub(crate) fn new(identifier: impl Into<String>, next: &'a mut dyn Pipeline) -> Self;
}
```

- [ ] **Step 1: Write failing state, chunk, and error tests**

Declare `pub(crate) mod ascii_hex;` and add exact cases:

```rust
//! qpdf correspondence: Pl_ASCIIHexDecoder.cc incremental decode state, output, error, and finish semantics.
```

```rust
let success_cases = [
    (b"48656c6c6f".as_slice(), b"Hello".as_slice()),
    (b"4f6".as_slice(), &[0x4f, 0x60]),
    (b"4F6C".as_slice(), &[0x4f, 0x6c]),
    (b"4 \x0c\x0b\t\r\n8".as_slice(), &[0x48]),
    (b"4>ignored".as_slice(), &[0x40]),
];
```

Assert `b"48\0"` returns the exact message
`character out of range during base Hex decode: ` after first recording
`Write([0x48])`. Assert `b"4G"` includes the visible `G` suffix.

For every split of `b"48656c6c6f>"`, compare output and individual one-byte
writes with the unsplit case. Add tests for:

- EOD after a pending nibble;
- data ignored after EOD;
- a pending nibble flushed by `finish`;
- two finishes producing two downstream finish calls;
- a failing complete-pair write resetting the nibble buffer before reuse;
- a failing partial flush suppressing that finish call and retaining EOD when
  the flush was triggered by `>`;
- without explicit EOD, `write; finish; write; finish` decodes both inputs.

- [ ] **Step 2: Run ASCIIHex tests to verify RED**

Run:

```bash
cargo test -p flpdf --lib pipeline::ascii_hex::tests -- --nocapture
```

Expected: compile failure because `AsciiHexDecoder` is absent.

- [ ] **Step 3: Implement the qpdf state machine**

Use:

```rust
pub(crate) struct AsciiHexDecoder<'a> {
    identifier: String,
    next: &'a mut dyn Pipeline,
    inbuf: [u8; 2],
    pos: usize,
    eod: bool,
}
```

Initialize `inbuf` to `[b'0', b'0']`. Process ASCII letters with
`to_ascii_uppercase`, ignore exactly qpdf's six whitespace bytes, mark EOD
before flushing on `>`, and stop the current write after EOD.

Build the invalid-character suffix from the uppercased `ch` as:

```rust
let suffix = if ch == 0 {
    String::new()
} else {
    char::from(ch).to_string()
};
PipelineError::runtime(format!(
    "character out of range during base Hex decode: {suffix}"
))
```

In `flush`, derive the byte from two ASCII digits, then reset `pos` and
`inbuf` before the one-byte downstream write. `finish` flushes then finishes
downstream without changing EOD.

- [ ] **Step 4: Run ASCIIHex tests to verify GREEN**

Run:

```bash
cargo test -p flpdf --lib pipeline::ascii_hex::tests -- --nocapture
```

Expected: PASS.

- [ ] **Step 5: Refactor and quality-check**

Compare directly with `libqpdf/Pl_ASCIIHexDecoder.cc`, particularly EOD
assignment and reset-before-write ordering.

Run:

```bash
cargo fmt --all -- --check
cargo clippy -p flpdf --lib --all-features -- -D warnings
```

Expected: PASS.

- [ ] **Step 6: Commit the ASCIIHex component**

```bash
git add crates/flpdf/src/pipeline.rs crates/flpdf/src/pipeline/ascii_hex.rs
git commit -m "feat: add qpdf ASCIIHex pipeline"
```

---

### Task 4: Implement qpdf RunLength encode and decode Pipelines

**Files:**

- Create: `crates/flpdf/src/pipeline/run_length.rs`
- Modify: `crates/flpdf/src/pipeline.rs`

**Interfaces:**

- Consumes: `Pipeline`, `PipelineError`, and Task 2 test support.
- Produces:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RunLengthAction {
    Encode,
    Decode,
}

pub(crate) struct RunLength<'a>;

impl<'a> RunLength<'a> {
    pub(crate) fn new(
        identifier: impl Into<String>,
        next: &'a mut dyn Pipeline,
        action: RunLengthAction,
    ) -> Self;
}
```

- [ ] **Step 1: Write failing decoder tests**

Declare `pub(crate) mod run_length;`. Add the following decode table:

```rust
//! qpdf correspondence: Pl_RunLength.cc incremental encode and decode state, output, error, and finish semantics.
```

```rust
[
    (vec![0x80], vec![]),
    (vec![0x02, b'A', b'B', b'C', 0x80], b"ABC".to_vec()),
    (vec![0xfe, b'A', 0x80], b"AAA".to_vec()),
    (vec![0x7f].into_iter().chain(0_u8..128).collect(), (0_u8..128).collect()),
    (vec![0x81, 0xab], vec![0xab; 128]),
    (vec![0x05, b'A', b'B', b'C'], b"ABC".to_vec()),
    (vec![0xfd], vec![]),
    (vec![0x80, 0x00, b'Z'], b"Z".to_vec()),
]
```

For each complete packet, split after every byte and assert the same output
and one-byte downstream writes. Add state-order tests:

- `write([0x02, A]); finish(); write([B, C])` continues the literal packet;
- `write([0xfd]); finish(); write([Z])` completes the repeat packet;
- failing literal write does not decrement remaining length;
- failing one call in repeat output keeps run state and retries the full run
  on a later input byte;
- two finishes make two downstream finish attempts.
- `identifier()` returns the constructor identifier;
- direct test-only state mutation reaches the qpdf logic-error branches for
  top state with length 2 and run state with lengths 1 and 129, asserting
  their exact messages.

- [ ] **Step 2: Run decoder tests to verify RED**

Run:

```bash
cargo test -p flpdf --lib pipeline::run_length::tests::decode -- --nocapture
```

Expected: compile failure because `RunLength` is absent.

- [ ] **Step 3: Implement minimal decode mode**

Use:

```rust
enum State {
    Top,
    Copying,
    Run,
}

pub(crate) struct RunLength<'a> {
    identifier: String,
    next: &'a mut dyn Pipeline,
    action: RunLengthAction,
    state: State,
    length: usize,
    buf: [u8; 128],
}
```

In top state, headers below 128 set `length = header + 1` and copying state;
headers above 128 set `length = 257 - header` and run state; 128 leaves top
state unchanged. Copying writes one byte and decrements only after success.
Run writes the same input byte in `length` separate calls and returns to top
only after all succeed. Decode `finish` only calls downstream `finish`.

- [ ] **Step 4: Run decoder tests to verify GREEN**

Run:

```bash
cargo test -p flpdf --lib pipeline::run_length::tests::decode -- --nocapture
```

Expected: PASS.

- [ ] **Step 5: Write failing encoder tests**

Pin exact bytes:

```rust
let cases = [
    (b"".as_slice(), vec![0x80]),
    (b"A".as_slice(), vec![0x00, b'A', 0x80]),
    (b"AA".as_slice(), vec![0xff, b'A', 0x80]),
    (b"AB".as_slice(), vec![0x01, b'A', b'B', 0x80]),
    (b"ABCC".as_slice(), vec![0x01, b'A', b'B', 0xff, b'C', 0x80]),
];
```

Add deterministic inputs for:

- 127, 128, and 129 distinct literal bytes;
- 127, 128, and 129 equal bytes;
- literal-to-run transitions with the equal pair at positions 2, 127, 128,
  and 129;
- every split point of a 260-byte mixed fixture, compared with one write;
- `write; finish; write; finish` emits two independently terminated encoded
  sequences and makes two downstream finish calls.

Assert exact downstream call chunks for `ABCC`:

```rust
[
    Write([0x01]),
    Write(b"AB"),
    Write([0xff]),
    Write(b"C"),
    Write([0x80]),
    Finish,
]
```

Inject failures at each of those five writes and at finish. Assert that no
later call in the same operation occurs and that retry behavior matches
qpdf's reset-after-success ordering.

- [ ] **Step 6: Run encoder tests to verify RED**

Run:

```bash
cargo test -p flpdf --lib pipeline::run_length::tests::encode -- --nocapture
```

Expected: FAIL because encode mode has no packetization.

- [ ] **Step 7: Implement qpdf encode mode**

Port the qpdf transition condition exactly:

```rust
if (matches!(self.state, State::Top) != (self.length <= 1)) {
    return Err(PipelineError::logic(
        "Pl_RunLength::encode: state/length inconsistency",
    ));
}

if self.length > 0
    && (matches!(self.state, State::Copying) || self.length < 128)
    && byte == self.buf[self.length - 1]
{
    if matches!(self.state, State::Copying) {
        self.length -= 1;
        self.flush_encode()?;
        self.buf[0] = byte;
        self.length = 1;
    }
    self.state = State::Run;
    self.buf[self.length] = byte;
    self.length += 1;
} else {
    if self.length == 128 || matches!(self.state, State::Run) {
        self.flush_encode()?;
    } else if self.length > 0 {
        self.state = State::Copying;
    }
    self.buf[self.length] = byte;
    self.length += 1;
}
```

`flush_encode` validates run lengths 2 through 128, writes header and payload
as separate downstream calls, and resets state only after both succeed.
Encode `finish` calls `flush_encode`, writes `[128]`, then finishes downstream.

- [ ] **Step 8: Run all RunLength tests and quality checks**

Run:

```bash
cargo test -p flpdf --lib pipeline::run_length::tests -- --nocapture
cargo fmt --all -- --check
cargo clippy -p flpdf --lib --all-features -- -D warnings
```

Expected: PASS.

- [ ] **Step 9: Commit the RunLength component**

```bash
git add crates/flpdf/src/pipeline.rs crates/flpdf/src/pipeline/run_length.rs
git commit -m "feat: add qpdf RunLength pipeline"
```

---

### Task 5: Register StreamFilter adapters and cut over production decoding

**Files:**

- Modify: `crates/flpdf/src/stream_filter.rs`
- Modify: `crates/flpdf/src/filters.rs`
- Modify: `crates/flpdf/tests/multi_filter_chain_tests.rs`

**Interfaces:**

- Consumes: all three decoder components from Tasks 2–4.
- Produces registered `StreamFilter` adapters for `ASCII85Decode`,
  `ASCIIHexDecode`, and `RunLengthDecode`.
- Removes those names from the fallback decoder; unsupported fallback
  behavior for LZW and passthrough codecs remains unchanged.

- [ ] **Step 1: Change factory and public-path tests to RED**

Replace `factory_returns_none_for_not_yet_migrated_filters` with a table that
requires all four production factories:

```rust
for name in [
    b"FlateDecode".as_slice(),
    b"ASCII85Decode",
    b"ASCIIHexDecode",
    b"RunLengthDecode",
] {
    assert!(stream_filter_for(name).is_some(), "{name:?}");
}
```

For each new adapter, assert `None` and `Some(Object::Null)` parameters are
accepted, while `Some(Object::Dictionary(Dictionary::new()))` and
`Some(Object::Integer(1))` are rejected. Assert only RunLength reports
specialized compression and none reports lossy compression.

Use `pipe_decode` to pin:

```rust
[
    (b"ASCII85Decode".as_slice(), b"z~>".as_slice(), vec![0, 0, 0, 0]),
    (b"ASCIIHexDecode".as_slice(), b"4142>".as_slice(), b"AB".to_vec()),
    (b"RunLengthDecode".as_slice(), &[0xff, b'A', 0x80], b"AA".to_vec()),
]
```

Add output-limit cases one byte below and exactly at each expected output
length.

Update the old divergence assertions in `filters.rs`:

```rust
assert_eq!(
    decode_stream_data(&run_length_dict(), &[0x05, b'A', b'B', b'C']).unwrap(),
    b"ABC"
);
assert_eq!(
    decode_stream_data(&run_length_dict(), &[0x80, 0x00, b'Z']).unwrap(),
    b"Z"
);
assert_eq!(
    decode_stream_data(&ascii85_dict(), b"uuuuu").unwrap(),
    [0x08, 0x78, 0x0e, 0xc4]
);
```

Assert ASCII85/ASCIIHex accept vertical tab and reject NUL with qpdf messages.
For each of the three filter names, supply a non-null empty dictionary and
assert:

```text
unsupported PDF feature: stream filter FILTER does not support supplied /DecodeParms
```

Use malformed or output-producing encoded bytes in these assertions, so the
parameter error proves validation happens before codec errors or writes.

Add a mixed chain regression in `multi_filter_chain_tests.rs` that decodes:

```text
[/ASCII85Decode /FlateDecode /RunLengthDecode]
```

Build bytes in reverse encode order and assert the original asymmetric
payload. Keep existing null-in-array Predictor coverage unchanged.

- [ ] **Step 2: Run factory and public-path tests to verify RED**

Run:

```bash
cargo test -p flpdf --lib stream_filter::tests -- --nocapture
cargo test -p flpdf --lib filters::tests -- --nocapture
cargo test -p flpdf --test multi_filter_chain_tests -- --nocapture
```

Expected: FAIL because the new names are not registered and the old direct
helpers retain the demonstrated qpdf divergences.

- [ ] **Step 3: Implement the three adapters and factories**

Use one small helper per codec so each borrow ends before returning sink data:

```rust
fn decode_ascii85(data: &[u8], max_output: Option<usize>) -> Result<Vec<u8>> {
    let mut sink = OutputBuffer::new(max_output);
    {
        let mut stage = Ascii85Decoder::new("ascii85 decode", &mut sink);
        stage.write(data).map_err(map_pipeline_error)?;
        stage.finish().map_err(map_pipeline_error)?;
    }
    Ok(sink.data)
}
```

Implement the other helpers explicitly as:

```rust
fn decode_ascii_hex(data: &[u8], max_output: Option<usize>) -> Result<Vec<u8>> {
    let mut sink = OutputBuffer::new(max_output);
    {
        let mut stage = AsciiHexDecoder::new("asciiHex decode", &mut sink);
        stage.write(data).map_err(map_pipeline_error)?;
        stage.finish().map_err(map_pipeline_error)?;
    }
    Ok(sink.data)
}

fn decode_run_length(data: &[u8], max_output: Option<usize>) -> Result<Vec<u8>> {
    let mut sink = OutputBuffer::new(max_output);
    {
        let mut stage = RunLength::new(
            "runlength decode",
            &mut sink,
            RunLengthAction::Decode,
        );
        stage.write(data).map_err(map_pipeline_error)?;
        stage.finish().map_err(map_pipeline_error)?;
    }
    Ok(sink.data)
}
```

Each adapter ignores the warning callback because the matching qpdf component
emits no warnings.

Register:

```rust
b"ASCII85Decode" => Some(Box::new(Ascii85StreamFilter)),
b"ASCIIHexDecode" => Some(Box::new(AsciiHexStreamFilter)),
b"RunLengthDecode" => Some(Box::new(RunLengthStreamFilter)),
```

Override only `RunLengthStreamFilter::is_specialized_compression` to return
`true`.

- [ ] **Step 4: Run factory and public-path tests to verify GREEN**

Run:

```bash
cargo test -p flpdf --lib stream_filter::tests -- --nocapture
cargo test -p flpdf --lib filters::tests -- --nocapture
cargo test -p flpdf --test multi_filter_chain_tests -- --nocapture
```

Expected: PASS through the registered adapters.

- [ ] **Step 5: Delete direct decode dispatch**

Remove these imports and branches from `filters.rs`:

```rust
use crate::run_length;

if filter_name == b"ASCII85Decode" { /* direct decode */ }
if filter_name == b"ASCIIHexDecode" { /* direct decode */ }
if filter_name == b"RunLengthDecode" { /* direct decode */ }
```

Keep `use crate::ascii85;` and `use crate::ascii_hex;` for their write-side
encoders; Task 6 retains these imports permanently. Do not alter LZW,
passthrough, Predictor, Crypt, chain length, or `Cow` ownership logic.

- [ ] **Step 6: Run focused and consumer tests after deletion**

Run:

```bash
cargo test -p flpdf --lib stream_filter::tests -- --nocapture
cargo test -p flpdf --lib filters::tests -- --nocapture
cargo test -p flpdf --test multi_filter_chain_tests -- --nocapture
cargo test -p flpdf --test reader_tests
cargo test -p flpdf --test writer_tests
cargo test -p flpdf-cli --test cli_tests
```

Expected: PASS.

- [ ] **Step 7: Commit the production decoder cutover**

```bash
git add \
  crates/flpdf/src/stream_filter.rs \
  crates/flpdf/src/filters.rs \
  crates/flpdf/tests/multi_filter_chain_tests.rs
git commit -m "feat: route ASCII and RunLength decode through pipelines"
```

---

### Task 6: Cut over RunLength encoding and delete superseded helpers

**Files:**

- Modify: `crates/flpdf/src/stream_filter.rs`
- Modify: `crates/flpdf/src/filters.rs`
- Modify: `crates/flpdf/src/ascii85.rs`
- Modify: `crates/flpdf/src/ascii_hex.rs`
- Modify: `crates/flpdf/src/lib.rs`
- Delete: `crates/flpdf/src/run_length.rs`

**Interfaces:**

- Consumes: `RunLength` encode mode from Task 4 and `pipeline::buffer::Buffer`.
- Produces:

```rust
pub(crate) fn encode_run_length(data: &[u8]) -> Result<Vec<u8>>;
```

- Leaves `ascii85::encode` and `ascii_hex::encode` unchanged.

- [ ] **Step 1: Add RED production encode assertions**

In `stream_filter.rs`, add:

```rust
#[test]
fn run_length_encoder_uses_qpdf_two_byte_run() {
    assert_eq!(encode_run_length(b"AA").unwrap(), [0xff, b'A', 0x80]);
}
```

In `filters.rs`, assert the public write path produces the same bytes and
round-trips boundary fixtures of lengths 127, 128, and 129.

- [ ] **Step 2: Run encode tests to verify RED**

Run:

```bash
cargo test -p flpdf --lib \
  stream_filter::tests::run_length_encoder_uses_qpdf_two_byte_run -- --exact
cargo test -p flpdf --lib filters::tests::encode_stream_data_run_length_qpdf_packets -- --exact
```

Expected: the helper test does not compile and the public path reports the old
literal encoding for `AA`.

- [ ] **Step 3: Implement `encode_run_length` and switch the caller**

Implement beside `encode_flate`:

```rust
pub(crate) fn encode_run_length(data: &[u8]) -> Result<Vec<u8>> {
    let mut sink = Buffer::new("stream data buffer", None);
    {
        let mut stage = RunLength::new(
            "compress stream",
            &mut sink,
            RunLengthAction::Encode,
        );
        stage.write(data).map_err(map_pipeline_error)?;
        stage.finish().map_err(map_pipeline_error)?;
    }
    sink.take_buffer().map_err(map_pipeline_error)
}
```

Import it in `filters.rs` and replace only the `RunLengthDecode` encode branch
with `encode_run_length(stream_data).map_err(|error| error.to_string())`.

- [ ] **Step 4: Remove old decoders and migrate retained encoder tests**

In `ascii85.rs`, delete `decode`, `group_to_u32`, decoder-only tests, and old
decoder notes. Keep the existing encoder implementation and exact encoder
tests. Change the module header to:

```rust
//! qpdf correspondence: flpdf-specific ASCII85 encoder for PDF stream write paths; qpdf 11.9.0 has Pl_ASCII85Decoder but no matching encoder component.
```

In `ascii_hex.rs`, delete `decode`, `hex_nibble`, decoder-only tests, and old
decoder notes. Keep the encoder and its exact lowercase/EOD tests. Change the
module header to:

```rust
//! qpdf correspondence: flpdf-specific ASCIIHex encoder for PDF stream write paths; qpdf 11.9.0 has Pl_ASCIIHexDecoder but no matching encoder component.
```

Move any still-useful RunLength packet boundary assertions into
`pipeline/run_length.rs`, then delete `crates/flpdf/src/run_length.rs` and
remove `pub(crate) mod run_length;` from `lib.rs`.

- [ ] **Step 5: Prove old paths are absent**

Run:

```bash
rg -n \
  'ascii85::decode|ascii_hex::decode|run_length::(decode|encode)' \
  crates/flpdf/src
rg -n 'pub\\(crate\\) fn decode\\(' \
  crates/flpdf/src/ascii85.rs crates/flpdf/src/ascii_hex.rs
rg -n 'mod run_length' crates/flpdf/src
rg -n 'ascii85::encode|ascii_hex::encode|encode_run_length' crates/flpdf/src
```

Expected:

- first two commands: no matches;
- third command: only `pipeline.rs`;
- fourth command: ASCII encoders occur only on write/test paths and
  `encode_run_length` is the sole production RunLength encoder.

- [ ] **Step 6: Run focused and workspace tests**

Run:

```bash
cargo test -p flpdf --lib pipeline::run_length::tests -- --nocapture
cargo test -p flpdf --lib stream_filter::tests -- --nocapture
cargo test -p flpdf --lib filters::tests -- --nocapture
cargo test -p flpdf --test multi_filter_chain_tests
cargo test -p flpdf
cargo test -p flpdf-cli --test cli_tests
```

Expected: PASS.

- [ ] **Step 7: Commit the encoder cutover and deletion**

```bash
git add \
  crates/flpdf/src/stream_filter.rs \
  crates/flpdf/src/filters.rs \
  crates/flpdf/src/ascii85.rs \
  crates/flpdf/src/ascii_hex.rs \
  crates/flpdf/src/lib.rs \
  crates/flpdf/src/pipeline/run_length.rs
git add -u crates/flpdf/src/run_length.rs
git commit -m "refactor: remove whole-buffer stream codecs"
```

---

### Task 7: Add the Rust differential and require exact qpdf traces

**Files:**

- Create: `crates/flpdf/src/pipeline/stream_codecs_oracle.rs`
- Modify: `crates/flpdf/src/pipeline.rs`
- Modify: `scripts/qpdf-stream-codecs-diff.sh`

**Interfaces:**

- Consumes: the probe protocol from Task 1 and all components from Tasks 2–4.
- Produces ignored test
  `pipeline::stream_codecs_oracle::qpdf_stream_codecs_differential`.
- Produces normally run tests for case generation, local trace generation,
  exact probe arguments, probe failure reporting, and trace comparison.

- [ ] **Step 1: Write the normally run oracle-harness tests**

Declare:

```rust
#[cfg(test)]
mod stream_codecs_oracle;
```

Start the test-only module with:

```rust
//! qpdf correspondence: live differential instrumentation for Pl_ASCII85Decoder.cc, Pl_ASCIIHexDecoder.cc, and Pl_RunLength.cc.
```

Represent each case as:

```rust
struct OracleCase {
    name: &'static str,
    codec: Codec,
    fail_writes: Vec<usize>,
    fail_finishes: Vec<usize>,
    operations: Vec<Operation>,
}

enum Codec {
    Ascii85,
    AsciiHex,
    RunLengthDecode,
    RunLengthEncode,
}

enum Operation {
    Write(Vec<u8>),
    Finish,
}
```

The case list must include these named discriminators:

```text
ascii85-all-whitespace-and-nul
ascii85-split-eod
ascii85-bare-tilde-finish
ascii85-one-digit-zero-write
ascii85-low32-overflow
ascii85-flush-failure-reuse
asciihex-partial-and-eod
asciihex-output-before-error
asciihex-flush-failure-reuse
runlength-decode-eod-continues
runlength-decode-truncated-literal-reuse
runlength-decode-truncated-repeat-reuse
runlength-decode-repeat-failure
runlength-encode-two-byte-run
runlength-encode-128-boundaries
runlength-encode-payload-failure-retry
runlength-repeated-finish
```

Add an ordinary test that generates flpdf traces for every case and asserts
each has at least one operation and a stable name. Add Unix fake-probe tests
that assert exact positional arguments, propagate stderr and exit status with
the case name, reject non-UTF-8/stdout protocol corruption, and compare the
complete trace rather than output bytes alone.

- [ ] **Step 2: Run oracle-harness tests to verify RED**

Run:

```bash
cargo test -p flpdf --lib pipeline::stream_codecs_oracle -- --nocapture
```

Expected: compile failure because the module and trace runner are absent.

- [ ] **Step 3: Implement local trace and probe invocation**

Use the Task 2 `RecordingSink` format for local traces. For each case, create a
fresh component and execute operations one by one, preserving the component
after an error. Encode error messages and write bytes as lowercase hex so the
record format is byte-stable.

Invoke the external probe with:

```rust
Command::new(probe)
    .arg(case.codec.as_probe_arg())
    .arg(csv_or_dash(&case.fail_writes))
    .arg(csv_or_dash(&case.fail_finishes))
    .args(case.operations.iter().map(Operation::as_probe_arg))
    .output()
```

Require successful exit and UTF-8 ASCII protocol. Comparison is:

```rust
assert_eq!(
    flpdf_trace(&case),
    run_qpdf_probe(probe, &case),
    "case {}",
    case.name
);
```

The ignored entry point is:

```rust
#[test]
#[ignore = "live qpdf 11.9.0 stream-codec oracle"]
// cov:ignore-start: ignored live entry point; ordinary tests cover case generation, local traces, probe arguments, failures, and comparison
fn qpdf_stream_codecs_differential() {
    let probe = std::env::var_os("QPDF_STREAM_CODECS_PROBE")
        .expect("set QPDF_STREAM_CODECS_PROBE to the qpdf 11.9.0 probe");
    assert_qpdf_oracle_matches(Path::new(&probe));
}
// cov:ignore-end
```

- [ ] **Step 4: Run ordinary harness tests to verify GREEN**

Run:

```bash
cargo test -p flpdf --lib pipeline::stream_codecs_oracle -- --nocapture
```

Expected: PASS.

- [ ] **Step 5: Run live differential and resolve only demonstrated mismatches**

Run:

```bash
scripts/qpdf-stream-codecs-diff.sh
```

Expected: PASS for every exact trace. If a case fails, reduce that named case,
compare the corresponding qpdf source branch, add an always-on Rust regression
that fails for the mismatch, then change the component and rerun both the
focused regression and the full live script.

- [ ] **Step 6: Run oracle runner contract again**

Run:

```bash
bash scripts/tests/qpdf-stream-codecs-diff-contract.sh
```

Expected: PASS after the final cargo selector and environment variable are
present.

- [ ] **Step 7: Commit the differential**

```bash
git add \
  crates/flpdf/src/pipeline.rs \
  crates/flpdf/src/pipeline/stream_codecs_oracle.rs \
  scripts/qpdf-stream-codecs-diff.sh
git commit -m "test: verify stream codecs against qpdf"
```

---

### Task 8: Update correspondence docs and run every completion gate

**Files:**

- Modify: `docs/qpdf-correspondence.md`
- Modify: `docs/qpdf-module-doc-index.md`

**Interfaces:**

- Consumes: final production module layout.
- Produces: source correspondence that no longer attributes qpdf decoder
  behavior to the retained ASCII encoder-only modules.

- [ ] **Step 1: Update correspondence rows**

Replace the three old rows with these mappings:

```markdown
| `Pl_ASCII85Decoder` / `SF_ASCII85Decode` | 108 + 31 | `pipeline/ascii85.rs` + `stream_filter.rs` | ✅ |
| `Pl_ASCIIHexDecoder` / `SF_ASCIIHexDecode` | 96 + 31 | `pipeline/ascii_hex.rs` + `stream_filter.rs` | ✅ |
| `Pl_RunLength` / `SF_RunLengthDecode` | 146 + 38 | `pipeline/run_length.rs` + `stream_filter.rs` | ✅ |
```

Keep `ascii85.rs` and `ascii_hex.rs` described as flpdf-specific encoder
helpers, not qpdf component mirrors.

- [ ] **Step 2: Regenerate and verify module documentation**

Run:

```bash
python3 scripts/qpdf-module-docs.py --write
python3 -m unittest scripts/tests/test_qpdf_module_docs.py
python3 scripts/qpdf-module-docs.py --check
```

Expected: PASS and a regenerated index containing the three Pipeline modules,
with no root `run_length.rs` row.

- [ ] **Step 3: Run deletion and scope inventories**

Run:

```bash
rg -n \
  'ascii85::decode|ascii_hex::decode|run_length::(decode|encode)' \
  crates/flpdf/src
rg -n 'pub\\(crate\\) fn decode\\(' \
  crates/flpdf/src/ascii85.rs crates/flpdf/src/ascii_hex.rs
rg -n 'mod run_length' crates/flpdf/src
git diff --check main...HEAD
git diff --name-status main...HEAD
```

Expected: no old decoder/helper or whole-buffer decoder definition matches;
only the Pipeline RunLength module; no whitespace errors; only files listed by
this plan plus the approved design and plan documents.

- [ ] **Step 4: Commit documentation and plan-order correction**

`scripts/patch-coverage.sh` requires a clean worktree. Commit the generated
documentation and this executable-order correction before running any
authoritative fresh coverage:

```bash
git add \
  docs/qpdf-correspondence.md \
  docs/qpdf-module-doc-index.md \
  docs/superpowers/plans/2026-07-28-qpdf-stream-codecs-cutover.md
git commit -m "docs: map qpdf stream codec pipelines"
git status --short --branch
```

Expected: the documentation and plan correction are committed and the feature
worktree is clean.

- [ ] **Step 5: Run formatting, lint, and strict rustdoc**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" \
  cargo doc --workspace --no-deps --document-private-items
```

Expected: PASS.

- [ ] **Step 6: Run focused tests and the live oracle from a clean test state**

Run:

```bash
cargo test -p flpdf --lib pipeline::ascii85::tests
cargo test -p flpdf --lib pipeline::ascii_hex::tests
cargo test -p flpdf --lib pipeline::run_length::tests
cargo test -p flpdf --lib stream_filter::tests
cargo test -p flpdf --lib filters::tests
cargo test -p flpdf --test multi_filter_chain_tests
cargo test -p flpdf --test reader_tests
cargo test -p flpdf --test writer_tests
cargo test -p flpdf-cli --test cli_tests
cargo test -p flpdf-cli --test compat_matrix_tests
bash scripts/tests/qpdf-stream-codecs-diff-contract.sh
scripts/qpdf-stream-codecs-diff.sh
```

Expected: PASS. `compat_matrix_tests` may report its documented skip only if
the qpdf executable is unavailable; the live source differential must not
skip.

- [ ] **Step 7: Run workspace and Linux byte-parity gates**

Run:

```bash
cargo test --workspace
cargo test -p flpdf --features qpdf-zlib-compat --test zlib_compat_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_diff_zero_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_null_visibility_tests
cargo test -p flpdf --features qpdf-zlib-compat --test deterministic_id_qpdf_parity_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_generate_objstm_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_objstm_tests
cargo test -p flpdf --features qpdf-zlib-compat --lib overlay::byte_gate
cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_byte_identical
cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_byte_identical_overlay
cargo test -p flpdf-cli --features qpdf-zlib-compat --test compat_baseline_static_id -- --nocapture
cargo test -p flpdf-cli --features qpdf-zlib-compat --test compat_matrix_baseline -- --nocapture
cargo test -p flpdf-test-compare --features qpdf-zlib-compat --test e2e
```

Expected: PASS.

- [ ] **Step 8: Generate fresh coverage and require 100% changed lines**

Refresh the remote base, then run without reusing an old LCOV file:

```bash
git fetch origin main
scripts/patch-coverage.sh --base origin/main
```

Expected: fresh `cargo llvm-cov` execution and:

```text
Patch coverage: 100.00%
```

Do not use `--allow-dirty` or reuse an old LCOV file. Do not add coverage
exclusions for executable branches that can be exercised by deterministic
unit tests. The only ignored block is the external live entry point whose
logic is covered through ordinary fake-probe tests.

- [ ] **Step 9: Final clean-tree verification**

Run:

```bash
git status --short --branch
git log --oneline main..HEAD
bd show flpdf-qynx.5.2
```

Expected: clean feature worktree; only the planned commits; Bead still
`IN_PROGRESS` until the verified implementation is published or otherwise
handed off through the chosen branch-completion workflow.
