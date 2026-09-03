# InPlace page-spec completion ownership

**Issue:** `flpdf-4wq4`

## Goal

Make the page-selection completion boundary shared by the two current
`PageSpecJobOutput::InPlace` consumers. The CLI and the production
`QPDFJob::run` path must perform the qpdf-shaped page-subset cleanup in one
ordered job-owned helper, so a future change cannot update only one consumer.

## qpdf oracle

qpdf has one `QPDFJob::createQPDF` sequence: it applies `handlePageSpecs`, then
`handleRotations`, then `handleUnderOverlay` and `handleTransformations`
(`libqpdf/QPDFJob.cc:428-535`). `QPDFJob::run` only connects that creation
stage to `writeQPDF` (`QPDFJob.cc:514-521`).

`handlePageSpecs` owns the page-selection mutation and final page ordering,
including source-page removal and AcroForm field filtering
(`libqpdf/QPDFJob.cc:2360-2632`). Rotation is a later operation over the
post-selection page list (`QPDFJob.cc:466-470`). The later transformation
stage is a separate qpdf responsibility (`QPDFJob.cc:2137-2210`) and must not
be reimplemented by a page-selection helper.

The live qpdf probe for an equivalent one-source page selection through the
CLI and job-JSON interfaces produced the same one-page page JSON, including
rotation. The probe also confirmed that both interfaces are expected to share
the same qpdf-owned lifecycle rather than define separate page-selection
semantics.

## Current flpdf boundary

`job/page_specs.rs::QPDFJob::handle_page_specs` already owns source selection
and returns `PageSpecJobOutput::InPlace` for a one-source selection. After that
return:

- `job/lifecycle.rs::run_document_erased` calls navigation remapping, subset
  prune, AcroForm prune, configured rotations, and its QPDFJob document stages.
- `flpdf-cli/src/main.rs::run_page_extraction_after_plan` repeats the page
  completion calls, currently applies CLI rotations before the cleanup calls,
  and also performs CLI-specific structural cleanup, image/output handling,
  overlays, split writing, and warning completion.

The structural cleanup functions already have qpdf-backed responsibilities
and are part of the page-subset boundary; they must be included in the shared
helper rather than silently dropped from the CLI route. CLI output/writer
handling is not part of qpdf `handlePageSpecs`, so it remains outside the
helper. `run_document_stages` remains the sole owner of the QPDFJob
transformation/inspection continuation; the CLI continuation remains its
separate output consumer until a distinct full QPDFJob CLI migration is
approved.

## Design

Add one job-owned `complete_in_place_page_selection` operation next to
`QPDFJob::handle_page_specs`. It accepts the live target PDF, the
`RebuildResult`, and the effective `RemoveUnreferencedResources` mode. In
qpdf order it:

1. remaps outlines and destinations;
2. drops dangling `/Pg` and `/P` structural references through the existing
   canonical helpers;
3. prunes page-local resources and writer-unreachable subset objects;
4. prunes the subset's AcroForm fields.

The helper returns the same `Result<()>` and uses the existing error/warning
boundaries. Both `run_document_erased` and
`run_page_extraction_after_plan` call it exactly once for `InPlace`. Both
apply rotation only after this helper, using their existing configuration
parsers and the shared `apply_rotate_to_pages` primitive. No callback-based
stage injection, proxy object, sentinel, or compatibility route is added.

The core QPDFJob route continues directly from the shared helper to its
existing `apply_configured_rotations` and `run_document_stages`. The CLI route
continues from the shared helper to its existing page-operation output stages;
its structural/output-specific code is kept explicit and is not presented as
qpdf `handleTransformations`.

## Testing

Add a source route-lock test requiring both InPlace consumers to call the
shared helper and requiring no duplicate direct calls to the owned page
completion operations. Add behavioral coverage for the helper's order and
for both callers: outline/destination remapping, structural `/Pg`/`/P` drop,
subset resource/AcroForm pruning, and rotation after cleanup. Existing qpdf
matrix tests remain the observable CLI oracle; the existing JSON job lifecycle
test remains the QPDFJob oracle. Run the full workspace and qtest suites,
including qpdf-ctest/C API cases, because this change touches the shared job
boundary.

## Non-goals

- Do not migrate all CLI argument parsing or writer/output ownership into
  `QPDFJob` in this issue.
- Do not remove public APIs or create a bridge around either existing route.
- Do not change qtest fixtures or add a qpdf-deviation marker.
