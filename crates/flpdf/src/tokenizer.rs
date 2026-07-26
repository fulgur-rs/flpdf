//! qpdf correspondence: complete mirror of qpdf 11.9.0 QPDFTokenizer.cc across push, pull, normal, and inline-image modes.
//!
//! Mirrors qpdf 11.9.0 `libqpdf/QPDFTokenizer.cc`.

use std::ops::Range;

use crate::{object::write_name_escaped, object::write_string_value, Error, Result};

#[allow(dead_code)] // Space, Comment, and InlineImage are produced by Task 2's state machine.
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
    #[allow(dead_code)] // Retained as part of qpdf's token range contract.
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

fn canonical_name_raw(value: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity(value.len());
    raw.push(b'/');
    write_name_escaped(&mut raw, value.strip_prefix(b"/").unwrap_or(value));
    raw
}

fn canonical_string_raw(value: &[u8]) -> Vec<u8> {
    let mut raw = Vec::new();
    write_string_value(&mut raw, value);
    raw
}

#[allow(dead_code)] // ImproperInlineImageState is produced by Task 4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenizerStateError {
    TokenWaiting,
    ImproperInlineImageState,
}

#[allow(dead_code)] // The push result is consumed by Task 3's pull routing.
pub(crate) struct PushedToken {
    pub(crate) token: Token,
    pub(crate) unread: Option<u8>,
}

#[allow(dead_code)] // InlineImage is entered by Task 4.
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

#[allow(dead_code)] // State fields are consumed by push mode before Task 3 routes production callers.
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

#[allow(dead_code)] // Push mode becomes a production caller in Task 3.
impl Tokenizer<'static> {
    pub(crate) fn push() -> Self {
        Self::new(b"")
    }
}

impl<'a> Tokenizer<'a> {
    pub(crate) fn new(input: &'a [u8]) -> Self {
        Self {
            input,
            pos: 0,
            state: State::BeforeToken,
            allow_eof: false,
            include_ignorable: false,
            token_type: TokenType::Bad,
            value: Vec::new(),
            raw: Vec::new(),
            error_message: None,
            before_token: true,
            in_token: false,
            char_to_unread: None,
            inline_image_bytes: 0,
            bad: false,
            string_depth: 0,
            char_code: 0,
            hex_byte: 0,
            digit_count: 0,
            token_start: 0,
        }
    }
}

#[allow(dead_code)] // Push APIs and handlers become production-used in Task 3.
impl<'a> Tokenizer<'a> {
    pub(crate) fn allow_eof(&mut self) {
        self.allow_eof = true;
    }

    pub(crate) fn include_ignorable(&mut self) {
        self.include_ignorable = true;
    }

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

