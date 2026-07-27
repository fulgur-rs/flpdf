# qpdf Content Normalizer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Beads is the source of truth for execution status; the numbered steps below are procedural instructions.

**Goal:** Replace flpdf's object-reconstructing content normalization with byte-for-byte qpdf 11.9.0 `Pl_QPDFTokenizer` plus `ContentNormalizer` behavior in the library and CLI.

**Architecture:** Keep `tokenizer.rs` as the sole lexical state machine. Add a focused `content_normalizer.rs` runner that drives the existing tokenizer and a stateful filter that writes qpdf-normalized token bytes and records bad-token state; then cut the public API and CLI over to that module and delete `NormalizationBridge`.

**Tech Stack:** Rust 2021 workspace, qpdf 11.9.0 C++ oracle, `assert_cmd`, pinned qpdf build probe, `cargo llvm-cov`.

## Global Constraints

- The behavioral oracle is qpdf 11.9.0 at commit `3b97c9bd266b7c32ea36d3536e22dab77412886d`.
- qpdf fidelity takes precedence over compatibility with flpdf's old one-operation-per-line bytes and malformed-content `Result::Err`.
- Do not implement another tokenizer; `crates/flpdf/src/tokenizer.rs` remains the only production lexical state machine and inline-image scanner.
- Do not add a general qpdf `Pipeline` hierarchy; use the already-buffered decoded content bytes and a focused private filter runner.
- `normalize_content_stream(&[u8])` is input-infallible and returns normalized bytes plus `any_bad_tokens` and `last_token_was_bad`.
- Preserve comments and raw token spacing; transform only qpdf's CR/CRLF space handling and canonical string/name representation.
- A bad token never aborts normalization. The CLI writes output, emits qpdf warning payloads, prints the qpdf warning-success summary, and exits 3.
- Normalize and warn once per distinct indirect content stream reference, matching qpdf's `normalized_streams` object-generation set.
- Do not vendor qpdf-qtest fixtures. Test PDFs must be flpdf-authored or built in test code.
- Every production change follows RED → verify RED → GREEN → verify GREEN → REFACTOR.
- CI patch coverage is a fresh whole-workspace run and must cover 100% of changed executable lines under `crates/flpdf/src`.
- Preserve unrelated worktree and repository state; stage only files listed by each task.

## File Map

- Create `crates/flpdf/src/content_normalizer.rs`: focused token-filter runner, qpdf `ContentNormalizer` port, public result, unit and differential tests.
- Modify `crates/flpdf/src/tokenizer.rs`: exact qpdf constructed-string spelling and one raw-byte cursor primitive; no new lexical state.
- Modify `crates/flpdf/src/content_stream.rs`: delete the transitional `NormalizationBridge` and normalization API.
- Modify `crates/flpdf/src/lib.rs`: publish and re-export the new module/API.
- Modify `crates/flpdf/tests/content_stream_tests.rs`: retain parser coverage and replace old normalizer contract coverage.
- Modify `crates/flpdf-cli/src/main.rs`: consume `ContentNormalization`, emit qpdf warnings, preserve output, exit 3.
- Modify `crates/flpdf-cli/tests/cli_tests.rs`: warning/exit/output regression coverage.
- Modify `crates/flpdf-cli/tests/cli_optimization_matrix.rs`: qpdf byte-parity content-stream E2E coverage and removal of stale divergence prose.
- Modify `tests/oracle/qpdf_tokenizer_probe.cc`: add a `Pl_QPDFTokenizer + ContentNormalizer` mode.
- Modify `scripts/qpdf-tokenizer-diff.sh`: compile the private qpdf normalizer source and run both exact ignored oracle tests.
- Modify `scripts/tests/qpdf-tokenizer-diff-contract.sh`: keep the secure build-driver contract aligned with the two oracle invocations.
- Modify `docs/qpdf-correspondence.md` and regenerate `docs/qpdf-module-doc-index.md`.

---

### Task 1: Complete the existing tokenizer's constructed-token support

**Files:**
- Modify: `crates/flpdf/src/tokenizer.rs:1-108`
- Modify: `crates/flpdf/src/tokenizer.rs:805-834`
- Test: `crates/flpdf/src/tokenizer.rs:2235-2243`

**Interfaces:**
- Consumes: existing `Token::new(TokenType, Vec<u8>)`, `Tokenizer::position`, and private `Tokenizer::reset`.
- Produces: qpdf-faithful `Token::new(TokenType::String, value).raw` and `Tokenizer::consume_one_byte_or(default: u8) -> u8` for Task 2.

#### Step 1: Expand the constructed-token test with hand-derived qpdf literals

Replace the narrow `constructed_name_and_string_tokens_have_canonical_pdf_raw_values` body with:

```rust
#[test]
fn constructed_name_and_string_tokens_have_qpdf_canonical_raw_values() {
    let name = Token::new(TokenType::Name, b"/text/plain".to_vec());
    assert_eq!(name.raw, b"/text#2fplain");

    for (value, expected) in [
        (b"a(b".as_slice(), br"(a\(b)".as_slice()),
        (b"a\nb".as_slice(), br"(a\nb)".as_slice()),
        (b"\x01".as_slice(), b"<01>".as_slice()),
        (b"\x18abcd".as_slice(), br"(\030abcd)".as_slice()),
        (b"\xa0abcd".as_slice(), b"(\xa0abcd)".as_slice()),
        (b"\xa0abc".as_slice(), b"<a0616263>".as_slice()),
    ] {
        assert_eq!(
            Token::new(TokenType::String, value.to_vec()).raw,
            expected,
            "value {value:?}"
        );
    }
}
```

This test catches replacing qpdf's PDFDoc/ISO-Latin-1 heuristic with flpdf's existing printable-ASCII-only object serializer.

#### Step 2: Run the constructed-token test and verify RED

Run:

```bash
cargo test -p flpdf --lib tokenizer::tests::constructed_name_and_string_tokens_have_qpdf_canonical_raw_values -- --exact
```

Expected: the assertion fails because newline/control/ISO-Latin-1 spellings
differ.

#### Step 3: Implement qpdf's constructed-string spelling without changing `Object::write_pdf`

Remove `object::write_string_value` from the imports. Replace `canonical_string_raw` with:

