# qpdf JSON Pipeline Complete Cutover Design

**Issue:** `flpdf-qynx.6`  
**Date:** 2026-07-28  
**Status:** Approved design

## 1. Purpose

Replace the JSON output subsystem's parallel `std::io::Write`-based
serialization, Base64, and stdio implementations with qpdf-compatible
`Pipeline` stages.

This is a complete cutover. API backward compatibility has lower priority than
matching qpdf 11.9.0 responsibility boundaries, byte output, state transitions,
finish behavior, and partial-output behavior.

The implementation is split into two stacked pull requests:

1. JSON core stages and serialization cutover.
2. stdio/file terminals and production output cutover.

Each PR must be reviewable, fully tested, and independently pass the repository
quality gates against its immediate parent.

## 2. Source of truth

The behavioral oracle is qpdf 11.9.0 at the repository-pinned commit. The main
corresponding qpdf components are:

- `Pipeline`
- `Pl_String`
- `Pl_Concatenate`
- `Pl_Base64`
- `Pl_OStream`
- `Pl_StdioFile`
- `JSON::write`
- `JSON::unparse`
- `QPDF::writeJSON`
- `QPDF::writeJSONStreamFile`
- `QPDFJob::writeJSON`
- `QPDFJob::doJSON`

Source citations added to module and correspondence documentation must use the
pinned source resolved by `scripts/fetch-qpdf-source.sh --print-path`.

Observed qpdf behavior is authoritative even when a more conventional Rust API
would be stricter. In particular, pipeline ownership and finish behavior must
not be inferred from `std::io::Write` conventions.

## 3. Responsibility boundary

### 3.1 Low-level library API

Low-level JSON serialization accepts `&mut dyn Pipeline`. This includes:

- the public JSON value write API;
- raw JSON inspection serialization;
- inline blob serialization callbacks;
- internal helpers that emit JSON bytes incrementally.

The JSON serializer writes bytes but does not own or finish the supplied
pipeline.

The public cutover changes the relevant signatures rather than retaining
Write-based compatibility overloads:

- `Json::write` and all incremental JSON punctuation/container helpers accept
  `&mut dyn Pipeline` and return `PipelineResult<()>`;
- `Json::make_blob` accepts a callback of the form
  `Fn(&mut dyn Pipeline) -> PipelineResult<()>`;
- `Json::unparse` returns `PipelineResult<Vec<u8>>`;
- the raw inspection writer accepts `&mut dyn Pipeline` and retains
  `JsonOutputError` as its combined conversion/output error type, with
  `PipelineError` replacing its direct serialization `io::Error` channel.

The `pipeline` module becomes public and exposes:

- `Pipeline`
- `PipelineError`
- `PipelineResult`
- `PlString`
- `PlConcatenate`
- `PlBase64`
- `PlOStream`
- `PlStdioFile` in the second PR

The public `PipelineError` constructors and message accessor are also public so
external pipeline implementations can preserve the same logic/runtime
categories.

Existing pipeline stages do not need broader visibility solely because this
module becomes public. Their visibility remains governed by their own
responsibilities.

### 3.2 Output coordination

The library owns a JSON output coordinator analogous to qpdf's `QPDFJob`
boundary. It accepts an ordinary stdout/file handle plus an output-kind
selection and constructs the correct terminal pipeline internally.

The coordinator is responsible for:

- selecting the terminal stage from the supplied output kind;
- constructing `PlOStream` or `PlStdioFile`;
- enforcing command-boundary flush or close behavior;
- constructing side-file output internally for externalized stream data;
- converting errors that occur outside a pipeline into the JSON output error
  model.

### 3.3 CLI boundary

`flpdf-cli` does not import, construct, or finish `Pipeline`, `PlOStream`, or
`PlStdioFile`. It supplies an ordinary output handle and destination kind to the
library coordinator.

This follows qpdf's effective responsibility split: the CLI-facing job layer
selects output behavior, while qpdf library code constructs and uses pipeline
terminals.

## 4. Pipeline contracts

### 4.1 `PlString`

`PlString` is a terminal or tee stage that appends every written byte to a
caller-owned `Vec<u8>`.

