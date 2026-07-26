# qpdf Tokenizer All-Modes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace flpdf's partial tokenizer and independent content lexer with the complete qpdf 11.9.0 tokenizer, content-mode parser, and object-callback pipeline.

**Architecture:** `tokenizer.rs` becomes the only byte-to-token state machine and serves both push and pull callers. `parser.rs` consumes that tokenizer in normal or content mode; `content_stream.rs` becomes qpdf-shaped object/callback orchestration and contains no lexical rules. Delivery uses three stacked branches: tokenizer core, inline-image mode, then content cutover.

**Tech Stack:** Rust 2021 workspace; qpdf 11.9.0 pinned source and live oracle; Beads; Git stacked branches; Cargo tests, Clippy, strict private-item rustdoc, `cargo llvm-cov`, and `scripts/patch-coverage.sh`.

## Global Constraints

- qpdf 11.9.0 is the behavior and component-boundary oracle. Resolve it with `scripts/fetch-qpdf-source.sh --print-path`; do not edit or re-clone the pinned tree.
- Pre-v1.0 qpdf parity outranks compatibility with the current `ContentStreamParser`/`ContentToken` API.
- `Tokenizer` and `Token` remain `pub(crate)`, but all qpdf token types, public operations, and modes must be implemented.
- `tokenizer.rs` begins with `//! Mirrors qpdf 11.9.0 libqpdf/QPDFTokenizer.cc.`
- Non-obvious recovery branches cite the matching qpdf 11.9.0 source lines.
- `content_stream.rs` must contain no PDF lexical boundary logic after Layer 3.
- `Pl_QPDFTokenizer` and exact `ContentNormalizer` behavior remain out of scope for this issue and belong to `flpdf-qxba.7`.
- Do not copy `qpdf/qtest` fixtures into flpdf. Use flpdf-authored vectors here and the separate `/home/ubuntu/flpdf-qtest` repository for qtest evidence.
- Every production change follows red/green/refactor: write a focused test, observe the expected failure, implement the minimum behavior, and rerun the focused test.
- Each stacked PR measures committed changed-line coverage against its own parent and must reach 100%.
- Preserve unrelated worktrees and the existing untracked `docs/superpowers/plans/2026-07-26-flpdf-qxba-8-3-nntree-outline-consolidation.md`.

## Delivery Topology

Use the existing pushed branch as Layer 1:

```text
main
  └─ feature/flpdf-n9t0-1-tokenizer
       └─ feature/flpdf-n9t0-1-tokenizer-inline-image
            └─ feature/flpdf-n9t0-1-tokenizer-content
```

Before implementation, invoke `superpowers:using-git-worktrees` and move work
to a project-local worktree for `feature/flpdf-n9t0-1-tokenizer`. Do not add
the unrelated untracked plan to any commit.

Per-PR patch coverage bases:

```text
Layer 1: origin/main
Layer 2: origin/feature/flpdf-n9t0-1-tokenizer
Layer 3: origin/feature/flpdf-n9t0-1-tokenizer-inline-image
```

---

### Task 1: Freeze the qpdf token contract and baseline evidence

**Layer:** 1 — tokenizer core

**Files:**
- Modify: `crates/flpdf/src/tokenizer.rs`
- Test: `crates/flpdf/src/tokenizer.rs`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/include/qpdf/QPDFTokenizer.hh`
- Read-only oracle: `$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/QPDFTokenizer.cc`

**Interfaces:**
- Consumes: existing borrowed `Token<'a>` model, `TokenType`, and
  `Tokenizer<'a>`.
- Produces:

```rust
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
    Space,
    Comment,
    InlineImage,
}

pub(crate) struct Token {
    pub(crate) token_type: TokenType,
    pub(crate) value: Vec<u8>,
    pub(crate) raw: Vec<u8>,
    pub(crate) error_message: Option<String>,
    pub(crate) error_offset: usize,
    pub(crate) start: usize,
    pub(crate) end: usize,
}
```

- `Token::new(TokenType, Vec<u8>)` canonicalizes `Name` and `String` raw syntax.
- `Token::is_integer()`, `Token::is_word()`, and `Token::is_word_value(&[u8])` mirror qpdf convenience methods.
- `PartialEq` compares type/value only and always returns false for `Bad`.

- [ ] **Step 1: Record the pre-change external qtest baseline**

Build the current CLI and run only the related external groups:

```bash
implementation_root="$(git rev-parse --show-toplevel)"
cargo build --release -p flpdf-cli -p flpdf-test-compare
cd /home/ubuntu/flpdf-qtest
FLPDF_DIR="$implementation_root" \
QTEST_TESTS="tokenizer token-filters basic-parsing inline-images" \
scripts/run.sh
```

Expected: the harness completes and writes exact subtest states to
`/home/ubuntu/flpdf-qtest/qtest-summary.md` and `harness.log`. Record the
current commit SHA and summary counts in the implementation handoff; do not
copy these generated files into flpdf.

- [ ] **Step 2: Write failing tests for the complete token type and equality contract**

Replace/add focused unit tests in `tokenizer.rs`:

```rust
#[test]
fn token_type_covers_qpdf_ignorable_and_inline_image_types() {
    let types = [
        TokenType::Bad,
        TokenType::ArrayClose,
        TokenType::ArrayOpen,
        TokenType::BraceClose,
        TokenType::BraceOpen,
        TokenType::DictClose,
        TokenType::DictOpen,
        TokenType::Integer,
        TokenType::Name,
        TokenType::Real,
        TokenType::String,
        TokenType::Null,
        TokenType::Bool,
        TokenType::Word,
        TokenType::Eof,
        TokenType::Space,
        TokenType::Comment,
        TokenType::InlineImage,
    ];
    assert_eq!(types.len(), 18);
}

#[test]
fn token_equality_matches_qpdf_type_and_value_only() {
    let left = Token::from_parts(
        TokenType::Name,
        b"/A".to_vec(),
        b"/A".to_vec(),
        None,
        3..5,
    );
    let right = Token::from_parts(
        TokenType::Name,
        b"/A".to_vec(),
        b"/#41".to_vec(),
        Some("ignored by equality".into()),
        40..44,
    );
    assert_eq!(left, right);

    let bad = Token::new(TokenType::Bad, b"x".to_vec());
    assert_ne!(bad, bad.clone());
}

#[test]
fn constructed_name_and_string_tokens_have_canonical_pdf_raw_values() {
    let name = Token::new(TokenType::Name, b"/text/plain".to_vec());
    assert_eq!(name.raw, b"/text#2fplain");

    let string = Token::new(TokenType::String, b"a(b".to_vec());
    assert_eq!(string.raw, br"(a\(b)");
}
```

The implementation must also keep existing qpdf cases for odd-nibble hex
strings, literal string CR normalization, and stray name hashes.

- [ ] **Step 3: Run the focused tests and verify RED**

Run:

```bash
cargo test -p flpdf tokenizer::tests::token_type_covers_qpdf_ignorable_and_inline_image_types -- --exact
cargo test -p flpdf tokenizer::tests::token_equality_matches_qpdf_type_and_value_only -- --exact
cargo test -p flpdf tokenizer::tests::constructed_name_and_string_tokens_have_canonical_pdf_raw_values -- --exact
```

Expected: compilation/test failure because `Space`, `Comment`,
`InlineImage`, owned `Token`, `from_parts`, and qpdf equality are absent.

- [ ] **Step 4: Implement the owned qpdf token model**

At the top of `tokenizer.rs`, add the D4 module doc and change the model:

```rust
//! Mirrors qpdf 11.9.0 libqpdf/QPDFTokenizer.cc.

use std::ops::Range;

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
    Space,
    Comment,
    InlineImage,
}

#[derive(Debug, Clone)]
pub(crate) struct Token {
    pub(crate) token_type: TokenType,
    pub(crate) value: Vec<u8>,
    pub(crate) raw: Vec<u8>,
    pub(crate) error_message: Option<String>,
    pub(crate) error_offset: usize,
    pub(crate) start: usize,
    pub(crate) end: usize,
}

impl PartialEq for Token {
    fn eq(&self, other: &Self) -> bool {
        self.token_type != TokenType::Bad
            && self.token_type == other.token_type
            && self.value == other.value
    }
}

impl Token {
    pub(crate) fn new(token_type: TokenType, value: Vec<u8>) -> Self {
        let raw = match token_type {
            TokenType::Name => canonical_name_raw(&value),
            TokenType::String => canonical_string_raw(&value),
            _ => value.clone(),
        };
        Self::from_parts(token_type, value, raw, None, 0..0)
    }

    pub(crate) fn from_parts(
        token_type: TokenType,
        value: Vec<u8>,
        raw: Vec<u8>,
        error_message: Option<String>,
        range: Range<usize>,
    ) -> Self {
        Self {
            token_type,
            value,
            raw,
            error_message,
            error_offset: range.start,
            start: range.start,
            end: range.end,
        }
    }

    pub(crate) fn is_integer(&self) -> bool {
        self.token_type == TokenType::Integer
    }

    pub(crate) fn is_word(&self) -> bool {
        self.token_type == TokenType::Word
    }

    pub(crate) fn is_word_value(&self, value: &[u8]) -> bool {
        self.is_word() && self.value == value
    }
}
```