```rust
fn canonical_string_raw(value: &[u8]) -> Vec<u8> {
    let mut non_ascii = 0usize;
    let mut force_hex = false;
    for &byte in value {
        if byte > 126 {
            non_ascii += 1;
        } else if byte >= 32 {
            continue;
        } else if byte >= 24 {
            non_ascii += 1;
        } else if !matches!(byte, b'\n' | b'\r' | b'\t' | b'\x08' | b'\x0c') {
            force_hex = true;
            break;
        }
    }
    let use_hex = force_hex || 5 * non_ascii > value.len();
    if use_hex {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut raw = Vec::with_capacity(value.len() * 2 + 2);
        raw.push(b'<');
        for &byte in value {
            raw.push(HEX[(byte >> 4) as usize]);
            raw.push(HEX[(byte & 0x0f) as usize]);
        }
        raw.push(b'>');
        return raw;
    }

    let mut raw = Vec::with_capacity(value.len() + 2);
    raw.push(b'(');
    for &byte in value {
        match byte {
            b'\n' => raw.extend_from_slice(br"\n"),
            b'\r' => raw.extend_from_slice(br"\r"),
            b'\t' => raw.extend_from_slice(br"\t"),
            b'\x08' => raw.extend_from_slice(br"\b"),
            b'\x0c' => raw.extend_from_slice(br"\f"),
            b'(' => raw.extend_from_slice(br"\("),
            b')' => raw.extend_from_slice(br"\)"),
            b'\\' => raw.extend_from_slice(br"\\"),
            32..=126 | 160..=255 => raw.push(byte),
            _ => {
                raw.push(b'\\');
                raw.push(b'0' + ((byte >> 6) & 0x07));
                raw.push(b'0' + ((byte >> 3) & 0x07));
                raw.push(b'0' + (byte & 0x07));
            }
        }
    }
    raw.push(b')');
    raw
}
```

Keep the object serializer unchanged so this task does not absorb the separate `good9` writer-format issue.

#### Step 4: Run the constructed-token test and verify GREEN

Run:

```bash
cargo test -p flpdf --lib tokenizer::tests::constructed_name_and_string_tokens_have_qpdf_canonical_raw_values -- --exact
```

Expected: pass.

#### Step 5: Add the missing cursor test

Add beside the constructed-token test:

```rust
#[test]
fn consume_one_byte_or_returns_input_then_default_without_advancing_past_eof() {
    let mut tokenizer = Tokenizer::new(b"xy");

    assert_eq!(tokenizer.consume_one_byte_or(b' '), b'x');
    assert_eq!(tokenizer.position(), 1);
    assert_eq!(tokenizer.consume_one_byte_or(b' '), b'y');
    assert_eq!(tokenizer.position(), 2);
    assert_eq!(tokenizer.consume_one_byte_or(b' '), b' ');
    assert_eq!(tokenizer.position(), 2);
}
```

This catches advancing beyond EOF or returning no qpdf-compatible default byte after a terminal `ID`.

#### Step 6: Run the cursor test and verify RED

Run:

```bash
cargo test -p flpdf --lib tokenizer::tests::consume_one_byte_or_returns_input_then_default_without_advancing_past_eof -- --exact
```

Expected: compilation fails because `consume_one_byte_or` does not exist.

#### Step 7: Add the qpdf-compatible byte primitive

Add next to `consume_one_byte`:

```rust
pub(crate) fn consume_one_byte_or(&mut self, default: u8) -> u8 {
    let byte = self.input.get(self.pos).copied().unwrap_or(default);
    if self.pos < self.input.len() {
        self.pos += 1;
    }
    self.reset();
    byte
}
```

It retrieves bytes only. It must not classify whitespace, look for `EI`, or alter `allow_eof`/`include_ignorable`.

#### Step 8: Run focused and tokenizer tests and verify GREEN

Run:

```bash
cargo test -p flpdf --lib tokenizer::tests::constructed_name_and_string_tokens_have_qpdf_canonical_raw_values -- --exact
cargo test -p flpdf --lib tokenizer::tests::consume_one_byte_or_returns_input_then_default_without_advancing_past_eof -- --exact
cargo test -p flpdf --lib tokenizer::tests
```

Expected: all pass.

#### Step 9: Format, inspect, and commit

Run:

```bash
cargo fmt --all
git diff --check
git diff -- crates/flpdf/src/tokenizer.rs
git add crates/flpdf/src/tokenizer.rs
git commit -m "fix(tokenizer): mirror qpdf constructed tokens"
```

---

### Task 2: Add the focused qpdf content normalizer

**Files:**
- Create: `crates/flpdf/src/content_normalizer.rs`
- Modify: `crates/flpdf/src/lib.rs:82-100`
- Test: `crates/flpdf/src/content_normalizer.rs` inline `#[cfg(test)]` module

**Interfaces:**
- Consumes: `Token`, `TokenType`, `Tokenizer`, `Tokenizer::consume_one_byte_or`, `Tokenizer::expect_inline_image`.
- Produces:
  - `pub struct ContentNormalization`
  - `pub fn normalize_content_stream(input: &[u8]) -> ContentNormalization`
  - methods `as_bytes`, `into_bytes`, `any_bad_tokens`, `last_token_was_bad`.
- Task 3 initially consumes this API through `flpdf::content_normalizer`; the crate-root re-export remains on the old function until the cutover.

#### Step 1: Declare the module and add RED tests for qpdf byte behavior

Add `pub mod content_normalizer;` immediately before `pub mod content_stream;` in `lib.rs`.

Create `content_normalizer.rs` with the module correspondence line and these
tests before adding the production definitions:

```rust
//! Mirrors qpdf 11.9.0 libqpdf/Pl_QPDFTokenizer.cc,
//! libqpdf/ContentNormalizer.cc.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokenizer::TokenType;

    #[test]
    fn preserves_layout_comments_and_only_normalizes_qpdf_token_forms() {
        let result =
            normalize_content_stream(b"% keep\r\nBT  /N#61me (a\rb) Tj\rQ");
        assert_eq!(result.as_bytes(), b"% keep\nBT  /Name (a\\nb)\n Tj\nQ");
        assert!(!result.any_bad_tokens());
        assert!(!result.last_token_was_bad());
    }

    #[test]
    fn normalizes_every_pdf_space_shape_without_collapsing_it() {
        let result = normalize_content_stream(b"q \t\0\x0c\r\r\n\nQ");
        assert_eq!(result.as_bytes(), b"q \t\0\x0c\n\n\nQ");
    }

    #[test]
    fn bad_token_state_clears_only_after_a_non_eof_good_token() {
        let recovered = normalize_content_stream(b"<0g> q");
        assert_eq!(recovered.as_bytes(), b"<0g> q");
        assert!(recovered.any_bad_tokens());
        assert!(!recovered.last_token_was_bad());

        let consecutive_then_recovered = normalize_content_stream(b")) q");
        assert_eq!(consecutive_then_recovered.as_bytes(), b")) q");
        assert!(consecutive_then_recovered.any_bad_tokens());
        assert!(!consecutive_then_recovered.last_token_was_bad());

        let terminal = normalize_content_stream(b"<0g");
        assert_eq!(terminal.as_bytes(), b"<0g");
        assert!(terminal.any_bad_tokens());
        assert!(terminal.last_token_was_bad());
    }

    #[test]
    fn id_at_eof_injects_default_space_then_reports_bad_inline_image() {
        let result = normalize_content_stream(b"ID");
        assert_eq!(result.as_bytes(), b"ID ");
        assert!(result.any_bad_tokens());
        assert!(result.last_token_was_bad());
    }

    #[test]
    fn id_separator_is_consumed_as_one_synthetic_space_token() {
        for (input, expected) in [
            (b"BI ID raw EI Q".as_slice(), b"BI ID raw EI Q".as_slice()),
            (b"BI ID\traw EI Q".as_slice(), b"BI ID\traw EI Q".as_slice()),
            (b"BI ID\nraw EI Q".as_slice(), b"BI ID\nraw EI Q".as_slice()),
            (b"BI ID\rraw EI Q".as_slice(), b"BI ID\nraw EI Q".as_slice()),
            (
                b"BI ID\r\nraw EI Q".as_slice(),
                b"BI ID\n\nraw EI Q".as_slice(),
            ),
            (
                b"BI ID\0raw EI Q".as_slice(),
                b"BI ID\0raw EI Q".as_slice(),
            ),
        ] {
            assert_eq!(normalize_content_stream(input).as_bytes(), expected);
        }
    }

    #[test]
    fn inline_image_payload_and_false_ei_candidates_remain_raw() {
        let input = b"BI /W 1 ID \0\xff EI A1 two EI Q";
        let result = normalize_content_stream(input);
        assert_eq!(result.as_bytes(), input);
        assert!(!result.any_bad_tokens());
    }

    #[derive(Default)]
    struct RecordingFilter(Vec<TokenType>);

    impl TokenFilter for RecordingFilter {
        fn handle_token(&mut self, token: &Token) {
            self.0.push(token.token_type);
        }

        fn handle_eof(&mut self) {
            self.0.push(TokenType::BraceOpen);
        }
    }

    #[test]
    fn runner_delivers_eof_token_before_handle_eof() {
        let mut filter = RecordingFilter::default();
        run_token_filter(b"q", &mut filter);
        assert_eq!(
            filter.0,
            vec![TokenType::Word, TokenType::Eof, TokenType::BraceOpen]
        );
    }
}
```

The `BraceOpen` marker is test-only and distinguishes `handle_eof` from the real EOF token without a mock.

#### Step 2: Run the new tests and verify RED

Run:

```bash
cargo test -p flpdf --lib content_normalizer::tests
```

Expected: compilation fails because the result type, filter runner, and normalizer do not exist.

#### Step 3: Implement the result and private filter contract

Add:

```rust
use crate::tokenizer::{Token, TokenType, Tokenizer};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentNormalization {
    bytes: Vec<u8>,
    any_bad_tokens: bool,
    last_token_was_bad: bool,
}

impl ContentNormalization {
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    #[must_use]
    pub fn any_bad_tokens(&self) -> bool {
        self.any_bad_tokens
    }

    #[must_use]
    pub fn last_token_was_bad(&self) -> bool {
        self.last_token_was_bad
    }
}

trait TokenFilter {
    fn handle_token(&mut self, token: &Token);
    fn handle_eof(&mut self);
}
```

#### Step 4: Implement the qpdf `Pl_QPDFTokenizer` runner

Add:

```rust
fn run_token_filter(input: &[u8], filter: &mut impl TokenFilter) {
    let mut tokenizer = Tokenizer::new(input);
    tokenizer.allow_eof();
    tokenizer.include_ignorable();

    loop {
        let token = tokenizer
            .read_token(true, 0)
            .expect("allow-bad qpdf tokenization is input-infallible");
        let is_eof = token.token_type == TokenType::Eof;
        let is_id = token.is_word_value(b"ID");
        filter.handle_token(&token);
        if is_eof {
            break;
        }
        if is_id {
            let separator = tokenizer.consume_one_byte_or(b' ');
            filter.handle_token(&Token::new(TokenType::Space, vec![separator]));
            tokenizer
                .expect_inline_image()
                .expect("ID handling leaves the tokenizer between tokens");
        }
    }
    filter.handle_eof();
}
```

This must reuse `Tokenizer::expect_inline_image`; do not scan `input` in this module.

#### Step 5: Implement the qpdf `ContentNormalizer` state and token rules

Add:

```rust
#[derive(Default)]
struct ContentNormalizer {
    output: Vec<u8>,
    any_bad_tokens: bool,
    last_token_was_bad: bool,
}

impl ContentNormalizer {
    fn write_space(&mut self, raw: &[u8]) {
        for (index, &byte) in raw.iter().enumerate() {
            if byte == b'\r' {
                if raw.get(index + 1) != Some(&b'\n') {
                    self.output.push(b'\n');
                }
            } else {
                self.output.push(byte);
            }
        }
    }

    fn finish(self) -> ContentNormalization {
        ContentNormalization {
            bytes: self.output,
            any_bad_tokens: self.any_bad_tokens,
            last_token_was_bad: self.last_token_was_bad,
        }
    }
}

impl TokenFilter for ContentNormalizer {
    fn handle_token(&mut self, token: &Token) {
        if token.token_type == TokenType::Bad {
            self.any_bad_tokens = true;
            self.last_token_was_bad = true;
        } else if token.token_type != TokenType::Eof {
            self.last_token_was_bad = false;
        }

        match token.token_type {
            TokenType::Space => self.write_space(&token.raw),
            TokenType::String | TokenType::Name => {
                let canonical = Token::new(token.token_type, token.value.clone());
                self.output.extend_from_slice(&canonical.raw);
            }
            _ => self.output.extend_from_slice(&token.raw),
        }

        if matches!(token.token_type, TokenType::String | TokenType::Name)
            && token.raw.iter().any(|byte| matches!(*byte, b'\r' | b'\n'))
        {
            self.output.push(b'\n');
        }
    }

    fn handle_eof(&mut self) {}
}

#[must_use]
pub fn normalize_content_stream(input: &[u8]) -> ContentNormalization {
    let mut normalizer = ContentNormalizer::default();
    run_token_filter(input, &mut normalizer);
    normalizer.finish()
}
```

