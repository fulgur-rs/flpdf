use crate::parser::{parse_indirect_object, Parser};
use crate::{filters, Diagnostic, Diagnostics, Dictionary, Error, Object, ObjectRef, Result};
use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Seek, SeekFrom};

#[derive(Debug, Clone)]
pub struct LoadedXref {
    pub version: String,
    pub startxref: u64,
    pub entries: BTreeMap<ObjectRef, XrefOffset>,
    pub trailer: Dictionary,
    pub last_xref_form: XrefForm,
    pub repair_diagnostics: Diagnostics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XrefOffset {
    Offset(u64),
    Compressed { stream: u32, index: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XrefForm {
    Table,
    Stream,
}

pub fn load_xref_and_trailer<R: Read + Seek>(reader: &mut R) -> Result<LoadedXref> {
    load_xref_and_trailer_with_repair(reader, false)
}

pub fn load_xref_and_trailer_with_repair<R: Read + Seek>(
    reader: &mut R,
    allow_repair: bool,
) -> Result<LoadedXref> {
    let mut bytes = Vec::new();
    reader.seek(SeekFrom::Start(0))?;
    reader.read_to_end(&mut bytes)?;

    let version = parse_header(&bytes)?;
    let mut parse_errors = Vec::new();
    let startxref = match parse_startxref(&bytes) {
        Ok(offset) => offset,
        Err(error) if allow_repair => {
            parse_errors.push(error);
            0
        }
        Err(error) => return Err(error),
    };
    let xref_pos = match usize::try_from(startxref) {
        Ok(xref_pos) => xref_pos,
        Err(_) if allow_repair => {
            parse_errors.push(Error::parse(0, "startxref does not fit usize"));
            0
        }
        Err(_) => return Err(Error::parse(0, "startxref does not fit usize")),
    };

    let mut loaded = match parse_xref_from_start(&bytes, xref_pos, startxref, &version) {
        Ok(loaded) => loaded,
        Err(error) if allow_repair => {
            parse_errors.push(error);
            return recover_xref_from_linear_scan(&bytes, version, startxref, parse_errors);
        }
        Err(error) => return Err(error),
    };

    if let Err(error) = merge_previous_xref_sections(
        &bytes,
        &version,
        &mut loaded.entries,
        &loaded.trailer,
        allow_repair,
    ) {
        if allow_repair {
            parse_errors.push(error);
            return recover_xref_from_linear_scan(&bytes, version, startxref, parse_errors);
        }
        return Err(error);
    }

    if !parse_errors.is_empty() {
        loaded.repair_diagnostics.push(Diagnostic::warning(
            format_repair_diagnostic(parse_errors),
            Some(startxref),
        ));
    }

    Ok(loaded)
}

fn parse_xref_from_start(
    bytes: &[u8],
    xref_pos: usize,
    startxref: u64,
    version: &str,
) -> Result<LoadedXref> {
    if bytes
        .get(xref_pos..)
        .is_some_and(|tail| tail.starts_with(b"xref"))
    {
        let mut cursor = ByteCursor::new(bytes, xref_pos + 4);
        let (entries, trailer) = parse_xref_table(&mut cursor, bytes)?;
        return Ok(LoadedXref {
            version: version.to_string(),
            startxref,
            entries,
            trailer,
            last_xref_form: XrefForm::Table,
            repair_diagnostics: Diagnostics::default(),
        });
    }

    parse_xref_stream(bytes, xref_pos, startxref, version.to_string())
}

fn merge_previous_xref_sections(
    bytes: &[u8],
    version: &str,
    entries: &mut BTreeMap<ObjectRef, XrefOffset>,
    trailer: &Dictionary,
    allow_repair: bool,
) -> Result<()> {
    let mut visited = HashSet::new();
    let mut previous_offset = parse_previous_xref_offset(trailer);

    while let Some(offset) = previous_offset {
        let previous_pos = usize::try_from(offset)
            .map_err(|_| Error::parse(0, "xref /Prev does not fit usize"))?;

        if !visited.insert(offset) {
            return if allow_repair {
                Ok(())
            } else {
                Err(Error::parse(0, "xref /Prev is circular"))
            };
        }

        let previous = parse_xref_from_start(bytes, previous_pos, offset, version)?;

        for (object_ref, xref_offset) in previous.entries {
            entries.entry(object_ref).or_insert(xref_offset);
        }

        previous_offset = parse_previous_xref_offset(&previous.trailer);
    }

    Ok(())
}

fn parse_previous_xref_offset(trailer: &Dictionary) -> Option<u64> {
    trailer
        .get("Prev")
        .and_then(|offset| parse_non_negative_u64(offset, "/Prev").ok())
        .and_then(|offset| if offset == 0 { None } else { Some(offset) })
}

fn recover_xref_from_linear_scan(
    bytes: &[u8],
    version: String,
    startxref: u64,
    parse_errors: Vec<Error>,
) -> Result<LoadedXref> {
    let entries = recover_xref_entries(bytes)?;
    let trailer = recover_trailer(bytes)?;

    let mut repair_diagnostics = Diagnostics::default();
    repair_diagnostics.push(Diagnostic::warning(
        format_repair_diagnostic(parse_errors),
        Some(startxref),
    ));

    Ok(LoadedXref {
        version,
        startxref,
        entries,
        trailer,
        last_xref_form: XrefForm::Table,
        repair_diagnostics,
    })
}

pub fn load_xref_and_trailer_best_effort<R: Read + Seek>(reader: &mut R) -> Result<LoadedXref> {
    load_xref_and_trailer_with_repair(reader, true)
}

fn recover_xref_entries(bytes: &[u8]) -> Result<BTreeMap<ObjectRef, XrefOffset>> {
    let mut entries = BTreeMap::new();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        if bytes[cursor].is_ascii_digit() && is_token_boundary(cursor, bytes) {
            if let Ok((object_ref, _)) = parse_indirect_object(&bytes[cursor..]) {
                entries.insert(object_ref, XrefOffset::Offset(cursor as u64));
                if let Ok((_object_ref, Object::Stream(stream))) =
                    parse_indirect_object(&bytes[cursor..])
                {
                    if let Some(Object::Name(type_name)) = stream.dict.get("Type") {
                        if type_name.as_slice() == b"ObjStm" {
                            recover_compressed_offsets_from_objstm(
                                &mut entries,
                                _object_ref,
                                &stream,
                            );
                        }
                    }
                }
                cursor = cursor.saturating_add(1);
                continue;
            }
        }
        cursor = cursor.saturating_add(1);
    }

    if entries.is_empty() {
        return Err(Error::parse(
            0,
            "unable to recover xref entries by linear scan",
        ));
    }

    Ok(entries)
}

fn recover_compressed_offsets_from_objstm(
    entries: &mut BTreeMap<ObjectRef, XrefOffset>,
    stream_ref: ObjectRef,
    stream: &crate::Stream,
) {
    let Ok(decoded_data) = crate::filters::decode_stream_data(&stream.dict, &stream.data) else {
        return;
    };

    let object_count =
        match parse_non_negative_u64(stream.dict.get("N").unwrap_or(&Object::Integer(0)), "/N") {
            Ok(count) => match usize::try_from(count) {
                Ok(count) => count,
                Err(_) => return,
            },
            Err(_) => return,
        };

    let mut cursor = Parser::new(&decoded_data);
    for index in 0..object_count {
        let number = match cursor.integer_for_indirect() {
            Ok(number) => match parse_non_negative_i64(number, "ObjStm object number") {
                Ok(number) => number,
                Err(_) => return,
            },
            Err(_) => return,
        };
        let object_ref = match u32::try_from(number) {
            Ok(object_ref) => ObjectRef::new(object_ref, 0),
            Err(_) => return,
        };

        match cursor.integer_for_indirect() {
            Ok(offset) => {
                if parse_non_negative_i64(offset, "ObjStm object offset").is_err() {
                    return;
                }
                entries.entry(object_ref).or_insert(XrefOffset::Compressed {
                    stream: stream_ref.number,
                    index: u32::try_from(index).unwrap_or(u32::MAX),
                });
            }
            Err(_) => return,
        }
    }
}

fn parse_non_negative_i64(value: i64, name: &str) -> Result<u64> {
    if value < 0 {
        return Err(Error::parse(0, format!("{name} is negative")));
    }
    Ok(value as u64)
}

fn format_repair_diagnostic(parse_errors: Vec<Error>) -> String {
    match parse_errors.len() {
        0 => String::from("xref parsing failed and was repaired by linear object scan"),
        1 => format!(
            "xref parsing failed ({}) and was repaired by linear object scan",
            parse_errors[0]
        ),
        _ => {
            let mut message =
                String::from("xref parsing failed and was repaired by linear object scan: ");
            for (index, error) in parse_errors.iter().enumerate() {
                if index > 0 {
                    message.push_str("; ");
                }
                message.push_str(&error.to_string());
            }
            message
        }
    }
}

fn recover_trailer(bytes: &[u8]) -> Result<Dictionary> {
    let marker = b"trailer";
    let Some(pos) = bytes
        .windows(marker.len())
        .rposition(|window| window == marker)
    else {
        return Err(Error::parse(0, "trailer dictionary not found"));
    };

    let mut cursor = ByteCursor::new(bytes, pos + marker.len());
    cursor.skip_ws();
    let mut parser = Parser::new(&bytes[cursor.pos..]);
    match parser.object()? {
        Object::Dictionary(trailer) => Ok(trailer),
        _ => Err(Error::parse(
            cursor.pos + parser.position(),
            "trailer dictionary is not a dictionary",
        )),
    }
}

fn is_token_boundary(index: usize, bytes: &[u8]) -> bool {
    if index == 0 {
        return true;
    }

    matches!(
        bytes[index - 1],
        b'\0'
            | b'\t'
            | b'\n'
            | b'\r'
            | b' '
            | b'\x0c'
            | b'['
            | b']'
            | b'<'
            | b'>'
            | b'('
            | b')'
            | b'/',
    )
}

fn parse_xref_table(
    cursor: &mut ByteCursor<'_>,
    bytes: &[u8],
) -> Result<(BTreeMap<ObjectRef, XrefOffset>, Dictionary)> {
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
                entries.insert(
                    ObjectRef::new(first + index, generation),
                    XrefOffset::Offset(offset),
                );
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
            ));
        }
    };

    Ok((entries, trailer))
}

