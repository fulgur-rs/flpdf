# Linearization Catalog Handle Resolution Design

## Goal

Migrate the active linearization writer's Catalog, `/Extensions`, `/ADBE`, and
`/Outlines` inspection from the raw `Object`/`resolve_borrowed` route to the
canonical `ObjectHandle` graph, while matching qpdf 11.9.0's writer ordering
and its acceptance of indirect document extensions.

## Oracle behavior

The pinned qpdf source is `/home/ubuntu/.cache/flpdf/qpdf-11.9.0` and the live
oracle is `/usr/bin/qpdf` 11.9.0.

`QPDFWriter::prepareFileForWrite` first calls `fixDanglingReferences`, gets the
live root, shallow-copies an indirect dictionary-valued `/Extensions` onto the
Catalog, and makes an indirect `/ADBE` value direct
(`libqpdf/QPDFWriter.cc:2034-2055`). `writeLinearized` then optimizes and
partitions the already-prepared graph (`QPDFWriter.cc:2537-2561`). During
`unparseObject`, qpdf preserves non-ADBE extension prefixes and replaces or
removes `/ADBE` according to key existence and the effective extension level
(`QPDFWriter.cc:1318-1437`).

Linearization categorization reads `/PageMode` and `/Outlines` through live
handles (`libqpdf/QPDF_linearization.cc:963-1043`), places the outline root and
its units (`:1405-1432`), and calculates the outline hint fields from the
resulting output numbering (`:1613-1631`).

A live probe using an indirect `/Extensions` dictionary containing `/ADBE` and
`/XYZW` confirmed that `qpdf --warning-exit-0 --linearize --static-id
--min-version=1.7.8` succeeds, emits a valid linearized PDF, directizes the
Catalog `/Extensions`, overwrites `/ADBE`, and retains `/XYZW`. Therefore the
existing flpdf rejection is not an acceptable parity boundary.

## Current gap

`linearization/writer.rs::compute_outline_hint_info` uses
`resolve_borrowed` and raw `Object` to obtain `/Outlines`. The same file's
`resolve_catalog_adbe_status` uses that route for `/Extensions` and rejects any
indirect value in the subtree. `write_linearized_for_pdf_writer` builds the
linearization plan before this extension reconciliation, so merely changing the
resolver would leave the plan/renumber graph inconsistent with qpdf.

## Design

1. Add a small canonical pre-plan Catalog preparation boundary in the
   linearization writer. Resolve the live Catalog handle; if `/Extensions` is
   an indirect dictionary, replace it with `ObjectHandle::shallow_copy()`; then
   if `/ADBE` is indirect, call `ObjectHandle::make_direct(false)` and replace
   that child. Mark the Catalog dirty once when a replacement occurs. This is
   the direct Rust translation of qpdf's `prepareFileForWrite`; it does not
   recursively normalize unrelated Catalog keys or invent a generic adapter.
2. Capture the output-only Catalog extension snapshot before this preparation
   so the public `PdfWriter` route can restore the caller's original extension
   entry after success or failure, while the plan and emitted bytes use the
   prepared graph.
3. Replace `compute_outline_hint_info`'s Catalog lookup with
   `get_object_handle` → `resolve` → `try_get_key`, retaining the existing
   ObjStm-container mapping and hint arithmetic.
4. Replace `resolve_catalog_adbe_status` with a handle-only visible-key check.
   Remove `orphans_indirect_object` and the associated Unsupported error. The
   effective `/ADBE` mutation remains on the writer's existing qpdf-shaped
   extension helpers, after the graph has been prepared before planning.
5. Retain the test-only frozen-plan helper only as a lower-level emission
   primitive. Its tests must stop asserting the flpdf-only indirect-extension
   rejection and use the canonical `PdfWriter` route for the behavior that
   depends on pre-plan preparation.

## Error and ownership behavior

Handle resolution errors propagate through the existing `Result` channel.
Non-dictionary `/Extensions` remains a non-extension value: qpdf does not
inspect it as an extension dictionary, while a requested effective extension
level may replace it through the existing injection path. No new warning,
sentinel, compatibility alias, or raw snapshot conversion is introduced.

## Tests

- A route-contract test will require both active Catalog helpers to contain no
  `resolve_borrowed`, `Object::Dictionary`, `Object::Reference`, or raw
  `as_dict()` access and will require the canonical handle operations.
- A committed one-page fixture with an indirect `/Extensions` dictionary will
  be compared byte-for-byte with qpdf's deterministic linearization golden.
- Unit tests will cover direct/indirect `/Extensions`, indirect `/ADBE`, a
  malformed non-dictionary value, `/PageMode /UseOutlines`, and restoration of
  the caller's original indirect Catalog entry.
- Existing linearization, encryption, hint-table, deterministic-ID, CLI, qpdf
  correspondence, workspace, strict rustdoc, clippy, and patch-coverage gates
  remain required.

## Non-goals

This slice does not remove raw routes from other linearization modules, replace
the test-only frozen-plan API wholesale, redesign hint-stream generation, or
rename `canonical_*` symbols. The naming cleanup is deferred until the legacy
consumer census proves that the prefix carries no remaining distinction.
