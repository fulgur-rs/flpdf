# QPDFJob JSON Output Orchestration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `--json-stream-data=file` reject stdout output without an explicit `--json-stream-prefix`, while introducing the source-faithful `QPDFJob::writeJSON`-equivalent library boundary that owns prefix resolution and output selection.

**Architecture:** Add a small `flpdf::job` JSON orchestration layer corresponding to qpdf 11.9.0 `QPDFJob::writeJSON` (`libqpdf/QPDFJob.cc:3094-3115`). It keeps the unresolved stream-data request separate from an optional prefix, resolves the default only after the input PDF has opened, and delegates all JSON construction and stream writing to the existing `json_inspect` writer. The CLI retains argument parsing, input/output safety checks, warning emission, and qpdf-shaped process exit formatting.

**Tech Stack:** Rust 2021, `thiserror`, existing `Pdf` and `json_inspect` APIs, `assert_cmd`, pinned qpdf 11.9.0 source and `/usr/bin/qpdf` behavioral oracle, Cargo fmt/test/Clippy/rustdoc, `cargo llvm-cov`, and `scripts/patch-coverage.sh`.

## Global Constraints

- Work only in `/home/ubuntu/flpdf/.worktrees/flpdf-oqdj-qpdf-job-json-output` on `feature/flpdf-oqdj-qpdf-job-json-output`; do not modify `main`.
- Treat qpdf 11.9.0 `QPDFJob.cc:3094-3115`, `QPDFJob.cc:297-300`, and `qpdf/qpdf.cc:9-23,37-38` as the source and diagnostic oracle.
- Preserve qpdf's error precedence: argument validation first, then input open/parse, then the missing-prefix usage error. A missing or malformed input must not be masked by the prefix error.
- Do not move or duplicate the existing `doJSON*`/`QPDF::writeJSON`-equivalent serialization in `json_inspect.rs`; the new layer only orchestrates output selection and resolved stream mode.
- Keep public rustdoc in English and free of Beads IDs or speculative follow-up work.
- Follow strict RED -> GREEN -> REFACTOR. Run the named failing test before adding the production implementation that makes it pass.
- Do not use a fallback prefix such as `"stream"` for stdout file mode.

---

## Task 1: Lock the CLI regression and qpdf diagnostic shape

**Files:**

- Modify: `crates/flpdf-cli/tests/cli_json.rs`

- [ ] **Step 1: Add the missing-prefix regression test**

Add a test beside `json_stream_data_file_creates_side_files` that uses the existing `one_page_pdf_with_stream` helper and runs from a temporary directory:

```rust
#[test]
fn json_stream_data_file_to_stdout_requires_explicit_prefix() {
    let temp = tempfile::tempdir().unwrap();
    let input_path = temp.path().join("input.pdf");
    std::fs::write(&input_path, one_page_pdf_with_stream()).unwrap();

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(temp.path())
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["--json=2", "--json-stream-data=file"])
        .arg(&input_path)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "\nqpdf: please specify --json-stream-prefix since the input file name is unknown\n\n\
For help:\n  qpdf --help=usage       usage information\n  qpdf --help=topic       help on a topic\n  \
qpdf --help=--option    help on an option\n  qpdf --help             general help and a topic list\n\n"
    );
    assert!(!temp.path().join("stream-4").exists());
}
```

- [ ] **Step 2: Run the focused test and confirm RED**

Run:

```bash
cargo test -p flpdf-cli --test cli_json json_stream_data_file_to_stdout_requires_explicit_prefix -- --exact
```

Expected: FAIL because the current CLI exits 0, writes JSON to stdout, and creates `stream-4`.

- [ ] **Step 3: Add the ignored live differential test**

Add `live_qpdf_json_file_stdout_requires_prefix`, an ignored test that first calls `skip_unless_qpdf_11_9()`, writes identical inputs into separate temporary working directories, and runs:

```text
qpdf --json=2 --json-stream-data=file <input>
flpdf --json=2 --json-stream-data=file <input>  (FLPDF_PROGNAME=qpdf)
```

Compare exit status, stdout, and stderr byte-for-byte, and assert that neither working directory contains a generated `stream-4` file. Mark it:

```rust
#[ignore = "live qpdf 11.9.0 missing JSON stream prefix oracle"]
```