#### Step 6: Run the focused tests and verify GREEN

Run:

```bash
cargo test -p flpdf --lib content_normalizer::tests
cargo test -p flpdf --lib tokenizer::tests
```

Expected: all pass. If a literal differs, verify it against the pinned qpdf probe/source rather than adapting the production behavior to the old flpdf output.

#### Step 7: Refactor only after GREEN

Inspect the module and confirm:

- no object parser or `Object` dependency;
- no `BTreeMap`;
- no source-byte scanning other than iterating a `Space` token's own raw bytes;
- one call to `expect_inline_image`;
- EOF token precedes `handle_eof`.

Run:

```bash
cargo fmt --all
cargo clippy -p flpdf --lib -- -D warnings
git diff --check
```

#### Step 8: Commit the focused component

```bash
git add crates/flpdf/src/content_normalizer.rs crates/flpdf/src/lib.rs
git commit -m "feat(content): add qpdf token normalizer"
```

---

### Task 3: Cut the public library and existing callers over and delete the bridge

**Files:**
- Modify: `crates/flpdf/src/content_stream.rs:1-16`
- Delete from: `crates/flpdf/src/content_stream.rs:201-414`
- Modify: `crates/flpdf/src/lib.rs:174-181`
- Modify: `crates/flpdf/tests/content_stream_tests.rs:1-4`
- Replace: `crates/flpdf/tests/content_stream_tests.rs:463-775`
- Modify: `crates/flpdf-cli/src/main.rs:20-32`
- Modify: `crates/flpdf-cli/src/main.rs:4430-4470`
- Modify: `crates/flpdf-cli/tests/cli_optimization_matrix.rs:180-220`

**Interfaces:**
- Consumes: Task 2 `ContentNormalization` and `content_normalizer::normalize_content_stream`.
- Produces: crate-root `flpdf::normalize_content_stream(&[u8]) -> ContentNormalization`; no production `NormalizationBridge`.
- Task 4 consumes `any_bad_tokens`/`last_token_was_bad` in the CLI.

#### Step 1: Write the public re-export regression before changing exports

Replace the normalization-only section in `content_stream_tests.rs` with:

```rust
#[test]
fn public_normalizer_reexport_preserves_qpdf_token_layout() {
    let result = flpdf::normalize_content_stream(b"% c\r\nBT  /N#61me Q");
    assert_eq!(result.as_bytes(), b"% c\nBT  /Name Q");
    assert!(!result.any_bad_tokens());
}
```

Remove `use flpdf::content_stream::normalize_content_stream;`. Keep all callback/parser tests outside the deleted normalization section unchanged.

#### Step 2: Run and verify RED

Run:

```bash
cargo test -p flpdf --test content_stream_tests public_normalizer_reexport_preserves_qpdf_token_layout -- --exact
```

Expected: compilation fails because the crate-root function still returns `Result<Vec<u8>>`.

#### Step 3: Move the public export

In `lib.rs`, add:

```rust
pub use content_normalizer::{normalize_content_stream, ContentNormalization};
```

and remove `normalize_content_stream` from the `pub use content_stream::{...}` list.

#### Step 4: Delete the transitional normalizer

From `content_stream.rs`, delete:

- `NormalizationState`;
- `NormalizationBridge`;
- its `ParserCallbacks` implementation;
- the old `normalize_content_stream`;
- `BTreeMap` and `is_ws` imports used only by that code.

Change the module correspondence header to:

```rust
//! qpdf correspondence: QPDFParser.cc content callbacks.
```

Do not change `parse_content_stream_data`, `OperationCallbacks`, or `parse_content_operations`.

#### Step 5: Make the CLI compile against the new result

In `normalize_and_store_stream`, replace:

```rust
let normalized = normalize_content_stream(&decoded)?;
```

with:

```rust
let normalized = normalize_content_stream(&decoded).into_bytes();
```

In `cli_optimization_matrix.rs`, replace `.expect(...)` calls with:

```rust
let expected = normalize_content_stream(&in_content).into_bytes();
let normalized_out = normalize_content_stream(&out_content).into_bytes();
```

Do not add warnings in this task; Task 4 adds the status-aware CLI flow under its own RED tests.

#### Step 6: Run focused crate and CLI tests and verify GREEN

Run:

```bash
cargo test -p flpdf --test content_stream_tests
cargo test -p flpdf --lib content_normalizer::tests
cargo test -p flpdf-cli --test cli_optimization_matrix normalize_content
cargo test -p flpdf-cli --test cli_tests rewrite_normalize_content
```

Expected: all pass.

#### Step 7: Verify the old implementation is gone

Run:

```bash
rg -n "NormalizationBridge|NormalizationState|one-operator-per-line|normalize_one_operator_per_line" crates/flpdf crates/flpdf-cli
```

Expected: no production or active-test matches. Stale historical design/plan docs outside these paths are not edited.

#### Step 8: Format and commit

```bash
cargo fmt --all
git diff --check
git add crates/flpdf/src/content_stream.rs crates/flpdf/src/lib.rs crates/flpdf/tests/content_stream_tests.rs crates/flpdf-cli/src/main.rs crates/flpdf-cli/tests/cli_optimization_matrix.rs
git commit -m "refactor(content): replace object normalizer"
```

---