Do not implement Rust's `Eq`: qpdf deliberately makes `Bad` non-reflexive, so
claiming Rust's reflexive equality contract would be incorrect.

Implement `canonical_name_raw` using the same name escaping policy as
`Object::Name` while retaining qpdf's leading slash convention. Implement
`canonical_string_raw` with the same literal/hex choice and escaping as
`Object::String`. Extract `pub(crate)` serialization helpers from `object.rs`
if needed; do not duplicate escaping tables in `tokenizer.rs`.

- [ ] **Step 5: Update existing tokenizer/parser tests for owned fields**

Mechanically change assertions and consumers:

```rust
assert_eq!(token.value, b"...".to_vec());
assert_eq!(token.raw, input.to_vec());
```

Remove `Cow`-specific calls such as `.as_ref()` and `.into_owned()` while
preserving the assertions' meaning.

- [ ] **Step 6: Run focused tests and verify GREEN**

Run:

```bash
cargo test -p flpdf tokenizer::tests
cargo test -p flpdf --test parser_tests
```

Expected: all tokenizer model tests and existing parser regressions pass.

- [ ] **Step 7: Commit the token contract**

```bash
git add crates/flpdf/src/tokenizer.rs crates/flpdf/src/object.rs crates/flpdf/src/parser.rs
git commit -m "refactor(tokenizer): mirror qpdf token values"
```

Only add `object.rs` or `parser.rs` if the owned-token conversion required
their mechanical updates.

---

### Task 2: Port the qpdf state machine and push mode

**Layer:** 1 — tokenizer core

**Files:**
- Modify: `crates/flpdf/src/tokenizer.rs`
- Test: `crates/flpdf/src/tokenizer.rs`

**Interfaces:**
- Consumes: owned `Token` and complete `TokenType` from Task 1.
- Produces:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenizerStateError {
    TokenWaiting,
    ImproperInlineImageState,
}

pub(crate) struct PushedToken {
    pub(crate) token: Token,
    pub(crate) unread: Option<u8>,
}

impl<'a> Tokenizer<'a> {
    pub(crate) fn push() -> Tokenizer<'static>;
    pub(crate) fn allow_eof(&mut self);
    pub(crate) fn include_ignorable(&mut self);
    pub(crate) fn present_character(
        &mut self,
        byte: u8,
    ) -> std::result::Result<(), TokenizerStateError>;
    pub(crate) fn present_eof(
        &mut self,
    ) -> std::result::Result<(), TokenizerStateError>;
    pub(crate) fn get_token(&mut self) -> Option<PushedToken>;
    pub(crate) fn between_tokens(&self) -> bool;
}
```

- Layer 2 implements the `ImproperInlineImageState` producer; defining the
  variant here keeps signatures stable.

- [ ] **Step 1: Write failing push/EOF/ignorable tests**

Add:

```rust
use std::collections::VecDeque;

fn push_all(tokenizer: &mut Tokenizer<'static>, input: &[u8]) -> Vec<PushedToken> {
    let mut output = Vec::new();
    let mut pending = input.iter().copied().collect::<VecDeque<_>>();
    while let Some(byte) = pending.pop_front() {
        tokenizer.present_character(byte).unwrap();
        if let Some(ready) = tokenizer.get_token() {
            if let Some(unread) = ready.unread {
                pending.push_front(unread);
            }
            output.push(ready);
        }
    }

    loop {
        tokenizer.present_eof().unwrap();
        let ready = tokenizer.get_token().expect("EOF must finish a token");
        let done = matches!(
            ready.token.token_type,
            TokenType::Eof | TokenType::Bad
        );
        output.push(ready);
        if done {
            break;
        }
    }
    output
}

#[test]
fn push_mode_returns_unread_delimiter_and_between_token_state() {
    let mut tokenizer = Tokenizer::push();
    tokenizer.allow_eof();

    tokenizer.present_character(b'1').unwrap();
    assert!(!tokenizer.between_tokens());
    tokenizer.present_character(b' ').unwrap();
    let ready = tokenizer.get_token().expect("integer");
    assert_eq!(ready.token.token_type, TokenType::Integer);
    assert_eq!(ready.token.raw, b"1");
    assert_eq!(ready.unread, Some(b' '));
}

#[test]
fn include_ignorable_returns_contiguous_space_and_comment_tokens() {
    let mut tokenizer = Tokenizer::push();
    tokenizer.allow_eof();
    tokenizer.include_ignorable();
    let tokens = push_all(&mut tokenizer, b"% comment\r\n \t/Name");
    let kinds = tokens
        .iter()
        .map(|ready| ready.token.token_type)
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        vec![
            TokenType::Comment,
            TokenType::Space,
            TokenType::Name,
            TokenType::Eof,
        ]
    );
    assert_eq!(tokens[0].token.raw, b"% comment");
    assert_eq!(tokens[1].token.raw, b"\r\n \t");
}

#[test]
fn eof_policy_matches_qpdf_default_and_allow_eof() {
    let mut strict = Tokenizer::push();
    strict.present_eof().unwrap();
    let token = strict.get_token().unwrap().token;
    assert_eq!(token.token_type, TokenType::Bad);
    assert_eq!(token.error_message.as_deref(), Some("unexpected EOF"));

    let mut allowed = Tokenizer::push();
    allowed.allow_eof();
    allowed.present_eof().unwrap();
    assert_eq!(
        allowed.get_token().unwrap().token.token_type,
        TokenType::Eof
    );
}

#[test]
fn push_rejects_input_while_token_is_waiting() {
    let mut tokenizer = Tokenizer::push();
    tokenizer.present_character(b'[').unwrap();
    assert_eq!(
        tokenizer.present_character(b']'),
        Err(TokenizerStateError::TokenWaiting)
    );
}
```

Also add table-driven cases for string escape/octal states, name hex states,
`<`/`>`/hex states, sign/decimal/number/real/literal states, braces, and bad
closing delimiters.

- [ ] **Step 2: Run focused tests and verify RED**

Run:

```bash
cargo test -p flpdf tokenizer::tests::push_mode_returns_unread_delimiter_and_between_token_state -- --exact
cargo test -p flpdf tokenizer::tests::include_ignorable_returns_contiguous_space_and_comment_tokens -- --exact
cargo test -p flpdf tokenizer::tests::eof_policy_matches_qpdf_default_and_allow_eof -- --exact
```

Expected: compilation failure because push APIs, ignorable token production,
and qpdf EOF configuration are absent.

- [ ] **Step 3: Replace token-specific pull functions with the qpdf state enum**

Define:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Top,
    InHexString,
    InString,
    InHexStringSecond,
    Name,
    Literal,
    InSpace,
    InComment,
    StringEscape,
    CharCode,
    StringAfterCr,
    Lt,
    Gt,
    InlineImage,
    Sign,
    Number,
    Real,
    Decimal,
    NameHex1,
    NameHex2,
    BeforeToken,
    TokenReady,
}
```

Make `Tokenizer` own qpdf's state fields:

```rust
pub(crate) struct Tokenizer<'a> {
    input: &'a [u8],
    pos: usize,
    state: State,
    allow_eof: bool,
    include_ignorable: bool,
    token_type: TokenType,
    value: Vec<u8>,
    raw: Vec<u8>,
    error_message: Option<String>,
    before_token: bool,
    in_token: bool,
    char_to_unread: Option<u8>,
    inline_image_bytes: usize,
    bad: bool,
    string_depth: usize,
    char_code: u16,
    hex_byte: u8,
    digit_count: usize,
    token_start: usize,
}
```

Port qpdf handlers one-for-one:

```rust
fn handle_character(&mut self, byte: u8) {
    match self.state {
        State::Top => self.in_top(byte),
        State::InSpace => self.in_space(byte),
        State::InComment => self.in_comment(byte),
        State::Lt => self.in_lt(byte),
        State::Gt => self.in_gt(byte),
        State::InString => self.in_string(byte),
        State::Name => self.in_name(byte),
        State::Number => self.in_number(byte),
        State::Real => self.in_real(byte),
        State::StringAfterCr => self.in_string_after_cr(byte),
        State::StringEscape => self.in_string_escape(byte),
        State::CharCode => self.in_char_code(byte),
        State::Literal => self.in_literal(byte),
        State::InlineImage => self.in_inline_image(byte),
        State::InHexString => self.in_hex_string(byte),
        State::InHexStringSecond => self.in_hex_string_second(byte),
        State::NameHex1 => self.in_name_hex1(byte),
        State::NameHex2 => self.in_name_hex2(byte),
        State::Sign => self.in_sign(byte),
        State::Decimal => self.in_decimal(byte),
        State::BeforeToken => self.in_before_token(byte),
        State::TokenReady => unreachable!("checked by present_character"),
    }
}
```

