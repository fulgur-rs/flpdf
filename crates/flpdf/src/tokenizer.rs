use std::borrow::Cow;

use crate::{Error, Result};

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
    pub(crate) value: Cow<'a, [u8]>,
    pub(crate) raw: &'a [u8],
    pub(crate) error_message: Option<String>,
    pub(crate) error_offset: usize,
    pub(crate) start: usize,
    pub(crate) end: usize,
}

pub(crate) struct Tokenizer<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Tokenizer<'a> {
    pub(crate) fn new(input: &'a [u8]) -> Self {
        Self { input, pos: 0 }
    }

    pub(crate) fn position(&self) -> usize {
        self.pos
    }

    pub(crate) fn skip_ignorable(&mut self) -> Result<()> {
        self.skip_ignorable_inner()
            .map_err(|start| Error::parse(start, "EOF while reading token (unterminated comment)"))
    }

    pub(crate) fn next_token(&mut self) -> Token<'a> {
        if let Err(start) = self.skip_ignorable_inner() {
            return self.bad_token(start, "EOF while reading token");
        }

        let start = self.pos;
        let Some(byte) = self.take_byte() else {
            return self.borrowed_token(TokenType::Eof, start, start);
        };

        match byte {
            b'[' => self.borrowed_token(TokenType::ArrayOpen, start, self.pos),
            b']' => self.borrowed_token(TokenType::ArrayClose, start, self.pos),
            b'{' => self.borrowed_token(TokenType::BraceOpen, start, self.pos),
            b'}' => self.borrowed_token(TokenType::BraceClose, start, self.pos),
            b'<' if self.peek_byte() == Some(b'<') => {
                self.pos += 1;
                self.borrowed_token(TokenType::DictOpen, start, self.pos)
            }
            b'>' if self.peek_byte() == Some(b'>') => {
                self.pos += 1;
                self.borrowed_token(TokenType::DictClose, start, self.pos)
            }
            b'<' => self.hex_string(start),
            b'>' => self.bad_token(start, "unexpected >"),
            b'(' => self.literal_string(start),
            b')' => self.bad_token(start, "unexpected )"),
            b'/' => self.name(start),
            _ => self.scalar(start),
        }
    }

    pub(crate) fn next_integer(&mut self) -> Result<i64> {
        let token = self.next_token();
        if token.token_type != TokenType::Integer {
            return Err(Error::parse(token.start, "expected integer"));
        }
        std::str::from_utf8(token.value.as_ref())
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
            .ok_or_else(|| Error::parse(token.start, "integer is out of range"))
    }

    pub(crate) fn expect_word(&mut self, expected: &[u8]) -> Result<()> {
        let token = self.next_token();
        if token.token_type == TokenType::Word && token.value.as_ref() == expected {
            Ok(())
        } else {
            Err(Error::parse(
                token.start,
                format!(
                    "expected word {}, found {}",
                    String::from_utf8_lossy(expected),
                    token_description(&token)
                ),
            ))
        }
    }

    fn skip_ignorable_inner(&mut self) -> std::result::Result<(), usize> {
        loop {
            while self.peek_byte().is_some_and(is_ws) {
                self.pos += 1;
            }
            if self.peek_byte() != Some(b'%') {
                return Ok(());
            }

            let comment_start = self.pos;
            while self
                .peek_byte()
                .is_some_and(|byte| byte != b'\r' && byte != b'\n')
            {
                self.pos += 1;
            }
            if self.pos == self.input.len() {
                return Err(comment_start);
            }
        }
    }

    fn hex_string(&mut self, start: usize) -> Token<'a> {
        let mut decoded = Vec::new();
        let mut high_nibble = None;

        while let Some(byte) = self.take_byte() {
            if byte == b'>' {
                if let Some(high) = high_nibble {
                    decoded.push(high << 4);
                }
                return self.owned_token(TokenType::String, decoded, start, self.pos, None);
            }
            if is_ws(byte) {
                continue;
            }
            let Some(nibble) = hex_value(byte) else {
                return self.bad_token_at(start, self.pos - 1, "invalid character in hex string");
            };
            if let Some(high) = high_nibble.take() {
                decoded.push((high << 4) | nibble);
            } else {
                high_nibble = Some(nibble);
            }
        }

        self.bad_token_at(start, self.pos, "EOF while reading token")
    }

    fn literal_string(&mut self, start: usize) -> Token<'a> {
        let mut decoded = Vec::new();
        let mut depth = 1usize;

        while let Some(byte) = self.take_byte() {
            match byte {
                b'(' => {
                    depth += 1;
                    decoded.push(byte);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return self.owned_token(TokenType::String, decoded, start, self.pos, None);
                    }
                    decoded.push(byte);
                }
                b'\\' => {
                    let Some(escaped) = self.take_byte() else {
                        return self.bad_token_at(start, self.pos, "EOF while reading token");
                    };
                    match escaped {
                        b'n' => decoded.push(b'\n'),
                        b'r' => decoded.push(b'\r'),
                        b't' => decoded.push(b'\t'),
                        b'b' => decoded.push(0x08),
                        b'f' => decoded.push(0x0c),
                        b'(' | b')' | b'\\' => decoded.push(escaped),
                        b'\r' => {
                            if self.peek_byte() == Some(b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'\n' => {}
                        b'0'..=b'7' => {
                            let mut value = usize::from(escaped - b'0');
                            for _ in 0..2 {
                                match self.peek_byte() {
                                    Some(next @ b'0'..=b'7') => {
                                        self.pos += 1;
                                        value = (value << 3) | usize::from(next - b'0');
                                    }
                                    _ => break,
                                }
                            }
                            decoded.push((value & 0xff) as u8);
                        }
                        _ => decoded.push(escaped),
                    }
                }
                b'\r' => {
                    if self.peek_byte() == Some(b'\n') {
                        self.pos += 1;
                    }
                    decoded.push(b'\n');
                }
                b'\n' => decoded.push(b'\n'),
                _ => decoded.push(byte),
            }
        }

        self.bad_token_at(start, self.pos, "EOF while reading token")
    }

    fn name(&mut self, start: usize) -> Token<'a> {
        let mut decoded = vec![b'/'];
        let mut error_message = None;
        let mut bad = false;

        while let Some(byte) = self.peek_byte() {
            if is_ws(byte) || is_delimiter(byte) {
                break;
            }
            self.pos += 1;
            if byte != b'#' {
                decoded.push(byte);
                continue;
            }

            loop {
                let Some(first) = self.take_byte() else {
                    error_message.get_or_insert_with(|| {
                        "name with stray # will not work with PDF >= 1.2".into()
                    });
                    decoded.push(0);
                    break;
                };
                let Some(high) = hex_value(first) else {
                    error_message.get_or_insert_with(|| {
                        "name with stray # will not work with PDF >= 1.2".into()
                    });
                    decoded.push(0);
                    if is_ws(first) || is_delimiter(first) {
                        self.pos -= 1;
                    } else if first == b'#' {
                        continue;
                    } else {
                        decoded.push(first);
                    }
                    break;
                };
                let Some(second) = self.take_byte() else {
                    error_message.get_or_insert_with(|| {
                        "name with stray # will not work with PDF >= 1.2".into()
                    });
                    decoded.push(0);
                    decoded.push(first);
                    break;
                };
                let Some(low) = hex_value(second) else {
                    error_message.get_or_insert_with(|| {
                        "name with stray # will not work with PDF >= 1.2".into()
                    });
                    decoded.push(0);
                    decoded.push(first);
                    if is_ws(second) || is_delimiter(second) {
                        self.pos -= 1;
                    } else if second == b'#' {
                        continue;
                    } else {
                        decoded.push(second);
                    }
                    break;
                };
                let value = (high << 4) | low;
                if value == 0 {
                    bad = true;
                    error_message
                        .get_or_insert_with(|| "null character not allowed in name token".into());
                    decoded.extend_from_slice(b"#00");
                } else {
                    decoded.push(value);
                }
                break;
            }
        }

        let token_type = if bad { TokenType::Bad } else { TokenType::Name };
        self.owned_token(token_type, decoded, start, self.pos, error_message)
    }

    fn scalar(&mut self, start: usize) -> Token<'a> {
        while self
            .peek_byte()
            .is_some_and(|byte| !is_ws(byte) && !is_delimiter(byte))
        {
            self.pos += 1;
        }
        let raw = &self.input[start..self.pos];
        let token_type = match raw {
            b"true" | b"false" => TokenType::Bool,
            b"null" => TokenType::Null,
            _ => classify_number(raw),
        };
        self.borrowed_token(token_type, start, self.pos)
    }

    fn take_byte(&mut self) -> Option<u8> {
        let byte = self.peek_byte()?;
        self.pos += 1;
        Some(byte)
    }

    fn peek_byte(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn borrowed_token(&self, token_type: TokenType, start: usize, end: usize) -> Token<'a> {
        Token {
            token_type,
            value: Cow::Borrowed(&self.input[start..end]),
            raw: &self.input[start..end],
            error_message: None,
            error_offset: start,
            start,
            end,
        }
    }

    fn owned_token(
        &self,
        token_type: TokenType,
        value: Vec<u8>,
        start: usize,
        end: usize,
        error_message: Option<String>,
    ) -> Token<'a> {
        Token {
            token_type,
            value: Cow::Owned(value),
            raw: &self.input[start..end],
            error_message,
            error_offset: start,
            start,
            end,
        }
    }

    fn bad_token(&self, start: usize, message: impl Into<String>) -> Token<'a> {
        self.bad_token_at(start, start, message)
    }

    fn bad_token_at(
        &self,
        start: usize,
        error_offset: usize,
        message: impl Into<String>,
    ) -> Token<'a> {
        Token {
            token_type: TokenType::Bad,
            value: Cow::Borrowed(&self.input[start..self.pos]),
            raw: &self.input[start..self.pos],
            error_message: Some(message.into()),
            error_offset,
            start,
            end: self.pos,
        }
    }
}