### Task 4: Mirror qpdf CLI bad-token warnings and exit status

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs:2974-3302`
- Modify: `crates/flpdf-cli/src/main.rs:4390-4470`
- Modify: `crates/flpdf-cli/src/main.rs:5190-5240`
- Test: `crates/flpdf-cli/tests/cli_tests.rs`

**Interfaces:**
- Consumes: Task 3 crate-root `normalize_content_stream` returning `ContentNormalization`.
- Produces:
  - `apply_normalize_content(..., seen: &mut HashSet<ObjectRef>) -> CliResult<Vec<bool>>`, where each bool is `last_token_was_bad` for one distinct stream with any bad token;
  - `finish_rewrite_warnings(..., normalization_last_bad: &[bool])`;
  - output-preserving qpdf exit 3 behavior.

#### Step 1: Add a test-built page-content fixture

Near `build_classic_pdf` in `cli_tests.rs`, add:

```rust
fn one_page_pdf_with_content(content: &[u8]) -> Vec<u8> {
    let stream = [
        format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len()).into_bytes(),
        content.to_vec(),
        b"\nendstream\nendobj\n".to_vec(),
    ]
    .concat();
    build_classic_pdf(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 4 0 R >>\nendobj\n",
        stream.as_slice(),
    ])
}
```

This helper computes stream length and xref offsets independently of the normalizer under test.

Add a second helper whose two pages share one indirect content stream:

```rust
fn two_page_pdf_with_shared_content(content: &[u8]) -> Vec<u8> {
    let stream = [
        format!("5 0 obj\n<< /Length {} >>\nstream\n", content.len()).into_bytes(),
        content.to_vec(),
        b"\nendstream\nendobj\n".to_vec(),
    ]
    .concat();
    let objects: [&[u8]; 5] = [
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>\nendobj\n",
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 5 0 R >>\nendobj\n",
        b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 5 0 R >>\nendobj\n",
        stream.as_slice(),
    ];
    build_classic_pdf(&objects)
}
```

#### Step 2: Add RED tests for recovered and terminal bad-token cases

Add:

```rust
#[test]
fn rewrite_normalize_content_bad_token_writes_output_warns_and_exits_three() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("bad-content.pdf");
    let output = temp.path().join("normalized.pdf");
    std::fs::write(&input, one_page_pdf_with_content(b"\r<0g")).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--normalize-content=y", "--compress-streams=n"])
        .arg(&input)
        .arg(&output)
        .assert()
        .code(3)
        .stderr(predicate::str::contains(format!(
            "WARNING: {}: content normalization encountered bad tokens",
            input.display()
        )))
        .stderr(predicate::str::contains(
            "normalized content ended with a bad token",
        ))
        .stderr(predicate::str::contains(
            "Resulting stream data may be corrupted but is may still useful",
        ))
        .stderr(predicate::str::contains(
            "flpdf: operation succeeded with warnings; resulting file may have some problems",
        ));

    assert!(output.exists(), "qpdf warning exit must retain output");
    let mut pdf = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    let page = flpdf::pages::page_refs(&mut pdf).unwrap()[0];
    assert_eq!(
        flpdf::pages::page_content_bytes(&mut pdf, page).unwrap(),
        b"\n<0g"
    );
}

#[test]
fn rewrite_normalize_content_recovered_bad_token_omits_terminal_warning() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("recovered-content.pdf");
    let output = temp.path().join("normalized.pdf");
    std::fs::write(&input, one_page_pdf_with_content(b"<0g> q")).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--normalize-content=y", "--compress-streams=n"])
        .arg(&input)
        .arg(&output)
        .assert()
        .code(3)
        .stderr(predicate::str::contains(
            "content normalization encountered bad tokens",
        ))
        .stderr(predicate::str::contains("normalized content ended").not())
        .stderr(predicate::str::contains(
            "Resulting stream data may be corrupted but is may still useful",
        ));

    assert!(output.exists());
}

#[test]
fn rewrite_normalize_content_shared_bad_stream_warns_once() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("shared-bad-content.pdf");
    let output = temp.path().join("normalized.pdf");
    std::fs::write(&input, two_page_pdf_with_shared_content(b"<0g")).unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", "--normalize-content=y", "--compress-streams=n"])
        .arg(&input)
        .arg(&output)
        .assert()
        .code(3)
        .stderr(
            predicate::str::contains(
                "content normalization encountered bad tokens",
            )
            .count(1),
        )
        .stderr(
            predicate::str::contains(
                "Resulting stream data may be corrupted but is may still useful",
            )
            .count(1),
        );

    assert!(output.exists());
}
```

These tests catch aborting before output, returning exit 0/1/2, applying the terminal warning to every bad-token stream, or normalizing and warning twice for one stream shared by multiple pages.

#### Step 3: Run and verify RED

Run:

```bash
cargo test -p flpdf-cli --test cli_tests rewrite_normalize_content_bad_token -- --nocapture
cargo test -p flpdf-cli --test cli_tests rewrite_normalize_content_recovered_bad_token_omits_terminal_warning -- --exact
cargo test -p flpdf-cli --test cli_tests rewrite_normalize_content_shared_bad_stream_warns_once -- --exact
```

Expected: all fail because the current object normalizer treats malformed
content as a fatal rewrite error (normally exit 2), does not retain the
requested output, and emits none of qpdf's normalization-warning sequence.

#### Step 4: Return bad-token status while storing normalized bytes

Change signatures:

```rust
fn apply_normalize_content<R: std::io::Read + std::io::Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    seen: &mut HashSet<ObjectRef>,
) -> CliResult<Vec<bool>>
```

and:

```rust
fn normalize_and_store_stream<R: std::io::Read + std::io::Seek>(
    pdf: &mut Pdf<R>,
    stream_ref: ObjectRef,
    seen: &mut HashSet<ObjectRef>,
) -> CliResult<Option<bool>>
```

For absent/non-stream/direct stream cases return an empty vector or `None`.
At the start of `normalize_and_store_stream`, mirror qpdf's
`QPDFWriter::Members::normalized_streams` object-generation set:

```rust
if !seen.insert(stream_ref) {
    return Ok(None);
}
```

Pass `seen` through every `/Contents` reference traversal. For a distinct
indirect stream:

```rust
let normalized = normalize_content_stream(&decoded);
let warning = normalized
    .any_bad_tokens()
    .then(|| normalized.last_token_was_bad());
