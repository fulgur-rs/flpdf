# flpdf-egzr.3.2.6.30 page-merge job ownership

## Goal

Move the existing canonical page-merge implementation under `crates/flpdf/src/job/`
so the `--pages` operation has one job-owned module boundary. Preserve the
current qpdf 11.9.0 observable behavior and public root exports without leaving
a second implementation or a forwarding compatibility module at the old
top-level `page_merge` path.

## Oracle boundary

The pinned qpdf 11.9.0 source is authoritative:

- `libqpdf/QPDFJob.cc:462-472` invokes `handlePageSpecs` as part of the job
  lifecycle before later job operations.
- `libqpdf/QPDFJob.cc:2360-2632` owns page-spec parsing, source lifetime,
  selection/collation, per-occurrence page copying, AcroForm handling, and
  final removal/pruning.
- `libqpdf/QPDFJob.cc:2517-2585` performs the page-copy loop in final parsed
  specification order, and `:2600-2629` performs the primary-page and
  AcroForm cleanup after that loop.

The current flpdf route is mixed: `page_merge.rs` owns the structural merge
and primary/catalog/AcroForm copy machinery, while `job/page_specs.rs` owns
the qpdf page-job ordering and resource-mode orchestration. The move is a
responsibility-boundary refactor only; it does not add a semantic adapter or
alter the canonical ObjectHandle route.

## Design

- Move `page_merge.rs` to `job/page_merge.rs` as the sole implementation.
- Make `job` the internal owner of the module and re-export only the public
  `merge_documents`/`MergeInput` API from the established crate root.
- Update `job/page_specs.rs`, module exports, intra-doc links, and tests to use
  the job-owned path.
- Remove the old top-level `page_merge` module; do not leave a forwarding
  compatibility wrapper or duplicate implementation.
- Keep raw `Object` cleanup, page-group consumer migration, and semantic gaps
  in their owning issues.

## Acceptance criteria

- `flpdf::job::merge_documents` is the public ownership route and the old
  `flpdf::page_merge` module no longer exists.
- Existing library, CLI, page-operation, AcroForm, PageLabels, and qpdf
  differential tests remain green with no output/exit-status regression.
- The moved module has no new `resolve_borrowed` or raw-route bridge.
- Format, strict private-item rustdoc, all-features clippy, workspace tests,
  qpdf module/deviation checks, and fresh per-PR patch coverage pass.
