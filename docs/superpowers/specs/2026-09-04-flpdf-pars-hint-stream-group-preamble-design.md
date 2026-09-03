# flpdf-pars: qpdf-shaped shared-object hint encoding

## Goal

Remove flpdf's latent shared-object `groups` pre-pass from the linearization
hint stream. The current production width is zero, so existing output bytes do
not change, but a non-zero width would shift every following field away from
the qpdf 11.9.0 layout.

## Oracle and current gap

qpdf 11.9.0 `QPDF::readHSharedObject` in
`libqpdf/QPDF_linearization.cc:374-407` reads the seven-field header and then
reads the shared-object columns in this order:

1. `delta_group_length` for every entry;
2. `signature_present` for every entry, including an inline 128-bit signature
   only when the flag is set;
3. `nobjects_minus_one` for every entry.

There is no separate group-entry column. qpdf's writer-side
`calculateHSharedObject` (`QPDF_linearization.cc:1569-1606`) likewise builds
only the per-object entries and their aggregate header values.

flpdf currently has an extra `SharedGroupEntry` vector and emits it from
`encode_shared_object_groups` before the three qpdf columns. The width is
currently hardcoded to zero in `hint_shared.rs`, making the divergence dormant.

## Design

### Data model

- Delete `SharedGroupEntry`.
- Delete `SharedObjectHintTable::groups`.
- Keep `SharedObjectHeader::bits_group_object_count`; it is the qpdf
  `nbits_nobjects` width and is used only for the `nobjects_minus_one` column.
- Preserve the one-object-per-group invariant by continuing to construct
  `SharedObjectEntry::nobjects_minus_one = 0`. No separate group vector is
  needed to represent that invariant.

### Encoder

- Delete `encode_shared_object_groups`.
- Remove its call and associated flush from `encode_shared_section`.
- Keep the existing column order, inline-signature handling, and byte alignment
  around the three qpdf columns unchanged.

### Tests and documentation

- First add a RED unit test using a non-zero group-count width and a distinct
  shared-entry payload. Decode the encoded bytes through the existing
  qpdf-shaped `read_h_shared_object` reader and assert that length, signature,
  signature bytes, and `nobjects_minus_one` are preserved. The current extra
  pre-pass must make this test fail by shifting the decoded fields.
- Update existing hint-stream and hint-shared fixtures and assertions to stop
  constructing or inspecting `groups`.
- Update the module documentation and test comments in `hint_shared.rs`,
  `hint_stream.rs`, and `show.rs` so they describe header plus three shared
  columns, not a serialized group section.
- Remove the obsolete `SharedGroupEntry` re-export from
  `linearization/mod.rs`.
- Update `docs/qpdf-correspondence.md` only where the existing linearization
  correspondence still claims a separate group section; do not add a new
  deviation marker because the change removes a qpdf-incompatible dormant
  representation.

## Alternatives rejected

1. Remove only the encoder call and retain `groups`: smaller diff, but leaves a
   qpdf-incompatible dead representation that can be reintroduced accidentally.
2. Derive and serialize group entries dynamically: still produces bytes that
   qpdf does not read and violates the oracle layout.

The selected data-model removal is the only option that makes the Rust
representation one-to-one with the qpdf shared-object table.

## Scope and non-goals

This change is limited to the shared-object hint table representation and its
tests/docs. It does not change object classification, linearization part
routing, overflow-stream splitting, DEFLATE implementation, signature
generation, or the existing one-object-per-group output policy.

## Verification

Run the focused hint-stream/hint-shared tests after the RED/GREEN cycle, then
the relevant linearization tests with `qpdf-zlib-compat`, formatting, all
workspace Clippy, strict private rustdoc, qpdf module-doc/deviation checks,
and the workspace tests. Confirm the worktree diff contains only this scoped
implementation, tests, and documentation.