let normalized = normalized.into_bytes();
```

Store the bytes exactly as before, then return `Ok(warning)`. Aggregate
`Some(last_bad)` values across distinct `/Contents` references. This makes a
shared stream produce one qpdf-equivalent normalization pipeline and one
warning sequence.

#### Step 5: Accumulate normalization warning state in `run_rewrite`

Before the content mutation passes:

```rust
let mut normalization_last_bad = Vec::new();
let mut normalized_streams = HashSet::new();
```

In the page loop:

```rust
normalization_last_bad.extend(apply_normalize_content(
    &mut pdf,
    page_ref,
    &mut normalized_streams,
)?);
```

After writing the output, replace the `finish_lazy_warnings` call with:

```rust
finish_rewrite_warnings(
    &input,
    &pdf,
    diagnostics_start,
    &normalization_last_bad,
)?;
```

#### Step 6: Implement exact warning payloads and summary selection

Add:

```rust
fn emit_content_normalization_warnings(input: &Path, last_token_was_bad: bool) {
    let location = diagnostic_location(input, None);
    eprintln!(
        "WARNING: {location}: content normalization encountered bad tokens"
    );
    if last_token_was_bad {
        eprintln!(
            "WARNING: {location}: normalized content ended with a bad token; \
             you may be able to resolve this by coalescing content streams in \
             combination with normalizing content. From the command line, \
             specify --coalesce-contents"
        );
    }
    eprintln!(
        "WARNING: {location}: Resulting stream data may be corrupted but is may \
         still useful for manual inspection. For more information on this \
         warning, search for content normalization in the manual."
    );
}

fn finish_rewrite_warnings<R: Read + Seek>(
    input: &Path,
    pdf: &Pdf<R>,
    diagnostics_start: usize,
    normalization_last_bad: &[bool],
) -> CliResult<()> {
    let has_lazy = pdf.repair_diagnostics().entries().len() != diagnostics_start;
    if has_lazy {
        emit_warnings_since(input, pdf, diagnostics_start);
    }
    for &last_bad in normalization_last_bad {
        emit_content_normalization_warnings(input, last_bad);
    }
    if !normalization_last_bad.is_empty() {
        eprintln!(
            "{}: operation succeeded with warnings; resulting file may have some problems",
            progname()
        );
    } else if has_lazy {
        eprintln!("{}: operation succeeded with warnings", progname());
    } else {
        return Ok(());
    }
    Err(Box::new(CliExitError {
        code: ExitCode::Warnings,
        message: String::new(),
    }))
}
```

Keep `finish_lazy_warnings` for other command paths.

#### Step 7: Run focused CLI tests and verify GREEN

Run:

```bash
cargo test -p flpdf-cli --test cli_tests rewrite_normalize_content
cargo test -p flpdf-cli --test cli_check_exitcodes rewrite_repair_warnings_use_qpdf_stderr_format -- --exact
```

Expected: all pass; repair-only warning summary remains unchanged.

#### Step 8: Format and commit

```bash
cargo fmt --all
cargo clippy -p flpdf-cli --all-targets -- -D warnings
git diff --check
git add crates/flpdf-cli/src/main.rs crates/flpdf-cli/tests/cli_tests.rs
git commit -m "fix(cli): mirror qpdf normalization warnings"
```

---

### Task 5: Add pinned qpdf differential and CLI byte-parity gates

**Files:**
- Modify: `tests/oracle/qpdf_tokenizer_probe.cc`
- Modify: `crates/flpdf/src/content_normalizer.rs`
- Modify: `scripts/qpdf-tokenizer-diff.sh`
- Modify: `scripts/tests/qpdf-tokenizer-diff-contract.sh`
- Modify: `crates/flpdf-cli/tests/cli_optimization_matrix.rs`

**Interfaces:**
- Consumes: Tasks 2-4 final library and CLI behavior.
- Produces:
  - qpdf probe mode `normalize`;
  - ignored test `content_normalizer::tests::qpdf_content_normalizer_differential`;
  - exact qpdf/flpdf decoded-content CLI parity test.

#### Step 1: Add the C++ oracle mode

Add includes:

```cpp
#include <qpdf/ContentNormalizer.hh>
#include <qpdf/Pl_Buffer.hh>
#include <qpdf/Pl_QPDFTokenizer.hh>
```

Add `normalize` to the usage mode list and accepted `parse_options` modes,
then add:

```cpp
void
dump_normalize(Options const& options)
{
    Pl_Buffer output("content normalizer output");
    ContentNormalizer normalizer;
    Pl_QPDFTokenizer tokenizer("content normalizer", &normalizer, &output);
    tokenizer.write(
        reinterpret_cast<unsigned char const*>(options.input.data()),
        options.input.size());
    tokenizer.finish();
    std::cout << "output\t" << hex_encode(output.getString()) << '\n'
              << "any_bad_tokens\t" << static_cast<int>(normalizer.anyBadTokens()) << '\n'
              << "last_token_was_bad\t"
              << static_cast<int>(normalizer.lastTokenWasBad()) << '\n';
}
```

Dispatch `options.mode == "normalize"` before the final `dump_between` arm.

#### Step 2: Add the Rust differential matrix before changing the build script

In `content_normalizer.rs` tests, add literal cases:

```rust
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

fn normalizer_oracle_cases() -> [(&'static str, &'static [u8]); 12] {
    [
        ("layout-comments-crlf", b"% keep\r\nBT  /N#61me Q"),
        ("string-control", b"(\x01) Tj"),
        ("string-newline", b"(a\rb) Tj"),
        ("iso-latin-literal", b"<a061626364> Tj"),
        ("iso-latin-hex", b"<a0616263> Tj"),
        ("bad-recovers", b"<0g> q"),
        ("bad-at-eof", b"<0g"),
        ("id-at-eof", b"ID"),
        ("id-crlf-separator", b"BI ID\r\nraw EI Q"),
        ("inline-false-ei", b"BI /W 1 ID one EI A1 two EI Q"),
        ("inline-binary", b"BI /W 1 ID \0\xff EI Q"),
        ("all-space", b"q \t\0\x0c\r\r\n\nQ"),
    ]
}
```

Add a renderer that returns the same three-line record as the C++ mode:

```rust
fn hex_encode(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(encoded, "{byte:02x}").unwrap();
    }
    encoded
}

