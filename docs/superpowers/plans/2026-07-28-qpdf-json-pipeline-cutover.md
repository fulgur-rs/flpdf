# qpdf JSON Pipeline Complete Cutover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace every JSON serialization and output byte path with the qpdf-shaped `Pipeline`, `PlString`, `PlConcatenate`, `PlBase64`, `PlOStream`, and `PlStdioFile` contracts, leaving the CLI unaware of pipeline stages.

**Architecture:** Layer 1 makes the pipeline contract public, adds the four JSON-core stages, cuts `Json` and raw inspection serialization over to `&mut dyn Pipeline`, and puts ordinary stdout/file handles behind a library coordinator. Layer 2 adds `PlStdioFile`, moves top-level and side-file output to qpdf's distinct close/explicit-finish lifecycles, and deletes the temporary JSON stdio implementation.

**Tech Stack:** Rust 2021; `std::io::{Write, BufWriter}` only at terminal boundaries; qpdf 11.9.0 pinned source and `/usr/bin/qpdf` behavioral oracle; Bash/C++ oracle probe; Cargo tests, Clippy, strict rustdoc, `cargo llvm-cov`, and `scripts/patch-coverage.sh`.

## Global Constraints

- Resolve qpdf with `scripts/fetch-qpdf-source.sh --print-path`; the expected pinned commit is `3b97c9bd266b7c32ea36d3536e22dab77412886d`.
- Treat the pinned qpdf source and observed qpdf 11.9.0 behavior as authoritative; do not edit or re-clone the source tree.
- This is a complete cutover; API backward compatibility has lower priority
  than qpdf responsibility parity. Do not retain Write-based JSON overloads,
  deprecated aliases, compatibility wrappers, `Base64Writer`, or
  `QpdfStdioWriter`.
- `Json::write`, every incremental JSON helper, raw inspection output, and blob callbacks use `&mut dyn Pipeline`.
- JSON serialization never calls `finish` on a caller-supplied outer pipeline.
- The CLI may own/open ordinary stdout/file handles but must not import, construct, or finish `Pipeline`, `PlString`, `PlConcatenate`, `PlBase64`, `PlOStream`, or `PlStdioFile`.
- `PlOStream` converts an underlying `io::Error` into sticky non-fatal state; subsequent write/finish calls are no-ops.
- Top-level file output relies on buffered-handle close/drop and does not explicitly finish `PlStdioFile`; side-file output explicitly finishes `PlStdioFile` before close/drop.
- Preserve already-emitted bytes on callback, conversion, pipeline, open, write, or finish failure. Never roll back or truncate a partial JSON prefix.
- Use `Base64Action::{Encode, Decode}` as the public Rust equivalent of qpdf's `Pl_Base64::action_e`.
- Remove the `base64` dependency from both workspace and `flpdf` manifests after `rg` proves no production consumer remains.
- Keep `QPDFLogger` (`flpdf-qynx.4`), page-content concatenate consumers (`flpdf-qynx.7`), other filters/crypto/hash stages, and JSON schema-content gaps out of scope.
- Every behavior change follows RED→GREEN→REFACTOR and receives a focused commit.
- Each stacked PR runs its quality and fresh 100% changed-executable-line coverage gates against its own immediate parent.

## Delivery Stack

| Layer | Branch | PR base | Patch-coverage base | Deliverable |
|---|---|---|---|---|
| 1 | `feature/flpdf-qynx-6-json-pipeline` | `main` | `origin/main` | Public core stages, JSON/raw writer cutover, library output coordinator |
| 2 | `feature/flpdf-qynx-6-json-stdio` | immutable Layer 1 remote PR head SHA | same immutable Layer 1 remote PR head SHA | `PlStdioFile`, top-level/side-file lifecycle cutover, legacy stdio deletion |

The design commit `73e13cf7` already starts Layer 1. Create Layer 2 only after
Layer 1 is committed, verified, pushed, and its exact remote PR head is
recorded.

The review-correction implementation and coverage tip is
`effca8d22e29fe29e37409432f92a803913351f0`. Fresh Layer 1 coverage at that
commit was 100%. This plan-record commit necessarily follows that commit and
therefore does not call it the final Layer 1 branch tip: a commit cannot record
its own SHA. At publication, record the immutable remote branch SHA/PR head in
Beads and the Task 9 report. Layer 2 must branch from that recorded SHA, not
from the mutable Layer 1 branch name.

If Layer 1 is rebased after verification, invalidate the previously recorded
SHA. Rerun the full Task 9 gates and fresh Layer 1 coverage on the rebased
commit, push it, record the resulting final remote SHA in Beads and the Task 9
report, and push the updated Beads state. Only then create or recreate Layer 2
from that exact immutable SHA.

## File and Responsibility Map

### Layer 1

- Modify `crates/flpdf/src/lib.rs`: make `pipeline` public.
- Modify `crates/flpdf/src/pipeline.rs`: public trait/error/result and public stage modules/re-exports.
- Create `crates/flpdf/src/pipeline/string.rs`: append/pass-through terminal.
- Create `crates/flpdf/src/pipeline/concatenate.rs`: finish-suppressing forwarding stage.
- Create `crates/flpdf/src/pipeline/base64.rs`: qpdf encode/decode state machine.
- Create `crates/flpdf/src/pipeline/ostream.rs`: sticky non-fatal `Write` terminal.
- Modify `crates/flpdf/src/json/value.rs`: pipeline blob callback type.
- Modify `crates/flpdf/src/json/writer.rs`: pipeline-native incremental writer, blob chain, and unparse.
- Modify `crates/flpdf/src/json/mod.rs`: remove substitution documentation and legacy stdio export.
- Modify `crates/flpdf/src/json_inspect.rs`: raw pipeline writer and ordinary-handle coordinator.
- Modify `crates/flpdf-cli/src/main.rs`: call the coordinator with `JsonOutput`.
- Modify `crates/flpdf/tests/json_tests.rs`: pipeline JSON contract coverage.
- Create `crates/flpdf/tests/pipeline_public_api.rs`: downstream-crate compile contract.
- Modify `crates/flpdf-cli/tests/cli_json.rs`: stdout/file and `/dev/full` parity.
- Create `tests/oracle/qpdf_json_pipeline_probe.cc`: qpdf core-stage record producer.
- Create `tests/oracle/qpdf_json_pipeline_core_records.tsv`: checked-in qpdf records.
- Create `scripts/qpdf-json-pipeline-diff.sh`: pinned-source probe builder and Rust differential runner.
- Create `scripts/tests/qpdf-json-pipeline-diff-contract.sh`: non-live harness contract.
- Modify `Cargo.toml` and `crates/flpdf/Cargo.toml`: remove `base64`.
- Regenerate `docs/qpdf-module-doc-index.md` with
  `scripts/qpdf-module-docs.py`.
- Manually update `docs/qpdf-correspondence.md`.

### Layer 2

- Create `crates/flpdf/src/pipeline/stdio_file.rs`: qpdf stdio terminal.
- Modify `crates/flpdf/src/pipeline.rs`: public module/re-export.
- Modify `crates/flpdf/src/json_inspect.rs`: `BufWriter` + `PlStdioFile` top-level and side-file paths.
- Delete `crates/flpdf/src/json/stdio.rs`: remove the duplicated stdio state machine.
- Modify `crates/flpdf/src/json/mod.rs`: remove the legacy stdio module.
- Modify `crates/flpdf-cli/tests/cli_json.rs`: top-level/side-file stdio lifecycle parity.
- Extend `tests/oracle/qpdf_json_pipeline_probe.cc`: stdio records.
- Create `tests/oracle/qpdf_json_pipeline_stdio_records.tsv`: checked-in qpdf stdio records.
- Extend `scripts/qpdf-json-pipeline-diff.sh` and its contract test for the Layer 2 ignored differential.
- Regenerate `docs/qpdf-module-doc-index.md` with
  `scripts/qpdf-module-docs.py`.
- Manually update `docs/qpdf-correspondence.md`.

---

### Task 1: Publish the Pipeline contract and add `PlString`

**Files:**
- Modify: `crates/flpdf/src/lib.rs`
- Modify: `crates/flpdf/src/pipeline.rs`
- Create: `crates/flpdf/src/pipeline/string.rs`
- Create: `crates/flpdf/tests/pipeline_public_api.rs`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/include/qpdf/Pl_String.hh`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/Pl_String.cc`

**Interfaces:**
- Produces `pub trait Pipeline`.
- Produces `pub enum PipelineError { Logic(String), Runtime(String) }`.
- Produces `pub type PipelineResult<T> = Result<T, PipelineError>`.
- Produces public `PipelineError::{logic, runtime, message}` methods.
- Produces `pub struct PlString<'a>` and `PlString::new(identifier, next, destination)`.
- `PlString` appends to `&mut Vec<u8>` before forwarding to `Option<&mut dyn Pipeline>`.

- [ ] **Step 1: Add the failing downstream-crate and unit tests**

Create `crates/flpdf/tests/pipeline_public_api.rs`:

