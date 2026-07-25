# qpdf-shaped Object Tokenizer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split normal PDF object tokenization from object construction along qpdf 11.9.0's `QPDFTokenizer`/`QPDFParser` boundary and make good13's strings, names, and nesting parse successfully.

**Architecture:** A new private `tokenizer.rs` owns the normal pull-mode lexical cursor and emits qpdf-shaped positioned tokens. `parser.rs` consumes those tokens to construct `Object`s and retains strict versus qpdf file-object policy, while `reader/file_object.rs` remains the `QPDF::readObject`/`readStream` boundary. Lexical-only object-stream/header readers consume `Tokenizer` directly instead of reaching through `Parser`.

**Tech Stack:** Rust 2021 workspace, existing `Object`/`Dictionary`/`Parser`/file-object reader, qpdf 11.9.0 source and executable as the oracle, flpdf-qtest, Cargo tests/Clippy/rustdoc, Beads, and `scripts/patch-coverage.sh`.

## Global Constraints

- qpdf 11.9.0 `QPDFTokenizer.cc`, `QPDFTokenizer.hh`, `QPDFParser.cc`, and observed qpdf output are the oracle.
- This issue implements normal pull-mode object tokenization only.
- `flpdf-n9t0.1` remains open for push mode, `allowEOF`, `includeIgnorable`, emitted whitespace/comments, inline-image mode, `betweenTokens`, max token length, and remaining content/token-filter routing.
- `tokenizer.rs` owns lexical classification and decoded token values; `parser.rs` owns arrays, dictionaries, indirect references, and strict/qpdf policy.
- `reader/file_object.rs` continues to own indirect-object and stream framing; do not move `stream`, `endstream`, or `endobj` completion into the tokenizer or parser.
- Preserve parser depth limits, source offsets, qpdf-valid real literal spelling, strict public APIs, bounded recovery, cache, and encryption behavior.
- Empty PDF names are valid. Embedded whitespace and odd nibbles in hex strings follow qpdf. Exponent-looking input such as `1e3` is a word token, not a real.
- Do not vendor qpdf qtest fixtures into this repository.
- Do not weaken or delete existing tests to make the refactor green; update only assertions that encode a demonstrated qpdf lexical divergence.
- Final changed-line coverage under `crates/flpdf/src` must be 100% from a clean committed `HEAD`.

---

## File Structure

- Create `crates/flpdf/src/tokenizer.rs`
  - Own normal-mode PDF lexical scanning, token type/value/raw bytes/error/span, whitespace/comment skipping, strings, names, numbers, words, and delimiters.
- Modify `crates/flpdf/src/lib.rs`
  - Declare the private tokenizer module.
- Modify `crates/flpdf/src/parser.rs`
  - Consume positioned tokens, construct objects, handle lookahead for references, and retain strict/qpdf mode flags and nesting limits.
- Modify `crates/flpdf/src/reader/file_object.rs`
  - Read indirect object number/generation/`obj` through `Tokenizer`; leave framing and stream completion unchanged.
- Modify `crates/flpdf/src/reader.rs`
  - Read ObjStm header integer pairs through `Tokenizer`.
- Modify `crates/flpdf/src/writer.rs`
  - Read ObjStm header integer pairs through `Tokenizer`.
- Modify `crates/flpdf/src/xref.rs`
  - Read reconstructed ObjStm header integer pairs through `Tokenizer`; keep object/trailer parsing through `Parser`.
- Modify `crates/flpdf/tests/parser_tests.rs`
  - Add the flpdf-authored good13-shaped regression and qpdf lexical classification assertions.
- Modify `docs/superpowers/specs/2026-07-25-flpdf-h8kt-qpdf-tokenizer-design.md`
  - Update only delivery status and final evidence after implementation.

---

### Task 1: Add the qpdf-shaped normal-mode tokenizer

**Files:**
- Create: `crates/flpdf/src/tokenizer.rs`
- Modify: `crates/flpdf/src/lib.rs:91-145`
- Test: `crates/flpdf/src/tokenizer.rs`

