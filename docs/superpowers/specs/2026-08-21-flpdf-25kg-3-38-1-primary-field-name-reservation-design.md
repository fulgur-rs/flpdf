# flpdf-25kg.3.38.1 primary AcroForm field-name reservation

## Goal

Make the qpdf 11.9.0 `--pages` merge observable behavior match when a primary
input has a top-level AcroForm field on a page that is not selected. Its
original `/T` name must remain unavailable to later copied fields while qpdf
processes the selected page occurrences, even though the unselected field is
removed from the final `/Fields` array.

## Oracle boundary

The pinned qpdf 11.9.0 source and `/usr/bin/qpdf` are authoritative:

- `libqpdf/QPDFJob.cc:2516-2574` creates the referenced-field accumulator and
  calls `fixCopiedAnnotations` for each selected page in final order.
- `libqpdf/QPDFJob.cc:2600-2629` prunes unselected primary pages and then
  filters `/AcroForm/Fields` using the fields accumulated during the copy loop.
- `libqpdf/QPDFAcroFormDocumentHelper.cc:62-110` renames each copied field
  against the live qualified-name index before appending it to `/Fields`.
- `libqpdf/QPDFAcroFormDocumentHelper.cc:235-362` builds that index from the
  document's currently reachable field tree.

flpdf's `page_merge` primitive groups source pages for object-copy sharing,
while `job/page_specs` owns the final page-occurrence order. The qpdf name
reservation therefore belongs at the page-job copy boundary, not in a public
compatibility adapter or a legacy `Object` bridge.

## Design

The primary source's original top-level `/T` names are collected before the
grouped merge drops unselected fields. The final occurrence replay passes those
names as collision reservations to both same-document and foreign-document
annotation-copy routes. `AcroFormDocumentHelper` unions the reservations with
the live destination qualified-name cache only for the rename decision; it does
not create placeholder fields and does not change the final field list.

The generic `page_merge` route also records the primary names while building
its grouped AcroForm. The job route then reapplies the same reservation at the
canonical final-order copy boundary, after `replace_merged_fields` has removed
the grouped secondary copies.

## Acceptance criteria

- A primary with selected `F`, unselected `F+1`, and a secondary `F` produces
  `F` on output page 1 and `F+2` on output page 2 under both qpdf 11.9.0 and
  flpdf.
- The unselected primary field is absent from the final output; only its name
  participates in collision avoidance.
- Existing repeated-primary, foreign-page, field-order, field-trimming, and
  fields-less-AcroForm tests remain green.
- The changed Rust code passes formatting, focused tests, workspace tests,
  strict private-item rustdoc, all-features clippy, qpdf module-doc checks,
  and fresh patch coverage.