Do not use this ignored test as the only regression; Step 1 is the always-on golden.

- [ ] **Step 4: Commit the RED tests**

```bash
git add crates/flpdf-cli/tests/cli_json.rs
git commit -m "test(json): lock missing stream prefix usage error"
```

---

## Task 2: Define the QPDFJob-shaped library contract with failing integration tests

**Files:**

- Create: `crates/flpdf/tests/job_json_tests.rs`
- Later create: `crates/flpdf/src/job/mod.rs`
- Later create: `crates/flpdf/src/job/json.rs`
- Later modify: `crates/flpdf/src/lib.rs`

- [ ] **Step 1: Add a fixture opener and default options helper**

Create `crates/flpdf/tests/job_json_tests.rs` using the committed compatibility fixture:

```rust
use flpdf::job::{
    write_json, JsonJobError, JsonJobOptions, JsonJobOutput, JsonStreamData,
};
use flpdf::json_inspect::DecodeLevel;
use flpdf::Pdf;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/one-page.pdf")
}

fn open_fixture() -> Pdf<BufReader<File>> {
    Pdf::open(BufReader::new(File::open(fixture()).unwrap())).unwrap()
}

fn options<'a>(stream_data: JsonStreamData, stream_prefix: Option<&'a str>) -> JsonJobOptions<'a> {
    JsonJobOptions {
        decode_level: DecodeLevel::Generalized,
        stream_data,
        stream_prefix,
        keys: &[],
        objects: &[],
    }
}
```

- [ ] **Step 2: Add contract tests for every prefix-resolution arm**

Add these tests:

1. `stdout_file_mode_without_prefix_is_usage_error`: pass `JsonJobOutput::Stdout`, assert `JsonJobError::Usage`, exact message `please specify --json-stream-prefix since the input file name is unknown`, and empty output.
2. `stdout_file_mode_uses_explicit_prefix`: use a temporary absolute prefix, assert success, valid JSON stdout, and the expected stream side file exists.
3. `file_output_file_mode_defaults_prefix_to_output_filename`: pass `JsonJobOutput::File { filename: &output_path, writer: &mut bytes }`, assert the JSON contains that filename plus the stream object suffix and the side file exists.
4. `none_and_inline_modes_do_not_require_prefix`: run both modes through stdout with no prefix and assert success; inline output contains `"data"`, while none output does not contain `"datafile"`.

The committed `one-page.pdf` fixture's content stream is object 7, so the positive tests must assert the exact `<prefix>-7` path.

- [ ] **Step 3: Run the new library test and confirm RED**

Run:

```bash
cargo test -p flpdf --test job_json_tests
```

Expected: compilation fails because `flpdf::job` and its public contract do not exist yet.

- [ ] **Step 4: Commit the RED contract tests**

```bash
git add crates/flpdf/tests/job_json_tests.rs
git commit -m "test(job): define JSON output orchestration contract"
```

---

## Task 3: Implement the minimal `QPDFJob::writeJSON` orchestration slice

**Files:**

- Create: `crates/flpdf/src/job/mod.rs`
- Create: `crates/flpdf/src/job/json.rs`
- Modify: `crates/flpdf/src/lib.rs`

- [ ] **Step 1: Export the new job module**

Add `pub mod job;` to the module list in `crates/flpdf/src/lib.rs`. In `crates/flpdf/src/job/mod.rs`, document the current qpdf source responsibility and re-export only the JSON orchestration API:

```rust
//! Command-level operations corresponding to qpdf's `QPDFJob` layer.
//!
//! The current surface implements the JSON output-selection responsibility
//! from qpdf 11.9.0 `QPDFJob::writeJSON`.

mod json;

pub use json::{
    write_json, JsonJobError, JsonJobOptions, JsonJobOutput, JsonStreamData, UsageError,
};
```

- [ ] **Step 2: Add unresolved options, output destination, and distinct errors**

In `crates/flpdf/src/job/json.rs`, define:

