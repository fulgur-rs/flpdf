# qpdf-shaped object tokenizer and parser design

## Problem

qpdf 11.9.0's `qpdf/qtest/qpdf/good13.pdf` exercises nested objects,
literal strings, hex strings, and escaped names. flpdf fails while reading
object 7:

```text
flpdf: parse error at byte 166: invalid hex string
```

The reported offset is relative to object 7. It points to the first space in
the hex string `<010 203 0004056>`.

This is not only a missing branch in `Parser::hex_string`. qpdf assigns lexical
rules for hex strings, names, literal strings, numbers, words, whitespace, and
delimiters to `QPDFTokenizer`. `QPDFParser` consumes those tokens and assembles
arrays, dictionaries, and indirect references. flpdf currently combines both
responsibilities in `parser.rs`.

The recently completed file-object reader refactor established the next outer
boundary: `parser.rs` parses direct objects, while `reader/file_object.rs`
corresponds to `QPDF::readObject` and `QPDF::readStream`. Fixing good13 by
adding another parser-local lexical exception would work against that
direction and make later qpdf parity changes harder to place.

## Oracle evidence

qpdf 11.9.0's `QPDFTokenizer::inHexstring` and
`QPDFTokenizer::inHexstring2nd`:

- ignore PDF whitespace between hex digits;
- decode pairs of hex digits into bytes;
- append a high nibble followed by a zero low nibble when `>` closes an odd
  number of digits;
- produce a bad token for a byte that is neither hex, whitespace, nor `>`.

For `<010 203 0004056>`, qpdf produces:

```text
01 02 03 00 04 05 60
```

qpdf successfully rewrites and checks good13 with exit status 0.

## Goals

- Add an internal tokenizer component corresponding to qpdf 11.9.0's
  `QPDFTokenizer` for normal object lexical analysis.
- Make `parser.rs` correspond to `QPDFParser`: it consumes tokens and owns
  composite-object construction and indirect-reference recognition.
- Preserve the existing qpdf-shaped `reader/file_object.rs` boundary.
- Reproduce qpdf's valid hex-string behavior, including embedded PDF
  whitespace and odd-nibble zero padding.
- Preserve source offsets, real-number spellings, nesting limits, strict API
  behavior, and the existing file-object recovery boundary.
- Make good13 readable by the CLI without vendoring the upstream qtest fixture.

## Non-goals

- Implement every `QPDFTokenizer` operation mode in this issue.
  `flpdf-n9t0.1` tracks push-mode tokenization, `allowEOF`,
  `includeIgnorable`, emitted space/comment tokens, inline-image mode,
  `betweenTokens`, token length limits, raw-value parity, and routing all
  content/token-filter consumers.
- Rewrite content-stream operator or inline-image handling.
- Expand malformed-token recovery or warning behavior beyond current parser
  contracts.
- Change file-object stream completion, xref recovery, QDF formatting,
  object numbering, compression, or writer policy.
- Vendor qpdf qtest inputs into this repository.

## Component correspondence

| flpdf component | qpdf 11.9.0 component | Responsibility |
| --- | --- | --- |
| `tokenizer.rs` | `QPDFTokenizer` | Convert bytes into positioned lexical tokens; decode names and strings; classify numbers, words, delimiters, and bad tokens |
| `parser.rs` | `QPDFParser` | Assemble direct objects, arrays, dictionaries, and indirect references from tokens; apply strict versus qpdf file-object parsing policy |
| `reader/file_object.rs` | `QPDF::readObject` and `QPDF::readStream` | Parse indirect-object framing, detect and complete streams, validate terminators, and record recovery diagnostics |
| `reader.rs` | `QPDF` object resolution/cache | Resolve xref entries and indirect lengths, decrypt, cache, and publish diagnostics |

These are responsibility correspondences, not public API copies. Rust types
remain private and idiomatic, while token kinds and state transitions follow
the qpdf source.

## Tokenizer design

Create `crates/flpdf/src/tokenizer.rs` as a private crate module.

The normal-object tokenizer owns:

- the input byte slice and current cursor;
- skipping PDF whitespace and comments before a token;
- token start and end offsets;
- delimiter classification;
- integer, real, and word classification;
- decoded name values and `#xx` escapes;
- decoded literal strings, nesting, escapes, octal escapes, and line
  continuation;
- decoded hex strings;
- bad-token error text for invalid lexical input.

The initial token kinds correspond to the qpdf kinds needed for object syntax:

- array open and close;
- dictionary open and close;
- brace open and close;
- integer and real;
- name and string;
- null and boolean;
- word;
- bad;
- end of input.

Each token carries its semantic value where applicable plus its source span.
Real tokens retain access to their raw spelling so `Object::RealLiteral`
continues to preserve `.4`, `0.400`, exponents, and other qpdf-visible source
forms.

Comments and whitespace are skipped in the normal mode delivered here. They
are not emitted as tokens until `flpdf-n9t0.1` adds qpdf's
`includeIgnorable` mode.

Hex strings use qpdf's two-state nibble algorithm:

1. In the first-nibble state, a hex digit stores the high nibble.
2. In the second-nibble state, a hex digit completes and appends the byte.
3. PDF whitespace is ignored without changing state.
4. `>` in the first-nibble state closes normally.
5. `>` in the second-nibble state appends `high << 4` and closes.
6. Any other byte produces a bad token carrying the offending offset.
7. End of input before `>` produces an unterminated-string bad token.

## Parser design

Refactor `Parser` to own the tokenizer rather than independently reading raw
bytes for each scalar syntax form.

The parser owns:

- recursive construction of arrays and dictionaries;
- dictionary key/value state;
- indirect-reference recognition from integer/integer/`R` token sequences;
- the existing `no_reference` content-operand policy;
- the existing top-level qpdf file-object bare-reference policy;
- the existing maximum nesting depth;
- conversion from lexical number tokens to `Object::Integer`,
  `Object::Real`, or `Object::RealLiteral`;
- conversion of tokenizer bad tokens into the existing `Error::Parse`
  contract.

A small parser-owned lookahead buffer handles indirect-reference recognition.
The tokenizer remains unaware of object references because qpdf also assigns
that interpretation to `QPDFParser`.

Strict and compatibility behavior remains separated:

- public `parse_object` and strict indirect parsing keep strict trailing-byte,
  empty-body, and top-level-reference behavior;
- qpdf file-object mode keeps its empty-object and top-level bare-reference
  recovery;
- nested references remain references in both modes;
- object-stream members remain direct objects without `endobj` checks.

## Other tokenizer consumers

Current call sites use `Parser` as an ad hoc integer scanner for indirect
headers and object-stream indexes. Those lexical-only call sites move to the
new tokenizer so the component boundary is real rather than parser-internal:

- `reader/file_object.rs` indirect object number, generation, and `obj` token;
- `reader.rs`, `writer.rs`, and `xref.rs` object-stream header integers.

Call sites that genuinely parse objects continue through `Parser`.
`ContentStreamParser` continues to own operator and inline-image framing in
this issue, but every operand parsed through `Parser` now uses the shared
tokenizer. Its remaining direct tokenization is part of `flpdf-n9t0.1`.

File-object recovery helpers that search bounded byte windows for
`endstream`/`endobj` remain in `reader/file_object.rs`; they are recovery and
framing operations, not ordinary lexical pulls.

## Error handling

- Valid hex strings with whitespace or an odd digit count succeed exactly as
  qpdf does.
- Invalid hex bytes remain parse errors at the offending byte.
- Unterminated literal/hex strings, arrays, and dictionaries retain an error
  rather than silently consuming the rest of the input. A name may terminate
  at EOF, matching qpdf tokenization.
- Integer overflow, invalid object numbers/generations, and excessive nesting
  keep their current error categories and limits.
- The tokenizer represents a bad token distinctly, but this issue does not
  broaden qpdf-style recovery from malformed tokens. The parser maps it to the
  current error contract.

## Test strategy

Follow red-green-refactor.

1. Add a parser regression containing the good13 object-7 body. Assert the
   decoded `/hex strings`, `/strings`, `/names`, and nested dictionary values.
   On current `main`, it must fail at the whitespace in the second hex string.
2. Add tokenizer unit tests for qpdf's token types and source spans, including:
   - even hex;
   - PDF whitespace in both nibble states, including NUL and form feed;
   - odd hex zero padding;
   - invalid hex;
   - unterminated hex;
   - escaped names and nested literal strings;
   - integer, real, word, boolean, null, and delimiters.
3. Refactor the parser to consume the tokenizer while keeping the existing
   parser, content-stream, reader, xref, and writer tests green.
4. Run the external qpdf 11.9.0 good13 fixture through the release CLI:

   ```text
   flpdf --static-id --qdf good13.pdf flpdf.pdf
   qpdf --check flpdf.pdf
   ```

   Both commands must exit 0 without a parse error.
5. Run the relevant flpdf-qtest `basic-parsing 37` case and explicitly verify
   that its result is `PASSED`; do not rely only on an allowlisted aggregate
   exit code.

The upstream fixture remains in the qpdf checkout or flpdf-qtest repository.
Repository tests use a minimal flpdf-authored object byte string.

## Verification gates

- `cargo fmt --all -- --check`;
- focused tokenizer and parser tests;
- `cargo test -p flpdf --test parser_tests`;
- `cargo test -p flpdf --test content_stream_tests`;
- `cargo test -p flpdf --test reader_tests`;
- `cargo test -p flpdf --test xref_tests`;
- `cargo test -p flpdf --test writer_tests`;
- `cargo test -p flpdf-cli --test cli_tests`;
- `cargo test`;
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
- strict workspace rustdoc with private items;
- qpdf 11.9.0 good13 differential checks;
- flpdf-qtest `basic-parsing 37`;
- committed-HEAD patch coverage at 100%.

## Delivery

This is one cohesive responsibility refactor and bug fix on
`fix/flpdf-h8kt-qpdf-tokenizer`. Keep design, implementation, and focused test
commits separately reviewable on the branch. If implementation reveals that
content/inline-image modes are required to preserve existing consumers, stop
instead of absorbing `flpdf-n9t0.1` into this issue.

At completion, close `flpdf-h8kt`, push Beads with `bd dolt push`, and push the
Git branch. Do not close `flpdf-n9t0.1`.
