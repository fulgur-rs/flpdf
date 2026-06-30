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
    Free { next: u32 },
    Offset(u64),
    Compressed { stream: u32, index: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XrefForm {
    Table,
    Stream,
}

/// Load the cross-reference table and trailer dictionary from `reader`, with
/// the qpdf-style recovery pass disabled (strict parse).
///
/// # Errors
///
/// Calls [`load_xref_and_trailer_with_repair`] with repair disabled, so it
/// propagates the same errors that function raises when `allow_repair` is
/// `false`:
///
/// - [`Error::Io`] when reading the input fails.
/// - [`Error::Parse`] when the PDF header, `startxref`, or a cross-reference
///   section is malformed (including a `startxref`/`/Prev` offset that does not
///   fit `usize` and a circular `/Prev` chain).
/// - [`Error::Missing`] when a required cross-reference stream entry (such as
///   `/Size` or `/W`) is absent.
/// - [`Error::Unsupported`] when a cross-reference stream uses an unsupported
///   object or entry type.
pub fn load_xref_and_trailer<R: Read + Seek>(reader: &mut R) -> Result<LoadedXref> {
    load_xref_and_trailer_with_repair(reader, false)
}

/// Load the cross-reference table and trailer dictionary from `reader`, running
/// the qpdf-style recovery pass when `allow_repair` is `true`.
///
/// # Errors
///
/// - [`Error::Io`] when seeking or reading the input fails.
/// - [`Error::Parse`] when the PDF header is missing or its version is not
///   UTF-8 (this check runs before any repair fallback). When `allow_repair`
///   is `false`, also when `startxref`, a cross-reference table or stream, or a
///   `/Prev` chain is malformed (including offsets that do not fit `usize` and a
///   circular `/Prev` chain). When `allow_repair` is `true`, such failures are
///   recorded as diagnostics and recovered by a linear scan, which itself
///   raises [`Error::Parse`] when no cross-reference entries can be recovered or
///   no `trailer` dictionary is found.
/// - [`Error::Missing`] when a required cross-reference stream entry (such as
///   `/Size` or `/W`) is absent and `allow_repair` is `false`.
/// - [`Error::Unsupported`] when a cross-reference stream uses an unsupported
///   object or entry type and `allow_repair` is `false`.
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
            // Report the first recorded failure; this parse error is only the
            // trigger when the startxref stage itself succeeded.
            let trigger = parse_errors.into_iter().next().unwrap_or(error);
            return recover_xref_from_linear_scan(&bytes, version, startxref, trigger);
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
            let trigger = parse_errors.into_iter().next().unwrap_or(error);
            return recover_xref_from_linear_scan(&bytes, version, startxref, trigger);
        }
        return Err(error);
    }

    if let Some(error) = parse_errors.into_iter().next() {
        push_repair_diagnostics(&mut loaded.repair_diagnostics, &error, startxref);
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
            if !entries
                .keys()
                .any(|entry_ref| entry_ref.number == object_ref.number)
            {
                entries.insert(object_ref, xref_offset);
            }
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
    trigger_error: Error,
) -> Result<LoadedXref> {
    let entries = recover_xref_entries(bytes)?;
    let trailer = recover_trailer(bytes)?;

    let mut repair_diagnostics = Diagnostics::default();
    push_repair_diagnostics(&mut repair_diagnostics, &trigger_error, startxref);

    Ok(LoadedXref {
        version,
        startxref,
        entries,
        trailer,
        last_xref_form: XrefForm::Table,
        repair_diagnostics,
    })
}

/// Load the cross-reference table and trailer dictionary from `reader`, with the
/// qpdf-style recovery pass always enabled (best-effort).
///
/// # Errors
///
/// Calls [`load_xref_and_trailer_with_repair`] with repair enabled, so
/// malformed cross-reference data is recovered rather than reported. It still
/// fails with:
///
/// - [`Error::Io`] when seeking or reading the input fails.
/// - [`Error::Parse`] when the PDF header is missing or its version is not
///   UTF-8, or when the linear-scan recovery cannot find any cross-reference
///   entries or a `trailer` dictionary.
pub fn load_xref_and_trailer_best_effort<R: Read + Seek>(reader: &mut R) -> Result<LoadedXref> {
    load_xref_and_trailer_with_repair(reader, true)
}