**Interfaces:**
- Consumes: `crate::parser::{is_delimiter, is_ws}` initially; move those two byte predicates into `tokenizer.rs` during this task and re-export them as `pub(crate)` for framing callers.
- Produces:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenType {
    Bad,
    ArrayClose,
    ArrayOpen,
    BraceClose,
    BraceOpen,
    DictClose,
    DictOpen,
    Integer,
    Name,
    Real,
    String,
    Null,
    Bool,
    Word,
    Eof,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Token<'a> {
    pub(crate) token_type: TokenType,
    pub(crate) value: std::borrow::Cow<'a, [u8]>,
    pub(crate) raw: &'a [u8],
    pub(crate) error_message: Option<String>,
    pub(crate) start: usize,
    pub(crate) end: usize,
}

pub(crate) struct Tokenizer<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Tokenizer<'a> {
    pub(crate) fn new(input: &'a [u8]) -> Self;
    pub(crate) fn position(&self) -> usize;
    pub(crate) fn skip_ignorable(&mut self) -> crate::Result<()>;
    pub(crate) fn next_token(&mut self) -> Token<'a>;
    pub(crate) fn next_integer(&mut self) -> crate::Result<i64>;
    pub(crate) fn expect_word(&mut self, expected: &[u8]) -> crate::Result<()>;
}

pub(crate) fn is_ws(byte: u8) -> bool;
pub(crate) fn is_delimiter(byte: u8) -> bool;
```

- [ ] **Step 1: Register the module and write tokenizer tests before implementation**

Add to `lib.rs` beside the other private lexical modules:

```rust
pub(crate) mod tokenizer;
```

Create `tokenizer.rs` with only the imports and a `#[cfg(test)]` module that
references the interfaces above. The tests must include:

```rust
#[test]
fn hex_strings_ignore_pdf_whitespace_and_zero_pad_odd_nibbles() {
    let mut tokenizer = Tokenizer::new(b"<010 203\0\x0c0004056>");
    let token = tokenizer.next_token();
    assert_eq!(token.token_type, TokenType::String);
    assert_eq!(token.value.as_ref(), b"\x01\x02\x03\x00\x04\x05\x60");
    assert_eq!(token.raw, b"<010 203\0\x0c0004056>");
    assert_eq!((token.start, token.end), (0, 18));
}

#[test]
fn empty_name_and_exponent_looking_word_match_qpdf_types() {
    let mut tokenizer = Tokenizer::new(b"/ 1e3");
    let name = tokenizer.next_token();
    assert_eq!(name.token_type, TokenType::Name);
    assert_eq!(name.value.as_ref(), b"/");
    let word = tokenizer.next_token();
    assert_eq!(word.token_type, TokenType::Word);
    assert_eq!(word.value.as_ref(), b"1e3");
}

#[test]
fn literal_string_normalizes_line_endings_and_octal_overflow() {
    let mut tokenizer = Tokenizer::new(b"(a\rb\r\nc\\777)");
    let token = tokenizer.next_token();
    assert_eq!(token.token_type, TokenType::String);
    assert_eq!(token.value.as_ref(), b"a\nb\nc\xff");
}

#[test]
fn invalid_and_unterminated_hex_are_bad_tokens_at_the_cause() {
    let mut invalid = Tokenizer::new(b"<0g>");
    let token = invalid.next_token();
    assert_eq!(token.token_type, TokenType::Bad);
    assert_eq!(token.start, 0);
    assert_eq!(token.end, 3);
    assert!(token.error_message.as_deref().unwrap().contains("invalid character"));

    let mut unterminated = Tokenizer::new(b"<01");
    let token = unterminated.next_token();
    assert_eq!(token.token_type, TokenType::Bad);
    assert_eq!(token.end, 3);
    assert_eq!(token.error_message.as_deref(), Some("EOF while reading token"));
}

#[test]
fn normal_mode_skips_comments_and_emits_qpdf_delimiter_types() {
    let mut tokenizer = Tokenizer::new(b" % comment\r\n[<<{}>>]");
    let types = std::iter::from_fn(|| {
        let token = tokenizer.next_token();
        (token.token_type != TokenType::Eof).then_some(token.token_type)
    })
    .collect::<Vec<_>>();
    assert_eq!(
        types,
        vec![
            TokenType::ArrayOpen,
            TokenType::DictOpen,
            TokenType::BraceOpen,
            TokenType::BraceClose,
            TokenType::DictClose,
            TokenType::ArrayClose,
        ]
    );
}

#[test]
fn unexpected_close_and_comment_at_eof_are_bad_like_qpdf() {
    let mut close = Tokenizer::new(b")");
    let token = close.next_token();
    assert_eq!(token.token_type, TokenType::Bad);
    assert_eq!(token.error_message.as_deref(), Some("unexpected )"));

    let mut comment = Tokenizer::new(b"% unterminated comment");
    let token = comment.next_token();
    assert_eq!(token.token_type, TokenType::Bad);
    assert_eq!(token.error_message.as_deref(), Some("EOF while reading token"));
}

#[test]
fn integer_helpers_require_qpdf_token_types() {
    let mut tokenizer = Tokenizer::new(b"12 -3 obj");
    assert_eq!(tokenizer.next_integer().unwrap(), 12);
    assert_eq!(tokenizer.next_integer().unwrap(), -3);
    tokenizer.expect_word(b"obj").unwrap();
    assert_eq!(tokenizer.position(), 9);
}
```