Contract:

- append input to the destination before invoking a downstream stage;
- optionally forward the same bytes to the downstream stage;
- if downstream write fails, retain the already-appended bytes;
- on `finish`, finish the downstream stage when present;
- support reuse according to qpdf's stage behavior rather than consuming the
  destination.

`JSON::unparse` uses `PlString` to collect serializer output into a byte vector.
It does not need to call `finish` when no downstream stage exists.

### 4.2 `PlConcatenate`

`PlConcatenate` forwards writes but suppresses ordinary downstream finish.

Contract:

- write calls are forwarded unchanged;
- ordinary `finish` succeeds without finishing the downstream stage;
- `manual_finish` explicitly finishes the downstream stage;
- downstream write and manual-finish errors propagate unchanged.

This stage allows an inner transformation to finish without ending the outer
JSON output pipeline.

### 4.3 `PlBase64`

`PlBase64` implements qpdf 11.9.0 Base64 encode and decode behavior as a
stateful pipeline stage.

The implementation must reproduce:

- encode and decode modes;
- arbitrary input splitting;
- buffered partial quantum handling;
- accepted decode whitespace;
- `-` and `_` decode aliases;
- padding validation and emission;
- exact invalid-input errors;
- state after write or finish failure;
- repeated finish and reuse behavior.

It must not silently normalize invalid input or automatically finalize state
after an error unless qpdf does so.

Production JSON Base64 output must use only this implementation. Once no
production consumer remains, remove the `base64` crate dependency from the
`flpdf` crate and workspace dependency list. Tests may use owned test helpers or
hard-coded vectors, but not a second production algorithm.

### 4.4 `PlOStream`

`PlOStream` adapts a borrowed Rust `std::io::Write` value to `Pipeline` while
matching qpdf's default `std::ostream` behavior.

Contract:

- it does not close the underlying writer;
- an underlying `io::Error` places the stage in a sticky failed state;
- the failing pipeline operation does not turn that writer failure into a
  fatal `PipelineError`;
- later writes and finishes are no-ops;
- reuse does not clear the sticky writer failure.

The library coordinator performs any command-boundary stdout flush. Until
`flpdf-qynx.4` introduces `QPDFLogger`, stdout remains a `PlOStream` owned by
the coordinator.

This intentionally preserves observed qpdf 11.9.0 behavior where JSON output
directed to `/dev/full` through stdout or a top-level output path exits
successfully.

### 4.5 `PlStdioFile`

`PlStdioFile` is added in the second PR for qpdf-compatible file/stdio
semantics.

Contract:

- writes loop across partial writes;
- zero-progress writes are runtime errors;
- interrupted writes are retried as qpdf's stdio behavior requires;
- write failures become `PipelineError::Runtime` with the stage identifier and
  operation;
- `finish` maps an already-closed/`EBADF` condition to
  `PipelineError::Logic("stream already closed")`;
- non-`EBADF` flush failures are ignored;
- repeated finish and reuse match qpdf 11.9.0.

The exact Rust ownership representation may differ from a C `FILE*`, but the
observable write, flush, close, and error behavior must not.

## 5. JSON data flow and lifecycle

### 5.1 Ordinary serialization

The serializer writes directly to the supplied pipeline:

```text
JSON value -> &mut dyn Pipeline -> caller-owned terminal chain
```

Neither `JSON::write` nor the raw inspection writer calls `finish` on the
pipeline. The owner of the terminal chain decides whether and when finishing is
required.

### 5.2 Inline blob serialization

Inline binary JSON values use the following chain:

```text
opening quote
    |
blob callback -> PlBase64 -> PlConcatenate -> outer JSON pipeline
    |
Base64 finish emits padding
    |
PlConcatenate suppresses outer finish
    |
closing quote
```

Required behavior:

- emit the opening quote before invoking the blob callback;
- stream callback bytes into `PlBase64`;
- call `PlBase64::finish` to emit a final padded quantum;
- do not finish the outer JSON pipeline;
- emit the closing quote only after the callback and Base64 finish succeed;
- if the callback fails, preserve all output already emitted and do not emit
  Base64 tail bytes or the closing quote;