```rust
use flpdf::pipeline::{Pipeline, PipelineError, PipelineResult, PlString};

struct ExternalSink(Vec<u8>);

impl Pipeline for ExternalSink {
    fn identifier(&self) -> &str {
        "external"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.0.extend_from_slice(data);
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}

#[test]
fn downstream_crates_can_implement_pipeline_and_construct_pl_string() {
    let mut captured = Vec::new();
    let mut sink = ExternalSink(Vec::new());
    {
        let mut stage = PlString::new("capture", Some(&mut sink), &mut captured);
        stage.write(b"payload").unwrap();
        stage.finish().unwrap();
    }
    assert_eq!(captured, b"payload");
    assert_eq!(sink.0, b"payload");
    assert_eq!(PipelineError::runtime("failure").message(), "failure");
}
```

In `pipeline/string.rs`, add unit tests named:

```rust
pl_string_appends_without_next_and_needs_no_finish
pl_string_appends_before_downstream_write_error
pl_string_forwards_empty_and_nonempty_chunks_and_finish
pl_string_propagates_downstream_finish_error
pl_string_reuse_appends_to_existing_destination
```

The append-before-error test uses a sink that records `b"prefix"` and then
returns `PipelineError::runtime("downstream rejected chunk")`; assert both the
destination and sink retain `b"prefix"`.

- [ ] **Step 2: Run the tests to verify RED**

Run:

```bash
cargo test -p flpdf --test pipeline_public_api
cargo test -p flpdf pipeline::string::tests
```

Expected: compilation fails because `pipeline` and `PlString` are not public
and `pipeline::string` does not exist.

- [ ] **Step 3: Make the contract public and implement `PlString`**

Change `lib.rs` to:

```rust
pub mod pipeline;
```

Change `pipeline.rs` visibility without broadening existing stage visibility:

```rust
pub mod string;
pub use string::PlString;

pub type PipelineResult<T> = std::result::Result<T, PipelineError>;

#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error("{0}")]
    Logic(String),
    #[error("{0}")]
    Runtime(String),
}

pub trait Pipeline {
    fn identifier(&self) -> &str;
    fn write(&mut self, data: &[u8]) -> PipelineResult<()>;
    fn finish(&mut self) -> PipelineResult<()>;
}
```

Make `PipelineError::logic`, `runtime`, and `message` public. Implement
`pipeline/string.rs` with this data flow:

```rust
pub struct PlString<'a> {
    identifier: String,
    next: Option<&'a mut dyn Pipeline>,
    destination: &'a mut Vec<u8>,
}

impl<'a> PlString<'a> {
    pub fn new(
        identifier: impl Into<String>,
        next: Option<&'a mut dyn Pipeline>,
        destination: &'a mut Vec<u8>,
    ) -> Self {
        Self {
            identifier: identifier.into(),
            next,
            destination,
        }
    }
}

impl Pipeline for PlString<'_> {
    fn identifier(&self) -> &str {
        &self.identifier
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.destination.extend_from_slice(data);
        if let Some(next) = self.next.as_deref_mut() {
            next.write(data)?;
        }
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        if let Some(next) = self.next.as_deref_mut() {
            next.finish()?;
        }
        Ok(())
    }
}
```

- [ ] **Step 4: Run focused GREEN and formatting**

Run:

```bash
cargo test -p flpdf --test pipeline_public_api
cargo test -p flpdf pipeline::string::tests
cargo fmt --all -- --check
```

Expected: all focused tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/flpdf/src/lib.rs crates/flpdf/src/pipeline.rs \
  crates/flpdf/src/pipeline/string.rs crates/flpdf/tests/pipeline_public_api.rs
git commit -m "feat(pipeline): publish contract and add PlString"
```

---

### Task 2: Add `PlConcatenate`

**Files:**
- Modify: `crates/flpdf/src/pipeline.rs`
- Create: `crates/flpdf/src/pipeline/concatenate.rs`
- Modify: `crates/flpdf/tests/pipeline_public_api.rs`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/include/qpdf/Pl_Concatenate.hh`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/Pl_Concatenate.cc`

**Interfaces:**
- Consumes public `Pipeline` and `PipelineResult`.
- Produces `pub struct PlConcatenate<'a>`.
- Produces `PlConcatenate::new(identifier, next)` and `manual_finish()`.
- Ordinary `finish()` succeeds without calling downstream; `manual_finish()`
  calls downstream exactly once per invocation.

- [ ] **Step 1: Add failing contract tests**

Add these unit tests in `pipeline/concatenate.rs`:

```rust
#[test]
fn ordinary_finish_is_suppressed_but_manual_finish_is_forwarded() {
    let mut sink = RecordingSink::default();
    {
        let mut concatenate = PlConcatenate::new("cat", &mut sink);
        concatenate.write(b"one").unwrap();
        concatenate.finish().unwrap();
        concatenate.write(b"two").unwrap();
        concatenate.manual_finish().unwrap();
    }
    assert_eq!(sink.bytes, b"onetwo");
    assert_eq!(sink.finishes, 1);
}
```

Also add:

```rust
pl_concatenate_forwards_empty_chunks
pl_concatenate_propagates_write_error_unchanged
pl_concatenate_ordinary_finish_ignores_a_failing_finish_sink
pl_concatenate_manual_finish_propagates_error_unchanged
pl_concatenate_is_reusable_after_ordinary_and_manual_finish
```

Extend the public API test to import and construct `PlConcatenate`.

- [ ] **Step 2: Run RED**

Run:

```bash
cargo test -p flpdf pipeline::concatenate::tests
cargo test -p flpdf --test pipeline_public_api
```

Expected: compilation fails because `PlConcatenate` does not exist.

- [ ] **Step 3: Implement the forwarding/suppression contract**

Create:

```rust
pub struct PlConcatenate<'a> {
    identifier: String,
    next: &'a mut dyn Pipeline,
}

impl<'a> PlConcatenate<'a> {
    pub fn new(identifier: impl Into<String>, next: &'a mut dyn Pipeline) -> Self {
        Self {
            identifier: identifier.into(),
            next,
        }
    }

    pub fn manual_finish(&mut self) -> PipelineResult<()> {
        self.next.finish()
    }
}

impl Pipeline for PlConcatenate<'_> {
    fn identifier(&self) -> &str {
        &self.identifier
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.next.write(data)
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}
```

Export it from `pipeline.rs`.

- [ ] **Step 4: Run GREEN**

Run:

```bash
cargo test -p flpdf pipeline::concatenate::tests
cargo test -p flpdf --test pipeline_public_api
```

Expected: all tests pass and error values retain their original category and
message.

- [ ] **Step 5: Commit**

```bash
git add crates/flpdf/src/pipeline.rs \
  crates/flpdf/src/pipeline/concatenate.rs \
  crates/flpdf/tests/pipeline_public_api.rs
git commit -m "feat(pipeline): add PlConcatenate finish suppression"
```

---

### Task 3: Add qpdf-exact `PlBase64`

**Files:**
- Modify: `crates/flpdf/src/pipeline.rs`
- Create: `crates/flpdf/src/pipeline/base64.rs`
- Modify: `crates/flpdf/tests/pipeline_public_api.rs`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/qpdf/Pl_Base64.hh`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/Pl_Base64.cc`

**Interfaces:**
- Produces `pub enum Base64Action { Encode, Decode }`.
- Produces `pub struct PlBase64<'a>`.
- Produces `PlBase64::new(identifier, next, action)`.
- Uses only an internal `[u8; 4]`, `position`, `end_of_data`, and `finished`;
  no crate Base64 engine is used.
- Exact errors:
  - `Logic("Pl_Base64 used after finished")`
  - `Runtime("{identifier}: base64 decode: invalid input")`
  - `Runtime("{identifier}: base64 decode: data follows pad characters")`

- [ ] **Step 1: Add the failing encode/decode matrix**

Add a `RecordingSink` helper and table-driven tests with these exact records:

```rust
let encode_cases: &[(&[&[u8]], &[u8])] = &[
    (&[b""], b""),
    (&[b"\x00"], b"AA=="),
    (&[b"\x00\xff"], b"AP8="),
    (&[b"\x00\xff\x10"], b"AP8Q"),
    (&[b"\x00", b"\xff", b"\x10\x20"], b"AP8QIA=="),
    (&[b"Man"], b"TWFu"),
];

let decode_cases: &[(&[&[u8]], &[u8])] = &[
    (&[b"TWFu"], b"Man"),
    (&[b"T", b"W", b"Fu"], b"Man"),
    (&[b" TQ==\r\n"], b"M"),
    (&[b"-\n_8="], b"\xfb\xff"),
    (&[b"TQ"], b"M"),
];
```

Add lifecycle/error tests named:

```rust
decode_rejects_invalid_input_with_exact_identifier
decode_rejects_data_after_padding_after_preserving_prior_output
decode_allows_whitespace_after_padding
decode_finish_rejects_a_single_symbol_quantum
write_after_finish_is_logic_error
repeated_finish_with_no_pending_data_finishes_downstream_each_time
finish_write_failure_retains_quantum_for_a_retry
finish_failure_from_downstream_leaves_stage_finished
split_writes_match_single_write_at_every_boundary
empty_write_does_not_change_state
```

