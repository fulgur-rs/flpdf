# qpdf Content Normalizer Design

**Issue:** `flpdf-qxba.7`

**Oracle:** qpdf 11.9.0 at pinned commit
`3b97c9bd266b7c32ea36d3536e22dab77412886d`

**Upstream sources:**

- `libqpdf/ContentNormalizer.cc:1-75`
- `libqpdf/Pl_QPDFTokenizer.cc:1-66`
- `libqpdf/QPDF_Stream.cc:534-635`
- `libqpdf/QPDFObjectHandle.cc:132-156`

## Goal

Replace flpdf's object-reconstructing content normalizer with a faithful Rust
port of qpdf 11.9.0's `Pl_QPDFTokenizer` plus `ContentNormalizer` behavior.
The library and CLI must use this single implementation, preserve raw token
layout except for qpdf's documented transformations, and report malformed
content with the same bad-token state and warning conditions as qpdf.

qpdf fidelity takes precedence over compatibility with flpdf's existing
one-operator-per-line output and malformed-content error contract.

## Current state and problem

`crates/flpdf/src/tokenizer.rs` already mirrors `QPDFTokenizer.cc`, including
pull mode, `allowEOF`, `includeIgnorable`, raw and parsed token values,
bad-token recovery, and inline-image discovery. This issue does not implement
another tokenizer.

The temporary `NormalizationBridge` in
`crates/flpdf/src/content_stream.rs` consumes parsed `Object` events. It:

- discards comments and original whitespace;
- serializes one operation per line;
- reorders inline-image dictionary keys through a `BTreeMap`;
- canonicalizes every operand through `Object::write_pdf`;
- rejects malformed object sequences.

qpdf instead passes every lexical token, including ignorable and bad tokens,
through `ContentNormalizer`. It preserves raw token bytes except for line
ending normalization and canonical string/name serialization. The current
implementation therefore cannot be adjusted at its object-event boundary to
match qpdf.

## Scope

This issue includes:

- a focused `content_normalizer.rs` module;
- a Rust content-token-filter runner corresponding to
  `Pl_QPDFTokenizer::finish`;
- the stateful `ContentNormalizer` behavior;
- a public normalization result that exposes qpdf's two bad-token states;
- migration of the library convenience function and CLI caller;
- qpdf 11.9.0 differential tests, CLI tests, and correspondence docs;
- deletion of `NormalizationBridge` and its old tests and documentation.

This issue does not include:

- another tokenizer state machine;
- a general qpdf `Pipeline` class hierarchy;
- unrelated QDF null traversal, string serialization, trailer formatting, or
  object-stream architecture;
- changing the content object parser or its strict/recovery behavior;
- applying normalization implicitly in QDF writer mode, which remains a
  separate writer integration concern.

## Alternatives considered

### Selected: focused filter runner plus stateful normalizer

Add the two missing responsibilities around the existing tokenizer:

1. a runner that obtains tokens and delivers them in qpdf order; and
2. a normalizer that writes token bytes and tracks bad-token state.

This preserves the upstream component boundary without introducing qpdf's
unneeded general-purpose pipeline framework.

### Rejected: one monolithic normalization function

This would be shorter but would merge tokenizer-driving and token-transform
state again. It would weaken the module correspondence this issue exists to
establish and make EOF/inline-image ordering harder to test independently.

### Rejected: port the full qpdf Pipeline hierarchy

This would imitate C++ plumbing rather than observable behavior. No other
current flpdf component requires that hierarchy, so it would expand the issue
beyond its two upstream source files without improving parity.

## Architecture

### Existing tokenizer remains the lexical authority

`crates/flpdf/src/tokenizer.rs` remains the only production implementation
that recognizes token boundaries and inline-image `EI`. The new module calls
its existing pull APIs with:

- `allow_eof`;
- `include_ignorable`;
- `read_token` with bad tokens allowed; and
- `expect_inline_image` after an `ID` separator.

Only the smallest cursor primitive needed to retrieve the byte after `ID` may
be added to `tokenizer.rs`. It must not recognize token syntax or duplicate
inline-image scanning.

### Focused token-filter runner

`crates/flpdf/src/content_normalizer.rs` owns a private token-filter interface
and runner corresponding to `Pl_QPDFTokenizer`. The runner:

1. constructs the existing tokenizer over the complete input buffer;
2. enables EOF and ignorable tokens;
3. passes each token to the filter, including the EOF token;
4. after a word token whose value is `ID`, reads exactly one input byte;
5. passes that byte as a synthetic space token, using ASCII space if the input
   is already at EOF, as qpdf does;
6. switches the tokenizer to inline-image mode;
7. continues until the EOF token has been handled; and
8. calls the filter's EOF handler exactly once.

The runner buffers its input because flpdf already has decoded stream bytes in
memory. It does not reproduce `Pl_Buffer` or general downstream `Pipeline`
ownership.

### Stateful content normalizer

The private `ContentNormalizer` owns:

- the output byte vector;
- `any_bad_tokens`;
- `last_token_was_bad`.

For each token it applies qpdf's exact rules:

- `Bad`: set both flags and write the token's raw bytes.
- Any non-bad, non-EOF token: clear `last_token_was_bad`.
- `Space`: preserve all bytes except normalize lone CR and CRLF to LF.
- `String`: construct canonical raw PDF string syntax from the parsed value.
- `Name`: construct canonical raw PDF name syntax from the parsed value.
- Every other token, including comments, delimiters, words, numbers, inline
  images, and EOF: write its raw bytes unchanged.
- If an original string or name token contained CR or LF, append one LF after
  the canonical token.

Handling EOF does not clear `last_token_was_bad`; this is how qpdf detects a
stream whose final non-EOF token was bad.