```rust
use crate::json_inspect::{
    write_qpdf_json_v2_selected_objects_to_output_with_options, DecodeLevel, JsonKey,
    JsonObjectSelector, JsonOutput, JsonOutputError, StreamDataMode,
};
use crate::Pdf;
use std::io::{Read, Seek, Write};
use std::path::Path;

const MISSING_STREAM_PREFIX: &str =
    "please specify --json-stream-prefix since the input file name is unknown";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JsonStreamData {
    #[default]
    None,
    Inline,
    File,
}

pub struct JsonJobOptions<'a> {
    pub decode_level: DecodeLevel,
    pub stream_data: JsonStreamData,
    pub stream_prefix: Option<&'a str>,
    pub keys: &'a [JsonKey],
    pub objects: &'a [JsonObjectSelector],
}

pub enum JsonJobOutput<'a> {
    Stdout(&'a mut dyn Write),
    File {
        filename: &'a Path,
        writer: &'a mut dyn Write,
    },
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct UsageError {
    message: String,
}

#[derive(Debug, thiserror::Error)]
pub enum JsonJobError {
    #[error(transparent)]
    Usage(#[from] UsageError),
    #[error(transparent)]
    Output(#[from] JsonOutputError),
}
```

Add complete English rustdoc to every public item and field. `write_json` must include an `# Errors` section distinguishing usage errors from serializer/output errors.

- [ ] **Step 3: Resolve stream mode exactly once, then delegate**

Implement `write_json` with this responsibility split:

```rust
pub fn write_json<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    options: JsonJobOptions<'_>,
    output: JsonJobOutput<'_>,
) -> Result<(), JsonJobError> {
    let stream_mode = match (options.stream_data, options.stream_prefix, &output) {
        (JsonStreamData::None, _, _) => StreamDataMode::None,
        (JsonStreamData::Inline, _, _) => StreamDataMode::Inline,
        (JsonStreamData::File, Some(prefix), _) => StreamDataMode::File {
            prefix: prefix.to_owned(),
        },
        (JsonStreamData::File, None, JsonJobOutput::File { filename, .. }) => {
            StreamDataMode::File {
                prefix: filename.to_string_lossy().into_owned(),
            }
        }
        (JsonStreamData::File, None, JsonJobOutput::Stdout(_)) => {
            return Err(UsageError {
                message: MISSING_STREAM_PREFIX.to_owned(),
            }
            .into());
        }
    };

    let output = match output {
        JsonJobOutput::Stdout(writer) => JsonOutput::Stdout(writer),
        JsonJobOutput::File { writer, .. } => JsonOutput::File(writer),
    };

    write_qpdf_json_v2_selected_objects_to_output_with_options(
        pdf,
        options.decode_level,
        &stream_mode,
        options.keys,
        options.objects,
        output,
    )?;
    Ok(())
}
```

Do not add an independent JSON builder, side-file writer, logger abstraction, or a general job configuration object in this slice.

- [ ] **Step 4: Run the library contract tests and confirm GREEN**

```bash
cargo test -p flpdf --test job_json_tests
```

Expected: PASS for usage, explicit-prefix, output-filename default, none, and inline arms.

- [ ] **Step 5: Run the existing JSON writer unit tests**

```bash
cargo test -p flpdf json_inspect
```

Expected: PASS; the serializer remains behaviorally unchanged.

- [ ] **Step 6: Commit the primitive**

```bash
git add crates/flpdf/src/lib.rs crates/flpdf/src/job crates/flpdf/tests/job_json_tests.rs
git commit -m "feat(job): add JSON output orchestration primitive"
```

---

## Task 4: Migrate the CLI to the primitive and make the regression green

**Files:**

- Modify: `crates/flpdf-cli/src/main.rs`
- Modify: `crates/flpdf-cli/tests/cli_json.rs`

- [ ] **Step 1: Replace direct serializer imports with job orchestration imports**

Import `write_json`, `JsonJobError`, `JsonJobOptions`, `JsonJobOutput`, `JsonStreamData`, and `UsageError` from `flpdf::job`. Keep `DecodeLevel`, `JsonKey`, and `JsonObjectSelector` from `json_inspect`; remove direct CLI imports of `JsonOutput`, `StreamDataMode as JsonStreamDataMode`, and `write_qpdf_json_v2_selected_objects_to_output_with_options`.

- [ ] **Step 2: Preserve the unresolved stream request until after input open**

