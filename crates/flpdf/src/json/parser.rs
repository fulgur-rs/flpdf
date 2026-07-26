use std::io::{Cursor, Read};

use super::{Json, JsonError};

/// Receives qpdf-style events while a JSON value is parsed.
///
/// Task 5 declares the public interface required by `parse_reader`. Event
/// dispatch and container consumption are added by the later Reactor task.
pub trait Reactor {
    fn dictionary_start(&mut self);
    fn array_start(&mut self);
    fn container_end(&mut self, value: &Json);
    fn top_level_scalar(&mut self);
    fn dictionary_item(&mut self, key: &[u8], value: &Json) -> bool;
    fn array_item(&mut self, value: &Json) -> bool;
}

/// Parse a scalar JSON value from bytes.
pub fn parse(input: &[u8]) -> Result<Json, JsonError> {
    let mut cursor = Cursor::new(input);
    parse_reader(&mut cursor, None)
}

/// Parse a scalar JSON value from a reader.
pub fn parse_reader<R: Read>(
    reader: &mut R,
    reactor: Option<&mut dyn Reactor>,
) -> Result<Json, JsonError> {
    // The public Reactor interface is declared in this task to make this API
    // usable. Dispatch and consumption semantics are deliberately deferred to
    // Task 7.
    let _ = reactor;

    let mut input = Vec::new();
    reader.read_to_end(&mut input)?;
    Parser::new(&input).parse()
}

impl Json {
    pub fn parse(input: &[u8]) -> Result<Self, JsonError> {
        parse(input)
    }

    pub fn parse_reader<R: Read>(
        reader: &mut R,
        reactor: Option<&mut dyn Reactor>,
    ) -> Result<Self, JsonError> {
        parse_reader(reader, reactor)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LexState {
    Top,
    NumberMinus,
    NumberLeadingZero,
    NumberBeforePoint,
    NumberPoint,
    NumberAfterPoint,
    NumberE,
    NumberESign,
    Number,
    Alpha,
    String,
    Backslash,
    U4,
    AfterString,
    BeginDictionary,
    EndDictionary,
    BeginArray,
    EndArray,
    Colon,
    Comma,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ParseState {
    Top,
    DictionaryBegin,
    DictionaryAfterKey,
    DictionaryAfterColon,
    DictionaryAfterItem,
    DictionaryAfterComma,
    ArrayBegin,
    ArrayAfterItem,
    ArrayAfterComma,
    Done,
}

struct StackEntry {
    state: ParseState,
    item: Json,
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
    state: LexState,
    done: bool,
    token: Vec<u8>,
    token_start: i64,
    u_count: usize,
    u_value: u32,
    high_surrogate: u32,
    high_offset: Option<i64>,
    parse_state: ParseState,
    stack: Vec<StackEntry>,
    dict_key: Vec<u8>,
    dict_key_offset: i64,
}

impl<'a> Parser<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self {
            input,
            pos: 0,
            state: LexState::Top,
            done: false,
            token: Vec::new(),
            token_start: 0,
            u_count: 0,
            u_value: 0,
            high_surrogate: 0,
            high_offset: None,
            parse_state: ParseState::Top,
            stack: Vec::new(),
            dict_key: Vec::new(),
            dict_key_offset: 0,
        }
    }

    fn parse(mut self) -> Result<Json, JsonError> {
        while !self.done {
            self.get_token()?;
            self.handle_token()?;
        }
        if self.parse_state != ParseState::Done {
            return Err(self.error("JSON: premature end of input"));
        }
        Ok(self
            .stack
            .last()
            .expect("completed JSON parser has a top-level value")
            .item
            .clone())
    }

    fn offset(&self) -> i64 {
        self.pos as i64
    }

