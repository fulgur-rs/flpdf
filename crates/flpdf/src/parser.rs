use crate::{Dictionary, Error, Object, ObjectRef, Result, Stream};

pub fn parse_object(input: &[u8]) -> Result<Object> {
    let mut parser = Parser::new(input);
    let object = parser.object()?;
    parser.skip_ws();
    if parser.pos != parser.input.len() {
        return Err(Error::parse(parser.pos, "trailing bytes after object"));
    }
    Ok(object)
}

pub(crate) fn parse_indirect_object(input: &[u8]) -> Result<(ObjectRef, Object)> {
    let mut parser = Parser::new(input);
    let number = parser.integer_for_indirect()?;
    let generation = parser.integer_for_indirect()?;
    parser.expect_keyword_for_indirect(b"obj")?;
    let object = parser.object()?;
    Ok((
        ObjectRef::new(
            u32::try_from(number).map_err(|_| Error::parse(0, "invalid indirect object number"))?,
            u16::try_from(generation)
                .map_err(|_| Error::parse(0, "invalid indirect generation"))?,
        ),
        object,
    ))
}

pub(crate) struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    pub(crate) fn new(input: &'a [u8]) -> Self {
        Self { input, pos: 0 }
    }

    pub(crate) fn position(&self) -> usize {
        self.pos
    }

    pub(crate) fn object(&mut self) -> Result<Object> {
        self.skip_ws();
        if self.starts_with(b"<<") {
            return self.dictionary();
        }
        match self.peek() {
            Some(b'[') => self.array(),
            Some(b'/') => self.name().map(Object::Name),
            Some(b'(') => self.literal_string().map(Object::String),
            Some(b'<') => self.hex_string().map(Object::String),
            Some(b'.') => self.real(),
            Some(b'-' | b'+')
                if self
                    .input
                    .get(self.pos + 1)
                    .is_some_and(|byte| *byte == b'.') =>
            {
                self.real()
            }
            Some(b't') if self.take_keyword(b"true") => Ok(Object::Boolean(true)),
            Some(b'f') if self.take_keyword(b"false") => Ok(Object::Boolean(false)),
            Some(b'n') if self.take_keyword(b"null") => Ok(Object::Null),
            Some(b'-' | b'+' | b'0'..=b'9') => self.number_or_ref(),
            _ => Err(Error::parse(self.pos, "expected PDF object")),
        }
    }

    fn dictionary(&mut self) -> Result<Object> {
        self.expect_bytes(b"<<")?;
        let mut dict = Dictionary::new();
        loop {
            self.skip_ws();
            if self.starts_with(b">>") {
                self.pos += 2;
                self.skip_ws();
                if self.starts_with(b"stream") {
                    return self.stream_from_dict(dict);
                }
                return Ok(Object::Dictionary(dict));
            }
            let key = self.name()?;
            let value = self.object()?;
            dict.insert(key, value);
        }
    }

    fn stream_from_dict(&mut self, dict: Dictionary) -> Result<Object> {
        let Some(length) = dict.get("Length") else {
            return Err(Error::parse(self.pos, "missing stream length"));
        };
        let Object::Integer(length) = length else {
            return Err(Error::parse(self.pos, "stream /Length is not integer"));
        };
        if *length < 0 {
            return Err(Error::parse(self.pos, "stream /Length is negative"));
        }

        self.expect_keyword_for_indirect(b"stream")?;
        if self.peek() == Some(b'\r') {
            self.pos += 1;
            if self.peek() == Some(b'\n') {
                self.pos += 1;
            }
        } else if self.peek() == Some(b'\n') {
            self.pos += 1;
        }

        let length = u64::try_from(*length)
            .map_err(|_| Error::parse(self.pos, "stream /Length does not fit u64"))?
            as usize;
        if self.pos + length > self.input.len() {
            return Err(Error::parse(self.pos, "stream data exceeds input"));
        }

        let data = self.input[self.pos..self.pos + length].to_vec();
        self.pos += length;

        self.skip_ws();
        self.expect_keyword_for_indirect(b"endstream")?;

        Ok(Object::Stream(Stream::new(dict, data)))
    }

    fn hex_string(&mut self) -> Result<Vec<u8>> {
        self.expect_byte(b'<')?;
        let mut value = Vec::new();
        let mut first_nibble: Option<u8> = None;

        while let Some(byte) = self.peek() {
            if byte == b'>' {
                self.pos += 1;
                if first_nibble.is_some() {
                    return Err(Error::parse(self.pos, "hex string has odd length"));
                }
                return Ok(value);
            }

            let nibble = match hex_value(byte) {
                Some(byte) => byte,
                None => return Err(Error::parse(self.pos, "invalid hex string")),
            };

            self.pos += 1;

            if let Some(high) = first_nibble {
                value.push((high << 4) | nibble);
                first_nibble = None;
            } else {
                first_nibble = Some(nibble);
            }
        }

        Err(Error::parse(self.pos, "unterminated hex string"))
    }

    fn array(&mut self) -> Result<Object> {
        self.expect_byte(b'[')?;
        let mut values = Vec::new();
        loop {
            self.skip_ws();
            if self.peek() == Some(b']') {
                self.pos += 1;
                return Ok(Object::Array(values));
            }
            values.push(self.object()?);
        }
    }

    fn number_or_ref(&mut self) -> Result<Object> {
        let start = self.pos;
        let first = self.integer()?;
        let saved = self.pos;
        if self.peek() == Some(b'.') {
            return self.real_with_integer_prefix(start);
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            return self.parse_real_exponent(start);
        }
        self.skip_ws();
        if let Ok(second) = self.integer() {
            self.skip_ws();
            if self.peek() == Some(b'R') {
                self.pos += 1;
                let number = u32::try_from(first)
                    .map_err(|_| Error::parse(saved, "invalid object number"))?;
                let generation = u16::try_from(second)
                    .map_err(|_| Error::parse(saved, "invalid generation number"))?;
                return Ok(Object::Reference(ObjectRef::new(number, generation)));
            }
        }
        self.pos = saved;
        Ok(Object::Integer(first))
    }

    fn real(&mut self) -> Result<Object> {
        let start = self.pos;
        if matches!(self.peek(), Some(b'+' | b'-')) {
            self.pos += 1;
        }
        if self.peek() != Some(b'.') {
            return Err(Error::parse(start, "expected real number"));
        }
        self.pos += 1;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        self.parse_real_exponent(start)
    }

    fn real_with_integer_prefix(&mut self, start: usize) -> Result<Object> {
        if self.peek() != Some(b'.') {
            return Err(Error::parse(start, "expected decimal point"));
        }
        self.pos += 1;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        self.parse_real_exponent(start)
    }

    fn parse_real_exponent(&mut self, start: usize) -> Result<Object> {
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            let exponent_start = self.pos;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
            if exponent_start == self.pos {
                return Err(Error::parse(start, "invalid real"));
            }
        }

        let text = std::str::from_utf8(&self.input[start..self.pos])
            .map_err(|_| Error::parse(start, "real is not utf-8"))?;
        text.parse::<f64>()
            .map(Object::Real)
            .map_err(|_| Error::parse(start, "invalid real"))
    }

    fn integer(&mut self) -> Result<i64> {
        self.skip_ws();
        let start = self.pos;
        if matches!(self.peek(), Some(b'-' | b'+')) {
            self.pos += 1;
        }
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        if start == self.pos
            || matches!(self.input.get(start), Some(b'-' | b'+')) && start + 1 == self.pos
        {
            return Err(Error::parse(start, "expected integer"));
        }
        let text = std::str::from_utf8(&self.input[start..self.pos])
            .map_err(|_| Error::parse(start, "integer is not utf-8"))?;
        text.parse::<i64>()
            .map_err(|_| Error::parse(start, "invalid integer"))
    }

    pub(crate) fn integer_for_indirect(&mut self) -> Result<i64> {
        self.integer()
    }

    pub(crate) fn expect_keyword_for_indirect(&mut self, keyword: &[u8]) -> Result<()> {
        self.skip_ws();
        if self.starts_with(keyword) {
            self.pos += keyword.len();
            Ok(())
        } else {
            Err(Error::parse(self.pos, "expected indirect object keyword"))
        }
    }

    fn name(&mut self) -> Result<Vec<u8>> {
        self.expect_byte(b'/')?;
        let mut value = Vec::new();
        while let Some(byte) = self.peek() {
            if is_delimiter(byte) || is_ws(byte) {
                break;
            }
            if byte == b'#' {
                let pos = self.pos;
                self.pos += 1;
                let high = self
                    .peek()
                    .and_then(hex_value)
                    .ok_or_else(|| Error::parse(pos, "invalid name escape"))?;
                self.pos += 1;
                let low = self
                    .peek()
                    .and_then(hex_value)
                    .ok_or_else(|| Error::parse(pos, "invalid name escape"))?;
                self.pos += 1;
                value.push((high << 4) | low);
            } else {
                value.push(byte);
                self.pos += 1;
            }
        }
        if value.is_empty() {
            return Err(Error::parse(self.pos, "empty name"));
        }
        Ok(value)
    }

    fn literal_string(&mut self) -> Result<Vec<u8>> {
        self.expect_byte(b'(')?;
        let mut value = Vec::new();
        let mut depth = 0;
        while let Some(byte) = self.peek() {
            if byte == b'(' {
                depth += 1;
                value.push(byte);
                self.pos += 1;
                continue;
            }

            if byte == b')' {
                if depth == 0 {
                    self.pos += 1;
                    return Ok(value);
                }
                depth -= 1;
                value.push(byte);
                self.pos += 1;
                continue;
            }

            if byte == b'\\' {
                self.pos += 1;
                match self.peek() {
                    Some(b'n') => {
                        value.push(b'\n');
                        self.pos += 1;
                    }
                    Some(b'r') => {
                        value.push(b'\r');
                        self.pos += 1;
                    }
                    Some(b't') => {
                        value.push(b'\t');
                        self.pos += 1;
                    }
                    Some(b'b') => {
                        value.push(0x08);
                        self.pos += 1;
                    }
                    Some(b'f') => {
                        value.push(0x0c);
                        self.pos += 1;
                    }
                    Some(b'(' | b')' | b'\\') => {
                        value.push(self.peek().unwrap_or_default());
                        self.pos += 1;
                    }
                    Some(b'\r') => {
                        self.pos += 1;
                        if self.peek() == Some(b'\n') {
                            self.pos += 1;
                        }
                    }
                    Some(b'\n') => {
                        self.pos += 1;
                    }
                    Some(byte @ b'0'..=b'7') => {
                        let first = byte;
                        let mut value_byte = first - b'0';
                        self.pos += 1;
                        for _ in 0..2 {
                            if let Some(next) = self.peek() {
                                if matches!(next, b'0'..=b'7') {
                                    value_byte = (value_byte << 3) | (next - b'0');
                                    self.pos += 1;
                                } else {
                                    break;
                                }
                            }
                        }
                        value.push(value_byte);
                    }
                    Some(_) => {
                        value.push(self.peek().unwrap_or_default());
                        self.pos += 1;
                    }
                    None => return Err(Error::parse(self.pos, "unterminated literal string")),
                }
                continue;
            }

            self.pos += 1;
            value.push(byte);
        }

        Err(Error::parse(self.pos, "unterminated literal string"))
    }

    pub(crate) fn skip_ws(&mut self) {
        loop {
            while matches!(self.peek(), Some(byte) if is_ws(byte)) {
                self.pos += 1;
            }
            if self.peek() == Some(b'%') {
                while !matches!(self.peek(), None | Some(b'\n' | b'\r')) {
                    self.pos += 1;
                }
                continue;
            }
            break;
        }
    }

    fn take_keyword(&mut self, keyword: &[u8]) -> bool {
        if self.starts_with(keyword) {
            self.pos += keyword.len();
            true
        } else {
            false
        }
    }

    fn expect_byte(&mut self, expected: u8) -> Result<()> {
        if self.peek() == Some(expected) {
            self.pos += 1;
            Ok(())
        } else {
            Err(Error::parse(self.pos, format!("expected byte {expected}")))
        }
    }

    fn expect_bytes(&mut self, expected: &[u8]) -> Result<()> {
        if self.starts_with(expected) {
            self.pos += expected.len();
            Ok(())
        } else {
            Err(Error::parse(self.pos, "expected token"))
        }
    }

    fn starts_with(&self, bytes: &[u8]) -> bool {
        self.input[self.pos..].starts_with(bytes)
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }
}

fn is_ws(byte: u8) -> bool {
    matches!(byte, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

fn is_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