    pub(crate) fn present_eof(&mut self) -> std::result::Result<(), TokenizerStateError> {
        match self.state {
            State::Name
            | State::NameHex1
            | State::NameHex2
            | State::Number
            | State::Real
            | State::Sign
            | State::Decimal
            | State::Literal => {
                self.present_character(b'\x0c')?;
                self.in_token = true;
            }
            State::Top | State::BeforeToken => {
                self.token_type = TokenType::Eof;
            }
            State::InSpace => {
                self.token_type = if self.include_ignorable {
                    TokenType::Space
                } else {
                    TokenType::Eof
                };
            }
            State::InComment => {
                self.token_type = if self.include_ignorable {
                    TokenType::Comment
                } else {
                    TokenType::Bad
                };
            }
            State::TokenReady => {}
            State::InHexString
            | State::InString
            | State::InHexStringSecond
            | State::StringEscape
            | State::CharCode
            | State::StringAfterCr
            | State::Lt
            | State::Gt
            | State::InlineImage => {
                self.token_type = TokenType::Bad;
                self.error_message = Some("EOF while reading token".into());
            }
        }
        self.state = State::TokenReady;
        if self.token_type == TokenType::Eof && !self.allow_eof {
            self.token_type = TokenType::Bad;
            self.error_message = Some("unexpected EOF".into());
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

    pub(crate) fn between_tokens(&self) -> bool {
        self.before_token
    }

    fn reset(&mut self) {
        self.state = State::BeforeToken;
        self.token_type = TokenType::Bad;
        self.value.clear();
        self.raw.clear();
        self.error_message = None;
        self.before_token = true;
        self.in_token = false;
        self.char_to_unread = None;
        self.inline_image_bytes = 0;
        self.string_depth = 0;
        self.bad = false;
        self.char_code = 0;
        self.hex_byte = 0;
        self.digit_count = 0;
        self.token_start = self.pos;
    }

    fn take_ready_token(&mut self) -> Token {
        let value = if matches!(self.token_type, TokenType::Name | TokenType::String) {
            std::mem::take(&mut self.value)
        } else {
            self.raw.clone()
        };
        let end = self.token_start + self.raw.len();
        Token::from_parts(
            self.token_type,
            value,
            std::mem::take(&mut self.raw),
            self.error_message.take(),
            self.token_start..end,
        )
    }

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

    fn in_before_token(&mut self, byte: u8) {
        if is_ws(byte) {
            self.before_token = !self.include_ignorable;
            self.in_token = self.include_ignorable;
            if self.include_ignorable {
                self.state = State::InSpace;
            }
        } else if byte == b'%' {
            self.before_token = !self.include_ignorable;
            self.in_token = self.include_ignorable;
            self.state = State::InComment;
        } else {
            self.before_token = false;
            self.in_token = true;
            self.in_top(byte);
        }
    }

    fn in_top(&mut self, byte: u8) {
        match byte {
            b'(' => {
                self.string_depth = 1;
                self.state = State::InString;
            }
            b'<' => self.state = State::Lt,
            b'>' => self.state = State::Gt,
            b')' => {
                self.token_type = TokenType::Bad;
                self.error_message = Some("unexpected )".into());
                self.state = State::TokenReady;
            }
            b'[' => {
                self.token_type = TokenType::ArrayOpen;
                self.state = State::TokenReady;
            }
            b']' => {
                self.token_type = TokenType::ArrayClose;
                self.state = State::TokenReady;
            }
            b'{' => {
                self.token_type = TokenType::BraceOpen;
                self.state = State::TokenReady;
            }
            b'}' => {
                self.token_type = TokenType::BraceClose;
                self.state = State::TokenReady;
            }
            b'/' => {
                self.state = State::Name;
                self.value.push(byte);
            }
            b'0'..=b'9' => self.state = State::Number,
            b'+' | b'-' => self.state = State::Sign,
            b'.' => self.state = State::Decimal,
            _ => self.state = State::Literal,
        }
    }

    fn in_space(&mut self, byte: u8) {
        if !is_ws(byte) {
            self.token_type = TokenType::Space;
            self.in_token = false;
            self.char_to_unread = Some(byte);
            self.state = State::TokenReady;
        }
    }

    fn in_comment(&mut self, byte: u8) {
        if matches!(byte, b'\r' | b'\n') {
            if self.include_ignorable {
                self.token_type = TokenType::Comment;
                self.in_token = false;
                self.char_to_unread = Some(byte);
                self.state = State::TokenReady;
            } else {
                self.state = State::BeforeToken;
            }
        }
    }

    fn in_string(&mut self, byte: u8) {
        match byte {
            b'\\' => self.state = State::StringEscape,
            b'(' => {
                self.value.push(byte);
                self.string_depth += 1;
            }
            b')' => {
                self.string_depth -= 1;
                if self.string_depth == 0 {
                    self.token_type = TokenType::String;
                    self.state = State::TokenReady;
                } else {
                    self.value.push(byte);
                }
            }
            b'\r' => {
                self.value.push(b'\n');
                self.state = State::StringAfterCr;
            }
            b'\n' => self.value.push(byte),
            _ => self.value.push(byte),
        }
    }

    fn in_name(&mut self, byte: u8) {
        if is_token_delimiter(byte) {
            self.token_type = if self.bad {
                TokenType::Bad
            } else {
                TokenType::Name
            };
            self.in_token = false;
            self.char_to_unread = Some(byte);
            self.state = State::TokenReady;
        } else if byte == b'#' {
            self.char_code = 0;
            self.state = State::NameHex1;
        } else {
            self.value.push(byte);
        }
    }

    fn in_name_hex1(&mut self, byte: u8) {
        self.hex_byte = byte;
        if let Some(value) = hex_value(byte) {
            self.char_code = u16::from(value) << 4;
            self.state = State::NameHex2;
        } else {
            self.error_message = Some("name with stray # will not work with PDF >= 1.2".into());
            self.value.push(0);
            self.state = State::Name;
            self.in_name(byte);
        }
    }

    fn in_name_hex2(&mut self, byte: u8) {
        if let Some(value) = hex_value(byte) {
            self.char_code |= u16::from(value);
        } else {
            self.error_message = Some("name with stray # will not work with PDF >= 1.2".into());
            self.value.push(0);
            self.value.push(self.hex_byte);
            self.state = State::Name;
            self.in_name(byte);
            return;
        }
        if self.char_code == 0 {
            self.error_message = Some("null character not allowed in name token".into());
            self.value.extend_from_slice(b"#00");
            self.state = State::Name;
            self.bad = true;
        } else {
            self.value.push(self.char_code as u8);
            self.state = State::Name;
        }
    }

    fn in_sign(&mut self, byte: u8) {
        if byte.is_ascii_digit() {
            self.state = State::Number;
        } else if byte == b'.' {
            self.state = State::Decimal;
        } else {
            self.state = State::Literal;
            self.in_literal(byte);
        }
    }

    fn in_decimal(&mut self, byte: u8) {
        if byte.is_ascii_digit() {
            self.state = State::Real;
        } else {
            self.state = State::Literal;
            self.in_literal(byte);
        }
    }

    fn in_number(&mut self, byte: u8) {
        if byte.is_ascii_digit() {
        } else if byte == b'.' {
            self.state = State::Real;
        } else if is_token_delimiter(byte) {
            self.token_type = TokenType::Integer;
            self.state = State::TokenReady;
            self.in_token = false;
            self.char_to_unread = Some(byte);
        } else {
            self.state = State::Literal;
        }
    }

    fn in_real(&mut self, byte: u8) {
        if byte.is_ascii_digit() {
        } else if is_token_delimiter(byte) {
            self.token_type = TokenType::Real;
            self.state = State::TokenReady;
            self.in_token = false;
            self.char_to_unread = Some(byte);
        } else {
            self.state = State::Literal;
        }
    }

    fn in_string_escape(&mut self, byte: u8) {
        self.state = State::InString;
        match byte {
            b'0'..=b'7' => {
                self.state = State::CharCode;
                self.char_code = 0;
                self.digit_count = 0;
                self.in_char_code(byte);
            }
            b'n' => self.value.push(b'\n'),
            b'r' => self.value.push(b'\r'),
            b't' => self.value.push(b'\t'),
            b'b' => self.value.push(b'\x08'),
            b'f' => self.value.push(b'\x0c'),
            b'\n' => {}
            b'\r' => self.state = State::StringAfterCr,
            _ => self.value.push(byte),
        }
    }

    fn in_string_after_cr(&mut self, byte: u8) {
        self.state = State::InString;
        if byte != b'\n' {
            self.in_string(byte);
        }
    }

    fn in_lt(&mut self, byte: u8) {
        if byte == b'<' {
            self.token_type = TokenType::DictOpen;
            self.state = State::TokenReady;
        } else {
            self.state = State::InHexString;
            self.in_hex_string(byte);
        }
    }

    fn in_gt(&mut self, byte: u8) {
        if byte == b'>' {
            self.token_type = TokenType::DictClose;
            self.state = State::TokenReady;
        } else {
            self.token_type = TokenType::Bad;
            self.error_message = Some("unexpected >".into());
            self.in_token = false;
            self.char_to_unread = Some(byte);
            self.state = State::TokenReady;
        }
    }

    fn in_literal(&mut self, byte: u8) {
        if is_token_delimiter(byte) {
            self.in_token = false;
            self.char_to_unread = Some(byte);
            self.state = State::TokenReady;
            self.token_type = match self.raw.as_slice() {
                b"true" | b"false" => TokenType::Bool,
                b"null" => TokenType::Null,
                _ => TokenType::Word,
            };
        }
    }

    fn in_hex_string(&mut self, byte: u8) {
        if let Some(value) = hex_value(byte) {
            self.char_code = u16::from(value) << 4;
            self.state = State::InHexStringSecond;
        } else if byte == b'>' {
            self.token_type = TokenType::String;
            self.state = State::TokenReady;
        } else if is_ws(byte) {
        } else {
            self.token_type = TokenType::Bad;
            self.error_message = Some(format!(
                "invalid character ({}) in hexstring",
                char::from(byte)
            ));
            self.state = State::TokenReady;
        }
    }

    fn in_hex_string_second(&mut self, byte: u8) {
        if let Some(value) = hex_value(byte) {
            self.value.push((self.char_code | u16::from(value)) as u8);
            self.state = State::InHexString;
        } else if byte == b'>' {
            self.value.push(self.char_code as u8);
            self.token_type = TokenType::String;
            self.state = State::TokenReady;
        } else if is_ws(byte) {
        } else {
            self.token_type = TokenType::Bad;
            self.error_message = Some(format!(
                "invalid character ({}) in hexstring",
                char::from(byte)
            ));
            self.state = State::TokenReady;
        }
    }

    fn in_char_code(&mut self, byte: u8) {
        let mut handled = false;
        if matches!(byte, b'0'..=b'7') {
            self.char_code = 8 * self.char_code + u16::from(byte - b'0');
            self.digit_count += 1;
            if self.digit_count < 3 {
                return;
            }
            handled = true;
        }
        self.value.push((self.char_code % 256) as u8);
        self.state = State::InString;
        if !handled {
            self.in_string(byte);
        }
    }

    fn in_inline_image(&mut self, _byte: u8) {
        if self.raw.len() + 1 == self.inline_image_bytes {
            self.token_type = TokenType::InlineImage;
            self.inline_image_bytes = 0;
            self.state = State::TokenReady;
        }
    }
}

impl<'a> Tokenizer<'a> {
    pub(crate) fn position(&self) -> usize {
        self.pos
    }

    pub(crate) fn skip_ignorable(&mut self) -> Result<()> {
        self.skip_ignorable_inner()
            .map_err(|start| Error::parse(start, "EOF while reading token (unterminated comment)"))
    }

    pub(crate) fn next_token(&mut self) -> Token {
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
        if !token.is_integer() {
            return Err(Error::parse(token.start, "expected integer"));
        }
        std::str::from_utf8(&token.value)
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
            .ok_or_else(|| Error::parse(token.start, "integer is out of range"))
    }

    pub(crate) fn expect_word(&mut self, expected: &[u8]) -> Result<()> {
        let token = self.next_token();
        if token.is_word_value(expected) {
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

    fn hex_string(&mut self, start: usize) -> Token {
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

    fn literal_string(&mut self, start: usize) -> Token {
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

    fn name(&mut self, start: usize) -> Token {
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

    fn scalar(&mut self, start: usize) -> Token {
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

    fn borrowed_token(&self, token_type: TokenType, start: usize, end: usize) -> Token {
        let mut token = Token::new(token_type, self.input[start..end].to_vec());
        token.error_offset = start;
        token.start = start;
        token.end = end;
        token
    }

    fn owned_token(
        &self,
        token_type: TokenType,
        value: Vec<u8>,
        start: usize,
        end: usize,
        error_message: Option<String>,
    ) -> Token {
        Token::from_parts(
            token_type,
            value,
            self.input[start..end].to_vec(),
            error_message,
            start..end,
        )
    }

    fn bad_token(&self, start: usize, message: impl Into<String>) -> Token {
        self.bad_token_at(start, start, message)
    }

    fn bad_token_at(&self, start: usize, error_offset: usize, message: impl Into<String>) -> Token {
        Token {
            token_type: TokenType::Bad,
            value: self.input[start..self.pos].to_vec(),
            raw: self.input[start..self.pos].to_vec(),
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
    matches!(
        byte,
        b'\0' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r' | b' '
    )
}

pub(crate) fn is_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

#[allow(dead_code)] // Used by push handlers before Task 3 routes production callers.
fn is_token_delimiter(byte: u8) -> bool {
    is_ws(byte) || is_delimiter(byte)
}

pub(crate) fn starts_number_token(input: &[u8]) -> bool {
    let mut tokenizer = Tokenizer::new(input);
    matches!(
        tokenizer.next_token().token_type,
        TokenType::Integer | TokenType::Real
    )
}

fn token_description(token: &Token) -> String {
    if token.token_type == TokenType::Eof {
        "EOF".into()
    } else {
        format!(
            "{:?} token {:?}",
            token.token_type,
            String::from_utf8_lossy(&token.raw)
        )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::{PushedToken, Token, TokenType, Tokenizer, TokenizerStateError};

    type ValueCase<'a> = (&'a [u8], TokenType, &'a [u8], Option<&'a str>);
    type RawValueCase<'a> = (&'a [u8], TokenType, &'a [u8], &'a [u8], Option<&'a str>);

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
            let done = matches!(ready.token.token_type, TokenType::Eof | TokenType::Bad);
            output.push(ready);
            if done {
                break;
            }
        }
        output
    }

    fn first_pushed(input: &[u8]) -> Token {
        let mut tokenizer = Tokenizer::push();
        tokenizer.allow_eof();
        push_all(&mut tokenizer, input).remove(0).token
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

    #[test]
    fn string_escape_and_octal_states_preserve_qpdf_value_and_raw() {
        let cases: &[(&[u8], &[u8])] = &[
            (br"(\n\r\t\b\f)", b"\n\r\t\x08\x0c"),
            (br"(\101\7x\777)", b"A\x07x\xff"),
            (b"(a\\\r\nb\\qc)", b"abqc"),
            (b"(a(b)c)", b"a(b)c"),
        ];

        for &(input, value) in cases {
            let token = first_pushed(input);
            assert_eq!(token.token_type, TokenType::String, "{input:?}");
            assert_eq!(token.value, value, "{input:?}");
            assert_eq!(token.raw, input, "{input:?}");
        }
    }

    #[test]
    fn name_hex_states_preserve_qpdf_recovery() {
        let cases: &[ValueCase<'_>] = &[
            (b"/A#20B", TokenType::Name, b"/A B", None),
            (
                b"/a#",
                TokenType::Name,
                b"/a\0",
                Some("name with stray # will not work with PDF >= 1.2"),
            ),
            (
                b"/a#1",
                TokenType::Name,
                b"/a\0\x31",
                Some("name with stray # will not work with PDF >= 1.2"),
            ),
            (
                b"/a#1x",
                TokenType::Name,
                b"/a\0\x31x",
                Some("name with stray # will not work with PDF >= 1.2"),
            ),
            (
                b"/a#00b",
                TokenType::Bad,
                b"/a#00b",
                Some("null character not allowed in name token"),
            ),
        ];

        for &(input, token_type, value, message) in cases {
            let token = first_pushed(input);
            assert_eq!(token.token_type, token_type, "{input:?}");
            assert_eq!(token.value, value, "{input:?}");
            assert_eq!(token.raw, input, "{input:?}");
            assert_eq!(token.error_message.as_deref(), message, "{input:?}");
        }
    }

    #[test]
    fn angle_and_hex_states_match_qpdf() {
        let cases: &[RawValueCase<'_>] = &[
            (b"<<", TokenType::DictOpen, b"<<", b"<<", None),
            (b">>", TokenType::DictClose, b">>", b">>", None),
            (b"<010 2>", TokenType::String, b"\x01\x02", b"<010 2>", None),
            (
                b"<0g",
                TokenType::Bad,
                b"<0g",
                b"<0g",
                Some("invalid character (g) in hexstring"),
            ),
            (b">x", TokenType::Bad, b">", b">", Some("unexpected >")),
        ];

        for &(input, token_type, value, raw, message) in cases {
            let token = first_pushed(input);
            assert_eq!(token.token_type, token_type, "{input:?}");
            assert_eq!(token.value, value, "{input:?}");
            assert_eq!(token.raw, raw, "{input:?}");
            assert_eq!(token.error_message.as_deref(), message, "{input:?}");
        }
    }

    #[test]
    fn sign_decimal_number_real_and_literal_states_match_qpdf() {
        let cases: &[(&[u8], TokenType)] = &[
            (b"12", TokenType::Integer),
            (b"+12", TokenType::Integer),
            (b"-0.5", TokenType::Real),
            (b".25", TokenType::Real),
            (b"+", TokenType::Word),
            (b".", TokenType::Word),
            (b"+.", TokenType::Word),
            (b"1e3", TokenType::Word),
            (b"word", TokenType::Word),
            (b"true", TokenType::Bool),
            (b"false", TokenType::Bool),
            (b"null", TokenType::Null),
        ];

        for &(input, token_type) in cases {
            let token = first_pushed(input);
            assert_eq!(token.token_type, token_type, "{input:?}");
            assert_eq!(token.value, input, "{input:?}");
            assert_eq!(token.raw, input, "{input:?}");
        }
    }

    #[test]
    fn braces_and_bad_closing_delimiters_match_qpdf() {
        let cases: &[(&[u8], TokenType, Option<&str>)] = &[
            (b"{", TokenType::BraceOpen, None),
            (b"}", TokenType::BraceClose, None),
            (b")", TokenType::Bad, Some("unexpected )")),
            (b">", TokenType::Bad, Some("EOF while reading token")),
        ];

        for &(input, token_type, message) in cases {
            let token = first_pushed(input);
            assert_eq!(token.token_type, token_type, "{input:?}");
            assert_eq!(token.raw, input, "{input:?}");
            assert_eq!(token.error_message.as_deref(), message, "{input:?}");
        }
    }

    #[test]
    fn eof_state_families_follow_qpdf_completion_table() {
        let appendable: &[(&[u8], TokenType)] = &[
            (b"/name", TokenType::Name),
            (b"/a#", TokenType::Name),
            (b"/a#1", TokenType::Name),
            (b"12", TokenType::Integer),
            (b"1.2", TokenType::Real),
            (b"+", TokenType::Word),
            (b".", TokenType::Word),
            (b"word", TokenType::Word),
        ];
        for &(input, token_type) in appendable {
            let token = first_pushed(input);
            assert_eq!(token.token_type, token_type, "{input:?}");
            assert_eq!(token.raw, input, "{input:?}");
        }

        for &input in &[b"(".as_slice(), b"(\\", b"<", b"<0", b">"] {
            let token = first_pushed(input);
            assert_eq!(token.token_type, TokenType::Bad, "{input:?}");
            assert_eq!(
                token.error_message.as_deref(),
                Some("EOF while reading token"),
                "{input:?}"
            );
        }

        let mut ignorable = Tokenizer::push();
        ignorable.allow_eof();
        ignorable.include_ignorable();
        let tokens = push_all(&mut ignorable, b" \t");
        assert_eq!(tokens[0].token.token_type, TokenType::Space);
        assert_eq!(tokens[0].token.raw, b" \t");

        let mut comment = Tokenizer::push();
        comment.allow_eof();
        comment.include_ignorable();
        let tokens = push_all(&mut comment, b"%comment");
        assert_eq!(tokens[0].token.token_type, TokenType::Comment);
        assert_eq!(tokens[0].token.raw, b"%comment");
    }

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
        let left = Token::from_parts(TokenType::Name, b"/A".to_vec(), b"/A".to_vec(), None, 3..5);
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

    #[test]
    fn hex_string_ignores_pdf_whitespace_and_pads_odd_nibble() {
        let input = b"<010 203\0\x0c0004056>";
        let token = Tokenizer::new(input).next_token();

        assert_eq!(token.token_type, TokenType::String);
        assert_eq!(token.value, b"\x01\x02\x03\x00\x04\x05\x60".to_vec());
        assert_eq!(token.raw, input.to_vec());
        assert_eq!((token.start, token.end), (0, 18));
        assert_eq!(token.error_message, None);
    }

    #[test]
    fn empty_name_is_valid_and_exponent_looking_token_is_a_word() {
        let mut tokenizer = Tokenizer::new(b"/ 1e3");

        let name = tokenizer.next_token();
        assert_eq!(name.token_type, TokenType::Name);
        assert_eq!(name.value, b"/".to_vec());
        assert_eq!(name.raw, b"/".to_vec());

        let exponent = tokenizer.next_token();
        assert_eq!(exponent.token_type, TokenType::Word);
        assert_eq!(exponent.value, b"1e3".to_vec());
        assert_eq!(exponent.raw, b"1e3".to_vec());
    }

    #[test]
    fn literal_string_normalizes_cr_and_wraps_octal_to_one_byte() {
        let token = Tokenizer::new(b"(a\rb\r\nc\\777)").next_token();

        assert_eq!(token.token_type, TokenType::String);
        assert_eq!(token.value, b"a\nb\nc\xff".to_vec());
        assert_eq!(token.raw, b"(a\rb\r\nc\\777)".to_vec());
        assert_eq!(token.error_message, None);
    }

    #[test]
    fn invalid_and_unterminated_hex_strings_are_bad_tokens() {
        let invalid = Tokenizer::new(b"<0g>").next_token();
        assert_eq!(invalid.token_type, TokenType::Bad);
        assert_eq!(invalid.raw, b"<0g".to_vec());
        assert_eq!((invalid.start, invalid.end), (0, 3));
        assert!(invalid.error_message.is_some());

        let unterminated = Tokenizer::new(b"<01").next_token();
        assert_eq!(unterminated.token_type, TokenType::Bad);
        assert_eq!(unterminated.raw, b"<01".to_vec());
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
        assert_eq!(literal.value, b"ab\x07xq".to_vec());

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
        assert_eq!(null.value, b"/a#00b".to_vec());
        assert_eq!(
            null.error_message.as_deref(),
            Some("null character not allowed in name token")
        );

        let stray = Tokenizer::new(b"/a#1x").next_token();
        assert_eq!(stray.token_type, TokenType::Name);
        assert_eq!(stray.value, b"/a\0\x31x".to_vec());
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
            assert_eq!(token.value, expected.to_vec());
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
        assert_eq!(name.value, b"/a\0".to_vec());
        assert_eq!(first_nibble.next_token().value, b"/tail".to_vec());

        let mut second_nibble = Tokenizer::new(b"/a#1/tail");
        let name = second_nibble.next_token();
        assert_eq!(name.token_type, TokenType::Name);
        assert_eq!(name.value, b"/a\0\x31".to_vec());
        assert_eq!(second_nibble.next_token().value, b"/tail".to_vec());
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