## Public API

The library exposes:

```rust
pub struct ContentNormalization {
    bytes: Vec<u8>,
    any_bad_tokens: bool,
    last_token_was_bad: bool,
}

impl ContentNormalization {
    pub fn as_bytes(&self) -> &[u8];
    pub fn into_bytes(self) -> Vec<u8>;
    pub fn any_bad_tokens(&self) -> bool;
    pub fn last_token_was_bad(&self) -> bool;
}

pub fn normalize_content_stream(input: &[u8]) -> ContentNormalization;
```

`ContentNormalization` and `normalize_content_stream` live in the public
`content_normalizer` module and are re-exported from the crate root.

The function is input-infallible. Arbitrary malformed content becomes token
output plus bad-token state rather than a `Result::Err`. An impossible
tokenizer/filter sequencing state is a programming invariant, not an input
error, matching qpdf's use of logic errors for invalid internal state.

## CLI integration and diagnostics

`flpdf rewrite --normalize-content=y` continues to decode each page content
stream, normalize it, remove stale filter keys, update `/Length`, and store the
new raw bytes. The CLI consumes `ContentNormalization::into_bytes()` instead
of the old `Result<Vec<u8>>`.

After writing a normalized stream whose result reports bad tokens, the CLI
emits qpdf's warning payloads in this order:

1. `content normalization encountered bad tokens`
2. If `last_token_was_bad`, `normalized content ended with a bad token; you may
   be able to resolve this by coalescing content streams in combination with
   normalizing content. From the command line, specify --coalesce-contents`
3. `Resulting stream data may be corrupted but is may still useful for manual
   inspection. For more information on this warning, search for content
   normalization in the manual.`

The warning prefix and stream context use the existing flpdf CLI diagnostic
convention. The payload text and conditional second warning mirror qpdf
11.9.0, including its wording.

If any normalized stream reports a bad token, the CLI finishes writing the
output, emits
`<progname>: operation succeeded with warnings; resulting file may have some problems`,
and exits with qpdf's warning status 3. This is the observed qpdf 11.9.0
behavior for `content-stream-errors.pdf`; the output file remains available
for inspection.

Filter decode failures remain errors because they occur before the content
normalizer receives bytes. Normalization itself does not reject malformed
content.

## Testing strategy

All implementation proceeds through RED, GREEN, and REFACTOR cycles.

### Rust unit and integration tests

Focused tests cover:

- ordinary raw tokens and preserved spacing;
- comments;
- lone CR, CRLF, LF, NUL, form feed, tab, and space tokens;
- canonical literal and hex strings;
- canonical escaped names;
- string/name tokens whose original spelling contains a line ending;
- a bad token followed by a good token;
- consecutive bad tokens;
- a bad token immediately before EOF;
- `ID` followed by each possible separator shape used in the matrix;
- `ID` at EOF, including qpdf's default ASCII-space injection;
- inline-image payloads containing binary bytes and false `EI` candidates;
- exact EOF-token then EOF-handler ordering.

Existing `content_stream_tests` retain object parser coverage but remove or
replace assertions tied to `NormalizationBridge`, one-operation-per-line
formatting, dictionary sorting, comment stripping, and malformed-content
errors.

### Pinned qpdf differential oracle

Extend the existing pinned qpdf tokenizer probe infrastructure with a
content-normalization mode. The qpdf side runs the same bytes through
`Pl_QPDFTokenizer` and `ContentNormalizer`, then reports:

- normalized output as hex;
- `anyBadTokens`;
- `lastTokenWasBad`.

The Rust ignored differential test runs a deterministic case matrix through
both implementations and asserts exact equality of all three values. The
probe build continues to use the read-only pinned qpdf 11.9.0 source and
verifies that the source tree stays clean.

### CLI end-to-end tests

Update `cli_optimization_matrix` to remove the documented normalization
divergences and compare decoded output content bytes directly with qpdf
11.9.0. An flpdf-authored fixture or test-built PDF exercises preserved
spacing, comments, canonical string/name output, and inline images.

A malformed-content CLI fixture verifies that:

- output is still produced;
- normalized bytes match qpdf;
- the first and third warnings always appear for any bad token;
- the second warning appears only when the last non-EOF token was bad.
- the process exits 3 after emitting qpdf's warning-success summary.

qpdf-qtest fixtures remain in the separate `flpdf-qtest` repository and are
not vendored into flpdf.

## Documentation and correspondence

After the production and oracle tests pass:

- change the source correspondence annotation so `content_stream.rs` no
  longer claims transitional `Pl_QPDFTokenizer` or `ContentNormalizer`
  responsibility;
- add the qpdf correspondence annotation to `content_normalizer.rs`;
- regenerate or update `docs/qpdf-module-doc-index.md`;
- change the `Pl_QPDFTokenizer.cc / ContentNormalizer.cc` row in
  `docs/qpdf-correspondence.md` from smeared to mirrored;
- delete the old byte-divergence documentation in
  `cli_optimization_matrix.rs`.

## Verification gates

The implementation is complete only after fresh runs of:

```text
cargo fmt --all -- --check
cargo test -p flpdf --test content_stream_tests
cargo test -p flpdf-cli --test cli_optimization_matrix
cargo test -p flpdf
cargo test -p flpdf-cli
cargo test
cargo clippy --workspace --all-targets --all-features -- -D warnings
scripts/qpdf-tokenizer-diff.sh
```

The repository's fresh patch-coverage workflow must report 100% of changed
executable lines. Searches must confirm that `NormalizationBridge` and its old
state enum are gone and that no new production lexer exists outside
`tokenizer.rs`.
