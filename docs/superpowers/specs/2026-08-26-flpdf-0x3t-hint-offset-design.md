# flpdf-0x3t: hint-object damagedPDF offset design

## Goal

Make the `show-linearization` warning for a non-stream hint object report the
same source offset as qpdf 11.9.0 when the parsed object's trailing token is
not literally `endobj`.

The change is diagnostic-only. Valid linearized output and all existing writer
bytes remain unchanged.

## qpdf contract

The pinned qpdf 11.9.0 implementation has two relevant layers:

- `QPDF::readObject` reads the object value and the trailing token. The
  tokenizer's last offset is the beginning of that token, and an unexpected
  token produces the `expected endobj` warning
  (`libqpdf/QPDF.cc:1331-1357`; `include/qpdf/QPDFTokenizer.hh:171-177`).
- `QPDF::readObjectAtOffset` records `end_before_space` and
  `end_after_space` only when the discovered object was unresolved in the
  cache (`libqpdf/QPDF.cc:1639-1693`). The direct whitespace scan updates the
  `FileInputSource` last offset to the first following non-whitespace byte
  (`libqpdf/FileInputSource.cc:115-133`).
- `QPDF::readHintStream` raises the non-stream error without passing an
  explicit offset (`libqpdf/QPDF_linearization.cc:284-321`). Therefore the
  warning inherits the current input last offset: the trailing-token start
  when the object was already cached, or `end_after_space` when this read
  populated a previously unresolved object.

The current flpdf code loses the trailing-token start and reconstructs it as
`end_before_space - len("endobj")` in
`crates/flpdf/src/linearization/check.rs`. That only works for the literal
`endobj` case.

## Design

1. Extend the resolver's private `ParsedObjectAtOffset` with the start offset
   of the trailing token when one exists. Keep it as read metadata; do not add
   a persistent field to `ObjectHandle`, because qpdf exposes this only through
   the `readObjectAtOffset` operation and ordinary object handles do not need
   the token span.
2. Add a resolver method used by the hint loader that captures whether the
   expected object was already resolved before the offset read, resolves and
   caches the object, and returns the handle plus qpdf's damage offset:
   - cached before the read: trailing-token start;
   - not cached: `end_after_space`;
   - missing metadata: retain the existing source-last-offset fallback.
3. Add the crate-private `Pdf` forwarding method and make
   `load_hint_stream_with_damage` consume the returned offset. Remove the fixed
   six-byte subtraction. Run the show consumer's initial hint-stream load before
   its diagnostic checker, matching qpdf's `readLinearizationData` then
   `checkLinearizationInternal` order; this prevents a failed hint load from
   being retried after its object has entered the cache.
4. Add regressions for the first qpdf hint read, a repeated cached read, and an
   uncached hint object whose trailing token is short. Retain the existing
   literal-`endobj` golden case.

## Non-goals

- No change to linearization validity rules, output writing, or warning text.
- No new legacy/materialized-Object route.
- No change to the public API.
- The deferred `prefix` cleanup is unrelated and remains untouched.

## Verification contract

The regressions compare the same mutated bytes against live qpdf 11.9.0
observations:

- first hint read with a short trailing token: qpdf offset 660;
- repeated cached read with the same token: qpdf offset 653;
- uncached short trailing token: qpdf offset 939;
- existing cached literal `endobj`: qpdf/flpdf offset 594.

Run the focused tests, formatting, strict private rustdoc, all-features
clippy, workspace tests, qpdf module/deviation checks, and fresh patch
coverage before opening the PR.
