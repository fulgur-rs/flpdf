# flpdf-jwx4: optimization redirect-chain traversal

## Goal

Make `Optimization::update_object_maps` continue through the in-memory
reference-valued redirects that `Pdf::set_object` can create, without changing
the parsed-PDF qpdf contract.

## Oracle and boundary

qpdf 11.9.0's `QPDF::updateObjectMapsInternal` records each indirect handle and
recurses through arrays, dictionaries, and stream dictionaries
(`libqpdf/QPDF_optimization.cc:261-334`). Its file parser forms `N G R` only
while parsing array/dictionary elements; a top-level object body is not a bare
reference (`libqpdf/QPDFParser.cc:26-90,140-176`). Therefore a
reference-to-reference top-level value has no qpdf file-parse counterpart. It
is nevertheless reachable in flpdf through the public `Pdf::set_object`
mutation path and was followed by the pre-ObjectHandle traversal.

The existing qpdf null visibility is already correct and remains unchanged:
dictionary keys whose values resolve to null are omitted, while an indirect
null in an array retains its identity (`libqpdf/QPDF_Dictionary.cc:59-67,98-125`).
The current regression `dictionary_null_is_hidden_but_array_null_keeps_indirect_identity`
pins that behavior.

## Design

When a pending indirect handle has resolved to `ObjectValue::Reference`,
resolve the referenced canonical handle and enqueue it as the next pending
value. Record each distinct indirect redirect hop for the current user, as the
pre-migration traversal did. Apply the existing non-top `/Page` boundary to
each dequeued hop before recording that hop, matching the old reference-arm
ordering: an intermediate redirect owner is recorded, while a target that is
itself a non-top `/Page` is not. Keep the existing visited set and
inline-depth reset at indirect boundaries. Preserve the old `via_array` signal
for a direct array reference so a null target is retained only in that
position.

Use `Pdf::resolve` and `ObjectHandle::as_reference` on the existing canonical
handles. Do not add a second terminal resolver or a compatibility alias: the
per-hop object-user records are required by the existing flpdf in-memory graph
behavior, while qpdf-parsed values never enter this branch.

## Scope

- Change `crates/flpdf/src/optimization.rs` only for redirect-chain traversal.
- Add unit regressions for one-hop and multi-hop `Pdf::set_object` redirects,
  including the non-top page boundary.
- Retain the existing null visibility test unchanged.
- Update `docs/qpdf-correspondence.md` only if the implementation changes the
  documented optimization boundary; no new qpdf deviation marker is needed.

Out of scope: parser behavior, qpdf null visibility, `resolve_to_terminal_ref`,
writer layout, and the deferred final legacy-route deletion.