fn parse_xref_stream(
    bytes: &[u8],
    xref_pos: usize,
    startxref: u64,
    version: String,
) -> Result<LoadedXref> {
    let (_, object) = parse_indirect_object(&bytes[xref_pos..])?;
    let stream = match object {
        Object::Stream(stream) => stream,
        _ => {
            return Err(Error::Unsupported(
                "xref stream expected an indirect object stream".to_string(),
            ))
        }
    };

    let trailer = stream.dict.clone();
    let size = parse_non_negative_u64(
        trailer
            .get("Size")
            .ok_or(Error::Missing("XRef stream /Size"))?,
        "/Size",
    )?;
    let size = u32::try_from(size).map_err(|_| Error::parse(0, "/Size does not fit u32"))?;

    let widths = parse_xref_widths(&trailer)?;
    let index = parse_xref_index(&trailer, size)?;
    let ranges = build_xref_ranges(index)?;
    let stream_data = filters::decode_stream_data(&stream.dict, &stream.data)?;
    let mut cursor = ByteCursor::new(&stream_data, 0);
    let entries = parse_xref_entries(&mut cursor, size, &ranges, widths)?;

    Ok(LoadedXref {
        version,
        startxref,
        entries,
        trailer,
        last_xref_form: XrefForm::Stream,
        repair_diagnostics: Diagnostics::default(),
    })
}