For `decode_rejects_data_after_padding_after_preserving_prior_output`, write
`b"TQ==AAAA"` and assert the sink contains only `b"M"` when the exact
`data follows pad characters` error is returned.

- [ ] **Step 2: Run RED**

Run:

```bash
cargo test -p flpdf pipeline::base64::tests -- --nocapture
cargo test -p flpdf --test pipeline_public_api
```

Expected: compilation fails because `Base64Action` and `PlBase64` do not exist.

- [ ] **Step 3: Implement the qpdf state machine**

Use this exact state shape:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Base64Action {
    Encode,
    Decode,
}

pub struct PlBase64<'a> {
    identifier: String,
    next: &'a mut dyn Pipeline,
    action: Base64Action,
    buffer: [u8; 4],
    position: usize,
    end_of_data: bool,
    finished: bool,
}
```

Implement ASCII-space recognition as:

```rust
fn is_qpdf_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}
```

Implement the alphabet mapping directly:

```rust
fn decode_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' | b'-' => Some(62),
        b'/' | b'_' => Some(63),
        _ => None,
    }
}

fn encode_value(value: u8) -> u8 {
    match value {
        0..=25 => b'A' + value,
        26..=51 => b'a' + value - 26,
        52..=61 => b'0' + value - 52,
        62 => b'+',
        63 => b'/',
        _ => unreachable!("six-bit value"),
    }
}
```

Match qpdf's finish ordering exactly:

```rust
fn finish(&mut self) -> PipelineResult<()> {
    if self.position > 0 {
        if self.finished {
            return Err(PipelineError::logic("Pl_Base64 used after finished"));
        }
        if self.action == Base64Action::Decode {
            self.buffer[self.position..].fill(b'=');
        }
        self.flush_quantum()?;
    }
    self.finished = true;
    self.next.finish()
}
```

`flush_quantum` calls the selected encode/decode flush first and resets
`position` and `buffer` only after that call succeeds. `write` checks
`finished` before accepting even an empty input.

- [ ] **Step 4: Run GREEN and mutation checks**

Run:

```bash
cargo test -p flpdf pipeline::base64::tests -- --nocapture
cargo test -p flpdf --test pipeline_public_api
```

Then temporarily invert the `b'-' => 62` alias and run the Base64 tests.
Expected: `decode_cases` fails. Restore the production mapping and rerun GREEN.

- [ ] **Step 5: Commit**

```bash
git add crates/flpdf/src/pipeline.rs \
  crates/flpdf/src/pipeline/base64.rs \
  crates/flpdf/tests/pipeline_public_api.rs
git commit -m "feat(pipeline): add qpdf-compatible PlBase64"
```

---

### Task 4: Add sticky non-fatal `PlOStream`

**Files:**
- Modify: `crates/flpdf/src/pipeline.rs`
- Create: `crates/flpdf/src/pipeline/ostream.rs`
- Modify: `crates/flpdf/tests/pipeline_public_api.rs`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/include/qpdf/Pl_OStream.hh`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/Pl_OStream.cc`

**Interfaces:**
- Produces `pub struct PlOStream<'a>`.
- Produces `PlOStream::new(identifier, writer: &mut dyn Write)`.
- Does not own or close the writer.
- Returns `Ok(())` on underlying write/flush failure, marks itself failed, and
  makes later write/finish calls no-ops.

- [ ] **Step 1: Add failing terminal tests**

Use a scripted writer that records accepted bytes, write calls, and flush
calls. Add:

```rust
#[test]
fn writer_error_is_sticky_and_nonfatal() {
    let mut writer = ScriptedWriter::new([
        WriteStep::Accept(2),
        WriteStep::Error(io::ErrorKind::Other),
    ]);
    {
        let mut stage = PlOStream::new("ostream", &mut writer);
        assert!(stage.write(b"abcd").is_ok());
        assert!(stage.write(b"later").is_ok());
        assert!(stage.finish().is_ok());
    }
    assert_eq!(writer.bytes, b"ab");
    assert_eq!(writer.flush_calls, 0);
}
```

Also add:

```rust
successful_writes_and_repeated_finish_reuse_the_external_writer
flush_error_becomes_sticky_and_nonfatal
empty_write_after_failure_is_a_noop
dropping_pl_ostream_does_not_flush_or_close
```

The successful test writes `b"ab"`, finishes, writes `b"cd"`, finishes again,
and asserts `b"abcd"` plus two flush calls.

- [ ] **Step 2: Run RED**

Run:

```bash
cargo test -p flpdf pipeline::ostream::tests
cargo test -p flpdf --test pipeline_public_api
```

Expected: compilation fails because `PlOStream` does not exist.

- [ ] **Step 3: Implement the sticky adapter**

```rust
pub struct PlOStream<'a> {
    identifier: String,
    writer: &'a mut dyn Write,
    failed: bool,
}

impl<'a> PlOStream<'a> {
    pub fn new(identifier: impl Into<String>, writer: &'a mut dyn Write) -> Self {
        Self {
            identifier: identifier.into(),
            writer,
            failed: false,
        }
    }
}

impl Pipeline for PlOStream<'_> {
    fn identifier(&self) -> &str {
        &self.identifier
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        if !self.failed && self.writer.write_all(data).is_err() {
            self.failed = true;
        }
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        if !self.failed && self.writer.flush().is_err() {
            self.failed = true;
        }
        Ok(())
    }
}
```

Export `PlOStream` from `pipeline.rs`.

- [ ] **Step 4: Run GREEN**

Run:

```bash
cargo test -p flpdf pipeline::ostream::tests
cargo test -p flpdf --test pipeline_public_api
```

Expected: all terminal and public API tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/flpdf/src/pipeline.rs \
  crates/flpdf/src/pipeline/ostream.rs \
  crates/flpdf/tests/pipeline_public_api.rs
git commit -m "feat(pipeline): add sticky PlOStream terminal"
```

---

### Task 5: Add the pinned qpdf core-stage oracle harness

**Files:**
- Create: `tests/oracle/qpdf_json_pipeline_probe.cc`
- Create: `tests/oracle/qpdf_json_pipeline_core_records.tsv`
- Create: `scripts/qpdf-json-pipeline-diff.sh`
- Create: `scripts/tests/qpdf-json-pipeline-diff-contract.sh`
- Modify: `crates/flpdf/tests/pipeline_public_api.rs`
- Reference: `scripts/qpdf-rc4-diff.sh`
- Reference: `scripts/tests/qpdf-rc4-diff-contract.sh`

**Interfaces:**
- The C++ probe accepts `core` and emits tab-separated records.
- Each record is `case`, `status`, `bytes_hex`, `write_count`,
  `finish_count`. `write_count` is the number of calls made to the stage
  under test; `finish_count` is the number of downstream finish/terminal
  flush events observed.
- `QPDF_JSON_PIPELINE_PROBE` names the compiled probe for an ignored Rust
  differential test.
- The checked-in records are exercised without qpdf installed or built.

- [ ] **Step 1: Add a failing checked-record and ignored-live Rust test**

In `pipeline_public_api.rs`, add:

```rust
fn rust_core_records() -> String {
    [
        "string-null\tok\t6162\t1\t0",
        "string-tee-error\tdownstream rejected chunk\t6162\t1\t0",
        "concatenate-finish\tok\t6f6e6574776f\t2\t1",
        "base64-encode-split\tok\t4150385149413d3d\t3\t1",
        "base64-decode-alias\tok\tfbff\t1\t1",
        "base64-data-after-pad\tbase64: base64 decode: data follows pad characters\t4d\t1\t0",
        "ostream-sticky\tok\t6162\t1\t0",
    ]
    .join("\n")
        + "\n"
}

#[test]
fn checked_qpdf_core_records_match_rust() {
    assert_eq!(
        rust_core_records(),
        include_str!("../../../tests/oracle/qpdf_json_pipeline_core_records.tsv")
    );
}

#[test]
#[ignore = "live pinned qpdf 11.9.0 JSON pipeline oracle"]
fn live_qpdf_core_records_match_rust() {
    let probe = std::env::var("QPDF_JSON_PIPELINE_PROBE").unwrap();
    let output = std::process::Command::new(probe).arg("core").output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8(output.stdout).unwrap(), rust_core_records());
}
```

Implement `rust_core_records` through the actual public stages; do not return
the literal array as production test behavior.

- [ ] **Step 2: Run RED**

Run:

```bash
cargo test -p flpdf --test pipeline_public_api checked_qpdf_core_records_match_rust
bash scripts/tests/qpdf-json-pipeline-diff-contract.sh
```

Expected: the Rust test fails because the TSV is absent and the shell command
fails because the harness does not exist.

- [ ] **Step 3: Implement the C++ probe and checked records**

The probe constructs qpdf `Pl_String`, `Pl_Concatenate`, `Pl_Base64`, and
`Pl_OStream` chains for the seven named records. Use:

```cpp
std::cout << case_name << '\t' << status << '\t' << hex(bytes) << '\t'
          << write_count << '\t' << finish_count << '\n';