In `run_json`, keep the existing pre-I/O validation order, but replace `prefix_default` and `JsonStreamDataMode` construction with:

```rust
let stream_data = match cli.json_stream_data.as_deref().unwrap_or("none") {
    "none" => JsonStreamData::None,
    "inline" => JsonStreamData::Inline,
    "file" => JsonStreamData::File,
    other => {
        eprintln!("flpdf: --json-stream-data must be none, inline, or file; got: {other}");
        std::process::exit(2);
    }
};
```

Do not perform the missing-prefix check here. Retain the existing same-input/output rejection, input file open, `Pdf` open/parse, and warning snapshot before calling `write_json`.

- [ ] **Step 3: Route both output destinations through `write_json`**

Build a fresh `JsonJobOptions` in each output branch without cloning the selector vectors. For file output pass both `filename: path` and the verified writer; for stdout pass only the locked writer:

```rust
let options = JsonJobOptions {
    decode_level: DecodeLevel::Generalized,
    stream_data,
    stream_prefix: cli.json_stream_prefix.as_deref(),
    keys: &json_keys,
    objects: &json_objects,
};
```

Map `JsonJobError::Output` through the existing warning-emission path. Return `JsonJobError::Usage` as the boxed `UsageError` without printing inside `run_json`, so process-boundary formatting happens once.

- [ ] **Step 4: Add qpdf-shaped usage formatting at the process boundary**

Before the generic error printer in `main`, downcast to `UsageError` and exit through a small helper:

```rust
fn usage_exit(error: &UsageError) -> ! {
    let who = progname();
    eprintln!(
        "\n{who}: {error}\n\nFor help:\n  {who} --help=usage       usage information\n  \
{who} --help=topic       help on a topic\n  {who} --help=--option    help on an option\n  \
{who} --help             general help and a topic list\n"
    );
    std::process::exit(2);
}
```

The explicit trailing newline inside the format string plus `eprintln!`'s newline is intentional and must match qpdf's final blank line.

- [ ] **Step 5: Correct the CLI help contract**

Update the `json_stream_prefix` documentation/help so it states:

- with `--json-output`, a missing prefix defaults to the JSON output filename;
- with JSON on stdout and `--json-stream-data=file`, an explicit prefix is required;
- no `"stream"` fallback exists.

- [ ] **Step 6: Run the focused regression and confirm GREEN**

```bash
cargo test -p flpdf-cli --test cli_json json_stream_data_file_to_stdout_requires_explicit_prefix -- --exact
```

Expected: PASS with exact exit 2/stdout/stderr and no side file.

- [ ] **Step 7: Add and run precedence and positive-path regressions**

Add these focused CLI tests:

1. `missing_stream_prefix_does_not_mask_missing_input`: a missing input with stdout file mode reports the input-open error, not the prefix usage error;
2. `missing_stream_prefix_does_not_mask_malformed_input`: an unrecoverably malformed input reports the parse/repair error first;
3. `json_stream_data_file_to_stdout_uses_explicit_prefix`: stdout file mode with an explicit prefix succeeds and creates the synthetic fixture's `<prefix>-4` side file;
4. `json_output_file_mode_defaults_stream_prefix`: file output with no explicit prefix succeeds and creates `<json-output-path>-4`.

Run:

```bash
cargo test -p flpdf-cli --test cli_json json_stream_data_file_to_stdout_uses_explicit_prefix -- --exact
cargo test -p flpdf-cli --test cli_json json_output_file_mode_defaults_stream_prefix -- --exact
cargo test -p flpdf-cli --test cli_json missing_stream_prefix_does_not_mask_missing_input -- --exact
cargo test -p flpdf-cli --test cli_json missing_stream_prefix_does_not_mask_malformed_input -- --exact
```

Expected: all four tests PASS.

- [ ] **Step 8: Run the live qpdf differential**

```bash
cargo test -p flpdf-cli --test cli_json live_qpdf_json_file_stdout_requires_prefix -- --ignored --exact --nocapture
```

Expected with qpdf 11.9.0 installed: PASS with byte-identical status/stdout/stderr and no side files. Locally skip only through the existing version guard; CI version mismatch is a test failure.

- [ ] **Step 9: Run the complete JSON CLI integration suite**

```bash
cargo test -p flpdf-cli --test cli_json
```

