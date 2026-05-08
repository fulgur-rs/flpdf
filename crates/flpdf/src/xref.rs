use crate::parser::Parser;
use crate::{Dictionary, Error, Object, ObjectRef, Result};
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};

#[derive(Debug, Clone)]
pub struct LoadedXref {
    pub version: String,
    pub startxref: u64,
    pub entries: BTreeMap<ObjectRef, u64>,
    pub trailer: Dictionary,
}

pub fn load_xref_and_trailer<R: Read + Seek>(reader: &mut R) -> Result<LoadedXref> {
    let mut bytes = Vec::new();
    reader.seek(SeekFrom::Start(0))?;
    reader.read_to_end(&mut bytes)?;

    let version = parse_header(&bytes)?;
    let startxref = parse_startxref(&bytes)?;
    let xref_pos =
        usize::try_from(startxref).map_err(|_| Error::parse(0, "startxref does not fit usize"))?;

    if !bytes
        .get(xref_pos..)
        .is_some_and(|tail| tail.starts_with(b"xref"))
    {
        return Err(Error::Unsupported(
            "xref streams are not supported in the first milestone".to_string(),
        ));
    }

    let mut cursor = ByteCursor::new(&bytes, xref_pos + 4);
    let mut entries = BTreeMap::new();
    loop {
        cursor.skip_ws();
        if cursor.starts_with(b"trailer") {
            cursor.pos += b"trailer".len();
            break;
        }

        let first = cursor.read_u32()?;
        let count = cursor.read_u32()?;
        for index in 0..count {
            cursor.skip_ws();
            let offset = cursor.read_fixed_u64(10)?;
            cursor.skip_ws();
            let generation = cursor.read_fixed_u16(5)?;
            cursor.skip_ws();
            let in_use = cursor.read_byte()?;
            cursor.skip_line();
            if in_use == b'n' {
                entries.insert(ObjectRef::new(first + index, generation), offset);
            }
        }
    }

    cursor.skip_ws();
    let mut parser = Parser::new(&bytes[cursor.pos..]);
    let trailer = match parser.object()? {
        Object::Dictionary(dict) => dict,
        _ => {
            return Err(Error::parse(
                cursor.pos + parser.position(),
                "trailer is not a dictionary",
            ))
        }
    };

    Ok(LoadedXref {
        version,
        startxref,
        entries,
        trailer,
    })
}

fn parse_header(bytes: &[u8]) -> Result<String> {
    if !bytes.starts_with(b"%PDF-") {
        return Err(Error::parse(0, "missing PDF header"));
    }

    let end = bytes
        .iter()
        .position(|byte| *byte == b'\n' || *byte == b'\r')
        .unwrap_or(bytes.len());
    let header = std::str::from_utf8(&bytes[5..end])
        .map_err(|_| Error::parse(5, "PDF version is not utf-8"))?;
    Ok(header.to_string())
}

fn parse_startxref(bytes: &[u8]) -> Result<u64> {
    let marker = b"startxref";
    let Some(pos) = bytes
        .windows(marker.len())
        .rposition(|window| window == marker)
    else {
        return Err(Error::parse(bytes.len(), "missing startxref"));
    };

    let mut cursor = ByteCursor::new(bytes, pos + marker.len());
    cursor.skip_ws();
    cursor.read_u64()
}

struct ByteCursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> ByteCursor<'a> {
    fn new(bytes: &'a [u8], pos: usize) -> Self {
        Self { bytes, pos }
    }

    fn starts_with(&self, token: &[u8]) -> bool {
        self.bytes[self.pos..].starts_with(token)
    }

    fn skip_ws(&mut self) {
        while matches!(
            self.bytes.get(self.pos),
            Some(b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
        ) {
            self.pos += 1;
        }
    }

    fn skip_line(&mut self) {
        while !matches!(self.bytes.get(self.pos), None | Some(b'\n' | b'\r')) {
            self.pos += 1;
        }
        while matches!(self.bytes.get(self.pos), Some(b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn read_byte(&mut self) -> Result<u8> {
        let Some(byte) = self.bytes.get(self.pos).copied() else {
            return Err(Error::parse(self.pos, "unexpected end of input"));
        };
        self.pos += 1;
        Ok(byte)
    }

    fn read_u32(&mut self) -> Result<u32> {
        let value = self.read_unsigned()?;
        u32::try_from(value).map_err(|_| Error::parse(self.pos, "number does not fit u32"))
    }

    fn read_u64(&mut self) -> Result<u64> {
        self.read_unsigned()
    }

    fn read_fixed_u64(&mut self, width: usize) -> Result<u64> {
        self.read_fixed(width)?
            .parse::<u64>()
            .map_err(|_| Error::parse(self.pos, "invalid fixed-width u64"))
    }

    fn read_fixed_u16(&mut self, width: usize) -> Result<u16> {
        self.read_fixed(width)?
            .parse::<u16>()
            .map_err(|_| Error::parse(self.pos, "invalid fixed-width u16"))
    }

    fn read_fixed(&mut self, width: usize) -> Result<&str> {
        if self.pos + width > self.bytes.len() {
            return Err(Error::parse(
                self.pos,
                "unexpected end of fixed-width field",
            ));
        }
        let text = std::str::from_utf8(&self.bytes[self.pos..self.pos + width])
            .map_err(|_| Error::parse(self.pos, "field is not utf-8"))?;
        self.pos += width;
        Ok(text)
    }

    fn read_unsigned(&mut self) -> Result<u64> {
        self.skip_ws();
        let start = self.pos;
        while matches!(self.bytes.get(self.pos), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        if start == self.pos {
            return Err(Error::parse(start, "expected unsigned integer"));
        }

        let text = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|_| Error::parse(start, "number is not utf-8"))?;
        text.parse::<u64>()
            .map_err(|_| Error::parse(start, "invalid unsigned integer"))
    }
}