    fn current(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn append(&mut self) {
        self.token
            .push(self.current().expect("append requires input"));
        self.pos += 1;
    }

    fn append_state(&mut self, state: LexState) {
        self.state = state;
        self.append();
    }

    fn ignore(&mut self) {
        self.pos += 1;
    }

    fn ignore_state(&mut self, state: LexState) {
        self.state = state;
        self.ignore();
    }

    fn error(&self, message: impl Into<String>) -> JsonError {
        JsonError::Parse(message.into())
    }

    fn token_error(&self) -> Result<(), JsonError> {
        if self.done {
            return Err(self.error("JSON: premature end of input"));
        }

        let byte = self.current().expect("not done has input");
        let offset = self.offset();
        match self.state {
            LexState::U4 => Err(self.error(format!(
                "JSON: offset {}: \\u must be followed by four hex digits",
                offset - self.u_count as i64 - 1
            ))),
            LexState::Alpha => Err(self.error(format!(
                "JSON: offset {offset}: keyword: unexpected character {}",
                printable_byte(byte)
            ))),
            LexState::String => Err(self.error(format!(
                "JSON: offset {offset}: control character in string (missing \"?)"
            ))),
            LexState::Backslash => Err(self.error(format!(
                "JSON: offset {offset}: invalid character after backslash: {}",
                printable_byte(byte)
            ))),
            _ => match byte {
                b'.' => {
                    if matches!(
                        self.state,
                        LexState::Number | LexState::NumberE | LexState::NumberESign
                    ) {
                        Err(self.error(format!(
                            "JSON: offset {offset}: numeric literal: decimal point after e"
                        )))
                    } else {
                        Err(self.error(format!(
                            "JSON: offset {offset}: numeric literal: decimal point already seen"
                        )))
                    }
                }
                b'e' | b'E' => Err(self.error(format!(
                    "JSON: offset {offset}: numeric literal: e already seen"
                ))),
                b'+' | b'-' => Err(self.error(format!(
                    "JSON: offset {offset}: numeric literal: unexpected sign"
                ))),
                b' ' | b'\t' | b'\n' | b'\r' | b'{' | b'}' | b'[' | b']' | b':' | b',' => Err(self
                    .error(format!(
                        "JSON: offset {offset}: numeric literal: incomplete number"
                    ))),
                _ => Err(self.error(format!(
                    "JSON: offset {offset}: numeric literal: unexpected character {}",
                    printable_byte(byte)
                ))),
            },
        }
    }

    fn handle_u_code(&mut self, codepoint: u32, offset: i64) -> Result<(), JsonError> {
        if (codepoint & 0xfc00) == 0xd800 {
            if let Some(high_offset) = self.high_offset {
                return Err(self.error(format!(
                    "JSON: offset {offset}: UTF-16 high surrogate found after previous high surrogate at offset {high_offset}"
                )));
            }
            self.high_offset = Some(offset);
            self.high_surrogate = codepoint;
        } else if (codepoint & 0xfc00) == 0xdc00 {
            // qpdf uses zero as its high-surrogate-offset sentinel. This
            // intentionally accepts a low surrogate beginning at offset 6
            // even when no high surrogate has been seen.
            let high_offset = self.high_offset.unwrap_or(0);
            if offset != high_offset + 6 {
                return Err(self.error(format!(
                    "JSON: offset {offset}: UTF-16 low surrogate found not immediately after high surrogate"
                )));
            }
            self.high_offset = None;
            let codepoint = 0x10000 + ((self.high_surrogate & 0x3ff) << 10) + (codepoint & 0x3ff);
            append_utf8(&mut self.token, codepoint);
        } else {
            append_utf8(&mut self.token, codepoint);
        }
        Ok(())
    }

    fn get_token(&mut self) -> Result<(), JsonError> {
        self.token.clear();
        self.high_surrogate = 0;
        self.high_offset = None;

        loop {
            let Some(byte) = self.current() else {
                self.done = true;
                break;
            };

            if byte < 32 {
                if matches!(byte, b'\t' | b'\n' | b'\r') {
                    if self.state == LexState::Top {
                        self.ignore();
                    } else {
                        break;
                    }
                } else {
                    return Err(self.error(format!(
                        "JSON: control or null character at offset {}",
                        self.offset()
                    )));
                }
                continue;
            }

            match byte {
                b',' => {
                    if self.state == LexState::Top {
                        self.ignore_state(LexState::Comma);
                        return Ok(());
                    }
                    if self.state == LexState::String {
                        self.append();
                    } else {
                        break;
                    }
                }
                b':' => {
                    if self.state == LexState::Top {
                        self.ignore_state(LexState::Colon);
                        return Ok(());
                    }
                    if self.state == LexState::String {
                        self.append();
                    } else {
                        break;
                    }
                }
                b' ' => {
                    if self.state == LexState::Top {
                        self.ignore();
                    } else if self.state == LexState::String {
                        self.append();
                    } else {
                        break;
                    }
                }
                b'{' => {
                    if self.state == LexState::Top {
                        self.token_start = self.offset();
                        self.ignore_state(LexState::BeginDictionary);
                        return Ok(());
                    }
                    if self.state == LexState::String {
                        self.append();
                    } else {
                        break;
                    }
                }
                b'}' => {
                    if self.state == LexState::Top {
                        self.ignore_state(LexState::EndDictionary);
                        return Ok(());
                    }
                    if self.state == LexState::String {
                        self.append();
                    } else {
                        break;
                    }
                }
                b'[' => {
                    if self.state == LexState::Top {
                        self.token_start = self.offset();
                        self.ignore_state(LexState::BeginArray);
                        return Ok(());
                    }
                    if self.state == LexState::String {
                        self.append();
                    } else {
                        break;
                    }
                }
                b']' => {
                    if self.state == LexState::Top {
                        self.ignore_state(LexState::EndArray);
                        return Ok(());
                    }
                    if self.state == LexState::String {
                        self.append();
                    } else {
                        break;
                    }
                }
                _ => match self.state {
                    LexState::Top => {
                        self.token_start = self.offset();
                        match byte {
                            b'"' => self.ignore_state(LexState::String),
                            b'a'..=b'z' => self.append_state(LexState::Alpha),
                            b'-' => self.append_state(LexState::NumberMinus),
                            b'1'..=b'9' => self.append_state(LexState::NumberBeforePoint),
                            b'0' => self.append_state(LexState::NumberLeadingZero),
                            _ => {
                                return Err(self.error(format!(
                                    "JSON: offset {}: unexpected character {}",
                                    self.offset(),
                                    printable_byte(byte)
                                )));
                            }
                        }
                    }
                    LexState::NumberMinus => match byte {
                        b'1'..=b'9' => self.append_state(LexState::NumberBeforePoint),
                        b'0' => self.append_state(LexState::NumberLeadingZero),
                        _ => {
                            return Err(self.error(format!(
                                "JSON: offset {}: numeric literal: no digit after minus sign",
                                self.offset()
                            )));
                        }
                    },
                    LexState::NumberLeadingZero => match byte {
                        b'.' => self.append_state(LexState::NumberPoint),
                        b'e' | b'E' => self.append_state(LexState::NumberE),
                        _ => {
                            return Err(self.error(format!(
                                "JSON: offset {}: number with leading zero",
                                self.offset()
                            )));
                        }
                    },
                    LexState::NumberBeforePoint => match byte {
                        b'0'..=b'9' => self.append(),
                        b'.' => self.append_state(LexState::NumberPoint),
                        b'e' | b'E' => self.append_state(LexState::NumberE),
                        _ => return self.token_error(),
                    },
                    LexState::NumberPoint => {
                        if byte.is_ascii_digit() {
                            self.append_state(LexState::NumberAfterPoint);
                        } else {
                            return self.token_error();
                        }
                    }
                    LexState::NumberAfterPoint => match byte {
                        b'0'..=b'9' => self.append(),
                        b'e' | b'E' => self.append_state(LexState::NumberE),
                        _ => return self.token_error(),
                    },
                    LexState::NumberE => match byte {
                        b'0'..=b'9' => self.append_state(LexState::Number),
                        b'+' | b'-' => self.append_state(LexState::NumberESign),
                        _ => return self.token_error(),
                    },
                    LexState::NumberESign => {
                        if byte.is_ascii_digit() {
                            self.append_state(LexState::Number);
                        } else {
                            return self.token_error();
                        }
                    }
                    LexState::Number => {
                        if byte.is_ascii_digit() {
                            self.append();
                        } else {
                            return self.token_error();
                        }
                    }
                    LexState::Alpha => {
                        if byte.is_ascii_lowercase() {
                            self.append();
                        } else {
                            return self.token_error();
                        }
                    }
                    LexState::String => match byte {
                        b'"' => {
                            if let Some(high_offset) = self.high_offset {
                                return Err(self.error(format!(
                                    "JSON: offset {high_offset}: UTF-16 high surrogate not followed by low surrogate"
                                )));
                            }
                            self.ignore_state(LexState::AfterString);
                            return Ok(());
                        }
                        b'\\' => self.ignore_state(LexState::Backslash),
                        _ => self.append(),
                    },
                    LexState::Backslash => {
                        self.state = LexState::String;
                        match byte {
                            b'\\' | b'"' | b'/' => self.token.push(byte),
                            b'b' => self.token.push(b'\x08'),
                            b'f' => self.token.push(b'\x0c'),
                            b'n' => self.token.push(b'\n'),
                            b'r' => self.token.push(b'\r'),
                            b't' => self.token.push(b'\t'),
                            b'u' => {
                                self.state = LexState::U4;
                                self.u_count = 0;
                                self.u_value = 0;
                            }
                            _ => {
                                self.state = LexState::Backslash;
                                return self.token_error();
                            }
                        }
                        self.ignore();
                    }
                    LexState::U4 => {
                        let Some(value) = hex_value(byte) else {
                            return self.token_error();
                        };
                        self.u_value = 16 * self.u_value + value;
                        self.u_count += 1;
                        if self.u_count == 4 {
                            self.handle_u_code(self.u_value, self.offset() - 5)?;
                            self.state = LexState::String;
                        }
                        self.ignore();
                    }
                    LexState::AfterString
                    | LexState::BeginDictionary
                    | LexState::EndDictionary
                    | LexState::BeginArray
                    | LexState::EndArray
                    | LexState::Colon
                    | LexState::Comma => unreachable!("delimiter states are handled immediately"), // cov:ignore: delimiter states return from get_token before dispatch
                },
            }
        }

        if !self.token.is_empty() {
            match self.state {
                LexState::NumberLeadingZero
                | LexState::NumberBeforePoint
                | LexState::NumberAfterPoint => self.state = LexState::Number,
                LexState::Number | LexState::Alpha => {}
                _ => self.token_error()?,
            }
        }
        Ok(())
    }

    fn handle_token(&mut self) -> Result<(), JsonError> {
        if self.state == LexState::Top {
            return Ok(());
        }

        if self.parse_state == ParseState::Done {
            return Err(self.error(format!(
                "JSON: offset {}: material follows end of object: {}",
                self.offset(),
                String::from_utf8_lossy(&self.token)
            )));
        }

        let state = std::mem::replace(&mut self.state, LexState::Top);
        let value = match state {
            LexState::BeginDictionary => Json::make_dictionary(),
            LexState::BeginArray => Json::make_array(),
            LexState::Colon => {
                if self.parse_state != ParseState::DictionaryAfterKey {
                    return Err(
                        self.error(format!("JSON: offset {}: unexpected colon", self.offset()))
                    );
                }
                self.parse_state = ParseState::DictionaryAfterColon;
                return Ok(());
            }
            LexState::Comma => {
                self.parse_state = match self.parse_state {
                    ParseState::DictionaryAfterItem => ParseState::DictionaryAfterComma,
                    ParseState::ArrayAfterItem => ParseState::ArrayAfterComma,
                    _ => {
                        return Err(
                            self.error(format!("JSON: offset {}: unexpected comma", self.offset()))
                        );
                    }
                };
                return Ok(());
            }
            LexState::EndArray => {
                if !matches!(
                    self.parse_state,
                    ParseState::ArrayBegin | ParseState::ArrayAfterItem
                ) {
                    return Err(self.error(format!(
                        "JSON: offset {}: unexpected array end delimiter",
                        self.offset()
                    )));
                }
                let entry = self
                    .stack
                    .last()
                    .expect("array end has a matching stack entry");
                self.parse_state = entry.state;
                entry.item.set_end(self.offset());
                if self.parse_state != ParseState::Done {
                    self.stack.pop();
                }
                return Ok(());
            }
            LexState::EndDictionary => {
                if !matches!(
                    self.parse_state,
                    ParseState::DictionaryBegin | ParseState::DictionaryAfterItem
                ) {
                    return Err(self.error(format!(
                        "JSON: offset {}: unexpected dictionary end delimiter",
                        self.offset()
                    )));
                }
                let entry = self
                    .stack
                    .last()
                    .expect("dictionary end has a matching stack entry");
                self.parse_state = entry.state;
                entry.item.set_end(self.offset());
                if self.parse_state != ParseState::Done {
                    self.stack.pop();
                }
                return Ok(());
            }
            LexState::Number => Json::make_number(&self.token),
            LexState::Alpha => match self.token.as_slice() {
                b"true" => Json::make_bool(true),
                b"false" => Json::make_bool(false),
                b"null" => Json::make_null(),
                _ => {
                    return Err(self.error(format!(
                        "JSON: offset {}: invalid keyword {}",
                        self.offset(),
                        String::from_utf8_lossy(&self.token)
                    )));
                }
            },
            LexState::AfterString => {
                if matches!(
                    self.parse_state,
                    ParseState::DictionaryBegin | ParseState::DictionaryAfterComma
                ) {
                    self.dict_key.clone_from(&self.token);
                    self.dict_key_offset = self.token_start;
                    self.parse_state = ParseState::DictionaryAfterKey;
                    return Ok(());
                }
                Json::make_string(&self.token)
            }
            _ => {
                return Err(self.error(format!(
                    "JSON: offset {}: premature end of input",
                    self.offset()
                )))
            }
        };

        value.set_start(self.token_start);
        value.set_end(self.offset());

        match self.parse_state {
            ParseState::DictionaryBegin | ParseState::DictionaryAfterComma => {
                return Err(self.error(format!(
                    "JSON: offset {}: expect string as dictionary key",
                    self.offset()
                )));
            }
            ParseState::DictionaryAfterColon => {
                let parent = &self
                    .stack
                    .last()
                    .expect("dictionary value has a matching stack entry")
                    .item;
                if parent.check_dictionary_key_seen(&self.dict_key)? {
                    return Err(self.error(format!(
                        "JSON: offset {}: duplicated dictionary key",
                        self.dict_key_offset
                    )));
                }
                parent.add_dictionary_member(&self.dict_key, value.clone())?;
                self.parse_state = ParseState::DictionaryAfterItem;
            }
            ParseState::ArrayBegin | ParseState::ArrayAfterComma => {
                self.stack
                    .last()
                    .expect("array value has a matching stack entry")
                    .item
                    .add_array_element(value.clone())?;
                self.parse_state = ParseState::ArrayAfterItem;
            }
            ParseState::Top => {
                self.parse_state = ParseState::Done;
            }
            ParseState::DictionaryAfterKey => {
                return Err(self.error(format!("JSON: offset {}: expected ':'", self.offset())));
            }
            ParseState::DictionaryAfterItem => {
                return Err(self.error(format!(
                    "JSON: offset {}: expected ',' or '}}'",
                    self.offset()
                )));
            }
            ParseState::ArrayAfterItem => {
                return Err(self.error(format!(
                    "JSON: offset {}: expected ',' or ']'",
                    self.offset()
                )));
            }
            ParseState::Done => unreachable!("done state is checked before token dispatch"),
        }

        if value.is_dictionary() || value.is_array() {
            self.stack.push(StackEntry {
                state: self.parse_state,
                item: value,
            });
            self.parse_state = if self
                .stack
                .last()
                .expect("new stack entry exists")
                .item
                .is_dictionary()
            {
                ParseState::DictionaryBegin
            } else {
                ParseState::ArrayBegin
            };

            if self.stack.len() > 500 {
                return Err(self.error(format!(
                    "JSON: offset {}: maximum object depth exceeded",
                    self.offset()
                )));
            }
        } else if self.parse_state == ParseState::Done {
            self.stack.push(StackEntry {
                state: ParseState::Done,
                item: value,
            });
        }
        Ok(())
    }
}

fn hex_value(byte: u8) -> Option<u32> {
    match byte {
        b'0'..=b'9' => Some((byte - b'0') as u32),
        b'a'..=b'f' => Some((byte - b'a' + 10) as u32),
        b'A'..=b'F' => Some((byte - b'A' + 10) as u32),
        _ => None,
    }
}

fn append_utf8(out: &mut Vec<u8>, codepoint: u32) {
    let character = char::from_u32(codepoint).expect("JSON UTF-16 code point is valid Unicode");
    let mut buf = [0; 4];
    out.extend_from_slice(character.encode_utf8(&mut buf).as_bytes());
}

fn printable_byte(byte: u8) -> char {
    char::from(byte)
}
