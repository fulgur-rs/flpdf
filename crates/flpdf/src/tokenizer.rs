//! Mirrors qpdf 11.9.0 libqpdf/QPDFTokenizer.cc.

use std::ops::Range;

use crate::{pdf_syntax::write_name_escaped, Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenType {
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
pub struct Token {
    pub token_type: TokenType,
    pub value: Vec<u8>,
    pub raw: Vec<u8>,
    pub error_message: Option<Vec<u8>>,
    pub(crate) error_offset: usize,
    pub start: usize,
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
    /// Construct an owned token for token-filter and normalization consumers.
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
        error_message: Option<Vec<u8>>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenizerStateError {
    TokenWaiting,
    ImproperInlineImageState,
}

pub(crate) struct PushedToken {
    pub(crate) token: Token,
    pub(crate) unread: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
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

pub struct Tokenizer<'a> {
    input: &'a [u8],
    pos: usize,
    state: State,
    allow_eof: bool,
    include_ignorable: bool,
    token_type: TokenType,
    value: Vec<u8>,
    raw: Vec<u8>,
    error_message: Option<Vec<u8>>,
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

impl Tokenizer<'static> {
    pub(crate) fn push() -> Self {
        Self::new(b"")
    }
}

impl<'a> Tokenizer<'a> {
    pub fn new(input: &'a [u8]) -> Self {
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

impl<'a> Tokenizer<'a> {
    pub fn allow_eof(&mut self) {
        self.allow_eof = true;
    }

    pub fn include_ignorable(&mut self) {
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
            State::BeforeToken => {
                self.token_type = TokenType::Eof;
            }
            State::InSpace => {
                self.token_type = if self.include_ignorable {
                    TokenType::Space
                } else {
                    TokenType::Eof // cov:ignore: InSpace is entered only when ignorable tokens are included
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
            State::TokenReady => unreachable!("checked by present_character"), // cov:ignore: caller rejects a waiting token
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
            self.error_message = Some(invalid_hex_character_message(byte));
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
            self.error_message = Some(invalid_hex_character_message(byte));
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
    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn expect_inline_image(&mut self) -> std::result::Result<(), TokenizerStateError> {
        if self.state == State::TokenReady {
            self.reset();
        } else if self.state != State::BeforeToken {
            return Err(TokenizerStateError::ImproperInlineImageState);
        }

        self.inline_image_bytes = self.find_ei().unwrap_or(0);
        self.before_token = false;
        self.in_token = true;
        self.state = State::InlineImage;
        Ok(())
    }

    fn find_ei(&mut self) -> Option<usize> {
        let initial_pos = self.pos;
        let mut search_pos = initial_pos;
        let mut candidate_distance = None;

        while search_pos < self.input.len() {
            let Some(relative_start) = self.input[search_pos..]
                .windows(2)
                .position(|window| window == b"EI")
            else {
                break;
            };
            let candidate_start = search_pos + relative_start;
            if let Some(after_ei) = word_token_at(self.input, candidate_start, b"EI") {
                candidate_distance = Some(candidate_start - initial_pos);
                let (plausible, next_search_pos) =
                    inline_lookahead_is_plausible(self.input, after_ei);
                if plausible {
                    break;
                }
                // qpdf continues from the cursor advanced by the rejected
                // lookahead, not from immediately after EI. This prevents an
                // EI embedded in the suspicious token from becoming the next
                // candidate (`libqpdf/QPDFTokenizer.cc:799-855`).
                search_pos = next_search_pos;
            } else {
                search_pos = candidate_start + 1;
            }
        }

        self.pos = initial_pos;
        candidate_distance
    }

    /// Pull adapter matching qpdf 11.9.0 `QPDFTokenizer::readToken` and
    /// `nextToken` (`libqpdf/QPDFTokenizer.cc:887-965`).
    pub fn read_token(&mut self, allow_bad: bool, max_len: usize) -> Result<Token> {
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
                    if max_len != 0 && self.raw.len() >= max_len && self.state != State::TokenReady
                    {
                        self.token_type = TokenType::Bad;
                        self.state = State::TokenReady;
                        self.error_message =
                            Some("exceeded allowable length while reading token".into());
                    }
                }
                None => {
                    // cov:ignore-start: defensive propagation; pull EOF cannot fail in a non-ready state
                    self.present_eof().map_err(|error| {
                        Error::parse(
                            self.pos,
                            match error {
                                TokenizerStateError::TokenWaiting => {
                                    "tokenizer already has a token waiting"
                                }
                                TokenizerStateError::ImproperInlineImageState => {
                                    "tokenizer is in an improper inline image state"
                                }
                            },
                        )
                    })?;
                    // cov:ignore-end
                    // `allowEOF` is a pull-only policy in qpdf. Direct push
                    // callers always receive an EOF token
                    // (`libqpdf/QPDFTokenizer.cc:723-762,933-939`).
                    if self.token_type == TokenType::Eof && !self.allow_eof {
                        self.token_type = TokenType::Bad;
                        self.error_message = Some("unexpected EOF".into());
                    }
                }
            }
        }

        if !self.in_token && !self.before_token {
            self.pos = self.pos.saturating_sub(1);
        }
        let token = self.take_ready_token();
        self.reset();
        if token.token_type == TokenType::Bad && !allow_bad {
            return Err(Error::parse(
                token.start,
                token
                    .error_message
                    .as_deref()
                    .map(|message| String::from_utf8_lossy(message).into_owned())
                    .unwrap_or_else(|| "bad token".into()),
            ));
        }
        Ok(token)
    }

    pub fn set_position(&mut self, position: usize) -> Result<()> {
        if position > self.input.len() {
            return Err(Error::parse(
                position,
                "tokenizer position beyond end of input",
            ));
        }
        self.pos = position;
        self.reset();
        Ok(())
    }

    /// Consume exactly one byte at the current cursor.
    ///
    /// qpdf uses this after the `ID` operator to discard the byte that
    /// terminated the token (`libqpdf/QPDFObjectHandle.cc:1820-1825`).
    pub fn consume_one_byte(&mut self) -> Result<()> {
        if self.pos >= self.input.len() {
            return Err(Error::parse(self.pos, "missing separator after ID"));
        }
        self.pos += 1;
        self.reset();
        Ok(())
    }

    pub(crate) fn consume_one_byte_or(&mut self, default: u8) -> u8 {
        let byte = self.input.get(self.pos).copied().unwrap_or(default);
        if self.pos < self.input.len() {
            self.pos += 1;
        }
        self.reset();
        byte
    }

    pub(crate) fn skip_ignorable(&mut self) -> Result<()> {
        let saved_allow_eof = self.allow_eof;
        let saved_include_ignorable = self.include_ignorable;
        self.allow_eof = true;
        self.include_ignorable = false;
        let token = self.read_token(false, 0);
        self.allow_eof = saved_allow_eof;
        self.include_ignorable = saved_include_ignorable;

        let token = token?;
        if token.token_type == TokenType::Eof {
            return Ok(());
        }
        self.set_position(token.start)
    }

    pub(crate) fn next_integer(&mut self) -> Result<i64> {
        let token = self.read_token(false, 0)?;
        if !token.is_integer() {
            return Err(Error::parse(token.start, "expected integer"));
        }
        std::str::from_utf8(&token.value)
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
            .ok_or_else(|| Error::parse(token.start, "integer is out of range"))
    }

    pub(crate) fn expect_word(&mut self, expected: &[u8]) -> Result<()> {
        let token = self.read_token(false, 0)?;
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
}

fn word_token_at(input: &[u8], start: usize, expected: &[u8]) -> Option<usize> {
    let end = start.checked_add(expected.len())?;
    if start == 0
        || input.get(start..end)? != expected
        || input
            .get(end)
            .is_some_and(|&byte| !is_token_delimiter(byte))
    {
        return None;
    }
    // Despite its preceding-delimiter comment, qpdf 11.9.0 checks only that
    // the absolute token start is nonzero and that the following byte is a
    // delimiter (`libqpdf/QPDFTokenizer.cc:45-72`).
    Some(end)
}

fn inline_lookahead_is_plausible(input: &[u8], after_ei: usize) -> (bool, usize) {
    let mut tokenizer = Tokenizer::new(&input[after_ei..]);
    tokenizer.allow_eof();

    for _ in 0..10 {
        let token = tokenizer
            .read_token(true, 0)
            .expect("allow_bad makes tokenizer errors observable as tokens");
        let next_search_pos = after_ei + tokenizer.position();
        if token.token_type == TokenType::Eof {
            return (true, next_search_pos);
        }
        if token.token_type == TokenType::Bad {
            return (false, next_search_pos);
        }
        if token.token_type == TokenType::Word {
            let mut found_alpha = false;
            let mut found_non_printable = false;
            let mut found_other = false;
            for &byte in &token.value {
                if byte.is_ascii_alphabetic() || byte == b'*' {
                    found_alpha = true;
                } else if (byte as i8) < 32 && !is_ws(byte) {
                    found_non_printable = true;
                    break;
                } else {
                    found_other = true;
                }
            }
            if found_non_printable || (found_alpha && found_other) {
                return (false, next_search_pos);
            }
        }
    }
    (true, after_ei + tokenizer.position())
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn invalid_hex_character_message(byte: u8) -> Vec<u8> {
    let mut message = b"invalid character (".to_vec();
    message.push(byte);
    message.extend_from_slice(b") in hexstring");
    message
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

fn is_token_delimiter(byte: u8) -> bool {
    is_ws(byte) || is_delimiter(byte)
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