type XrefWidths = (usize, usize, usize);

fn parse_xref_widths(trailer: &Dictionary) -> Result<XrefWidths> {
    let Object::Array(values) = trailer.get("W").ok_or(Error::Missing("XRef stream /W"))? else {
        return Err(Error::parse(0, "/W must be array"));
    };

    if values.len() != 3 {
        return Err(Error::parse(0, "/W must contain three integers"));
    }

    let w0 = parse_usize(parse_non_negative_u64(&values[0], "/W[0]")?, "/W[0]")?;
    let w1 = parse_usize(parse_non_negative_u64(&values[1], "/W[1]")?, "/W[1]")?;
    let w2 = parse_usize(parse_non_negative_u64(&values[2], "/W[2]")?, "/W[2]")?;

    Ok((w0, w1, w2))
}

fn parse_xref_index(trailer: &Dictionary, size: u32) -> Result<Vec<u32>> {
    match trailer.get("Index") {
        None => Ok(vec![0, size]),
        Some(Object::Array(values)) => {
            if values.len() % 2 != 0 {
                return Err(Error::parse(
                    0,
                    "/Index must contain an even number of integers",
                ));
            }

            let mut index = Vec::with_capacity(values.len());
            for value in values {
                let integer = parse_non_negative_u64(value, "/Index")?;
                index.push(
                    integer
                        .try_into()
                        .map_err(|_| Error::parse(0, "xref /Index value must fit u32"))?,
                );
            }
            Ok(index)
        }
        _ => Err(Error::parse(0, "/Index must be array")),
    }
}