Expected: all non-ignored tests PASS.

- [ ] **Step 10: Commit the CLI migration**

```bash
git add crates/flpdf-cli/src/main.rs crates/flpdf-cli/tests/cli_json.rs
git commit -m "fix(json): require prefix for stdout stream files"
```

---

## Task 5: Record the new source-faithful responsibility boundary

**Files:**

- Modify: `docs/qpdf-correspondence.md`

- [ ] **Step 1: Update the QPDFJob correspondence row**

Change the Job/CLI table and nearby prose to record that:

- `crates/flpdf/src/job/json.rs` now corresponds to the output-selection portion of `QPDFJob::writeJSON` at `QPDFJob.cc:3094-3115`;
- `json_inspect.rs` still owns the `doJSON*`/`QPDF::writeJSON`-equivalent serialization;
- the overall QPDFJob migration remains partial because the rest of the job orchestration has not moved.

Keep ownership counts honest: this slice changes responsibility attribution but does not claim the full 3,116-line component complete.

- [ ] **Step 2: Run documentation checks**

```bash
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" \
  cargo doc --workspace --no-deps --document-private-items
```

Expected: PASS with no rustdoc diagnostics.

- [ ] **Step 3: Commit the correspondence update**

```bash
git add docs/qpdf-correspondence.md
git commit -m "docs: map QPDFJob JSON output selection"
```

---

## Task 6: Run full verification, coverage, and persist the issue state

**Files:**

- Modify when required by coverage: `crates/flpdf/tests/job_json_tests.rs`
- Modify when required by coverage: `crates/flpdf-cli/tests/cli_json.rs`
- Update via tool: Beads issue `flpdf-oqdj`.

- [ ] **Step 1: Run formatting and focused suites**

```bash
cargo fmt --all -- --check
cargo test -p flpdf --test job_json_tests
cargo test -p flpdf-cli --test cli_json
```

Expected: all PASS.

- [ ] **Step 2: Run crate and workspace suites**

```bash
cargo test -p flpdf
cargo test -p flpdf-cli
cargo test
```

Expected: all PASS, with only tests already marked ignored remaining ignored.

- [ ] **Step 3: Run lint and strict rustdoc gates**

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" \
  cargo doc --workspace --no-deps --document-private-items
```

Expected: both PASS with zero warnings/errors.

- [ ] **Step 4: Ensure a clean committed HEAD before coverage**

```bash
git status --short
```

Expected: empty. If verification required a legitimate code/test change, commit it before continuing; never use `--allow-dirty` to bypass the gate.

- [ ] **Step 5: Generate fresh LCOV and run the authoritative patch gate**

```bash
cargo llvm-cov clean --workspace
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail \
  --lcov --output-path target/patch-cov.lcov
scripts/patch-coverage.sh --base origin/main --lcov target/patch-cov.lcov
```

Expected: `patch-coverage: OK` and 100% coverage for every changed executable line under `crates/flpdf/src`. Add regression tests for uncovered reachable arms; use `cov:ignore` only for a locally justified unreachable line and put the reason on the executable line.

- [ ] **Step 6: Re-check branch scope**

```bash
git status --short --branch
git log --oneline origin/main..HEAD
git diff --stat origin/main...HEAD
git diff --check origin/main...HEAD
```

Expected: clean worktree; only the approved design, job JSON orchestration, CLI migration, regressions, and correspondence update.

- [ ] **Step 7: Close and persist the Bead only after all gates pass**

```bash
bd close flpdf-oqdj --reason "Implemented qpdf 11.9.0 QPDFJob::writeJSON output selection; stdout file mode now requires an explicit prefix; exact diagnostics, precedence, positive paths, full tests, lint, rustdoc, and 100% changed-line coverage verified"
bd dolt push
```

Read the issue back with `bd show flpdf-oqdj` and confirm it is closed and the dependency from `flpdf-q2fo` remains present.

- [ ] **Step 8: Push the feature branch and verify the remote**

```bash
git pull --rebase
git push -u origin feature/flpdf-oqdj-qpdf-job-json-output
git status --short --branch
```

Before the push, report that the operation will publish the branch. Expected: push succeeds and the local branch is clean and tracking its remote. Do not merge.