Use `QPDFTokenizer.cc:145-720` as the handler-by-handler oracle. Preserve its
state transitions and diagnostic strings. Rust helpers may share byte
classification, but may not combine states in a way that changes unread,
`before_token`, raw accumulation, or EOF behavior.

- [ ] **Step 4: Implement push completion and EOF**

Follow `QPDFTokenizer.cc:723-762` and `:865-885`:

```rust
pub(crate) fn present_character(
    &mut self,
    byte: u8,
) -> std::result::Result<(), TokenizerStateError> {
    if self.state == State::TokenReady {
        return Err(TokenizerStateError::TokenWaiting);
    }
    self.handle_character(byte);
    if self.in_token {
        self.raw.push(byte);
    }
    Ok(())
}

pub(crate) fn get_token(&mut self) -> Option<PushedToken> {
    if self.state != State::TokenReady {
        return None;
    }
    let unread = if !self.in_token && !self.before_token {
        self.char_to_unread
    } else {
        None
    };
    let token = self.take_ready_token();
    self.reset();
    Some(PushedToken { token, unread })
}
```

`present_eof` uses the exact state table in the design spec. A final appendable
token is completed via the qpdf synthetic form-feed delimiter; do not append
that delimiter to the returned raw token.

- [ ] **Step 5: Run the full tokenizer state matrix**

Run:

```bash
cargo test -p flpdf tokenizer::tests
```

Expected: every state-family test passes, including existing good13/name
recovery regressions.

- [ ] **Step 6: Commit the state machine**

```bash
git add crates/flpdf/src/tokenizer.rs
git commit -m "feat(tokenizer): port qpdf push state machine"
```

---

### Task 3: Route pull tokenization and object consumers through the state machine

**Layer:** 1 — tokenizer core

This task implements pull mode as a cursor-backed adapter over the Task 2
state machine; it does not retain the former pull scanner.

**Files:**
- Modify: `crates/flpdf/src/tokenizer.rs`
- Modify: `crates/flpdf/src/parser.rs`
- Modify: `crates/flpdf/src/reader/file_object.rs`
- Modify: `crates/flpdf/src/reader.rs`
- Modify: `crates/flpdf/src/writer.rs`
- Modify: `crates/flpdf/src/xref.rs`
- Test: `crates/flpdf/src/tokenizer.rs`
- Test: `crates/flpdf/tests/parser_tests.rs`
- Test: `crates/flpdf/tests/reader_tests.rs`
- Test: `crates/flpdf/tests/writer_tests.rs`
- Test: `crates/flpdf/tests/xref_tests.rs`

**Interfaces:**
- Consumes: Task 2 push state machine.
- Produces:

```rust
impl<'a> Tokenizer<'a> {
    pub(crate) fn new(input: &'a [u8]) -> Self;
    pub(crate) fn read_token(
        &mut self,
        allow_bad: bool,
        max_len: usize,
    ) -> Result<Token>;
    pub(crate) fn position(&self) -> usize;
    pub(crate) fn set_position(&mut self, position: usize) -> Result<()>;
}
```

- `read_token` is the only pull lexical path.
- `Parser` uses `read_token(true, 0)` and applies strict/recovery policy itself.
- `Parser<'tokenizer, 'input>` borrows `&'tokenizer mut Tokenizer<'input>`.
  Normal parser entry points create a tokenizer locally; content orchestration
  can probe, seek back, and lend that same tokenizer to a short-lived parser.

- [ ] **Step 1: Write failing pull/push equivalence and max-length tests**

Add:

```rust
#[test]
fn pull_and_push_modes_return_identical_token_payloads() {
    let input = b"%c\r\n[ /A#2fB (x\\n) <abc> +2 -.5 true null word ]";

    let mut pull = Tokenizer::new(input);
    pull.allow_eof();
    pull.include_ignorable();
    let mut pulled = Vec::new();
    loop {
        let token = pull.read_token(true, 0).unwrap();
        let done = token.token_type == TokenType::Eof;
        pulled.push((token.token_type, token.value, token.raw, token.error_message));
        if done {
            break;
        }
    }

    let mut push = Tokenizer::push();
    push.allow_eof();
    push.include_ignorable();
    let pushed = push_all(&mut push, input)
        .into_iter()
        .map(|ready| {
            let token = ready.token;
            (token.token_type, token.value, token.raw, token.error_message)
        })
        .collect::<Vec<_>>();

    assert_eq!(pulled, pushed);
}

#[test]
fn pull_max_len_returns_qpdf_bad_token_or_error() {
    let mut allowed = Tokenizer::new(b"abcdefgh ");
    let token = allowed.read_token(true, 5).unwrap();
    assert_eq!(token.token_type, TokenType::Bad);
    assert_eq!(token.raw, b"abcde");
    assert_eq!(
        token.error_message.as_deref(),
        Some("exceeded allowable length while reading token")
    );

    let mut strict = Tokenizer::new(b"abcdefgh ");
    let error = strict.read_token(false, 5).unwrap_err();
    assert_eq!(
        error.to_string(),
        "parse error at byte 0: exceeded allowable length while reading token"
    );
}

#[test]
fn pull_offsets_exclude_leading_ignorable_bytes() {
    let mut tokenizer = Tokenizer::new(b" \n% c\r\n/Name ");
    let token = tokenizer.read_token(false, 0).unwrap();
    assert_eq!((token.start, token.end), (7, 12));
    assert_eq!(token.raw, b"/Name");
}
```

- [ ] **Step 2: Run focused tests and verify RED**

Run:

```bash
cargo test -p flpdf tokenizer::tests::pull_and_push_modes_return_identical_token_payloads -- --exact
cargo test -p flpdf tokenizer::tests::pull_max_len_returns_qpdf_bad_token_or_error -- --exact
cargo test -p flpdf tokenizer::tests::pull_offsets_exclude_leading_ignorable_bytes -- --exact
```

Expected: compilation failure because `read_token` and qpdf max-length policy
are absent.

- [ ] **Step 3: Implement pull as a loop over the push state machine**

Use `QPDFTokenizer.cc:887-965`:

```rust
pub(crate) fn read_token(&mut self, allow_bad: bool, max_len: usize) -> Result<Token> {
    if self.state != State::InlineImage {
        self.reset();
    }
    self.token_start = self.pos;

    while self.state != State::TokenReady {
        match self.input.get(self.pos).copied() {
            Some(byte) => {
                self.pos += 1;
                self.handle_character(byte);
                if self.before_token {
                    self.token_start += 1;
                }
                if self.in_token {
                    self.raw.push(byte);
                }
                if max_len != 0
                    && self.raw.len() >= max_len
                    && self.state != State::TokenReady
                {
                    self.token_type = TokenType::Bad;
                    self.state = State::TokenReady;
                    self.error_message =
                        Some("exceeded allowable length while reading token".into());
                }
            }
            None => self.present_eof().map_err(tokenizer_state_as_parse_error)?,
        }
    }

    if !self.in_token && !self.before_token {
        self.pos = self.pos.saturating_sub(1);
    }
    let token = self.take_ready_token();
    if token.token_type == TokenType::Bad && !allow_bad {
        return Err(Error::parse(
            token.start,
            token.error_message.clone().unwrap_or_else(|| "bad token".into()),
        ));
    }
    self.reset();
    Ok(token)
}
```

Adjust the exact reset/take ordering so the returned `end` is the cursor just
after the token and unread characters remain available to the next call.
`set_position` validates `position <= input.len()` and resets lexical state.

- [ ] **Step 4: Migrate the object parser and integer-header consumers**

In `parser.rs`, make the parser borrow its tokenizer and propagate lexical
errors rather than synthesizing a token:

```rust
pub(crate) struct Parser<'tokenizer, 'input> {
    tokenizer: &'tokenizer mut Tokenizer<'input>,
    buffered: VecDeque<Token>,
    // existing parser policy fields...
}

fn next_token(&mut self) -> Result<Token> {
    if let Some(token) = self.buffered.pop_front() {
        return Ok(token);
    }
    let token = self.tokenizer.read_token(true, 0)?;
    if token.token_type != TokenType::Bad {
        if let Some(message) = token.error_message.clone() {
            self.diagnostics.push(ParserDiagnostic {
                relative_offset: token.start,
                message,
            });
        }
    }
    Ok(token)
}
```

Change `object_inner`, `dictionary`, `array`, `integer_or_ref`, and
`peek_token` to use `next_token()?`. Each top-level parse function constructs
`let mut tokenizer = Tokenizer::new(input)` and then a short-lived
`Parser::with_tokenizer(&mut tokenizer, false)`. Do not swallow actual
tokenizer diagnostics. Preserve `ParserDiagnostic` collection for recoverable
name warnings and preserve `top_level_no_reference` behavior.

In `reader/file_object.rs`, `reader.rs`, `writer.rs`, and `xref.rs`, use
`read_token(false, 0)` or the existing exact helpers backed by it. There must
be no consumer that scans numeric/name/string token bytes independently.

