# QPDFJob JSON Output Orchestration Design

**Status:** Approved on 2026-08-05

**Beads:** `flpdf-oqdj`; prerequisite of `flpdf-q2fo`

## Goal

Make flpdf's JSON file-mode output selection follow qpdf 11.9.0's
`QPDFJob::writeJSON` responsibility and call order. In particular, writing JSON
to stdout with `--json-stream-data=file` must require an explicit
`--json-stream-prefix`, while JSON written to a file continues to default the
stream prefix to the JSON output filename.

This is the first `job/` JSON orchestration slice. It does not claim that the
whole `QPDFJob` component is complete. The later `flpdf-q2fo` slice moves the
`doJSON*` implementations into the same boundary and completes the planned
JSON job layer.

## Oracle

Pinned qpdf 11.9.0 keeps these configuration values separate:

- `json_stream_data` (`include/qpdf/QPDFJob.hh:687`)
- `json_stream_prefix` (`include/qpdf/QPDFJob.hh:689`)
- `outfilename` (the output destination owned by `QPDFJob`)

`QPDFJob::writeJSON` (`libqpdf/QPDFJob.cc:3094-3115`) resolves them only after
the input document has been opened:

1. For file output, an empty prefix becomes the output filename before the
   output file is opened.
2. For stdout plus `qpdf_sj_file`, an empty prefix raises `QPDFUsage` before
   `doJSON` runs.
3. Otherwise the logger's standard-output pipeline is selected.
4. The selected pipeline and resolved prefix are consumed by `doJSON`.

`QPDFJob::usage` (`QPDFJob.cc:297-300`) only raises `QPDFUsage`. The executable
boundary catches it and renders the help block (`qpdf/qpdf.cc:9-23,37-38`).
The job layer therefore owns the usage condition and message, while the CLI
owns executable-name formatting and process exit.

The upstream regression is `qpdf/qtest/json.test:158-164`, with expected output
in `qpdf/qtest/qpdf/file-stdout-needs-prefix.out`.

### Observed behavior

The following probes used `/usr/bin/qpdf` 11.9.0:

- Clean input, stdout, file stream mode, missing prefix: exit 2, zero stdout
  bytes, the qpdf usage block on stderr, and no stream side file.
- The same input with `--json-stream-prefix=stream`: exit 0, JSON on stdout,
  and a referenced side file (`stream-7` for the probe fixture).
- Missing input with a missing prefix: the input-open error wins; no prefix
  usage error is emitted.
- Malformed unrecoverable input with a missing prefix: repair/open diagnostics
  and the terminal input error win; no prefix usage error is emitted.

These precedence observations are part of the required behavior. Moving the
missing-prefix check into clap or before `Pdf` open would be an oracle mismatch.

## Existing divergence

`crates/flpdf-cli/src/main.rs::run_json` currently combines the requested
stream-data mode and prefix too early:

```rust
JsonStreamDataMode::File {
    prefix: explicit_prefix
        .or(json_output_filename)
        .unwrap_or_else(|| "stream".to_owned()),
}
```

The string `"stream"` is a sentinel for state qpdf represents explicitly as
an empty `json_stream_prefix`. This erases the distinction required by
`QPDFJob::writeJSON` and puts a job-level policy in the CLI.

The correspondence table already records that `QPDFJob.cc` is smeared across
the CLI and library modules. `flpdf-q2fo` plans to create `job/`, but its stated
source range is `QPDFJob.cc:958-1620`; it omitted `writeJSON` at
`QPDFJob.cc:3094-3115`. `flpdf-oqdj` fills that dependency gap first, and
`flpdf-q2fo` now depends on it.

## Design

### Module boundary

Create `crates/flpdf/src/job/` with a JSON orchestration module. The module doc
states that it mirrors the JSON output-selection portion of qpdf 11.9.0
`QPDFJob.cc`. It describes only the current public behavior and source
responsibility; internal Beads identifiers and future migration notes stay in
this design document rather than published rustdoc.

The slice introduces five concepts:

```rust
pub enum JsonStreamData {
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

pub struct UsageError {
    message: String,
}

pub enum JsonJobError {
    Usage(UsageError),
    Output(JsonOutputError),
}
```

`JsonStreamData` is the unresolved job configuration corresponding to qpdf's
`qpdf_json_stream_data_e`. It deliberately has no prefix payload.
`JsonJobOptions::stream_prefix` independently preserves whether the caller
actually supplied a prefix.

`JsonJobOutput` preserves the output filename alongside an injected Rust
writer. Injecting `Write` replaces qpdf's configurable logger/pipeline
container without changing output bytes or ownership of the selection policy.
The CLI may retain its existing same-file identity checks and safe file-open
mechanism; after opening the destination it passes the filename and writer to
the job layer.

`UsageError` is the Rust signal corresponding to `QPDFUsage` for this slice.
It carries only the qpdf message. It does not print, choose a program name, or
exit the process.

`JsonJobError` preserves that usage category separately from incremental JSON
conversion/output failures. The CLI pattern-matches the category rather than
recovering it from message text.

### JSON write flow

