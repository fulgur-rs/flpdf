# qtest coalesce-contents Warning-Offset Parity Design

## Goal

Make `coalesce-contents.test` pass all 8 cases against qpdf 11.9.0. The
current main branch passes rows 2--8 and fails only row 1 because the four
content-normalization warning groups omit their stream-data offsets.

The authoritative acceptance remains the separate `/home/ubuntu/flpdf-qtest`
repository. Its qpdf fixtures and expected output must not be copied into
flpdf or edited to hide a behavior difference.

## Oracle evidence

The pinned qpdf 11.9.0 source establishes the ownership and call order:

- `QPDFJob.cc:2185-2188` applies `coalesceContentStreams` before later page
  transformations.
- `QPDFWriter.cc:2078-2087` enables content normalization by default for QDF
  mode, and `QPDFWriter.cc:1279-1281` sends normalized streams through the
  normalization pipeline instead of compression.
- `QPDF_Stream.cc:624-635` emits the three content-normalization warnings
  after the stream pipeline finishes.
- `QPDF_Stream.cc:695-698` reports those warnings with the stream's
  `parsed_offset`. For `split-tokens.pdf`, qpdf therefore reports offsets
  671, 823, 962, and 1338.

flpdf already has the equivalent canonical stream warning boundary:
`ObjectHandle::stream_data_warning` reads `get_parsed_offset()` and routes to
`DocumentResolver::warn_stream_data`. However, the CLI's explicit page
normalization path currently calls `normalize_content_stream` directly and
returns `Vec<bool>` from `normalize_page_contents`; `finish_rewrite_warnings`
then formats every warning with `diagnostic_location(input, None)`. This is the
mixed adapter that loses the canonical stream offset. The fix belongs at this
CLI-to-normalizer result boundary, not in qtest data, stderr normalization, or
the coalesce provider.

## Design

Introduce a small structured normalization-warning value carrying:

- `parsed_offset: Option<u64>` — the source stream-data offset when the stream
  came from a parsed PDF; `None` for document-owned/generated streams.
- `last_token_was_bad: bool` — the existing qpdf warning decision.

`normalize_and_store_stream_handle` records the handle's parsed offset when it
observes `any_bad_tokens`. `normalize_page_contents` and
`finish_rewrite_warnings` pass these values without changing traversal order.
The existing warning emitter receives the optional offset and calls
`diagnostic_location(input, offset)`, preserving the current no-offset output
for generated streams. The three warning messages and final warning exit code
remain unchanged.

This keeps the existing pre-writer normalization required by the linearization
planner and page-transform ordering while restoring qpdf's observable warning
contract. It also leaves the already matching QDF/non-QDF stream bytes on the
same writer route.

## Alternatives rejected

1. Move all normalization into the writer pipeline. This would more closely
   resemble qpdf's internal timing but would change the existing linearization
   planning and mutation order, and is broader than the observed defect.
2. Infer offsets in the CLI after the fact or alter qtest normalization rules.
   Both make the adapter or fixture know about a particular PDF rather than
   preserving qpdf's stream-owned warning responsibility.

## Verification

Before production code, add and run a RED regression that exercises the real
CLI warning contract with parsed stream offsets. Then implement the structured
result and run it green. The final gate is:

- `object-stream`-style focused qtest invocation of `coalesce-contents.test`:
  8/8, with row 1 warnings byte-for-byte matching qpdf after the runner's
  normal path substitution;
- `cargo test -p flpdf --test coalesce_tests`;
- relevant CLI normalization/coalesce tests and `cargo test -p flpdf-cli`;
- formatting, full-workspace clippy with `-D warnings`, and the strict
  rustdoc gate.

Beads: `flpdf-25kg.6.23`.