- [ ] **Step 5: Verify focused object consumers**

Run:

```bash
cargo test -p flpdf tokenizer::tests
cargo test -p flpdf --test parser_tests
cargo test -p flpdf --test reader_tests
cargo test -p flpdf --test writer_tests
cargo test -p flpdf --test xref_tests
```

Expected: all pass. In particular, good13 odd-hex behavior and compressed
object warning offsets remain green.

- [ ] **Step 6: Run Layer 1 quality gates**

Run:

```bash
cargo fmt --all -- --check
cargo test
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" \
  cargo doc --workspace --no-deps --document-private-items
```

Fix production defects with new failing tests. Do not weaken existing
assertions or delete regressions to obtain green output.

- [ ] **Step 7: Commit Layer 1 integration**

```bash
git add crates/flpdf/src/tokenizer.rs crates/flpdf/src/parser.rs \
  crates/flpdf/src/reader/file_object.rs crates/flpdf/src/reader.rs \
  crates/flpdf/src/writer.rs crates/flpdf/src/xref.rs \
  crates/flpdf/tests/parser_tests.rs crates/flpdf/tests/reader_tests.rs \
  crates/flpdf/tests/writer_tests.rs crates/flpdf/tests/xref_tests.rs
git commit -m "refactor(parser): route pulls through qpdf tokenizer"
```

- [ ] **Step 8: Measure Layer 1 committed patch coverage**

Run from a clean committed worktree:

```bash
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail \
  --lcov --output-path target/layer1.lcov
scripts/patch-coverage.sh --base origin/main --lcov target/layer1.lcov
```

Expected: changed lines `100.00%`. Add focused tests for uncovered executable
branches, recommit, and rerun until 100%.

- [ ] **Step 9: Push Layer 1**

```bash
git push origin feature/flpdf-n9t0-1-tokenizer
```

Do not create Layer 2 until the remote Layer 1 head equals local `HEAD`.

---

### Task 4: Port qpdf inline-image discovery and tokenization

**Layer:** 2 — inline image

**Files:**
- Modify: `crates/flpdf/src/tokenizer.rs`
- Test: `crates/flpdf/src/tokenizer.rs`

**Interfaces:**
- Consumes: full core tokenizer from Layer 1.
- Produces:

```rust
impl<'a> Tokenizer<'a> {
    pub(crate) fn expect_inline_image(
        &mut self,
    ) -> std::result::Result<(), TokenizerStateError>;
}
```

- The next `read_token(true, 0)` returns `InlineImage` or `Bad`.

- [ ] **Step 1: Create and push the Layer 2 branch**

```bash
git switch -c feature/flpdf-n9t0-1-tokenizer-inline-image
git push -u origin feature/flpdf-n9t0-1-tokenizer-inline-image
```

- [ ] **Step 2: Write failing inline-image candidate tests**

Add qpdf-derived, flpdf-authored cases:

```rust
fn inline_image_token(input_after_id_separator: &[u8]) -> Token {
    let mut tokenizer = Tokenizer::new(input_after_id_separator);
    tokenizer.allow_eof();
    tokenizer.expect_inline_image().unwrap();
    tokenizer.read_token(true, 0).unwrap()
}

#[test]
fn inline_image_skips_false_ei_followed_by_suspicious_tokens() {
    let token = inline_image_token(b"abc EI \x01bad EI Q");
    assert_eq!(token.token_type, TokenType::InlineImage);
    assert_eq!(token.value, b"abc EI \x01bad ");
    assert_eq!(token.raw, token.value);
}

#[test]
fn inline_image_accepts_ei_followed_by_ten_good_content_tokens() {
    let token = inline_image_token(b"payload EI q 1 0 0 1 0 0 cm Q");
    assert_eq!(token.token_type, TokenType::InlineImage);
    assert_eq!(token.value, b"payload ");
}

#[test]
fn inline_image_requires_word_boundaries() {
    let token = inline_image_token(b"aEIx b EI Q");
    assert_eq!(token.token_type, TokenType::InlineImage);
    assert_eq!(token.value, b"aEIx b ");
}

#[test]
fn inline_image_without_ei_returns_qpdf_bad_eof_token() {
    let token = inline_image_token(b"unterminated");
    assert_eq!(token.token_type, TokenType::Bad);
    assert_eq!(
        token.error_message.as_deref(),
        Some("EOF while reading token")
    );
}
```

Add cases for non-printable word bytes, mixed alphabetic/other word bytes,
`*` as alphabetic, candidate at EOF, more than one rejected candidate, and
cursor restoration.

- [ ] **Step 3: Run focused tests and verify RED**

Run:

```bash
cargo test -p flpdf tokenizer::tests::inline_image_skips_false_ei_followed_by_suspicious_tokens -- --exact
cargo test -p flpdf tokenizer::tests::inline_image_accepts_ei_followed_by_ten_good_content_tokens -- --exact
cargo test -p flpdf tokenizer::tests::inline_image_without_ei_returns_qpdf_bad_eof_token -- --exact
```

Expected: compilation failure because `expect_inline_image` is absent.

- [ ] **Step 4: Implement `find_ei` exactly from qpdf**

Add private helpers:

```rust
fn word_token_at(input: &[u8], start: usize, expected: &[u8]) -> Option<usize>;
fn inline_lookahead_is_plausible(input: &[u8], after_ei: usize) -> bool;
fn find_ei(&mut self) -> Option<usize>;
```

`word_token_at` requires a delimiter before `EI` and delimiter/EOF after it.
`inline_lookahead_is_plausible` creates a fresh tokenizer and checks at most
ten tokens with `allow_bad = true`, applying qpdf's three word flags:

```rust
let mut found_alpha = false;
let mut found_non_printable = false;
let mut found_other = false;
for &byte in &token.value {
    if byte.is_ascii_alphabetic() || byte == b'*' {
        found_alpha = true;
    } else if byte < 32 && !is_ws(byte) {
        found_non_printable = true;
        break;
    } else {
        found_other = true;
    }
}
let suspicious = found_non_printable || (found_alpha && found_other);
```

Restore `pos` after lookahead. Store the accepted/fallback candidate distance
in `inline_image_bytes`, set `State::InlineImage`, and let
`in_inline_image` make the token ready at that exact byte count. Do not search
for or consume `EI` in a separate content parser.

- [ ] **Step 5: Verify inline-image and existing content behavior**

Run:

```bash
cargo test -p flpdf tokenizer::tests
cargo test -p flpdf --test content_stream_tests
cargo test -p flpdf-cli --test cli_optimization_matrix
```

Expected: all pass. Layer 2 proves tokenizer inline-image behavior directly;
the still-independent legacy content consumer is only checked for regressions
and is not claimed to use the new path until Layer 3.

- [ ] **Step 6: Commit and gate Layer 2**

```bash
git add crates/flpdf/src/tokenizer.rs
git commit -m "feat(tokenizer): match qpdf inline image scanning"

cargo fmt --all -- --check
cargo test
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" \
  cargo doc --workspace --no-deps --document-private-items
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail \
  --lcov --output-path target/layer2.lcov
scripts/patch-coverage.sh \
  --base origin/feature/flpdf-n9t0-1-tokenizer \
  --lcov target/layer2.lcov
```

Expected: all quality gates pass and Layer 2 patch coverage is 100%.

- [ ] **Step 7: Push Layer 2**

```bash
git push origin feature/flpdf-n9t0-1-tokenizer-inline-image
```

---

### Task 5: Add qpdf content-only object values

**Layer:** 3 — content cutover

**Files:**
- Modify: `crates/flpdf/src/object.rs`
- Modify: all exhaustive `Object` matches reported by `cargo check`
- Test: `crates/flpdf/src/object.rs`
- Test: `crates/flpdf/tests/object_tests.rs`

**Interfaces:**
- Consumes: no content parser changes.
- Produces:

```rust
pub enum Object {
    // existing variants...
    Operator(Vec<u8>),
    InlineImage(Vec<u8>),
}

impl Object {
    pub fn as_operator(&self) -> Option<&[u8]>;
    pub fn as_inline_image(&self) -> Option<&[u8]>;
}
```

- Both variants unparse stored bytes verbatim, write JSON as `null`, and are
  terminal in structural/reference walkers.

- [ ] **Step 1: Create and push the Layer 3 branch**

```bash
git switch -c feature/flpdf-n9t0-1-tokenizer-content
git push -u origin feature/flpdf-n9t0-1-tokenizer-content
```

- [ ] **Step 2: Write failing object value tests**

Add:

