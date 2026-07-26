//! Mirrors qpdf 11.9.0 libqpdf/QPDFTokenizer.cc.

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
    pub(crate) error_message: Option<Vec<u8>>,
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
    #[allow(dead_code)] // Synthetic owned tokens remain part of the Task 1 contract.
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

#[allow(dead_code)] // Used by synthetic owned name tokens.
fn canonical_name_raw(value: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity(value.len());
    raw.push(b'/');
    write_name_escaped(&mut raw, value.strip_prefix(b"/").unwrap_or(value));
    raw
}

#[allow(dead_code)] // Used by synthetic owned string tokens.
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
            State::Top => self.in_top(byte), // cov:ignore: legacy qpdf state is never assigned
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
    pub(crate) fn position(&self) -> usize {
        self.pos
    }

    #[allow(dead_code)] // Task 6 routes content-stream inline images through this tokenizer API.
    pub(crate) fn expect_inline_image(&mut self) -> std::result::Result<(), TokenizerStateError> {
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

    #[allow(dead_code)] // Called by expect_inline_image before Task 6 adds its production caller.
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

    pub(crate) fn set_position(&mut self, position: usize) -> Result<()> {
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
    pub(crate) fn consume_one_byte(&mut self) -> Result<()> {
        if self.pos >= self.input.len() {
            return Err(Error::parse(self.pos, "missing separator after ID"));
        }
        self.pos += 1;
        self.reset();
        Ok(())
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

#[allow(dead_code)] // Called by find_ei before Task 6 adds the production entry path.
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

#[allow(dead_code)] // Called by find_ei before Task 6 adds the production entry path.
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

#[allow(dead_code)] // Used by push handlers before Task 3 routes production callers.
fn is_token_delimiter(byte: u8) -> bool {
    is_ws(byte) || is_delimiter(byte)
}

pub(crate) fn starts_number_token(input: &[u8]) -> bool {
    let mut tokenizer = Tokenizer::new(input);
    tokenizer
        .read_token(true, 0)
        .is_ok_and(|token| matches!(token.token_type, TokenType::Integer | TokenType::Real))
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
    use std::fmt::Write as _;
    use std::path::Path;
    use std::process::Command;
    #[cfg(unix)]
    use std::{fs, os::unix::fs::PermissionsExt};

    use super::{PushedToken, Token, TokenType, Tokenizer, TokenizerStateError};

    type ValueCase<'a> = (&'a [u8], TokenType, &'a [u8], Option<&'a [u8]>);
    type RawValueCase<'a> = (&'a [u8], TokenType, &'a [u8], &'a [u8], Option<&'a [u8]>);
    type MessageCase<'a> = (&'a [u8], TokenType, Option<&'a [u8]>);

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

    fn first_pulled(input: &[u8]) -> Token {
        Tokenizer::new(input).read_token(true, 0).unwrap()
    }

    fn inline_image_token(input_after_id_separator: &[u8]) -> Token {
        let mut tokenizer = Tokenizer::new(input_after_id_separator);
        tokenizer.allow_eof();
        tokenizer.expect_inline_image().unwrap();
        tokenizer.read_token(true, 0).unwrap()
    }

    #[derive(Clone, Copy)]
    enum OracleMode {
        Pull,
        Push,
        PullInline,
        PushInline,
        Between,
    }

    impl OracleMode {
        fn as_arg(self) -> &'static str {
            match self {
                Self::Pull => "pull",
                Self::Push => "push",
                Self::PullInline => "pull-inline",
                Self::PushInline => "push-inline",
                Self::Between => "between",
            }
        }
    }

    struct OracleCase {
        name: &'static str,
        mode: OracleMode,
        input: &'static [u8],
        allow_eof: bool,
        include_ignorable: bool,
        max_len: usize,
        inline_offset: Option<usize>,
    }

    fn qpdf_oracle_cases() -> Vec<OracleCase> {
        let all_types = b"%c\r\n \t[]{}<<>> 12 -0.5 /A#2fB (x\\n) <414> null true false word ) >x";
        vec![
            OracleCase {
                name: "pull-all-types-ignorable",
                mode: OracleMode::Pull,
                input: all_types,
                allow_eof: true,
                include_ignorable: true,
                max_len: 0,
                inline_offset: None,
            },
            OracleCase {
                name: "push-all-types-ignorable",
                mode: OracleMode::Push,
                input: all_types,
                allow_eof: true,
                include_ignorable: true,
                max_len: 0,
                inline_offset: None,
            },
            OracleCase {
                name: "push-between-and-unread",
                mode: OracleMode::Between,
                input: b" %c\r\n 1 /Name ",
                allow_eof: true,
                include_ignorable: false,
                max_len: 0,
                inline_offset: None,
            },
            OracleCase {
                name: "push-default-eof",
                mode: OracleMode::Push,
                input: b"",
                allow_eof: false,
                include_ignorable: false,
                max_len: 0,
                inline_offset: None,
            },
            OracleCase {
                name: "pull-default-eof",
                mode: OracleMode::Pull,
                input: b"",
                allow_eof: false,
                include_ignorable: false,
                max_len: 0,
                inline_offset: None,
            },
            OracleCase {
                name: "pull-max-length",
                mode: OracleMode::Pull,
                input: b"abcdefgh ",
                allow_eof: true,
                include_ignorable: false,
                max_len: 5,
                inline_offset: None,
            },
            OracleCase {
                name: "pull-raw-value-and-recovery",
                mode: OracleMode::Pull,
                input: b"/A#20B /a#1x /a#00b (a\\r\\101) <010 2> <0g",
                allow_eof: true,
                include_ignorable: false,
                max_len: 0,
                inline_offset: None,
            },
            OracleCase {
                name: "pull-leading-ignorable-offset",
                mode: OracleMode::Pull,
                input: b" \n% c\r\n/Name ",
                allow_eof: true,
                include_ignorable: false,
                max_len: 0,
                inline_offset: None,
            },
            OracleCase {
                name: "pull-non-utf8-error-byte",
                mode: OracleMode::Pull,
                input: b"<\x80",
                allow_eof: true,
                include_ignorable: false,
                max_len: 0,
                inline_offset: None,
            },
            OracleCase {
                name: "pull-inline-false-ei",
                mode: OracleMode::PullInline,
                input: b"XX abc EI \x01bad EI Q",
                allow_eof: true,
                include_ignorable: false,
                max_len: 0,
                inline_offset: Some(3),
            },
            OracleCase {
                name: "push-inline-false-ei",
                mode: OracleMode::PushInline,
                input: b"XX abc EI \x01bad EI Q",
                allow_eof: true,
                include_ignorable: false,
                max_len: 0,
                inline_offset: Some(3),
            },
            OracleCase {
                name: "pull-inline-preceding-boundary",
                mode: OracleMode::PullInline,
                input: b"XX zaEI Q EI Q",
                allow_eof: true,
                include_ignorable: false,
                max_len: 0,
                inline_offset: Some(3),
            },
            OracleCase {
                name: "pull-inline-rejected-lookahead-cursor",
                mode: OracleMode::PullInline,
                input: b"XX one EI A1EI Q tail",
                allow_eof: true,
                include_ignorable: false,
                max_len: 0,
                inline_offset: Some(3),
            },
            OracleCase {
                name: "pull-inline-unterminated",
                mode: OracleMode::PullInline,
                input: b"XX unterminated",
                allow_eof: true,
                include_ignorable: false,
                max_len: 0,
                inline_offset: Some(3),
            },
        ]
    }

    fn hex_encode(bytes: &[u8]) -> String {
        let mut encoded = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            write!(encoded, "{byte:02x}").unwrap();
        }
        encoded
    }

    fn token_type_name(token_type: TokenType) -> &'static str {
        match token_type {
            TokenType::Bad => "bad",
            TokenType::ArrayClose => "array_close",
            TokenType::ArrayOpen => "array_open",
            TokenType::BraceClose => "brace_close",
            TokenType::BraceOpen => "brace_open",
            TokenType::DictClose => "dict_close",
            TokenType::DictOpen => "dict_open",
            TokenType::Integer => "integer",
            TokenType::Name => "name",
            TokenType::Real => "real",
            TokenType::String => "string",
            TokenType::Null => "null",
            TokenType::Bool => "bool",
            TokenType::Word => "word",
            TokenType::Eof => "eof",
            TokenType::Space => "space",
            TokenType::Comment => "comment",
            TokenType::InlineImage => "inline-image",
        }
    }

    fn append_token_record(
        records: &mut String,
        token: &Token,
        range: Option<(usize, usize)>,
        unread: Option<u8>,
    ) {
        let (start, end) = range
            .map(|(start, end)| (start.to_string(), end.to_string()))
            .unwrap_or_else(|| ("-".into(), "-".into()));
        writeln!(
            records,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            token_type_name(token.token_type),
            hex_encode(&token.value),
            hex_encode(&token.raw),
            token
                .error_message
                .as_deref()
                .map_or_else(String::new, hex_encode),
            start,
            end,
            unread.map_or_else(String::new, |byte| hex_encode(&[byte])),
        )
        .unwrap();
    }

    fn configure_tokenizer(tokenizer: &mut Tokenizer<'_>, case: &OracleCase) {
        if case.allow_eof {
            tokenizer.allow_eof();
        }
        if case.include_ignorable {
            tokenizer.include_ignorable();
        }
    }

    fn dump_flpdf_pull(case: &OracleCase, inline: bool) -> String {
        let mut tokenizer = Tokenizer::new(case.input);
        configure_tokenizer(&mut tokenizer, case);
        if inline {
            let offset = case.inline_offset.expect("inline case offset");
            tokenizer.set_position(offset).unwrap();
            tokenizer.expect_inline_image().unwrap();
        }

        let mut records = String::new();
        for _ in 0..case.input.len() + 32 {
            let token = tokenizer.read_token(true, case.max_len).unwrap();
            let done = token.token_type == TokenType::Eof
                || (!case.allow_eof
                    && token.token_type == TokenType::Bad
                    && token.raw.is_empty()
                    && tokenizer.position() == case.input.len());
            append_token_record(&mut records, &token, Some((token.start, token.end)), None);
            if done {
                return records;
            }
        }
        panic!("pull oracle case did not terminate: {}", case.name); // cov:ignore: bounded harness guard; every finite authored pull case terminates
    }

    fn push_input(case: &OracleCase, inline: bool) -> (Tokenizer<'static>, VecDeque<u8>) {
        if inline {
            let offset = case.inline_offset.expect("inline case offset");
            let input: &'static [u8] = case.input;
            let mut tokenizer = Tokenizer::new(input);
            tokenizer.set_position(offset).unwrap();
            tokenizer.expect_inline_image().unwrap();
            (
                tokenizer,
                input[offset..].iter().copied().collect::<VecDeque<_>>(),
            )
        } else {
            (
                Tokenizer::push(),
                case.input.iter().copied().collect::<VecDeque<_>>(),
            )
        }
    }

    fn dump_flpdf_push(case: &OracleCase, inline: bool) -> String {
        let (mut tokenizer, mut pending) = push_input(case, inline);
        configure_tokenizer(&mut tokenizer, case);
        let mut records = String::new();

        while let Some(byte) = pending.pop_front() {
            tokenizer.present_character(byte).unwrap();
            if let Some(ready) = tokenizer.get_token() {
                if let Some(unread) = ready.unread {
                    pending.push_front(unread);
                }
                append_token_record(&mut records, &ready.token, None, ready.unread);
            }
        }

        for _ in 0..4 {
            tokenizer.present_eof().unwrap();
            let ready = tokenizer.get_token().expect("EOF must finish a token");
            let done = matches!(ready.token.token_type, TokenType::Eof | TokenType::Bad);
            append_token_record(&mut records, &ready.token, None, ready.unread);
            if done {
                return records;
            }
        }
        panic!("push oracle case did not terminate: {}", case.name); // cov:ignore: bounded harness guard; every finite authored push case terminates
    }

    fn dump_flpdf_between(case: &OracleCase) -> String {
        let (mut tokenizer, mut pending) = push_input(case, false);
        configure_tokenizer(&mut tokenizer, case);
        let mut records = String::new();
        let mut event = 0;

        while let Some(byte) = pending.pop_front() {
            let before = tokenizer.between_tokens();
            tokenizer.present_character(byte).unwrap();
            let after_present = tokenizer.between_tokens();
            let ready = tokenizer.get_token();
            let after_get = tokenizer.between_tokens();
            let unread = ready.as_ref().and_then(|token| token.unread);
            if let Some(unread) = unread {
                pending.push_front(unread);
            }
            writeln!(
                records,
                "state\t{event}\t{}\t{}\t{}\t{}\t{}",
                hex_encode(&[byte]),
                u8::from(before),
                u8::from(after_present),
                u8::from(ready.is_some()),
                unread.map_or_else(String::new, |byte| hex_encode(&[byte])),
            )
            .unwrap();
            writeln!(records, "reset\t{event}\t{}", u8::from(after_get)).unwrap();
            event += 1;
        }
        records
    }

    fn dump_flpdf_tokens(case: &OracleCase) -> String {
        match case.mode {
            OracleMode::Pull => dump_flpdf_pull(case, false),
            OracleMode::Push => dump_flpdf_push(case, false),
            OracleMode::PullInline => dump_flpdf_pull(case, true),
            OracleMode::PushInline => dump_flpdf_push(case, true),
            OracleMode::Between => dump_flpdf_between(case),
        }
    }

    fn run_qpdf_probe(probe: &Path, case: &OracleCase) -> String {
        let inline_offset = case
            .inline_offset
            .map_or_else(|| "none".into(), |offset| offset.to_string());
        let output = Command::new(probe)
            .args([
                "--mode",
                case.mode.as_arg(),
                "--input-hex",
                &hex_encode(case.input),
                "--allow-eof",
                if case.allow_eof { "1" } else { "0" },
                "--include-ignorable",
                if case.include_ignorable { "1" } else { "0" },
                "--allow-bad",
                "1",
                "--max-len",
                &case.max_len.to_string(),
                "--inline-offset",
                &inline_offset,
            ])
            .output()
            // cov:ignore-start: script supplies a verified executable; spawn failure is only a harness diagnostic
            .unwrap_or_else(|error| {
                panic!(
                    "failed to execute qpdf tokenizer probe {}: {error}",
                    probe.display()
                )
            });
        // cov:ignore-end
        assert!(
            output.status.success(),
            "qpdf tokenizer probe failed for {} ({}):\n{}",
            case.name,
            output.status,
            String::from_utf8_lossy(&output.stderr), // cov:ignore: failure-only assert diagnostic
        );
        String::from_utf8(output.stdout).expect("probe records are ASCII")
    }

    fn assert_qpdf_oracle_matches(mut qpdf_records: impl FnMut(&OracleCase) -> String) {
        for case in qpdf_oracle_cases() {
            let qpdf = qpdf_records(&case);
            let flpdf = dump_flpdf_tokens(&case);
            assert_eq!(flpdf, qpdf, "case {}", case.name);
        }
    }

    #[test]
    #[ignore = "live qpdf 11.9.0 tokenizer oracle"]
    // cov:ignore-start: ignored live entry point; ordinary tests cover the comparison loop and fake-probe boundary
    fn qpdf_tokenizer_differential_all_modes() {
        let probe = std::env::var_os("QPDF_TOKENIZER_PROBE")
            .expect("set QPDF_TOKENIZER_PROBE to the built qpdf 11.9.0 probe");
        assert_qpdf_oracle_matches(|case| run_qpdf_probe(Path::new(&probe), case));
    }
    // cov:ignore-end

    #[test]
    fn qpdf_oracle_case_dumps_match_record_snapshots() {
        let expected = [
            ("pull-all-types-ignorable", "pull", 825, 0x9311b6348d3ad84a),
            ("push-all-types-ignorable", "push", 814, 0x4a3c11fd1a28cc10),
            (
                "push-between-and-unread",
                "between",
                464,
                0xd36dd04e5bc4426f,
            ),
            ("push-default-eof", "push", 12, 0x0ce86a1b248464d9),
            ("pull-default-eof", "pull", 40, 0x42c8e6e90a40a245),
            ("pull-max-length", "pull", 159, 0xec08cd6ed738226b),
            (
                "pull-raw-value-and-recovery",
                "pull",
                463,
                0xb26af9e055dc7438,
            ),
            (
                "pull-leading-ignorable-offset",
                "pull",
                48,
                0x4d83e56be5eb7476,
            ),
            ("pull-non-utf8-error-byte", "pull", 100, 0x948da051dc166688),
            (
                "pull-inline-false-ei",
                "pull-inline",
                126,
                0x11d2fe8257de43e8,
            ),
            (
                "push-inline-false-ei",
                "push-inline",
                121,
                0x48e0db9dda93da90,
            ),
            (
                "pull-inline-preceding-boundary",
                "pull-inline",
                123,
                0x5fa6af99646f0f7a,
            ),
            (
                "pull-inline-rejected-lookahead-cursor",
                "pull-inline",
                153,
                0x50ea4d1cb1687be4,
            ),
            (
                "pull-inline-unterminated",
                "pull-inline",
                121,
                0x808e0936d25566d2,
            ),
        ];
        let cases = qpdf_oracle_cases();
        assert_eq!(cases.len(), expected.len());
        for (case, (name, mode, expected_len, expected_fingerprint)) in
            cases.into_iter().zip(expected)
        {
            let records = dump_flpdf_tokens(&case);
            let fingerprint = records.bytes().fold(0xcbf29ce484222325_u64, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
            });
            assert_eq!(case.name, name);
            assert_eq!(case.mode.as_arg(), mode, "case {name}");
            assert_eq!(records.len(), expected_len, "case {name}");
            assert_eq!(fingerprint, expected_fingerprint, "case {name}");
        }
        assert_qpdf_oracle_matches(dump_flpdf_tokens);
    }

    #[cfg(unix)]
    fn write_test_probe(path: &Path, source: &str) {
        fs::write(path, source).unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(path, permissions).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn qpdf_probe_command_passes_exact_case_arguments_and_returns_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("probe");
        write_test_probe(&probe, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n");
        let cases = qpdf_oracle_cases();

        assert_eq!(
            run_qpdf_probe(&probe, &cases[5]),
            "--mode\npull\n--input-hex\n616263646566676820\n--allow-eof\n1\n\
             --include-ignorable\n0\n--allow-bad\n1\n--max-len\n5\n--inline-offset\nnone\n"
        );
        assert_eq!(
            run_qpdf_probe(&probe, &cases[9]),
            "--mode\npull-inline\n--input-hex\n58582061626320454920016261642045492051\n\
             --allow-eof\n1\n--include-ignorable\n0\n--allow-bad\n1\n--max-len\n0\n\
             --inline-offset\n3\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn qpdf_probe_failure_reports_case_status_and_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("probe");
        write_test_probe(&probe, "#!/bin/sh\nprintf 'probe stderr' >&2\nexit 7\n");
        let case = &qpdf_oracle_cases()[5];

        let panic = std::panic::catch_unwind(|| run_qpdf_probe(&probe, case)).unwrap_err();
        let message = panic.downcast_ref::<String>().unwrap();
        assert!(message.contains("qpdf tokenizer probe failed for pull-max-length"));
        assert!(message.contains("exit status: 7"));
        assert!(message.contains("probe stderr"));
    }

    #[cfg(unix)]
    #[test]
    fn qpdf_probe_rejects_non_utf8_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("probe");
        write_test_probe(&probe, "#!/bin/sh\nprintf '\\377'\n");
        let case = &qpdf_oracle_cases()[5];

        let panic = std::panic::catch_unwind(|| run_qpdf_probe(&probe, case)).unwrap_err();
        let message = panic.downcast_ref::<String>().unwrap();
        assert!(message.contains("probe records are ASCII"));
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
    fn inline_image_requires_only_a_following_word_boundary_like_qpdf() {
        let token = inline_image_token(b"zaEI aEIx b EI Q");
        assert_eq!(token.token_type, TokenType::InlineImage);
        assert_eq!(token.value, b"za");
    }

    #[test]
    fn inline_image_resumes_search_after_rejected_lookahead_token_like_qpdf() {
        let token = inline_image_token(b"one EI A1EI Q tail");
        assert_eq!(token.token_type, TokenType::InlineImage);
        assert_eq!(token.value, b"one ");
    }

    #[test]
    fn inline_image_without_ei_returns_qpdf_bad_eof_token() {
        let token = inline_image_token(b"unterminated");
        assert_eq!(token.token_type, TokenType::Bad);
        assert_eq!(
            token.error_message.as_deref(),
            Some(b"EOF while reading token".as_slice())
        );
    }

    #[test]
    fn inline_image_rejects_non_printable_word_bytes() {
        let token = inline_image_token(b"one EI \x01 two EI Q");
        assert_eq!(token.token_type, TokenType::InlineImage);
        assert_eq!(token.value, b"one EI \x01 two ");
    }

    #[test]
    fn inline_image_treats_high_bytes_as_signed_non_printable() {
        let token = inline_image_token(b"one EI \x80 two EI Q");
        assert_eq!(token.token_type, TokenType::InlineImage);
        assert_eq!(token.value, b"one EI \x80 two ");
    }

    #[test]
    fn inline_image_rejects_mixed_alphabetic_and_other_word_bytes() {
        let token = inline_image_token(b"one EI A1 two EI Q");
        assert_eq!(token.token_type, TokenType::InlineImage);
        assert_eq!(token.value, b"one EI A1 two ");
    }

    #[test]
    fn inline_image_treats_star_as_alphabetic() {
        let token = inline_image_token(b"one EI f* Q two EI Q");
        assert_eq!(token.token_type, TokenType::InlineImage);
        assert_eq!(token.value, b"one ");
    }

    #[test]
    fn inline_image_accepts_candidate_at_eof() {
        let token = inline_image_token(b"payload EI");
        assert_eq!(token.token_type, TokenType::InlineImage);
        assert_eq!(token.value, b"payload ");
    }

    #[test]
    fn inline_image_skips_more_than_one_rejected_candidate() {
        let token = inline_image_token(b"one EI A1 two EI \x01 three EI Q");
        assert_eq!(token.token_type, TokenType::InlineImage);
        assert_eq!(token.value, b"one EI A1 two EI \x01 three ");
    }

    #[test]
    fn inline_image_falls_back_to_last_rejected_candidate() {
        let token = inline_image_token(b"one EI A1 two EI \x01 tail");
        assert_eq!(token.token_type, TokenType::InlineImage);
        assert_eq!(token.value, b"one EI A1 two ");
    }

    #[test]
    fn inline_image_lookahead_stops_after_ten_good_tokens() {
        let token = inline_image_token(b"one EI q 1 0 0 1 0 0 cm Q q A1 two EI Q");
        assert_eq!(token.token_type, TokenType::InlineImage);
        assert_eq!(token.value, b"one ");
    }

    #[test]
    fn inline_image_lookahead_rejects_bad_tokens() {
        let token = inline_image_token(b"one EI ) two EI Q");
        assert_eq!(token.token_type, TokenType::InlineImage);
        assert_eq!(token.value, b"one EI ) two ");
    }

    #[test]
    fn inline_image_discovery_restores_cursor_and_token_offsets() {
        let mut tokenizer = Tokenizer::new(b"XX payload EI Q");
        tokenizer.set_position(3).unwrap();

        tokenizer.expect_inline_image().unwrap();
        assert_eq!(tokenizer.position(), 3);

        let image = tokenizer.read_token(true, 0).unwrap();
        assert_eq!(image.token_type, TokenType::InlineImage);
        assert_eq!(image.value, b"payload ");
        assert_eq!((image.start, image.end), (3, 11));
        assert_eq!(tokenizer.position(), 11);

        let end = tokenizer.read_token(true, 0).unwrap();
        assert_eq!(end.token_type, TokenType::Word);
        assert_eq!(end.value, b"EI");
        assert_eq!((end.start, end.end), (11, 13));
    }

    #[test]
    fn inline_image_expectation_rejects_improper_state() {
        let mut tokenizer = Tokenizer::new(b"(payload) EI");
        tokenizer.present_character(b'(').unwrap();

        assert_eq!(
            tokenizer.expect_inline_image(),
            Err(TokenizerStateError::ImproperInlineImageState)
        );
    }

    #[test]
    fn inline_image_expectation_discards_a_waiting_token() {
        let mut tokenizer = Tokenizer::new(b"payload EI Q");
        tokenizer.present_character(b'[').unwrap();

        tokenizer.expect_inline_image().unwrap();
        let token = tokenizer.read_token(true, 0).unwrap();
        assert_eq!(token.token_type, TokenType::InlineImage);
        assert_eq!(token.value, b"payload ");
    }

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
            pulled.push((
                token.token_type,
                token.value,
                token.raw,
                token.error_message,
            ));
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
                (
                    token.token_type,
                    token.value,
                    token.raw,
                    token.error_message,
                )
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
            Some(b"exceeded allowable length while reading token".as_slice())
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

    #[test]
    fn pull_position_can_seek_within_input_but_not_past_eof() {
        let mut tokenizer = Tokenizer::new(b"1 2");
        assert_eq!(tokenizer.read_token(false, 0).unwrap().value, b"1");

        tokenizer.set_position(0).unwrap();
        assert_eq!(tokenizer.read_token(false, 0).unwrap().value, b"1");

        let error = tokenizer.set_position(4).unwrap_err();
        assert_eq!(
            error.to_string(),
            "parse error at byte 4: tokenizer position beyond end of input"
        );
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
    fn push_eof_is_always_a_token_while_pull_requires_allow_eof() {
        let mut default_push = Tokenizer::push();
        default_push.present_eof().unwrap();
        let token = default_push.get_token().unwrap().token;
        assert_eq!(token.token_type, TokenType::Eof);
        assert_eq!(token.error_message, None);

        let mut allowed_push = Tokenizer::push();
        allowed_push.allow_eof();
        allowed_push.present_eof().unwrap();
        assert_eq!(
            allowed_push.get_token().unwrap().token.token_type,
            TokenType::Eof
        );

        let mut default_pull = Tokenizer::new(b"");
        let token = default_pull.read_token(true, 0).unwrap();
        assert_eq!(token.token_type, TokenType::Bad);
        assert_eq!(
            token.error_message.as_deref(),
            Some(b"unexpected EOF".as_slice())
        );

        let mut allowed_pull = Tokenizer::new(b"");
        allowed_pull.allow_eof();
        assert_eq!(
            allowed_pull.read_token(true, 0).unwrap().token_type,
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
    fn present_eof_preserves_a_waiting_token() {
        let mut tokenizer = Tokenizer::push();
        tokenizer.present_character(b'[').unwrap();
        tokenizer.present_eof().unwrap();

        assert_eq!(
            tokenizer.get_token().unwrap().token.token_type,
            TokenType::ArrayOpen
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
                Some(b"name with stray # will not work with PDF >= 1.2"),
            ),
            (
                b"/a#1",
                TokenType::Name,
                b"/a\0\x31",
                Some(b"name with stray # will not work with PDF >= 1.2"),
            ),
            (
                b"/a#1x",
                TokenType::Name,
                b"/a\0\x31x",
                Some(b"name with stray # will not work with PDF >= 1.2"),
            ),
            (
                b"/a#00b",
                TokenType::Bad,
                b"/a#00b",
                Some(b"null character not allowed in name token"),
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
                Some(b"invalid character (g) in hexstring"),
            ),
            (
                b"<g",
                TokenType::Bad,
                b"<g",
                b"<g",
                Some(b"invalid character (g) in hexstring"),
            ),
            (b">x", TokenType::Bad, b">", b">", Some(b"unexpected >")),
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
            (b"1.2x", TokenType::Word),
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
        let cases: &[MessageCase<'_>] = &[
            (b"{", TokenType::BraceOpen, None),
            (b"}", TokenType::BraceClose, None),
            (b")", TokenType::Bad, Some(b"unexpected )")),
            (b">", TokenType::Bad, Some(b"EOF while reading token")),
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
                Some(b"EOF while reading token".as_slice()),
                "{input:?}"
            );
        }

        let mut ignorable = Tokenizer::push();
        ignorable.allow_eof();
        ignorable.include_ignorable();
        let tokens = push_all(&mut ignorable, b" \t");
        assert_eq!(tokens[0].token.token_type, TokenType::Space);
        assert_eq!(tokens[0].token.raw, b" \t");

        let mut skipped_space = Tokenizer::push();
        skipped_space.allow_eof();
        skipped_space.present_character(b' ').unwrap();
        skipped_space.present_eof().unwrap();
        assert_eq!(
            skipped_space.get_token().unwrap().token.token_type,
            TokenType::Eof
        );

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
        let token = first_pulled(input);

        assert_eq!(token.token_type, TokenType::String);
        assert_eq!(token.value, b"\x01\x02\x03\x00\x04\x05\x60".to_vec());
        assert_eq!(token.raw, input.to_vec());
        assert_eq!((token.start, token.end), (0, 18));
        assert_eq!(token.error_message, None);
    }

    #[test]
    fn empty_name_is_valid_and_exponent_looking_token_is_a_word() {
        let mut tokenizer = Tokenizer::new(b"/ 1e3");

        let name = tokenizer.read_token(true, 0).unwrap();
        assert_eq!(name.token_type, TokenType::Name);
        assert_eq!(name.value, b"/".to_vec());
        assert_eq!(name.raw, b"/".to_vec());

        let exponent = tokenizer.read_token(true, 0).unwrap();
        assert_eq!(exponent.token_type, TokenType::Word);
        assert_eq!(exponent.value, b"1e3".to_vec());
        assert_eq!(exponent.raw, b"1e3".to_vec());
    }

    #[test]
    fn literal_string_normalizes_cr_and_wraps_octal_to_one_byte() {
        let token = first_pulled(b"(a\rb\r\nc\\777)");

        assert_eq!(token.token_type, TokenType::String);
        assert_eq!(token.value, b"a\nb\nc\xff".to_vec());
        assert_eq!(token.raw, b"(a\rb\r\nc\\777)".to_vec());
        assert_eq!(token.error_message, None);
    }

    #[test]
    fn invalid_and_unterminated_hex_strings_are_bad_tokens() {
        let invalid = first_pulled(b"<0g>");
        assert_eq!(invalid.token_type, TokenType::Bad);
        assert_eq!(invalid.raw, b"<0g".to_vec());
        assert_eq!((invalid.start, invalid.end), (0, 3));
        assert!(invalid.error_message.is_some());

        let unterminated = first_pulled(b"<01");
        assert_eq!(unterminated.token_type, TokenType::Bad);
        assert_eq!(unterminated.raw, b"<01".to_vec());
        assert_eq!((unterminated.start, unterminated.end), (0, 3));
        assert!(unterminated.error_message.is_some());
    }

    #[test]
    fn high_bit_hex_error_preserves_qpdf_message_bytes() {
        let token = first_pulled(b"<\x80");
        assert_eq!(
            token.error_message.as_deref(),
            Some(b"invalid character (\x80) in hexstring".as_slice())
        );
    }

    #[test]
    fn comments_and_pdf_delimiters_follow_normal_pull_mode() {
        let mut tokenizer = Tokenizer::new(b"% comment\r\n[<<{}>>]");
        tokenizer.allow_eof();

        assert_eq!(
            tokenizer.read_token(true, 0).unwrap().token_type,
            TokenType::ArrayOpen
        );
        assert_eq!(
            tokenizer.read_token(true, 0).unwrap().token_type,
            TokenType::DictOpen
        );
        assert_eq!(
            tokenizer.read_token(true, 0).unwrap().token_type,
            TokenType::BraceOpen
        );
        assert_eq!(
            tokenizer.read_token(true, 0).unwrap().token_type,
            TokenType::BraceClose
        );
        assert_eq!(
            tokenizer.read_token(true, 0).unwrap().token_type,
            TokenType::DictClose
        );
        assert_eq!(
            tokenizer.read_token(true, 0).unwrap().token_type,
            TokenType::ArrayClose
        );
        assert_eq!(
            tokenizer.read_token(true, 0).unwrap().token_type,
            TokenType::Eof
        );
    }

    #[test]
    fn unexpected_close_paren_and_comment_at_eof_are_bad_tokens() {
        let unexpected = first_pulled(b")");
        assert_eq!(unexpected.token_type, TokenType::Bad);
        assert_eq!(unexpected.raw, b")");
        assert_eq!(
            unexpected.error_message.as_deref(),
            Some(b"unexpected )".as_slice())
        );

        let comment = first_pulled(b"% no newline");
        assert_eq!(comment.token_type, TokenType::Bad);
        assert!(comment.raw.is_empty());
        assert_eq!(comment.error_message, None);
        assert_eq!((comment.start, comment.end), (12, 12));
    }

    #[test]
    fn unexpected_close_angle_and_literal_escape_edges_are_qpdf_tokens() {
        let unexpected = first_pulled(b">");
        assert_eq!(unexpected.token_type, TokenType::Bad);
        assert_eq!(
            unexpected.error_message.as_deref(),
            Some(b"EOF while reading token".as_slice())
        );

        let literal = first_pulled(b"(a\\\r\nb\\7x\\q)");
        assert_eq!(literal.token_type, TokenType::String);
        assert_eq!(literal.value, b"ab\x07xq".to_vec());

        let trailing_escape = first_pulled(b"(abc\\");
        assert_eq!(trailing_escape.token_type, TokenType::Bad);
        assert_eq!(
            trailing_escape.error_message.as_deref(),
            Some(b"EOF while reading token".as_slice())
        );
    }

    #[test]
    fn name_null_and_stray_hashes_preserve_qpdf_recovery_values() {
        let null = first_pulled(b"/a#00b");
        assert_eq!(null.token_type, TokenType::Bad);
        assert_eq!(null.value, b"/a#00b".to_vec());
        assert_eq!(
            null.error_message.as_deref(),
            Some(b"null character not allowed in name token".as_slice())
        );

        let stray = first_pulled(b"/a#1x");
        assert_eq!(stray.token_type, TokenType::Name);
        assert_eq!(stray.value, b"/a\0\x31x".to_vec());
        assert_eq!(
            stray.error_message.as_deref(),
            Some(b"name with stray # will not work with PDF >= 1.2".as_slice())
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
            let token = first_pulled(input);
            assert_eq!(token.token_type, TokenType::Name);
            assert_eq!(token.value, expected.to_vec());
            assert_eq!(
                token.error_message.as_deref(),
                Some(b"name with stray # will not work with PDF >= 1.2".as_slice())
            );
        }
    }

    #[test]
    fn name_escape_delimiters_are_left_for_the_next_token() {
        let mut first_nibble = Tokenizer::new(b"/a#/tail");
        let name = first_nibble.read_token(true, 0).unwrap();
        assert_eq!(name.token_type, TokenType::Name);
        assert_eq!(name.value, b"/a\0".to_vec());
        assert_eq!(
            first_nibble.read_token(true, 0).unwrap().value,
            b"/tail".to_vec()
        );

        let mut second_nibble = Tokenizer::new(b"/a#1/tail");
        let name = second_nibble.read_token(true, 0).unwrap();
        assert_eq!(name.token_type, TokenType::Name);
        assert_eq!(name.value, b"/a\0\x31".to_vec());
        assert_eq!(
            second_nibble.read_token(true, 0).unwrap().value,
            b"/tail".to_vec()
        );
    }

    #[test]
    fn eof_word_description_is_bounded() {
        let mut tokenizer = Tokenizer::new(b"");
        let error = tokenizer.expect_word(b"obj").unwrap_err();
        assert_eq!(error.to_string(), "parse error at byte 0: unexpected EOF");
    }

    #[test]
    fn skip_ignorable_preserves_qpdf_comment_eof_error_and_offset() {
        let mut tokenizer = Tokenizer::new(b"% unterminated");
        let error = tokenizer.skip_ignorable().unwrap_err();

        assert_eq!(error.to_string(), "parse error at byte 14: bad token");
    }

    #[test]
    fn skip_ignorable_rewinds_probe_before_next_token() {
        let mut tokenizer = Tokenizer::new(b" \n% c\r\n/Name");
        tokenizer.skip_ignorable().unwrap();

        assert_eq!(tokenizer.position(), 7);
        let token = tokenizer.read_token(false, 0).unwrap();
        assert_eq!(token.token_type, TokenType::Name);
        assert_eq!(token.raw, b"/Name");
        assert_eq!((token.start, token.end), (7, 12));
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
