# Overlay Destination Page Handle Resolution Design

## Goal

Migrate the active overlay/underlay destination-page rewrite from the raw
`Object`/`resolve_borrowed` snapshot route to the live `ObjectHandle` graph.
The change must preserve qpdf 11.9.0 page ordering, source annotation copying,
malformed-page errors, and dirty propagation.

## Oracle behavior

The pinned qpdf source is the repository-managed read-only tree printed by
`scripts/fetch-qpdf-source.sh --print-path`; the behavioral oracle is
`/usr/bin/qpdf` 11.9.0.

`QPDFJob::doUnderOverlayForPage` places each source Form XObject and then calls
`dest_page.copyAnnotations` (`libqpdf/QPDFJob.cc:1859-1911`).
`QPDFPageObjectHelper::copyAnnotations` reads source `/Annots`, preserves the
destination page's existing live annotation array, creates an array when the
destination value is not an array, and appends the transformed annotations
(`libqpdf/QPDFPageObjectHelper.cc:992-1039`).

`QPDFJob::handleUnderOverlay` keeps the destination page object live while it
replaces `/Resources` and `/Contents` in underlay, original, overlay order
(`libqpdf/QPDFJob.cc:1937-2031`). It does not resolve a raw page dictionary,
copy an annotation snapshot, and install that snapshot after annotation copy.
The underlying live access and mutation boundary is
`QPDFObjectHandle::getKey`/`replaceKey`
(`libqpdf/QPDFObjectHandle.cc:979-989,1200-1217`).

A live probe with `link-annot-no-acroform.pdf` as the destination and
`one-page.pdf` as the overlay completed with exit 0 and `qpdf --check` exit 0.
The output retained destination `/Annots [4 0 R]` while installing `/Fx0` and
`/Fx1`, confirming that no annotation snapshot is needed.

## Current gap

`crates/flpdf/src/job/overlay.rs::apply_overlays_to_page_with_sources` already
copies source annotations through
`PageObjectHelper::copy_annotations_from`. At the end it then calls
`Pdf::resolve_borrowed` to clone the current `/Annots` value into a raw page
dictionary, and `page_dictionary` calls `resolve_object` before replacing the
page object. This duplicates the page and annotation state after the canonical
helper has mutated it.

## Design

1. Add a small canonical resolver for the destination page that obtains
   `Pdf::get_object_handle`, calls `Pdf::resolve`, and validates a dictionary
   with `ObjectHandle::try_as_dictionary`. Keep the existing unsupported error
   text for a non-dictionary page.
2. Build the new `/Resources` value as an `ObjectHandle` dictionary whose
   `/XObject` children are the existing destination-owned handles for `/Fx0`
   and `/Fx1` onward. Build `/Contents` from the destination handle for the
   newly allocated stream object.
3. Replace only `/Resources` and `/Contents` on the live page handle and mark
   that handle dirty. All other keys, especially `/Annots`, remain in the same
   canonical page object that `copy_annotations_from` updated.
4. Remove `page_dictionary`, the raw `/Annots` snapshot, and their obsolete
   raw-route unit test. Keep the separate `overlay_annotations.rs` cleanup in
   `flpdf-3yn9.37`, and do not rename `apply_overlay_specs` or any
   `canonical_*` symbol.

## Error and ownership behavior

Resolution and typed-accessor errors propagate through the existing `Result`
boundary. The page resolver retains the current non-dictionary error. Every
new child in the resource dictionary is obtained from the destination PDF, so
`ObjectHandle::replace_key` ownership checks pass without an adapter or raw
materialization. Annotation copying continues to own source/destination
AcroForm reconciliation and its existing qpdf-compatible malformed-value
behavior.

## Tests

- Add a route-contract test for the active rewrite that requires the canonical
  page-handle operations and rejects `resolve_borrowed`, `resolve_object`,
  `live_annots`, and `page_dictionary` in that production slice.
- Add a byte-golden test using an existing destination page annotation and a
  content-only overlay; compare the library output byte-for-byte with qpdf.
- Retain the existing overlay/underlay annotation-copy, malformed-page, and
  multi-source qpdf gates, and migrate the non-dictionary helper test to the
  canonical page resolver.

## Non-goals

This slice does not remove raw routes from other job or overlay modules, delete
`overlay_annotations.rs`, change annotation transformation semantics, or
rename `apply_overlay_specs`/`canonical_*` symbols. Those remain separately
tracked work.