```

Use a throwing downstream pipeline for `string-tee-error`, a recording
pipeline for concatenate/Base64, and a custom `std::streambuf` that accepts
two bytes then returns EOF for `ostream-sticky`.

Write the probe's exact `core` output to
`tests/oracle/qpdf_json_pipeline_core_records.tsv`.

- [ ] **Step 4: Implement the safe pinned-source runner and contract**

Base the runner's source-pin, clean-tree, private temporary-directory,
symlink-swap, cleanup, and compile/link verification on
`scripts/qpdf-rc4-diff.sh`. It must:

```bash
qpdf_source="$("${repo_root}/scripts/fetch-qpdf-source.sh" --print-path)"
qpdf_commit="3b97c9bd266b7c32ea36d3536e22dab77412886d"
QPDF_JSON_PIPELINE_PROBE="${probe}" \
  cargo test -p flpdf --test pipeline_public_api \
  live_qpdf_core_records_match_rust -- --ignored --exact
```

The contract script supplies fake `git`, `mktemp`, compiler/linker, and cargo
commands and asserts:

- wrong qpdf commit is rejected;
- dirty source before and after compilation is rejected;
- a symlink-swapped temp leaf is never cleaned;
- the compiler receives the probe path and pinned qpdf include/library paths;
- cargo receives the exact ignored-test selector and probe environment;
- compiler, probe, and cargo failures propagate;
- validated private temp output is removed.

- [ ] **Step 5: Run GREEN**

Run:

```bash
cargo test -p flpdf --test pipeline_public_api checked_qpdf_core_records_match_rust
bash scripts/tests/qpdf-json-pipeline-diff-contract.sh
scripts/qpdf-json-pipeline-diff.sh
```

Expected: checked records, harness contract, and live qpdf differential all
pass; the pinned qpdf tree remains clean.

- [ ] **Step 6: Commit**

```bash
git add tests/oracle/qpdf_json_pipeline_probe.cc \
  tests/oracle/qpdf_json_pipeline_core_records.tsv \
  scripts/qpdf-json-pipeline-diff.sh \
  scripts/tests/qpdf-json-pipeline-diff-contract.sh \
  crates/flpdf/tests/pipeline_public_api.rs
git commit -m "test(pipeline): add qpdf JSON stage oracle"
```

---

### Task 6: Atomically cut `Json` and raw inspection serialization over to Pipeline

**Files:**
- Modify: `crates/flpdf/src/json/value.rs`
- Modify: `crates/flpdf/src/json/writer.rs`
- Modify: `crates/flpdf/src/json/mod.rs`
- Modify: `crates/flpdf/src/json_inspect.rs`
- Modify: `crates/flpdf/tests/json_tests.rs`
- Verify: `crates/flpdf/tests/json_parse_tests.rs`
- Verify: `crates/flpdf/tests/json_handler_tests.rs`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/JSON.cc:182-212`

**Interfaces:**
- Consumes `Pipeline`, `PipelineResult`, `PlString`, `PlConcatenate`,
  `PlBase64`, and `Base64Action`.
- Changes every `Json::write_*` helper's first parameter to
  `out: &mut dyn Pipeline`, retains its other parameters, and returns
  `PipelineResult<()>`.
- Changes `Json::write(&self, &mut dyn Pipeline, usize)`.
- Changes `Json::unparse(&self) -> PipelineResult<Vec<u8>>`.
- Changes `Json::make_blob` callback to
  `Fn(&mut dyn Pipeline) -> PipelineResult<()> + 'static`.
- Changes the public raw writer to:

```rust
pub fn write_qpdf_json_v2_selected_objects_with_options<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    decode_level: DecodeLevel,
    stream_mode: &StreamDataMode,
    keys: &[JsonKey],
    objects: &[JsonObjectSelector],
    out: &mut dyn Pipeline,
) -> Result<(), JsonOutputError>
```

- Adds `JsonOutputError::Pipeline(#[from] PipelineError)`.
- Keeps `JsonOutputError::SideFileIo` and the temporary
  `QpdfStdioWriter` only for Layer 1 side files.

- [ ] **Step 1: Rewrite JSON tests for the wished-for Pipeline API**

In `json_tests.rs`, replace `Vec<u8>` sinks with `PlString` and replace
`io::Error` callbacks with `PipelineError`. Add:

```rust
#[test]
fn blob_callback_receives_pipeline_and_outer_pipeline_is_not_finished() {
    let blob = Json::make_blob(|out| {
        out.write(b"\x01")?;
        out.write(b"\x02\x03\x04")
    });
    let mut sink = RecordingPipeline::default();
    blob.write(&mut sink, 0).unwrap();
    assert_eq!(sink.bytes, b"\"AQIDBA==\"");
    assert_eq!(sink.finishes, 0);
}

#[test]
fn blob_callback_failure_keeps_prefix_without_tail_or_closing_quote() {
    let blob = Json::make_blob(|out| {
        out.write(b"\x01")?;
        Err(PipelineError::runtime("blob callback failure"))
    });
    let mut sink = RecordingPipeline::default();
    let error = blob.write(&mut sink, 0).unwrap_err();
    assert_eq!(error.message(), "blob callback failure");
    assert_eq!(sink.bytes, b"\"");
    assert_eq!(sink.finishes, 0);
}
```

Retain and convert all existing exact-byte, split-callback, shared-mutation,
partial-output, and no-flush tests. Add:

```rust
unparse_uses_pl_string_without_finishing
blob_base64_finish_failure_keeps_open_quote_and_complete_groups
json_write_propagates_pipeline_logic_and_runtime_categories
```

- [ ] **Step 2: Add the raw-writer pipeline tests**

Inside `json_inspect.rs` tests, add:

```rust
fn write_selected_to_vec<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    decode_level: DecodeLevel,
    stream_mode: &StreamDataMode,
    keys: &[JsonKey],
    objects: &[JsonObjectSelector],
) -> Result<Vec<u8>, JsonOutputError> {
    let mut bytes = Vec::new();
    {
        let mut output = PlString::new("json test output", None, &mut bytes);
        write_qpdf_json_v2_selected_objects_with_options(
            pdf,
            decode_level,
            stream_mode,
            keys,
            objects,
            &mut output,
        )?;
    }
    Ok(bytes)
}
```

Convert every direct `Vec<u8>` main-output call to this helper or an explicit
`PlString`. Add:

```rust
raw_writer_does_not_finish_supplied_pipeline
raw_writer_retains_prefix_on_pipeline_runtime_error
raw_writer_retains_pipeline_error_category
```

Use a sink that fails on the write beginning with `b"\"parameters\""` and
assert the prefix is exactly `b"{\n  \"version\": 2,\n  "`.

- [ ] **Step 3: Run the combined RED**

Run:

```bash
cargo test -p flpdf --test json_tests -- --nocapture
cargo test -p flpdf json_inspect::tests -- --nocapture
```

Expected: compilation fails because both the public `Json` API and raw writer
still accept `std::io::Write`.

- [ ] **Step 4: Change the value/writer interfaces and blob chain**

In `value.rs`:

```rust
type BlobWriter = Rc<dyn Fn(&mut dyn Pipeline) -> PipelineResult<()>>;

pub fn make_blob(
    callback: impl Fn(&mut dyn Pipeline) -> PipelineResult<()> + 'static,
) -> Self {
    Self::with_value(Value::Blob(Rc::new(callback)))
}
```

In `writer.rs`, replace every `write_all(bytes)` with `write(bytes)` and use:

```rust
pub fn unparse(&self) -> PipelineResult<Vec<u8>> {
    let mut bytes = Vec::new();
    {
        let mut output = PlString::new("unparse", None, &mut bytes);
        self.write(&mut output, 0)?;
    }
    Ok(bytes)
}
```

Implement blobs with the exact qpdf ownership chain:

```rust
out.write(b"\"")?;
{
    let mut concatenate = PlConcatenate::new("blob concatenate", out);
    let mut base64 = PlBase64::new(
        "blob base64",
        &mut concatenate,
        Base64Action::Encode,
    );
    writer(&mut base64)?;
    base64.finish()?;
}
out.write(b"\"")
```

Delete `Base64Writer`, its constants, its `Write` implementation, and the
JSON module comment that describes Pipeline stages as substitutions.

- [ ] **Step 5: Migrate the complete raw main-output call graph**

Change every main JSON output parameter in `json_inspect.rs` from:

```rust
out: &mut (impl Write + ?Sized)
```

to:

```rust
out: &mut dyn Pipeline
```

This includes `emit_section`, qpdf metadata/object writers, non-file and
file-mode entry writers, and the public raw writer. Convert inline stream blob
callbacks from `sink.write_all(&bytes)` to `sink.write(&bytes)`.

Keep the temporary side-file generic separate:

```rust
fn write_file_mode_stream_value<R: Read + Seek, W: Write>(
    side_file: &mut W,
    out: &mut dyn Pipeline,
) -> Result<(), JsonOutputError>
```

Add:

