# Test Driver Review Remediation Design

## Goal

Address all three actionable review threads on PR #591 while preserving the
qpdf 11.9.0 `test_driver 1` output contract:

1. derive stream-warning offsets from the terminal stream in an indirect
   reference chain;
2. report array-item indirectness without resolving the referenced object; and
3. ignore nested values under `/DecodeParms` keys that the selected filter does
   not consume.

The changes remain internal to `flpdf-qtest-tools`. The public `flpdf` API and
the existing unparse behavior are unchanged.

## Reference provenance

`Handle` will retain two distinct references while resolving a chain:

- the first reference is the qpdf handle identity used by `is_indirect`,
  `unparse`, and `unparse_resolved`; and
- the terminal reference is the object whose body supplied the resolved value.

For a chain `6 0 R -> 7 0 R -> stream`, `6 0 R` remains the first reference and
`7 0 R` is the terminal reference. Stream warning offsets will query
`Pdf::source_stream_data_offset` with the terminal reference. Direct streams
have neither reference and continue to produce no source offset.

This keeps the existing qpdf-compatible identity contract while locating
diagnostics at the stream data that actually triggered them.

## Lazy array-item metadata

The `test_0_1` array branch only calls qpdf's `isIndirect()` on each item.
Accordingly, the Rust path will inspect each raw array value and report whether
it is an `Object::Reference` without constructing a fully resolved child
`Handle`.

This allows an item to be reported as indirect even when its target is
malformed, missing, cyclic, or beyond the supported reference-chain depth.
Operations that need an item's value remain responsible for resolving it.
Dictionary traversal is unchanged because it has a separate qpdf contract:
resolved-null values are omitted from the reported dictionary items.

## Filter-aware `/DecodeParms` resolution

`resolve_stream_dictionary` will continue to resolve the `/Filter` structure
far enough to obtain the ordered normalized filter names. It will resolve the
top-level `/DecodeParms` container and any array slot paired with a filter, but
dictionary contents will be resolved selectively:

- `FlateDecode` consumes `/Predictor`, `/Columns`, `/Colors`, and
  `/BitsPerComponent`.
- `LZWDecode` consumes the same keys plus `/EarlyChange`.
- Other current filters do not consume dictionary values through this helper.
- Unknown and filter-irrelevant keys retain their original values and are not
  traversed.

Recognized parameter values may still resolve through the existing bounded
reference-chain helper. Container-shape and `/DecodeParms` length errors retain
their current ordering and text.

This matches qpdf 11.9.0: `QPDF_Stream` pairs each decode-parameter object with
its filter, while `SF_FlateLzwDecode::setDecodeParms` reads only recognized
keys and ignores all others without dereferencing their values.

## Error handling

- A missing terminal stream offset remains `None`; it does not become a new
  driver error.
- Array indirectness inspection cannot fail from the child target because it
  does not resolve that target.
- Recognized filter parameters retain the existing 64-hop reference-chain
  bound.
- Unknown `/DecodeParms` values cannot trigger the driver's nesting or
  reference-chain errors.
- Existing unsupported-filter and inconsistent-length warnings remain
  byte-for-byte compatible with qpdf.

## Test strategy

Implementation follows three independent RED-GREEN-REFACTOR cycles:

1. Add a chained-reference stream fixture whose terminal stream emits the
   inconsistent-`/DecodeParms` warning. Assert the qpdf warning contains the
   terminal stream-data offset.
2. Add an array regression whose indirect child has a reference chain beyond
   the driver's resolution limit. Assert `test_0_1` reports the child as
   indirect and completes without resolving it.
3. Add a Flate stream whose `/DecodeParms` contains a deeply nested unknown
   `/Metadata` value. Assert the stream decodes normally while existing tests
   continue to prove that recognized indirect predictor values are resolved.

The qpdf/Rust differential corpus will include the new observable cases.
Focused tests will run after each cycle. Final verification includes formatting,
workspace Clippy with warnings denied, workspace all-feature tests, the pinned
qpdf differential script, and fresh 100% patch coverage against `origin/main`.

## Non-goals

- Replacing `Handle` with a general lazy object model.
- Adding resolver callbacks to the public stream-filter API.
- Changing dictionary-item null omission.
- Extending the supported filter or predictor set.
- Replying to or resolving GitHub review threads.