fn normalizer_record(input: &[u8]) -> String {
    let result = normalize_content_stream(input);
    format!(
        "output\t{}\nany_bad_tokens\t{}\nlast_token_was_bad\t{}\n",
        hex_encode(result.as_bytes()),
        u8::from(result.any_bad_tokens()),
        u8::from(result.last_token_was_bad()),
    )
}
```

Add a `Command` runner that passes the existing required options:

```rust
fn run_normalizer_probe(probe: &Path, name: &str, input: &[u8]) -> String {
    let output = Command::new(probe)
        .args([
            "--mode",
            "normalize",
            "--input-hex",
            &hex_encode(input),
            "--allow-eof",
            "1",
            "--include-ignorable",
            "1",
            "--allow-bad",
            "1",
            "--max-len",
            "0",
            "--inline-offset",
            "none",
        ])
        .output()
        // cov:ignore-start: the script supplies a verified executable; this is failure-only harness diagnostics
        .unwrap_or_else(|error| {
            panic!(
                "failed to execute qpdf content normalizer probe {} for {name}: {error}",
                probe.display()
            )
        });
    // cov:ignore-end
    assert!(
        output.status.success(),
        "qpdf content normalizer probe failed for {name} ({}):\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr), // cov:ignore: failure-only assert diagnostic
    );
    String::from_utf8(output.stdout).expect("probe records are ASCII")
}
```

Then add:

```rust
#[test]
#[ignore = "live qpdf 11.9.0 content normalizer oracle"]
// cov:ignore-start: ignored live entry point; ordinary tests cover every authored case locally
fn qpdf_content_normalizer_differential() {
    let probe = std::env::var_os("QPDF_TOKENIZER_PROBE")
        .expect("set QPDF_TOKENIZER_PROBE to the built qpdf 11.9.0 probe");
    for (name, input) in normalizer_oracle_cases() {
        assert_eq!(
            normalizer_record(input),
            run_normalizer_probe(std::path::Path::new(&probe), name, input),
            "case {name}"
        );
    }
}
// cov:ignore-end
```

Before production changes in this task, run the ignored test with any existing tokenizer probe and verify RED: it must fail because `normalize` is not yet a supported probe mode.

#### Step 3: Extend the secure probe build

In the `c++` command in `scripts/qpdf-tokenizer-diff.sh`, add:

```bash
-I"${qpdf_source}/libqpdf" \
"${qpdf_source}/libqpdf/ContentNormalizer.cc" \
```

The local compilation of `ContentNormalizer.cc` is required because its private class symbols are not an installed/exported libqpdf API.

After the existing exact tokenizer test, add:

```bash
LD_LIBRARY_PATH="${probe_library_path}" \
  QPDF_TOKENIZER_PROBE="${probe_binary}" \
  cargo test -p flpdf --lib \
  content_normalizer::tests::qpdf_content_normalizer_differential \
  -- --ignored --exact
```

#### Step 4: Update and run the driver contract

Change the fake `cargo` command to accept exactly either ignored test path and
reject any other selector. Extend the fake `c++` check to require both:

```text
-I${fixture_source}/libqpdf
${fixture_source}/libqpdf/ContentNormalizer.cc
```

so the contract fails if the private normalizer source or include path falls
out of the build. In the final parallel-driver assertion, expect four cargo
records instead of two because two parallel runs each execute two exact tests.

Run:

```bash
scripts/tests/qpdf-tokenizer-diff-contract.sh
```

Expected: `qpdf-tokenizer-diff contract: PASS`.

#### Step 5: Run the live pinned differential

Run:

```bash
scripts/qpdf-tokenizer-diff.sh
```

Expected:

- existing all-mode tokenizer oracle passes;
- new content-normalizer oracle passes all 12 cases;
- pinned qpdf source remains clean.

#### Step 6: Add a CLI qpdf byte-parity E2E

In `cli_optimization_matrix.rs`, add a local classic-PDF builder that computes
`/Length`, object offsets, xref entries, and `startxref`:

```rust
fn one_page_content_pdf(content: &[u8]) -> Vec<u8> {
    let stream = [
        format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len()).into_bytes(),
        content.to_vec(),
        b"\nendstream\nendobj\n".to_vec(),
    ]
    .concat();
    let objects: [&[u8]; 4] = [
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 4 0 R >>\nendobj\n",
        stream.as_slice(),
    ];

    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for object in objects {
        offsets.push(pdf.len());
        pdf.extend_from_slice(object);
    }
    let xref_start = pdf.len();
    pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n"
        )
        .as_bytes(),
    );
    pdf
}