- [ ] **Step 2: Run the tokenizer tests and verify RED**

Run:

```bash
cargo test -p flpdf tokenizer::tests -- --nocapture
```

Expected: compilation fails because `Tokenizer`, `Token`, and `TokenType` do
not exist. This is the expected RED cause; do not add parser changes yet.

- [ ] **Step 3: Implement positioned token types and normal pull scanning**

Implement the interfaces exactly as declared. `next_token` must:

1. Use a private ignorable scanner to skip PDF whitespace and complete
   comments, recording the first
   non-ignorable byte as `start`. A comment that reaches EOF without CR/LF
   emits `Bad("EOF while reading token")`, matching qpdf normal mode.
2. Return `Eof` with an empty borrowed value/raw slice at `input.len()` when
   EOF occurs between tokens.
3. Emit one-byte array/brace tokens.
4. Emit `<<`/`>>`; a lone `>` is `Bad("unexpected >")`, and a lone `)` is
   `Bad("unexpected )")`.
5. Parse `(` with qpdf literal-string rules:
   - nested parentheses;
   - `\n`, `\r`, `\t`, `\b`, `\f`;
   - escaped `(`, `)`, `\`, or any other byte;
   - one-to-three octal digits with the accumulated value modulo 256;
   - unescaped CR and CRLF normalized to one LF;
   - escaped CR, CRLF, or LF removed as line continuation.
6. Parse `/` names with the leading slash in `value`, accepting `/` alone.
   Decode valid `#xx`; preserve qpdf's error message for stray `#`; make
   `#00` a bad token.
7. Parse `<...>` hex strings with the two-state nibble algorithm from the
   design. Ignore only `is_ws`; odd closing nibble appends `high << 4`.
8. Scan all other appendable tokens until `is_ws || is_delimiter` and classify
   with qpdf's number state rules:
   - digits only, optionally preceded by `+`/`-` → `Integer`;
   - `1.`, `.1`, `+.1`, `-1.`, and `1.2` → `Real`;
   - `.`, `+.`, `1..2`, and `1e3` → `Word`;
   - `true`/`false` → `Bool`; `null` → `Null`;
   - otherwise → `Word`, including `1e3`, bare `+`, and bare `.`.
9. Set `raw = &input[start..end]`. Use `Cow::Borrowed(raw)` for scalar
   tokens and `Cow::Owned(decoded)` for name/string tokens.
10. Never use `char` classification or locale-sensitive functions.

Implement helpers:

```rust
pub(crate) fn is_ws(byte: u8) -> bool {
    matches!(byte, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

pub(crate) fn is_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}
```

The public-to-crate `skip_ignorable` uses the same scanner and maps its bad
comment-at-EOF result to `Error::Parse`.

`next_integer` accepts only `TokenType::Integer`, parses the token's ASCII
bytes as `i64`, and reports the token start on overflow/type mismatch.
`expect_word` accepts only `TokenType::Word` with exact byte equality.

- [ ] **Step 4: Run tokenizer tests and existing parser tests**

Run:

```bash
cargo fmt --all
cargo test -p flpdf tokenizer::tests -- --nocapture
cargo test -p flpdf --test parser_tests
```

Expected: tokenizer tests pass; the existing parser remains unchanged and its
tests still pass.

- [ ] **Step 5: Commit the standalone tokenizer**

```bash
git add crates/flpdf/src/lib.rs crates/flpdf/src/tokenizer.rs
git commit -m "refactor: add qpdf-shaped object tokenizer"
```

---

### Task 2: Make `Parser` consume tokenizer output

**Files:**
- Modify: `crates/flpdf/src/parser.rs:1-596`
- Modify: `crates/flpdf/tests/parser_tests.rs:76-205`
- Test: `crates/flpdf/tests/parser_tests.rs`
- Test: `crates/flpdf/src/parser.rs`