```rust
#[error(transparent)]
Pipeline(#[from] PipelineError),
```

- [ ] **Step 6: Run combined GREEN**

Run:

```bash
cargo test -p flpdf --test json_tests -- --nocapture
cargo test -p flpdf json_inspect::tests -- --nocapture
cargo test -p flpdf --test json_parse_tests
cargo test -p flpdf --test json_handler_tests
cargo test -p flpdf --test json_schema_tests
```

Expected: the library compiles atomically and every JSON byte/error test
passes.

- [ ] **Step 7: Commit**

```bash
git add crates/flpdf/src/json/mod.rs crates/flpdf/src/json/value.rs \
  crates/flpdf/src/json/writer.rs crates/flpdf/src/json_inspect.rs \
  crates/flpdf/tests/json_tests.rs
git commit -m "refactor(json): cut serialization over to Pipeline"
```

---

### Task 7: Remove obsolete JSON error/dependency surfaces

**Files:**
- Modify: `crates/flpdf/src/json/value.rs`
- Modify: `crates/flpdf/src/json_inspect.rs`
- Modify: `Cargo.toml`
- Modify: `crates/flpdf/Cargo.toml`
- Verify: all files under `crates/flpdf/src/json/`

**Interfaces:**
- Deletes `JsonError::Io`.
- Deletes `JsonOutputError::Io`; main output now uses
  `JsonOutputError::Pipeline`, while side-file open errors use
  `JsonOutputError::SideFileIo`.
- Deletes the `base64` workspace/crate dependency.
- Produces a clean low-level audit with no Write-based main JSON serializer.

- [ ] **Step 1: Prove only obsolete definitions/manifests remain**

Run:

```bash
rg -n 'JsonError::Io|\\bIo\\(\\#\\[from\\] io::Error\\)|JsonOutputError::Io' \
  crates/flpdf/src crates/flpdf/tests
rg -n '\\bbase64\\b|Base64Writer|Engine as' \
  crates/flpdf/src Cargo.toml crates/flpdf/Cargo.toml
```

Expected: the first command finds only the two obsolete enum variants and
tests that must change to `Pipeline`; the second finds only manifest entries.

- [ ] **Step 2: Delete the obsolete variants and dependency**

Remove:

```rust
#[error(transparent)]
Io(#[from] io::Error),
```

from `JsonError`, remove the equivalent `JsonOutputError` variant, remove
`base64 = "0.22"` from workspace dependencies, and remove
`base64.workspace = true` from `crates/flpdf/Cargo.toml`.

Update any error assertions identified in Step 1 to match:

```rust
JsonOutputError::Pipeline(PipelineError::Runtime(message))
```

- [ ] **Step 3: Run GREEN and the complete-cutover audit**

Run:

```bash
cargo test -p flpdf --test json_tests
cargo test -p flpdf json_inspect::tests
cargo test -p flpdf --test json_parse_tests
! rg -n 'Base64Writer|base64::|base64\\.workspace|JsonError::Io|JsonOutputError::Io' \
  crates/flpdf/src Cargo.toml crates/flpdf/Cargo.toml
! rg -n 'out: &mut \\(impl Write|out: &mut dyn Write' \
  crates/flpdf/src/json/writer.rs
```

Expected: tests pass and both forbidden-symbol audits are empty.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml crates/flpdf/Cargo.toml \
  crates/flpdf/src/json/value.rs crates/flpdf/src/json_inspect.rs \
  crates/flpdf/tests
git commit -m "refactor(json): remove obsolete Write surfaces"
```

---

### Task 8: Add the library output coordinator and remove Pipeline from CLI

**Files:**
- Modify: `crates/flpdf/src/json_inspect.rs`
- Modify: `crates/flpdf-cli/src/main.rs`
- Modify: `crates/flpdf-cli/tests/cli_json.rs`

**Interfaces:**
- Produces:

```rust
pub enum JsonOutput<'a> {
    Stdout(&'a mut dyn Write),
    File(&'a mut dyn Write),
}
```

- Produces:

```rust
pub fn write_qpdf_json_v2_selected_objects_to_output_with_options<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    decode_level: DecodeLevel,
    stream_mode: &StreamDataMode,
    keys: &[JsonKey],
    objects: &[JsonObjectSelector],
    output: JsonOutput<'_>,
) -> Result<(), JsonOutputError>
```

- The coordinator creates `PlOStream` internally in Layer 1.
- `Stdout` calls `PlOStream::finish` at the command boundary; `File` does not
  finish the stage.
- CLI supplies only the ordinary handle and `JsonOutput` variant.

- [ ] **Step 1: Add failing coordinator and CLI parity tests**

In `json_inspect.rs` tests, add:

```rust
coordinator_stdout_matches_raw_pipeline_bytes
coordinator_file_matches_raw_pipeline_bytes
coordinator_stdout_flushes_at_command_boundary
coordinator_file_does_not_explicitly_flush
coordinator_ostream_failure_is_nonfatal_and_preserves_prefix
```

The flush probe asserts one flush for `Stdout` and zero for `File`.

In `cli_json.rs`, add Linux-only tests:

```rust
#[cfg(target_os = "linux")]
#[test]
fn json_stdout_to_dev_full_matches_qpdf_success() {
    use std::process::Stdio;

    if !is_qpdf_available() {
        return;
    }
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let qpdf = ShellCommand::new("qpdf")
        .args(["--json=2"])
        .arg(input.path())
        .stdout(Stdio::from(File::create("/dev/full").unwrap()))
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--json=2"])
        .arg(input.path())
        .stdout(Stdio::from(File::create("/dev/full").unwrap()))
        .output()
        .unwrap();
    assert!(qpdf.status.success(), "{qpdf:?}");
    assert!(flpdf.status.success(), "{flpdf:?}");
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[cfg(target_os = "linux")]
#[test]
fn json_output_dev_full_matches_qpdf_success() {
    if !is_qpdf_available() {
        return;
    }
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let args = ["--json=2", "--json-output=/dev/full"];
    let qpdf = ShellCommand::new("qpdf")
        .args(args)
        .arg(input.path())
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .args(args)
        .arg(input.path())
        .output()
        .unwrap();
    assert!(qpdf.status.success(), "{qpdf:?}");
    assert!(flpdf.status.success(), "{flpdf:?}");
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}
```

- [ ] **Step 2: Run RED**

Run:

```bash
cargo test -p flpdf json_inspect::tests::coordinator -- --nocapture
cargo test -p flpdf-cli --test cli_json json_stdout_to_dev_full_matches_qpdf_success
```

Expected: compilation fails because `JsonOutput` and the coordinator do not
exist.

- [ ] **Step 3: Implement the Layer 1 coordinator**

Implement:

```rust
pub fn write_qpdf_json_v2_selected_objects_to_output_with_options<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    decode_level: DecodeLevel,
    stream_mode: &StreamDataMode,
    keys: &[JsonKey],
    objects: &[JsonObjectSelector],
    output: JsonOutput<'_>,
) -> Result<(), JsonOutputError> {
    match output {
        JsonOutput::Stdout(writer) => {
            let mut terminal = PlOStream::new("json output", writer);
            write_qpdf_json_v2_selected_objects_with_options(
                pdf, decode_level, stream_mode, keys, objects, &mut terminal,
            )?;
            terminal.finish()?;
            Ok(())
        }
        JsonOutput::File(writer) => {
            let mut terminal = PlOStream::new("json output", writer);
            write_qpdf_json_v2_selected_objects_with_options(
                pdf, decode_level, stream_mode, keys, objects, &mut terminal,
            )
        }
    }
}
```

The Layer 2 task replaces only the `File` terminal construction.

- [ ] **Step 4: Migrate CLI to the coordinator**

Import only:

```rust
write_qpdf_json_v2_selected_objects_to_output_with_options, JsonOutput
```

For a top-level file pass `JsonOutput::File(&mut file)`. For stdout pass
`JsonOutput::Stdout(&mut locked)`. Delete the CLI's direct `locked.flush()`
and `JsonOutputError` combination logic.

- [ ] **Step 5: Run GREEN and source audit**

Run:

```bash
cargo test -p flpdf json_inspect::tests -- --nocapture
cargo test -p flpdf-cli --test cli_json
! rg -n 'Pipeline|PlString|PlConcatenate|PlBase64|PlOStream|PlStdioFile' \
  crates/flpdf-cli/src
```

Expected: JSON library and CLI tests pass; CLI source contains no pipeline
stage name.

- [ ] **Step 6: Commit**

```bash
git add crates/flpdf/src/json_inspect.rs \
  crates/flpdf-cli/src/main.rs crates/flpdf-cli/tests/cli_json.rs