fn single_page_content(path: &Path) -> Vec<u8> {
    let bytes = std::fs::read(path).unwrap();
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let page = page_refs(&mut pdf).unwrap()[0];
    page_content_bytes(&mut pdf, page).unwrap()
}
```

Add the real CLI comparison:

```rust
#[test]
fn normalize_content_y_matches_qpdf_11_9_decoded_bytes() {
    if skip_if_qpdf_missing() {
        return;
    }
    let version = ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .expect("run qpdf --version");
    assert_eq!(
        String::from_utf8(version.stdout).unwrap(),
        "qpdf version 11.9.0\n"
    );

    let tmp = tempdir().unwrap();
    let input = tmp.path().join("input.pdf");
    let qpdf_output = tmp.path().join("qpdf.pdf");
    let flpdf_output = tmp.path().join("flpdf.pdf");
    std::fs::write(
        &input,
        one_page_content_pdf(
            b"% keep\r\nBT  /N#61me (a\rb) Tj\rBI /W 1 ID raw EI Q",
        ),
    )
    .unwrap();

    let qpdf = ShellCommand::new("qpdf")
        .args([
            "--normalize-content=y",
            "--stream-data=uncompress",
            "--object-streams=disable",
        ])
        .arg(&input)
        .arg(&qpdf_output)
        .output()
        .expect("run qpdf content normalization");
    assert!(
        qpdf.status.success(),
        "qpdf failed:\n{}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    run_rewrite(
        &input,
        &flpdf_output,
        &[
            "--full-rewrite",
            "--normalize-content=y",
            "--compress-streams=n",
        ],
    );

    assert_eq!(
        single_page_content(&flpdf_output),
        single_page_content(&qpdf_output)
    );
}
```

Add malformed-content parity through both real CLIs as well:

```rust
#[test]
fn normalize_content_bad_tokens_match_qpdf_bytes_and_warning_exit() {
    if skip_if_qpdf_missing() {
        return;
    }

    let tmp = tempdir().unwrap();
    let input = tmp.path().join("bad-input.pdf");
    let qpdf_output = tmp.path().join("qpdf-bad.pdf");
    let flpdf_output = tmp.path().join("flpdf-bad.pdf");
    std::fs::write(&input, one_page_content_pdf(b"\r<0g")).unwrap();

    let qpdf = ShellCommand::new("qpdf")
        .args([
            "--normalize-content=y",
            "--stream-data=uncompress",
            "--object-streams=disable",
        ])
        .arg(&input)
        .arg(&qpdf_output)
        .output()
        .expect("run qpdf malformed content normalization");
    assert_eq!(qpdf.status.code(), Some(3));

    let mut flpdf = CargoCommand::cargo_bin("flpdf").unwrap();
    let flpdf = flpdf
        .args([
            "rewrite",
            "--full-rewrite",
            "--normalize-content=y",
            "--compress-streams=n",
        ])
        .arg(&input)
        .arg(&flpdf_output)
        .output()
        .expect("run flpdf malformed content normalization");
    assert_eq!(flpdf.status.code(), Some(3));

    assert_eq!(
        single_page_content(&flpdf_output),
        single_page_content(&qpdf_output)
    );
}
```

Use these literal inputs and real CLI outputs; do not calculate expected bytes
with `normalize_content_stream`. The valid case must exit 0 and the malformed
case must retain both output files while both tools exit 3.

#### Step 7: Remove stale observable-only prose and update existing assertions

Delete the `.12.2` byte-divergence section and change the normalization observability description to exact decoded-content byte parity with qpdf. Keep unrelated compression divergence documentation.

Run:

```bash
cargo test -p flpdf-cli --test cli_optimization_matrix normalize_content -- --nocapture
```

Expected: library-driven and qpdf CLI parity tests pass.

#### Step 8: Format and commit

```bash
cargo fmt --all
git diff --check
git add tests/oracle/qpdf_tokenizer_probe.cc crates/flpdf/src/content_normalizer.rs scripts/qpdf-tokenizer-diff.sh scripts/tests/qpdf-tokenizer-diff-contract.sh crates/flpdf-cli/tests/cli_optimization_matrix.rs
git commit -m "test(content): gate qpdf normalizer parity"
```

---

### Task 6: Update correspondence, run all gates, and publish

**Files:**
- Modify: `docs/qpdf-correspondence.md`
- Regenerate: `docs/qpdf-module-doc-index.md`
- Verify: all files changed since `origin/main`
- Tracker: `flpdf-qxba.7`

**Interfaces:**
- Consumes: Tasks 1-5 complete implementation and tests.
- Produces: mirrored correspondence status, complete verification evidence, closed/pushed Beads state, pushed Git branch.

#### Step 1: Update the correspondence table

Change the `Pl_QPDFTokenizer.cc / ContentNormalizer.cc` row to:

```markdown
| `Pl_QPDFTokenizer.cc` / `ContentNormalizer.cc` | 141 | `content_normalizer.rs`（既存 `tokenizer.rs` を駆動する token-filter runner、EOF-token → `handle_eof`、`ID` separator 注入、inline-image 切替、raw-token normalization、bad-token state、CR/string/name normalization） | ✅ |
```

Ensure `content_stream.rs` is described only as parser callback orchestration.

#### Step 2: Regenerate and verify the module index

Run:

```bash
python3 scripts/qpdf-module-docs.py --write
python3 scripts/qpdf-module-docs.py --check
python3 -m unittest scripts/tests/test_qpdf_module_docs.py
```

Expected: all pass and the index contains `content_normalizer.rs` as a mirror of the two qpdf files.

#### Step 3: Run focused gates

Run:

```bash
cargo fmt --all -- --check
cargo test -p flpdf --lib content_normalizer::tests
cargo test -p flpdf --test content_stream_tests
cargo test -p flpdf-cli --test cli_tests rewrite_normalize_content
cargo test -p flpdf-cli --test cli_optimization_matrix normalize_content
scripts/tests/qpdf-tokenizer-diff-contract.sh
scripts/qpdf-tokenizer-diff.sh
```

Expected: all pass.

#### Step 4: Run crate, workspace, documentation, and lint gates

Run:

```bash
cargo test -p flpdf
cargo test -p flpdf-cli
cargo test
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: all pass with no warnings.

#### Step 5: Commit documentation and any gate-only corrections

```bash
git diff --check
git add docs/qpdf-correspondence.md docs/qpdf-module-doc-index.md
git commit -m "docs(content): mark qpdf normalizer mirrored"
```

If a gate required a source/test correction, commit that correction separately with its affected tests before this documentation commit; do not fold unreviewed code into the docs commit.

#### Step 6: Run fresh 100% patch coverage on the committed tree

Run:

```bash
scripts/patch-coverage.sh --base origin/main
```

Expected: 100% of changed executable lines under `crates/flpdf/src` and no uncovered gated lines. Do not reuse an older lcov report.

#### Step 7: Audit final scope and repository state

Run:

```bash
rg -n "NormalizationBridge|NormalizationState" crates/flpdf crates/flpdf-cli
rg -n "ContentNormalizer|Pl_QPDFTokenizer" crates/flpdf/src docs/qpdf-correspondence.md docs/qpdf-module-doc-index.md
git diff --stat origin/main...HEAD
git diff --check origin/main...HEAD
git status --short --branch
```

Expected:

- no old bridge/state matches;
- new component and docs matches are present;
- only `flpdf-qxba.7` files are changed;
- worktree is clean.

#### Step 8: Close and persist Beads

```bash
bd close flpdf-qxba.7 --reason="Ported qpdf 11.9.0 Pl_QPDFTokenizer and ContentNormalizer with exact differential, CLI warning, and 100% patch-coverage gates."
bd dolt push
```

Expected: issue is closed and `Push complete.`

#### Step 9: Rebase, rerun the fast post-rebase gate, and push Git

State before push: this publishes the completed feature branch; no destructive command is involved.

```bash
git pull --rebase origin main
cargo fmt --all -- --check
cargo test -p flpdf --test content_stream_tests
cargo test -p flpdf-cli --test cli_optimization_matrix normalize_content
git push -u origin feature/flpdf-qxba-7-content-normalizer
```

Expected: rebase succeeds, focused tests remain green, and the remote branch is updated successfully.

#### Step 10: Record final evidence

Run:

```bash
git status --short --branch
git log --oneline origin/main..HEAD
bd show flpdf-qxba.7
```

Report the pushed branch, commits, exact test/coverage results, Beads closure/push, and any intentionally deferred non-scope items. Do not claim PR creation unless a PR was separately requested and confirmed.