**Interfaces:**
- Consumes: `Tokenizer`, `Token`, and `TokenType` from Task 1.
- Produces: the existing `Parser::new`, `Parser::new_no_reference`,
  `Parser::position`, `Parser::parse_one_object`, `Parser::object`,
  `parse_qpdf_file_object`, `parse_qpdf_direct_object`, and
  `parse_strict_direct_object` APIs with token-backed internals.
- Retains: `keyword_token_end` as a raw bounded-framing helper, importing
  `is_ws`/`is_delimiter` from `tokenizer.rs`.

- [ ] **Step 1: Add good13-shaped and normal lexical parity regressions**

Add to `parser_tests.rs`:

```rust
#[test]
fn parses_qpdf_good13_shaped_nesting_strings_and_names() {
    let object = parse_object(
        br#"<<
          /strings [(one) ($\242) () (()) (\() (\)) (a\f\b\t\r\nb) (A\000B)]
          /hex#20strings [<506F7461746f> <010 203 0004056> <41
42>]
          /n#65sting <<
            /a [1 2 << /x (y) >> [(z)]]
            /b <</a [1 2] / (legal)>>
          >>
          /names [/n#65sting /hex#20strings /text#2fplain]
        >>"#,
    )
    .unwrap();
    let dict = object.as_dict().unwrap();
    assert_eq!(
        dict.get("hex strings"),
        Some(&Object::Array(vec![
            Object::String(b"Potato".to_vec()),
            Object::String(b"\x01\x02\x03\x00\x04\x05\x60".to_vec()),
            Object::String(b"AB".to_vec()),
        ]))
    );
    assert_eq!(
        dict.get("names"),
        Some(&Object::Array(vec![
            Object::Name(b"nesting".to_vec()),
            Object::Name(b"hex strings".to_vec()),
            Object::Name(b"text/plain".to_vec()),
        ]))
    );
    let nesting = dict.get("nesting").unwrap().as_dict().unwrap();
    let b_dict = nesting.get("b").unwrap().as_dict().unwrap();
    assert_eq!(b_dict.get(""), Some(&Object::String(b"legal".to_vec())));
}

#[test]
fn qpdf_empty_name_is_valid() {
    assert_eq!(parse_object(b"/").unwrap(), Object::Name(Vec::new()));
}

#[test]
fn qpdf_literal_string_normalizes_unescaped_line_endings() {
    assert_eq!(
        parse_object(b"(a\rb\r\nc)").unwrap(),
        Object::String(b"a\nb\nc".to_vec())
    );
}

#[test]
fn qpdf_exponent_looking_token_is_not_a_real() {
    assert!(parse_object(b"1e3").is_err());
}
```

In `parses_real_numbers`, remove `1e3` and its `RealLiteral` assertion. Keep
`.75`, `1.`, `+.25`, and canonical real assertions unchanged.

- [ ] **Step 2: Run parser tests and verify RED for the qpdf gaps**

Run:

```bash
cargo test -p flpdf --test parser_tests -- --nocapture
```

Expected: the good13-shaped test fails with `invalid hex string`; the empty
name and line-ending tests fail; the exponent test fails because current
flpdf accepts `1e3`.

- [ ] **Step 3: Replace parser-owned scalar lexing with token consumption**

Change `Parser` to:

```rust
pub(crate) struct Parser<'a> {
    tokenizer: crate::tokenizer::Tokenizer<'a>,
    buffered: std::collections::VecDeque<crate::tokenizer::Token<'a>>,
    no_reference: bool,
    top_level_no_reference: bool,
    depth: usize,
}
```

Add these exact helpers:

```rust
fn next_token(&mut self) -> Token<'a> {
    self.buffered
        .pop_front()
        .unwrap_or_else(|| self.tokenizer.next_token())
}

fn unread_token(&mut self, token: Token<'a>) {
    self.buffered.push_front(token);
}

fn peek_token(&mut self) -> Token<'a> {
    let token = self.next_token();
    self.unread_token(token.clone());
    token
}

pub(crate) fn position(&self) -> usize {
    self.buffered
        .front()
        .map_or_else(|| self.tokenizer.position(), |token| token.start)
}
```

Implement `object_inner` by matching the next token:

```rust
match token.token_type {
    TokenType::DictOpen => self.dictionary(),
    TokenType::ArrayOpen => self.array(),
    TokenType::Name => Ok(Object::Name(token.value.as_ref()[1..].to_vec())),
    TokenType::String => Ok(Object::String(token.value.into_owned())),
    TokenType::Bool => Ok(Object::Boolean(token.value.as_ref() == b"true")),
    TokenType::Null => Ok(Object::Null),
    TokenType::Integer => self.integer_or_ref(token),
    TokenType::Real => self.real_object(token),
    TokenType::Bad => Err(Error::parse(
        token.start,
        token.error_message.unwrap_or_else(|| "bad token".to_string()),
    )),
    TokenType::Eof => Err(Error::parse(token.start, "unexpected EOF")),
    _ => Err(Error::parse(token.start, "expected PDF object")),
}
```

`dictionary` consumes `Name` keys until `DictClose`; strip the leading slash
from the key and allow an empty remaining byte slice. `array` consumes objects
until `ArrayClose`. On a mismatched closer or EOF, report that token's start.

`integer_or_ref` parses the first token's `value` as `i64`. If references are
enabled, consume a second token and then a third:

- `Integer`, `Integer`, `Word("R")` → checked `ObjectRef`;
- otherwise unread the consumed non-reference tokens in reverse order and
  return the first integer;
- when `no_reference` is true or `top_level_no_reference && depth == 1`,
  return the first integer without lookahead.

`real_object` parses the raw ASCII bytes as `f64` and preserves them in
`RealLiteral` when `value.to_string().as_bytes() != token.raw`.

Delete parser-owned `name`, `literal_string`, `hex_string`, `real`,
`real_with_integer_prefix`, `parse_real_exponent`, `integer`, `take_keyword`,
`expect_byte`, `expect_bytes`, `starts_with`, `peek`, and `hex_value`.

Keep the depth increment/decrement around `object_inner` exactly balanced.

- [ ] **Step 4: Route parser entry points through logical token positions**

- `Parser::new` and `new_no_reference` construct `Tokenizer::new(input)` and
  an empty `VecDeque`.
- `parse_qpdf_direct_object` identifies an empty body by peeking for
  `TokenType::Word` with value `endobj`; because the token remains buffered,
  `position()` points to its start.
- `parse_strict_direct_object` and `parse_qpdf_direct_object` use
  `position()` rather than raw parser fields.
- `keyword_token_end` continues to use raw bytes and the tokenizer predicates
  because stream recovery needs exact bounded keyword positions.

- [ ] **Step 5: Run RED→GREEN focused gates**

Run:

```bash
cargo fmt --all
cargo test -p flpdf --test parser_tests -- --nocapture
cargo test -p flpdf parser::stream_length_tests -- --nocapture
cargo test -p flpdf --test content_stream_tests
cargo test -p flpdf --test reader_tests
```

Expected: all commands exit 0. The good13-shaped dictionary includes the empty
key and decoded odd-nibble string; `1e3` is rejected by object parsing.

- [ ] **Step 6: Commit the parser integration**

```bash
git add crates/flpdf/src/parser.rs crates/flpdf/tests/parser_tests.rs
git commit -m "fix: parse objects through qpdf-shaped tokens"
```

---

### Task 3: Route lexical-only readers through `Tokenizer`

**Files:**
- Modify: `crates/flpdf/src/reader/file_object.rs:1-205`
- Modify: `crates/flpdf/src/reader.rs:1930-1970`
- Modify: `crates/flpdf/src/writer.rs:1285-1360`
- Modify: `crates/flpdf/src/xref.rs:440-480`
- Modify: `crates/flpdf/src/parser.rs:100-180`
- Test: existing reader, writer, xref, and file-object tests

**Interfaces:**
- Consumes: `Tokenizer::next_integer`, `Tokenizer::expect_word`,
  `Tokenizer::skip_ignorable`, and `Tokenizer::position`.
- Produces: no new API. Removes `Parser::integer_for_indirect`,
  `Parser::expect_keyword_for_indirect`, and public-to-crate parser whitespace
  scanning after all callers are migrated.

- [ ] **Step 1: Record the green characterization baseline**

Run:

```bash
cargo test -p flpdf reader::file_object::tests -- --nocapture
cargo test -p flpdf object_stream -- --nocapture
cargo test -p flpdf --test xref_tests
cargo test -p flpdf --test writer_tests
```

Expected: all commands exit 0 before caller migration.