- do not roll back partial JSON output.

This removes the current `Base64Writer<W: Write>` path.

### 5.3 Unparse

Unparse uses:

```text
JSON serializer -> PlString -> caller-owned Vec<u8>
```

It returns the collected bytes as `PipelineResult<Vec<u8>>`. No parallel
serializer or `std::io::Write` bridge remains.

### 5.4 Stdout

The output coordinator wraps stdout in `PlOStream`, invokes the low-level
pipeline JSON writer, and performs the existing command-boundary flush.

The CLI itself has no pipeline awareness. Logger ownership is deferred to
`flpdf-qynx.4`.

### 5.5 Top-level file output

The output coordinator receives the ordinary buffered file handle and
constructs a `PlStdioFile`. The JSON writer does not finish the terminal.
Closing the coordinator-owned handle provides the top-level flush/close
boundary, matching qpdf's non-fatal top-level output behavior.

### 5.6 Side-file output

The library-side stream-file writer opens the side file, constructs
`PlStdioFile`, writes the stream, and explicitly calls `finish` before closing.
This matches qpdf's `QPDF::writeJSONStreamFile` responsibility.

Open failures occur before pipeline construction and remain structured JSON
output errors containing the operation and path. No rollback of previously
emitted main JSON output is attempted.

## 6. Error model

`PipelineError` remains the single stage error type:

- `Logic(String)` for invalid pipeline lifecycle/state;
- `Runtime(String)` for runtime processing failures.

No parallel `io::Error` channel is added to the `Pipeline` trait.

Error rules:

- `PlString` retains appended bytes when downstream propagation fails;
- `PlConcatenate` passes downstream errors through unchanged;
- ordinary `PlConcatenate::finish` always succeeds;
- `PlBase64` uses qpdf-equivalent error text and state transitions;
- `PlStdioFile` converts write/zero-progress failures into `Runtime` errors
  including stage identifier and operation;
- `PlStdioFile::finish` reports only the qpdf-equivalent already-closed
  condition and ignores other flush failures;
- side-file open failures remain `JsonOutputError` values with path context;
- `PlOStream` records writer failure internally and does not create a fatal
  pipeline error;
- JSON serialization preserves partial output on all failures.

## 7. Pull request stack

### 7.1 PR 1: JSON core pipeline cutover

Add:

- `pipeline/string.rs`
- `pipeline/concatenate.rs`
- `pipeline/base64.rs`
- `pipeline/ostream.rs`
- a pinned qpdf C++ oracle probe for the new stage contracts;
- a diff script for oracle comparison;
- a contract test for the probe and script.

Change:

- `pipeline.rs` and `lib.rs` for the public contract;
- JSON value serialization to accept `&mut dyn Pipeline`;
- inline blob callbacks to stream through `PlBase64` and `PlConcatenate`;
- unparse to collect with `PlString`;
- raw inspection JSON serialization to use `Pipeline`;
- `json_inspect` to expose the library output coordinator;
- correspondence and module documentation.

Delete:

- `Base64Writer`;
- Write-generic JSON serialization paths;
- the production `base64` crate dependency if no production use remains.

PR 1 may retain the existing stdio/file terminal internally only as a temporary
implementation detail behind the new coordinator. It must not preserve a
public compatibility API that competes with the pipeline contract.

### 7.2 PR 2: stdio/file terminal cutover

Add:

- `pipeline/stdio_file.rs`;
- focused stdio/file contract tests.

Change:

- top-level JSON file output to use `PlStdioFile`;
- side-file output to use `PlStdioFile` and explicit finish;
- production stdout/file selection to remain entirely inside the library
  coordinator;
- CLI calls to select destinations without constructing pipeline stages;
- correspondence and module documentation.

Delete:

- `json/stdio.rs`;
- `QpdfStdioWriter`;
- all remaining JSON-specific stdio duplication.

## 8. Testing strategy

### 8.1 PR 1 tests

Unit tests cover:

