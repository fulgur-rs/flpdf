use crate::{Dictionary, Error, Object, ObjectRef, Result};

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
                return Ok(Object::Dictionary(dict));
            }
            let key = self.name()?;
            let value = self.object()?;
            dict.insert(key, value);
        }
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
        let first = self.integer()?;
        let saved = self.pos;
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
        let start = self.pos;
        while let Some(byte) = self.peek() {
            if is_delimiter(byte) || is_ws(byte) {
                break;
            }
            self.pos += 1;
        }
        if start == self.pos {
            return Err(Error::parse(start, "empty name"));
        }
        Ok(self.input[start..self.pos].to_vec())
    }

    fn literal_string(&mut self) -> Result<Vec<u8>> {
        self.expect_byte(b'(')?;
        let start = self.pos;
        while let Some(byte) = self.peek() {
            if byte == b')' {
                let value = self.input[start..self.pos].to_vec();
                self.pos += 1;
                return Ok(value);
            }
            self.pos += 1;
        }
        Err(Error::parse(start, "unterminated literal string"))
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