- [ ] **Step 2: Migrate the file-object header**

Replace the header `Parser` in `parse_file_object_syntax_impl` with:

```rust
let mut tokenizer = Tokenizer::new(input);
let number = tokenizer.next_integer()?;
let generation = tokenizer.next_integer()?;
tokenizer.expect_word(b"obj")?;
tokenizer.skip_ignorable()?;
let body_start = tokenizer.position();
```

Keep object number/generation range checks, direct-object parsing, pending
stream detection, diagnostics, and completion unchanged.

- [ ] **Step 3: Migrate ObjStm header readers**

In `reader.rs`, `writer.rs`, and `xref.rs`, replace only `Parser` instances
used to read `N` pairs of object number/object offset with `Tokenizer`.
Each existing `integer_for_indirect()?` becomes `next_integer()?`.

Do not replace:

- `parse_qpdf_file_object` for ObjStm member bodies;
- `Parser::object` for reconstructed trailer/dictionary/object bodies;
- content-stream operand parsing.

- [ ] **Step 4: Remove superseded parser lexical helpers and fix imports**

Delete `Parser::integer_for_indirect`, `Parser::expect_keyword_for_indirect`,
and any parser-owned raw whitespace method with no remaining caller. Import
`Tokenizer` from `crate::tokenizer` and import `is_ws`/`is_delimiter` from the
same module where framing code needs them.

Use:

```bash
rg -n "integer_for_indirect|expect_keyword_for_indirect|Parser::new\\(&stream_data\\)" crates/flpdf/src
```

Expected: no lexical-only call remains routed through `Parser`; any remaining
`Parser::new(&stream_data[..])` parses an actual object body.

- [ ] **Step 5: Run caller and workspace crate gates**

Run:

```bash
cargo fmt --all
cargo test -p flpdf reader::file_object::tests -- --nocapture
cargo test -p flpdf object_stream -- --nocapture
cargo test -p flpdf --test xref_tests
cargo test -p flpdf --test writer_tests
cargo test -p flpdf
cargo test -p flpdf-cli
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: every command exits 0 with no warning.

- [ ] **Step 6: Commit lexical caller routing**

```bash
git add crates/flpdf/src/parser.rs crates/flpdf/src/reader/file_object.rs crates/flpdf/src/reader.rs crates/flpdf/src/writer.rs crates/flpdf/src/xref.rs
git commit -m "refactor: route lexical readers through tokenizer"
```

---

### Task 4: Verify good13, qtest, documentation, and coverage

**Files:**
- Modify: `docs/superpowers/specs/2026-07-25-flpdf-h8kt-qpdf-tokenizer-design.md`
- Modify only if coverage requires a behavioral test:
  `crates/flpdf/src/tokenizer.rs`, `crates/flpdf/src/parser.rs`,
  `crates/flpdf/tests/parser_tests.rs`

**Interfaces:**
- Consumes: final token-backed parser and external qpdf 11.9.0 good13 fixture.
- Produces: verified branch, closed `flpdf-h8kt`, pushed Beads state, and
  pushed Git commits. Leaves `flpdf-n9t0.1` open.

- [ ] **Step 1: Run full local quality gates**

```bash
cargo fmt --all -- --check
cargo test -p flpdf --test parser_tests
cargo test -p flpdf --test content_stream_tests
cargo test -p flpdf --test reader_tests
cargo test -p flpdf --test xref_tests
cargo test -p flpdf --test writer_tests
cargo test -p flpdf-cli --test cli_tests
cargo test
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
```

Expected: every command exits 0. The rustdoc command matches the strict CI
private-item link gate.

- [ ] **Step 2: Run the external qpdf 11.9.0 good13 gate**

```bash
cargo build --release -p flpdf-cli
qpdf --version
target/release/flpdf --static-id --qdf /tmp/qpdf-11.9.0/qpdf/qtest/qpdf/good13.pdf /tmp/flpdf-h8kt-good13.pdf
qpdf --check /tmp/flpdf-h8kt-good13.pdf
qpdf --json=2 --json-key=qpdf --json-object=7 /tmp/qpdf-11.9.0/qpdf/qtest/qpdf/good13.pdf > /tmp/flpdf-h8kt-qpdf-object7.json
target/release/flpdf --json=2 --json-key=qpdf --json-object=7 /tmp/qpdf-11.9.0/qpdf/qtest/qpdf/good13.pdf > /tmp/flpdf-h8kt-flpdf-object7.json
jq '.qpdf[] | .["obj:7 0 R"]? | select(.) | .value' /tmp/flpdf-h8kt-qpdf-object7.json > /tmp/flpdf-h8kt-qpdf-object7-value.json
jq '.qpdf[] | .["obj:7 0 R"]? | select(.) | .value' /tmp/flpdf-h8kt-flpdf-object7.json > /tmp/flpdf-h8kt-flpdf-object7-value.json
diff -u /tmp/flpdf-h8kt-qpdf-object7-value.json /tmp/flpdf-h8kt-flpdf-object7-value.json
```

Expected:

- qpdf reports version 11.9.0;
- flpdf exits 0 without `invalid hex string` or `empty name`;
- qpdf check exits 0;
- the extracted object-7 values are identical, covering `/hex strings`,
  `/names`, `/nesting`, and `/strings`.

- [ ] **Step 3: Run flpdf-qtest basic-parsing**

From `/home/ubuntu/flpdf-qtest`:

```bash
QTEST_TESTS=basic-parsing \
FLPDF_CLI_BIN=/home/ubuntu/flpdf/.worktrees/flpdf-h8kt-qpdf-tokenizer/target/release/flpdf \
./scripts/run.sh
rg -n "^basic-parsing 37 \\(nesting, strings, names\\).*PASSED$" harness.log
rg -n "^basic-parsing 38 \\(create qdf\\).*PASSED$" harness.log
rg -n "^basic-parsing 39 \\(check output\\).*PASSED$" harness.log
```

Expected: all three exact lines are present. Use `harness.log`, never
`qtest.log`, because qtest-driver owns and unlinks the latter.

- [ ] **Step 4: Update delivery evidence and commit**

Append a `## Delivery status` section to the design spec recording:

- implemented commit IDs;
- good13 CLI/qpdf results;
- qtest 37/38/39 results;
- full test/Clippy/rustdoc results;
- final patch-coverage numerator/denominator.

Then:

```bash
git add docs/superpowers/specs/2026-07-25-flpdf-h8kt-qpdf-tokenizer-design.md
git commit -m "docs: record qpdf tokenizer parity evidence"
```

- [ ] **Step 5: Run authoritative committed-HEAD patch coverage**

The tree must be clean before this command:

```bash
git status --short
scripts/patch-coverage.sh --base origin/main
```

Expected: `crates/flpdf/src` reports 100% changed-line coverage and exits 0.
If executable tokenizer/parser lines are uncovered, add focused behavioral
tests that execute the exact token state or parser branch, commit them, rerun
all affected focused tests, and rerun the fresh coverage command. Use
`cov:ignore` only for a demonstrated unreachable/compiler-artifact line with
an inline concrete reason.

- [ ] **Step 6: Close and push tracker/Git state**

```bash
bd close flpdf-h8kt --reason "qpdf 11.9.0-shaped normal object tokenizer/parser boundary implemented; good13 and qtest 37-39 pass; full gates and 100% patch coverage pass"
bd dolt push
git status --short --branch
git push
git rev-parse HEAD
git rev-parse '@{upstream}'
bd show flpdf-h8kt
bd show flpdf-n9t0.1
```

Expected:

- `flpdf-h8kt` is closed;
- `flpdf-n9t0.1` remains open;
- the Git worktree is clean;
- local `HEAD` equals the upstream branch tip;
- Beads and Git pushes both succeed.

---

## Self-Review Checklist

- [ ] Every valid normal-mode lexical rule used by good13 is owned by `Tokenizer`.
- [ ] Empty names and odd/whitespace hex strings match qpdf.
- [ ] `1e3` is a word token and is not accepted as a PDF real object.
- [ ] Token spans keep parser and file-object offsets exact after lookahead.
- [ ] `Parser` alone recognizes indirect references.
- [ ] Strict direct/indirect APIs remain separate from qpdf file-object recovery.
- [ ] ObjStm headers use `Tokenizer`; ObjStm member bodies use `Parser`.
- [ ] Stream framing remains in `reader/file_object.rs`.
- [ ] Content operator/inline-image migration did not leak in from `flpdf-n9t0.1`.
- [ ] Full workspace, Clippy, strict private rustdoc, qpdf good13, qtest 37-39, and committed-HEAD patch coverage pass.
- [ ] `flpdf-h8kt` is closed/pushed and `flpdf-n9t0.1` remains open.
