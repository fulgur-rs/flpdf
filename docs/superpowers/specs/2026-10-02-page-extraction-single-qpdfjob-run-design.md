# Page Extraction Through One `QPDFJob::run()`

Date: 2026-10-02
Parent: `flpdf-3yn9.48.180`
Status: design for review

## Goal

Move every CLI operation that extracts pages into one configured `QPDFJob` whose `run()` owns input creation, page selection, transformations, output writing, warning completion, and exit status. This is the selected Native direction for the remaining E-19/E-21 page-extraction split.

The goal is a single lifecycle owner per extraction invocation. It is not enough for several Jobs to expose the same public methods or for the final writer alone to use `QPDFJob`.

## Current routes

There are two page-extraction shapes in `crates/flpdf-cli/src/main.rs`:

- Top-level `--pages` reaches `run_page_operations_with_qpdf_job`. One Job already owns both stages, but the caller manually invokes `create_qpdf()`, performs optional linearization content normalization, then invokes `write_qpdf()`.
- `rewrite --pages` reaches `run_page_extraction`, which splits into empty-primary, same-primary, and distinct-source paths. Those paths prepare a document with one Job, then `run_page_extraction_after_plan` / `finish_page_extraction` may rebuild the page tree, create a transformation Job, and create another split or ordinary writer Job.

The qpdf 11.9.0 CLI calls `QPDFJob::initializeFromArgv`, `QPDFJob::run`, and `QPDFJob::getExitCode` (`qpdf/qpdf.cc:26-44`). `QPDFJob::run` calls `createQPDF` and then `writeQPDF` on the same instance (`libqpdf/QPDFJob.cc:535-557`). `createQPDF` applies update-from-JSON, page specs, rotation, overlay/underlay, and transformations before returning (`QPDFJob.cc:428-480`); `writeQPDF` selects ordinary or split output and completes warnings/status after writing (`QPDFJob.cc:483-511`).

The Rust `QPDFJob::run` already owns `create_qpdf` and `write_qpdf` (`crates/flpdf/src/job/lifecycle.rs`). The design therefore consolidates CLI configuration and removes caller-owned intermediate documents and Jobs; it does not introduce another compatibility facade.

## Selected architecture

### One extraction Job builder

Use one shared page-extraction configuration path for the top-level CLI and `rewrite` command. It configures the original input or `empty_input`, password and recovery policy, update-from-JSON where supported, page specs and their source passwords, collate, resource policy, rotations, overlays, image and page transformations, split behavior, encryption/decryption, writer options, output destination, progress, verbosity, and warning suppression on one `QPDFJob`.

After CLI syntax and usage preflight, the route calls `job.run()` exactly once and maps the returned `JobExitCode` through the existing CLI status boundary. The caller does not hold a `Pdf` between create and write stages.

### Job-owned transformations and output

The existing page-selection-specific manual rebuild and completion helpers are retired after their required semantics are represented by `QPDFJob` configuration or a Job-owned stage invoked by `run()`. If an existing configuration operation cannot preserve a supported behavior, add that operation to the canonical Job lifecycle and test it there; do not retain a second CLI Job or pass a prepared `Pdf` through a parallel route.

`--split-pages` is configured on the same Job so `write_qpdf` selects its normal or split writer branch. Overlay source documents remain owned by that Job until its write stage finishes. Source warnings stay on the same Job and are reported once by its write completion. Input version floors, encryption preservation/copying, and output resource policy are collected and applied within the same lifecycle.

The `--linearize --normalize-content` page-extraction path currently has a CLI-owned normalization step between `create_qpdf` and `write_qpdf`. Its output and diagnostic behavior must be preserved while moving that work under the Job lifecycle, so `run()` remains the only create/write boundary.

### CLI boundaries retained

CLI parsing and preflight remain at the CLI boundary where they are needed to preserve usage-error order, positional input/output mapping, the `.` page-source spelling, same-file checks, output reservation, and raw page-source/password tokens. These checks must not open or mutate documents in a second Job.

The scope includes top-level `--pages`, `rewrite --pages`, `--empty --pages`, same-input and `.` selections, repeated and distinct page sources, and split output from an extraction. The no-`--pages` `--rotate`/`--split-pages` rewrite path is outside this slice.

## Behavior contract

The cutover preserves qpdf 11.9.0 observable behavior for each supported extraction form:

- output bytes where deterministic comparison applies, including qpdf-zlib-compat comparisons;
- exit status, stdout, stderr, warning summary count and ordering, and usage-versus-operation error treatment;
- page selection and collate order, source-password handling, empty-primary behavior, and multiple-source behavior;
- overlay/underlay ordering and provider lifetime through the write;
- image, rotation, appearance, annotation, coalescing, flattening, and resource-policy behavior;
- split output, encryption preservation/copy/decrypt, version floor, standard-output routing, and warning suppression.

Existing behavior tests remain in place. New route contracts require each extraction entrypoint to configure one `QPDFJob` and call `run()` once, with no CLI-owned `create_qpdf`/`write_qpdf` pair, intermediate transform Job, or independent split/writer Job in the extraction path.

## Validation

Use qpdf 11.9.0 as the behavioral oracle. Focused differential coverage must include the existing page-operations matrix for same-source selection, cross-document merge, empty primary, collate, overlay/rotation, split, encryption, warning/error paths, and deterministic output. Add regression coverage for any current behavior that only exists in the manual completion path, especially linearization content normalization and source warnings.

Run the CLI page-operation test suites, route-contract tests, formatting, all-feature workspace Clippy, strict rustdoc, route-matrix/deviation checks, and patch coverage before proposing the implementation branch for review. Update the E-19/E-21 route evidence from the resulting source. Do not classify the entire E-19/E-21 row canonical solely because this page-extraction slice is complete if other callers still keep either row mixed.

## Non-goals

- Changing the qpdf option parser or the `initializeFromArgv` boundary.
- Reworking no-`--pages` rotation/split behavior.
- Changing PDF writer internals beyond a Job-owned stage required to keep `run()` as the sole lifecycle boundary.
- Changing page identity or raw `QpdfObjGen` behavior.
- Closing `flpdf-3yn9.48.180` until all remaining parent acceptance criteria are satisfied on integrated main.