```rust
#[test]
fn operator_and_inline_image_unparse_verbatim() {
    for object in [
        Object::Operator(b"cm".to_vec()),
        Object::InlineImage(b"\x00EI\xff".to_vec()),
    ] {
        let expected = match &object {
            Object::Operator(value) | Object::InlineImage(value) => value.clone(),
            _ => unreachable!(),
        };
        let mut out = Vec::new();
        object.write_pdf(&mut out);
        assert_eq!(out, expected);
    }
}

#[test]
fn content_only_objects_have_qpdf_accessors() {
    assert_eq!(
        Object::Operator(b"q".to_vec()).as_operator(),
        Some(b"q".as_slice())
    );
    assert_eq!(
        Object::InlineImage(b"data".to_vec()).as_inline_image(),
        Some(b"data".as_slice())
    );
}
```

Add a JSON test through the existing object-to-JSON path that asserts both
variants produce `null`.

- [ ] **Step 3: Run focused tests and verify RED**

Run:

```bash
cargo test -p flpdf object::tests::operator_and_inline_image_unparse_verbatim -- --exact
cargo test -p flpdf object::tests::content_only_objects_have_qpdf_accessors -- --exact
```

Expected: compilation failure because the variants and accessors are absent.

- [ ] **Step 4: Implement variants and update exhaustive matches**

Add the variants adjacent to other scalar values. Update:

- `write_pdf` and `write_pdf_qdf`: append stored bytes;
- JSON conversion: emit null;
- reference traversal and mutation: treat as terminal;
- equality/clone: derive behavior;
- type accessors: return stored slices.

Use compiler exhaustiveness errors to locate every `Object` match, but classify
each site deliberately:

- structural walkers: terminal/no references;
- normal file-object writer: raw unparse, matching qpdf value behavior;
- APIs that reject content-only values: return the existing type error rather
  than silently coercing;
- JSON: null.

- [ ] **Step 5: Run object and workspace checks**

Run:

```bash
cargo test -p flpdf object::tests
cargo test -p flpdf --test object_tests
cargo check --workspace --all-targets --all-features
```

Expected: all pass with no non-exhaustive matches.

- [ ] **Step 6: Commit content object values**

```bash
git diff --name-only
git add crates/flpdf/src/object.rs
# Add only the exact exhaustive-match files named by the preceding command.
git add crates/flpdf/src/writer.rs crates/flpdf/src/json.rs
git commit -m "feat(object): add qpdf content object values"
```

The second `git add` line is the expected baseline set; replace it with the
fresh compiler-reported list if the current tree differs. Review
`git diff --cached --stat` before committing so unrelated source files are not
staged.

---

### Task 6: Add parser content mode and qpdf-shaped callbacks

**Layer:** 3 — content cutover

**Files:**
- Modify: `crates/flpdf/src/parser.rs`
- Modify: `crates/flpdf/src/content_stream.rs` alongside the legacy API
- Modify: `crates/flpdf/src/lib.rs`
- Test: `crates/flpdf/tests/content_stream_tests.rs`
- Test: `crates/flpdf/tests/parser_tests.rs`

**Interfaces:**
- Consumes: `Object::Operator`, `Object::InlineImage`, and full tokenizer.
- Produces:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseControl {
    Continue,
    Stop,
}

pub trait ParserCallbacks {
    fn content_size(&mut self, _size: usize) -> Result<()> {
        Ok(())
    }

    fn handle_object(
        &mut self,
        object: Object,
        offset: usize,
        length: usize,
    ) -> Result<ParseControl>;

    fn handle_eof(&mut self) -> Result<()>;
}

pub fn parse_content_stream_data(
    input: &[u8],
    callbacks: &mut impl ParserCallbacks,
) -> Result<()>;
```

`Parser::parse_content_object()` returns `Result<Option<Object>>`; `None`
means content EOF. `Parser` continues to borrow the tokenizer introduced in
Task 3, so the orchestrator and parser share one cursor without copying input
or reconstructing state.

- [ ] **Step 1: Write failing parser content-mode tests**

Add to `parser_tests.rs`:

```rust
#[test]
fn content_mode_returns_words_as_operators_and_never_builds_references() {
    let mut tokenizer = Tokenizer::new(b"0 0 1 R");
    tokenizer.allow_eof();
    let mut parser = Parser::with_tokenizer(&mut tokenizer, true);
    assert_eq!(parser.parse_content_object().unwrap(), Some(Object::Integer(0)));
    assert_eq!(parser.parse_content_object().unwrap(), Some(Object::Integer(0)));
    assert_eq!(parser.parse_content_object().unwrap(), Some(Object::Integer(1)));
    assert_eq!(
        parser.parse_content_object().unwrap(),
        Some(Object::Operator(b"R".to_vec()))
    );
    assert_eq!(parser.parse_content_object().unwrap(), None);
}
```

If `Parser` remains crate-private, place this test in its internal `#[cfg(test)]`
module rather than exposing it solely for integration testing.

- [ ] **Step 2: Write failing callback lifecycle tests**

Replace old aggregate-token expectations in focused
`content_stream_tests.rs` tests with:

```rust
#[derive(Default)]
struct RecordingCallbacks {
    size: Option<usize>,
    objects: Vec<(Object, usize, usize)>,
    eof: bool,
    stop_after: Option<usize>,
}

impl ParserCallbacks for RecordingCallbacks {
    fn content_size(&mut self, size: usize) -> Result<()> {
        self.size = Some(size);
        Ok(())
    }

    fn handle_object(
        &mut self,
        object: Object,
        offset: usize,
        length: usize,
    ) -> Result<ParseControl> {
        self.objects.push((object, offset, length));
        Ok(if self.stop_after == Some(self.objects.len()) {
            ParseControl::Stop
        } else {
            ParseControl::Continue
        })
    }

    fn handle_eof(&mut self) -> Result<()> {
        self.eof = true;
        Ok(())
    }
}

#[test]
fn callbacks_receive_qpdf_object_offsets_lengths_and_eof() {
    let input = b"  1 2 cm\n";
    let mut callbacks = RecordingCallbacks::default();
    parse_content_stream_data(input, &mut callbacks).unwrap();

    assert_eq!(callbacks.size, Some(input.len()));
    assert_eq!(
        callbacks.objects,
        vec![
            (Object::Integer(1), 2, 1),
            (Object::Integer(2), 4, 1),
            (Object::Operator(b"cm".to_vec()), 6, 2),
        ]
    );
    assert!(callbacks.eof);
}

#[test]
fn early_stop_skips_handle_eof_like_qpdf() {
    let mut callbacks = RecordingCallbacks {
        stop_after: Some(1),
        ..RecordingCallbacks::default()
    };
    parse_content_stream_data(b"1 2 cm", &mut callbacks).unwrap();
    assert_eq!(callbacks.objects.len(), 1);
    assert!(!callbacks.eof);
}

#[test]
fn callbacks_report_inline_image_as_a_separate_qpdf_object_event() {
    let input = b"BI /W 1 /H 1 ID x EI Q";
    let mut callbacks = RecordingCallbacks::default();
    parse_content_stream_data(input, &mut callbacks).unwrap();

    assert_eq!(
        callbacks.objects,
        vec![
            (Object::Operator(b"BI".to_vec()), 0, 2),
            (Object::Name(b"W".to_vec()), 3, 2),
            (Object::Integer(1), 6, 1),
            (Object::Name(b"H".to_vec()), 8, 2),
            (Object::Integer(1), 11, 1),
            (Object::Operator(b"ID".to_vec()), 13, 2),
            (Object::InlineImage(b"x ".to_vec()), 16, 2),
            (Object::Operator(b"EI".to_vec()), 18, 2),
            (Object::Operator(b"Q".to_vec()), 21, 1),
        ]
    );
}
```

- [ ] **Step 3: Run focused tests and verify RED**

Run:

```bash
cargo test -p flpdf parser::tests::content_mode_returns_words_as_operators_and_never_builds_references -- --exact
cargo test -p flpdf --test content_stream_tests callbacks_receive_qpdf_object_offsets_lengths_and_eof -- --exact
cargo test -p flpdf --test content_stream_tests early_stop_skips_handle_eof_like_qpdf -- --exact
cargo test -p flpdf --test content_stream_tests callbacks_report_inline_image_as_a_separate_qpdf_object_event -- --exact
```

Expected: compilation failure because content mode, callback types, and parser
function are absent.

- [ ] **Step 4: Implement `Parser::parse_content_object`**

Add a `content_stream: bool` mode or a dedicated constructor. In the token
dispatch:

```rust
TokenType::Eof if self.content_stream => Ok(None),
TokenType::Word if self.content_stream => {
    Ok(Some(Object::Operator(token.value)))
}
TokenType::Integer if self.content_stream => {
    Ok(Some(Object::Integer(parse_integer_token(&token)?)))
}
```

Arrays and dictionaries still call the normal nested object builder. Do not
recognize indirect references in content mode. Preserve qpdf bad-token
diagnostics and the 500-level nesting guard.

- [ ] **Step 5: Replace content orchestration without migrating consumers yet**

Add the callback trait and `parse_content_stream_data` to
`content_stream.rs`, leaving the legacy API intact until all consumers move in
Tasks 7–9. For each event:

```rust
callbacks.content_size(input.len())?;
let mut tokenizer = Tokenizer::new(input);
tokenizer.allow_eof();

while tokenizer.position() < input.len() {
    let probe = tokenizer.read_token(true, 0)?;
    let offset = probe.start;
    tokenizer.set_position(offset)?;

    let mut parser = Parser::with_tokenizer(&mut tokenizer, true);
    let Some(object) = parser.parse_content_object()? else {
        break;
    };
    let length = tokenizer.position() - offset;
    let is_id = object.as_operator() == Some(b"ID");
    if callbacks.handle_object(object, offset, length)? == ParseControl::Stop {
        return Ok(());
    }

    if is_id {
        tokenizer.consume_one_byte()?;
        tokenizer.expect_inline_image().map_err(tokenizer_state_as_parse_error)?;
        let image = tokenizer.read_token(true, 0)?;
        let image_offset = image.start;
        let image_length = image.end - image.start;
        if image.token_type == TokenType::Bad {
            return Err(Error::parse(
                image.error_offset,
                "EOF found while reading inline image",
            ));
        }
        if callbacks.handle_object(
            Object::InlineImage(image.value),
            image_offset,
            image_length,
        )? == ParseControl::Stop
        {
            return Ok(());
        }
    }
}

callbacks.handle_eof()
```

Offset and length must match qpdf's non-ignorable-token start and consumed
cursor distance. `consume_one_byte()` returns a parse error when no separator
byte follows `ID`; it does not silently synthesize one.

- [ ] **Step 6: Run callback and parser tests**

Run:

```bash
cargo test -p flpdf parser::tests
cargo test -p flpdf --test content_stream_tests callbacks_
```

Expected: new callback tests and all old consumer tests pass. The legacy API
coexists in this commit; Task 7 and Task 8 perform the deliberate cutover, and
Task 9 deletes it.

- [ ] **Step 7: Commit parser/callback core**

```bash
git add crates/flpdf/src/parser.rs crates/flpdf/src/content_stream.rs \
  crates/flpdf/src/lib.rs crates/flpdf/tests/content_stream_tests.rs \
  crates/flpdf/tests/parser_tests.rs
git commit -m "feat(content): add qpdf parser callbacks"
```

Do not partially delete or rename the legacy API in this task. This makes the
parser/callback-core commit independently buildable without adding a second
compatibility adapter.

---

### Task 7: Migrate resource, default-appearance, and page-helper consumers

**Layer:** 3 — content cutover

**Files:**
- Modify: `crates/flpdf/src/resources.rs`
- Modify: `crates/flpdf/src/default_appearance.rs`
- Modify: `crates/flpdf/src/page_object_helper.rs`
- Modify: `crates/flpdf/tests/resource_pruning_tests.rs`
- Modify: `crates/flpdf/tests/page_object_helper_tests.rs`
- Modify: `crates/flpdf/tests/coalesce_tests.rs`

**Interfaces:**
- Consumes: `parse_content_stream_data`, `ParserCallbacks`, `ParseControl`.
- Produces:

```rust
pub(crate) struct OperationCallbacks<F> {
    operands: Vec<Object>,
    on_operation: F,
}

pub(crate) fn parse_content_operations<F>(
    input: &[u8],
    on_operation: F,
) -> Result<()>
where
    F: FnMut(&[Object], &[u8]) -> Result<ParseControl>;
```

`OperationCallbacks` is an event accumulator only. It must not inspect input
bytes or implement token boundaries. The convenience adapter ignores
`InlineImage`; consumers that need inline-image events, such as resource
discovery, implement `ParserCallbacks` directly.

- [ ] **Step 1: Write failing operation-adapter tests**

Add to `content_stream_tests.rs`:

```rust
#[test]
fn operation_adapter_groups_objects_without_lexing_bytes() {
    let mut seen = Vec::new();
    parse_content_operations(b"1 2 cm q", |operands, operator| {
        seen.push((operands.to_vec(), operator.to_vec()));
        Ok(ParseControl::Continue)
    })
    .unwrap();
    assert_eq!(
        seen,
        vec![
            (vec![Object::Integer(1), Object::Integer(2)], b"cm".to_vec()),
            (vec![], b"q".to_vec()),
        ]
    );
}
```

- [ ] **Step 2: Run the adapter test and verify RED**

Run:

```bash
cargo test -p flpdf --test content_stream_tests operation_adapter_groups_objects_without_lexing_bytes -- --exact
```

Expected: compilation failure because `parse_content_operations` is absent.

- [ ] **Step 3: Implement the callback-only operation adapter**

`handle_object` behavior:

```rust
match object {
    Object::Operator(operator) => {
        let control = (self.on_operation)(&self.operands, &operator)?;
        self.operands.clear();
        Ok(control)
    }
    Object::InlineImage(_) => Ok(ParseControl::Continue),
    operand => {
        self.operands.push(operand);
        Ok(ParseControl::Continue)
    }
}
```

On EOF, return a parse error if operands remain only where the existing
consumer requires strict dangling-operand detection. Recovery-oriented
consumers may explicitly discard them.

- [ ] **Step 4: Migrate `default_appearance.rs`**

Replace `ContentParseOptions`/`ContentToken` with
`parse_content_operations`. Preserve last-wins and malformed-token recovery:

```rust
let _ = parse_content_operations(da, |operands, operator| {
    match operator {
        b"Tf" => { /* existing last-two operand logic */ }
        b"g" => { /* existing last operand logic */ }
        b"rg" => { /* existing last-three operand logic */ }
        b"k" => { /* existing last-four operand logic */ }
        _ => {}
    }
    Ok(ParseControl::Continue)
});
```

Use the parser's qpdf bad-token recovery mode rather than manually skipping
one byte.

- [ ] **Step 5: Migrate `resources.rs` with a dedicated callback**

Create a `ResourceCallbacks` that maintains:

- the current operand stack;
- whether it is between `BI` and `ID`;
- inline-image key/value pairs needed to resolve `/CS`;
- the existing `CollectCtx`, `Scope`, and recursion depth.

On `Operator`, call the existing `process_operator`. On `Operator("BI")`,
enter inline-header mode. On `Operator("ID")`, inspect collected `/CS` or
`/ColorSpace`, then clear inline-header state. `InlineImage` itself contains
payload only and adds no resource name.

Malformed content returns `Ok(false)` exactly as the existing conservative
resource-pruning contract requires.

- [ ] **Step 6: Change page helper output to qpdf object events**

Replace:

```rust
pub fn content_streams(&mut self) -> Result<Vec<ContentToken>>
```

with:

```rust
pub fn content_stream_objects(&mut self) -> Result<Vec<Object>>
```

Implement it with a recording `ParserCallbacks`. Update its documentation and
tests to assert `Object::Operator` and `Object::InlineImage` events.

- [ ] **Step 7: Run focused consumer tests**

Run:

```bash
cargo test -p flpdf default_appearance
cargo test -p flpdf --test resource_pruning_tests
cargo test -p flpdf --test page_object_helper_tests
cargo test -p flpdf --test coalesce_tests
```

Expected: all migrated consumers pass without importing
`ContentStreamParser` or `ContentToken`.

- [ ] **Step 8: Commit non-appearance consumers**

```bash
git add crates/flpdf/src/content_stream.rs crates/flpdf/src/resources.rs \
  crates/flpdf/src/default_appearance.rs crates/flpdf/src/page_object_helper.rs \
  crates/flpdf/tests/content_stream_tests.rs \
  crates/flpdf/tests/resource_pruning_tests.rs \
  crates/flpdf/tests/page_object_helper_tests.rs \
  crates/flpdf/tests/coalesce_tests.rs
git commit -m "refactor(content): migrate core callback consumers"
```

---

### Task 8: Migrate appearance consumers without recreating a lexer

**Layer:** 3 — content cutover

**Files:**
- Modify: `crates/flpdf/src/appearance.rs`
- Modify: `crates/flpdf/src/overlay_annotations.rs` documentation reference
- Modify: appearance-related tests in `crates/flpdf/src/appearance.rs`

**Interfaces:**
- Consumes: `parse_content_operations` from Task 7.
- Produces: no new parsing interface.

- [ ] **Step 1: Inventory every appearance call site before editing**

Run:

```bash
rg -n "ContentStreamParser|ContentToken" crates/flpdf/src/appearance.rs
```

Expected on the design baseline: all call sites around lines 2213, 2241, 2431,
2564, 2711, 2815, 4214, 4287, 4359, 4532, 4844, 4917, 4984, 5069, 5143,
5245, 5313, 5551, 5578, 6709, and 6723 are listed. Save the fresh list in the
implementation log because line numbers may shift.

- [ ] **Step 2: Add a failing representative appearance regression**

Add a test named
`callback_parser_preserves_appearance_operation_sequence`. It covers one
consumer that reads operands, one that counts operators, and one that tolerates
malformed content, proving the same generated appearance bytes or extracted
values before/after callback migration. The representative operand case uses:

```rust
let content = b"q 1 0 0 1 12 34 cm /Fm0 Do Q";
let mut operations = Vec::new();
parse_content_operations(content, |operands, operator| {
    operations.push((operands.to_vec(), operator.to_vec()));
    Ok(ParseControl::Continue)
})
.unwrap();
assert_eq!(operations[2].1, b"Do");
assert_eq!(operations[2].0.last().and_then(Object::as_name), Some(b"Fm0".as_slice()));
```

- [ ] **Step 3: Run the representative test and verify RED**

Run the exact test name added in Step 2:

```bash
cargo test -p flpdf appearance::tests::callback_parser_preserves_appearance_operation_sequence -- --exact
```

Expected: failure because the appearance consumer still calls the removed
aggregate parser or because the new callback assertion is not yet wired.

- [ ] **Step 4: Convert every appearance call site**

Use `parse_content_operations` for operand/operator consumers. For
operator-only checks, ignore operands in the closure:

```rust
parse_content_operations(content, |_operands, operator| {
    if operator == b"f" {
        fills += 1;
    }
    Ok(ParseControl::Continue)
})?;
```

For tolerant scans that formerly used `.flatten()`, explicitly disposition
parse errors according to the existing function contract; do not silently
turn every error into success globally.

Do not introduce `read_keyword`, `skip_ws`, `starts_number_token`, direct byte
splitting, or a replacement `ContentToken` enum in `appearance.rs`.

- [ ] **Step 5: Prove the old parser types are absent from appearance**

Run:

```bash
rg -n "ContentStreamParser|ContentToken|read_keyword|starts_number_token" \
  crates/flpdf/src/appearance.rs
```

Expected: no matches.

- [ ] **Step 6: Run appearance and workspace-focused tests**

Run:

```bash
cargo test -p flpdf appearance
cargo test -p flpdf --all-features
```

Expected: all pass.

- [ ] **Step 7: Commit appearance migration**

```bash
git diff --name-only
git add crates/flpdf/src/appearance.rs crates/flpdf/src/overlay_annotations.rs
git diff --cached --stat
git commit -m "refactor(appearance): consume qpdf content events"
```

The regression belongs to `appearance.rs`'s existing internal test module.
Do not stage the whole tests directory.

---

### Task 9: Remove the legacy content lexer and bridge normalization

**Layer:** 3 — content cutover

**Files:**
- Modify: `crates/flpdf/src/content_stream.rs`
- Modify: `crates/flpdf/src/lib.rs`
- Modify: `crates/flpdf-cli/src/main.rs`
- Modify: `crates/flpdf-cli/tests/cli_optimization_matrix.rs`
- Modify: `crates/flpdf/tests/content_stream_tests.rs`
- Modify: any remaining files found by the required searches

**Interfaces:**
- Consumes: callback/event pipeline and operation adapter.
- Produces:
  - no `ContentStreamParser`, `ContentToken`, or `ContentParseOptions`;
  - `normalize_content_stream` remains temporarily callable but consumes
    callback events only;
  - all lexical boundary decisions originate in `tokenizer.rs`.

- [ ] **Step 1: Write a failing single-lexer contract test**

Add a workspace contract test under `crates/flpdf/tests/content_stream_tests.rs`
or a dedicated source-contract test:

```rust
#[test]
fn content_module_has_no_independent_lexer_helpers() {
    let source = include_str!("../src/content_stream.rs");
    for forbidden in [
        "skip_ws_collect_comment",
        "read_keyword",
        "at_operand_start",
        "starts_number_token",
        "fn parse_inline_image",
    ] {
        assert!(
            !source.contains(forbidden),
            "legacy lexical helper remains: {forbidden}"
        );
    }
}
```

Add compile-level assertions by removing imports of
`ContentStreamParser`/`ContentToken` from tests before implementation.

- [ ] **Step 2: Run focused tests and verify RED**

Run:

```bash
cargo test -p flpdf --test content_stream_tests content_module_has_no_independent_lexer_helpers -- --exact
```

Expected: failure listing the existing legacy helper names.

- [ ] **Step 3: Adapt temporary normalization to object events**

Implement a `NormalizationBridge` callback that accumulates operands and emits
the existing one-operator-per-line output. On `InlineImage`, emit the payload
using the current CLI contract only; do not scan for `BI`/`ID`/`EI` or
re-tokenize bytes.

This bridge is intentionally deleted/replaced by `flpdf-qxba.7`. Its only
allowed dependencies are `Object::write_pdf`, callback events, and an output
buffer.

- [ ] **Step 4: Delete old APIs and lexical helpers**

Remove:

- `ContentToken`;
- `ContentParseOptions`;
- `ContentStreamParser`;
- iterator implementation;
- byte cursor fields and helper methods;
- independent inline-image dictionary/payload scanner;
- public re-exports from `lib.rs`.

Update CLI and test imports to `normalize_content_stream`,
`parse_content_stream_data`, or the new callback types as appropriate.

- [ ] **Step 5: Prove all production consumers are cut over**

Run:

```bash
rg -n "ContentStreamParser|ContentToken|ContentParseOptions" \
  crates/flpdf/src crates/flpdf-cli/src crates/flpdf/tests crates/flpdf-cli/tests
```

Expected: no matches.

Run:

```bash
rg -n "skip_ws_collect_comment|read_keyword|at_operand_start|fn parse_inline_image" \
  crates/flpdf/src
```

Expected: no production lexical duplicate. Any same-name helper outside the
content/parser/tokenizer domain must be inspected and justified rather than
blindly renamed.

- [ ] **Step 6: Run library and CLI content tests**

Run:

```bash
cargo test -p flpdf --test content_stream_tests
cargo test -p flpdf --test resource_pruning_tests
cargo test -p flpdf --test page_object_helper_tests
cargo test -p flpdf-cli --test cli_tests rewrite_normalize_content
cargo test -p flpdf-cli --test cli_optimization_matrix
```

Expected: all pass using the new tokenizer route.

- [ ] **Step 7: Commit legacy deletion**

```bash
git add crates/flpdf/src/content_stream.rs crates/flpdf/src/lib.rs \
  crates/flpdf-cli/src/main.rs crates/flpdf/tests/content_stream_tests.rs \
  crates/flpdf-cli/tests/cli_optimization_matrix.rs
git commit -m "refactor(content): remove duplicate content lexer"
```

Add remaining migrated files explicitly if the searches identified them.

---

### Task 10: Add differential evidence, update correspondence, and gate Layer 3

**Layer:** 3 — content cutover

**Files:**
- Create: `scripts/qpdf-tokenizer-diff.sh`
- Create: `tests/oracle/qpdf_tokenizer_probe.cc`
- Create: `crates/flpdf/tests/tokenizer_oracle_vectors.rs` only if the public test surface can exercise the internal probe without exposing tokenizer; otherwise keep the ignored differential unit test in `tokenizer.rs`
- Modify: `crates/flpdf/src/tokenizer.rs`
- Modify: `docs/qpdf-correspondence.md`
- Modify: `docs/superpowers/specs/2026-07-27-qpdf-tokenizer-all-modes-design.md` only if implementation revealed an approved design correction

**Interfaces:**
- Consumes: completed tokenizer and content callback stack.
- Produces:
  - ignored live test `qpdf_tokenizer_differential_all_modes`;
  - script exit 0 only when qpdf/flpdf token records match;
  - correspondence row marked complete with source citations.

- [ ] **Step 1: Write the ignored differential test before the script**

In `tokenizer.rs`, add:

```rust
#[test]
#[ignore = "live qpdf 11.9.0 tokenizer oracle"]
fn qpdf_tokenizer_differential_all_modes() {
    let probe = std::env::var_os("QPDF_TOKENIZER_PROBE")
        .expect("set QPDF_TOKENIZER_PROBE to the built qpdf 11.9.0 probe");
    let cases = qpdf_oracle_cases();
    for case in cases {
        let qpdf = run_qpdf_probe(&probe, &case);
        let flpdf = dump_flpdf_tokens(&case);
        assert_eq!(flpdf, qpdf, "case {}", case.name);
    }
}
```

`qpdf_oracle_cases()` uses only flpdf-authored byte arrays and covers push,
pull, ignorable, EOF, max length, raw/canonical value, bad recovery, unread,
offset, and inline image.

- [ ] **Step 2: Run the ignored test and verify RED**

Run:

```bash
cargo test -p flpdf qpdf_tokenizer_differential_all_modes \
  -- --ignored --exact
```

Expected: failure explaining that `QPDF_TOKENIZER_PROBE` is unset or that the
probe script/binary is absent.

- [ ] **Step 3: Implement the live oracle script**

`scripts/qpdf-tokenizer-diff.sh` must:

```bash
#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
qpdf_source="$("${repo_root}/scripts/fetch-qpdf-source.sh" --print-path)"
build_dir="${TMPDIR:-/tmp}/flpdf-qpdf-tokenizer-probe-11.9.0"

cmake -S "${qpdf_source}" -B "${build_dir}" \
  -DBUILD_STATIC_LIBS=OFF \
  -DBUILD_SHARED_LIBS=ON \
  -DREQUIRE_CRYPTO_NATIVE=OFF
cmake --build "${build_dir}" --target libqpdf --parallel

c++ -std=c++17 \
  -I"${qpdf_source}/include" \
  "${repo_root}/tests/oracle/qpdf_tokenizer_probe.cc" \
  -L"${build_dir}/libqpdf" \
  -Wl,-rpath,"${build_dir}/libqpdf" \
  -lqpdf \
  -o "${build_dir}/flpdf-qpdf-tokenizer-probe"
```

The committed flpdf-authored C++ probe accepts hex-encoded input and explicit
push/pull, ignorable, EOF, max-length, and inline-image flags. It links to the
pinned shared `libqpdf` and emits one tab-separated record per token:

```text
type<TAB>value_hex<TAB>raw_hex<TAB>error_hex<TAB>start<TAB>end<TAB>unread_hex
```

Then set `QPDF_TOKENIZER_PROBE` and run the ignored Rust test. The script may
cache build artifacts outside the repository but must never modify the pinned
source tree.

- [ ] **Step 4: Run the live differential test and resolve every mismatch**

Run:

```bash
scripts/qpdf-tokenizer-diff.sh
```

Expected: exit 0 with every authored case matching. For each mismatch, add a
minimal non-ignored Rust regression first, observe it fail, fix the
corresponding source-backed branch, and rerun.

- [ ] **Step 5: Update qpdf correspondence documentation**

Change the `QPDFTokenizer.cc` row in `docs/qpdf-correspondence.md` from partial
to complete and list:

- all 18 token types;
- push/pull;
- EOF and ignorable;
- raw/error/max length;
- `betweenTokens`;
- inline image;
- parser/content callback consumers;
- deletion of the old content lexer;
- follow-up `Pl_QPDFTokenizer`/`ContentNormalizer` issue `flpdf-qxba.7`.

Do not mark `Pl_QPDFTokenizer.cc` or `ContentNormalizer.cc` complete in this
issue.

- [ ] **Step 6: Run external qtest after the full cutover**

```bash
implementation_root="$(git rev-parse --show-toplevel)"
cargo build --release -p flpdf-cli -p flpdf-test-compare
cd /home/ubuntu/flpdf-qtest
FLPDF_DIR="$implementation_root" \
QTEST_TESTS="tokenizer token-filters basic-parsing inline-images" \
scripts/run.sh
```

Expected: harness completes. Compare exact subtest identities with Task 1's
baseline and record improvements/regressions. Any regression caused by this
stack must be fixed; unrelated pre-existing failures remain reported, not
patched opportunistically.

- [ ] **Step 7: Run the complete Layer 3 verification**

Run:

```bash
cargo fmt --all -- --check
cargo test -p flpdf --test reader_tests
cargo test -p flpdf --test xref_tests
cargo test -p flpdf --test check_tests
cargo test -p flpdf --test writer_tests
cargo test -p flpdf-cli --test cli_tests
cargo test -p flpdf-cli --test compat_matrix_tests
cargo test
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" \
  cargo doc --workspace --no-deps --document-private-items
```

Expected: all pass. `compat_matrix_tests` may skip only under its established
missing-qpdf rule; on Linux CI qpdf must be present.

- [ ] **Step 8: Commit oracle/docs changes**

```bash
git add scripts/qpdf-tokenizer-diff.sh tests/oracle/qpdf_tokenizer_probe.cc \
  crates/flpdf/src/tokenizer.rs docs/qpdf-correspondence.md
git commit -m "test(tokenizer): lock qpdf all-mode parity"
```

- [ ] **Step 9: Measure Layer 3 committed patch coverage**

Run:

```bash
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail \
  --lcov --output-path target/layer3.lcov
scripts/patch-coverage.sh \
  --base origin/feature/flpdf-n9t0-1-tokenizer-inline-image \
  --lcov target/layer3.lcov
```

Expected: changed lines `100.00%`. Add tests for uncovered branches and
recommit until the committed head is 100%.

- [ ] **Step 10: Push Layer 3 and Beads**

```bash
git push origin feature/flpdf-n9t0-1-tokenizer-content
bd dolt push
```

Confirm local and remote heads match:

```bash
git rev-parse HEAD
git rev-parse origin/feature/flpdf-n9t0-1-tokenizer-content
```

Expected: identical SHAs.

---

### Task 11: Publish the three draft PRs and hand off exact state

**Files:**
- Read: `.github/pull_request_template.md`
- No source changes expected

**Interfaces:**
- Consumes: pushed Layer 1, Layer 2, and Layer 3 branches.
- Produces: three draft PRs with dependency bases and exact verification
  evidence.

- [ ] **Step 1: Verify every stack boundary**

Run:

```bash
git fetch origin
git log --oneline origin/main..origin/feature/flpdf-n9t0-1-tokenizer
git log --oneline \
  origin/feature/flpdf-n9t0-1-tokenizer..origin/feature/flpdf-n9t0-1-tokenizer-inline-image
git log --oneline \
  origin/feature/flpdf-n9t0-1-tokenizer-inline-image..origin/feature/flpdf-n9t0-1-tokenizer-content
```

Expected: each range contains only that layer's intended commits.

- [ ] **Step 2: Create Layer 1 draft PR**

Use the repository template sections and include the Layer 1 focused tests,
full gates, and patch-coverage result. Create
`/tmp/flpdf-n9t0-layer1-pr.md` with the template's Summary, Test plan, and
Compat matrix headings; replace recorded result values with the exact outputs
from Tasks 1–3. Create the file with `apply_patch`, not shell redirection:

```bash
gh pr create --draft \
  --base main \
  --head feature/flpdf-n9t0-1-tokenizer \
  --title "refactor: complete qpdf tokenizer core modes" \
  --body-file /tmp/flpdf-n9t0-layer1-pr.md
```

- [ ] **Step 3: Create Layer 2 draft PR**

Create `/tmp/flpdf-n9t0-layer2-pr.md` from the same template, summarizing only
inline-image discovery/tokenization and listing the exact Layer 2 test and
patch-coverage results. Create it with `apply_patch`, then run:

```bash
gh pr create --draft \
  --base feature/flpdf-n9t0-1-tokenizer \
  --head feature/flpdf-n9t0-1-tokenizer-inline-image \
  --title "feat: match qpdf inline image tokenization" \
  --body-file /tmp/flpdf-n9t0-layer2-pr.md
```

- [ ] **Step 4: Create Layer 3 draft PR**

Create `/tmp/flpdf-n9t0-layer3-pr.md` from the same template, summarizing
parser content mode, callbacks, consumer cutover, legacy lexer deletion, live
oracle, and exact Layer 3 verification. Create it with `apply_patch`, then run:

```bash
gh pr create --draft \
  --base feature/flpdf-n9t0-1-tokenizer-inline-image \
  --head feature/flpdf-n9t0-1-tokenizer-content \
  --title "refactor: route content parsing through qpdf tokenizer" \
  --body-file /tmp/flpdf-n9t0-layer3-pr.md
```

- [ ] **Step 5: Verify PR metadata and checks**

Resolve the PR numbers from their exact heads, then inspect each:

```bash
layer1_pr="$(gh pr list --head feature/flpdf-n9t0-1-tokenizer --json number --jq '.[0].number')"
layer2_pr="$(gh pr list --head feature/flpdf-n9t0-1-tokenizer-inline-image --json number --jq '.[0].number')"
layer3_pr="$(gh pr list --head feature/flpdf-n9t0-1-tokenizer-content --json number --jq '.[0].number')"

gh pr view "$layer1_pr" --json number,url,isDraft,baseRefName,headRefName,state
gh pr view "$layer2_pr" --json number,url,isDraft,baseRefName,headRefName,state
gh pr view "$layer3_pr" --json number,url,isDraft,baseRefName,headRefName,state
gh pr checks "$layer1_pr"
gh pr checks "$layer2_pr"
gh pr checks "$layer3_pr"
```

Expected: OPEN draft PR, exact base/head pairing above. Report pending checks
as pending; do not claim CI success before completion.

- [ ] **Step 6: Update and close Beads only after implementation is complete**

When all three branches are pushed, all local gates pass, differential evidence
is recorded, and draft PRs exist:

```bash
bd close flpdf-n9t0.1 --reason "QPDFTokenizer 11.9.0 all modes, inline-image scanning, parser content mode, and callback consumer cutover implemented in a three-PR stack; old content lexer removed; local quality and per-layer 100% patch-coverage gates passed"
bd dolt push
```

If any required gate remains red, leave the Bead `in_progress` and record the
exact failing command instead of closing it.

- [ ] **Step 7: Final handoff**

Report:

- all three PR URLs and base/head relationships;
- branch head SHAs;
- focused/workspace/Clippy/rustdoc results;
- per-layer patch coverage numerator/denominator and 100%;
- live qpdf differential result;
- exact qtest before/after subtest changes;
- Beads state and Dolt push result;
- remaining untracked user file, unchanged.