fn build_xref_ranges(index: Vec<u32>) -> Result<Vec<(u32, u32)>> {
    let mut ranges = Vec::with_capacity(index.len() / 2);
    for chunk in index.chunks_exact(2) {
        if chunk[1] == 0 {
            continue;
        }
        ranges.push((chunk[0], chunk[1]));
    }
    Ok(ranges)
}

fn parse_xref_entries(
    cursor: &mut ByteCursor<'_>,
    size: u32,
    ranges: &[(u32, u32)],
    widths: XrefWidths,
) -> Result<BTreeMap<ObjectRef, XrefOffset>> {
    let (w0, w1, w2) = widths;
    let entry_width = w0 + w1 + w2;
    if entry_width == 0 {
        return Err(Error::parse(0, "invalid cross-reference stream widths"));
    }

    let mut entries = BTreeMap::new();
    for &(start, count) in ranges {
        let start =
            usize::try_from(start).map_err(|_| Error::parse(0, "object number too large"))?;
        let count = usize::try_from(count).map_err(|_| Error::parse(0, "range count too large"))?;

        for index in 0..count {
            if start + index >= usize::try_from(size).unwrap_or(usize::MAX) {
                return Err(Error::parse(0, "xref range exceeds /Size"));
            }

            if cursor.pos + entry_width > cursor.bytes.len() {
                return Err(Error::parse(cursor.pos, "xref stream data truncated"));
            }

            let object_type = if w0 == 0 {
                1
            } else {
                let value = cursor.read_be_u64(w0)?;
                u8::try_from(value).map_err(|_| {
                    Error::parse(cursor.pos, "xref stream object type does not fit u8")
                })?
            };
            let field1 = if w1 == 0 { 0 } else { cursor.read_be_u64(w1)? };
            let field2 = if w2 == 0 { 0 } else { cursor.read_be_u64(w2)? };

            let object_number = (start + index) as u32;
            match object_type {
                0 => {}
                1 => {
                    let generation = u16::try_from(field2)
                        .map_err(|_| Error::parse(0, "generation does not fit u16"))?;
                    entries.insert(
                        ObjectRef::new(object_number, generation),
                        XrefOffset::Offset(field1),
                    );
                }
                2 => {
                    let stream = u32::try_from(field1).map_err(|_| {
                        Error::parse(0, "xref stream object number does not fit u32")
                    })?;
                    let index = u32::try_from(field2)
                        .map_err(|_| Error::parse(0, "xref stream index does not fit u32"))?;
                    entries.insert(
                        ObjectRef::new(object_number, 0),
                        XrefOffset::Compressed { stream, index },
                    );
                }
                _ => {
                    return Err(Error::Unsupported(format!(
                        "unsupported xref entry type {object_type}"
                    )))
                }
            }
        }
    }

    Ok(entries)
}

fn parse_non_negative_u64(value: &Object, name: &str) -> Result<u64> {
    let Object::Integer(integer) = value else {
        return Err(Error::parse(0, format!("{name} is not integer")));
    };
    if *integer < 0 {
        return Err(Error::parse(0, format!("{name} is negative")));
    }
    Ok(*integer as u64)
}

fn parse_usize(value: u64, name: &str) -> Result<usize> {
    usize::try_from(value).map_err(|_| Error::parse(0, format!("{name} does not fit usize")))
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

    fn read_be_u64(&mut self, width: usize) -> Result<u64> {
        if self.pos + width > self.bytes.len() {
            return Err(Error::parse(self.pos, "unexpected end of stream field"));
        }

        let mut value = 0u64;
        for _ in 0..width {
            value = (value << 8) | u64::from(self.bytes[self.pos]);
            self.pos += 1;
        }
        Ok(value)
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
