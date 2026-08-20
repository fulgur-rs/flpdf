# Embedded-files ObjectHandle Cutover Design

## Goal

Move the five production `resolve_borrowed` entry points in
`crates/flpdf/src/embedded_files.rs` onto the canonical `ObjectHandle` and
`HandleNameTree` route, preserving qpdf 11.9.0 observable behavior and
removing the legacy consumer dependency.

## Oracle and responsibility

qpdf 11.9.0 owns the embedded-file document helper. Its constructor reads
`/Root -> /Names -> /EmbeddedFiles` through `QPDFObjectHandle` accessors
(`libqpdf/QPDFEmbeddedFileDocumentHelper.cc:33-45`). Listing and lookup keep
the live value handles returned by `QPDFNameTreeObjectHelper` (`:72-95`),
replacement initializes the missing tree and inserts a handle (`:53-69,
:97-103`), and removal removes the name-tree entry before replacing an
indirect value with a direct null in the document cache (`:105-121`).

The Rust implementation already has the corresponding live helper and
`HandleNameTree`. This change is a consumer cutover, not a new embedded-file
feature and not a change to the correspondence row for the D1 helper.

## Data flow

1. Resolve the catalog, `/Names`, and `/EmbeddedFiles` holders as canonical
   handles. Missing or non-dictionary nodes return the existing empty/false
   result.
2. Traverse, insert, and remove through `HandleNameTree`; name-tree values
   remain direct or indirect `ObjectHandle`s and retain their identity.
3. For `/AF` cleanup owned by `remove_attachment`, mutate the live array handle
   in place. Remove `/AF` from its parent only when the resulting array is
   empty, and preserve shared indirect arrays.
4. Keep `collect_embedded_file_pairs_raw` only as the explicitly recorded raw
   projection boundary needed by the remaining attachment cleanup route. An
   indirect handle is projected as its `ObjectRef`; a direct handle is
   materialized only at that boundary. No new legacy bridge is introduced.

## Scope boundaries

In scope:

- production call sites at the five locations recorded by `flpdf-3yn9.23`;
- the existing bounded-list entry point, using the same depth limit without
  changing the qpdf-shaped default helper path;
- direct and indirect `/Names`, `/EmbeddedFiles`, filespec, and `/AF` values;
- dirty propagation and retained-handle identity after mutation.

Out of scope:

- the test-only `/EF` stream helper and test-module `resolve_borrowed` calls;
- removal of the raw projection itself, owned by the final consumer-cutover
  issue `flpdf-egzr.3.2.8`;
- JSON attachment duplication, which belongs to `flpdf-q2fo`;
- new compatibility aliases, sentinel values, or fallback adapters.

## Error and mutation behavior

Canonical handle resolution errors propagate unchanged. Missing/non-dictionary
paths retain the existing `Ok(None)`, `Ok(false)`, or empty-list behavior.
Handle mutation uses the existing ownership checks and
`Pdf::mark_object_handle_dirty`; it does not rewrite a cloned dictionary with
`Pdf::set_object`. Removing an indirect filespec uses the existing canonical
`remove_object_handle` behavior, matching qpdf's null replacement, while
`remove_attachment` retains its separate `/AF` and reachability responsibilities.

## Verification

The first regression test must fail against the current raw route by asserting
that a retained canonical names/tree handle observes the legacy public insert
operation. The implementation then makes that test pass. Existing embedded
file tests must continue to cover malformed paths, direct-kid repair, deep
trees, direct filespec identity, removal, and attachment garbage collection.
The final verification includes the pinned qpdf attachment probe, focused and
workspace tests, formatting, strict private rustdoc, all-features clippy,
qpdf module-doc checks, and fresh patch coverage.
