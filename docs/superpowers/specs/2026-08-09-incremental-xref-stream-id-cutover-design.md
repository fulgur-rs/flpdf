# Incremental xref-stream ID cutover

## Context

The incremental writer currently selects the ordinary/static `/ID` in
`write_incremental_trailer`. When the source's last xref form is a stream,
`write_incremental_xref_stream` clones the source trailer and writes the
reader-visible `/ID` in the xref-stream dictionary before the trailing classic
trailer is written. The trailing dictionary is ignored because `startxref`
points at the xref stream, so `static_id` is not visible to readers.

qpdf 11.9.0 keeps one lazily-selected ID (`QPDFWriter::generateID`,
`libqpdf/QPDFWriter.cc:1823-1909`) and uses the same trailer data while writing
the xref stream (`writeXRefStream` / `writeTrailer`,
`libqpdf/QPDFWriter.cc:2392-2494`). The xref-stream form has no second classic
trailer after the stream.

## Design

`write_pdf_incremental` selects the ordinary/static ID array once, before
choosing the xref form. The selected value is passed to every writer that can
emit it.

- For `XrefForm::Stream`, the xref-stream dictionary receives the selected
  `/ID`, and the stream branch emits the final `startxref`/`%%EOF` directly.
  It does not call `write_incremental_trailer`; the obsolete trailing classic
  trailer is removed from this route.
- For `XrefForm::Table`, the existing classic xref table plus trailer remains.
  This preserves incremental compatibility for table-form inputs and old PDF
  versions; retiring that separate format is outside this review fix.
- The xref-stream helper's interface makes the selected ID explicit rather
  than deriving or inheriting a second value from the source trailer.

This leaves one canonical ID decision per incremental write while retaining
the format-specific xref writer required by the source document.

## Verification

Add a regression test using
`tests/fixtures/compat/three-page-objstm.pdf`, mutate a reachable object, and
write incrementally with `static_id`. Reopen the result through
`load_xref_and_trailer` and assert that the reader-visible xref-stream
`/ID[1]` is qpdf's pi constant and `/ID[0]` is preserved from the source. Also
assert that the stream route has no trailing classic `trailer` section.

Run the focused writer test, the full `writer_tests` integration test, library
unit tests, formatting, diff checks, and qpdf/flpdf validity checks. The source
prefix must remain byte-identical, and the existing table-form incremental
tests must continue to pass.

## Non-goals

- Do not remove classic xref-table support for table-form input.
- Do not change full-rewrite, QDF, linearization, or encryption ID policies.
- Do not add a compatibility trailer solely to preserve the current malformed
  stream-route shape.
