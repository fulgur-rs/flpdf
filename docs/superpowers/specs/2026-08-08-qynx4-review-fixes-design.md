# qynx.4 Review Fixes Design

## Scope

Address the three unresolved review threads on PR #677 without treating the
review text as the qpdf oracle. Two threads identify qpdf parity gaps and one
does not.

## Oracle classification

### Standard-output write failures: oracle mismatch

qpdf 11.9.0 creates its standard-output sink with `Pl_OStream`
(`libqpdf/QPDFLogger.cc:43-51`). `Pl_OStream::write` and `finish` call the
underlying C++ stream without checking or propagating its failure state
(`libqpdf/Pl_OStream.cc:22-34`). A real-PDF probe that redirected linearized
output to `/dev/full` exited 0 for both qpdf and flpdf. The review request to
restore Rust `Write` error propagation therefore conflicts with qpdf and will
receive an evidence-only reply; production semantics will not change.

### Page descriptions: oracle match

qpdf's `QPDFJob::doShowPages` writes each page and field directly to the info
pipeline inside the page loop (`libqpdf/QPDFJob.cc:843-874`). flpdf currently
builds one `String` for the complete document before calling the logger. This
changes peak-memory and partial-output behavior and must be corrected.

### Linearization pass 1 with final output on stdout: oracle match

`QPDFJob` gives the requested pass-1 filename to `QPDFWriter`
(`libqpdf/QPDFJob.cc:2907-2909`) and independently routes the final output to
the logger save pipeline (`libqpdf/QPDFJob.cc:3039-3054`). `QPDFWriter` opens a
dedicated pass-1 file, writes the first pass through `Pl_StdioFile`, and appends
its diagnostic offsets (`libqpdf/QPDFWriter.cc:2661-2668,2886-2900`). A
real-PDF probe with `--linearize-pass1=PATH ... -` exited 0 and created both
outputs under qpdf; flpdf emitted the final PDF, failed to copy the path named
`-`, exited 2, and created no pass-1 file.

## Design

### Incremental page output

Extract the page-description loop into a helper that accepts a `QPDFLogger`.
Each logical output line is sent to `logger.info` as soon as it is formatted.
The CLI wrapper supplies the process logger. Tests supply a logger with a
recording pipeline and assert both the exact concatenated bytes and multiple
incremental writes. No complete-document output buffer remains.

### Writer-owned pass-1 output

Keep the existing `write_linearized` API unchanged. Add an opt-in sibling API
for a pass-1 filename, backed by one internal implementation. The core
linearization writer owns pass-1 serialization and file creation, matching the
qpdf `QPDFWriter` responsibility boundary. It writes the actual first-pass
representation and qpdf-shaped diagnostic offset comments, rather than
copying the final PDF.

The existing API delegates with no pass-1 destination and pays no additional
serialization or I/O cost. The CLI passes `--linearize-pass1` into the new core
entry point and removes its post-write `std::fs::copy`. A pass-1 open or write
failure is returned before the final document is emitted, so stdout never
contains a successful-looking final PDF followed by exit 2.

Rejecting stdout plus pass 1 is not an option because qpdf supports the
combination. Capturing and duplicating final stdout bytes is also excluded: it
would preserve flpdf's false pass-1 semantics and leave the responsibility in
the CLI.

## Error handling

- Logger failures from incremental page output propagate through the existing
  `crate::Error` to `CliResult` boundary.
- Pass-1 file open/write failures use the existing file-aware error mapping and
  abort before final output.
- Standard-output terminal failures retain qpdf's nonfatal `Pl_OStream`
  behavior.

## TDD and verification

1. Add a failing test proving page descriptions reach the info sink through
   multiple writes while concatenating to the existing expected output.
2. Add failing core and CLI tests proving a real pass-1 file is distinct from
   the final linearized document and that stdout plus `--linearize-pass1`
   succeeds.
3. Implement the smallest source-derived changes and make each focused test
   green before proceeding.
4. Run focused logger/linearization/CLI tests, qpdf differential probes, fmt,
   denied-warning clippy, workspace tests, module-doc checks, and fresh patch
   coverage.

## GitHub thread handling

Reply to all three original inline threads after the verified commit is pushed.
Each reply states the oracle classification, qpdf source evidence, probe result
when applicable, action taken, and verification. Read every reply back. Do not
resolve any thread without a separate explicit request.