git commit -m "refactor(cli): delegate JSON terminals to library"
```

---

### Task 9: Document, verify, and publish Layer 1

**Files:**
- Modify: module docs in every changed/new Rust module
- Regenerate: `docs/qpdf-module-doc-index.md`
- Manually update: `docs/qpdf-correspondence.md`
- Modify: `docs/superpowers/plans/2026-07-28-qpdf-json-pipeline-cutover.md`

**Interfaces:**
- Produces a reviewable Layer 1 tip with no Write-based JSON serializer and
  no external Base64 dependency.
- Records the verified implementation/coverage commit in this plan.
- Records the final immutable Layer 1 remote SHA/PR head externally at
  publication for the Layer 2 branch and coverage base.

- [ ] **Step 1: Update correspondence annotations and public docs**

Every new module starts with a `//! qpdf correspondence:` line naming its
qpdf source responsibility. Update `json/mod.rs`, `json/writer.rs`, and
`json_inspect.rs` docs to state that callers own the outer finish boundary and
that the CLI-facing coordinator accepts ordinary handles.

`scripts/qpdf-module-docs.py` generates
`docs/qpdf-module-doc-index.md` from the module-level
`//! qpdf correspondence:` annotations. It does not generate
`docs/qpdf-correspondence.md`; that file is the manually curated
responsibility/status table and must be reviewed separately against the
current implementation.

Run:

```bash
python3 scripts/qpdf-module-docs.py --write
PYTHONDONTWRITEBYTECODE=1 \
  python3 -m unittest scripts/tests/test_qpdf_module_docs.py
python3 scripts/qpdf-module-docs.py --check
```

Expected: 55 checker tests pass, the generated module index is current, and
the manually curated correspondence table has been reviewed against the Layer
1 implementation.

- [ ] **Step 2: Run Layer 1 focused and workspace gates**

Run:

```bash
cargo fmt --all -- --check
cargo test -p flpdf --test pipeline_public_api
cargo test -p flpdf --test json_tests
cargo test -p flpdf json_inspect::tests
cargo test -p flpdf-cli --test cli_json
bash scripts/tests/qpdf-json-pipeline-diff-contract.sh
scripts/qpdf-json-pipeline-diff.sh
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" \
  cargo doc --workspace --no-deps --document-private-items
```

Expected: every command exits zero.

- [ ] **Step 3: Commit docs and any test-only coverage correction**

```bash
git status --short
git add docs/qpdf-module-doc-index.md docs/qpdf-correspondence.md \
  docs/superpowers/plans/2026-07-28-qpdf-json-pipeline-cutover.md \
  crates/flpdf/src/json/mod.rs crates/flpdf/src/json/writer.rs \
  crates/flpdf/src/json_inspect.rs
git diff --cached --name-only
git diff --cached --check
git commit -m "docs(pipeline): record JSON stage correspondence"
```

If `git status --short` shows a focused test-only coverage correction, inspect
it and run a separate `git add` naming every intended file path explicitly
before the cached-diff checks above. Never pass a directory, glob, `scripts`,
or `git add -u`; leave every unrelated path unstaged. If the index is empty,
do not create an empty commit.

- [ ] **Step 4: Obtain fresh committed Layer 1 patch coverage**

Run:

```bash
cargo llvm-cov clean --workspace
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail \
  --lcov --output-path target/qynx-6-layer1.lcov
scripts/patch-coverage.sh --base origin/main \
  --lcov target/qynx-6-layer1.lcov
```

Expected: changed executable lines under the enforced paths are exactly 100%.
If a line is uncovered, add a focused behavioral test, commit it, regenerate
the LCOV file from the new committed tip, and rerun the gate. Commit a
test-only coverage correction before the documentation record commit, then
record that verified implementation/coverage commit SHA in the documentation
record. Do not attempt to record a commit's own SHA inside itself.

- [ ] **Step 5: Push and externally record the exact Layer 1 tip**

Run:

```bash
git status --short
git push origin feature/flpdf-qynx-6-json-pipeline
git fetch origin feature/flpdf-qynx-6-json-pipeline
layer1_tip="$(git rev-parse HEAD)"
test "${layer1_tip}" = \
  "$(git rev-parse origin/feature/flpdf-qynx-6-json-pipeline)"
bd update flpdf-qynx.6 --append-notes \
  "Layer 1 immutable remote SHA/PR head: ${layer1_tip}"
# Manually record the same layer1_tip in the Task 9 report.
bd dolt push
```

Expected: status is clean and local Layer 1 equals its remote branch. Record
that immutable remote SHA/PR head in Beads and the Task 9 report before
`bd dolt push`; then confirm the Beads push succeeds. Those external records,
rather than self-referential tracked prose, define the exact Layer 1 base for
Layer 2.

- [ ] **Step 6: Open the Layer 1 draft PR**

Run:

```bash
gh pr create --draft \
  --base main \
  --head feature/flpdf-qynx-6-json-pipeline \
  --title "Pipeline-native qpdf JSON core cutover" \
  --body "Implements flpdf-qynx.6 Layer 1: public JSON Pipeline stages, Pipeline-native Json/raw inspection serialization, qpdf oracle records, and library-owned stdout/file coordination. Layer 2 will stack PlStdioFile and side-file lifecycle removal on this PR."
```

Expected: a draft PR URL is returned. Do not mark it ready until Layer 1
review and CI are green.

---

### Task 10: Create Layer 2 and add `PlStdioFile`

**Files:**
- Modify: `crates/flpdf/src/pipeline.rs`
- Create: `crates/flpdf/src/pipeline/stdio_file.rs`
- Modify: `crates/flpdf/tests/pipeline_public_api.rs`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/include/qpdf/Pl_StdioFile.hh`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/Pl_StdioFile.cc`

**Interfaces:**
- Produces `pub struct PlStdioFile<'a>`.
- Produces `PlStdioFile::new(identifier, writer: &mut dyn Write)`.
- Direct writes loop over partial progress, retry `Interrupted`, and convert
  zero/error to `PipelineError::Runtime`.
- `finish` converts only raw `EBADF` (`9`) to exact `PipelineError::Logic`
  and ignores every other flush error.
- A 4096-capacity `BufWriter` supplied by the output coordinator represents
  C stdio buffering; `PlStdioFile` itself does not own or close the handle.

- [ ] **Step 1: Branch from the recorded Layer 1 tip**

Run:

```bash
git status --short
layer1_tip=<immutable SHA recorded in Beads and the Task 9 report>
git fetch origin
git switch -c feature/flpdf-qynx-6-json-stdio "${layer1_tip}"
test "$(git rev-parse HEAD)" = "${layer1_tip}"
```

Expected: the working tree is clean and the new branch is exactly stacked on
the immutable Layer 1 remote PR head, without resolving the mutable Layer 1
branch name.

- [ ] **Step 2: Add failing direct-write and buffered-boundary tests**

Add direct scripted-writer tests:

```rust
partial_writes_are_retried_until_the_full_input_is_written
interrupted_write_is_retried
zero_progress_is_runtime_error_with_identifier_and_operation
write_error_is_runtime_error_with_identifier_and_operation
finish_ebadf_is_exact_stream_already_closed_logic_error
finish_non_ebadf_error_is_ignored
repeated_finish_and_write_after_finish_remain_reusable
drop_does_not_flush_or_close
```

The exact EBADF assertion is:

```rust
assert_eq!(
    stage.finish().unwrap_err().to_string(),
    "stdio: Pl_StdioFile::finish: stream already closed"
);
```

Add `BufWriter::with_capacity(4096, ProbeSink)` tests:

```rust
buffered_4095_byte_enospc_is_deferred_to_finish_and_ignored
buffered_4096_byte_enospc_is_a_write_runtime_error
buffered_4097_bytes_preserve_all_successful_bytes
buffered_partial_progress_preserves_the_exact_prefix
```

- [ ] **Step 3: Run RED**

Run:

```bash
cargo test -p flpdf pipeline::stdio_file::tests -- --nocapture
cargo test -p flpdf --test pipeline_public_api
```

Expected: compilation fails because `PlStdioFile` does not exist.

- [ ] **Step 4: Implement `PlStdioFile`**

Use:

```rust
const EBADF_ERRNO: i32 = 9;

pub struct PlStdioFile<'a> {
    identifier: String,
    writer: &'a mut dyn Write,
}

impl Pipeline for PlStdioFile<'_> {
    fn identifier(&self) -> &str {
        &self.identifier
    }

    fn write(&mut self, mut data: &[u8]) -> PipelineResult<()> {
        while !data.is_empty() {
            match self.writer.write(data) {
                Ok(0) => {
                    let source = io::Error::new(
                        io::ErrorKind::WriteZero,
                        "failed to write buffered data",
                    );
                    return Err(self.write_error(source));
                }
                Ok(written) => data = &data[written..],
                Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
                Err(source) => return Err(self.write_error(source)),
            }
        }
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        match self.writer.flush() {
            Err(source) if source.raw_os_error() == Some(EBADF_ERRNO) => {
                Err(PipelineError::logic(format!(
                    "{}: Pl_StdioFile::finish: stream already closed",
                    self.identifier
                )))
            }
            Ok(()) | Err(_) => Ok(()),
        }
    }
}
```

`write_error` returns:

```rust
PipelineError::runtime(format!(
    "{}: Pl_StdioFile::write: {source}",
    self.identifier
))
```

Export the stage from `pipeline.rs` and extend the downstream-crate public
API test.

- [ ] **Step 5: Run GREEN**