#[derive(Clone, Copy)]
enum NumberState {
    Integer,
    Real,
    Sign,
    Decimal,
    Word,
}

fn classify_number(raw: &[u8]) -> TokenType {
    let Some((&first, rest)) = raw.split_first() else {
        return TokenType::Word;
    };
    let mut state = match first {
        b'0'..=b'9' => NumberState::Integer,
        b'+' | b'-' => NumberState::Sign,
        b'.' => NumberState::Decimal,
        _ => NumberState::Word,
    };

    for &byte in rest {
        state = match state {
            NumberState::Integer if byte.is_ascii_digit() => NumberState::Integer,
            NumberState::Integer if byte == b'.' => NumberState::Real,
            NumberState::Real if byte.is_ascii_digit() => NumberState::Real,
            NumberState::Sign if byte.is_ascii_digit() => NumberState::Integer,
            NumberState::Sign if byte == b'.' => NumberState::Decimal,
            NumberState::Decimal if byte.is_ascii_digit() => NumberState::Real,
            NumberState::Word => NumberState::Word,
            _ => NumberState::Word,
        };
    }

    match state {
        NumberState::Integer => TokenType::Integer,
        NumberState::Real => TokenType::Real,
        NumberState::Sign | NumberState::Decimal | NumberState::Word => TokenType::Word,
    }
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub(crate) fn is_ws(byte: u8) -> bool {
    matches!(byte, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

pub(crate) fn is_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

pub(crate) fn starts_number_token(input: &[u8]) -> bool {
    let mut tokenizer = Tokenizer::new(input);
    matches!(
        tokenizer.next_token().token_type,
        TokenType::Integer | TokenType::Real
    )
}

fn token_description(token: &Token<'_>) -> String {
    if token.token_type == TokenType::Eof {
        "EOF".into()
    } else {
        format!(
            "{:?} token {:?}",
            token.token_type,
            String::from_utf8_lossy(token.raw)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{TokenType, Tokenizer};

    #[test]
    fn hex_string_ignores_pdf_whitespace_and_pads_odd_nibble() {
        let input = b"<010 203\0\x0c0004056>";
        let token = Tokenizer::new(input).next_token();

        assert_eq!(token.token_type, TokenType::String);
        assert_eq!(token.value.as_ref(), b"\x01\x02\x03\x00\x04\x05\x60");
        assert_eq!(token.raw, input);
        assert_eq!((token.start, token.end), (0, 18));
        assert_eq!(token.error_message, None);
    }

    #[test]
    fn empty_name_is_valid_and_exponent_looking_token_is_a_word() {
        let mut tokenizer = Tokenizer::new(b"/ 1e3");

        let name = tokenizer.next_token();
        assert_eq!(name.token_type, TokenType::Name);
        assert_eq!(name.value.as_ref(), b"/");
        assert_eq!(name.raw, b"/");

        let exponent = tokenizer.next_token();
        assert_eq!(exponent.token_type, TokenType::Word);
        assert_eq!(exponent.value.as_ref(), b"1e3");
        assert_eq!(exponent.raw, b"1e3");
    }

    #[test]
    fn literal_string_normalizes_cr_and_wraps_octal_to_one_byte() {
        let token = Tokenizer::new(b"(a\rb\r\nc\\777)").next_token();

        assert_eq!(token.token_type, TokenType::String);
        assert_eq!(token.value.as_ref(), b"a\nb\nc\xff");
        assert_eq!(token.raw, b"(a\rb\r\nc\\777)");
        assert_eq!(token.error_message, None);
    }

    #[test]
    fn invalid_and_unterminated_hex_strings_are_bad_tokens() {
        let invalid = Tokenizer::new(b"<0g>").next_token();
        assert_eq!(invalid.token_type, TokenType::Bad);
        assert_eq!(invalid.raw, b"<0g");
        assert_eq!((invalid.start, invalid.end), (0, 3));
        assert!(invalid.error_message.is_some());

        let unterminated = Tokenizer::new(b"<01").next_token();
        assert_eq!(unterminated.token_type, TokenType::Bad);
        assert_eq!(unterminated.raw, b"<01");
        assert_eq!((unterminated.start, unterminated.end), (0, 3));
        assert!(unterminated.error_message.is_some());
    }

    #[test]
    fn comments_and_pdf_delimiters_follow_normal_pull_mode() {
        let mut tokenizer = Tokenizer::new(b"% comment\r\n[<<{}>>]");

        assert_eq!(tokenizer.next_token().token_type, TokenType::ArrayOpen);
        assert_eq!(tokenizer.next_token().token_type, TokenType::DictOpen);
        assert_eq!(tokenizer.next_token().token_type, TokenType::BraceOpen);
        assert_eq!(tokenizer.next_token().token_type, TokenType::BraceClose);
        assert_eq!(tokenizer.next_token().token_type, TokenType::DictClose);
        assert_eq!(tokenizer.next_token().token_type, TokenType::ArrayClose);
        assert_eq!(tokenizer.next_token().token_type, TokenType::Eof);
    }

    #[test]
    fn unexpected_close_paren_and_comment_at_eof_are_bad_tokens() {
        let unexpected = Tokenizer::new(b")").next_token();
        assert_eq!(unexpected.token_type, TokenType::Bad);
        assert_eq!(unexpected.raw, b")");
        assert_eq!(unexpected.error_message.as_deref(), Some("unexpected )"));

        let comment = Tokenizer::new(b"% no newline").next_token();
        assert_eq!(comment.token_type, TokenType::Bad);
        assert_eq!(comment.raw, b"% no newline");
        assert_eq!(
            comment.error_message.as_deref(),
            Some("EOF while reading token")
        );
    }

    #[test]
    fn unexpected_close_angle_and_literal_escape_edges_are_qpdf_tokens() {
        let unexpected = Tokenizer::new(b">").next_token();
        assert_eq!(unexpected.token_type, TokenType::Bad);
        assert_eq!(unexpected.error_message.as_deref(), Some("unexpected >"));

        let literal = Tokenizer::new(b"(a\\\r\nb\\7x\\q)").next_token();
        assert_eq!(literal.token_type, TokenType::String);
        assert_eq!(literal.value.as_ref(), b"ab\x07xq");

        let trailing_escape = Tokenizer::new(b"(abc\\").next_token();
        assert_eq!(trailing_escape.token_type, TokenType::Bad);
        assert_eq!(
            trailing_escape.error_message.as_deref(),
            Some("EOF while reading token")
        );
    }

    #[test]
    fn name_null_and_stray_hashes_preserve_qpdf_recovery_values() {
        let null = Tokenizer::new(b"/a#00b").next_token();
        assert_eq!(null.token_type, TokenType::Bad);
        assert_eq!(null.value.as_ref(), b"/a#00b");
        assert_eq!(
            null.error_message.as_deref(),
            Some("null character not allowed in name token")
        );

        let stray = Tokenizer::new(b"/a#1x").next_token();
        assert_eq!(stray.token_type, TokenType::Name);
        assert_eq!(stray.value.as_ref(), b"/a\0\x31x");
        assert_eq!(
            stray.error_message.as_deref(),
            Some("name with stray # will not work with PDF >= 1.2")
        );
    }

    #[test]
    fn incomplete_name_escapes_follow_qpdf_state_recovery() {
        let cases: &[(&[u8], &[u8])] = &[
            (b"/a#", b"/a\0"),
            (b"/a#x", b"/a\0x"),
            (b"/a##x", b"/a\0\0x"),
            (b"/a#1", b"/a\0\x31"),
            (b"/a#1#x", b"/a\0\x31\0x"),
        ];

        for &(input, expected) in cases {
            let token = Tokenizer::new(input).next_token();
            assert_eq!(token.token_type, TokenType::Name);
            assert_eq!(token.value.as_ref(), expected);
            assert_eq!(
                token.error_message.as_deref(),
                Some("name with stray # will not work with PDF >= 1.2")
            );
        }
    }

    #[test]
    fn name_escape_delimiters_are_left_for_the_next_token() {
        let mut first_nibble = Tokenizer::new(b"/a#/tail");
        let name = first_nibble.next_token();
        assert_eq!(name.token_type, TokenType::Name);
        assert_eq!(name.value.as_ref(), b"/a\0");
        assert_eq!(first_nibble.next_token().value.as_ref(), b"/tail");

        let mut second_nibble = Tokenizer::new(b"/a#1/tail");
        let name = second_nibble.next_token();
        assert_eq!(name.token_type, TokenType::Name);
        assert_eq!(name.value.as_ref(), b"/a\0\x31");
        assert_eq!(second_nibble.next_token().value.as_ref(), b"/tail");
    }

    #[test]
    fn empty_number_classification_and_eof_word_description_are_bounded() {
        assert_eq!(super::classify_number(b""), TokenType::Word);

        let mut tokenizer = Tokenizer::new(b"");
        let error = tokenizer.expect_word(b"obj").unwrap_err();
        assert_eq!(
            error.to_string(),
            "parse error at byte 0: expected word obj, found EOF"
        );
    }

    #[test]
    fn integer_helpers_consume_ignorable_input_and_require_exact_types() {
        let mut tokenizer = Tokenizer::new(b"% header\n12 -3 obj");

        assert_eq!(tokenizer.next_integer().unwrap(), 12);
        assert_eq!(tokenizer.next_integer().unwrap(), -3);
        tokenizer.expect_word(b"obj").unwrap();
        assert_eq!(tokenizer.position(), 18);

        let mut tokenizer = Tokenizer::new(b"1.0");
        assert!(tokenizer.next_integer().is_err());

        let mut tokenizer = Tokenizer::new(b"endobj");
        assert!(tokenizer.expect_word(b"obj").is_err());
    }
}