/// Recover uncompressed object offsets by replaying qpdf's `reconstruct_xref`
/// (`libqpdf/QPDF.cc`, qpdf 11.9.0): scan the file line by line, and on each line
/// whose first token sequence is `int int obj`, record the object at the offset of
/// its *number* token. Only the first token of a line is inspected, object bodies
/// are never parsed, and the last occurrence of an object in the file wins (qpdf's
/// `insertReconstructedXrefEntry` overwrites). Inspecting at most three short
/// tokens per line — never re-parsing a body to end-of-file — makes the scan
/// linear in the file size, unlike a per-candidate full-object parse which an
/// unterminated literal string can drive to quadratic cost.
fn recover_xref_entries(bytes: &[u8]) -> Result<BTreeMap<ObjectRef, XrefOffset>> {
    let mut entries = BTreeMap::new();
    let mut line_start = 0usize;
    while line_start < bytes.len() {
        let next_line_start = next_line_start(bytes, line_start);
        if let Some((object_ref, offset)) =
            scan_object_header_at_line(bytes, line_start, next_line_start)
        {
            entries.insert(object_ref, XrefOffset::Offset(offset));
        }
        line_start = next_line_start;
    }

    // qpdf records only uncompressed (type-1) entries during reconstruction and
    // declines to look inside object streams (`reconstruct_xref` trailing comment
    // in QPDF.cc). flpdf additionally recovers the objects packed in a recovered
    // `/Type /ObjStm` so they remain resolvable without a usable xref; this extra
    // pass is bounded per object to keep recovery linear (see below).
    recover_objstm_compressed_entries(bytes, &mut entries);

    if entries.is_empty() {
        return Err(Error::parse(
            0,
            "unable to recover xref entries by linear scan",
        ));
    }

    Ok(entries)
}

/// Upper bound on read-to-end fallbacks during ObjStm recovery (see
/// [`recover_objstm_compressed_entries`]). Each fallback may parse to end of
/// file, so the count is capped to keep the total work O(file size) while still
/// recovering a handful of object streams whose payloads happen to contain a
/// header-like line.
const MAX_OBJSTM_RECOVERY_FALLBACKS: u32 = 64;

/// Recover the compressed objects packed in any recovered `/Type /ObjStm`,
/// emitting `XrefOffset::Compressed` entries that point back at the stream.
///
/// Each recovered object is parsed within the window that ends at the next
/// recovered object's offset (or end-of-file for the last). The windows are
/// disjoint, so the common case is bounded by the file size — a malformed object
/// cannot drive the parse to end-of-file once per candidate. When a window does
/// not hold a complete object — a header-like line (`int int obj`) recorded
/// inside an object stream's payload became the next offset and truncated it —
/// it retries against the rest of the file so the stream's own `/Length`
/// delimits it. Those retries are capped by [`MAX_OBJSTM_RECOVERY_FALLBACKS`] so
/// a flood of stream-like candidates cannot reintroduce quadratic cost.
fn recover_objstm_compressed_entries(bytes: &[u8], entries: &mut BTreeMap<ObjectRef, XrefOffset>) {
    // The line scan only ever inserts `XrefOffset::Offset`, so every entry here is
    // an uncompressed object whose offset bounds a window.
    let mut offsets: Vec<u64> = Vec::new();
    for entry in entries.values() {
        if let XrefOffset::Offset(offset) = entry {
            offsets.push(*offset);
        }
    }
    offsets.sort_unstable();

    let mut fallbacks = MAX_OBJSTM_RECOVERY_FALLBACKS;
    for (index, &offset) in offsets.iter().enumerate() {
        let start = offset as usize;
        let window_end = offsets
            .get(index + 1)
            .map_or(bytes.len(), |next| *next as usize);
        if try_recover_objstm_in(entries, &bytes[start..window_end]) {
            continue;
        }
        // The bounded window stopped short of a complete object. Retry against
        // the rest of the file so a real ObjStm truncated by a header-like line
        // in its payload is still recovered, capped so it stays linear.
        if window_end < bytes.len() && fallbacks > 0 {
            fallbacks -= 1;
            try_recover_objstm_in(entries, &bytes[start..]);
        }
    }
}