Run:

```bash
cargo test -p flpdf pipeline::stdio_file::tests -- --nocapture
cargo test -p flpdf --test pipeline_public_api
```

Expected: direct and 4095/4096/4097 buffered contracts pass.

- [ ] **Step 6: Commit**

```bash
git add crates/flpdf/src/pipeline.rs \
  crates/flpdf/src/pipeline/stdio_file.rs \
  crates/flpdf/tests/pipeline_public_api.rs
git commit -m "feat(pipeline): add qpdf PlStdioFile terminal"
```

---

### Task 11: Migrate side files and delete `QpdfStdioWriter`

**Files:**
- Modify: `crates/flpdf/src/json_inspect.rs`
- Modify: `crates/flpdf/src/json/mod.rs`
- Delete: `crates/flpdf/src/json/stdio.rs`
- Modify: `crates/flpdf-cli/tests/cli_json.rs`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/QPDF_json.cc:835-852`

**Interfaces:**
- Side-file open failures remain `JsonOutputError::SideFileIo` with
  operation/path/source.
- Side-file bytes flow through a 4096-capacity `BufWriter` and
  `PlStdioFile`.
- Side-file code explicitly calls `PlStdioFile::finish`.
- `write_file_mode_stream_value` accepts `&mut dyn Pipeline` for both the
  main JSON output and side-file output.

- [ ] **Step 1: Add failing side-file lifecycle tests**

Convert the current `QpdfStdioWriter` unit scenarios into production-boundary
tests in `json_inspect.rs`:

```rust
side_file_open_failure_keeps_main_json_prefix_and_path_context
side_file_pipeline_write_failure_keeps_datafile_prefix
side_file_explicit_finish_ignores_enospc
side_file_explicit_finish_reports_ebadf_logic_error
side_file_success_writes_exact_payload_and_complete_json
```

For the existing Linux `/dev/full` CLI case, continue asserting qpdf and
flpdf both exit successfully and emit complete parseable main JSON.

- [ ] **Step 2: Run RED**

Run:

```bash
cargo test -p flpdf json_inspect::tests::side_file -- --nocapture
cargo test -p flpdf-cli --test cli_json file_stream_to_dev_full_matches_qpdf_success_and_complete_json
```

Expected: the new tests fail because production still constructs
`QpdfStdioWriter`.

- [ ] **Step 3: Replace the side-file chain**

Use:

```rust
let side_file = File::create(&side_path)
    .map_err(|source| side_file_io_error("open", &side_path, source))?;
let mut buffered = BufWriter::with_capacity(4096, side_file);
let mut terminal = PlStdioFile::new("stream data", &mut buffered);
write_file_mode_stream_value(
    pdf,
    &stream,
    decode_level,
    &side_path,
    &mut terminal,
    out,
)?;
terminal.finish()?;
```

Change side-file payload emission to:

```rust
side_file.write(payload.bytes.as_ref())?;
```

Delete `finish_file_mode_side_file`, the `QpdfStdioWriter` import/re-export,
the `mod stdio` declaration, and `json/stdio.rs`.

- [ ] **Step 4: Run GREEN and deletion audit**

Run:

```bash
cargo test -p flpdf json_inspect::tests -- --nocapture
cargo test -p flpdf-cli --test cli_json
test ! -e crates/flpdf/src/json/stdio.rs
! rg -n 'QpdfStdioWriter|finish_file_mode_side_file|mod stdio' \
  crates/flpdf/src
```

Expected: all JSON production tests pass and every legacy stdio symbol is
absent.

- [ ] **Step 5: Commit**

```bash
git add crates/flpdf/src/json_inspect.rs crates/flpdf/src/json/mod.rs \
  crates/flpdf-cli/tests/cli_json.rs
git add -u crates/flpdf/src/json/stdio.rs
git commit -m "refactor(json): cut side files over to PlStdioFile"
```

---

### Task 12: Move top-level files to the qpdf stdio close lifecycle

**Files:**
- Modify: `crates/flpdf/src/json_inspect.rs`
- Modify: `crates/flpdf-cli/tests/cli_json.rs`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/QPDFJob.cc:3088-3116`

**Interfaces:**
- `JsonOutput::Stdout` remains `PlOStream` and command-boundary finish.
- `JsonOutput::File` becomes:
  `BufWriter::with_capacity(4096, handle) -> PlStdioFile`.
- The top-level raw writer does not finish the terminal.
- Dropping the `BufWriter` supplies the qpdf `FileCloser`-equivalent close
  flush and ignores any drop-time flush error.

- [ ] **Step 1: Add failing file-close boundary tests**

Add library tests:

```rust
coordinator_file_4095_bytes_are_flushed_on_buffered_drop
coordinator_file_4096_write_failure_is_pipeline_runtime_error
coordinator_file_does_not_call_pipeline_finish
coordinator_stdout_still_uses_ostream_finish
```

The 4095-byte file failure probe asserts close/drop-time failure is ignored
while the accepted prefix remains. The 4096-byte probe asserts a
`JsonOutputError::Pipeline(PipelineError::Runtime(_))` with
`Pl_StdioFile::write` in the message.

Keep/add Linux CLI parity:

```rust
json_output_dev_full_matches_qpdf_success
json_stdout_to_dev_full_matches_qpdf_success
```

- [ ] **Step 2: Run RED**

Run:

```bash
cargo test -p flpdf json_inspect::tests::coordinator_file -- --nocapture
cargo test -p flpdf-cli --test cli_json json_output_dev_full_matches_qpdf_success
```

Expected: the file lifecycle test shows Layer 1 still uses `PlOStream`.

- [ ] **Step 3: Replace only the file coordinator arm**

Implement:

```rust
JsonOutput::File(writer) => {
    let mut buffered = BufWriter::with_capacity(4096, writer);
    {
        let mut terminal = PlStdioFile::new("json output", &mut buffered);
        write_qpdf_json_v2_selected_objects_with_options(
            pdf,
            decode_level,
            stream_mode,
            keys,
            objects,
            &mut terminal,
        )?;
    }
    Ok(())
}
```

Do not call `terminal.finish()`. The `BufWriter` drops at the end of the arm;
its ignored drop-time flush failure matches qpdf's top-level file close
behavior. Leave the stdout arm unchanged.

- [ ] **Step 4: Run GREEN**

Run:

```bash
cargo test -p flpdf json_inspect::tests -- --nocapture
cargo test -p flpdf-cli --test cli_json
```

Expected: stdout, top-level file, side-file, partial-output, and `/dev/full`
tests pass.

- [ ] **Step 5: Audit CLI and JSON terminal ownership**

Run:

```bash
! rg -n 'Pipeline|PlString|PlConcatenate|PlBase64|PlOStream|PlStdioFile' \
  crates/flpdf-cli/src
rg -n 'JsonOutput::Stdout|JsonOutput::File|PlOStream::new|PlStdioFile::new' \
  crates/flpdf/src/json_inspect.rs
```

Expected: CLI has no stage knowledge; the library has exactly one stdout
terminal selection and the approved top-level/side-file stdio selections.

- [ ] **Step 6: Commit**

```bash
git add crates/flpdf/src/json_inspect.rs crates/flpdf-cli/tests/cli_json.rs
git commit -m "refactor(json): use qpdf top-level file lifecycle"
```

---

### Task 13: Extend the oracle with stdio lifecycle records

**Files:**
- Modify: `tests/oracle/qpdf_json_pipeline_probe.cc`
- Create: `tests/oracle/qpdf_json_pipeline_stdio_records.tsv`
- Modify: `scripts/qpdf-json-pipeline-diff.sh`
- Modify: `scripts/tests/qpdf-json-pipeline-diff-contract.sh`
- Modify: `crates/flpdf/tests/pipeline_public_api.rs`

**Interfaces:**
- The C++ probe adds a `stdio` mode.
- The Rust checked/live tests render the same stdio records through
  `PlStdioFile` and a 4096-capacity buffered handle.
- Record fields remain `case`, `status`, `bytes_hex`, `write_count`,
  `finish_count`.

- [ ] **Step 1: Add failing checked/live stdio tests**

Add records for:

```text
stdio-4095-enospc
stdio-4096-enospc
stdio-4097-success
stdio-partial-write
stdio-interrupted-write
stdio-zero-progress
stdio-finish-ebadf
stdio-finish-enospc
stdio-repeated-finish
```

Add:

```rust
#[test]
fn checked_qpdf_stdio_records_match_rust()

#[test]
#[ignore = "live pinned qpdf 11.9.0 Pl_StdioFile oracle"]
fn live_qpdf_stdio_records_match_rust()
```

The checked test uses
`include_str!("../../../tests/oracle/qpdf_json_pipeline_stdio_records.tsv")`;
the live test runs the probe with `stdio`.

- [ ] **Step 2: Run RED**

Run:

```bash
cargo test -p flpdf --test pipeline_public_api checked_qpdf_stdio_records_match_rust
bash scripts/tests/qpdf-json-pipeline-diff-contract.sh
```

Expected: missing record file/test mode failures.

