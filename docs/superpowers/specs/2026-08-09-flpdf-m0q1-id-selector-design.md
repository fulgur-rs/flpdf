# qpdf ID Selector Unification Design

## Goal

Make the full-rewrite and linearized writers reproduce qpdf 11.9.0's ordinary
and static `/ID` selection for empty `/ID[0]` values, while ensuring the
identifier used for encryption is the same identifier emitted in every trailer.
After cutover, remove private duplicate selector paths that no longer have a
consumer.

## Oracle model

qpdf `QPDFWriter::generateID` creates the changing identifier (`id2`) once.
It then reads the source permanent identifier (`id1`) and replaces it with
`id2` when the source value is empty. Consequently:

- a non-empty, supported source `/ID[0]` is preserved verbatim;
- an empty supported source `/ID[0]` makes `/ID[0] == /ID[1]`;
- `--static-id` uses the fixed pi value for `id2`, so an empty source produces
  the fixed value in both elements;
- explicit encryption derives its file key from that selected `/ID[0]`;
- linearization pass 1 still emits a same-width all-zero placeholder and does
  not select a second independent identifier.

The pinned source is qpdf 11.9.0 at commit `3b97c9bd`, especially
`libqpdf/QPDFWriter.cc:591-648`, `1194-1231`, and `1812-1909`.

Malformed `/ID` array shape remains the separate `flpdf-vqap` responsibility.
Copy-encryption remains separate because its donor supplies the permanent
identifier.

## Proposed architecture

`crates/flpdf/src/writer.rs` will own one qpdf-equivalent source-ID predicate
and one ordinary/static ID-array generator:

1. The source predicate accepts the existing supported two-string array only
   when its first string is non-empty. The existing deterministic-id path will
   reuse this predicate through `source_permanent_id`.
2. The generator creates `id2` once (the pi constant for static-id, otherwise
   `fresh_id_bytes()`), selects the non-empty source `id1` or clones `id2`,
   and returns the complete two-element `/ID` array.
3. Incremental, full-rewrite, and linearized ordinary/static callers use that
   generator. The old `apply_static_id`, `random_id_array`, and
   `resolve_id0_for_encryption` selector routes are removed after all consumers
   have moved; no compatibility bridge is introduced.
4. Full-rewrite explicit encryption computes the generated array once before
   building `EncryptionContext`, passes its first element to key derivation,
   and reuses the same complete array when writing both possible trailer forms.
   Copy-encryption continues to use its donor context and existing route.
5. Linearization calls the same generator before layout. It stores the complete
   array on the working trailer, extracts its first element for encryption, and
   leaves the pass-1 placeholder logic unchanged.

No sentinel, panic, or qpdf-incompatible intermediate representation is added.
The complete ID array is the direct representation of qpdf's `id1`/`id2`
members and is reused rather than regenerated at each output site.

## Verification design

RED tests will cover the selector before the implementation is changed:

- empty hex and literal source IDs produce equal, non-empty elements in the
  default full-rewrite and linearized outputs;
- empty source IDs produce the pi value in both elements for static-id;
- non-empty supported source IDs remain the first element while the second is
  generated or static as appropriate;
- explicit encryption uses the emitted first element and remains readable with
  the configured password;
- all repeated linearized `/ID` sites are identical and pass the linearization
  checker;
- existing malformed-shape and copy-encryption tests retain their boundaries.

The implementation will be checked with focused Rust tests, qpdf 11.9.0
`--check`/live probes, `cargo fmt --all -- --check`, the affected crate tests,
and the workspace quality gates before completion.

## Non-goals

- expanding malformed `/ID` array support from `flpdf-vqap`;
- changing deterministic-id's content-derived algorithm or its encryption
  restriction;
- implementing or changing copy-encryption semantics;
- retaining an old selector solely for API compatibility when it is private and
  no longer consumed.