/// Parse the indirect object in `slice`; if it is a `/Type /ObjStm`, insert its
/// packed objects' compressed entries. Returns `false` only when `slice` did not
/// contain a complete object (a parse error) — the signal that a bounded window
/// may have truncated a real stream and a wider retry is worthwhile.
fn try_recover_objstm_in(entries: &mut BTreeMap<ObjectRef, XrefOffset>, slice: &[u8]) -> bool {
    match parse_indirect_object(slice) {
        Ok((object_ref, Object::Stream(stream))) => {
            if let Some(Object::Name(type_name)) = stream.dict.get("Type") {
                if type_name.as_slice() == b"ObjStm" {
                    recover_compressed_offsets_from_objstm(entries, object_ref, &stream);
                }
            }
            true
        }
        Ok(_) => true,
        Err(_) => false,
    }
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

/// Push the qpdf-compatible repair warning sequence onto `diagnostics`.
///
/// qpdf (`reconstruct_xref` in `QPDF_objects.cc`, observed with qpdf 11.9.0)
/// emits the same three warnings regardless of how the damaged
/// cross-reference data is ultimately recovered: `file is damaged`, the error
/// that triggered recovery, and `Attempting to reconstruct cross-reference
/// table`. `trigger_error` is the first failure that initiated recovery;
/// subsequent failures from the retry-at-offset-0 detour are not reported
/// because qpdf has no such detour and they have no counterpart on its
/// stderr. The triggering error's warning carries that error's own byte
/// offset when available (falling back to the `startxref` offset); the
/// surrounding warnings carry no offset, matching qpdf, which reports them
/// at offset 0 and suppresses the display.
fn push_repair_diagnostics(diagnostics: &mut Diagnostics, trigger_error: &Error, startxref: u64) {
    diagnostics.push(Diagnostic::warning("file is damaged", None));
    let (message, offset) = match trigger_error {
        Error::Parse { offset, message } => (message.clone(), Some(*offset as u64)),
        other => (other.to_string(), Some(startxref)),
    };
    diagnostics.push(Diagnostic::warning(message, offset));
    diagnostics.push(Diagnostic::warning(
        "Attempting to reconstruct cross-reference table",
        None,
    ));
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

/// Return the offset just past the next end-of-line at or after `from`, or
/// `bytes.len()` when no further end-of-line exists. A run of consecutive
/// `\r`/`\n` bytes is treated as a single line terminator (mirroring qpdf's
/// `findAndSkipNextEOL`, which collapses `\r\n` and blank lines). When
/// `from < bytes.len()` the result is always strictly greater than `from`, so
/// the line scan in [`recover_xref_entries`] always makes progress.
fn next_line_start(bytes: &[u8], from: usize) -> usize {
    let mut pos = from;
    while pos < bytes.len() && !matches!(bytes[pos], b'\n' | b'\r') {
        pos += 1;
    }
    // Skip the run of end-of-line bytes so blank lines do not become their own
    // iterations; this keeps the scan linear by advancing `line_start` past the
    // whole run that a forward token read would otherwise re-scan. When no
    // end-of-line exists this loop is a no-op and `pos` is already `bytes.len()`.
    while pos < bytes.len() && matches!(bytes[pos], b'\n' | b'\r') {
        pos += 1;
    }
    pos
}

/// PDF whitespace: NUL, TAB, LF, FF, CR, and space (ISO 32000-2 Table 1).
fn is_pdf_whitespace(byte: u8) -> bool {
    matches!(byte, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

/// PDF delimiters plus `%` (comment introducer); any of these ends a regular
/// token (ISO 32000-2 Table 2).
fn is_pdf_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

/// Advance `pos` past PDF whitespace and `%...EOL` comments — the two ignorable
/// constructs allowed between tokens — but never beyond `limit`. qpdf's
/// tokenizer skips comments in this position too, so an object header with a
/// comment between its tokens (`1 %c<EOL>0 obj`) is still recovered.
fn skip_ignorable(bytes: &[u8], mut pos: usize, limit: usize) -> usize {
    loop {
        while pos < limit && is_pdf_whitespace(bytes[pos]) {
            pos += 1;
        }
        if pos < limit && bytes[pos] == b'%' {
            while pos < limit && !matches!(bytes[pos], b'\n' | b'\r') {
                pos += 1;
            }
        } else {
            return pos;
        }
    }
}

/// A token read by the reconstruction scanner, with its byte range. Only the
/// distinctions the `int int obj` match needs are modelled.
struct ScanToken {
    start: usize,
    end: usize,
    kind: ScanKind,
}

enum ScanKind {
    /// A numeric token (`[+-]?[0-9]+` that fits `i64`).
    Integer(i64),
    /// A non-numeric regular token (compared to `obj` via the byte range).
    Word,
    /// A delimiter-led token. The scanner stops as soon as a slot it expected to
    /// be `int`/`obj` is one of these.
    Delimiter,
}

/// Read the next token whose start lies in `[from, limit)`, skipping leading
/// whitespace and `%...EOL` comments. Returns `None` when no token starts before
/// `limit` (the region is only whitespace/comments).
///
/// `limit` bounds where the token may *start*, not where it ends — a regular
/// token still runs to the next whitespace/delimiter. Passing `next_line_start`
/// for the first token keeps the scan linear: a whitespace- or comment-only line
/// is rejected after scanning only its own bytes, instead of skipping forward
/// into later lines and re-scanning that suffix on every iteration (an O(n^2)
/// blowup). Later tokens pass `bytes.len()` so an `int int obj` header may span
/// lines, matching qpdf.
fn read_scan_token(bytes: &[u8], from: usize, limit: usize) -> Option<ScanToken> {
    let start = skip_ignorable(bytes, from, limit);
    if start >= limit {
        return None;
    }
    let first = bytes[start];
    if is_pdf_delimiter(first) {
        return Some(ScanToken {
            start,
            end: start + 1,
            kind: ScanKind::Delimiter,
        });
    }

    let mut pos = start;
    while pos < bytes.len() && !is_pdf_whitespace(bytes[pos]) && !is_pdf_delimiter(bytes[pos]) {
        pos += 1;
    }
    let end = pos;
    let kind = match parse_scan_integer(&bytes[start..end]) {
        Some(value) => ScanKind::Integer(value),
        None => ScanKind::Word,
    };
    Some(ScanToken { start, end, kind })
}

/// Classify a regular token as an unsigned PDF integer (`[0-9]+` that fits
/// `i64`). Returns `None` for any token containing a non-digit (a word such as
/// `obj`) or a digit run too long to fit `i64`. Object and generation numbers in
/// real PDFs are unsigned, so a leading sign is treated as a word; this only ever
/// changes the outcome for synthetic `+N`/`-N` headers, which qpdf's
/// `obj > 0`/`gen >= 0` guards reject anyway.
fn parse_scan_integer(token: &[u8]) -> Option<i64> {
    if token.is_empty() || !token.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(token).ok()?.parse().ok()
}

/// If the line beginning at `line_start` opens with an `int int obj` token
/// sequence, return the recovered object and the offset of its number token.
///
/// Mirrors qpdf's `reconstruct_xref` per-line logic: the first token must begin
/// on this line (otherwise the line records nothing — qpdf's
/// `token_start >= next_line_start` guard, here enforced by bounding the first
/// token read to `next_line_start`), the second and third tokens may spill onto
/// following lines, and the object/generation must satisfy qpdf's
/// `insertReconstructedXrefEntry` guards (`obj > 0`, `0 <= gen < 65535`).
fn scan_object_header_at_line(
    bytes: &[u8],
    line_start: usize,
    next_line_start: usize,
) -> Option<(ObjectRef, u64)> {
    // Bounding the first token to this line is what keeps a whitespace- or
    // comment-only line from re-scanning the remaining file on every iteration.
    let number_token = read_scan_token(bytes, line_start, next_line_start)?;
    let ScanKind::Integer(obj) = number_token.kind else {
        return None;
    };

    let gen_token = read_scan_token(bytes, number_token.end, bytes.len())?;
    let ScanKind::Integer(gen) = gen_token.kind else {
        return None;
    };

    let obj_token = read_scan_token(bytes, gen_token.end, bytes.len())?;
    if !matches!(obj_token.kind, ScanKind::Word) || &bytes[obj_token.start..obj_token.end] != b"obj"
    {
        return None;
    }

    // qpdf's `insertReconstructedXrefEntry` guards (`obj > 0`, `0 <= gen < 65535`).
    if obj <= 0 || !(0..65535).contains(&gen) {
        return None;
    }
    let number = u32::try_from(obj).ok()?;
    let generation = u16::try_from(gen).ok()?;
    Some((
        ObjectRef::new(number, generation),
        number_token.start as u64,
    ))
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
            match in_use {
                b'f' => {
                    entries.insert(
                        ObjectRef::new(first + index, generation),
                        XrefOffset::Free {
                            next: u32::try_from(offset).map_err(|_| {
                                Error::parse(0, "free xref next object does not fit u32")
                            })?,
                        },
                    );
                }
                b'n' => {
                    entries.insert(
                        ObjectRef::new(first + index, generation),
                        XrefOffset::Offset(offset),
                    );
                }
                _ => return Err(Error::parse(0, "xref table entry status is not f or n")),
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
    let tail = bytes
        .get(xref_pos..)
        .filter(|slice| !slice.is_empty())
        .ok_or_else(|| Error::parse(xref_pos, "xref stream offset is beyond end of file"))?;
    let (_, object) = parse_indirect_object(tail).map_err(|err| err.rebase_offset(xref_pos))?;
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
                0 => {
                    let next = u32::try_from(field1)
                        .map_err(|_| Error::parse(0, "free xref next object does not fit u32"))?;
                    let generation = u16::try_from(field2)
                        .map_err(|_| Error::parse(0, "generation does not fit u16"))?;
                    entries.insert(
                        ObjectRef::new(object_number, generation),
                        XrefOffset::Free { next },
                    );
                }
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
        return Err(Error::parse(bytes.len(), "can't find startxref"));
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