- `PlString` terminal and tee behavior;
- append-before-forward partial output;
- downstream write and finish failures;
- `PlConcatenate` suppressed finish and `manual_finish`;
- `PlBase64` encode/decode vectors;
- every input split around quantum boundaries;
- decode whitespace, aliases, padding, and invalid input;
- finish, repeated finish, reuse, and failure-state behavior;
- `PlOStream` success, sticky failure, later no-op calls, and non-closing
  ownership;
- JSON exact bytes for all value shapes;
- inline blob success and partial-output failure;
- unparse byte identity;
- public importability of the intended pipeline contract;
- absence of `Base64Writer` and Write-generic JSON serializer symbols.

Oracle coverage includes:

- a C++ probe compiled against pinned qpdf 11.9.0;
- checked-in default record snapshots;
- a normal non-live contract test for the harness and records;
- an ignored live oracle test;
- a diff script that compares Rust and qpdf records.

The probe must exercise lifecycle and partial-output behavior, not only final
happy-path bytes.

### 8.2 PR 2 tests

Unit tests cover:

- 4095, 4096, and 4097-byte boundaries;
- partial writes;
- zero-progress writes;
- interrupted writes;
- `EBADF` finish;
- non-`EBADF` flush failure;
- repeated finish;
- reuse after finish or failure.

Production-path tests cover:

- coordinator stdout output;
- coordinator top-level file output;
- coordinator side-file output;
- exact byte identity across supported destinations;
- open, write, and finish failure prefixes and partial output;
- CLI behavior without pipeline imports;
- qpdf comparison for `/dev/full` stdout and top-level file behavior;
- qpdf comparison for side-file output and failures;
- absence of `QpdfStdioWriter` and `json/stdio.rs`.

### 8.3 Quality gates

Each PR runs against its immediate parent:

- focused stage and JSON tests;
- `cargo test --workspace`;
- `cargo fmt --all -- --check`;
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
- `RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D
  rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc
  --workspace --no-deps --document-private-items`;
- `python3 -m unittest scripts/tests/test_qpdf_module_docs.py`;
- `python3 scripts/qpdf-module-docs.py --check`;
- a fresh changed-executable-line coverage run with 100% patch coverage.

Tests must use asymmetric chunks and explicit failure positions where symmetry
could hide buffering or lifecycle errors.

## 9. Documentation

Update public and module documentation to state:

- JSON serialization is pipeline-native;
- callers own the supplied pipeline and its finish boundary;
- `PlConcatenate` is required to finish inner transformations without ending
  outer JSON output;
- `PlOStream` intentionally uses sticky, non-fatal writer failure semantics;
- `PlStdioFile` intentionally follows qpdf stdio behavior rather than general
  Rust `Write` expectations;
- the CLI uses a library-owned output coordinator and does not own pipeline
  stages.

Correspondence documentation must cite the exact qpdf 11.9.0 source locations
used to establish each contract.

## 10. Non-goals

This issue does not implement:

- `QPDFLogger` ownership or routing (`flpdf-qynx.4`);
- page-content concatenation consumers (`flpdf-qynx.7`);
- writer, filter, crypto, or hash pipeline migrations;
- unrelated JSON schema-content gaps such as page labels;
- a general stdio terminal outside JSON consumers;
- qpdf-absent generic ownership/state abstractions;
- compatibility aliases for the removed Write-based JSON APIs.

Adjacent gaps discovered during implementation should become separate Beads
issues unless they are required to preserve this cutover's qpdf behavior.

## 11. Completion criteria

The stack is complete when:

- every JSON byte path is implemented through `Pipeline`;
- every inline Base64 byte is produced by `PlBase64`;
- every JSON string collection path uses `PlString`;
- every inner Base64 finish is isolated by `PlConcatenate`;
- stdout is adapted by library-owned `PlOStream`;
- top-level and side-file output use the approved `PlStdioFile` lifecycle;
- the CLI has no direct pipeline-stage knowledge;
- `Base64Writer`, `QpdfStdioWriter`, `json/stdio.rs`, and duplicate
  Write-generic serialization are removed;
- qpdf 11.9.0 byte, lifecycle, and partial-output behavior is covered by
  focused tests and oracle records;
- both PRs independently satisfy all quality gates, including 100% patch
  coverage.
