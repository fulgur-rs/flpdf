# qpdf Page Document Helper Boundary Design

## Context

`QPDFPageDocumentHelper` is qpdf's document-level facade for page operations.
Its seven public operations delegate to `QPDF` page-tree handling,
`QPDFPageObjectHelper` resource processing, and AcroForm annotation flattening.
The existing Rust `PageDocumentHelper` exposes a different, partly overlapping
API and calls the non-repairing `pages::page_refs` traversal.  Separately,
`page_extract.rs` already owns the fresh-document `emptyPDF() + addPage()`
flow used for page selection.

The boundary must follow qpdf 11.9.0 rather than merge these two responsibilities:
the page-document helper is a facade over a live document, while page extraction
creates a new document from selected source pages.

## Goals

1. Make `PageDocumentHelper` cover every public member of
   `include/qpdf/QPDFPageDocumentHelper.hh` in qpdf 11.9.0.
2. Preserve qpdf's repair-aware `getAllPages()` semantics before any
   document-level page operation.
3. Reuse the existing canonical Rust primitives for inherited attributes,
   page-tree rebuilding, resource pruning, and annotation flattening.
4. Document `page_extract.rs` as the distinct fresh-document
   `emptyPDF() + addPage()` path.

## Non-goals

- Do not move page selection/extraction into `PageDocumentHelper`.
- Do not introduce a second page-tree traversal, resource-pruning algorithm,
  or annotation-flattening implementation.
- Do not change CLI option behavior except by routing existing behavior through
  the canonical helper where it is already equivalent.

## Design

### Helper facade

`PageDocumentHelper` remains a borrowing wrapper around `&mut Pdf<R>`.  It
exposes qpdf-aligned methods for:

- repair-aware page enumeration;
- materializing inherited page attributes;
- pruning unreferenced resources on every current page;
- adding at the front or end, adding before or after a reference page, and
  removing a page; and
- flattening annotations using the existing flattening mode/flag surface.

The facade obtains its ordered page list from the same qpdf-compatible repair
path used by optimization.  It then delegates mutations to existing primitives;
there is no cached page list, so callers must enumerate again after mutation,
as in qpdf.

### Fresh-document extraction boundary

`page_extract.rs` retains ownership of `extract_pages` and `extract_page`.
These construct a fresh minimal target and populate it from source pages,
corresponding to qpdf's `emptyPDF()` plus `addPage()` use in page extraction.
Its module documentation states this explicitly and states that it is separate
from `pages.rs` traversal and the live-document helper facade.

### Compatibility and errors

All helper operations propagate the existing primitive errors.  Page positions
are validated before page-tree mutation.  A supplied reference page for
`addPageAt` must identify a page in the current repaired page list; otherwise
the operation returns the project's existing unsupported/missing-style error
without mutating the document.

## Verification

1. Add focused unit/integration tests for repair-aware enumeration, front/end
   insertion, before/after insertion, removal, and facade routing of resource
   pruning and annotation flattening.
2. Compare the new behavior with qpdf 11.9.0 probes where method semantics are
   ambiguous.
3. Run the affected `flpdf` tests, formatting, workspace clippy, workspace
   tests, and changed-line coverage.
