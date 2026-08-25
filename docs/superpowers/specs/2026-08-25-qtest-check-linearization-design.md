# qtest `--check-linearization` canonical inspection route

## Goal

Make every `qpdf --check-linearization` invocation in qpdf 11.9.0's
`linearization.test` execute through flpdf's qpdf-shaped inspection lifecycle
and match qpdf's observable result for success, non-linearized input, and
linearization warnings/errors.

## Oracle and current gap

The pinned qpdf 11.9.0 source is authoritative:

- `QPDF::checkLinearization` (`libqpdf/QPDF_linearization.cc:70-81`) calls
  `readLinearizationData`, then `checkLinearizationInternal`, catches the
  runtime errors raised by those phases, and routes them through
  `linearizationWarning`. The return value is true only when no warning was
  recorded.
- `QPDF::isLinearized` and `readLinearizationData`
  (`libqpdf/QPDF_linearization.cc:84-155,161-230`) own detection and
  parameter/hint loading; they are not replaced with a filename-specific
  shortcut.
- `QPDFJob::doInspection` (`libqpdf/QPDFJob.cc:1646-1674`) owns the CLI
  contract: a non-linearized file prints `<input> is not linearized`, a clean
  check prints `<input>: no linearization errors`, and a warning result sets
  qpdf's warning exit status. The same opened document and logger are reused.
- The bare option is registered by `QPDFJob_config.cc:80-85` and
  `libqpdf/qpdf/auto_job_init.hh:40`.

flpdf already has the detector and deep checker in
`crates/flpdf/src/linearization/check.rs`, including a soft-warning route used
by generic `--check`. It does not yet have the corresponding
`QPDFJob::check_linearization` consumer. The CLI has only the flpdf-specific
`check-linearization` subcommand; its top-level `Cli` has no
`check_linearization` field. `arg_parser.rs` recognizes the bare option for
grammar normalization, but there is no clap field or dispatch branch to
consume it. Consequently qtest rejects the real qpdf-shaped command before a
PDF is opened.

## Design

### Canonical job consumer

Add `QPDFJob::check_linearization` in `crates/flpdf/src/job/check.rs`. It
accepts the already-open `Pdf`, installs the job logger, and retains the job's
input description. The method follows qpdf's call order:

1. Call `Pdf::is_linearized`.
2. If false, write `<input> is not linearized` and complete with success.
3. If true, read the same document source bytes through `Pdf::source_bytes`
   and invoke the existing qpdf-shaped soft checker.
4. Route every collected linearization warning through the job logger in order;
   convert malformed linearization data to qpdf's warning text rather than a
   Rust panic or a CLI-only error.
5. On no warnings, write `<input>: no linearization errors`.
6. Record warnings on the job and call the shared `complete(false)` boundary,
   so warning exit status, suppression, and logger failures are owned by
   `QPDFJob` rather than the CLI.

The current strict public checker remains available only as an underlying
library primitive where its existing contract is required. The qpdf CLI route
uses the warning-accumulating checker and does not reuse the old standalone
path wrapper or open a second document.

### CLI surface

Add a top-level clap bool for `--check-linearization` with the same inspection
conflicts as `--check`/`--show-linearization`. Add a `run_check_linearization`
function that opens the input once with a configured `QPDFJob`, invokes the new
consumer, and maps `JobExitCode` through the existing job-status adapter.
The existing subcommand delegates to this same canonical route so it cannot
develop different logger, filename, or warning semantics. The qpdf-shaped
top-level option remains the authoritative qtest entry point.

### Testing and qtest attribution

Add CLI process tests before production changes for:

- a clean linearized fixture, asserting exact qpdf output and exit 0;
- a non-linearized fixture, asserting exact qpdf output and exit 0;
- a malformed linearization parameter fixture, asserting qpdf warning text,
  shared warning completion, and exit 3;
- the existing subcommand, asserting it delegates to the same output route.

Use `/usr/bin/qpdf` 11.9.0 as the differential oracle where available and keep
qpdf-qtest fixtures in `/home/ubuntu/flpdf-qtest`. After the Rust route is
green, run only the qtest `linearization` suite first, preserving the paired
`harness.log` and `qtest-results.xml`. Update qtest parity attribution only
from that green run; do not remove blocked rows in advance.

## Error and lifecycle boundaries

- PDF damage discovered by the linearization checker is a qpdf warning and
  produces exit 3, unless qpdf's surrounding operation reports a true
  operation failure.
- Logger/pipeline failures propagate as `Error::System`/`Error::Internal`
  through the job API; they are not converted into malformed-PDF warnings.
- Input opening and password errors remain owned by the existing CLI/job open
  boundary. The check route does not reopen the source or create a second
  logger.
- No qtest shim translation or test-only bypass is added.

## Non-goals

- Changing linearized writer layout, hint-table generation, or object-stream
  placement.
- Rewriting qpdf-qtest fixtures or vendoring them into flpdf.
- Broad refactoring of unrelated inspection subcommands.

## Verification contract

The focused CLI tests, `cargo fmt --all -- --check`, relevant flpdf and CLI
tests, all-features clippy, and the focused qtest suite must be run. The full
workspace test result must report the pre-existing unrelated failure separately
if it remains; it must not be mistaken for this route's result. Before issue
closure, re-read Beads state, run `bd dep cycles`, require `bd dolt push` to
print `Push complete.`, and verify the exact qtest target rows from fresh
`harness.log` and `qtest-results.xml` artifacts.