The job entry point accepts an already-open `Pdf`, unresolved job options, and
the output destination:

```rust
pub fn write_json<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    options: JsonJobOptions<'_>,
    output: JsonJobOutput<'_>,
) -> Result<(), JsonJobError>;
```

It resolves `JsonStreamDataMode` exactly once:

- `None` becomes `JsonStreamDataMode::None`.
- `Inline` becomes `JsonStreamDataMode::Inline`.
- `File` plus an explicit prefix uses that prefix for either destination.
- `File` plus no prefix and `JsonJobOutput::File` uses the output filename.
- `File` plus no prefix and `JsonJobOutput::Stdout` returns
  `UsageError("please specify --json-stream-prefix since the input file name is unknown")`.

After resolution it calls the existing
`write_qpdf_json_v2_selected_objects_to_output_with_options` implementation.
That function is the current flpdf correspondence for qpdf's `doJSON` plus
`QPDF::writeJSON`. This slice does not copy, wrap with a callback, or create a
second JSON body writer. `flpdf-q2fo` later moves the `doJSON*` responsibility
behind `job/` without changing this command boundary.

### CLI boundary

`run_json` retains argument spelling validation, input/output identity safety,
password handling, PDF open, and warning emission. It changes as follows:

1. Parse `--json-stream-data` into unresolved `JsonStreamData`; invalid values
   remain argument errors before input I/O.
2. Do not invent a prefix and do not construct `JsonStreamDataMode` in the CLI.
3. Open the input and `Pdf` before calling the job entry point.
4. Pass stdout or the safely opened output file as `JsonJobOutput`. Opening the
   Rust `Write` container at this boundary is the existing file-identity safety
   mechanism; the job layer still owns the destination-dependent prefix and
   stream-mode policy.
5. Map `UsageError` to the qpdf-shaped help block using `progname()` and exit 2.
6. Leave conversion, pipeline, side-file, and ordinary I/O failures on the
   existing error path.

The `--json-stream-prefix` help text changes from the incorrect
`"--json-output path or stream"` default to: file output defaults to the JSON
output filename; stdout file mode requires an explicit prefix.

## Error and side-effect ordering

The following observable ordering is required:

1. clap and enum-value validation
2. input/output same-file preflight
3. input file open and PDF open/repair
4. safe construction of the injected top-level output writer, when file output
   was requested
5. job destination policy and missing-prefix usage check
6. `doJSON`-equivalent incremental output and side-file creation

For stdout file mode with no prefix, step 4 has no file to open and step 5
produces no stdout bytes and no side files. Input-open and unrecoverable parse
failures at step 3 remain more important than the prefix error, matching qpdf.

For output-file mode, the existing verified-open safety remains in force. The
job layer derives the default side-file prefix from the filename carried by
`JsonJobOutput::File`, never from a synthetic sentinel. The injected Rust
writer may be opened before the job call, unlike qpdf's internal `safe_fopen`,
but prefix derivation has no side effects and file mode has no missing-prefix
error; the externally observable error and output order is unchanged.

## Tests

TDD begins with a focused CLI regression that fails because current flpdf exits
0 and writes a `stream-*` side file:

- valid stream-bearing PDF
- stdout output
- `--json-stream-data=file`
- no `--json-stream-prefix`
- expected exit 2, empty stdout, qpdf-shaped stderr, and no side file

Additional tests cover:

- a live qpdf/flpdf differential using `FLPDF_PROGNAME=qpdf`
- explicit prefix with stdout remains successful and writes the referenced
  side file
- JSON file output without an explicit prefix remains successful and uses the
  JSON output filename as the side-file prefix
- nonexistent input reports the input-open failure rather than the prefix
  usage error
- malformed unrecoverable input reports its input failure rather than the
  prefix usage error
- job-layer unit tests pin all destination/mode/prefix combinations without
  process exits

The focused CLI test is run once before production changes to demonstrate RED,
then again after the minimal implementation to demonstrate GREEN.

## Scope boundaries

Included:

- qpdf `QPDFJob::writeJSON` output-selection responsibility
- unresolved stream mode/prefix representation
- usage signal and CLI formatting
- removal of the `"stream"` default
- caller migration from the inline CLI policy
- help correction and focused regression/differential tests

Excluded:

- moving or rewriting `doJSONPages`, `doJSONPageLabels`, `doJSONOutlines`,
  `doJSONAcroform`, `doJSONEncrypt`, or `doJSONAttachments`
- JSON v1, JSON input, or job JSON configuration
- page operations, checks, attachments, writer orchestration, or the complete
  public `QPDFJob` API
- unrelated JSON serialization or stream decoding changes

The correspondence table remains `QPDFJob.cc` = partial/smeared after this
slice. Completion is not claimed until the later Phase 2 job slices satisfy
their full D1-D5 gates.

## Verification gates

- `cargo fmt --all -- --check`
- focused RED/GREEN CLI and job tests
- `cargo test -p flpdf-cli --test cli_json`
- `cargo test -p flpdf-cli`
- `cargo test -p flpdf`
- `cargo test`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- fresh patch coverage against the branch base
- pinned `/usr/bin/qpdf` differential/probes
