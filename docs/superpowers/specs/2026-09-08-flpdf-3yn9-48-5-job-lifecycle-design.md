# QPDFJob lifecycle integration design

## Goal

Make flpdf's `QPDFJob` follow qpdf 11.9.0's public lifecycle: `create_qpdf`
performs document creation and configured transformations, `write_qpdf` selects
writing or inspection, and `run` is only the `create_qpdf` then `write_qpdf`
composition. Warning completion and exit-code calculation must happen once at
the enclosing job boundary.

## Oracle contract

qpdf's `QPDFJob::createQPDF` performs configuration validation, input creation,
JSON update, page selection, rotations, underlay/overlay, and transformations
before returning (`libqpdf/QPDFJob.cc:428-481`). `writeQPDF` selects
`doInspection`, `doSplitPages`, or `writeOutfile`, then observes and drains
document warnings, emits one completion summary, and reports memory usage
(`libqpdf/QPDFJob.cc:483-511`). `run` calls only `createQPDF` and `writeQPDF`
(`libqpdf/QPDFJob.cc:513-520`). `getExitCode` is a side-effect-free query over
encryption status, warning state, and `warnings_exit_zero`
(`libqpdf/QPDFJob.cc:535-564`). `doInspection` executes selected reports in
the fixed order at `libqpdf/QPDFJob.cc:1645-1693`.

## Current gap

flpdf's `create_qpdf` currently prepares only the ordinary update-JSON and
rotation path. `run_document_erased` and `run_document_stages` still perform
page selection, overlay, transformations, inspection selection, JSON output,
and file output together. `write_qpdf` requires an output path and returns a
status after doing output-specific completion. `complete` combines the warning
summary and status query, so direct stage calls can complete more than once.

## Chosen design

1. Keep `JobDocument = Pdf<Box<dyn ReadSeek>>` as the returned create-stage
   document type. Move all transformations that can be represented by the
   current job configuration into the create-stage preparation path. The
   page-spec branch returns the merged primary document only after the page
   sources have been copied or otherwise made safe for the returned document;
   provider-backed values must retain a valid source owner through the write
   boundary, and a regression test will prove that direct create/write still
   works.
2. Split the current stage runner at its operation boundary. A preparation
   helper performs underlay/overlay and `handleTransformations` order and
   returns the document. A write-stage dispatcher then chooses inspection,
   split, JSON, or ordinary writer based on `creates_output` and the configured
   operation. Report helpers remain report-only; they do not emit completion.
3. Make `run` call `create_qpdf`, then call the write-stage dispatcher. The
   direct `create_qpdf`/`write_qpdf` API therefore observes the same transformed
   graph as `run`, while user mutations inserted between the two calls remain
   visible to the writer.
4. Preserve `JobExitCode` as the Rust representation of qpdf exit constants and
   add a pure `get_exit_code` query for the completed job state. The final
   completion helper records/drains document warnings before the summary and
   leaves the query itself free of logger or document mutation. Encryption
   status jobs retain qpdf's special status codes and early create-stage return.
5. Keep existing standalone public inspection methods working through the
   report-plus-completion boundary, while the combined dispatcher uses report
   methods and completes once. Do not route qtest exception fixtures through a
   new compatibility shim.

## Error and warning behavior

- Creation failures are reported at the job boundary and do not enter the
  write stage.
- A write or inspection operation may return an operation error, but its
  warning collection remains available until the qpdf-equivalent completion
  boundary observes it.
- `suppress_warnings` suppresses only the completion message; it does not
  discard warning state. `warnings_exit_zero` affects only the returned status.
- The summary suffix distinguishes output jobs from inspections, matching
  qpdf's `operation succeeded with warnings` wording.

## Non-goals

- Do not merge or close the Beads issue in this implementation session.
- Do not alter qtest exception work or the separate qtest exception repository.
- Do not complete the later CLI direct-writer migration tracked by E-4/E-21;
  this slice changes the `QPDFJob` lifecycle and its existing consumers.
- Do not keep a second output/inspection implementation merely to preserve the
  pre-qpdf lifecycle shape.

## Verification contract

The implementation must have RED/GREEN coverage for direct two-stage use,
combined inspection with one summary, no-output inspection dispatch,
warnings-exit-0/no-warn, encryption status, output writes, split writes, and
user mutation after `create_qpdf` but before `write_qpdf`. Local quality gates
must include fmt, all-features clippy, strict private rustdoc, workspace tests,
qpdf module/deviation/route checkers, and fresh patch coverage.