- [ ] **Step 3: Extend probe, runner, and contract**

Use a temporary regular file and `/dev/full` for C++ stdio records. For
scripted partial/interrupted/zero cases, compile the Linux probe with
`#define _GNU_SOURCE` and construct a `FILE*` using `fopencookie`:

```cpp
struct Cookie {
    std::vector<unsigned char> bytes;
    std::deque<WriteStep> steps;
};

cookie_io_functions_t io{
    .read = nullptr,
    .write = cookie_write,
    .seek = nullptr,
    .close = nullptr,
};
FILE* file = fopencookie(&cookie, "wb", io);
setvbuf(file, nullptr, _IOFBF, 4096);
```

`cookie_write` returns the scripted partial count, returns `-1` with
`errno = EINTR` for interruption, or returns `0` with `errno = ENOSPC` for
zero progress. The runner skips the live stdio differential on non-Linux
hosts; checked Rust records remain platform-independent.

The runner executes both exact ignored tests:

```bash
QPDF_JSON_PIPELINE_PROBE="${probe}" \
  cargo test -p flpdf --test pipeline_public_api \
  live_qpdf_core_records_match_rust -- --ignored --exact
QPDF_JSON_PIPELINE_PROBE="${probe}" \
  cargo test -p flpdf --test pipeline_public_api \
  live_qpdf_stdio_records_match_rust -- --ignored --exact
```

Extend the contract log assertions to require both selectors.

- [ ] **Step 4: Run GREEN**

Run:

```bash
cargo test -p flpdf --test pipeline_public_api
bash scripts/tests/qpdf-json-pipeline-diff-contract.sh
scripts/qpdf-json-pipeline-diff.sh
```

Expected: core and stdio checked records and live differentials all pass.

- [ ] **Step 5: Commit**

```bash
git add tests/oracle/qpdf_json_pipeline_probe.cc \
  tests/oracle/qpdf_json_pipeline_stdio_records.tsv \
  scripts/qpdf-json-pipeline-diff.sh \
  scripts/tests/qpdf-json-pipeline-diff-contract.sh \
  crates/flpdf/tests/pipeline_public_api.rs
git commit -m "test(pipeline): cover qpdf stdio lifecycle oracle"
```

---

### Task 14: Document, verify, and publish Layer 2

**Files:**
- Modify: module docs in `pipeline/stdio_file.rs`, `json/mod.rs`, and
  `json_inspect.rs`
- Regenerate: `docs/qpdf-module-doc-index.md`
- Manually update: `docs/qpdf-correspondence.md`
- Modify: `docs/superpowers/plans/2026-07-28-qpdf-json-pipeline-cutover.md`

**Interfaces:**
- Produces the final complete-cutover stack.
- Leaves `flpdf-qynx.6` in progress until both PRs merge and merged `main` is
  verified.

- [ ] **Step 1: Regenerate the module index and validate correspondence docs**

Run:

```bash
python3 scripts/qpdf-module-docs.py --write
PYTHONDONTWRITEBYTECODE=1 \
  python3 -m unittest scripts/tests/test_qpdf_module_docs.py
python3 scripts/qpdf-module-docs.py --check
```

Expected: 55 checker tests pass, the generated module index is current, and
the manually curated correspondence table has been reviewed against the Layer
2 implementation.

- [ ] **Step 2: Run deletion and responsibility audits**

Run:

```bash
test ! -e crates/flpdf/src/json/stdio.rs
! rg -n 'Base64Writer|QpdfStdioWriter|base64::|base64\\.workspace' \
  Cargo.toml crates/flpdf/Cargo.toml crates/flpdf/src
! rg -n 'out: &mut \\(impl Write|out: &mut dyn Write' \
  crates/flpdf/src/json/writer.rs
! rg -n 'Pipeline|PlString|PlConcatenate|PlBase64|PlOStream|PlStdioFile' \
  crates/flpdf-cli/src
```

Expected: all commands return success with no forbidden symbol.

- [ ] **Step 3: Run Layer 2 focused and workspace gates**

Run:

```bash
cargo fmt --all -- --check
cargo test -p flpdf pipeline::stdio_file::tests -- --nocapture
cargo test -p flpdf --test pipeline_public_api
cargo test -p flpdf --test json_tests
cargo test -p flpdf json_inspect::tests
cargo test -p flpdf-cli --test cli_json
bash scripts/tests/qpdf-json-pipeline-diff-contract.sh
scripts/qpdf-json-pipeline-diff.sh
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" \
  cargo doc --workspace --no-deps --document-private-items
```

Expected: every command exits zero.

- [ ] **Step 4: Commit docs and any focused coverage correction**

```bash
git status --short
git add docs/qpdf-module-doc-index.md docs/qpdf-correspondence.md \
  docs/superpowers/plans/2026-07-28-qpdf-json-pipeline-cutover.md \
  crates/flpdf/src/pipeline/stdio_file.rs \
  crates/flpdf/src/json/mod.rs crates/flpdf/src/json_inspect.rs
git diff --cached --name-only
git diff --cached --check
git commit -m "docs(pipeline): record JSON stdio correspondence"
```

If `git status --short` shows a focused test-only coverage correction, inspect
it and run a separate `git add` naming every intended file path explicitly
before the cached-diff checks above. Never pass a directory, glob, `scripts`,
or `git add -u`; leave every unrelated path unstaged. If the index is empty,
do not create an empty commit.

- [ ] **Step 5: Obtain fresh committed Layer 2 patch coverage**

Manually inject the exact immutable Layer 1 SHA recorded externally by Task
9. Do not resolve it from the mutable Layer 1 branch name:

```bash
layer1_tip="<paste immutable Layer 1 SHA recorded by Task 9>"
test "${layer1_tip}" != "<paste immutable Layer 1 SHA recorded by Task 9>"
git cat-file -e "${layer1_tip}^{commit}"
coverage_base="${layer1_tip}"
layer2_merge_base="$(git merge-base HEAD "${layer1_tip}")"
test "${layer2_merge_base}" = "${coverage_base}"
cargo llvm-cov clean --workspace
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail \
  --lcov --output-path target/qynx-6-layer2.lcov
scripts/patch-coverage.sh --base "${coverage_base}" \
  --lcov target/qynx-6-layer2.lcov
```

Expected: the Layer 2 branch merge-base and patch-coverage base are the same
externally recorded immutable Layer 1 SHA, and Layer 2 changed executable lines
are exactly 100%. Add and commit focused tests for any uncovered line,
regenerate the LCOV file, and rerun.

- [ ] **Step 6: Push Beads and Layer 2**

Run:

```bash
git status --short
bd dolt push
git push --set-upstream origin feature/flpdf-qynx-6-json-stdio
git rev-parse HEAD
git rev-parse '@{upstream}'
```

Expected: status is clean and local/remote Layer 2 tips are identical.

- [ ] **Step 7: Open the stacked Layer 2 draft PR**

Run:

```bash
gh pr create --draft \
  --base feature/flpdf-qynx-6-json-pipeline \
  --head feature/flpdf-qynx-6-json-stdio \
  --title "Complete qpdf JSON stdio pipeline cutover" \
  --body "Completes flpdf-qynx.6 on top of the JSON core PR: adds PlStdioFile, moves top-level and side-file output to qpdf lifecycles, removes QpdfStdioWriter/json/stdio.rs, and verifies stdio oracle and /dev/full parity."
```

Expected: a stacked draft PR URL is returned. Keep the Bead open until both
PRs are merged and the merged-main verification/cleanup sequence succeeds.

## Final Verification Checklist

- [ ] Layer 1 diff contains the public core stages, pipeline-native JSON/raw
  output, coordinator, oracle, dependency removal, and no stdio deletion.
- [ ] Layer 2 diff contains only stdio/file terminal completion, side/top-level
  lifecycle cutover, oracle extension, and legacy stdio deletion.
- [ ] `Json::write`, helpers, unparse, blob callbacks, and raw inspection use
  only `Pipeline`.
- [ ] JSON serializers never finish the caller's outer pipeline.
- [ ] Inline blob output is quote → Base64 → concatenate → outer pipeline →
  quote, with no tail/closing quote after callback failure.
- [ ] `PlOStream` failures and top-level close-time `/dev/full` failures remain
  non-fatal like qpdf 11.9.0; a stdio write-time failure remains a
  `PipelineError::Runtime`.
- [ ] Side-file output explicitly finishes `PlStdioFile`; top-level output
  relies on buffered close/drop.
- [ ] CLI source contains no Pipeline or `Pl*` stage knowledge.
- [ ] `Base64Writer`, `QpdfStdioWriter`, `json/stdio.rs`, the external
  `base64` dependency, and Write-only JSON serialization are absent.
- [ ] Core and stdio checked records, live qpdf differentials, focused tests,
  workspace tests, fmt, denied-warning Clippy, strict rustdoc, and module-doc
  checks pass.
- [ ] Layer 1 and Layer 2 each have fresh 100% patch coverage against their
  own immediate parent.
- [ ] Both branches and Beads state are pushed; the Bead remains open until
  merged-main verification.
