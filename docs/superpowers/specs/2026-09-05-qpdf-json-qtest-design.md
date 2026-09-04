# qpdf-json qtest parity design

## Goal

Make every subtest in qpdf 11.9.0's `qpdf-json.test` pass through the existing
flpdf Rust production paths and qtest process boundary. The authoritative target
is one isolated run with 145 total tests, 145 passes, and no failures or missing
tests.

## Evidence and scope

The current isolated run on flpdf main reports 130 passes and 15 failures:

- qpdf-json 120 and 122 compare JSON produced after `--pages . --`. qpdf runs
  `QPDFJob::handlePageSpecs` before `QPDFJob::doJSON`, which repairs and flattens
  the live page tree and pushes inherited attributes before serializing the
  object map (`libqpdf/QPDFJob.cc:428-480,1545-1619,2360-2632`,
  `libqpdf/QPDF_pages.cc:140-180,204-251`,
  `libqpdf/QPDF_optimization.cc:121-245`).
- qpdf-json 126-138 exercise qpdf-ctest tests 42-47. The C source calls the
  public JSON create/update/write contracts, but the qtest Rust process adapter
  currently dispatches only tests 1, 2, and 11-20
  (`qpdf/qpdf-ctest.c:1252-1320`, `libqpdf/qpdf-c.cc:1893-1950`).

The pinned qpdf 11.9.0 source and binary are the semantic and byte-output
oracle. The qtest vendor files remain unchanged. C ABI compatibility is outside
the scope; qpdf's portable observable behavior is reproduced by Rust consumers.

## Architecture

### Slice A: JSON page-selection consumer

The JSON CLI route will use the same open/update/page-selection/JSON ordering as
qpdf's `createQPDF` and `writeQPDF`. The route will apply the existing
`QPDFJob::handle_page_specs` and `pages/tree_rebuild` ObjectHandle-native
consumer before invoking the existing JSON writer. It will not duplicate page
tree traversal in `main.rs` or in the JSON serializer.

The document model will expose a monotonic observation corresponding to qpdf's
`everPushedInheritedAttributesToPages()`. The flag is set only by the canonical
`PageDocumentHelper::push_inherited_attributes_to_pages` boundary and is not
inferred from an empty value or from whether a particular page currently has a
resource. JSON v2 metadata will read this document state alongside the existing
`ever_called_get_all_pages` state. Page cache invalidation and live
ObjectHandle mutation remain owned by their existing modules.

The qpdf order is important: page selection can call `addPage`, whose flatten
operation invokes inherited-attribute normalization; JSON pages are then
generated first, followed by the qpdf object map. This produces the repaired
root `/Pages` `/Kids`, cloned duplicate leaves, leaf-level inherited resources,
and object identifiers observed in qpdf-json 120 and 122.

### Slice B: portable qpdf-ctest JSON consumer

The existing `qpdf-ctest` Rust binary will gain one dispatch function per qpdf
test responsibility:

- tests 42 and 43 create a document from JSON loaded from a file and from a
  buffer, then write a static-ID PDF;
- tests 44 and 45 read a PDF, apply JSON loaded from a file and from a buffer,
  then write a static-ID PDF;
- test 46 reads a PDF and writes complete JSON v2 with inline data and decode
  level `none`;
- test 47 reads a PDF and writes only object 4 and the trailer as JSON v2 with
  specialized decoding and file-backed stream data using the supplied prefix.

These functions call `Pdf::create_from_json`, `Pdf::update_from_json`, the
canonical `PdfWriter`, and the existing JSON `Pipeline`/side-file writer. They
preserve native path bytes at the process boundary and leave error/status
ownership with the canonical layers. No qpdf shell command, C library, local
JSON serializer, or hard-coded expected output is introduced.

### Slice C: qtest evidence

After slices A and B are built from committed binaries, run
`qpdf-json.test` in a disposable copied datadir. Preserve `harness.log` and
`qtest-results.xml` from that same invocation. Only after all 145 XML outcomes
are pass may the qtest parity ledger be reconciled; represented Rust-oracle
rows remain represented when that is the established ownership model.

## Alternatives considered

1. Add a special JSON-only page repair path. Rejected because it would duplicate
   `QPDFJob::handlePageSpecs` responsibility and could diverge from rewrite and
   page-operation behavior.
2. Implement qpdf's C ABI or shell out to qpdf. Rejected because it does not
   port qpdf behavior to Rust and would hide the production ownership boundary.
3. Route both cases through the existing canonical page/job/JSON and process
   adapter surfaces. Selected because it preserves qpdf call order, makes each
   missing consumer independently testable, and keeps the qtest driver thin.

## Testing and invariants

Each production change starts with a focused failing test and a recorded
qpdf 11.9.0 comparison. Tests cover the exact page JSON metadata/object graph,
static-ID PDF comparisons, complete and selected JSON bytes, side-file bytes,
argument errors, input/JSON errors, and output failure behavior. Existing page,
JSON, qpdf-ctest, workspace, formatting, all-feature clippy, strict rustdoc,
route/deviation, and fresh changed-line coverage gates remain required.

The final qtest acceptance evidence is:

```text
qpdf-json.test: total 145, pass 145, fail 0, unexpected pass 0, missing 0
```

The qtest suite's `vendor/qpdf-qtest/qpdf-json.test` is not edited, and an
allowlist or manifest state is never used to turn a failing command into a
passing command.
