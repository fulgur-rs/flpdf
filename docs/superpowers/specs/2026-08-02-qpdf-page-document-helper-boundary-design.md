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

### Direct catalog `/Pages` root

qpdf's `QPDF::flattenPagesTree`, `insertPage`, and `removePage` operate on a
`QPDFObjectHandle`, so a direct `/Pages` dictionary embedded in the catalog
remains direct during non-final insertion and removal. Rust's rebuild boundary
therefore receives the repaired root location as either an indirect object or
the catalog-owned direct dictionary; it must not mint a replacement indirect
root. The direct branch rewrites the catalog's `/Pages` dictionary in place and
reparents every flattened leaf to the final direct root value. The same
selection, cloning, and inherited-attribute materialization logic is shared
with the indirect-root branch.

### Direct page-tree handles and caller depth limits

The direct-root ownership boundary must also apply while finding the effective
root and resolving inherited attributes. qpdf uses `QPDFObjectHandle` for both
paths: `getAllPages()` follows `/Parent` while the current handle is a
dictionary, whether that dictionary is direct or indirect, and
`pushInheritedAttributesToPage()` traverses direct `/Kids` entries in the same
way. The Rust translation therefore uses one internal parent cursor that can
hold either an indirect object reference or an owned direct dictionary. It
supports the catalog `/Pages` correction path and the `/Resources`, `/Rotate`,
`/MediaBox`, and `/CropBox` inherited-value walks without materializing a
direct root into a new indirect object.

`rebuild_page_tree_with_max_depth` must pass its supplied bound through the
repair traversal as well as its existing inherited-value walks. qpdf has no
public depth-limit parameter, but the Rust API does; using the default only for
repair would make the same caller-supplied limit observe two different trees.
`prepare_for_optimization` remains the default-bound wrapper used by qpdf-like
public helper operations, while an internal `with_max_depth` entry point keeps
the bound coherent for rebuilding.

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
2. Exercise front/end insertion and non-final removal against a direct catalog
   `/Pages` dictionary; assert that it stays direct while `/Kids`, `/Count`,
   and each flattened leaf's `/Parent` match qpdf's direct-handle semantics.
3. Compare the new behavior with qpdf 11.9.0 probes where method semantics are
   ambiguous.
4. Run the affected `flpdf` tests, formatting, workspace clippy, workspace
   tests, and changed-line coverage.
5. Verify a direct catalog page value with an indirect `/Parent` is corrected
   to the real root, and verify direct `/Parent` dictionaries supply inherited
   resources and rotation during helper mutation and resource pruning.
6. Verify a small `rebuild_page_tree_with_max_depth` bound rejects an overly
   deep repaired tree before any page-tree mutation.
