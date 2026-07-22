//! qpdf JSON v2 inspection builders.
//!
//! Provides the structural frame for qpdf `--json` output.  Each builder
//! returns a [`JsonValue`] that the caller can extend with per-section data
//! (pages, objects, …) in later subtasks.

use crate::json::JsonValue;
use crate::object::{Dictionary, Object, ObjectRef, Stream};
use crate::reader::Pdf;
use std::borrow::Cow;
use std::io::{Read, Seek};

// ── ConvertError ──────────────────────────────────────────────────────────────

/// Errors that can occur when converting PDF objects to JSON values.
#[derive(Debug, Clone, PartialEq)]
pub enum ConvertError {
    /// A non-finite float (NaN or infinity) was encountered.
    NonFiniteFloat,
    /// An underlying PDF read/parse error.
    PdfError(String),
}

impl std::fmt::Display for ConvertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConvertError::NonFiniteFloat => {
                write!(f, "non-finite float cannot be serialized as JSON")
            }
            ConvertError::PdfError(msg) => write!(f, "PDF error: {msg}"),
        }
    }
}

impl std::error::Error for ConvertError {}

impl From<crate::Error> for ConvertError {
    fn from(err: crate::Error) -> Self {
        ConvertError::PdfError(err.to_string())
    }
}

// ── pdf_object_to_json ────────────────────────────────────────────────────────

/// PDFDocEncoding lookup table per ISO 32000-1 Annex D.3.
///
/// `None` means the code point is unassigned and the string cannot be safely
/// decoded as PDFDocEncoding text.  Most byte values map to the corresponding
/// ISO 8859-1 character; the 0x18..=0x1F and 0x80..=0x9F ranges are
/// PDF-specific additions.
const PDFDOC_ENCODING: [Option<char>; 256] = build_pdfdoc_table();

const fn build_pdfdoc_table() -> [Option<char>; 256] {
    let mut table: [Option<char>; 256] = [None; 256];
    // 0x08..=0x0D: standard control codes
    table[0x08] = Some('\u{0008}');
    table[0x09] = Some('\t');
    table[0x0A] = Some('\n');
    table[0x0B] = Some('\u{000B}');
    table[0x0C] = Some('\u{000C}');
    table[0x0D] = Some('\r');
    // 0x18..=0x1F: PDF-specific accents
    table[0x18] = Some('\u{02D8}'); // ˘ BREVE
    table[0x19] = Some('\u{02C7}'); // ˇ CARON
    table[0x1A] = Some('\u{02C6}'); // ˆ MODIFIER LETTER CIRCUMFLEX ACCENT
    table[0x1B] = Some('\u{02D9}'); // ˙ DOT ABOVE
    table[0x1C] = Some('\u{02DD}'); // ˝ DOUBLE ACUTE ACCENT
    table[0x1D] = Some('\u{02DB}'); // ˛ OGONEK
    table[0x1E] = Some('\u{02DA}'); // ˚ RING ABOVE
    table[0x1F] = Some('\u{02DC}'); // ˜ SMALL TILDE
                                    // 0x20..=0x7E: same as ASCII printable
    let mut b = 0x20u8;
    while b <= 0x7E {
        table[b as usize] = Some(b as char);
        b += 1;
    }
    // 0x80..=0x9F: PDF-specific symbols
    table[0x80] = Some('\u{2022}'); // BULLET
    table[0x81] = Some('\u{2020}'); // DAGGER
    table[0x82] = Some('\u{2021}'); // DOUBLE DAGGER
    table[0x83] = Some('\u{2026}'); // HORIZONTAL ELLIPSIS
    table[0x84] = Some('\u{2014}'); // EM DASH
    table[0x85] = Some('\u{2013}'); // EN DASH
    table[0x86] = Some('\u{0192}'); // LATIN SMALL LETTER F WITH HOOK
    table[0x87] = Some('\u{2044}'); // FRACTION SLASH
    table[0x88] = Some('\u{2039}'); // SINGLE LEFT-POINTING ANGLE QUOTATION MARK
    table[0x89] = Some('\u{203A}'); // SINGLE RIGHT-POINTING ANGLE QUOTATION MARK
    table[0x8A] = Some('\u{2212}'); // MINUS SIGN
    table[0x8B] = Some('\u{2030}'); // PER MILLE SIGN
    table[0x8C] = Some('\u{201E}'); // DOUBLE LOW-9 QUOTATION MARK
    table[0x8D] = Some('\u{201C}'); // LEFT DOUBLE QUOTATION MARK
    table[0x8E] = Some('\u{201D}'); // RIGHT DOUBLE QUOTATION MARK
    table[0x8F] = Some('\u{2018}'); // LEFT SINGLE QUOTATION MARK
    table[0x90] = Some('\u{2019}'); // RIGHT SINGLE QUOTATION MARK
    table[0x91] = Some('\u{201A}'); // SINGLE LOW-9 QUOTATION MARK
    table[0x92] = Some('\u{2122}'); // TRADE MARK SIGN
    table[0x93] = Some('\u{FB01}'); // LATIN SMALL LIGATURE FI
    table[0x94] = Some('\u{FB02}'); // LATIN SMALL LIGATURE FL
    table[0x95] = Some('\u{0141}'); // LATIN CAPITAL LETTER L WITH STROKE
    table[0x96] = Some('\u{0152}'); // LATIN CAPITAL LIGATURE OE
    table[0x97] = Some('\u{0160}'); // LATIN CAPITAL LETTER S WITH CARON
    table[0x98] = Some('\u{0178}'); // LATIN CAPITAL LETTER Y WITH DIAERESIS
    table[0x99] = Some('\u{017D}'); // LATIN CAPITAL LETTER Z WITH CARON
    table[0x9A] = Some('\u{0131}'); // LATIN SMALL LETTER DOTLESS I
    table[0x9B] = Some('\u{0142}'); // LATIN SMALL LETTER L WITH STROKE
    table[0x9C] = Some('\u{0153}'); // LATIN SMALL LIGATURE OE
    table[0x9D] = Some('\u{0161}'); // LATIN SMALL LETTER S WITH CARON
    table[0x9E] = Some('\u{017E}'); // LATIN SMALL LETTER Z WITH CARON
                                    // 0x9F: unassigned
    table[0xA0] = Some('\u{20AC}'); // EURO SIGN
                                    // 0xA1..=0xFF: same as ISO 8859-1 (Latin-1 Supplement)
    let mut b = 0xA1u8;
    loop {
        table[b as usize] = Some(b as char);
        if b == 0xFF {
            break;
        }
        b += 1;
    }
    table
}

/// Match qpdf `QPDF_String::getUTF8Val`: UTF-16 BOM, explicit UTF-8 BOM,
/// otherwise PDFDocEncoding with U+FFFD for undefined entries.
///
/// The return type preserves qpdf's `std::string` bytes: in particular, bytes
/// after an explicit UTF-8 BOM are returned verbatim even when they are not
/// valid UTF-8.
pub(crate) fn qpdf_utf8_value(bytes: &[u8]) -> Vec<u8> {
    if let Some(rest) = bytes.strip_prefix(&[0xfe, 0xff]) {
        return lossy_utf16_to_utf8(rest, false).into_bytes();
    }
    if let Some(rest) = bytes.strip_prefix(&[0xff, 0xfe]) {
        return lossy_utf16_to_utf8(rest, true).into_bytes();
    }
    if let Some(rest) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        return rest.to_vec();
    }

    bytes
        .iter()
        .map(|&byte| match byte {
            0x7f | 0x9f | 0xad => '\u{fffd}',
            _ => PDFDOC_ENCODING[byte as usize].unwrap_or(byte as char),
        })
        .collect::<String>()
        .into_bytes()
}

/// Decode a PDF text string (ISO 32000-1 §7.9.2) into a Rust `String`.
///
/// Returns `Some(text)` when the byte sequence is valid as a PDF text string
/// (UTF-16BE/UTF-16LE BOM-prefixed UTF-16, or PDFDocEncoding-mapped bytes).
/// Returns `None` when the byte sequence cannot be safely interpreted as
/// text — at which point the caller falls back to the `b:` hex representation.
///
/// `pub(crate)` so other modules (e.g. `attachment_list`) reuse this single
/// canonical PDFDocEncoding/UTF-16 decoder instead of duplicating it.
pub(crate) fn decode_pdf_text_string(bytes: &[u8]) -> Option<String> {
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        if rest.len() % 2 != 0 {
            return None;
        }
        let units = rest
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]));
        return char::decode_utf16(units)
            .collect::<Result<String, _>>()
            .ok();
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        if rest.len() % 2 != 0 {
            return None;
        }
        let units = rest
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]));
        return char::decode_utf16(units)
            .collect::<Result<String, _>>()
            .ok();
    }
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        out.push(PDFDOC_ENCODING[b as usize]?);
    }
    Some(out)
}

/// Heuristic that mirrors qpdf's `QPDF_String::useHexString()` (libqpdf
/// `QPDF_String.cc`): returns true when the PDF string contains enough
/// non-printable or non-ASCII bytes that qpdf would emit it as a
/// `b:<hex>` blob in JSON v2 output rather than attempting a PDFDocEncoding
/// round-trip.
///
/// Rules (with byte values expressed as unsigned `u8` — qpdf uses signed
/// `char` arithmetic but the semantics are identical):
/// - `0x20..=0x7E` (printable ASCII): considered plain, contributes nothing.
/// - `0x18..=0x1F`, `0x7F`, `0x80..=0xFF`: count as `non_ascii`.
/// - `\n \r \t \b \f` (`0x08 0x09 0x0A 0x0C 0x0D`): considered plain.
/// - Any other byte below `0x20` (e.g. NUL, 0x01, 0x0B, 0x0E..0x17):
///   short-circuit and force hex.
///
/// After the scan, hex is used when `5 * non_ascii > len` — i.e. when more
/// than 20% of the bytes are non-ASCII / control symbols.
fn use_hex_string(bytes: &[u8]) -> bool {
    let mut non_ascii: usize = 0;
    for &b in bytes {
        match b {
            0x20..=0x7E => continue,
            0x18..=0x1F | 0x7F | 0x80..=0xFF => non_ascii += 1,
            0x08 | 0x09 | 0x0A | 0x0C | 0x0D => continue,
            _ => return true,
        }
    }
    5 * non_ascii > bytes.len()
}

/// Lossy UTF-16 → UTF-8 decoder matching `QUtil::utf16_to_utf8` in qpdf.
///
/// A trailing odd byte is silently ignored; a high surrogate without a
/// following low surrogate is dropped; a low surrogate without a preceding
/// high surrogate yields its low 10 bits as a codepoint. Invalid scalar
/// values (e.g. lone surrogates surviving the above) are silently skipped
/// via [`char::from_u32`]. This intentionally mirrors qpdf so that JSON v2
/// output stays byte-identical for fixtures containing the same input.
///
/// `is_le` selects little-endian byte order (BOM `0xFF 0xFE`) versus
/// big-endian (`0xFE 0xFF`); the caller is expected to have stripped the
/// BOM before calling.
fn lossy_utf16_to_utf8(bytes: &[u8], is_le: bool) -> String {
    // bytes.len() is a sound capacity hint: each UTF-16 unit is 2 bytes and
    // expands to 1–3 UTF-8 bytes (4 only for surrogate pairs, which consume
    // 4 UTF-16 bytes). For ASCII-dominant inputs this slightly over-allocates;
    // for BMP-heavy inputs it is roughly accurate.
    let mut out = String::with_capacity(bytes.len());
    let mut codepoint: u32 = 0;
    let mut i = 0;
    while i + 1 < bytes.len() {
        let (msb_idx, lsb_idx) = if is_le { (i + 1, i) } else { (i, i + 1) };
        let bits = (u16::from(bytes[msb_idx]) << 8) | u16::from(bytes[lsb_idx]);
        match bits & 0xFC00 {
            0xD800 => {
                codepoint = 0x10000 + ((u32::from(bits) & 0x3FF) << 10);
                i += 2;
                continue;
            }
            0xDC00 => {
                codepoint += u32::from(bits) & 0x3FF;
            }
            _ => {
                codepoint = u32::from(bits);
            }
        }
        if let Some(c) = char::from_u32(codepoint) {
            out.push(c);
        }
        codepoint = 0;
        i += 2;
    }
    out
}

/// Classify a PDF string as either a `u:` text string or `b:` binary string
/// using the same decision tree as qpdf's `QPDF_String::writeJSON` (JSON v2).
///
/// The order of checks mirrors qpdf's `libqpdf/QPDF_String.cc` exactly:
/// 1. UTF-16 BOM (`FE FF` BE or `FF FE` LE): decode lossily and emit
///    `u:<utf8>`. Matches `util::is_utf16` + `QUtil::utf16_to_utf8`.
/// 2. UTF-8 BOM (`EF BB BF`): emit `u:<rest>` for the substring after the
///    BOM. qpdf trusts the BOM without re-validating UTF-8 — we additionally
///    require `std::str::from_utf8` to succeed so we never emit invalid
///    UTF-8 ourselves.
/// 3. Run [`use_hex_string`]; if it returns `false`, attempt PDFDocEncoding
///    decode. A successful decode is equivalent to qpdf's
///    `utf8_to_pdf_doc(...)` round-trip because our
///    [`decode_pdf_text_string`] returns `None` for any byte without a
///    1-to-1 PDFDoc mapping — so decode-success implies round-trip-success.
/// 4. Otherwise emit `b:<hex>` (lowercase).
fn pdf_string_to_json_string(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return format!("u:{}", lossy_utf16_to_utf8(rest, false));
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return format!("u:{}", lossy_utf16_to_utf8(rest, true));
    }
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        if let Ok(s) = std::str::from_utf8(rest) {
            return format!("u:{s}");
        }
    }
    if !use_hex_string(bytes) {
        // PDFDocEncoding decode-success ⇒ 1-to-1 mapping ⇒ no separate
        // round-trip check needed (see [`decode_pdf_text_string`]).
        if let Some(text) = decode_pdf_text_string(bytes) {
            return format!("u:{text}");
        }
    }
    // Hex-encode with a single allocation: "b:" prefix + 2 nibbles per byte.
    // Avoids the per-byte format!() allocation of the previous implementation.
    let mut out = String::with_capacity(2 + bytes.len() * 2);
    out.push_str("b:");
    for &b in bytes {
        out.push(char::from_digit(u32::from(b >> 4), 16).expect("nibble < 16"));
        out.push(char::from_digit(u32::from(b & 0xf), 16).expect("nibble < 16"));
    }
    out
}

/// Encode a PDF name byte sequence into a `/NAME` JSON string using the
/// PDF/qpdf `#XX` escape rules (ISO 32000-1 §7.3.5).
///
/// Every byte outside printable ASCII (`0x21..=0x7E`) — and every PDF
/// delimiter (`( ) < > [ ] { } / %`) and the `#` character itself — is
/// emitted as `#hh` with lowercase hex. This is lossless: round-tripping
/// the result back through the PDF name parser yields the original bytes.
fn encode_pdf_name_bytes(bytes: &[u8]) -> String {
    fn is_safe(b: u8) -> bool {
        if !(0x21..=0x7E).contains(&b) {
            return false;
        }
        !matches!(
            b,
            b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%' | b'#'
        )
    }

    let mut out = String::with_capacity(bytes.len() + 1);
    out.push('/');
    for &b in bytes {
        if is_safe(b) {
            out.push(b as char);
        } else {
            use std::fmt::Write as _;
            // safe to unwrap: write! to String is infallible
            let _ = write!(out, "#{b:02x}");
        }
    }
    out
}

/// Convert a PDF [`Dictionary`] to a JSON object, with keys sorted alphabetically
/// (with the `/` prefix included in the sort key).
fn dict_to_json(dict: &Dictionary) -> Result<JsonValue, ConvertError> {
    // Dictionary::iter() already yields entries in lexicographic order of raw
    // bytes (BTreeMap). We encode each key losslessly using the PDF name
    // escape rules so that names containing delimiters, whitespace, `#`, or
    // non-UTF8 bytes round-trip without information loss.
    let mut pairs = Vec::new();
    for (raw_key, value) in dict.iter() {
        let key_str = encode_pdf_name_bytes(raw_key);
        let json_val = pdf_object_to_json(value)?;
        pairs.push((key_str, json_val));
    }
    // Sort by the escaped "/Name" string so the lexicographic order is stable
    // across runs and matches qpdf's alphabetical output.
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(JsonValue::Object(pairs))
}

fn qpdf_reference_is_valid(reference: ObjectRef) -> bool {
    reference.number > 0 && reference.generation < u16::MAX
}

fn qpdf_reference_resolves_to_null<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    reference: ObjectRef,
) -> Result<bool, ConvertError> {
    if !qpdf_reference_is_valid(reference) {
        return Ok(true);
    }
    let mut current = reference;
    let mut visited = std::collections::BTreeSet::new();
    loop {
        if !visited.insert(current) {
            return Ok(true);
        }
        match pdf.resolve(current)? {
            Object::Null => return Ok(true),
            Object::Reference(next) => current = next,
            _ => return Ok(false),
        }
    }
}

fn qpdf_dict_to_json<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict: &Dictionary,
) -> Result<JsonValue, ConvertError> {
    let mut pairs = Vec::new();
    for (raw_key, value) in dict.iter() {
        let omit = match value {
            Object::Null => true,
            Object::Reference(reference) => qpdf_reference_resolves_to_null(pdf, *reference)?,
            _ => false,
        };
        if omit {
            continue;
        }
        pairs.push((
            encode_pdf_name_bytes(raw_key),
            qpdf_pdf_object_to_json(pdf, value)?,
        ));
    }
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(JsonValue::Object(pairs))
}

fn qpdf_pdf_object_to_json<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    object: &Object,
) -> Result<JsonValue, ConvertError> {
    match object {
        Object::Reference(reference) if !qpdf_reference_is_valid(*reference) => Ok(JsonValue::Null),
        Object::Reference(reference) => Ok(JsonValue::String(format!(
            "{} {} R",
            reference.number, reference.generation
        ))),
        Object::Array(items) => items
            .iter()
            .map(|item| qpdf_pdf_object_to_json(pdf, item))
            .collect::<Result<Vec<_>, _>>()
            .map(JsonValue::Array),
        Object::Dictionary(dict) => qpdf_dict_to_json(pdf, dict),
        Object::Stream(stream) => Ok(JsonValue::Object(vec![(
            "stream".to_string(),
            JsonValue::Object(vec![(
                "dict".to_string(),
                qpdf_dict_to_json(pdf, &stream.dict)?,
            )]),
        )])),
        other => pdf_object_to_json(other),
    }
}

fn qpdf_resolve_top_level_object<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    start: ObjectRef,
) -> Result<Object, ConvertError> {
    let mut current = start;
    let mut visited = std::collections::BTreeSet::new();
    loop {
        if !visited.insert(current) {
            return Ok(Object::Null);
        }
        match pdf.resolve(current)? {
            Object::Reference(next) => current = next,
            terminal => return Ok(terminal),
        }
    }
}

/// Convert a stream's dict to a JSON stream-shape object: `{ "stream": { "dict": ... } }`.
fn stream_to_json(stream: &Stream) -> Result<JsonValue, ConvertError> {
    let dict_json = dict_to_json(&stream.dict)?;
    Ok(JsonValue::Object(vec![(
        "stream".to_string(),
        JsonValue::Object(vec![("dict".to_string(), dict_json)]),
    )]))
}

/// Convert a PDF object into the qpdf v2 JSON value form.
///
/// For streams, only the dictionary part is included; the stream data body is
/// handled through a separate path. A stream nested inside another object also
/// returns `{"stream":{"dict":...}}`.
///
/// # Errors
///
/// Returns [`ConvertError::NonFiniteFloat`] when a [`Object::Real`] value is
/// non-finite (NaN or infinity).
pub fn pdf_object_to_json(obj: &Object) -> Result<JsonValue, ConvertError> {
    match obj {
        Object::Null => Ok(JsonValue::Null),
        Object::Boolean(b) => Ok(JsonValue::Bool(*b)),
        Object::Integer(n) => Ok(JsonValue::Integer(*n)),
        Object::Real(f) | Object::RealLiteral { value: f, .. } => {
            if !f.is_finite() {
                return Err(ConvertError::NonFiniteFloat);
            }
            Ok(JsonValue::Float(*f))
        }
        Object::Name(bytes) => Ok(JsonValue::String(encode_pdf_name_bytes(bytes))),
        Object::String(bytes) => Ok(JsonValue::String(pdf_string_to_json_string(bytes))),
        Object::Reference(r) => Ok(JsonValue::String(format!(
            "{} {} R",
            r.number, r.generation
        ))),
        Object::Array(elems) => {
            let values: Result<Vec<JsonValue>, ConvertError> =
                elems.iter().map(pdf_object_to_json).collect();
            Ok(JsonValue::Array(values?))
        }
        Object::Dictionary(dict) => dict_to_json(dict),
        // Stream nested inside a container — not spec-compliant, but handle safely.
        Object::Stream(stream) => stream_to_json(stream),
    }
}

// ── QpdfMetadata ─────────────────────────────────────────────────────────────

/// Metadata for the `qpdf` top-level key's first element.
pub struct QpdfMetadata {
    /// PDF version header (e.g. `"1.3"`).
    pub pdf_version: String,
    /// Maximum object id seen in this document.
    pub max_object_id: u32,
    /// Whether inherited page resources were pushed into page dictionaries.
    pub pushed_inherited_page_resources: bool,
    /// Whether the document's complete page tree was enumerated before emission.
    pub called_get_all_pages: bool,
    // jsonversion is always 2 in v2 output.
}

// ── StreamDataMode ────────────────────────────────────────────────────────────

/// Controls how stream payloads are emitted in the qpdf JSON v2 output.
///
/// Applies to each `obj:N M R` entry in the `qpdf` top-level key when the
/// resolved object is a Stream.
#[derive(Debug, Clone, Default)]
pub enum StreamDataMode {
    /// Emit only the dict (default). The `stream` entry is `{ "dict": ... }`.
    #[default]
    None,
    /// Emit the raw stream bytes as a base64 string under `data`.
    /// Yields `{ "stream": { "data": "<base64>", "dict": ... } }`.
    Inline,
    /// Emit a side-file path under `datafile`.
    /// Yields `{ "stream": { "datafile": "<prefix>-<obj_num>", "dict": ... } }`.
    /// The CLI is responsible for actually writing the bytes; this layer only
    /// computes the file name from `prefix` + the object number.
    File { prefix: String },
}

/// Format the side-file path for a `File`-mode stream entry.
///
/// qpdf 11.9.0 names side files `<prefix>-<obj_num>` — the bare object
/// number with no zero-padding. Centralized here so the JSON `datafile`
/// value and the CLI side-file writer always produce the same name.
pub fn format_json_side_file_path(prefix: &str, obj_num: u32) -> String {
    format!("{prefix}-{obj_num}")
}

// ── base64_encode ─────────────────────────────────────────────────────────────

/// Encode `bytes` as a standard Base64 string (RFC 4648, with `=` padding).
///
/// No external dependencies: ~30 lines of pure Rust.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let combined = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((combined >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((combined >> 12) & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[((combined >> 6) & 0x3F) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(combined & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
}

// ── build_qpdf_key ────────────────────────────────────────────────────────────

/// Build the contents of the top-level `qpdf` key (`[metadata, objects_map]`).
///
/// Returns a [`JsonValue::Array`] of exactly two elements:
/// 1. The metadata object with fixed key order.
/// 2. The objects map with all indirect objects and the trailer, sorted alphabetically
///    by key.
///
/// This is a thin wrapper around [`build_qpdf_key_with_stream_mode`] using
/// [`StreamDataMode::None`] (the default — stream entries contain `dict` only).
///
/// # Errors
///
/// Returns a [`ConvertError`] if any object cannot be converted to JSON.
pub fn build_qpdf_key<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    metadata: QpdfMetadata,
) -> Result<JsonValue, ConvertError> {
    // StreamDataMode::None emits dict only, so the decode level is irrelevant.
    build_qpdf_key_with_stream_mode(
        pdf,
        metadata,
        DecodeLevel::Generalized,
        &StreamDataMode::None,
    )
}

/// Like [`build_qpdf_key`], but accepts a [`StreamDataMode`] that controls
/// whether each `obj:N M R` stream entry includes `data` (Inline) or
/// `datafile` (File) alongside `dict`, plus a [`DecodeLevel`] that controls
/// how the `Inline` payload is decoded (see [`stream_payload_for_decode_level`]).
///
/// # Stream entry shapes
///
/// - `None`   → `{ "stream": { "dict": ... } }`
/// - `Inline` → `{ "stream": { "data": "<base64>", "dict": ... } }`
/// - `File`   → `{ "stream": { "datafile": "<prefix>-<obj_num>", "dict": ... } }`
///
/// For `Inline`, `decode_level` selects between the raw filter-encoded bytes
/// (`DecodeLevel::None`) and the filter-decoded content (any other level),
/// matching `qpdf --json-stream-data=inline --decode-level=...`. `File` mode
/// emits only the side-file path here; the caller writes the bytes and must
/// apply the same `decode_level` (see [`stream_payload_for_decode_level`]).
///
/// # Errors
///
/// Returns a [`ConvertError`] if any object cannot be converted to JSON.
pub fn build_qpdf_key_with_stream_mode<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    metadata: QpdfMetadata,
    decode_level: DecodeLevel,
    stream_mode: &StreamDataMode,
) -> Result<JsonValue, ConvertError> {
    let prepared = pdf.prepare_qpdf_json_objects()?;
    build_qpdf_key_selected_with_stream_mode(
        pdf,
        metadata,
        decode_level,
        stream_mode,
        &[],
        &prepared.refs,
    )
}

fn build_qpdf_key_selected_with_stream_mode<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    metadata: QpdfMetadata,
    decode_level: DecodeLevel,
    stream_mode: &StreamDataMode,
    selectors: &[JsonObjectSelector],
    prepared_refs: &[ObjectRef],
) -> Result<JsonValue, ConvertError> {
    // ── 1. Build metadata object (fixed key order per qpdf v2 spec) ────────
    let meta = JsonValue::Object(vec![
        ("jsonversion".to_string(), JsonValue::Integer(2)),
        (
            "pdfversion".to_string(),
            JsonValue::String(metadata.pdf_version.clone()),
        ),
        (
            "pushedinheritedpageresources".to_string(),
            JsonValue::Bool(metadata.pushed_inherited_page_resources),
        ),
        (
            "calledgetallpages".to_string(),
            JsonValue::Bool(metadata.called_get_all_pages),
        ),
        (
            "maxobjectid".to_string(),
            JsonValue::Integer(i64::from(metadata.max_object_id)),
        ),
    ]);

    // ── 2. Build objects_map ───────────────────────────────────────────────
    // `prepared_refs` contains every live object plus qpdf-style placeholders
    // for valid references whose exact generation is absent. Both a placeholder
    // and a live indirect null are emitted as `{ "value": null }`.
    let mut map_pairs: Vec<(String, JsonValue)> = Vec::new();

    for &oref in prepared_refs {
        let selected = selectors.is_empty()
            || selectors.iter().any(|selector| {
                matches!(
                    selector,
                    JsonObjectSelector::Object { number, generation }
                        if *number == oref.number && *generation == oref.generation
                )
            });
        if !selected {
            continue;
        }
        let key = format!("obj:{} {} R", oref.number, oref.generation);
        let obj = qpdf_resolve_top_level_object(pdf, oref)?;
        let json_val = match &obj {
            Object::Stream(stream) => {
                // Stream: emit according to stream_mode.
                let dict_json = qpdf_dict_to_json(pdf, &stream.dict)?;
                let stream_inner = match stream_mode {
                    StreamDataMode::None => {
                        // Default: dict only.
                        JsonValue::Object(vec![("dict".to_string(), dict_json)])
                    }
                    StreamDataMode::Inline => {
                        // Encode the stream payload as base64 under "data".
                        // The payload is decoded per `decode_level` so that
                        // Inline output matches `qpdf --decode-level=...`.
                        // Key order: data, dict (alphabetical).
                        let payload = stream_payload_for_decode_level(stream, decode_level);
                        let data_str = base64_encode(&payload);
                        JsonValue::Object(vec![
                            ("data".to_string(), JsonValue::String(data_str)),
                            ("dict".to_string(), dict_json),
                        ])
                    }
                    StreamDataMode::File { prefix } => {
                        // Emit a side-file path under "datafile".
                        // Key order: datafile, dict (alphabetical).
                        let datafile = format_json_side_file_path(prefix, oref.number);
                        JsonValue::Object(vec![
                            ("datafile".to_string(), JsonValue::String(datafile)),
                            ("dict".to_string(), dict_json),
                        ])
                    }
                };
                JsonValue::Object(vec![("stream".to_string(), stream_inner)])
            }
            other => {
                // Non-stream (including live Object::Null): emit { "value": <json> }.
                let val = qpdf_pdf_object_to_json(pdf, other)?;
                JsonValue::Object(vec![("value".to_string(), val)])
            }
        };
        map_pairs.push((key, json_val));
    }

    // ── 3. Add trailer ─────────────────────────────────────────────────────
    if selectors.is_empty()
        || selectors
            .iter()
            .any(|selector| matches!(selector, JsonObjectSelector::Trailer))
    {
        let trailer_dict = pdf.trailer().clone();
        let trailer_json = qpdf_dict_to_json(pdf, &trailer_dict)?;
        let trailer_val = JsonValue::Object(vec![("value".to_string(), trailer_json)]);
        map_pairs.push(("trailer".to_string(), trailer_val));
    }

    // ── 4. Sort objects_map alphabetically by key ──────────────────────────
    map_pairs.sort_by(|a, b| a.0.cmp(&b.0));

    let objects_map = JsonValue::Object(map_pairs);

    Ok(JsonValue::Array(vec![meta, objects_map]))
}

// ── DecodeLevel ──────────────────────────────────────────────────────────────

/// Controls which stream filters are applied when reading PDF streams.
///
/// Maps directly to the `decodelevel` field in qpdf JSON v2 output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DecodeLevel {
    /// Do not decode any streams.
    None,
    /// Decode generalized filters (FlateDecode, ASCII85Decode, etc.).
    /// This is the default.
    #[default]
    Generalized,
    /// Decode generalized and specialized filters (LZWDecode, RunLengthDecode,
    /// etc.).
    Specialized,
    /// Decode all streams including lossy filters (DCTDecode, JPXDecode, etc.).
    All,
}

impl DecodeLevel {
    /// Return the lowercase string representation used by qpdf.
    pub fn as_qpdf_str(&self) -> &'static str {
        match self {
            DecodeLevel::None => "none",
            DecodeLevel::Generalized => "generalized",
            DecodeLevel::Specialized => "specialized",
            DecodeLevel::All => "all",
        }
    }
}

// ── stream_payload_for_decode_level ──────────────────────────────────────────

/// Return the stream payload bytes to emit for a given [`DecodeLevel`].
///
/// `stream.data` is assumed to be the resolved (decrypted, but still
/// filter-encoded) bytes returned by [`Pdf::resolve`](crate::reader::Pdf::resolve).
///
/// - [`DecodeLevel::None`] → the raw filter-encoded bytes, verbatim.
/// - Any other level → the filter-decoded content, computed via
///   [`crate::filters::decode_stream_data`].
///
/// flpdf only implements generalized filters, so `Generalized`, `Specialized`
/// and `All` are equivalent here (qpdf treats `specialized`/`all` as supersets
/// of `generalized`). When the filter pipeline cannot decode a stream — e.g. an
/// unsupported filter such as `DCTDecode` — this falls back to the raw bytes
/// rather than erroring, matching qpdf, which emits the raw payload for filters
/// it does not decode rather than failing the whole document.
///
/// Returns a [`Cow`] so the raw-bytes paths ([`DecodeLevel::None`] and the
/// decode-error fallback) borrow `stream.data` instead of copying it — only the
/// successful decode path allocates (it must: the decoded bytes are new).
pub fn stream_payload_for_decode_level(
    stream: &Stream,
    decode_level: DecodeLevel,
) -> Cow<'_, [u8]> {
    match decode_level {
        DecodeLevel::None => Cow::Borrowed(&stream.data),
        DecodeLevel::Generalized | DecodeLevel::Specialized | DecodeLevel::All => {
            match crate::filters::decode_stream_data(&stream.dict, &stream.data) {
                Ok(decoded) => Cow::Owned(decoded),
                Err(_) => Cow::Borrowed(&stream.data),
            }
        }
    }
}

// ── build_envelope ───────────────────────────────────────────────────────────

/// Build the qpdf JSON v2 top-level envelope.
///
/// Returns a [`JsonValue::Object`] with exactly two keys in qpdf order:
/// `"version"` and `"parameters"`.  Callers can append additional section
/// keys (e.g. `"pages"`, `"objects"`) to the returned object's pair list.
///
/// # Example
///
/// ```
/// use flpdf::json_inspect::{build_envelope, DecodeLevel};
/// use flpdf::json::write;
///
/// let envelope = build_envelope(DecodeLevel::Generalized);
/// let mut buf = Vec::new();
/// write(&envelope, &mut buf).unwrap();
/// let s = String::from_utf8(buf).unwrap();
/// assert!(s.contains("\"version\": 2"));
/// assert!(s.contains("\"decodelevel\": \"generalized\""));
/// ```
pub fn build_envelope(decode_level: DecodeLevel) -> JsonValue {
    let parameters = JsonValue::Object(vec![(
        "decodelevel".to_string(),
        JsonValue::String(decode_level.as_qpdf_str().to_string()),
    )]);

    JsonValue::Object(vec![
        ("version".to_string(), JsonValue::Integer(2)),
        ("parameters".to_string(), parameters),
    ])
}

// ── build_pages_section ───────────────────────────────────────────────────────

/// Flatten a `/Contents` entry into a list of indirect-reference strings.
///
/// Handles three forms:
/// - `Object::Reference(r)` → `["N M R"]`
/// - `Object::Array([Reference, ...])` → each element as `"N M R"` (direct
///   streams in the array are silently skipped — they carry no ref string)
/// - `Object::Null` or absent → `[]`
///
/// Direct inline Streams outside an array have no object number and are
/// therefore skipped (spec-compliant PDFs use indirect refs for /Contents).
/// Collect the page's `/Contents` references as `"N M R"` strings.
///
/// PDF allows `/Contents` in several shapes:
///
/// 1. A direct Stream (rare; no ref to emit, returns `[]`).
/// 2. A `Reference` to a Stream → one entry.
/// 3. A `Reference` to an Array (`/Contents 12 0 R` where `12 0 obj [4 0 R 5 0 R]`)
///    → resolve the indirect array and recurse over its elements.
/// 4. A direct `Array` of References → one entry per Reference element.
///
/// In every variant the function emits the *original* reference strings, not
/// the wrapper array's ref number — that matches qpdf's `contents` output.
fn collect_content_refs<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    content_obj: &Object,
) -> Result<Vec<String>, ConvertError> {
    fn ref_string(r: &crate::ObjectRef) -> String {
        format!("{} {} R", r.number, r.generation)
    }

    match content_obj {
        Object::Reference(r) => {
            // Resolve to see whether the indirect object is a Stream (in
            // which case this ref itself is the content) or an Array of
            // Stream refs (in which case we recurse to flatten).
            let resolved_array = match pdf.resolve_borrowed(*r).map_err(ConvertError::from)? {
                Object::Stream(_) => return Ok(vec![ref_string(r)]),
                Object::Array(arr) => Object::Array(arr.clone()),
                // /Contents pointing at anything else (Null, missing) → empty.
                _ => return Ok(vec![]),
            };
            // Recurse so a nested indirect array is also unwrapped.
            collect_content_refs(pdf, &resolved_array)
        }
        Object::Array(elems) => {
            let mut refs = Vec::with_capacity(elems.len());
            for e in elems {
                if let Object::Reference(r) = e {
                    refs.push(ref_string(r));
                }
                // Direct inline streams have no ref string — skip them.
            }
            Ok(refs)
        }
        // Null, missing, or direct Stream — emit empty list.
        _ => Ok(vec![]),
    }
}

/// Collect image XObject reference strings for a single page.
///
/// Walks the inherited `/Resources /XObject` dictionary and, for each entry
/// whose resolved value is a Stream with `/Subtype /Image`, appends
/// `"N M R"` (the *original* reference string) to the result. Entries that
/// are direct inline Streams (no ref number) are skipped. The output is
/// sorted by XObject name (alphabetical byte-lex order).
fn collect_image_refs<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: crate::ObjectRef,
) -> Result<Vec<String>, ConvertError> {
    let resources = match crate::pages::resolve_inherited_resources(pdf, page_ref) {
        Ok(Some(d)) => d,
        Ok(None) => return Ok(vec![]),
        Err(e) => return Err(ConvertError::PdfError(e.to_string())),
    };

    // Resolve the /XObject sub-dictionary (may itself be indirect).
    let xobject_dict = match resources.get("XObject") {
        Some(Object::Dictionary(d)) => d.clone(),
        Some(Object::Reference(r)) => {
            let resolved = pdf.resolve_borrowed(*r).map_err(ConvertError::from)?;
            match resolved {
                Object::Dictionary(d) => d.clone(),
                _ => return Ok(vec![]),
            }
        }
        _ => return Ok(vec![]),
    };

    // Iterate in name (key) order — BTreeMap gives byte-lex order automatically.
    let mut image_refs: Vec<String> = Vec::new();
    for (_name, value) in xobject_dict.iter() {
        // Each XObject entry should be an indirect Reference.
        let xobj_ref = match value {
            Object::Reference(r) => *r,
            // Direct inline stream — no ref string available, skip.
            _ => continue,
        };
        let resolved = pdf.resolve_borrowed(xobj_ref).map_err(ConvertError::from)?;
        if let Some(stream) = resolved.as_stream() {
            if let Some(Object::Name(subtype)) = stream.dict.get("Subtype") {
                if subtype.as_slice() == b"Image" {
                    image_refs.push(format!("{} {} R", xobj_ref.number, xobj_ref.generation));
                }
            }
        }
    }
    Ok(image_refs)
}

/// Build the qpdf JSON v2 `"pages"` section.
///
/// Returns a [`JsonValue::Array`] where each element is a JSON object with
/// keys in alphabetical order:
/// `contents`, `images`, `label`, `object`, `outlines`, `pageposfrom1`.
///
/// - `label` is always `null` (placeholder; not yet populated).
/// - `outlines` is always `[]` (placeholder; not yet populated).
///
/// # Errors
///
/// Returns a [`ConvertError`] if the page tree cannot be traversed or any
/// object resolution fails.
pub fn build_pages_section<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<JsonValue, ConvertError> {
    let page_refs =
        crate::pages::page_refs(pdf).map_err(|e| ConvertError::PdfError(e.to_string()))?;

    let mut entries: Vec<JsonValue> = Vec::with_capacity(page_refs.len());

    for (idx, page_ref) in page_refs.into_iter().enumerate() {
        let pageposfrom1 = (idx as i64) + 1;
        let object_str = format!("{} {} R", page_ref.number, page_ref.generation);

        // Resolve the page dict to extract /Contents.
        let page_obj = pdf.resolve_borrowed(page_ref).map_err(ConvertError::from)?;
        let contents_obj = match page_obj {
            Object::Dictionary(d) => d.get("Contents").cloned(),
            _ => None,
        };
        let contents: Vec<JsonValue> = match &contents_obj {
            Some(c) => collect_content_refs(pdf, c)?
                .into_iter()
                .map(JsonValue::String)
                .collect(),
            None => vec![],
        };

        // Collect image XObject refs from (inherited) Resources.
        let images: Vec<JsonValue> = collect_image_refs(pdf, page_ref)?
            .into_iter()
            .map(JsonValue::String)
            .collect();

        // Build page entry with keys in strict alphabetical order:
        // contents < images < label < object < outlines < pageposfrom1
        let entry = JsonValue::Object(vec![
            ("contents".to_string(), JsonValue::Array(contents)),
            ("images".to_string(), JsonValue::Array(images)),
            // placeholder until flpdf-9hc.11.5 (page labels)
            ("label".to_string(), JsonValue::Null),
            ("object".to_string(), JsonValue::String(object_str)),
            // placeholder until flpdf-9hc.11.6 (outline back-references)
            ("outlines".to_string(), JsonValue::Array(vec![])),
            ("pageposfrom1".to_string(), JsonValue::Integer(pageposfrom1)),
        ]);
        entries.push(entry);
    }

    Ok(JsonValue::Array(entries))
}

// ── build_pagelabels_section ──────────────────────────────────────────────────

/// Convert a page-label dictionary (`/Type /PageLabel`) to a JSON object with
/// keys in alphabetical order: `first`, `prefix`, `style`.
///
/// - `first`  = `/St` (integer, default 1)
/// - `prefix` = `/P` (PDF text string, decoded without `u:`/`b:` prefix; default `""`)
/// - `style`  = `/S` (name string `"D"/"R"/"r"/"A"/"a"`, or `null` when absent)
fn label_dict_to_json(dict: &Dictionary) -> JsonValue {
    // Derive /S, /P, /St via the shared (non-resolving) LabelRange parser to keep
    // a single source of truth. `from_dict` is byte-for-byte equivalent to the
    // prior inline extraction; use it (not `from_dict_resolved`) to preserve the
    // existing non-resolving JSON behavior.
    let range = crate::page_label_document_helper::LabelRange::from_dict(dict);
    let style = match range.style.to_name() {
        Some(s) => JsonValue::String(s.to_string()),
        None => JsonValue::Null,
    };

    // Key order: alphabetical → first, prefix, style
    JsonValue::Object(vec![
        ("first".to_string(), JsonValue::Integer(range.start)),
        ("prefix".to_string(), JsonValue::String(range.prefix)),
        ("style".to_string(), style),
    ])
}

/// Build the qpdf JSON v2 `"pagelabels"` section.
///
/// Returns a [`JsonValue::Array`] where each element is a JSON object with
/// keys in alphabetical order: `index`, `label`.
///
/// Returns `JsonValue::Array(vec![])` when the document has no `/PageLabels` entry.
///
/// # Errors
///
/// Returns a [`ConvertError`] if any indirect object resolution fails during tree walk.
pub fn build_pagelabels_section<R: Read + Seek>(
    pdf: &mut Pdf<R>,
) -> Result<JsonValue, ConvertError> {
    use crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH;

    // qpdf's doJSONPageLabels obtains the full page list before checking
    // whether /PageLabels exists. Preserve both that validation side effect
    // and the observable everCalledGetAllPages metadata state.
    crate::pages::page_refs(pdf)?;

    // Resolve the Catalog.
    let catalog_ref = match pdf.root_ref() {
        Some(r) => r,
        None => return Ok(JsonValue::Array(vec![])),
    };
    let catalog = pdf
        .resolve_borrowed(catalog_ref)
        .map_err(ConvertError::from)?;
    let catalog_dict = match catalog {
        Object::Dictionary(d) => d,
        _ => return Ok(JsonValue::Array(vec![])),
    };

    // Look up /PageLabels.  May be absent, a direct Dictionary, or a Reference.
    let pagelabels_val = match catalog_dict.get("PageLabels") {
        Some(v) => v.clone(),
        None => return Ok(JsonValue::Array(vec![])),
    };

    // The generic number-tree walker resolves the root reference itself, so
    // pass `pagelabels_val` (Reference or Dictionary) straight in.
    let mut entries: Vec<(i64, Dictionary)> = crate::name_number_tree::read_number_tree(
        pdf,
        pagelabels_val,
        |pdf, v| match v {
            Object::Dictionary(d) => Ok(Some(d)),
            Object::Reference(r) => Ok(pdf.resolve_borrowed(r)?.as_dict().cloned()),
            _ => Ok(None),
        },
        DEFAULT_MAX_PAGE_TREE_DEPTH,
    )
    .map_err(ConvertError::from)?;

    // Sort by page index (ascending) — spec guarantees ascending order in a
    // well-formed number tree, but we sort defensively.
    entries.sort_by_key(|(idx, _)| *idx);

    let result: Vec<JsonValue> = entries
        .into_iter()
        .map(|(idx, label_dict)| {
            let label_json = label_dict_to_json(&label_dict);
            JsonValue::Object(vec![
                ("index".to_string(), JsonValue::Integer(idx)),
                ("label".to_string(), label_json),
            ])
        })
        .collect();

    Ok(JsonValue::Array(result))
}

// ── build_outlines_section ────────────────────────────────────────────────────

/// Project one materialized outline item into qpdf's JSON v2 shape.
fn outline_item_to_json(
    tree: &crate::OutlineTree,
    id: crate::OutlineId,
    page_numbers: &std::collections::BTreeMap<crate::ObjectRef, i64>,
) -> Result<JsonValue, ConvertError> {
    let item = &tree[id];
    let dest = pdf_object_to_json(&item.dest)?;
    let destpageposfrom1 = match item.dest_page() {
        Object::Reference(reference) => page_numbers
            .get(&reference)
            .copied()
            .map(JsonValue::Integer)
            .unwrap_or(JsonValue::Null),
        _ => JsonValue::Null,
    };
    let kids = item
        .kids
        .iter()
        .copied()
        .map(|kid| outline_item_to_json(tree, kid, page_numbers))
        .collect::<Result<Vec<_>, _>>()?;
    let object = match item.source_ref {
        Some(reference) => JsonValue::String(reference.to_string()),
        None => pdf_object_to_json(&item.object)?,
    };

    Ok(JsonValue::Object(vec![
        ("dest".to_string(), dest),
        ("destpageposfrom1".to_string(), destpageposfrom1),
        ("kids".to_string(), JsonValue::Array(kids)),
        ("object".to_string(), object),
        ("open".to_string(), JsonValue::Bool(item.count >= 0)),
        ("title".to_string(), JsonValue::String(item.title.clone())),
    ]))
}

/// Build the qpdf JSON v2 `"outlines"` section.
///
/// Returns a [`JsonValue::Array`] where each element is a JSON object
/// representing one root-level outline item (with `kids` recursively
/// expanded).  Returns `JsonValue::Array(vec![])` when the document has no
/// `/Outlines` entry or the outline dictionary has no `/First` child.
///
/// Each entry has keys in alphabetical order: `dest`, `destpageposfrom1`,
/// `kids`, `object`, `open`, `title`.
///
/// # Errors
///
/// Returns a [`ConvertError`] if any indirect object resolution fails.
pub fn build_outlines_section<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<JsonValue, ConvertError> {
    let page_numbers = crate::pages::page_refs(pdf)?
        .into_iter()
        .enumerate()
        .map(|(index, reference)| (reference, index as i64 + 1))
        .collect::<std::collections::BTreeMap<_, _>>();
    let tree = pdf.outline().get_tree()?;
    let entries = tree
        .roots()
        .iter()
        .copied()
        .map(|id| outline_item_to_json(&tree, id, &page_numbers))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(JsonValue::Array(entries))
}

// ── build_acroform_section ────────────────────────────────────────────────────

/// Walk a single AcroForm field (and its sub-fields), appending flat field
/// entries to `output`.
///
/// - `field_ref`: the ObjectRef of this field
/// - `parent_fullname`: dot-joined name path of the parent (empty string at root)
/// - `parent_ref`: the parent field's ObjectRef (None at root)
/// - `output`: flat list of JSON field entries to append to
/// - `seen`: cycle guard (ObjectRef set)
/// - `depth` / `max_depth`: recursion limit
#[allow(clippy::too_many_arguments)]
fn walk_acroform_fields<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    field_ref: crate::ObjectRef,
    parent_fullname: &str,
    parent_ref: Option<crate::ObjectRef>,
    output: &mut Vec<JsonValue>,
    seen: &mut std::collections::BTreeSet<crate::ObjectRef>,
    depth: usize,
    max_depth: usize,
) -> Result<(), ConvertError> {
    if depth > max_depth {
        return Ok(());
    }
    if !seen.insert(field_ref) {
        return Ok(()); // cycle guard
    }

    let field_obj = pdf
        .resolve_borrowed(field_ref)
        .map_err(ConvertError::from)?;
    let field_dict = match field_obj {
        Object::Dictionary(d) => d.clone(),
        _ => return Ok(()), // non-dictionary field — skip
    };

    // Compute fullname: parent.T or just T at root.
    let t_string = match field_dict.get("T") {
        Some(Object::String(bytes)) => decode_pdf_text_string(bytes)
            .unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned()),
        _ => String::new(),
    };
    let fullname = if parent_fullname.is_empty() {
        t_string.clone()
    } else if t_string.is_empty() {
        parent_fullname.to_string()
    } else {
        format!("{parent_fullname}.{t_string}")
    };

    // object ref string
    let object_str = format!("{} {} R", field_ref.number, field_ref.generation);

    // parent ref string (null at root)
    let parent_json = match parent_ref {
        Some(r) => JsonValue::String(format!("{} {} R", r.number, r.generation)),
        None => JsonValue::Null,
    };

    // /FT, /V, /DV, /Ff are all inheritable down the /Parent chain
    // (ISO 32000-1 §12.7.3.1). Use the same lookup helper for each.
    let ft_obj = inherited_field_value(pdf, &field_dict, "FT")?;
    let fieldtype = match ft_obj {
        Some(Object::Name(bytes)) => {
            JsonValue::String(String::from_utf8_lossy(&bytes).into_owned())
        }
        _ => JsonValue::Null,
    };

    // /V — value, run through pdf_object_to_json. Inherited from /Parent.
    let value = match inherited_field_value(pdf, &field_dict, "V")? {
        Some(v) => pdf_object_to_json(&v)?,
        None => JsonValue::Null,
    };

    // /DV — default value. Inherited from /Parent.
    let defaultvalue = match inherited_field_value(pdf, &field_dict, "DV")? {
        Some(v) => pdf_object_to_json(&v)?,
        None => JsonValue::Null,
    };

    // /Ff — field flags integer. Inherited from /Parent.
    let fieldflags = match inherited_field_value(pdf, &field_dict, "Ff")? {
        Some(Object::Integer(n)) => JsonValue::Integer(n),
        _ => JsonValue::Null,
    };

    // /TU — alternate name.
    let alternatename = match field_dict.get("TU") {
        Some(Object::String(bytes)) => {
            let s = decode_pdf_text_string(bytes)
                .unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned());
            JsonValue::String(s)
        }
        _ => JsonValue::Null,
    };

    // /TM — mapping name.
    let mappingname = match field_dict.get("TM") {
        Some(Object::String(bytes)) => {
            let s = decode_pdf_text_string(bytes)
                .unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned());
            JsonValue::String(s)
        }
        _ => JsonValue::Null,
    };

    // Determine if this field is itself a widget annotation.
    let is_widget = field_dict
        .get("Subtype")
        .map(|v| matches!(v, Object::Name(n) if n.as_slice() == b"Widget"))
        .unwrap_or(false);

    // /Kids — may be a direct Array or an indirect Reference to an Array.
    // Resolve the indirect form so we don't silently drop the entire kid
    // chain when it lives in its own object.
    let kids = match field_dict.get("Kids").cloned() {
        Some(Object::Array(arr)) => arr,
        Some(Object::Reference(r)) => match pdf.resolve_borrowed(r).map_err(ConvertError::from)? {
            Object::Array(arr) => arr.clone(),
            _ => vec![],
        },
        _ => vec![],
    };

    let mut annotations: Vec<JsonValue> = Vec::new();
    let mut has_subfield_kids = false;

    // Classify each kid as a widget annotation vs a (sub-)field dictionary.
    //
    // A kid is a widget *only* when its dictionary has `/Subtype /Widget` AND
    // carries no field-like entries that would make it a real field. Field
    // dictionaries can legitimately have no `/T` (unnamed intermediate
    // grouping nodes — `/T` is optional per ISO 32000-1 §12.7.3.1, only
    // `fullname` derivation cares); checking only for `/T` previously dropped
    // those branches and their descendants from the flat list.
    let mut subfield_refs: Vec<crate::ObjectRef> = Vec::new();
    for kid in &kids {
        let kid_ref = match kid {
            Object::Reference(r) => *r,
            _ => continue,
        };
        let kid_obj = pdf.resolve_borrowed(kid_ref).map_err(ConvertError::from)?;
        let Object::Dictionary(d) = kid_obj else {
            continue;
        };
        let is_widget_subtype = matches!(
            d.get("Subtype"),
            Some(Object::Name(n)) if n.as_slice() == b"Widget"
        );
        // Field-like markers that mean the kid acts as a (possibly unnamed)
        // field even when /Subtype is /Widget. Covers the merged widget+field
        // case where the widget dictionary carries field state (value, flags,
        // alternate / mapping names, etc.) directly. /Parent is intentionally
        // NOT here: standalone widget annotations point back to their owning
        // field via /Parent, so its presence alone doesn't make a kid a field.
        let has_field_entries = d.get("T").is_some()
            || d.get("FT").is_some()
            || d.get("Kids").is_some()
            || d.get("V").is_some()
            || d.get("DV").is_some()
            || d.get("Ff").is_some()
            || d.get("TU").is_some()
            || d.get("TM").is_some();

        if is_widget_subtype && !has_field_entries {
            // Pure widget annotation — collect ref string, do not recurse.
            annotations.push(JsonValue::String(format!(
                "{} {} R",
                kid_ref.number, kid_ref.generation
            )));
        } else {
            // Field dict (named or unnamed) — recurse so descendants are
            // emitted in the flat list.
            subfield_refs.push(kid_ref);
            has_subfield_kids = true;
        }
    }

    // If this field itself is a widget (and no sub-fields), add self to annotations.
    if is_widget && !has_subfield_kids {
        annotations.insert(0, JsonValue::String(object_str.clone()));
    }

    // Build and push this field's JSON entry (alphabetical key order).
    let entry = JsonValue::Object(vec![
        ("alternatename".to_string(), alternatename),
        ("annotations".to_string(), JsonValue::Array(annotations)),
        ("defaultvalue".to_string(), defaultvalue),
        ("fieldflags".to_string(), fieldflags),
        ("fieldtype".to_string(), fieldtype),
        ("fullname".to_string(), JsonValue::String(fullname.clone())),
        ("mappingname".to_string(), mappingname),
        ("object".to_string(), JsonValue::String(object_str)),
        ("parent".to_string(), parent_json),
        ("value".to_string(), value),
    ]);
    output.push(entry);

    // Recurse into sub-field kids.
    if depth < max_depth {
        for kid_ref in subfield_refs {
            walk_acroform_fields(
                pdf,
                kid_ref,
                &fullname,
                Some(field_ref),
                output,
                seen,
                depth + 1,
                max_depth,
            )?;
        }
    }

    Ok(())
}

// Look up an inheritable AcroForm field entry on `field_dict`, walking the
// `/Parent` chain if the key is absent locally (ISO 32000-1 §12.7.3.1).
//
// Returns `Some(value)` for the first non-absent value found at this dict or
// any ancestor; `None` if neither this field nor any ancestor carries `key`.
// Cycle-safe: never visits the same `/Parent` twice.
fn inherited_field_value<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    field_dict: &Dictionary,
    key: &str,
) -> Result<Option<Object>, ConvertError> {
    use crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH;
    if let Some(local) = field_dict.get(key).cloned() {
        return Ok(Some(local));
    }
    let mut parent = field_dict.get("Parent").cloned();
    let mut seen: std::collections::BTreeSet<crate::ObjectRef> = std::collections::BTreeSet::new();
    let mut depth: usize = 0;
    while let Some(Object::Reference(pr)) = parent {
        if depth >= DEFAULT_MAX_PAGE_TREE_DEPTH {
            return Err(ConvertError::PdfError(format!(
                "AcroForm field-tree depth limit {DEFAULT_MAX_PAGE_TREE_DEPTH} exceeded"
            )));
        }
        if !seen.insert(pr) {
            break;
        }
        match pdf.resolve_borrowed(pr).map_err(ConvertError::from)? {
            Object::Dictionary(pd) => {
                if let Some(v) = pd.get(key).cloned() {
                    return Ok(Some(v));
                }
                parent = pd.get("Parent").cloned();
            }
            _ => break,
        }
        depth += 1;
    }
    Ok(None)
}

/// Build the qpdf JSON v2 `"acroform"` section.
///
/// Returns a [`JsonValue::Object`] with three keys in alphabetical order:
/// `fields`, `hasacroform`, `needappearances`.
///
/// - `hasacroform`: true iff `/Catalog/AcroForm` exists.
/// - `needappearances`: value of `/AcroForm/NeedAppearances` (default false).
/// - `fields`: flat list of all field entries, recursively expanded.
///
/// # Errors
///
/// Returns a [`ConvertError`] if any indirect object resolution fails.
pub fn build_acroform_section<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<JsonValue, ConvertError> {
    use crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH;

    // qpdf's doJSONAcroform always walks every page to discover widgets,
    // including when the catalog has no /AcroForm entry.
    crate::pages::page_refs(pdf)?;

    // Resolve the Catalog.
    let catalog_ref = match pdf.root_ref() {
        Some(r) => r,
        None => {
            return Ok(JsonValue::Object(vec![
                ("fields".to_string(), JsonValue::Array(vec![])),
                ("hasacroform".to_string(), JsonValue::Bool(false)),
                ("needappearances".to_string(), JsonValue::Bool(false)),
            ]))
        }
    };
    let catalog = pdf
        .resolve_borrowed(catalog_ref)
        .map_err(ConvertError::from)?;
    let catalog_dict = match catalog {
        Object::Dictionary(d) => d,
        _ => {
            return Ok(JsonValue::Object(vec![
                ("fields".to_string(), JsonValue::Array(vec![])),
                ("hasacroform".to_string(), JsonValue::Bool(false)),
                ("needappearances".to_string(), JsonValue::Bool(false)),
            ]))
        }
    };

    // Check for /AcroForm.
    let acroform_val = match catalog_dict.get("AcroForm") {
        Some(v) => v.clone(),
        None => {
            return Ok(JsonValue::Object(vec![
                ("fields".to_string(), JsonValue::Array(vec![])),
                ("hasacroform".to_string(), JsonValue::Bool(false)),
                ("needappearances".to_string(), JsonValue::Bool(false)),
            ]))
        }
    };

    // Resolve indirect reference if needed.
    let acroform_dict = match acroform_val {
        Object::Reference(r) => match pdf.resolve_borrowed(r).map_err(ConvertError::from)? {
            Object::Dictionary(d) => d.clone(),
            _ => {
                return Ok(JsonValue::Object(vec![
                    ("fields".to_string(), JsonValue::Array(vec![])),
                    ("hasacroform".to_string(), JsonValue::Bool(true)),
                    ("needappearances".to_string(), JsonValue::Bool(false)),
                ]))
            }
        },
        Object::Dictionary(d) => d,
        _ => {
            return Ok(JsonValue::Object(vec![
                ("fields".to_string(), JsonValue::Array(vec![])),
                ("hasacroform".to_string(), JsonValue::Bool(true)),
                ("needappearances".to_string(), JsonValue::Bool(false)),
            ]))
        }
    };

    // /NeedAppearances (default false).
    let need_appearances = match acroform_dict.get("NeedAppearances") {
        Some(Object::Boolean(b)) => *b,
        _ => false,
    };

    // /Fields array (top-level field refs). Same as /Kids below: must
    // accept both the direct Array form and an indirect Reference to an
    // Array, so AcroForm dictionaries that store /Fields as its own object
    // don't surface as an empty list.
    let fields_array = match acroform_dict.get("Fields").cloned() {
        Some(Object::Array(arr)) => arr,
        Some(Object::Reference(r)) => match pdf.resolve_borrowed(r).map_err(ConvertError::from)? {
            Object::Array(arr) => arr.clone(),
            _ => vec![],
        },
        _ => vec![],
    };

    let mut output: Vec<JsonValue> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();

    for field_obj in &fields_array {
        let field_ref = match field_obj {
            Object::Reference(r) => *r,
            _ => continue,
        };
        walk_acroform_fields(
            pdf,
            field_ref,
            "",
            None,
            &mut output,
            &mut seen,
            0,
            DEFAULT_MAX_PAGE_TREE_DEPTH,
        )?;
    }

    Ok(JsonValue::Object(vec![
        ("fields".to_string(), JsonValue::Array(output)),
        ("hasacroform".to_string(), JsonValue::Bool(true)),
        (
            "needappearances".to_string(),
            JsonValue::Bool(need_appearances),
        ),
    ]))
}

// ── build_attachments_section ─────────────────────────────────────────────────

/// Parse a PDF date string (ISO 32000-1 §7.9.4) into an ISO 8601 string.
///
/// The PDF date format is `D:YYYYMMDDhhmmss±HH'mm'` (or `Z` for UTC).
/// Returns `None` if the bytes cannot be parsed as a date.
///
/// The input is required to be pure ASCII: PDF dates are an ASCII-only
/// format, and treating them as such avoids panicking on multibyte byte
/// boundaries when caller code passes a stray non-ASCII string.
fn parse_pdf_date(bytes: &[u8]) -> Option<String> {
    // PDF dates are ASCII-only. Reject any non-ASCII byte up front so the
    // byte-index slicing below cannot land in the middle of a UTF-8
    // multibyte sequence.
    if !bytes.is_ascii() {
        return None;
    }
    let s = std::str::from_utf8(bytes).ok()?;
    let s = s.strip_prefix("D:").unwrap_or(s);

    // Must have at least 4 chars for the year
    if s.len() < 4 {
        return None;
    }

    // Validate each fixed-width component is all digits before slicing it.
    let is_digits = |slice: &str| !slice.is_empty() && slice.bytes().all(|b| b.is_ascii_digit());

    let year = &s[0..4];
    if !is_digits(year) {
        return None;
    }

    // The fixed-width date prefix must end at one of the valid component
    // boundaries: YYYY, YYYYMM, YYYYMMDD, YYYYMMDDhh, YYYYMMDDhhmm, or
    // YYYYMMDDhhmmss. A trailing partial component (e.g. an odd 5th char in
    // "D:20261") is malformed; we refuse it rather than discarding the
    // dangling digits.
    let prefix_len = s.len().min(14);
    if !matches!(prefix_len, 4 | 6 | 8 | 10 | 12 | 14) {
        return None;
    }

    let month_default = "01";
    let day_default = "01";
    let zero_default = "00";

    let take = |start: usize, end: usize, fallback: &'static str| -> Option<&str> {
        if s.len() >= end {
            let slice = &s[start..end];
            if is_digits(slice) {
                Some(slice)
            } else {
                None
            }
        } else {
            Some(fallback)
        }
    };

    let month = take(4, 6, month_default)?;
    let day = take(6, 8, day_default)?;
    let hour = take(8, 10, zero_default)?;
    let minute = take(10, 12, zero_default)?;
    let second = take(12, 14, zero_default)?;

    // Numeric range validation so we don't emit ISO 8601 strings that
    // downstream parsers will reject (e.g. month=13, hour=24). All fields
    // are guaranteed to be 2-digit ASCII at this point.
    let in_range = |s: &str, lo: u8, hi: u8| -> bool {
        s.parse::<u8>().map(|n| n >= lo && n <= hi).unwrap_or(false)
    };
    if !in_range(month, 1, 12)
        || !in_range(day, 1, 31)
        || !in_range(hour, 0, 23)
        || !in_range(minute, 0, 59)
        || !in_range(second, 0, 59)
    {
        return None;
    }

    // Parse timezone. Trailing garbage (anything not empty / Z / z / +... /
    // -...) must yield None rather than silently defaulting to "Z", to keep
    // the function's "unparseable input -> None" contract honest.
    let tz_str = if s.len() > 14 { &s[14..] } else { "" };
    let tz = if tz_str.is_empty() || tz_str == "Z" || tz_str == "z" {
        "Z".to_string()
    } else if let Some(rest) = tz_str.strip_prefix('+') {
        parse_tz_offset('+', rest)?
    } else {
        let rest = tz_str.strip_prefix('-')?;
        parse_tz_offset('-', rest)?
    };

    Some(format!("{year}-{month}-{day}T{hour}:{minute}:{second}{tz}"))
}

/// Parse a timezone offset in the form `HH'mm'` or `HH` and return a string
/// like `+HH:MM`. Returns `None` for malformed offsets so callers can
/// propagate the failure up.
fn parse_tz_offset(sign: char, rest: &str) -> Option<String> {
    // Accept exactly one of: `HH'mm'`, `HH'mm`, `HHmm`, or `HH`.
    // Strip the single optional closing apostrophe so the remaining shapes
    // collapse to four; anything else (multiple trailing apostrophes,
    // garbage suffix, partial component) is rejected.
    let rest = rest.strip_suffix('\'').unwrap_or(rest);
    let (hh, mm) = if rest.len() == 5 && rest.as_bytes().get(2) == Some(&b'\'') {
        (&rest[0..2], &rest[3..5])
    } else if rest.len() == 4 {
        (&rest[0..2], &rest[2..4])
    } else if rest.len() == 2 {
        (rest, "00")
    } else {
        return None;
    };
    // Validate digits and numeric ranges for the tz offset.
    if !hh.chars().all(|c| c.is_ascii_digit()) || !mm.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let hh_n = hh.parse::<u8>().ok()?;
    let mm_n = mm.parse::<u8>().ok()?;
    if hh_n > 23 || mm_n > 59 {
        return None;
    }
    // If +00:00, emit Z
    if sign == '+' && hh_n == 0 && mm_n == 0 {
        Some("Z".to_string())
    } else {
        Some(format!("{sign}{hh}:{mm}"))
    }
}

/// Convert raw bytes to lowercase hex string.
fn checksum_to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Source of a filespec value found in the EmbeddedFiles name tree.
///
/// PDF name tree leaf values can be either an indirect Reference (the common
/// case) or a direct Dictionary embedded inline. Both shapes must produce an
/// `attachments` entry.
enum FilespecSource {
    Indirect(crate::ObjectRef),
    Direct(Dictionary),
}

/// Build a JSON entry for one filespec dictionary.
///
/// Returns an object with keys in alphabetical order:
/// `description`, `filespec`, `names`, `preferredcontents`, `preferredname`, `streams`.
fn filespec_to_json<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filespec_ref: crate::ObjectRef,
) -> Result<JsonValue, ConvertError> {
    let filespec_str = format!("{} {} R", filespec_ref.number, filespec_ref.generation);

    let filespec_obj = pdf
        .resolve_borrowed(filespec_ref)
        .map_err(ConvertError::from)?;
    let filespec_dict = match filespec_obj {
        Object::Dictionary(d) => d.clone(),
        _ => {
            // Malformed filespec — return a minimal entry
            return Ok(JsonValue::Object(vec![
                ("description".to_string(), JsonValue::Null),
                ("filespec".to_string(), JsonValue::String(filespec_str)),
                ("names".to_string(), JsonValue::Object(vec![])),
                ("preferredcontents".to_string(), JsonValue::Null),
                ("preferredname".to_string(), JsonValue::Null),
                ("streams".to_string(), JsonValue::Object(vec![])),
            ]));
        }
    };

    filespec_dict_to_json(pdf, &filespec_dict, Some(filespec_str))
}

/// Same as [`filespec_to_json`] but takes the filespec dictionary directly,
/// for the case where the name tree leaf value is a direct dictionary rather
/// than an indirect reference. When `filespec_str` is `Some`, it is used for
/// the `filespec` key; when `None`, that key emits `JsonValue::Null` because
/// no reference number exists.
fn filespec_dict_to_json<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filespec_dict: &Dictionary,
    filespec_str: Option<String>,
) -> Result<JsonValue, ConvertError> {
    let filespec_dict = filespec_dict.clone();
    let filespec_value = match filespec_str {
        Some(s) => JsonValue::String(s),
        None => JsonValue::Null,
    };

    // description: /Desc decoded as PDF text string, bare (no u:/b: prefix)
    let description = match filespec_dict.get("Desc") {
        Some(Object::String(bytes)) => {
            let s = decode_pdf_text_string(bytes)
                .unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned());
            JsonValue::String(s)
        }
        _ => JsonValue::Null,
    };

    // names: collect /F, /UF, /DOS, /Mac, /Unix — each decoded as PDF text string
    // Keys are in alphabetical order (they already are in BTree).
    let name_keys = ["DOS", "F", "Mac", "UF", "Unix"];
    let mut names_pairs: Vec<(String, JsonValue)> = Vec::new();
    for key in &name_keys {
        if let Some(Object::String(bytes)) = filespec_dict.get(*key) {
            let s = decode_pdf_text_string(bytes)
                .unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned());
            names_pairs.push((format!("/{key}"), JsonValue::String(s)));
        }
    }

    // preferredname: /UF > /F > /Unix > /Mac > /DOS
    let preferred_name_key_order = ["UF", "F", "Unix", "Mac", "DOS"];
    let preferredname = preferred_name_key_order
        .iter()
        .find_map(|key| {
            if let Some(Object::String(bytes)) = filespec_dict.get(*key) {
                let s = decode_pdf_text_string(bytes)
                    .unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned());
                Some(JsonValue::String(s))
            } else {
                None
            }
        })
        .unwrap_or(JsonValue::Null);

    // /EF dictionary: embedded file stream refs, keyed by /F /UF /DOS /Mac /Unix
    let ef_dict = match filespec_dict.get("EF") {
        Some(Object::Dictionary(d)) => Some(d.clone()),
        Some(Object::Reference(r)) => match pdf.resolve_borrowed(*r).map_err(ConvertError::from)? {
            Object::Dictionary(d) => Some(d.clone()),
            _ => None,
        },
        _ => None,
    };

    // preferredcontents: /EF/UF > /EF/F > /EF/Unix > /EF/Mac > /EF/DOS
    let preferred_ef_key_order = ["UF", "F", "Unix", "Mac", "DOS"];
    let preferredcontents = if let Some(ref ef) = ef_dict {
        preferred_ef_key_order
            .iter()
            .find_map(|key| {
                if let Some(Object::Reference(r)) = ef.get(*key) {
                    Some(JsonValue::String(format!(
                        "{} {} R",
                        r.number, r.generation
                    )))
                } else {
                    None
                }
            })
            .unwrap_or(JsonValue::Null)
    } else {
        JsonValue::Null
    };

    // streams: for each key in /EF, build a stream-info object
    // Keys alphabetical: /DOS, /F, /Mac, /UF, /Unix
    let ef_key_order = ["DOS", "F", "Mac", "UF", "Unix"];
    let mut streams_pairs: Vec<(String, JsonValue)> = Vec::new();

    if let Some(ref ef) = ef_dict {
        for key in &ef_key_order {
            let stream_ref = match ef.get(*key) {
                Some(Object::Reference(r)) => *r,
                _ => continue,
            };

            let stream_obj = pdf
                .resolve_borrowed(stream_ref)
                .map_err(ConvertError::from)?;
            let stream_dict = match stream_obj {
                Object::Stream(s) => s.dict.clone(),
                _ => continue,
            };

            // mimetype: /Subtype name → bare string (no "/" prefix), or null
            let mimetype = match stream_dict.get("Subtype") {
                Some(Object::Name(bytes)) => {
                    let s = String::from_utf8_lossy(bytes).into_owned();
                    JsonValue::String(s)
                }
                _ => JsonValue::Null,
            };

            // /Params sub-dict
            let params_dict = match stream_dict.get("Params") {
                Some(Object::Dictionary(d)) => Some(d.clone()),
                Some(Object::Reference(r)) => {
                    match pdf.resolve_borrowed(*r).map_err(ConvertError::from)? {
                        Object::Dictionary(d) => Some(d.clone()),
                        _ => None,
                    }
                }
                _ => None,
            };

            // checksum: /Params /CheckSum bytes → lowercase hex, or null
            let checksum = if let Some(ref p) = params_dict {
                match p.get("CheckSum") {
                    Some(Object::String(bytes)) => JsonValue::String(checksum_to_hex(bytes)),
                    _ => JsonValue::Null,
                }
            } else {
                JsonValue::Null
            };

            // creationdate: /Params /CreationDate → ISO 8601, or null
            let creationdate = if let Some(ref p) = params_dict {
                match p.get("CreationDate") {
                    Some(Object::String(bytes)) => match parse_pdf_date(bytes) {
                        Some(s) => JsonValue::String(s),
                        None => JsonValue::Null,
                    },
                    _ => JsonValue::Null,
                }
            } else {
                JsonValue::Null
            };

            // modificationdate: /Params /ModDate → ISO 8601, or null
            let modificationdate = if let Some(ref p) = params_dict {
                match p.get("ModDate") {
                    Some(Object::String(bytes)) => match parse_pdf_date(bytes) {
                        Some(s) => JsonValue::String(s),
                        None => JsonValue::Null,
                    },
                    _ => JsonValue::Null,
                }
            } else {
                JsonValue::Null
            };

            // Stream entry keys: checksum, creationdate, mimetype, modificationdate
            let stream_entry = JsonValue::Object(vec![
                ("checksum".to_string(), checksum),
                ("creationdate".to_string(), creationdate),
                ("mimetype".to_string(), mimetype),
                ("modificationdate".to_string(), modificationdate),
            ]);
            streams_pairs.push((format!("/{key}"), stream_entry));
        }
    }

    Ok(JsonValue::Object(vec![
        ("description".to_string(), description),
        ("filespec".to_string(), filespec_value),
        ("names".to_string(), JsonValue::Object(names_pairs)),
        ("preferredcontents".to_string(), preferredcontents),
        ("preferredname".to_string(), preferredname),
        ("streams".to_string(), JsonValue::Object(streams_pairs)),
    ]))
}

/// Build the qpdf JSON v2 `"attachments"` section.
///
/// Returns a [`JsonValue::Object`] where each key is an EmbeddedFiles name-tree
/// entry name (decoded PDF string, bare without prefix) and each value is a
/// filespec entry object.
///
/// Returns `JsonValue::Object(vec![])` when the document has no `/Names/EmbeddedFiles`.
///
/// # Errors
///
/// Returns a [`ConvertError`] if any indirect object resolution fails.
pub fn build_attachments_section<R: Read + Seek>(
    pdf: &mut Pdf<R>,
) -> Result<JsonValue, ConvertError> {
    use crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH;

    // Resolve the Catalog.
    let catalog_ref = match pdf.root_ref() {
        Some(r) => r,
        None => return Ok(JsonValue::Object(vec![])),
    };
    let catalog = pdf
        .resolve_borrowed(catalog_ref)
        .map_err(ConvertError::from)?;
    let names_val = match catalog {
        Object::Dictionary(d) => d.get("Names").cloned(),
        _ => return Ok(JsonValue::Object(vec![])),
    };

    // /Names dictionary from catalog
    let names_dict = match names_val {
        Some(Object::Dictionary(d)) => d.clone(),
        Some(Object::Reference(r)) => match pdf.resolve_borrowed(r).map_err(ConvertError::from)? {
            Object::Dictionary(d) => d.clone(),
            _ => return Ok(JsonValue::Object(vec![])),
        },
        _ => return Ok(JsonValue::Object(vec![])),
    };

    // /EmbeddedFiles name tree root: keep the original object shape so the
    // shared walker can resolve an indirect root itself and track its
    // ObjectRef in the visited set (cycle guard on a self-referential root).
    let ef_root = match names_dict.get("EmbeddedFiles").cloned() {
        Some(v) => v,
        None => return Ok(JsonValue::Object(vec![])),
    };

    // Walk the name tree to collect (name, filespec source) pairs via the
    // shared name-tree reader; decode the raw string key afterwards.
    let mut raw_entries: Vec<(String, FilespecSource)> = crate::name_number_tree::read_name_tree(
        pdf,
        ef_root,
        |_, v| {
            Ok(match v {
                Object::Reference(r) => Some(FilespecSource::Indirect(r)),
                Object::Dictionary(d) => Some(FilespecSource::Direct(d)),
                _ => None,
            })
        },
        DEFAULT_MAX_PAGE_TREE_DEPTH,
    )
    .map_err(ConvertError::from)?
    .into_iter()
    .map(|(key_bytes, source)| {
        let name = decode_pdf_text_string(&key_bytes)
            .unwrap_or_else(|| String::from_utf8_lossy(&key_bytes).into_owned());
        (name, source)
    })
    .collect();

    // Sort by name (alphabetical)
    raw_entries.sort_by(|a, b| a.0.cmp(&b.0));

    // Build the output object. Both indirect (Reference) and direct
    // (inlined Dictionary) filespec values yield an attachments entry.
    let mut pairs: Vec<(String, JsonValue)> = Vec::new();
    for (name, source) in raw_entries {
        let entry = match source {
            FilespecSource::Indirect(filespec_ref) => filespec_to_json(pdf, filespec_ref)?,
            FilespecSource::Direct(dict) => filespec_dict_to_json(pdf, &dict, None)?,
        };
        pairs.push((name, entry));
    }

    Ok(JsonValue::Object(pairs))
}

// ── build_encrypt_section ─────────────────────────────────────────────────────

/// Determine the qpdf method string ("none", "RC4", "AESv2", "AESv3") for a
/// crypt filter name looked up from the /CF dictionary of `encrypt`.
///
/// Returns `"none"` only for the explicit `Identity` selector or when there
/// is no `/Encrypt` revision to derive a default from. When the selector is
/// absent or the looked-up filter has no `/CFM`, the method falls back to
/// the revision-based default used by the reader and the qpdf handler:
///
/// - `/R >= 5` → `"AESv3"`
/// - `/R == 4` → `"AESv2"`
/// - everything else (legacy) → `"RC4"`
fn cf_method_string(encrypt: &Dictionary, selector: Option<&str>) -> &'static str {
    fn revision_default(encrypt: &Dictionary) -> &'static str {
        match encrypt.get("R") {
            Some(Object::Integer(r)) if *r >= 5 => "AESv3",
            Some(Object::Integer(4)) => "AESv2",
            _ => "RC4",
        }
    }

    let Some(selector) = selector else {
        return revision_default(encrypt);
    };
    if selector == "Identity" {
        return "none";
    }
    // Look up the CFM entry inside /CF/<selector>
    let Some(Object::Dictionary(cf)) = encrypt.get("CF") else {
        return revision_default(encrypt);
    };
    let Some(Object::Dictionary(filter)) = cf.get(selector) else {
        return revision_default(encrypt);
    };
    match filter.get("CFM") {
        Some(Object::Name(cfm)) => match cfm.as_slice() {
            b"AESV2" => "AESv2",
            b"AESV3" => "AESv3",
            b"V2" => "RC4",
            b"None" => "none",
            _ => revision_default(encrypt),
        },
        _ => revision_default(encrypt),
    }
}

/// Read an optional name key from `dict` and return it as `Option<&str>`.
fn dict_name_str<'a>(dict: &'a Dictionary, key: &str) -> Option<&'a str> {
    match dict.get(key) {
        Some(Object::Name(n)) => std::str::from_utf8(n).ok(),
        _ => None,
    }
}

/// Decode /P integer into per-capability booleans.
///
/// `p_raw` is the signed /P value. Per ISO 32000-1 §7.6.3.2 the bits are
/// tested after casting to u32 so that negative values (like -4) behave as
/// the expected all-bits-set value.
fn capabilities_from_p(p_raw: i32) -> Vec<(String, JsonValue)> {
    let p = p_raw as u32;
    // All nine capabilities in alphabetical order (qpdf schema).
    let accessibility = (p & 0x0200) != 0;
    let extract = (p & 0x0010) != 0;
    let modify = (p & 0x0008) != 0;
    let modifyannotations = (p & 0x0020) != 0;
    let modifyassembly = (p & 0x0400) != 0;
    let modifyforms = (p & 0x0100) != 0;
    // modifyother mirrors modify (qpdf behaviour for standard handler)
    let modifyother = modify;
    let printhigh = (p & 0x0800) != 0;
    let printlow = (p & 0x0004) != 0;

    vec![
        ("accessibility".into(), JsonValue::Bool(accessibility)),
        ("extract".into(), JsonValue::Bool(extract)),
        ("modify".into(), JsonValue::Bool(modify)),
        (
            "modifyannotations".into(),
            JsonValue::Bool(modifyannotations),
        ),
        ("modifyassembly".into(), JsonValue::Bool(modifyassembly)),
        ("modifyforms".into(), JsonValue::Bool(modifyforms)),
        ("modifyother".into(), JsonValue::Bool(modifyother)),
        ("printhigh".into(), JsonValue::Bool(printhigh)),
        ("printlow".into(), JsonValue::Bool(printlow)),
    ]
}

/// All-true capabilities object used for plaintext (no /Encrypt) documents.
fn all_true_capabilities() -> JsonValue {
    JsonValue::Object(vec![
        ("accessibility".into(), JsonValue::Bool(true)),
        ("extract".into(), JsonValue::Bool(true)),
        ("modify".into(), JsonValue::Bool(true)),
        ("modifyannotations".into(), JsonValue::Bool(true)),
        ("modifyassembly".into(), JsonValue::Bool(true)),
        ("modifyforms".into(), JsonValue::Bool(true)),
        ("modifyother".into(), JsonValue::Bool(true)),
        ("printhigh".into(), JsonValue::Bool(true)),
        ("printlow".into(), JsonValue::Bool(true)),
    ])
}

/// Build the `encrypt` section of the qpdf JSON v2 output.
///
/// Schema follows qpdf 11.x `--json --json-key=encrypt`:
/// - Plaintext / no `/Encrypt`: `encrypted: false`, all capabilities `true`,
///   all parameters 0 / "none".
/// - Encrypted: parameters from the `/Encrypt` dictionary; key is always
///   `null`; `recovereduserpassword` is always `null`.
///
/// The function reads the trailer's `/Encrypt` entry directly and does **not**
/// require any internal `EncryptionState` accessor, making it self-contained
/// inside `json_inspect`.
///
/// # Errors
///
/// Returns a [`ConvertError`] only when an indirect `/Encrypt` reference
/// cannot be resolved (i.e. an underlying I/O or parse error).
pub fn build_encrypt_section<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<JsonValue, ConvertError> {
    // Resolve /Encrypt dictionary from the trailer.
    let encrypt_dict: Option<Dictionary> = match pdf.trailer().get("Encrypt").cloned() {
        None => None,
        Some(Object::Dictionary(d)) => Some(d),
        Some(Object::Reference(r)) => match pdf.resolve_borrowed(r).map_err(ConvertError::from)? {
            Object::Dictionary(d) => Some(d.clone()),
            _ => None,
        },
        _ => None,
    };

    let is_encrypted = pdf.is_encrypted();

    match encrypt_dict {
        None => {
            // Plaintext document: all defaults.
            let capabilities = all_true_capabilities();
            let parameters = JsonValue::Object(vec![
                ("P".into(), JsonValue::Integer(0)),
                ("R".into(), JsonValue::Integer(0)),
                ("V".into(), JsonValue::Integer(0)),
                ("bits".into(), JsonValue::Integer(0)),
                ("filemethod".into(), JsonValue::String("none".into())),
                ("key".into(), JsonValue::Null),
                ("method".into(), JsonValue::String("none".into())),
                ("streammethod".into(), JsonValue::String("none".into())),
                ("stringmethod".into(), JsonValue::String("none".into())),
            ]);
            Ok(JsonValue::Object(vec![
                ("capabilities".into(), capabilities),
                ("encrypted".into(), JsonValue::Bool(false)),
                ("ownerpasswordmatched".into(), JsonValue::Bool(false)),
                ("parameters".into(), parameters),
                ("recovereduserpassword".into(), JsonValue::Null),
                ("userpasswordmatched".into(), JsonValue::Bool(false)),
            ]))
        }
        Some(ref enc) => {
            // Encrypted document: read V, R, P, /Length, CF methods.
            let v = match enc.get("V") {
                Some(Object::Integer(n)) => *n,
                _ => 0,
            };
            let r = match enc.get("R") {
                Some(Object::Integer(n)) => *n,
                _ => 0,
            };
            let p_raw = match enc.get("P") {
                Some(Object::Integer(n)) => *n as i32,
                _ => 0,
            };
            let bits = match enc.get("Length") {
                Some(Object::Integer(n)) => *n,
                // Default key length when /Length is absent: 40 bits (V=1/2).
                None => 40,
                // Malformed /Length (not an integer): treat as 0.
                _ => 0,
            };

            // Determine method strings from /StmF, /StrF, /EFF selectors.
            let stmf = dict_name_str(enc, "StmF");
            let strf = dict_name_str(enc, "StrF");
            let eff = dict_name_str(enc, "EFF");

            let (streammethod, stringmethod, filemethod) = if v >= 4 {
                let sm = cf_method_string(enc, stmf);
                let st = cf_method_string(enc, strf);
                let fm = cf_method_string(enc, eff.or(stmf));
                (sm, st, fm)
            } else if v == 1 || v == 2 {
                ("RC4", "RC4", "RC4")
            } else {
                ("none", "none", "none")
            };
            // top-level `method` mirrors streammethod (qpdf behaviour)
            let method = streammethod;

            let capabilities = JsonValue::Object(capabilities_from_p(p_raw));
            let parameters = JsonValue::Object(vec![
                ("P".into(), JsonValue::Integer(p_raw as i64)),
                ("R".into(), JsonValue::Integer(r)),
                ("V".into(), JsonValue::Integer(v)),
                ("bits".into(), JsonValue::Integer(bits)),
                ("filemethod".into(), JsonValue::String(filemethod.into())),
                ("key".into(), JsonValue::Null),
                ("method".into(), JsonValue::String(method.into())),
                (
                    "streammethod".into(),
                    JsonValue::String(streammethod.into()),
                ),
                (
                    "stringmethod".into(),
                    JsonValue::String(stringmethod.into()),
                ),
            ]);

            // ownerpasswordmatched / userpasswordmatched come from the
            // reader's authentication record so user-only-authenticated
            // documents do not falsely report owner=true.
            Ok(JsonValue::Object(vec![
                ("capabilities".into(), capabilities),
                ("encrypted".into(), JsonValue::Bool(is_encrypted)),
                (
                    "ownerpasswordmatched".into(),
                    JsonValue::Bool(pdf.owner_password_matched()),
                ),
                ("parameters".into(), parameters),
                ("recovereduserpassword".into(), JsonValue::Null),
                (
                    "userpasswordmatched".into(),
                    JsonValue::Bool(pdf.user_password_matched()),
                ),
            ]))
        }
    }
}

// ── JsonKey / filter_json_keys ────────────────────────────────────────────────

/// A top-level qpdf JSON v2 key that the caller may request via --json-key.
///
/// qpdf's v1-only `objects` and `objectinfo` selectors are intentionally not
/// represented: qpdf rejects both when JSON version 2 is selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JsonKey {
    Acroform,
    Attachments,
    Encrypt,
    Outlines,
    Pagelabels,
    Pages,
    Qpdf,
}

impl JsonKey {
    /// All qpdf JSON v2 key names in alphabetical order.
    pub const ALL_NAMES: &'static [&'static str] = &[
        "acroform",
        "attachments",
        "encrypt",
        "outlines",
        "pagelabels",
        "pages",
        "qpdf",
    ];

    /// Parse a key name string. Returns `None` for unknown keys.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "acroform" => Some(JsonKey::Acroform),
            "attachments" => Some(JsonKey::Attachments),
            "encrypt" => Some(JsonKey::Encrypt),
            "outlines" => Some(JsonKey::Outlines),
            "pagelabels" => Some(JsonKey::Pagelabels),
            "pages" => Some(JsonKey::Pages),
            "qpdf" => Some(JsonKey::Qpdf),
            _ => None,
        }
    }

    /// The v2 top-level key name represented by this selector.
    fn output_key_name(self) -> &'static str {
        match self {
            JsonKey::Acroform => "acroform",
            JsonKey::Attachments => "attachments",
            JsonKey::Encrypt => "encrypt",
            JsonKey::Outlines => "outlines",
            JsonKey::Pagelabels => "pagelabels",
            JsonKey::Pages => "pages",
            JsonKey::Qpdf => "qpdf",
        }
    }
}

/// Filter a fully-built qpdf JSON v2 document to only the requested keys.
///
/// `version` and `parameters` are always preserved (they are the envelope).
/// `keys` may contain duplicates; the result still contains each key at
/// most once. Returns the input unchanged when `keys` is empty (no filter).
///
/// Every selector maps directly to the same-named qpdf JSON v2 section.
pub fn filter_json_keys(v2: JsonValue, keys: &[JsonKey]) -> JsonValue {
    // No filter — return unchanged.
    if keys.is_empty() {
        return v2;
    }

    // Collect the set of resolved output-key names that were requested.
    let mut requested: std::collections::HashSet<&'static str> = std::collections::HashSet::new();
    for k in keys {
        requested.insert(k.output_key_name());
    }

    // Pattern-match the top-level Object; return as-is for any other variant.
    let pairs = match v2 {
        JsonValue::Object(p) => p,
        other => return other,
    };

    // Rebuild: always keep version + parameters first, then walk qpdf v2 order.
    let mut out: Vec<(String, JsonValue)> = Vec::new();

    // Pass 1: copy the envelope keys in their fixed order (version,
    // parameters) regardless of how they were laid out in the input. If
    // either key is absent, it is simply skipped — the function contract is
    // that the output never inverts the envelope order.
    for envelope_key in ["version", "parameters"] {
        if let Some((k, v)) = pairs.iter().find(|(k, _)| k == envelope_key) {
            out.push((k.clone(), v.clone()));
        }
    }

    // Pass 2: walk the fixed qpdf v2 emission order and pick requested keys.
    // Order: pages, pagelabels, acroform, attachments, encrypt, outlines, qpdf
    const V2_ORDER: &[&str] = &[
        "pages",
        "pagelabels",
        "acroform",
        "attachments",
        "encrypt",
        "outlines",
        "qpdf",
    ];
    for &section_name in V2_ORDER {
        if !requested.contains(section_name) {
            continue;
        }
        // Find this key in the input pairs (may be absent if caller built a
        // partial document — just skip rather than panic).
        if let Some((k, v)) = pairs.iter().find(|(k, _)| k == section_name) {
            out.push((k.clone(), v.clone()));
        }
    }

    JsonValue::Object(out)
}

// ── JsonObjectSelector / filter_json_objects ──────────────────────────────────

/// A `--json-object` selector. Either a specific (obj_num, generation) or
/// the special `trailer` token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JsonObjectSelector {
    /// A specific indirect object identified by (number, generation).
    Object { number: u32, generation: u16 },
    /// The trailer dictionary entry in the objects map.
    Trailer,
}

impl JsonObjectSelector {
    /// Parse a qpdf-style selector string: `"trailer"`, `"3"`, `"3,0"`.
    ///
    /// Returns `None` if the syntax is malformed (caller maps to the
    /// actionable error required by the acceptance criteria).
    ///
    /// Rules:
    /// - `"trailer"` → `Trailer` (exact lowercase match only)
    /// - `"N"` → `Object { number: N, generation: 0 }`
    /// - `"N,G"` → `Object { number: N, generation: G }`
    /// - More than 2 comma-separated parts, empty string, non-numeric
    ///   parts, negative numbers, or integer overflow → `None`.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        if s == "trailer" {
            return Some(JsonObjectSelector::Trailer);
        }
        if s.is_empty() {
            return None;
        }
        let parts: Vec<&str> = s.splitn(3, ',').collect();
        if parts.len() > 2 {
            return None;
        }
        // Reject leading '+' or any non-digit characters to match qpdf's strict parsing.
        let num_str = parts[0];
        if num_str.is_empty() || !num_str.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let number: u32 = num_str.parse().ok()?;

        let generation: u16 = if parts.len() == 2 {
            let gen_str = parts[1];
            if gen_str.is_empty() || !gen_str.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            gen_str.parse().ok()?
        } else {
            0
        };

        Some(JsonObjectSelector::Object { number, generation })
    }
}

/// Filter the `qpdf` top-level key's objects map to only the requested
/// selectors. `version`, `parameters`, and every other top-level section
/// (pages, pagelabels, etc.) are preserved untouched. The metadata
/// (`qpdf[0]`) is also preserved.
///
/// When `selectors` is empty, the input is returned unchanged.
///
/// Selectors that match no object in the input are silently dropped (the
/// resulting `qpdf[1]` is simply missing them), matching qpdf's behavior.
pub fn filter_json_objects(v2: JsonValue, selectors: &[JsonObjectSelector]) -> JsonValue {
    // No filter — return unchanged.
    if selectors.is_empty() {
        return v2;
    }

    // Build a HashSet of wanted object-map keys (deduped automatically).
    let mut wanted: std::collections::HashSet<String> =
        std::collections::HashSet::with_capacity(selectors.len());
    for sel in selectors {
        match sel {
            JsonObjectSelector::Object { number, generation } => {
                wanted.insert(format!("obj:{number} {generation} R"));
            }
            JsonObjectSelector::Trailer => {
                wanted.insert("trailer".to_string());
            }
        }
    }

    // Pattern-match the top-level Object; return as-is for any other variant.
    let mut pairs = match v2 {
        JsonValue::Object(p) => p,
        other => return other,
    };

    // Find the "qpdf" entry index; if absent return unchanged.
    let qpdf_idx = match pairs.iter().position(|(k, _)| k == "qpdf") {
        Some(i) => i,
        None => return JsonValue::Object(pairs),
    };

    // Extract the qpdf value and validate it is Array([metadata, objects_map]).
    let qpdf_val = std::mem::replace(
        &mut pairs[qpdf_idx].1,
        JsonValue::Null, // temporary placeholder
    );

    let mut arr = match qpdf_val {
        JsonValue::Array(a) if a.len() == 2 => a,
        other => {
            // Restore and return unchanged.
            pairs[qpdf_idx].1 = other;
            return JsonValue::Object(pairs);
        }
    };

    // arr[0] = metadata (preserve as-is), arr[1] = objects_map (filter).
    let objects_map = std::mem::replace(&mut arr[1], JsonValue::Null);

    let filtered_map = match objects_map {
        JsonValue::Object(obj_pairs) => {
            // Keep only pairs whose key is in `wanted`, preserving original order.
            let kept: Vec<(String, JsonValue)> = obj_pairs
                .into_iter()
                .filter(|(k, _)| wanted.contains(k))
                .collect();
            JsonValue::Object(kept)
        }
        other => {
            // Not an Object — restore and return unchanged.
            arr[1] = other;
            pairs[qpdf_idx].1 = JsonValue::Array(arr);
            return JsonValue::Object(pairs);
        }
    };

    arr[1] = filtered_map;
    pairs[qpdf_idx].1 = JsonValue::Array(arr);
    JsonValue::Object(pairs)
}

// ── build_qpdf_json_v2 (top-level composite) ─────────────────────────────────

/// Build the full qpdf JSON v2 document for `pdf`, combining the envelope
/// (`version`, `parameters`) with every section that flpdf currently
/// implements.
///
/// This produces: `version`, `parameters`, `pages`,
/// `pagelabels`, `acroform`, `attachments`, `encrypt`, `outlines`, `qpdf`.
/// Key order matches qpdf v2 output (fixed, not alphabetical).
///
/// Key order matches qpdf v2 output: top-level keys are emitted in the
/// fixed order shown in qpdf's `--json=2` output, not alphabetical.
///
/// This is a thin wrapper around [`build_qpdf_json_v2_with_options`] using
/// [`StreamDataMode::None`] (the default — stream entries contain `dict` only).
///
/// # Errors
///
/// Returns a [`ConvertError`] if any section builder fails.
pub fn build_qpdf_json_v2<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    decode_level: DecodeLevel,
) -> Result<JsonValue, ConvertError> {
    build_qpdf_json_v2_with_options(pdf, decode_level, &StreamDataMode::None)
}

/// Like [`build_qpdf_json_v2`], but also takes a [`StreamDataMode`] that is
/// forwarded to the `qpdf` top-level key builder.
///
/// # Errors
///
/// Returns a [`ConvertError`] if any section builder fails.
pub fn build_qpdf_json_v2_with_options<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    decode_level: DecodeLevel,
    stream_mode: &StreamDataMode,
) -> Result<JsonValue, ConvertError> {
    build_qpdf_json_v2_selected_with_options(pdf, decode_level, stream_mode, &[])
}

fn json_section_selected(keys: &[JsonKey], section: JsonKey) -> bool {
    keys.is_empty()
        || keys
            .iter()
            .any(|key| key.output_key_name() == section.output_key_name())
}

/// Build a qpdf JSON v2 document containing only the requested top-level
/// sections.
///
/// The `version` and `parameters` envelope is always present. An empty
/// `keys` slice selects every section. Selected sections are constructed in
/// qpdf's fixed order; unselected section builders are not called. This API
/// accepts only JSON v2 selectors; qpdf's v1-only `objects` and `objectinfo`
/// names are not aliases for `qpdf`.
///
/// # Errors
///
/// Returns a [`ConvertError`] if a selected section builder fails.
pub fn build_qpdf_json_v2_selected_with_options<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    decode_level: DecodeLevel,
    stream_mode: &StreamDataMode,
    keys: &[JsonKey],
) -> Result<JsonValue, ConvertError> {
    build_qpdf_json_v2_selected_objects_with_options(pdf, decode_level, stream_mode, keys, &[])
}

/// Build selected qpdf JSON v2 sections and serialize only the requested raw
/// qpdf objects after completing qpdf's all-xref preparation pass.
///
/// An empty `objects` slice emits every prepared object plus the trailer.
/// Non-empty selectors affect only the qpdf raw map; metadata preparation still
/// resolves every live xref object. Existing section selection semantics are
/// identical to [`build_qpdf_json_v2_selected_with_options`].
///
/// # Errors
///
/// Returns a [`ConvertError`] if a selected section or raw object cannot be
/// resolved or converted.
pub fn build_qpdf_json_v2_selected_objects_with_options<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    decode_level: DecodeLevel,
    stream_mode: &StreamDataMode,
    keys: &[JsonKey],
    objects: &[JsonObjectSelector],
) -> Result<JsonValue, ConvertError> {
    let mut pairs = match build_envelope(decode_level) {
        JsonValue::Object(p) => p,
        _ => unreachable!("build_envelope always returns an Object"),
    };

    if json_section_selected(keys, JsonKey::Pages) {
        let pages = build_pages_section(pdf)?;
        pairs.push(("pages".to_string(), pages));
    }

    if json_section_selected(keys, JsonKey::Pagelabels) {
        let pagelabels = build_pagelabels_section(pdf)?;
        pairs.push(("pagelabels".to_string(), pagelabels));
    }

    if json_section_selected(keys, JsonKey::Acroform) {
        let acroform = build_acroform_section(pdf)?;
        pairs.push(("acroform".to_string(), acroform));
    }

    if json_section_selected(keys, JsonKey::Attachments) {
        let attachments = build_attachments_section(pdf)?;
        pairs.push(("attachments".to_string(), attachments));
    }

    if json_section_selected(keys, JsonKey::Encrypt) {
        let encrypt = build_encrypt_section(pdf)?;
        pairs.push(("encrypt".to_string(), encrypt));
    }

    if json_section_selected(keys, JsonKey::Outlines) {
        let outlines = build_outlines_section(pdf)?;
        pairs.push(("outlines".to_string(), outlines));
    }

    if json_section_selected(keys, JsonKey::Qpdf) {
        // qpdf resolves every live xref object before reading the object-cache
        // maximum. Parsing those bodies registers dangling generations as
        // placeholders, while unrelated free xref entries remain excluded.
        let prepared = pdf.prepare_qpdf_json_objects()?;
        let qpdf_metadata = QpdfMetadata {
            pdf_version: pdf.version().to_string(),
            max_object_id: prepared.max_object_id,
            pushed_inherited_page_resources: false,
            called_get_all_pages: pdf.ever_called_get_all_pages(),
        };
        let qpdf = build_qpdf_key_selected_with_stream_mode(
            pdf,
            qpdf_metadata,
            decode_level,
            stream_mode,
            objects,
            &prepared.refs,
        )?; // cov:ignore: Err propagation requires an I/O failure after the Pdf has opened
        pairs.push(("qpdf".to_string(), qpdf));
    }

    Ok(JsonValue::Object(pairs))
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::write;

    fn emit(v: &JsonValue) -> String {
        let mut buf = Vec::new();
        write(v, &mut buf).expect("write failed");
        String::from_utf8(buf).expect("not utf-8")
    }

    // Minimal valid PDF; nodes are supplied via set_object refs (catalog unused).
    fn empty_pdf() -> Pdf<std::io::Cursor<Vec<u8>>> {
        use std::io::Cursor;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"%PDF-1.4\n");
        let off1 = bytes.len() as u64;
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        let xref = bytes.len() as u64;
        bytes.extend_from_slice(
            format!(
                "xref\n0 2\n0000000000 65535 f \n{off1:010} 00000 n \ntrailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        Pdf::open(Cursor::new(bytes)).expect("open")
    }

    // Register a /Parent chain obj(start)->obj(start+1)->...->obj(start+len-1).
    // The deepest node carries `key`; the starting dict (returned) only has /Parent.
    fn parent_chain(
        pdf: &mut Pdf<std::io::Cursor<Vec<u8>>>,
        start: u32,
        len: u32,
        key: &str,
    ) -> Dictionary {
        for i in 0..len {
            let num = start + i;
            let mut d = Dictionary::new();
            if i + 1 < len {
                d.insert(
                    "Parent",
                    Object::Reference(crate::ObjectRef::new(num + 1, 0)),
                );
            } else {
                // deepest node holds the inheritable value
                d.insert(key, Object::Integer(42));
            }
            pdf.set_object(crate::ObjectRef::new(num, 0), Object::Dictionary(d));
        }
        let mut start_dict = Dictionary::new();
        start_dict.insert("Parent", Object::Reference(crate::ObjectRef::new(start, 0)));
        start_dict
    }

    #[test]
    fn inherited_field_value_errors_on_excessive_parent_depth() {
        use crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH;
        let mut pdf = empty_pdf();
        let start_dict = parent_chain(&mut pdf, 2, (DEFAULT_MAX_PAGE_TREE_DEPTH as u32) + 5, "V");
        let err = inherited_field_value(&mut pdf, &start_dict, "V");
        assert!(matches!(err, Err(ConvertError::PdfError(_))));
    }

    #[test]
    fn inherited_field_value_resolves_within_limit() {
        let mut pdf = empty_pdf();
        let start_dict = parent_chain(&mut pdf, 2, 4, "V");
        let got = inherited_field_value(&mut pdf, &start_dict, "V").unwrap();
        assert_eq!(got, Some(Object::Integer(42)));
    }

    // ── 1. Default (Generalized) envelope matches qpdf structural output ──────

    #[test]
    fn envelope_generalized_structural_match() {
        let envelope = build_envelope(DecodeLevel::Generalized);
        let out = emit(&envelope);
        // Must contain the two top-level fields and the decodelevel value.
        assert!(
            out.contains("\"version\": 2"),
            "missing version field: {out}"
        );
        assert!(
            out.contains("\"decodelevel\": \"generalized\""),
            "missing decodelevel: {out}"
        );
        // Must end with a single trailing newline.
        assert!(out.ends_with('\n'), "missing trailing newline");
        // Full structural match (no extra sections like pages/objects).
        let expected = "{\n  \"version\": 2,\n  \"parameters\": {\n    \"decodelevel\": \"generalized\"\n  }\n}\n";
        assert_eq!(out, expected, "structural mismatch");
    }

    // ── 2. All other DecodeLevel variants produce the right string ────────────

    #[test]
    fn envelope_none_decodelevel() {
        let out = emit(&build_envelope(DecodeLevel::None));
        assert!(
            out.contains("\"decodelevel\": \"none\""),
            "wrong decodelevel: {out}"
        );
    }

    #[test]
    fn envelope_specialized_decodelevel() {
        let out = emit(&build_envelope(DecodeLevel::Specialized));
        assert!(
            out.contains("\"decodelevel\": \"specialized\""),
            "wrong decodelevel: {out}"
        );
    }

    #[test]
    fn envelope_all_decodelevel() {
        let out = emit(&build_envelope(DecodeLevel::All));
        assert!(
            out.contains("\"decodelevel\": \"all\""),
            "wrong decodelevel: {out}"
        );
    }

    // ── 3. Key order: version must appear before parameters ───────────────────

    #[test]
    fn key_order_version_before_parameters() {
        let envelope = build_envelope(DecodeLevel::Generalized);
        let out = emit(&envelope);
        let version_pos = out.find("\"version\"").expect("version not found");
        let parameters_pos = out.find("\"parameters\"").expect("parameters not found");
        assert!(
            version_pos < parameters_pos,
            "version must appear before parameters, got: {out}"
        );
    }

    // ── 4. parameters object contains exactly one key (decodelevel only) ──────

    #[test]
    fn parameters_has_only_decodelevel() {
        let envelope = build_envelope(DecodeLevel::Generalized);
        // Inspect the JsonValue directly, not the string.
        let JsonValue::Object(pairs) = &envelope else {
            panic!("envelope is not an Object");
        };
        let (_, params_val) = pairs
            .iter()
            .find(|(k, _)| k == "parameters")
            .expect("parameters key not found");
        let JsonValue::Object(param_pairs) = params_val else {
            panic!("parameters is not an Object");
        };
        assert_eq!(
            param_pairs.len(),
            1,
            "parameters must have exactly 1 key, got: {:?}",
            param_pairs.iter().map(|(k, _)| k).collect::<Vec<_>>()
        );
        assert_eq!(
            param_pairs[0].0, "decodelevel",
            "the single key must be 'decodelevel'"
        );
    }

    // ── 5. Default is Generalized ─────────────────────────────────────────────

    #[test]
    fn default_decode_level_is_generalized() {
        assert_eq!(DecodeLevel::default(), DecodeLevel::Generalized);
    }

    // ── 6. as_qpdf_str covers all variants ───────────────────────────────────

    #[test]
    fn as_qpdf_str_all_variants() {
        assert_eq!(DecodeLevel::None.as_qpdf_str(), "none");
        assert_eq!(DecodeLevel::Generalized.as_qpdf_str(), "generalized");
        assert_eq!(DecodeLevel::Specialized.as_qpdf_str(), "specialized");
        assert_eq!(DecodeLevel::All.as_qpdf_str(), "all");
    }

    // ══════════════════════════════════════════════════════════════════════════
    // pdf_object_to_json unit tests
    // ══════════════════════════════════════════════════════════════════════════

    // ── 7. Boolean conversion ─────────────────────────────────────────────────

    #[test]
    fn object_bool_true_to_json() {
        assert_eq!(
            pdf_object_to_json(&Object::Boolean(true)).unwrap(),
            JsonValue::Bool(true)
        );
    }

    #[test]
    fn object_bool_false_to_json() {
        assert_eq!(
            pdf_object_to_json(&Object::Boolean(false)).unwrap(),
            JsonValue::Bool(false)
        );
    }

    // ── 8. Integer conversion ─────────────────────────────────────────────────

    #[test]
    fn object_integer_to_json() {
        assert_eq!(
            pdf_object_to_json(&Object::Integer(42)).unwrap(),
            JsonValue::Integer(42)
        );
        assert_eq!(
            pdf_object_to_json(&Object::Integer(-7)).unwrap(),
            JsonValue::Integer(-7)
        );
    }

    // ── 9. Real (float) conversion ────────────────────────────────────────────

    #[test]
    fn object_real_to_json() {
        assert_eq!(
            pdf_object_to_json(&Object::Real(1.5)).unwrap(),
            JsonValue::Float(1.5)
        );
    }

    #[test]
    fn object_real_non_finite_returns_error() {
        assert_eq!(
            pdf_object_to_json(&Object::Real(f64::NAN)),
            Err(ConvertError::NonFiniteFloat)
        );
        assert_eq!(
            pdf_object_to_json(&Object::Real(f64::INFINITY)),
            Err(ConvertError::NonFiniteFloat)
        );
    }

    // ── 10. Name conversion ───────────────────────────────────────────────────

    #[test]
    fn object_name_to_json() {
        assert_eq!(
            pdf_object_to_json(&Object::Name(b"Type".to_vec())).unwrap(),
            JsonValue::String("/Type".to_string())
        );
        assert_eq!(
            pdf_object_to_json(&Object::Name(b"Font".to_vec())).unwrap(),
            JsonValue::String("/Font".to_string())
        );
    }

    // ── 11. String conversion (text vs binary) ────────────────────────────────

    #[test]
    fn qpdf_utf8_value_decodes_all_qpdf_string_encodings() {
        assert_eq!(qpdf_utf8_value(b"plain"), b"plain");
        assert_eq!(
            qpdf_utf8_value(&[0xef, 0xbb, 0xbf, 0xe5, 0x90, 0x8d]),
            "名".as_bytes()
        );
        assert_eq!(
            qpdf_utf8_value(&[0xfe, 0xff, 0x54, 0x0d, 0x52, 0x4d]),
            "名前".as_bytes()
        );
        assert_eq!(
            qpdf_utf8_value(&[0xff, 0xfe, 0x0d, 0x54, 0x4d, 0x52]),
            "名前".as_bytes()
        );
        assert_eq!(qpdf_utf8_value(&[0x95]), "Ł".as_bytes());
    }

    #[test]
    fn qpdf_utf8_value_replaces_undefined_pdfdoc_byte() {
        assert_eq!(
            qpdf_utf8_value(&[b'a', 0xad, b'b']),
            "a\u{fffd}b".as_bytes()
        );
    }

    #[test]
    fn qpdf_utf8_value_preserves_invalid_explicit_utf8_bytes() {
        assert_eq!(qpdf_utf8_value(&[0xef, 0xbb, 0xbf, 0xff]), &[0xff]);
    }

    #[test]
    fn object_string_ascii_text_has_u_prefix() {
        let result = pdf_object_to_json(&Object::String(b"hello".to_vec())).unwrap();
        assert_eq!(result, JsonValue::String("u:hello".to_string()));
    }

    #[test]
    fn object_string_binary_has_b_prefix() {
        // 0x01 is unassigned in PDFDocEncoding (no UTF-16 BOM either), so the
        // string is not decodable as PDF text and must fall back to "b:" hex.
        let result = pdf_object_to_json(&Object::String(vec![0x2d, 0x01, 0x80])).unwrap();
        assert_eq!(result, JsonValue::String("b:2d0180".to_string()));
    }

    #[test]
    fn object_string_pdfdoc_high_byte_too_dense_falls_back_to_binary() {
        // 0xC7 ("Ç" in PDFDocEncoding) counts as non-ASCII under qpdf's
        // useHexString() heuristic. With non_ascii=1 and len=3, 5*1 > 3 so
        // qpdf emits b:<hex>; flpdf matches that.
        let result = pdf_object_to_json(&Object::String(vec![b'A', 0xC7, b'B'])).unwrap();
        assert_eq!(result, JsonValue::String("b:41c742".to_string()));
    }

    #[test]
    fn object_string_pdfdoc_high_byte_sparse_decodes_as_text() {
        // With non_ascii=1 and len=16, 5*1 = 5 ≤ 16 → qpdf attempts the
        // PDFDocEncoding round-trip and emits u:<text>. 0xC7 → "Ç".
        let bytes: Vec<u8> = b"the quick"
            .iter()
            .copied()
            .chain(std::iter::once(0xC7u8))
            .chain(b"brown!!".iter().copied())
            .collect();
        let result = pdf_object_to_json(&Object::String(bytes)).unwrap();
        assert_eq!(
            result,
            JsonValue::String("u:the quick\u{00C7}brown!!".to_string())
        );
    }

    #[test]
    fn object_string_utf16be_bom_decodes_to_unicode() {
        // FEFF + 0041 + 0042 → "u:AB"
        let bytes = vec![0xFE, 0xFF, 0x00, 0x41, 0x00, 0x42];
        let result = pdf_object_to_json(&Object::String(bytes)).unwrap();
        assert_eq!(result, JsonValue::String("u:AB".to_string()));
    }

    #[test]
    fn object_string_utf16le_bom_decodes_to_unicode() {
        // FFFE + 41 00 + 42 00 → "u:AB"
        let bytes = vec![0xFF, 0xFE, 0x41, 0x00, 0x42, 0x00];
        let result = pdf_object_to_json(&Object::String(bytes)).unwrap();
        assert_eq!(result, JsonValue::String("u:AB".to_string()));
    }

    #[test]
    fn object_string_utf16be_japanese_decodes_to_unicode() {
        // FEFF + 3042 (あ) + 3044 (い) → "u:あい"
        let bytes = vec![0xFE, 0xFF, 0x30, 0x42, 0x30, 0x44];
        let result = pdf_object_to_json(&Object::String(bytes)).unwrap();
        assert_eq!(result, JsonValue::String("u:あい".to_string()));
    }

    #[test]
    fn object_string_utf16be_with_odd_length_drops_trailing_byte() {
        // FEFF + 0041 + 00 (truncated last unit). qpdf silently ignores
        // the trailing odd byte and emits u:A; flpdf matches that.
        let bytes = vec![0xFE, 0xFF, 0x00, 0x41, 0x00];
        let result = pdf_object_to_json(&Object::String(bytes)).unwrap();
        assert_eq!(result, JsonValue::String("u:A".to_string()));
    }

    #[test]
    fn object_string_random_md5_emits_hex() {
        // 16 random bytes (MD5-shaped /ID payload) — well over the 20%
        // non-ASCII threshold of useHexString(), so qpdf emits b:<hex>.
        let bytes = vec![
            0xd4, 0x1d, 0x8c, 0xd9, 0x8f, 0x00, 0xb2, 0x04, 0xe9, 0x80, 0x09, 0x98, 0xec, 0xf8,
            0x42, 0x7e,
        ];
        let result = pdf_object_to_json(&Object::String(bytes)).unwrap();
        assert_eq!(
            result,
            JsonValue::String("b:d41d8cd98f00b204e9800998ecf8427e".to_string())
        );
    }

    #[test]
    fn object_string_single_x1e_forces_hex() {
        // 0x1E falls in the 0x18..=0x1F range that useHexString counts as
        // non_ascii; with len=3 the 5*non_ascii > len threshold triggers.
        let result = pdf_object_to_json(&Object::String(vec![b'A', 0x1E, b'B'])).unwrap();
        assert_eq!(result, JsonValue::String("b:411e42".to_string()));
    }

    #[test]
    fn object_string_del_0x7f_forces_hex() {
        // 0x7F (DEL) is counted as non_ascii by qpdf.
        let result = pdf_object_to_json(&Object::String(vec![b'A', 0x7F, b'B'])).unwrap();
        assert_eq!(result, JsonValue::String("b:417f42".to_string()));
    }

    #[test]
    fn object_string_explicit_utf8_bom_decodes() {
        // EF BB BF + "AB" → u:AB. The BOM is stripped; the remainder is
        // emitted as a UTF-8 string.
        let bytes = vec![0xEF, 0xBB, 0xBF, b'A', b'B'];
        let result = pdf_object_to_json(&Object::String(bytes)).unwrap();
        assert_eq!(result, JsonValue::String("u:AB".to_string()));
    }

    #[test]
    fn object_string_explicit_utf8_bom_non_ascii() {
        // EF BB BF + "café" (UTF-8 bytes for café) → u:café.
        let bytes = vec![0xEF, 0xBB, 0xBF, b'c', b'a', b'f', 0xC3, 0xA9];
        let result = pdf_object_to_json(&Object::String(bytes)).unwrap();
        assert_eq!(result, JsonValue::String("u:café".to_string()));
    }

    #[test]
    fn object_string_with_nul_falls_back_to_binary() {
        // 0x00 (NUL) is below 0x20 and not one of the allowed control bytes
        // (\b \t \n \f \r), so use_hex_string short-circuits to true and the
        // whole string is emitted as b:<hex> — the qpdf-equivalent treatment
        // for an /ID array element that contains a NUL.
        let bytes = vec![0xab, 0xcd, 0x00, 0xef];
        let result = pdf_object_to_json(&Object::String(bytes)).unwrap();
        assert_eq!(result, JsonValue::String("b:abcd00ef".to_string()));
    }

    #[test]
    fn object_string_empty_is_text() {
        let result = pdf_object_to_json(&Object::String(vec![])).unwrap();
        assert_eq!(result, JsonValue::String("u:".to_string()));
    }

    // ── 12. Reference conversion ──────────────────────────────────────────────

    #[test]
    fn object_reference_to_json() {
        use crate::ObjectRef;
        let result = pdf_object_to_json(&Object::Reference(ObjectRef::new(2, 0))).unwrap();
        assert_eq!(result, JsonValue::String("2 0 R".to_string()));
    }

    // ── 13. Null conversion ───────────────────────────────────────────────────

    #[test]
    fn object_null_to_json() {
        assert_eq!(pdf_object_to_json(&Object::Null).unwrap(), JsonValue::Null);
    }

    // ── 14. Array conversion ──────────────────────────────────────────────────

    #[test]
    fn object_array_to_json() {
        let arr = Object::Array(vec![
            Object::Integer(1),
            Object::Boolean(true),
            Object::Null,
        ]);
        let result = pdf_object_to_json(&arr).unwrap();
        assert_eq!(
            result,
            JsonValue::Array(vec![
                JsonValue::Integer(1),
                JsonValue::Bool(true),
                JsonValue::Null,
            ])
        );
    }

    // ── 15. Dictionary conversion with alphabetical key sort ──────────────────

    #[test]
    fn object_dict_to_json_keys_alphabetical() {
        use crate::object::Dictionary;
        let mut dict = Dictionary::new();
        dict.insert("Zebra", Object::Integer(1));
        dict.insert("Apple", Object::Integer(2));
        dict.insert("Mango", Object::Integer(3));
        let result = pdf_object_to_json(&Object::Dictionary(dict)).unwrap();
        let JsonValue::Object(pairs) = result else {
            panic!("expected Object, got {:?}", result);
        };
        // Keys should be in alphabetical order: /Apple, /Mango, /Zebra
        let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["/Apple", "/Mango", "/Zebra"]);
    }

    // ── 16. Stream nested in container yields stream shape ────────────────────

    #[test]
    fn object_stream_nested_yields_stream_shape() {
        use crate::object::{Dictionary, Stream};
        let stream = Object::Stream(Stream::new(Dictionary::new(), vec![]));
        let result = pdf_object_to_json(&stream).unwrap();
        let JsonValue::Object(pairs) = &result else {
            panic!("expected Object");
        };
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "stream");
    }

    // ══════════════════════════════════════════════════════════════════════════
    // build_qpdf_key integration tests (one-page.pdf fixture)
    // ══════════════════════════════════════════════════════════════════════════

    fn load_one_page_pdf() -> crate::Pdf<std::io::Cursor<Vec<u8>>> {
        // CARGO_MANIFEST_DIR points to crates/flpdf; the fixture lives at
        // <workspace-root>/tests/fixtures/compat/one-page.pdf, which is two
        // levels up from the crate manifest.
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let fixture = manifest.join("../../tests/fixtures/compat/one-page.pdf");
        let bytes = std::fs::read(&fixture)
            .unwrap_or_else(|e| panic!("one-page.pdf not found at {}: {e}", fixture.display()));
        crate::Pdf::open_mem_owned(bytes).expect("failed to open one-page.pdf")
    }

    fn load_three_page_pdf() -> crate::Pdf<std::io::Cursor<Vec<u8>>> {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let fixture = manifest.join("../../tests/fixtures/compat/three-page.pdf");
        let bytes = std::fs::read(&fixture)
            .unwrap_or_else(|e| panic!("three-page.pdf not found at {}: {e}", fixture.display()));
        crate::Pdf::open_mem_owned(bytes).expect("failed to open three-page.pdf")
    }

    // ── 17. build_qpdf_key returns a 2-element array ──────────────────────────

    #[test]
    fn build_qpdf_key_returns_two_element_array() {
        let mut pdf = load_one_page_pdf();
        let meta = QpdfMetadata {
            pdf_version: "1.3".to_string(),
            max_object_id: 7,
            pushed_inherited_page_resources: false,
            called_get_all_pages: true,
        };
        let result = build_qpdf_key(&mut pdf, meta).expect("build_qpdf_key failed");
        let JsonValue::Array(elems) = result else {
            panic!("expected Array, got {:?}", result);
        };
        assert_eq!(
            elems.len(),
            2,
            "expected 2 elements (metadata + objects_map)"
        );
    }

    // ── 18. Metadata object has correct keys in fixed order ───────────────────

    #[test]
    fn build_qpdf_key_metadata_keys_and_values() {
        let mut pdf = load_one_page_pdf();
        let meta = QpdfMetadata {
            pdf_version: "1.3".to_string(),
            max_object_id: 7,
            pushed_inherited_page_resources: false,
            called_get_all_pages: true,
        };
        let JsonValue::Array(elems) =
            build_qpdf_key(&mut pdf, meta).expect("build_qpdf_key failed")
        else {
            panic!("expected Array");
        };
        let JsonValue::Object(meta_pairs) = &elems[0] else {
            panic!("metadata is not an Object");
        };
        // Check fixed key order: jsonversion, pdfversion, pushedinheritedpageresources,
        // calledgetallpages, maxobjectid
        assert_eq!(meta_pairs[0].0, "jsonversion");
        assert_eq!(meta_pairs[0].1, JsonValue::Integer(2));
        assert_eq!(meta_pairs[1].0, "pdfversion");
        assert_eq!(meta_pairs[1].1, JsonValue::String("1.3".to_string()));
        assert_eq!(meta_pairs[2].0, "pushedinheritedpageresources");
        assert_eq!(meta_pairs[2].1, JsonValue::Bool(false));
        assert_eq!(meta_pairs[3].0, "calledgetallpages");
        assert_eq!(meta_pairs[3].1, JsonValue::Bool(true));
        assert_eq!(meta_pairs[4].0, "maxobjectid");
        assert_eq!(meta_pairs[4].1, JsonValue::Integer(7));
    }

    // ── 19. objects_map has the expected keys ─────────────────────────────────

    #[test]
    fn build_qpdf_key_objects_map_has_expected_keys() {
        let mut pdf = load_one_page_pdf();
        let meta = QpdfMetadata {
            pdf_version: "1.3".to_string(),
            max_object_id: 7,
            pushed_inherited_page_resources: false,
            called_get_all_pages: true,
        };
        let JsonValue::Array(elems) =
            build_qpdf_key(&mut pdf, meta).expect("build_qpdf_key failed")
        else {
            panic!("expected Array");
        };
        let JsonValue::Object(map_pairs) = &elems[1] else {
            panic!("objects_map is not an Object");
        };
        let keys: Vec<&str> = map_pairs.iter().map(|(k, _)| k.as_str()).collect();
        // one-page.pdf has objects 1..7 (some may be free) plus trailer.
        // At minimum, trailer must be present.
        assert!(keys.contains(&"trailer"), "trailer key missing: {keys:?}");
        // Exactly 7 object entries + 1 trailer = 8 keys total (one-page.pdf has
        // objects 1..7, all of which are live).
        assert_eq!(
            map_pairs.len(),
            8,
            "expected 7 objs + trailer, got {keys:?}"
        );
        // Keys must be alphabetically sorted.
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted, "objects_map keys are not sorted: {keys:?}");
    }

    // ── 20. obj:7 0 R is a stream ─────────────────────────────────────────────

    #[test]
    fn build_qpdf_key_obj7_is_stream() {
        let mut pdf = load_one_page_pdf();
        let meta = QpdfMetadata {
            pdf_version: "1.3".to_string(),
            max_object_id: 7,
            pushed_inherited_page_resources: false,
            called_get_all_pages: true,
        };
        let JsonValue::Array(elems) =
            build_qpdf_key(&mut pdf, meta).expect("build_qpdf_key failed")
        else {
            panic!("expected Array");
        };
        let JsonValue::Object(map_pairs) = &elems[1] else {
            panic!("objects_map is not an Object");
        };
        let obj7 = map_pairs
            .iter()
            .find(|(k, _)| k == "obj:7 0 R")
            .map(|(_, v)| v)
            .expect("obj:7 0 R not found");
        // Must be { "stream": { "dict": { ... } } }
        let JsonValue::Object(obj7_pairs) = obj7 else {
            panic!("obj:7 0 R is not an Object: {obj7:?}");
        };
        assert_eq!(obj7_pairs[0].0, "stream", "obj:7 must have 'stream' key");
        let JsonValue::Object(stream_inner) = &obj7_pairs[0].1 else {
            panic!("stream value is not an Object");
        };
        assert_eq!(stream_inner[0].0, "dict", "stream must have 'dict' key");
    }

    // ── 21. trailer has a value wrapper ──────────────────────────────────────

    #[test]
    fn build_qpdf_key_trailer_has_value_wrapper() {
        let mut pdf = load_one_page_pdf();
        let meta = QpdfMetadata {
            pdf_version: "1.3".to_string(),
            max_object_id: 7,
            pushed_inherited_page_resources: false,
            called_get_all_pages: true,
        };
        let JsonValue::Array(elems) =
            build_qpdf_key(&mut pdf, meta).expect("build_qpdf_key failed")
        else {
            panic!("expected Array");
        };
        let JsonValue::Object(map_pairs) = &elems[1] else {
            panic!("objects_map is not an Object");
        };
        let trailer = map_pairs
            .iter()
            .find(|(k, _)| k == "trailer")
            .map(|(_, v)| v)
            .expect("trailer not found");
        let JsonValue::Object(trailer_pairs) = trailer else {
            panic!("trailer is not an Object");
        };
        assert_eq!(trailer_pairs[0].0, "value", "trailer must have 'value' key");
        // /Size should be Integer(8)
        let JsonValue::Object(trailer_dict) = &trailer_pairs[0].1 else {
            panic!("trailer.value is not an Object");
        };
        let size = trailer_dict
            .iter()
            .find(|(k, _)| k == "/Size")
            .map(|(_, v)| v)
            .expect("/Size not found in trailer");
        assert_eq!(*size, JsonValue::Integer(8), "/Size should be 8");
    }

    // ── 22. Live null indirect object is emitted, not silently dropped ────────
    //
    // Regression test for the earlier `Object::Null => continue` bug: a live
    // indirect object that *is* null (e.g. `1 0 obj null endobj`) must appear
    // in objects_map as `{ "value": null }`, just like a non-null live object.
    // qpdf does the same.

    #[test]
    fn build_qpdf_key_live_null_indirect_object_is_emitted_with_value_null() {
        let mut pdf = load_one_page_pdf();

        // Patch obj 2 (the Font dictionary in one-page.pdf) to a live null
        // indirect object. The xref entry remains live; only the resolved
        // value becomes Null. build_qpdf_key must still emit obj:2 0 R.
        pdf.set_object(crate::ObjectRef::new(2, 0), Object::Null);

        let meta = QpdfMetadata {
            pdf_version: "1.3".to_string(),
            max_object_id: 7,
            pushed_inherited_page_resources: false,
            called_get_all_pages: true,
        };
        let JsonValue::Array(elems) =
            build_qpdf_key(&mut pdf, meta).expect("build_qpdf_key failed")
        else {
            panic!("expected Array");
        };
        let JsonValue::Object(map_pairs) = &elems[1] else {
            panic!("objects_map is not an Object");
        };

        let obj2 = map_pairs
            .iter()
            .find(|(k, _)| k == "obj:2 0 R")
            .map(|(_, v)| v)
            .expect("obj:2 0 R must remain in objects_map when it is live and null");
        let JsonValue::Object(obj2_pairs) = obj2 else {
            panic!("obj:2 0 R is not an Object");
        };
        assert_eq!(
            obj2_pairs.len(),
            1,
            "live null indirect must have a single 'value' key"
        );
        assert_eq!(obj2_pairs[0].0, "value");
        assert_eq!(obj2_pairs[0].1, JsonValue::Null);
    }

    // ── 23. Pdf::live_object_refs() unit check ────────────────────────────────
    //
    // Direct check that live_object_refs() returns the same set of live refs
    // as object_refs() on a fixture with no free entries, and that an
    // explicitly deleted object is excluded.

    // ── 24. PDF Name escape via #XX (ISO 32000-1 §7.3.5) ──────────────────────
    //
    // Names containing non-UTF8 bytes, delimiters, whitespace, or `#` itself
    // must round-trip losslessly through the JSON output. The earlier
    // implementation used String::from_utf8_lossy and replaced invalid bytes
    // with U+FFFD, permanently dropping information.

    #[test]
    fn pdf_object_to_json_name_with_non_utf8_byte_escapes_as_hex() {
        // /A followed by 0xFF — invalid UTF-8 in the raw name bytes.
        let obj = Object::Name(b"A\xffB".to_vec());
        let json = pdf_object_to_json(&obj).unwrap();
        assert_eq!(json, JsonValue::String("/A#ffB".to_string()));
    }

    #[test]
    fn pdf_object_to_json_name_with_delimiters_escapes_them() {
        // Each PDF delimiter / whitespace / `#` must be emitted as #XX.
        let obj = Object::Name(b"a b#(c)".to_vec());
        let json = pdf_object_to_json(&obj).unwrap();
        assert_eq!(
            json,
            JsonValue::String("/a#20b#23#28c#29".to_string()),
            "space, #, (, ) must all be hex-escaped"
        );
    }

    #[test]
    fn pdf_object_to_json_name_with_only_safe_bytes_is_passthrough() {
        // Plain ASCII names are emitted unchanged.
        let obj = Object::Name(b"Helvetica".to_vec());
        let json = pdf_object_to_json(&obj).unwrap();
        assert_eq!(json, JsonValue::String("/Helvetica".to_string()));
    }

    #[test]
    fn dict_to_json_keys_with_non_utf8_bytes_use_hex_escape() {
        // Dictionary keys go through the same escape path as Object::Name.
        let mut dict = Dictionary::new();
        dict.insert(b"K\xffey", Object::Integer(7));
        let json = pdf_object_to_json(&Object::Dictionary(dict)).unwrap();
        let JsonValue::Object(pairs) = json else {
            panic!("expected Object");
        };
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "/K#ffey");
        assert_eq!(pairs[0].1, JsonValue::Integer(7));
    }

    #[test]
    fn pdf_object_to_json_name_with_control_byte_escapes() {
        // Control bytes (here, 0x01) are not printable and must be hex-escaped.
        let obj = Object::Name(b"x\x01y".to_vec());
        let json = pdf_object_to_json(&obj).unwrap();
        assert_eq!(json, JsonValue::String("/x#01y".to_string()));
    }

    // ── 25. Pdf::live_object_refs() unit check ────────────────────────────────

    #[test]
    fn live_object_refs_excludes_explicitly_deleted_entries() {
        let mut pdf = load_one_page_pdf();
        let before: std::collections::BTreeSet<_> = pdf.live_object_refs().into_iter().collect();
        assert!(
            before.contains(&crate::ObjectRef::new(2, 0)),
            "obj 2 should start out as live"
        );

        pdf.delete_object(crate::ObjectRef::new(2, 0));
        let after: std::collections::BTreeSet<_> = pdf.live_object_refs().into_iter().collect();
        assert!(
            !after.contains(&crate::ObjectRef::new(2, 0)),
            "obj 2 must drop out of live_object_refs after delete_object"
        );
        assert_eq!(
            before.len() - 1,
            after.len(),
            "exactly one ref should be removed"
        );
    }

    // ══════════════════════════════════════════════════════════════════════════
    // build_pages_section tests (flpdf-9hc.11.4)
    // ══════════════════════════════════════════════════════════════════════════

    fn load_fixture_pdf(name: &str) -> crate::Pdf<std::io::Cursor<Vec<u8>>> {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let fixture = manifest.join("../../tests/fixtures/compat").join(name);
        let bytes = std::fs::read(&fixture)
            .unwrap_or_else(|e| panic!("{name} not found at {}: {e}", fixture.display()));
        crate::Pdf::open_mem_owned(bytes).unwrap_or_else(|e| panic!("failed to open {name}: {e}"))
    }

    // Helper: get page entry at index from build_pages_section result.
    fn get_page_entry(pages: &JsonValue, idx: usize) -> &[(String, JsonValue)] {
        let JsonValue::Array(arr) = pages else {
            panic!("pages section is not an Array");
        };
        let JsonValue::Object(pairs) = &arr[idx] else {
            panic!("page entry {idx} is not an Object");
        };
        pairs.as_slice()
    }

    // ── 26. one-page.pdf: pages array length ─────────────────────────────────

    #[test]
    fn build_pages_section_one_page_length() {
        let mut pdf = load_fixture_pdf("one-page.pdf");
        let pages = build_pages_section(&mut pdf).expect("build_pages_section failed");
        let JsonValue::Array(arr) = &pages else {
            panic!("expected Array");
        };
        assert_eq!(arr.len(), 1, "one-page.pdf must have exactly 1 page entry");
    }

    // ── 27. one-page.pdf: key order is alphabetical ───────────────────────────

    #[test]
    fn build_pages_section_one_page_key_order() {
        let mut pdf = load_fixture_pdf("one-page.pdf");
        let pages = build_pages_section(&mut pdf).expect("build_pages_section failed");
        let pairs = get_page_entry(&pages, 0);
        let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "contents",
                "images",
                "label",
                "object",
                "outlines",
                "pageposfrom1"
            ],
            "key order must be strictly alphabetical"
        );
    }

    // ── 28. one-page.pdf: entry values match qpdf --json=2 --json-key=pages ──

    #[test]
    fn build_pages_section_one_page_values() {
        // Expected from: qpdf --json=2 --json-key=pages one-page.pdf
        // contents: ["7 0 R"], images: [], label: null, object: "3 0 R",
        // outlines: [], pageposfrom1: 1
        let mut pdf = load_fixture_pdf("one-page.pdf");
        let pages = build_pages_section(&mut pdf).expect("build_pages_section failed");
        let pairs = get_page_entry(&pages, 0);

        // contents = ["7 0 R"]
        assert_eq!(
            pairs[0].1,
            JsonValue::Array(vec![JsonValue::String("7 0 R".to_string())]),
            "contents mismatch"
        );
        // images = []
        assert_eq!(pairs[1].1, JsonValue::Array(vec![]), "images must be empty");
        // label = null
        assert_eq!(pairs[2].1, JsonValue::Null, "label must be null");
        // object = "3 0 R"
        assert_eq!(
            pairs[3].1,
            JsonValue::String("3 0 R".to_string()),
            "object mismatch"
        );
        // outlines = []
        assert_eq!(
            pairs[4].1,
            JsonValue::Array(vec![]),
            "outlines must be empty"
        );
        // pageposfrom1 = 1
        assert_eq!(pairs[5].1, JsonValue::Integer(1), "pageposfrom1 must be 1");
    }

    #[test]
    fn collect_image_refs_keeps_only_indirect_image_streams() {
        let mut pdf = load_one_page_pdf();
        let page_ref = ObjectRef::new(3, 0);
        let image_ref = ObjectRef::new(99, 0);

        let mut image_dict = Dictionary::new();
        image_dict.insert("Subtype", Object::Name(b"Image".to_vec()));
        pdf.set_object(
            image_ref,
            Object::Stream(Stream::new(image_dict, Vec::new())),
        );

        let mut xobjects = Dictionary::new();
        xobjects.insert(
            "Direct",
            Object::Stream(Stream::new(Dictionary::new(), Vec::new())),
        );
        xobjects.insert("Im", Object::Reference(image_ref));
        let mut resources = Dictionary::new();
        resources.insert("XObject", Object::Dictionary(xobjects));

        let Object::Dictionary(mut page) = pdf.resolve(page_ref).expect("resolve page") else {
            panic!("page must be a dictionary"); // cov:ignore: fixture-shape guard
        };
        page.insert("Resources", Object::Dictionary(resources));
        pdf.set_object(page_ref, Object::Dictionary(page));

        assert_eq!(
            collect_image_refs(&mut pdf, page_ref).expect("collect images"),
            vec!["99 0 R"]
        );
    }

    // ── 29. three-page.pdf: length and pageposfrom1 sequence ─────────────────

    #[test]
    fn build_pages_section_three_page_length_and_positions() {
        // Expected from: qpdf --json=2 --json-key=pages three-page.pdf
        // pages[0]: object="3 0 R", contents=["9 0 R"],  pageposfrom1=1
        // pages[1]: object="4 0 R", contents=["10 0 R"], pageposfrom1=2
        // pages[2]: object="5 0 R", contents=["11 0 R"], pageposfrom1=3
        let mut pdf = load_fixture_pdf("three-page.pdf");
        let pages = build_pages_section(&mut pdf).expect("build_pages_section failed");
        let JsonValue::Array(arr) = &pages else {
            panic!("expected Array");
        };
        assert_eq!(arr.len(), 3, "three-page.pdf must have 3 page entries");

        let expected = [
            ("3 0 R", "9 0 R", 1i64),
            ("4 0 R", "10 0 R", 2),
            ("5 0 R", "11 0 R", 3),
        ];
        for (i, (exp_obj, exp_contents, exp_pos)) in expected.iter().enumerate() {
            let pairs = get_page_entry(&pages, i);
            assert_eq!(
                pairs[0].1,
                JsonValue::Array(vec![JsonValue::String(exp_contents.to_string())]),
                "page {i} contents mismatch"
            );
            assert_eq!(
                pairs[3].1,
                JsonValue::String(exp_obj.to_string()),
                "page {i} object mismatch"
            );
            assert_eq!(
                pairs[5].1,
                JsonValue::Integer(*exp_pos),
                "page {i} pageposfrom1 mismatch"
            );
            // label and outlines are placeholders
            assert_eq!(pairs[2].1, JsonValue::Null, "page {i} label must be null");
            assert_eq!(
                pairs[4].1,
                JsonValue::Array(vec![]),
                "page {i} outlines must be empty"
            );
        }
    }

    // ── 30. attachment-two-page.pdf: length and object/contents refs ──────────

    #[test]
    fn build_pages_section_attachment_two_page_values() {
        // Expected from: qpdf --json=2 --json-key=pages attachment-two-page.pdf
        // pages[0]: object="6 0 R", contents=["9 0 R"],  pageposfrom1=1
        // pages[1]: object="7 0 R", contents=["11 0 R"], pageposfrom1=2
        let mut pdf = load_fixture_pdf("attachment-two-page.pdf");
        let pages = build_pages_section(&mut pdf).expect("build_pages_section failed");
        let JsonValue::Array(arr) = &pages else {
            panic!("expected Array");
        };
        assert_eq!(
            arr.len(),
            2,
            "attachment-two-page.pdf must have 2 page entries"
        );

        let expected = [("6 0 R", "9 0 R", 1i64), ("7 0 R", "11 0 R", 2)];
        for (i, (exp_obj, exp_contents, exp_pos)) in expected.iter().enumerate() {
            let pairs = get_page_entry(&pages, i);
            assert_eq!(
                pairs[0].1,
                JsonValue::Array(vec![JsonValue::String(exp_contents.to_string())]),
                "page {i} contents mismatch"
            );
            assert_eq!(
                pairs[3].1,
                JsonValue::String(exp_obj.to_string()),
                "page {i} object mismatch"
            );
            assert_eq!(
                pairs[5].1,
                JsonValue::Integer(*exp_pos),
                "page {i} pageposfrom1 mismatch"
            );
        }
    }

    // ── 31. collect_content_refs: single Reference to a Stream ────────────────

    #[test]
    fn collect_content_refs_single_ref_to_stream() {
        // one-page.pdf's /Contents is a single reference to a Stream
        // (object 7). The function must return that ref as-is.
        let mut pdf = load_one_page_pdf();
        let obj = Object::Reference(crate::ObjectRef::new(7, 0));
        let refs = collect_content_refs(&mut pdf, &obj).expect("collect_content_refs failed");
        assert_eq!(refs, vec!["7 0 R".to_string()]);
    }

    // ── 32. collect_content_refs: Array of References ─────────────────────────

    #[test]
    fn collect_content_refs_array_of_refs() {
        let mut pdf = load_one_page_pdf();
        let obj = Object::Array(vec![
            Object::Reference(crate::ObjectRef::new(4, 0)),
            Object::Reference(crate::ObjectRef::new(5, 0)),
        ]);
        let refs = collect_content_refs(&mut pdf, &obj).expect("collect_content_refs failed");
        assert_eq!(refs, vec!["4 0 R".to_string(), "5 0 R".to_string()]);
    }

    // ── 33. collect_content_refs: Null → empty ────────────────────────────────

    #[test]
    fn collect_content_refs_null_returns_empty() {
        let mut pdf = load_one_page_pdf();
        let obj = Object::Null;
        let refs = collect_content_refs(&mut pdf, &obj).expect("collect_content_refs failed");
        assert!(refs.is_empty());
    }

    // ── 34. collect_content_refs: Array with mixed types skips non-refs ───────

    #[test]
    fn collect_content_refs_array_skips_non_refs() {
        let mut pdf = load_one_page_pdf();
        let obj = Object::Array(vec![
            Object::Reference(crate::ObjectRef::new(3, 0)),
            Object::Integer(99), // not a ref — must be skipped
            Object::Reference(crate::ObjectRef::new(5, 0)),
        ]);
        let refs = collect_content_refs(&mut pdf, &obj).expect("collect_content_refs failed");
        assert_eq!(refs, vec!["3 0 R".to_string(), "5 0 R".to_string()]);
    }

    // ── 35. collect_content_refs: indirect Array unwraps to inner refs ────────
    //
    // `/Contents 2 0 R` where `2 0 obj [4 0 R 5 0 R] endobj` is legal in PDF.
    // qpdf-compatible output must flatten this to ["4 0 R", "5 0 R"], not
    // ["2 0 R"]. Regression test for CodeRabbit's finding.

    #[test]
    fn collect_content_refs_indirect_array_is_flattened() {
        let mut pdf = load_one_page_pdf();
        // Patch object 2 (currently the Font dict in one-page.pdf) into an
        // Array of References. /Contents -> 2 0 R must then unwrap to those.
        pdf.set_object(
            crate::ObjectRef::new(2, 0),
            Object::Array(vec![
                Object::Reference(crate::ObjectRef::new(4, 0)),
                Object::Reference(crate::ObjectRef::new(5, 0)),
            ]),
        );

        let obj = Object::Reference(crate::ObjectRef::new(2, 0));
        let refs = collect_content_refs(&mut pdf, &obj).expect("collect_content_refs failed");
        assert_eq!(
            refs,
            vec!["4 0 R".to_string(), "5 0 R".to_string()],
            "indirect Array of refs must be unwrapped, not emitted as the array's ref number"
        );
    }

    // ══════════════════════════════════════════════════════════════════════════
    // build_pagelabels_section tests (flpdf-9hc.11.5)
    // ══════════════════════════════════════════════════════════════════════════

    // Helper: build a synthetic catalog with a /PageLabels entry.
    fn patch_pagelabels(pdf: &mut crate::Pdf<std::io::Cursor<Vec<u8>>>, pagelabels: Object) {
        let catalog_ref = pdf.root_ref().expect("no /Root");
        let mut catalog = match pdf.resolve_borrowed(catalog_ref).expect("resolve catalog") {
            Object::Dictionary(d) => d.clone(),
            _ => panic!("catalog is not a Dictionary"),
        };
        catalog.insert("PageLabels", pagelabels);
        pdf.set_object(catalog_ref, Object::Dictionary(catalog));
    }

    // ── 36. No /PageLabels → empty array ─────────────────────────────────────

    #[test]
    fn pagelabels_missing_returns_empty_array() {
        // one-page.pdf has no /PageLabels — must return [].
        let mut pdf = load_one_page_pdf();
        let result = build_pagelabels_section(&mut pdf).expect("build_pagelabels_section failed");
        assert_eq!(
            result,
            JsonValue::Array(vec![]),
            "missing /PageLabels must yield empty array"
        );
    }

    // ── 37. Single range: /Nums [0 << /S /D /St 1 >>] ───────────────────────

    #[test]
    fn pagelabels_single_range_decimal() {
        let mut pdf = load_one_page_pdf();

        let mut label = Dictionary::new();
        label.insert("S", Object::Name(b"D".to_vec()));
        label.insert("St", Object::Integer(1));

        let pagelabels = Object::Dictionary({
            let mut d = Dictionary::new();
            d.insert(
                "Nums",
                Object::Array(vec![Object::Integer(0), Object::Dictionary(label)]),
            );
            d
        });
        patch_pagelabels(&mut pdf, pagelabels);

        let result = build_pagelabels_section(&mut pdf).expect("build_pagelabels_section failed");
        let JsonValue::Array(arr) = &result else {
            panic!("expected Array, got {result:?}");
        };
        assert_eq!(arr.len(), 1, "expected 1 entry");

        let JsonValue::Object(entry) = &arr[0] else {
            panic!("entry is not an Object");
        };
        // Key order: index, label
        assert_eq!(entry[0].0, "index");
        assert_eq!(entry[0].1, JsonValue::Integer(0));
        assert_eq!(entry[1].0, "label");

        let JsonValue::Object(label_pairs) = &entry[1].1 else {
            panic!("label is not an Object");
        };
        // Key order: first, prefix, style
        assert_eq!(label_pairs[0], ("first".to_string(), JsonValue::Integer(1)));
        assert_eq!(
            label_pairs[1],
            ("prefix".to_string(), JsonValue::String(String::new()))
        );
        assert_eq!(
            label_pairs[2],
            ("style".to_string(), JsonValue::String("D".to_string()))
        );
    }

    // ── 38. Multiple ranges ────────────────────────────────────────────────

    #[test]
    fn pagelabels_multiple_ranges() {
        // /Nums [0 << /S /D >> 5 << /S /R /P "Appx" /St 1 >> 10 << /S /a >>]
        let mut pdf = load_one_page_pdf();

        let mut label0 = Dictionary::new();
        label0.insert("S", Object::Name(b"D".to_vec()));

        let mut label5 = Dictionary::new();
        label5.insert("S", Object::Name(b"R".to_vec()));
        label5.insert("P", Object::String(b"Appx".to_vec()));
        label5.insert("St", Object::Integer(1));

        let mut label10 = Dictionary::new();
        label10.insert("S", Object::Name(b"a".to_vec()));

        let pagelabels = Object::Dictionary({
            let mut d = Dictionary::new();
            d.insert(
                "Nums",
                Object::Array(vec![
                    Object::Integer(0),
                    Object::Dictionary(label0),
                    Object::Integer(5),
                    Object::Dictionary(label5),
                    Object::Integer(10),
                    Object::Dictionary(label10),
                ]),
            );
            d
        });
        patch_pagelabels(&mut pdf, pagelabels);

        let result = build_pagelabels_section(&mut pdf).expect("build_pagelabels_section failed");
        let JsonValue::Array(arr) = &result else {
            panic!("expected Array"); // cov:ignore: test-shape guard
        };
        assert_eq!(arr.len(), 3, "expected 3 entries");

        // Check indices
        let get_index = |i: usize| {
            let JsonValue::Object(e) = &arr[i] else {
                panic!()
            };
            match &e[0].1 {
                JsonValue::Integer(n) => *n,
                _ => panic!(),
            }
        };
        assert_eq!(get_index(0), 0);
        assert_eq!(get_index(1), 5);
        assert_eq!(get_index(2), 10);

        // Check styles
        let get_style = |i: usize| {
            let JsonValue::Object(e) = &arr[i] else {
                panic!()
            };
            let JsonValue::Object(lp) = &e[1].1 else {
                panic!()
            };
            lp[2].1.clone()
        };
        assert_eq!(get_style(0), JsonValue::String("D".to_string()));
        assert_eq!(get_style(1), JsonValue::String("R".to_string()));
        assert_eq!(get_style(2), JsonValue::String("a".to_string()));

        // Check prefix on entry 1
        let JsonValue::Object(e1) = &arr[1] else {
            panic!()
        };
        let JsonValue::Object(lp1) = &e1[1].1 else {
            panic!()
        };
        assert_eq!(lp1[1].1, JsonValue::String("Appx".to_string()));
    }

    // ── 38b. Indirect label value is resolved ────────────────────────────────

    #[test]
    fn pagelabels_indirect_label_value_resolved() {
        // A /Nums value that is an indirect reference to a label dict must be
        // resolved (covers the `Object::Reference` arm of the decode hook).
        let mut pdf = load_one_page_pdf();
        let label_ref = crate::ObjectRef::new(900, 0);
        let mut label = Dictionary::new();
        label.insert("S", Object::Name("D".into()));
        pdf.set_object(label_ref, Object::Dictionary(label));

        let pagelabels = Object::Dictionary({
            let mut d = Dictionary::new();
            d.insert(
                "Nums",
                Object::Array(vec![Object::Integer(0), Object::Reference(label_ref)]),
            );
            d
        });
        patch_pagelabels(&mut pdf, pagelabels);

        let result = build_pagelabels_section(&mut pdf).expect("build_pagelabels_section failed");
        assert!(
            matches!(&result, JsonValue::Array(arr) if arr.len() == 1),
            "indirect label value must resolve to one entry, got {result:?}"
        );
    }

    // ── 38c. Non-dict label value is skipped ──────────────────────────────────

    #[test]
    fn pagelabels_non_dict_value_skipped() {
        // A /Nums value that is neither a dict nor a reference is skipped
        // (covers the `_ => Ok(None)` arm of the decode hook).
        let mut pdf = load_one_page_pdf();
        let pagelabels = Object::Dictionary({
            let mut d = Dictionary::new();
            d.insert(
                "Nums",
                Object::Array(vec![Object::Integer(0), Object::Integer(42)]),
            );
            d
        });
        patch_pagelabels(&mut pdf, pagelabels);

        let result = build_pagelabels_section(&mut pdf).expect("build_pagelabels_section failed");
        assert_eq!(
            result,
            JsonValue::Array(vec![]),
            "non-dict label value yields no entries"
        );
    }

    // ── 39. /S absent → style: null ──────────────────────────────────────────

    #[test]
    fn pagelabels_no_style_gives_null() {
        // Label dict with only /P — no /S → style must be null
        let mut pdf = load_one_page_pdf();

        let mut label = Dictionary::new();
        label.insert("P", Object::String(b"App".to_vec()));

        let pagelabels = Object::Dictionary({
            let mut d = Dictionary::new();
            d.insert(
                "Nums",
                Object::Array(vec![Object::Integer(0), Object::Dictionary(label)]),
            );
            d
        });
        patch_pagelabels(&mut pdf, pagelabels);

        let result = build_pagelabels_section(&mut pdf).expect("build_pagelabels_section failed");
        let JsonValue::Array(arr) = &result else {
            panic!("expected Array"); // cov:ignore: test-shape guard
        };
        assert_eq!(arr.len(), 1);
        let JsonValue::Object(entry) = &arr[0] else {
            panic!() // cov:ignore: test-shape guard
        };
        let JsonValue::Object(label_pairs) = &entry[1].1 else {
            panic!() // cov:ignore: test-shape guard
        };
        assert_eq!(label_pairs[2].0, "style");
        assert_eq!(
            label_pairs[2].1,
            JsonValue::Null,
            "style must be null when /S is absent"
        );
    }

    // ── 40. /Kids subtree walk ────────────────────────────────────────────────

    #[test]
    fn pagelabels_kids_subtree_walk() {
        // /PageLabels << /Kids [99 0 R] >>  where 99 0 obj << /Nums [0 << /S /r >>] >>
        let mut pdf = load_one_page_pdf();

        let mut label = Dictionary::new();
        label.insert("S", Object::Name(b"r".to_vec()));

        let mut subtree = Dictionary::new();
        subtree.insert(
            "Nums",
            Object::Array(vec![Object::Integer(0), Object::Dictionary(label)]),
        );

        let subtree_ref = crate::ObjectRef::new(99, 0);
        pdf.set_object(subtree_ref, Object::Dictionary(subtree));

        let pagelabels = Object::Dictionary({
            let mut d = Dictionary::new();
            d.insert("Kids", Object::Array(vec![Object::Reference(subtree_ref)]));
            d
        });
        patch_pagelabels(&mut pdf, pagelabels);

        let result = build_pagelabels_section(&mut pdf).expect("build_pagelabels_section failed");
        let JsonValue::Array(arr) = &result else {
            panic!("expected Array"); // cov:ignore: test-shape guard
        };
        assert_eq!(arr.len(), 1, "expected 1 entry from /Kids walk");
        let JsonValue::Object(entry) = &arr[0] else {
            panic!() // cov:ignore: test-shape guard
        };
        assert_eq!(entry[0].1, JsonValue::Integer(0));
        let JsonValue::Object(lp) = &entry[1].1 else {
            panic!() // cov:ignore: test-shape guard
        };
        assert_eq!(lp[2].1, JsonValue::String("r".to_string()));
    }

    // ── 41. All compat fixtures without /PageLabels yield empty array ─────────

    #[test]
    fn pagelabels_compat_fixtures_all_empty() {
        let fixtures = ["one-page.pdf", "three-page.pdf", "attachment-two-page.pdf"];
        for name in fixtures {
            let mut pdf = load_fixture_pdf(name);
            let result = build_pagelabels_section(&mut pdf)
                .unwrap_or_else(|e| panic!("{name}: build_pagelabels_section failed: {e:?}"));
            assert_eq!(
                result,
                JsonValue::Array(vec![]),
                "{name}: expected empty pagelabels array"
            );
        }
    }

    // ── build_qpdf_json_v2 (top-level composite output) ───────────────────────

    #[test]
    fn build_qpdf_json_v2_has_expected_top_level_keys_in_order() {
        let mut pdf = load_one_page_pdf();
        let v2 = build_qpdf_json_v2(&mut pdf, DecodeLevel::Generalized)
            .expect("build_qpdf_json_v2 failed");
        let JsonValue::Object(pairs) = v2 else {
            panic!("expected Object at top level"); // cov:ignore: test-shape guard
        };
        // qpdf-style fixed order: version, parameters, pages, pagelabels, acroform, attachments, encrypt, outlines, qpdf
        let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "version",
                "parameters",
                "pages",
                "pagelabels",
                "acroform",
                "attachments",
                "encrypt",
                "outlines",
                "qpdf"
            ]
        );
    }

    fn load_repairable_outline_pdf() -> Pdf<std::io::Cursor<Vec<u8>>> {
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R /Names << /Dests << /Kids [8 0 R] >> >> >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
            "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>",
            "<< /Title (One) /Parent 4 0 R /Dest (shape) >>",
            "null",
            "null",
            "<< /Names [(shape) [3 0 R /Fit]] >>",
        ];
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::with_capacity(objects.len());
        for (index, object) in objects.iter().enumerate() {
            offsets.push(bytes.len());
            bytes
                .extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", index + 1, object).as_bytes());
        }
        let start_xref = bytes.len();
        bytes.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        bytes.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{start_xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        Pdf::open(std::io::Cursor::new(bytes)).unwrap()
    }

    fn top_level_key_names(value: &JsonValue) -> Vec<&str> {
        let JsonValue::Object(pairs) = value else {
            panic!("expected top-level JSON object"); // cov:ignore: test-shape guard
        };
        pairs.iter().map(|(key, _)| key.as_str()).collect()
    }

    fn qpdf_object_value<'a>(value: &'a JsonValue, object: &str) -> &'a JsonValue {
        let JsonValue::Object(top) = value else {
            panic!("expected top-level JSON object"); // cov:ignore: test-shape guard
        };
        let JsonValue::Array(qpdf) = value_for_key(top, "qpdf") else {
            panic!("expected qpdf array"); // cov:ignore: test-shape guard
        };
        let JsonValue::Object(objects) = &qpdf[1] else {
            panic!("expected qpdf object map"); // cov:ignore: test-shape guard
        };
        let JsonValue::Object(entry) = value_for_key(objects, object) else {
            panic!("expected qpdf object entry"); // cov:ignore: test-shape guard
        };
        value_for_key(entry, "value")
    }

    fn direct_dests_root(pdf: &mut Pdf<std::io::Cursor<Vec<u8>>>) -> Dictionary {
        let Object::Dictionary(catalog) = pdf.resolve(crate::ObjectRef::new(1, 0)).unwrap() else {
            panic!("catalog must be a dictionary"); // cov:ignore: test-fixture shape guard
        };
        let Some(Object::Dictionary(names)) = catalog.get("Names") else {
            panic!("catalog /Names must be a direct dictionary"); // cov:ignore: test-fixture shape guard
        };
        let Some(Object::Dictionary(dests)) = names.get("Dests") else {
            panic!("/Names /Dests must be a direct dictionary"); // cov:ignore: test-fixture shape guard
        };
        dests.clone()
    }

    #[test]
    fn selected_qpdf_skips_outline_repair_and_preserves_raw_objects() {
        let mut pdf = load_repairable_outline_pdf();
        let before = pdf.resolve(crate::ObjectRef::new(1, 0)).unwrap().clone();

        let json = build_qpdf_json_v2_selected_with_options(
            &mut pdf,
            DecodeLevel::Generalized,
            &StreamDataMode::None,
            &[JsonKey::Qpdf],
        )
        .unwrap();

        assert_eq!(
            top_level_key_names(&json),
            ["version", "parameters", "qpdf"]
        );
        assert!(pdf.repair_diagnostics().entries().is_empty());
        assert_eq!(pdf.resolve(crate::ObjectRef::new(1, 0)).unwrap(), before);
        assert_eq!(
            qpdf_object_value(&json, "obj:1 0 R"),
            &pdf_object_to_json(&before).unwrap()
        );
    }

    #[test]
    fn selected_outlines_repairs_only_the_requested_section() {
        let mut pdf = load_repairable_outline_pdf();

        let json = build_qpdf_json_v2_selected_with_options(
            &mut pdf,
            DecodeLevel::Generalized,
            &StreamDataMode::None,
            &[JsonKey::Outlines],
        )
        .unwrap();

        assert_eq!(
            top_level_key_names(&json),
            ["version", "parameters", "outlines"]
        );
        assert_eq!(pdf.repair_diagnostics().entries().len(), 1);
        let dests = direct_dests_root(&mut pdf);
        assert!(dests.get("Kids").is_none());
        assert!(matches!(dests.get("Names"), Some(Object::Array(_))));
    }

    #[test]
    fn selected_outlines_precede_qpdf_and_raw_objects_reflect_repair() {
        let mut pdf = load_repairable_outline_pdf();

        let json = build_qpdf_json_v2_selected_with_options(
            &mut pdf,
            DecodeLevel::Generalized,
            &StreamDataMode::None,
            &[JsonKey::Qpdf, JsonKey::Outlines],
        )
        .unwrap();

        assert_eq!(
            top_level_key_names(&json),
            ["version", "parameters", "outlines", "qpdf"]
        );
        assert_eq!(pdf.repair_diagnostics().entries().len(), 1);
        let repaired_catalog = pdf.resolve(crate::ObjectRef::new(1, 0)).unwrap().clone();
        assert_eq!(
            qpdf_object_value(&json, "obj:1 0 R"),
            &pdf_object_to_json(&repaired_catalog).unwrap()
        );
    }

    #[test]
    fn selected_json_section_matrix_preserves_v2_order() {
        let cases = vec![
            (
                vec![],
                vec![
                    "version",
                    "parameters",
                    "pages",
                    "pagelabels",
                    "acroform",
                    "attachments",
                    "encrypt",
                    "outlines",
                    "qpdf",
                ],
            ),
            (vec![JsonKey::Pages], vec!["version", "parameters", "pages"]),
            (
                vec![JsonKey::Pagelabels],
                vec!["version", "parameters", "pagelabels"],
            ),
            (
                vec![JsonKey::Acroform],
                vec!["version", "parameters", "acroform"],
            ),
            (
                vec![JsonKey::Attachments],
                vec!["version", "parameters", "attachments"],
            ),
            (
                vec![JsonKey::Encrypt],
                vec!["version", "parameters", "encrypt"],
            ),
            (
                vec![JsonKey::Outlines],
                vec!["version", "parameters", "outlines"],
            ),
            (vec![JsonKey::Qpdf], vec!["version", "parameters", "qpdf"]),
            (
                vec![JsonKey::Qpdf, JsonKey::Outlines, JsonKey::Qpdf],
                vec!["version", "parameters", "outlines", "qpdf"],
            ),
        ];

        for (keys, expected) in cases {
            let mut pdf = load_one_page_pdf();
            let json = build_qpdf_json_v2_selected_with_options(
                &mut pdf,
                DecodeLevel::Generalized,
                &StreamDataMode::None,
                &keys,
            )
            .unwrap();
            assert_eq!(top_level_key_names(&json), expected, "keys={keys:?}");
        }
    }

    fn selected_qpdf_metadata(json: &JsonValue) -> &JsonValue {
        let JsonValue::Object(top) = json else {
            panic!("top-level JSON must be an object"); // cov:ignore: test-shape guard
        };
        let qpdf = top
            .iter()
            .find(|(key, _)| key == "qpdf")
            .map(|(_, value)| value)
            .expect("selected document must contain qpdf");
        let JsonValue::Array(qpdf) = qpdf else {
            panic!("qpdf must be an array"); // cov:ignore: test-shape guard
        };
        qpdf.first().expect("qpdf metadata element")
    }

    #[test]
    fn selected_json_metadata_reflects_actual_page_enumeration() {
        let cases = [
            ("qpdf only", vec![JsonKey::Qpdf], false),
            (
                "attachments then qpdf",
                vec![JsonKey::Attachments, JsonKey::Qpdf],
                false,
            ),
            (
                "encrypt then qpdf",
                vec![JsonKey::Encrypt, JsonKey::Qpdf],
                false,
            ),
            ("pages then qpdf", vec![JsonKey::Pages, JsonKey::Qpdf], true),
            (
                "pagelabels then qpdf",
                vec![JsonKey::Pagelabels, JsonKey::Qpdf],
                true,
            ),
            (
                "acroform then qpdf",
                vec![JsonKey::Acroform, JsonKey::Qpdf],
                true,
            ),
            (
                "outlines then qpdf",
                vec![JsonKey::Outlines, JsonKey::Qpdf],
                true,
            ),
            (
                "request order does not change construction order",
                vec![JsonKey::Qpdf, JsonKey::Attachments, JsonKey::Pages],
                true,
            ),
            ("full document", vec![], true),
        ];

        for (label, keys, called_get_all_pages) in cases {
            let mut pdf = load_one_page_pdf();
            let pdf_version = pdf.version().to_string();
            let max_object_id = pdf
                .object_refs()
                .iter()
                .map(|reference| reference.number)
                .max()
                .unwrap_or(0);
            let json = build_qpdf_json_v2_selected_with_options(
                &mut pdf,
                DecodeLevel::Generalized,
                &StreamDataMode::None,
                &keys,
            )
            .unwrap();

            assert_eq!(
                selected_qpdf_metadata(&json),
                &JsonValue::Object(vec![
                    ("jsonversion".to_string(), JsonValue::Integer(2)),
                    ("pdfversion".to_string(), JsonValue::String(pdf_version)),
                    (
                        "pushedinheritedpageresources".to_string(),
                        JsonValue::Bool(false),
                    ),
                    (
                        "calledgetallpages".to_string(),
                        JsonValue::Bool(called_get_all_pages),
                    ),
                    (
                        "maxobjectid".to_string(),
                        JsonValue::Integer(i64::from(max_object_id)),
                    ),
                ]),
                "{label}: keys={keys:?}"
            );
        }
    }

    #[test]
    fn qpdf_dangling_body_reference_participates_in_maxobjectid_for_trailer_selection() {
        let mut pdf = load_fixture_pdf("dangling-body-one-page.pdf");
        let json = build_qpdf_json_v2_selected_with_options(
            &mut pdf,
            DecodeLevel::Generalized,
            &StreamDataMode::None,
            &[JsonKey::Qpdf],
        )
        .expect("build selected qpdf JSON");

        assert_eq!(
            selected_qpdf_metadata(&json),
            &JsonValue::Object(vec![
                ("jsonversion".to_string(), JsonValue::Integer(2)),
                (
                    "pdfversion".to_string(),
                    JsonValue::String("1.3".to_string()),
                ),
                (
                    "pushedinheritedpageresources".to_string(),
                    JsonValue::Bool(false),
                ),
                ("calledgetallpages".to_string(), JsonValue::Bool(false),),
                ("maxobjectid".to_string(), JsonValue::Integer(99)),
            ])
        );
    }

    fn build_qpdf_dangling_xref_pdf(
        catalog_extra: &str,
        extra_objects: &[(u32, u16, &str)],
        free_entries: &[(u32, u16)],
        size: u32,
    ) -> Vec<u8> {
        build_qpdf_dangling_xref_pdf_with_trailer(
            catalog_extra,
            "",
            extra_objects,
            free_entries,
            size,
        )
    }

    fn build_qpdf_dangling_xref_pdf_with_trailer(
        catalog_extra: &str,
        trailer_extra: &str,
        extra_objects: &[(u32, u16, &str)],
        free_entries: &[(u32, u16)],
        size: u32,
    ) -> Vec<u8> {
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut entries = vec![
            (
                1u32,
                0u16,
                format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>"),
            ),
            (
                2,
                0,
                "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            ),
            (
                3,
                0,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>".to_string(),
            ),
        ];
        entries.extend(
            extra_objects
                .iter()
                .map(|(number, generation, body)| (*number, *generation, (*body).to_string())),
        );
        entries.sort_by_key(|(number, generation, _)| (*number, *generation));

        let mut offsets = Vec::new();
        for (number, generation, body) in &entries {
            offsets.push((*number, *generation, bytes.len()));
            bytes.extend_from_slice(
                format!("{number} {generation} obj\n{body}\nendobj\n").as_bytes(),
            );
        }

        let xref = bytes.len();
        bytes.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
        for (number, generation, offset) in offsets {
            bytes.extend_from_slice(
                format!("{number} 1\n{offset:010} {generation:05} n \n").as_bytes(),
            );
        }
        for (number, generation) in free_entries {
            bytes.extend_from_slice(
                format!("{number} 1\n0000000000 {generation:05} f \n").as_bytes(),
            );
        }
        bytes.extend_from_slice(
            format!(
                "trailer\n<< /Size {size} /Root 1 0 R {trailer_extra} >>\nstartxref\n{xref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        bytes
    }

    #[test]
    fn qpdf_dangling_preparation_does_not_leak_missing_refs_to_public_enumeration() {
        let mut pdf = load_fixture_pdf("dangling-body-one-page.pdf");
        let dangling = crate::ObjectRef::new(99, 0);

        let prepared = pdf
            .prepare_qpdf_json_objects()
            .expect("prepare qpdf objects");

        assert!(prepared.refs.contains(&dangling));
        assert_eq!(prepared.max_object_id, 99);
        assert!(!pdf.object_refs().contains(&dangling));
        assert!(!pdf.live_object_refs().contains(&dangling));
    }

    #[test]
    fn qpdf_dangling_preparation_excludes_unreferenced_high_free_xref_entry() {
        let bytes = build_qpdf_dangling_xref_pdf("", &[], &[(200, 7)], 201);
        let mut pdf = crate::Pdf::open_mem_owned(bytes).expect("open high-free fixture");

        let prepared = pdf
            .prepare_qpdf_json_objects()
            .expect("prepare qpdf objects");

        assert_eq!(prepared.max_object_id, 3);
        assert!(!prepared
            .refs
            .iter()
            .any(|reference| reference.number == 200));
        assert!(pdf.object_refs().contains(&crate::ObjectRef::new(200, 7)));
    }

    #[test]
    fn qpdf_dangling_preparation_includes_referenced_free_generation() {
        let bytes = build_qpdf_dangling_xref_pdf("/Probe 200 7 R", &[], &[(200, 7)], 201);
        let mut pdf = crate::Pdf::open_mem_owned(bytes).expect("open referenced-free fixture");

        let prepared = pdf
            .prepare_qpdf_json_objects()
            .expect("prepare qpdf objects");

        assert_eq!(prepared.max_object_id, 200);
        assert!(prepared.refs.contains(&crate::ObjectRef::new(200, 7)));
        assert!(!pdf
            .live_object_refs()
            .contains(&crate::ObjectRef::new(200, 7)));
    }

    #[test]
    fn qpdf_dangling_preparation_keeps_live_and_dangling_generations_distinct() {
        let bytes = build_qpdf_dangling_xref_pdf(
            "/Live 8 1 R /Stale 8 0 R",
            &[(8, 1, "<< /Value 1 >>")],
            &[],
            9,
        );
        let mut pdf = crate::Pdf::open_mem_owned(bytes).expect("open generation fixture");

        let prepared = pdf
            .prepare_qpdf_json_objects()
            .expect("prepare qpdf objects");

        assert!(prepared.refs.contains(&crate::ObjectRef::new(8, 0)));
        assert!(prepared.refs.contains(&crate::ObjectRef::new(8, 1)));
        assert!(!pdf
            .live_object_refs()
            .contains(&crate::ObjectRef::new(8, 0)));
        assert!(pdf
            .live_object_refs()
            .contains(&crate::ObjectRef::new(8, 1)));
    }

    #[test]
    fn qpdf_dangling_preparation_includes_valid_trailer_only_generations() {
        let bytes = build_qpdf_dangling_xref_pdf_with_trailer(
            "",
            "/Info 99 0 R /Gen 88 4 R /Zero 0 0 R /BadGen 77 65535 R",
            &[],
            &[(200, 7)],
            201,
        );
        let mut pdf = crate::Pdf::open_mem_owned(bytes).expect("open trailer-only fixture");

        let prepared = pdf
            .prepare_qpdf_json_objects()
            .expect("prepare qpdf objects");

        for dangling in [crate::ObjectRef::new(99, 0), crate::ObjectRef::new(88, 4)] {
            assert!(prepared.refs.contains(&dangling), "{dangling:?}");
            assert!(!pdf.object_refs().contains(&dangling), "{dangling:?}");
            assert!(!pdf.live_object_refs().contains(&dangling), "{dangling:?}");
        }
        assert_eq!(prepared.max_object_id, 99);
        assert!(!prepared.refs.contains(&crate::ObjectRef::new(0, 0)));
        assert!(!prepared.refs.contains(&crate::ObjectRef::new(77, u16::MAX)));
        assert!(!prepared
            .refs
            .iter()
            .any(|reference| reference.number == 200));
        assert!(pdf.object_refs().contains(&crate::ObjectRef::new(200, 7)));
    }

    fn selected_qpdf_object_map(json: &JsonValue) -> &[(String, JsonValue)] {
        let JsonValue::Object(top) = json else {
            panic!("top-level JSON must be an object"); // cov:ignore: test-shape guard
        };
        let JsonValue::Array(qpdf) = top
            .iter()
            .find(|(key, _)| key == "qpdf")
            .map(|(_, value)| value)
            .expect("qpdf section")
        else {
            panic!("qpdf section must be an array"); // cov:ignore: test-shape guard
        };
        let JsonValue::Object(map) = &qpdf[1] else {
            panic!("qpdf object map"); // cov:ignore: test-shape guard
        };
        map
    }

    #[test]
    fn qpdf_dangling_raw_projection_matches_qpdf_container_null_rules() {
        let mut pdf = load_fixture_pdf("dangling-body-one-page.pdf");
        let json = build_qpdf_json_v2_selected_with_options(
            &mut pdf,
            DecodeLevel::Generalized,
            &StreamDataMode::None,
            &[JsonKey::Qpdf],
        )
        .expect("build qpdf JSON");
        let map = selected_qpdf_object_map(&json);

        let obj4 = map
            .iter()
            .find(|(key, _)| key == "obj:4 0 R")
            .map(|(_, value)| value)
            .expect("catalog object");
        assert_eq!(
            obj4,
            &JsonValue::Object(vec![(
                "value".to_string(),
                JsonValue::Object(vec![
                    (
                        "/ArrZero".to_string(),
                        JsonValue::Array(vec![JsonValue::Null]),
                    ),
                    ("/Nested".to_string(), JsonValue::Object(Vec::new()),),
                    (
                        "/PageMode".to_string(),
                        JsonValue::String("/UseNone".to_string()),
                    ),
                    ("/Pages".to_string(), JsonValue::String("6 0 R".to_string()),),
                    (
                        "/Type".to_string(),
                        JsonValue::String("/Catalog".to_string()),
                    ),
                ]),
            )])
        );
        assert_eq!(
            map.iter()
                .find(|(key, _)| key == "obj:99 0 R")
                .map(|(_, value)| value),
            Some(&JsonValue::Object(vec![(
                "value".to_string(),
                JsonValue::Null,
            )]))
        );
    }

    #[test]
    fn qpdf_dangling_raw_selectors_filter_serialization_after_full_preparation() {
        let cases = [
            (
                "trailer",
                vec![JsonObjectSelector::Trailer],
                vec!["trailer"],
            ),
            (
                "dangling generation",
                vec![JsonObjectSelector::Object {
                    number: 99,
                    generation: 0,
                }],
                vec!["obj:99 0 R"],
            ),
        ];

        for (label, selectors, expected_keys) in cases {
            let mut pdf = load_fixture_pdf("dangling-body-one-page.pdf");
            let json = build_qpdf_json_v2_selected_objects_with_options(
                &mut pdf,
                DecodeLevel::Generalized,
                &StreamDataMode::None,
                &[JsonKey::Qpdf],
                &selectors,
            )
            .expect("build selected objects");

            let JsonValue::Object(metadata) = selected_qpdf_metadata(&json) else {
                panic!("qpdf metadata must be an object"); // cov:ignore: test-shape guard
            };
            assert_eq!(
                metadata
                    .iter()
                    .find(|(key, _)| key == "maxobjectid")
                    .map(|(_, value)| value),
                Some(&JsonValue::Integer(99)),
                "{label}"
            );
            assert_eq!(
                selected_qpdf_object_map(&json)
                    .iter()
                    .map(|(key, _)| key.as_str())
                    .collect::<Vec<_>>(),
                expected_keys,
                "{label}"
            );
        }
    }

    #[test]
    fn qpdf_json_helpers_cover_reference_cycles_nulls_and_nested_streams() {
        let mut pdf = load_one_page_pdf();
        let first = ObjectRef::new(80, 0);
        let second = ObjectRef::new(81, 0);
        pdf.set_object(first, Object::Reference(second));
        pdf.set_object(second, Object::Reference(first));

        assert!(qpdf_reference_resolves_to_null(&mut pdf, first).unwrap());
        assert_eq!(
            qpdf_resolve_top_level_object(&mut pdf, first).unwrap(),
            Object::Null
        );

        let mut nested_dict = Dictionary::new();
        nested_dict.insert("Drop", Object::Null);
        let nested_stream = Object::Stream(Stream::new(nested_dict, Vec::new()));
        assert_eq!(
            qpdf_pdf_object_to_json(&mut pdf, &nested_stream).unwrap(),
            JsonValue::Object(vec![(
                "stream".to_string(),
                JsonValue::Object(vec![("dict".to_string(), JsonValue::Object(Vec::new()),)]),
            )])
        );
    }

    #[test]
    fn build_qpdf_json_v2_includes_pagelabels_section() {
        // Regression for CodeRabbit's flpdf-9hc.11.5 finding: the
        // pagelabels builder was added but never wired into the top-level
        // JSON, so users would never see the section. This test fails if
        // the wiring is dropped again.
        let mut pdf = load_one_page_pdf();
        let v2 = build_qpdf_json_v2(&mut pdf, DecodeLevel::Generalized)
            .expect("build_qpdf_json_v2 failed");
        let JsonValue::Object(pairs) = v2 else {
            panic!("expected Object at top level"); // cov:ignore: test-shape guard
        };
        let pagelabels = pairs
            .iter()
            .find(|(k, _)| k == "pagelabels")
            .map(|(_, v)| v)
            .expect("pagelabels key must be present in the composite output");
        assert!(
            matches!(pagelabels, JsonValue::Array(_)),
            "pagelabels must be an Array"
        );
    }

    #[test]
    fn build_qpdf_json_v2_pages_count_matches_fixture() {
        let mut pdf = load_three_page_pdf();
        let v2 = build_qpdf_json_v2(&mut pdf, DecodeLevel::Generalized)
            .expect("build_qpdf_json_v2 failed");
        let JsonValue::Object(pairs) = v2 else {
            panic!("expected Object at top level"); // cov:ignore: test-shape guard
        };
        let pages = pairs
            .iter()
            .find(|(k, _)| k == "pages")
            .map(|(_, v)| v)
            .expect("pages key missing");
        let JsonValue::Array(page_entries) = pages else {
            panic!("pages must be Array"); // cov:ignore: test-shape guard
        };
        assert_eq!(
            page_entries.len(),
            3,
            "three-page.pdf must produce 3 page entries"
        );
    }

    #[test]
    fn build_qpdf_json_v2_qpdf_metadata_uses_actual_pdf_version() {
        let mut pdf = load_one_page_pdf();
        let v2 = build_qpdf_json_v2(&mut pdf, DecodeLevel::Generalized)
            .expect("build_qpdf_json_v2 failed");
        let JsonValue::Object(pairs) = v2 else {
            panic!("expected Object"); // cov:ignore: test-shape guard
        };
        let qpdf = pairs.iter().find(|(k, _)| k == "qpdf").unwrap().1.clone();
        let JsonValue::Array(qpdf_arr) = qpdf else {
            panic!("qpdf must be Array");
        };
        let JsonValue::Object(meta_pairs) = &qpdf_arr[0] else {
            panic!("qpdf[0] must be Object");
        };
        let pdf_version = meta_pairs
            .iter()
            .find(|(k, _)| k == "pdfversion")
            .map(|(_, v)| v)
            .expect("pdfversion missing");
        // one-page.pdf header is "%PDF-1.3".
        assert_eq!(*pdf_version, JsonValue::String("1.3".to_string()));
    }

    // ══════════════════════════════════════════════════════════════════════════
    // build_outlines_section tests (flpdf-9hc.11.6)
    // ══════════════════════════════════════════════════════════════════════════

    fn load_direct_outline_fixture() -> Pdf<std::io::Cursor<Vec<u8>>> {
        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/json-diff/direct-outlines.pdf"
        ));
        Pdf::open(std::io::Cursor::new(bytes.to_vec())).unwrap()
    }

    fn value_for_key<'a>(pairs: &'a [(String, JsonValue)], key: &str) -> &'a JsonValue {
        &pairs.iter().find(|(name, _)| name == key).unwrap().1
    }

    fn json_array(value: &JsonValue) -> &[JsonValue] {
        match value {
            JsonValue::Array(items) => items,
            _ => panic!("expected JSON array"), // cov:ignore: test-shape guard
        }
    }

    fn json_object(value: &JsonValue) -> &[(String, JsonValue)] {
        match value {
            JsonValue::Object(pairs) => pairs,
            _ => panic!("expected JSON object"), // cov:ignore: test-shape guard
        }
    }

    #[test]
    fn outline_json_v2_has_exact_qpdf_keys_and_values() {
        let mut pdf = load_direct_outline_fixture();
        let result = build_outlines_section(&mut pdf).unwrap();
        let entries = json_array(&result);
        let first = json_object(&entries[0]);

        let keys: Vec<_> = first.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "dest",
                "destpageposfrom1",
                "kids",
                "object",
                "open",
                "title"
            ]
        );
        assert_eq!(
            value_for_key(first, "dest"),
            &JsonValue::Array(vec![
                JsonValue::String("8 0 R".into()),
                JsonValue::String("/XYZ".into()),
                JsonValue::Null,
                JsonValue::Null,
                JsonValue::Null,
            ])
        );
        assert_eq!(
            value_for_key(first, "destpageposfrom1"),
            &JsonValue::Integer(6)
        );
        assert_eq!(
            value_for_key(first, "object"),
            &JsonValue::String("96 0 R".into())
        );
        assert_eq!(value_for_key(first, "open"), &JsonValue::Bool(true));
        assert_eq!(
            value_for_key(first, "title"),
            &JsonValue::String("Isís 1 -> 5: /XYZ null null null".into())
        );

        let kids = json_array(value_for_key(first, "kids"));
        assert_eq!(kids.len(), 2);
        let first_kid = json_object(&kids[0]);
        let second_kid = json_object(&kids[1]);
        assert_eq!(
            value_for_key(first_kid, "title"),
            &JsonValue::String("Amanda 1.1 -> 11: /Fit".into())
        );
        assert_eq!(value_for_key(first_kid, "open"), &JsonValue::Bool(false));
        assert_eq!(
            value_for_key(second_kid, "title"),
            &JsonValue::String("Sandy ÷Σανδι÷ 1.2 -> 13: /FitH 792".into())
        );
    }

    #[test]
    fn outline_json_v2_projects_direct_items_exactly() {
        let mut pdf = load_one_page_pdf();
        let page_ref = crate::pages::page_refs(&mut pdf).unwrap()[0];

        let mut next = Dictionary::new();
        next.insert("Title", Object::String(b"Direct B".to_vec()));

        let mut first = Dictionary::new();
        first.insert("Count", Object::Integer(-1));
        first.insert(
            "Dest",
            Object::Array(vec![
                Object::Reference(page_ref),
                Object::Name(b"Fit".to_vec()),
            ]),
        );
        first.insert("Next", Object::Dictionary(next));
        first.insert("Title", Object::String(b"Direct A".to_vec()));

        let mut outlines = Dictionary::new();
        outlines.insert("First", Object::Dictionary(first));
        let catalog_ref = pdf.root_ref().unwrap();
        let mut catalog = pdf.resolve(catalog_ref).unwrap().as_dict().unwrap().clone();
        catalog.insert("Outlines", Object::Dictionary(outlines));
        pdf.set_object(catalog_ref, Object::Dictionary(catalog));

        let result = build_outlines_section(&mut pdf).unwrap();
        let entries = json_array(&result);
        assert_eq!(entries.len(), 2);
        let first = json_object(&entries[0]);
        let second = json_object(&entries[1]);

        assert_eq!(
            value_for_key(first, "object"),
            &JsonValue::Object(vec![
                ("/Count".into(), JsonValue::Integer(-1)),
                (
                    "/Dest".into(),
                    JsonValue::Array(vec![
                        JsonValue::String(page_ref.to_string()),
                        JsonValue::String("/Fit".into()),
                    ]),
                ),
                (
                    "/Next".into(),
                    JsonValue::Object(vec![(
                        "/Title".into(),
                        JsonValue::String("u:Direct B".into()),
                    )]),
                ),
                ("/Title".into(), JsonValue::String("u:Direct A".into())),
            ])
        );
        assert_eq!(
            value_for_key(first, "destpageposfrom1"),
            &JsonValue::Integer(1)
        );
        assert_eq!(value_for_key(first, "open"), &JsonValue::Bool(false));
        assert_eq!(
            value_for_key(first, "title"),
            &JsonValue::String("Direct A".into())
        );

        assert_eq!(value_for_key(second, "dest"), &JsonValue::Null);
        assert_eq!(value_for_key(second, "destpageposfrom1"), &JsonValue::Null);
        assert_eq!(
            value_for_key(second, "object"),
            &JsonValue::Object(vec![(
                "/Title".into(),
                JsonValue::String("u:Direct B".into()),
            )])
        );
        assert_eq!(value_for_key(second, "open"), &JsonValue::Bool(true));
        assert_eq!(
            value_for_key(second, "title"),
            &JsonValue::String("Direct B".into())
        );
    }

    #[test]
    fn outline_json_v2_stops_at_an_indirect_null_child() {
        let mut pdf = load_one_page_pdf();
        let outline_root_ref = crate::ObjectRef::new(100, 0);
        let item_ref = crate::ObjectRef::new(101, 0);
        let null_child_ref = crate::ObjectRef::new(102, 0);

        let mut outline_root = Dictionary::new();
        outline_root.insert("First", Object::Reference(item_ref));
        patch_outline_root(&mut pdf, outline_root_ref, outline_root);

        let mut item = Dictionary::new();
        item.insert("Title", Object::String(b"Parent".to_vec()));
        item.insert("First", Object::Reference(null_child_ref));
        pdf.set_object(item_ref, Object::Dictionary(item));
        pdf.set_object(null_child_ref, Object::Null);

        let result = build_outlines_section(&mut pdf).unwrap();
        let entries = json_array(&result);
        let parent = json_object(&entries[0]);
        assert_eq!(value_for_key(parent, "kids"), &JsonValue::Array(Vec::new()));
    }

    #[test]
    fn outline_json_v2_resolves_a_multi_hop_catalog_dest_holder() {
        let mut pdf = load_one_page_pdf();
        let page_ref = crate::pages::page_refs(&mut pdf).unwrap()[0];
        let outline_root_ref = crate::ObjectRef::new(100, 0);
        let item_ref = crate::ObjectRef::new(101, 0);
        let first_holder_ref = crate::ObjectRef::new(110, 0);
        let dests_ref = crate::ObjectRef::new(111, 0);

        let mut outline_root = Dictionary::new();
        outline_root.insert("First", Object::Reference(item_ref));
        patch_outline_root(&mut pdf, outline_root_ref, outline_root);

        let mut item = Dictionary::new();
        item.insert("Title", Object::String(b"Named".to_vec()));
        item.insert("Dest", Object::Name(b"named".to_vec()));
        pdf.set_object(item_ref, Object::Dictionary(item));

        let mut dests = Dictionary::new();
        dests.insert(
            "named",
            Object::Array(vec![
                Object::Reference(page_ref),
                Object::Name(b"Fit".to_vec()),
            ]),
        );
        pdf.set_object(first_holder_ref, Object::Reference(dests_ref));
        pdf.set_object(dests_ref, Object::Dictionary(dests));

        let catalog_ref = pdf.root_ref().unwrap();
        let mut catalog = pdf.resolve(catalog_ref).unwrap().as_dict().unwrap().clone();
        catalog.insert("Dests", Object::Reference(first_holder_ref));
        pdf.set_object(catalog_ref, Object::Dictionary(catalog));

        let result = build_outlines_section(&mut pdf).unwrap();
        let entries = json_array(&result);
        let item = json_object(&entries[0]);
        assert_eq!(
            value_for_key(item, "dest"),
            &JsonValue::Array(vec![
                JsonValue::String(page_ref.to_string()),
                JsonValue::String("/Fit".into()),
            ])
        );
        assert_eq!(
            value_for_key(item, "destpageposfrom1"),
            &JsonValue::Integer(1)
        );
    }

    /// Helper: inject a synthetic /Outlines tree into the catalog of `pdf`.
    ///
    /// Creates the outline root dict at `outline_root_ref`, then places it
    /// in the catalog's /Outlines entry.
    fn patch_outline_root(
        pdf: &mut crate::Pdf<std::io::Cursor<Vec<u8>>>,
        outline_root_ref: crate::ObjectRef,
        outline_root: Dictionary,
    ) {
        // Wire catalog → outline root.
        let catalog_ref = pdf.root_ref().expect("no /Root");
        let mut catalog = match pdf.resolve_borrowed(catalog_ref).expect("resolve catalog") {
            Object::Dictionary(d) => d.clone(),
            _ => panic!("catalog is not a Dictionary"),
        };
        catalog.insert("Outlines", Object::Reference(outline_root_ref));
        pdf.set_object(catalog_ref, Object::Dictionary(catalog));
        pdf.set_object(outline_root_ref, Object::Dictionary(outline_root));
    }

    // ── Test 1: No /Outlines → empty array ───────────────────────────────────

    #[test]
    fn outlines_missing_returns_empty_array() {
        // one-page.pdf has no /Outlines — must return [].
        let mut pdf = load_one_page_pdf();
        let result = build_outlines_section(&mut pdf).expect("build_outlines_section failed");
        assert_eq!(
            result,
            JsonValue::Array(vec![]),
            "missing /Outlines must yield empty array"
        );
    }

    // ── Test 1b: All compat fixtures produce empty outlines ──────────────────

    #[test]
    fn outlines_compat_fixtures_all_empty() {
        let fixtures = ["one-page.pdf", "three-page.pdf", "attachment-two-page.pdf"];
        for name in fixtures {
            let mut pdf = load_fixture_pdf(name);
            let result = build_outlines_section(&mut pdf)
                .unwrap_or_else(|e| panic!("{name}: build_outlines_section failed: {e:?}"));
            assert_eq!(
                result,
                JsonValue::Array(vec![]),
                "{name}: expected empty outlines array"
            );
        }
    }

    // ── Test 2: Single entry — synthetic PDF ─────────────────────────────────

    #[test]
    fn outlines_single_entry() {
        let mut pdf = load_one_page_pdf();

        let outline_root_ref = crate::ObjectRef::new(100, 0);
        let item_ref = crate::ObjectRef::new(101, 0);

        // Create outline root dictionary pointing to the single item.
        let mut outline_root = Dictionary::new();
        outline_root.insert("Type", Object::Name(b"Outlines".to_vec()));
        outline_root.insert("First", Object::Reference(item_ref));
        outline_root.insert("Last", Object::Reference(item_ref));
        outline_root.insert("Count", Object::Integer(1));
        patch_outline_root(&mut pdf, outline_root_ref, outline_root);

        // Create a single outline item.
        let mut item = Dictionary::new();
        item.insert("Title", Object::String(b"Chapter 1".to_vec()));
        item.insert("Parent", Object::Reference(outline_root_ref));
        pdf.set_object(item_ref, Object::Dictionary(item));

        let result = build_outlines_section(&mut pdf).expect("build_outlines_section failed");
        let JsonValue::Array(entries) = &result else {
            panic!("expected Array, got {result:?}");
        };
        assert_eq!(entries.len(), 1, "expected 1 outline entry");

        let JsonValue::Object(entry) = &entries[0] else {
            panic!("entry is not an Object");
        };

        // Key order: dest, destpageposfrom1, kids, object, open, title.
        let keys: Vec<&str> = entry.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "dest",
                "destpageposfrom1",
                "kids",
                "object",
                "open",
                "title"
            ],
            "key order must be alphabetical"
        );

        assert_eq!(entry[0].1, JsonValue::Null, "dest must be Null");
        assert_eq!(entry[1].1, JsonValue::Null, "page position must be Null");
        // kids = [] (no /First in item)
        assert_eq!(entry[2].1, JsonValue::Array(vec![]), "kids must be empty");
        // object = "101 0 R"
        assert_eq!(
            entry[3].1,
            JsonValue::String("101 0 R".to_string()),
            "object mismatch"
        );
        assert_eq!(entry[4].1, JsonValue::Bool(true), "open must default true");
        // title = bare "Chapter 1" (no u: prefix)
        assert_eq!(
            entry[5].1,
            JsonValue::String("Chapter 1".to_string()),
            "title must be bare string without u: prefix"
        );
    }

    // ── Test 3: Hierarchical tree (parent + 2 children) ──────────────────────

    #[test]
    fn outlines_hierarchical_tree() {
        let mut pdf = load_one_page_pdf();

        let outline_root_ref = crate::ObjectRef::new(100, 0);
        let parent_ref = crate::ObjectRef::new(101, 0);
        let child1_ref = crate::ObjectRef::new(102, 0);
        let child2_ref = crate::ObjectRef::new(103, 0);

        // Outline root → parent.
        let mut outline_root = Dictionary::new();
        outline_root.insert("Type", Object::Name(b"Outlines".to_vec()));
        outline_root.insert("First", Object::Reference(parent_ref));
        outline_root.insert("Last", Object::Reference(parent_ref));
        outline_root.insert("Count", Object::Integer(3)); // parent + 2 children
        patch_outline_root(&mut pdf, outline_root_ref, outline_root);

        // Parent item with 2 children.
        let mut parent = Dictionary::new();
        parent.insert("Title", Object::String(b"Part 1".to_vec()));
        parent.insert("Parent", Object::Reference(outline_root_ref));
        parent.insert("First", Object::Reference(child1_ref));
        parent.insert("Last", Object::Reference(child2_ref));
        parent.insert("Count", Object::Integer(2));
        pdf.set_object(parent_ref, Object::Dictionary(parent));

        // Child 1.
        let mut child1 = Dictionary::new();
        child1.insert("Title", Object::String(b"Chapter 1".to_vec()));
        child1.insert("Parent", Object::Reference(parent_ref));
        child1.insert("Next", Object::Reference(child2_ref));
        pdf.set_object(child1_ref, Object::Dictionary(child1));

        // Child 2.
        let mut child2 = Dictionary::new();
        child2.insert("Title", Object::String(b"Chapter 2".to_vec()));
        child2.insert("Parent", Object::Reference(parent_ref));
        child2.insert("Prev", Object::Reference(child1_ref));
        pdf.set_object(child2_ref, Object::Dictionary(child2));

        let result = build_outlines_section(&mut pdf).expect("build_outlines_section failed");
        let JsonValue::Array(root_entries) = &result else {
            panic!("expected Array");
        };
        assert_eq!(root_entries.len(), 1, "root chain has 1 entry (parent)");

        let JsonValue::Object(parent_entry) = &root_entries[0] else {
            panic!("parent entry is not an Object");
        };
        // kids should contain 2 children.
        let kids_val = &parent_entry.iter().find(|(k, _)| k == "kids").unwrap().1;
        let JsonValue::Array(kids) = kids_val else {
            panic!("kids is not an Array");
        };
        assert_eq!(kids.len(), 2, "parent must have 2 children in kids");

        // Verify child titles.
        let get_title = |entry: &JsonValue| {
            let JsonValue::Object(pairs) = entry else {
                panic!("kid entry is not an Object");
            };
            pairs.iter().find(|(k, _)| k == "title").unwrap().1.clone()
        };
        assert_eq!(
            get_title(&kids[0]),
            JsonValue::String("Chapter 1".to_string())
        );
        assert_eq!(
            get_title(&kids[1]),
            JsonValue::String("Chapter 2".to_string())
        );
    }

    // ── Test 4: Cycle guard — /Next pointing to itself ────────────────────────

    #[test]
    fn outlines_cycle_guard_prevents_infinite_loop() {
        let mut pdf = load_one_page_pdf();

        let outline_root_ref = crate::ObjectRef::new(100, 0);
        let item_ref = crate::ObjectRef::new(101, 0);

        // Outline root → item.
        let mut outline_root = Dictionary::new();
        outline_root.insert("Type", Object::Name(b"Outlines".to_vec()));
        outline_root.insert("First", Object::Reference(item_ref));
        outline_root.insert("Last", Object::Reference(item_ref));
        patch_outline_root(&mut pdf, outline_root_ref, outline_root);

        // Item whose /Next points back to itself (cycle).
        let mut item = Dictionary::new();
        item.insert("Title", Object::String(b"Loop".to_vec()));
        item.insert("Parent", Object::Reference(outline_root_ref));
        item.insert("Next", Object::Reference(item_ref)); // self-loop!
        pdf.set_object(item_ref, Object::Dictionary(item));

        // Must not hang; must return exactly 1 entry (the item itself, not looped).
        let result = build_outlines_section(&mut pdf).expect("build_outlines_section failed");
        let JsonValue::Array(entries) = &result else {
            panic!("expected Array");
        };
        assert_eq!(
            entries.len(),
            1,
            "cycle guard must stop after 1 entry, got {}",
            entries.len()
        );
    }

    // ── Test 5: Broken /Parent link does not crash ────────────────────────────

    #[test]
    fn outlines_broken_parent_link_does_not_crash() {
        let mut pdf = load_one_page_pdf();

        let outline_root_ref = crate::ObjectRef::new(100, 0);
        let item_ref = crate::ObjectRef::new(101, 0);

        // Outline root → item.
        let mut outline_root = Dictionary::new();
        outline_root.insert("Type", Object::Name(b"Outlines".to_vec()));
        outline_root.insert("First", Object::Reference(item_ref));
        outline_root.insert("Last", Object::Reference(item_ref));
        patch_outline_root(&mut pdf, outline_root_ref, outline_root);

        // Item with a /Parent pointing to a non-existent object (broken link).
        let mut item = Dictionary::new();
        item.insert("Title", Object::String(b"Broken Parent".to_vec()));
        item.insert("Parent", Object::Reference(crate::ObjectRef::new(999, 0))); // non-existent
        pdf.set_object(item_ref, Object::Dictionary(item));

        // Must not crash — /Parent is never followed by our implementation.
        let result = build_outlines_section(&mut pdf).expect("build_outlines_section failed");
        let JsonValue::Array(entries) = &result else {
            panic!("expected Array");
        };
        assert_eq!(entries.len(), 1, "expected 1 entry despite broken /Parent");
        let JsonValue::Object(entry) = &entries[0] else {
            panic!("entry is not an Object");
        };
        let title = entry.iter().find(|(k, _)| k == "title").unwrap().1.clone();
        assert_eq!(title, JsonValue::String("Broken Parent".to_string()));
    }

    // ── Test 6: build_qpdf_json_v2 includes outlines section ─────────────────

    #[test]
    fn build_qpdf_json_v2_includes_outlines_section() {
        let mut pdf = load_one_page_pdf();
        let v2 = build_qpdf_json_v2(&mut pdf, DecodeLevel::Generalized)
            .expect("build_qpdf_json_v2 failed");
        let JsonValue::Object(pairs) = v2 else {
            panic!("expected Object at top level");
        };
        let outlines = pairs
            .iter()
            .find(|(k, _)| k == "outlines")
            .map(|(_, v)| v)
            .expect("outlines key must be present in composite output");
        assert!(
            matches!(outlines, JsonValue::Array(_)),
            "outlines must be an Array"
        );
        // one-page.pdf has no /Outlines → must be empty array
        assert_eq!(
            *outlines,
            JsonValue::Array(vec![]),
            "one-page.pdf has no outlines"
        );
    }

    // ── Test 7: Unicode title via UTF-16BE BOM ────────────────────────────────

    #[test]
    fn outlines_utf16be_title_decoded_as_bare_string() {
        let mut pdf = load_one_page_pdf();

        let outline_root_ref = crate::ObjectRef::new(100, 0);
        let item_ref = crate::ObjectRef::new(101, 0);

        let mut outline_root = Dictionary::new();
        outline_root.insert("Type", Object::Name(b"Outlines".to_vec()));
        outline_root.insert("First", Object::Reference(item_ref));
        outline_root.insert("Last", Object::Reference(item_ref));
        patch_outline_root(&mut pdf, outline_root_ref, outline_root);

        // UTF-16BE BOM + "AB" (0x0041 0x0042)
        let title_bytes = vec![0xFE, 0xFF, 0x00, 0x41, 0x00, 0x42];
        let mut item = Dictionary::new();
        item.insert("Title", Object::String(title_bytes));
        item.insert("Parent", Object::Reference(outline_root_ref));
        pdf.set_object(item_ref, Object::Dictionary(item));

        let result = build_outlines_section(&mut pdf).expect("build_outlines_section failed");
        let JsonValue::Array(entries) = &result else {
            panic!("expected Array");
        };
        let JsonValue::Object(entry) = &entries[0] else {
            panic!("entry is not an Object");
        };
        let title = entry.iter().find(|(k, _)| k == "title").unwrap().1.clone();
        // Must be bare "AB" — no "u:" prefix.
        assert_eq!(title, JsonValue::String("AB".to_string()));
    }

    // ── Test 8: raw actions are projected only through resolved dest ──────
    //
    // The JSON projection exposes the resolved destination, not the raw action.

    #[test]
    fn outlines_non_goto_action_yields_null_dest() {
        let mut pdf = load_one_page_pdf();

        let outline_root_ref = crate::ObjectRef::new(100, 0);
        let item_ref = crate::ObjectRef::new(101, 0);

        let mut outline_root = Dictionary::new();
        outline_root.insert("Type", Object::Name(b"Outlines".to_vec()));
        outline_root.insert("First", Object::Reference(item_ref));
        outline_root.insert("Last", Object::Reference(item_ref));
        patch_outline_root(&mut pdf, outline_root_ref, outline_root);

        // /A is a direct URI action dictionary, not a destination.
        let mut action = Dictionary::new();
        action.insert("S", Object::Name(b"URI".to_vec()));
        action.insert("URI", Object::String(b"https://example.com".to_vec()));

        let mut item = Dictionary::new();
        item.insert("Title", Object::String(b"Visit example".to_vec()));
        item.insert("Parent", Object::Reference(outline_root_ref));
        item.insert("A", Object::Dictionary(action));
        pdf.set_object(item_ref, Object::Dictionary(item));

        let result = build_outlines_section(&mut pdf).expect("build_outlines_section failed");
        let JsonValue::Array(entries) = &result else {
            panic!("expected Array");
        };
        let JsonValue::Object(entry) = &entries[0] else {
            panic!("entry is not an Object");
        };
        assert_eq!(value_for_key(entry, "dest"), &JsonValue::Null);
        assert_eq!(
            value_for_key(entry, "object"),
            &JsonValue::String("101 0 R".to_string())
        );
        assert!(entry.iter().all(|(key, _)| key != "action"));
    }

    #[test]
    fn outlines_goto_action_without_destination_yields_null_dest() {
        let mut pdf = load_one_page_pdf();
        let outline_root_ref = crate::ObjectRef::new(100, 0);
        let item_ref = crate::ObjectRef::new(101, 0);
        let action_ref = crate::ObjectRef::new(102, 0);

        let mut outline_root = Dictionary::new();
        outline_root.insert("Type", Object::Name(b"Outlines".to_vec()));
        outline_root.insert("First", Object::Reference(item_ref));
        outline_root.insert("Last", Object::Reference(item_ref));
        patch_outline_root(&mut pdf, outline_root_ref, outline_root);

        let mut action = Dictionary::new();
        action.insert("S", Object::Name(b"GoTo".to_vec()));
        pdf.set_object(action_ref, Object::Dictionary(action));

        let mut item = Dictionary::new();
        item.insert("Title", Object::String(b"Go".to_vec()));
        item.insert("Parent", Object::Reference(outline_root_ref));
        item.insert("A", Object::Reference(action_ref));
        pdf.set_object(item_ref, Object::Dictionary(item));

        let result = build_outlines_section(&mut pdf).expect("build_outlines_section failed");
        let JsonValue::Array(entries) = result else {
            panic!("expected Array");
        };
        let JsonValue::Object(entry) = &entries[0] else {
            panic!("entry is not an Object");
        };
        assert_eq!(value_for_key(entry, "dest"), &JsonValue::Null);
        assert_eq!(
            value_for_key(entry, "object"),
            &JsonValue::String("101 0 R".to_string())
        );
    }

    // ══════════════════════════════════════════════════════════════════════════
    // build_acroform_section tests (flpdf-9hc.11.7)
    // ══════════════════════════════════════════════════════════════════════════

    /// Helper: inject a synthetic /AcroForm into the catalog.
    fn patch_acroform(
        pdf: &mut crate::Pdf<std::io::Cursor<Vec<u8>>>,
        acroform_ref: crate::ObjectRef,
        acroform: Dictionary,
    ) {
        let catalog_ref = pdf.root_ref().expect("no /Root");
        let mut catalog = match pdf.resolve_borrowed(catalog_ref).expect("resolve catalog") {
            Object::Dictionary(d) => d.clone(),
            _ => panic!("catalog is not a Dictionary"),
        };
        catalog.insert("AcroForm", Object::Reference(acroform_ref));
        pdf.set_object(catalog_ref, Object::Dictionary(catalog));
        pdf.set_object(acroform_ref, Object::Dictionary(acroform));
    }

    // ── acroform Test 1: No /AcroForm → hasacroform: false, empty fields ──────

    #[test]
    fn acroform_missing_returns_empty() {
        let mut pdf = load_one_page_pdf();
        let result = build_acroform_section(&mut pdf).expect("build_acroform_section failed");
        let JsonValue::Object(pairs) = &result else {
            panic!("expected Object, got {result:?}");
        };
        let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["fields", "hasacroform", "needappearances"]);
        assert_eq!(pairs[0].1, JsonValue::Array(vec![]), "fields must be empty");
        assert_eq!(
            pairs[1].1,
            JsonValue::Bool(false),
            "hasacroform must be false"
        );
        assert_eq!(
            pairs[2].1,
            JsonValue::Bool(false),
            "needappearances must be false"
        );
    }

    // ── acroform Test 2: AcroForm present, empty /Fields → hasacroform: true ──

    #[test]
    fn acroform_present_empty_fields() {
        let mut pdf = load_one_page_pdf();

        let acroform_ref = crate::ObjectRef::new(200, 0);
        let mut acroform = Dictionary::new();
        acroform.insert("Fields", Object::Array(vec![]));
        patch_acroform(&mut pdf, acroform_ref, acroform);

        let result = build_acroform_section(&mut pdf).expect("build_acroform_section failed");
        let JsonValue::Object(pairs) = &result else {
            panic!("expected Object");
        };
        assert_eq!(pairs[0].1, JsonValue::Array(vec![]), "fields must be empty");
        assert_eq!(
            pairs[1].1,
            JsonValue::Bool(true),
            "hasacroform must be true"
        );
        assert_eq!(
            pairs[2].1,
            JsonValue::Bool(false),
            "needappearances must be false"
        );
    }

    // ── acroform Test 3: Single leaf field (synthetic) ────────────────────────

    #[test]
    fn acroform_single_leaf_field() {
        let mut pdf = load_one_page_pdf();

        let acroform_ref = crate::ObjectRef::new(200, 0);
        let field_ref = crate::ObjectRef::new(201, 0);

        let mut acroform = Dictionary::new();
        acroform.insert("Fields", Object::Array(vec![Object::Reference(field_ref)]));
        acroform.insert("NeedAppearances", Object::Boolean(true));
        patch_acroform(&mut pdf, acroform_ref, acroform);

        let mut field = Dictionary::new();
        field.insert("T", Object::String(b"firstname".to_vec()));
        field.insert("FT", Object::Name(b"Tx".to_vec()));
        field.insert("V", Object::String(b"John".to_vec()));
        field.insert("Ff", Object::Integer(0));
        pdf.set_object(field_ref, Object::Dictionary(field));

        let result = build_acroform_section(&mut pdf).expect("build_acroform_section failed");
        let JsonValue::Object(top) = &result else {
            panic!("expected Object");
        };
        assert_eq!(top[1].1, JsonValue::Bool(true), "hasacroform must be true");
        assert_eq!(
            top[2].1,
            JsonValue::Bool(true),
            "needappearances must be true"
        );

        let JsonValue::Array(fields) = &top[0].1 else {
            panic!("fields must be Array");
        };
        assert_eq!(fields.len(), 1, "expected 1 field entry");

        let JsonValue::Object(entry) = &fields[0] else {
            panic!("field entry must be Object");
        };
        let keys: Vec<&str> = entry.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "alternatename",
                "annotations",
                "defaultvalue",
                "fieldflags",
                "fieldtype",
                "fullname",
                "mappingname",
                "object",
                "parent",
                "value"
            ],
            "key order must be alphabetical"
        );

        // Check values
        assert_eq!(entry[0].1, JsonValue::Null, "alternatename must be null");
        assert_eq!(
            entry[1].1,
            JsonValue::Array(vec![]),
            "annotations must be empty (no widget)"
        );
        assert_eq!(entry[2].1, JsonValue::Null, "defaultvalue must be null");
        assert_eq!(entry[3].1, JsonValue::Integer(0), "fieldflags must be 0");
        assert_eq!(
            entry[4].1,
            JsonValue::String("Tx".to_string()),
            "fieldtype must be Tx (bare, no /)"
        );
        assert_eq!(
            entry[5].1,
            JsonValue::String("firstname".to_string()),
            "fullname must match /T"
        );
        assert_eq!(entry[6].1, JsonValue::Null, "mappingname must be null");
        assert_eq!(
            entry[7].1,
            JsonValue::String("201 0 R".to_string()),
            "object must be ref string"
        );
        assert_eq!(entry[8].1, JsonValue::Null, "parent must be null at root");
        assert_eq!(
            entry[9].1,
            JsonValue::String("u:John".to_string()),
            "value must be u:John"
        );
    }

    // ── acroform Test 4: parent + child, fullname = "parent.child" ───────────

    #[test]
    fn acroform_parent_child_fullname() {
        let mut pdf = load_one_page_pdf();

        let acroform_ref = crate::ObjectRef::new(200, 0);
        let parent_field_ref = crate::ObjectRef::new(201, 0);
        let child_field_ref = crate::ObjectRef::new(202, 0);

        let mut acroform = Dictionary::new();
        acroform.insert(
            "Fields",
            Object::Array(vec![Object::Reference(parent_field_ref)]),
        );
        patch_acroform(&mut pdf, acroform_ref, acroform);

        // Parent field (has /Kids → sub-field child, so parent itself is NOT a leaf)
        let mut parent_field = Dictionary::new();
        parent_field.insert("T", Object::String(b"person".to_vec()));
        parent_field.insert("FT", Object::Name(b"Tx".to_vec()));
        parent_field.insert(
            "Kids",
            Object::Array(vec![Object::Reference(child_field_ref)]),
        );
        pdf.set_object(parent_field_ref, Object::Dictionary(parent_field));

        // Child field (leaf, has /T)
        let mut child_field = Dictionary::new();
        child_field.insert("T", Object::String(b"name".to_vec()));
        child_field.insert("FT", Object::Name(b"Tx".to_vec()));
        child_field.insert("Parent", Object::Reference(parent_field_ref));
        pdf.set_object(child_field_ref, Object::Dictionary(child_field));

        let result = build_acroform_section(&mut pdf).expect("build_acroform_section failed");
        let JsonValue::Object(top) = &result else {
            panic!("expected Object");
        };
        let JsonValue::Array(fields) = &top[0].1 else {
            panic!("fields must be Array");
        };
        // flat list: parent + child = 2 entries
        assert_eq!(fields.len(), 2, "expected 2 entries (parent + child)");

        // First entry is the parent
        let JsonValue::Object(parent_entry) = &fields[0] else {
            panic!("first entry must be Object");
        };
        let parent_fullname = parent_entry
            .iter()
            .find(|(k, _)| k == "fullname")
            .map(|(_, v)| v)
            .expect("fullname missing");
        assert_eq!(
            *parent_fullname,
            JsonValue::String("person".to_string()),
            "parent fullname must be 'person'"
        );
        let parent_obj = parent_entry
            .iter()
            .find(|(k, _)| k == "object")
            .map(|(_, v)| v)
            .expect("object missing");
        assert_eq!(*parent_obj, JsonValue::String("201 0 R".to_string()));

        // Second entry is the child
        let JsonValue::Object(child_entry) = &fields[1] else {
            panic!("second entry must be Object");
        };
        let child_fullname = child_entry
            .iter()
            .find(|(k, _)| k == "fullname")
            .map(|(_, v)| v)
            .expect("fullname missing");
        assert_eq!(
            *child_fullname,
            JsonValue::String("person.name".to_string()),
            "child fullname must be 'person.name'"
        );
        let child_parent = child_entry
            .iter()
            .find(|(k, _)| k == "parent")
            .map(|(_, v)| v)
            .expect("parent missing");
        assert_eq!(
            *child_parent,
            JsonValue::String("201 0 R".to_string()),
            "child parent must point to 201 0 R"
        );
    }

    // ── acroform Test 5: NeedAppearances = true ───────────────────────────────

    #[test]
    fn acroform_needappearances_true() {
        let mut pdf = load_one_page_pdf();

        let acroform_ref = crate::ObjectRef::new(200, 0);
        let mut acroform = Dictionary::new();
        acroform.insert("Fields", Object::Array(vec![]));
        acroform.insert("NeedAppearances", Object::Boolean(true));
        patch_acroform(&mut pdf, acroform_ref, acroform);

        let result = build_acroform_section(&mut pdf).expect("build_acroform_section failed");
        let JsonValue::Object(pairs) = &result else {
            panic!("expected Object");
        };
        assert_eq!(
            pairs[2].1,
            JsonValue::Bool(true),
            "needappearances must be true"
        );
    }

    // ── acroform Test 6: AcroForm.Fields absent → fields: [] ─────────────────

    #[test]
    fn acroform_no_fields_key_yields_empty() {
        let mut pdf = load_one_page_pdf();

        let acroform_ref = crate::ObjectRef::new(200, 0);
        // AcroForm without /Fields key
        let acroform = Dictionary::new();
        patch_acroform(&mut pdf, acroform_ref, acroform);

        let result = build_acroform_section(&mut pdf).expect("build_acroform_section failed");
        let JsonValue::Object(pairs) = &result else {
            panic!("expected Object");
        };
        assert_eq!(
            pairs[0].1,
            JsonValue::Array(vec![]),
            "fields must be empty when /Fields absent"
        );
        assert_eq!(
            pairs[1].1,
            JsonValue::Bool(true),
            "hasacroform must be true even with no /Fields"
        );
    }

    // ── acroform: all compat fixtures produce hasacroform: false ─────────────

    #[test]
    fn acroform_compat_fixtures_all_no_acroform() {
        let fixtures = ["one-page.pdf", "three-page.pdf", "attachment-two-page.pdf"];
        for name in fixtures {
            let mut pdf = load_fixture_pdf(name);
            let result = build_acroform_section(&mut pdf)
                .unwrap_or_else(|e| panic!("{name}: build_acroform_section failed: {e:?}"));
            let JsonValue::Object(pairs) = &result else {
                panic!("{name}: expected Object");
            };
            assert_eq!(
                pairs[1].1,
                JsonValue::Bool(false),
                "{name}: hasacroform must be false"
            );
            assert_eq!(
                pairs[0].1,
                JsonValue::Array(vec![]),
                "{name}: fields must be empty"
            );
        }
    }

    // ── acroform: build_qpdf_json_v2 has acroform key before outlines ─────────

    #[test]
    fn build_qpdf_json_v2_has_expected_top_level_keys_with_acroform() {
        let mut pdf = load_one_page_pdf();
        let v2 = build_qpdf_json_v2(&mut pdf, DecodeLevel::Generalized)
            .expect("build_qpdf_json_v2 failed");
        let JsonValue::Object(pairs) = v2 else {
            panic!("expected Object at top level");
        };
        let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "version",
                "parameters",
                "pages",
                "pagelabels",
                "acroform",
                "attachments",
                "encrypt",
                "outlines",
                "qpdf"
            ],
            "key order must match qpdf v2: acroform, attachments, encrypt, outlines"
        );
    }

    // ── acroform: inheritable fields /V, /DV, /Ff propagate from parent ──────
    //
    // Regression for CodeRabbit's flpdf-9hc.11.7 review: /V, /DV, /Ff are
    // inheritable per ISO 32000-1 §12.7.3.1, just like /FT. A child field
    // that omits them must still see the ancestor value in the JSON output.

    #[test]
    fn acroform_inheritable_fields_descend_from_parent() {
        let mut pdf = load_one_page_pdf();

        let acroform_ref = crate::ObjectRef::new(200, 0);
        let parent_field_ref = crate::ObjectRef::new(201, 0);
        let child_field_ref = crate::ObjectRef::new(202, 0);

        let mut acroform = Dictionary::new();
        acroform.insert(
            "Fields",
            Object::Array(vec![Object::Reference(parent_field_ref)]),
        );
        patch_acroform(&mut pdf, acroform_ref, acroform);

        // Parent carries /FT, /V, /DV, /Ff. Child has only /T and /Parent
        // so it must inherit all four through the /Parent chain.
        let mut parent_field = Dictionary::new();
        parent_field.insert("T", Object::String(b"person".to_vec()));
        parent_field.insert("FT", Object::Name(b"Tx".to_vec()));
        parent_field.insert("V", Object::String(b"alice".to_vec()));
        parent_field.insert("DV", Object::String(b"default-alice".to_vec()));
        parent_field.insert("Ff", Object::Integer(8192));
        parent_field.insert(
            "Kids",
            Object::Array(vec![Object::Reference(child_field_ref)]),
        );
        pdf.set_object(parent_field_ref, Object::Dictionary(parent_field));

        let mut child_field = Dictionary::new();
        child_field.insert("T", Object::String(b"name".to_vec()));
        child_field.insert("Parent", Object::Reference(parent_field_ref));
        pdf.set_object(child_field_ref, Object::Dictionary(child_field));

        let result = build_acroform_section(&mut pdf).expect("build_acroform_section failed");
        let JsonValue::Object(top) = &result else {
            panic!("expected Object");
        };
        let JsonValue::Array(fields) = &top[0].1 else {
            panic!("fields must be Array");
        };
        // The child is the second entry (parent listed first).
        let JsonValue::Object(child_entry) = &fields[1] else {
            panic!("child entry must be Object");
        };

        let get = |k: &str| -> JsonValue {
            child_entry
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
                .unwrap_or(JsonValue::Null)
        };

        assert_eq!(
            get("fieldtype"),
            JsonValue::String("Tx".to_string()),
            "child must inherit /FT from parent"
        );
        assert_eq!(
            get("value"),
            JsonValue::String("u:alice".to_string()),
            "child must inherit /V from parent"
        );
        assert_eq!(
            get("defaultvalue"),
            JsonValue::String("u:default-alice".to_string()),
            "child must inherit /DV from parent"
        );
        assert_eq!(
            get("fieldflags"),
            JsonValue::Integer(8192),
            "child must inherit /Ff from parent"
        );
    }

    // ── acroform: unnamed intermediate field still recursed into ─────────────
    //
    // Regression for the CodeRabbit finding on kid classification: a /Kids
    // member that is a field dictionary (has /Kids or /FT or /Parent) but
    // lacks /T is a valid unnamed intermediate grouping node per ISO
    // 32000-1 §12.7.3.1. The previous "has /T -> sub-field, else widget"
    // rule dropped such nodes and silently lost every descendant.

    #[test]
    fn acroform_unnamed_intermediate_field_with_kids_recurses() {
        let mut pdf = load_one_page_pdf();

        let acroform_ref = crate::ObjectRef::new(200, 0);
        let unnamed_ref = crate::ObjectRef::new(201, 0); // no /T, but has /Kids
        let leaf_ref = crate::ObjectRef::new(202, 0); // has /T, real leaf

        let mut acroform = Dictionary::new();
        acroform.insert(
            "Fields",
            Object::Array(vec![Object::Reference(unnamed_ref)]),
        );
        patch_acroform(&mut pdf, acroform_ref, acroform);

        // Unnamed intermediate field: no /T, no /Subtype, but has /Kids.
        // Must be classified as a field (so we recurse into its Kids).
        let mut unnamed = Dictionary::new();
        unnamed.insert("FT", Object::Name(b"Tx".to_vec()));
        unnamed.insert("Kids", Object::Array(vec![Object::Reference(leaf_ref)]));
        pdf.set_object(unnamed_ref, Object::Dictionary(unnamed));

        // Real leaf field with /T — must reach the flat list.
        let mut leaf = Dictionary::new();
        leaf.insert("T", Object::String(b"name".to_vec()));
        leaf.insert("Parent", Object::Reference(unnamed_ref));
        pdf.set_object(leaf_ref, Object::Dictionary(leaf));

        let result = build_acroform_section(&mut pdf).expect("build_acroform_section failed");
        let JsonValue::Object(top) = &result else {
            panic!("expected Object");
        };
        let JsonValue::Array(fields) = &top[0].1 else {
            panic!("fields must be Array");
        };
        // Both the unnamed intermediate and the leaf must appear (2 entries).
        assert_eq!(
            fields.len(),
            2,
            "expected 2 entries: unnamed intermediate + leaf"
        );

        // The leaf entry must be present with fullname == "name" (intermediate
        // contributed no name segment because /T was absent).
        let JsonValue::Object(leaf_entry) = &fields[1] else {
            panic!("leaf entry must be Object");
        };
        let leaf_fullname = leaf_entry
            .iter()
            .find(|(k, _)| k == "fullname")
            .map(|(_, v)| v.clone())
            .unwrap();
        assert_eq!(
            leaf_fullname,
            JsonValue::String("name".to_string()),
            "leaf fullname must be 'name' (unnamed intermediate contributes no segment)"
        );

        // Leaf must inherit /FT from the unnamed intermediate.
        let leaf_ft = leaf_entry
            .iter()
            .find(|(k, _)| k == "fieldtype")
            .map(|(_, v)| v.clone())
            .unwrap();
        assert_eq!(leaf_ft, JsonValue::String("Tx".to_string()));
    }

    // ── acroform: widget kid with /Parent is still classified as annotation ─
    //
    // Regression for CodeRabbit's 2nd-pass review on kid classification:
    // tightening kid-recursion to "field-like entries" must not regress the
    // common case of a /Subtype /Widget kid that carries /Parent (the normal
    // way a widget annotation refers back to its owning field). Such kids
    // must end up under the parent field's annotations[], not get recursed
    // into as bogus field entries.

    #[test]
    fn acroform_widget_kid_with_parent_classified_as_annotation() {
        let mut pdf = load_one_page_pdf();

        let acroform_ref = crate::ObjectRef::new(200, 0);
        let field_ref = crate::ObjectRef::new(201, 0);
        let widget_ref = crate::ObjectRef::new(202, 0);

        let mut acroform = Dictionary::new();
        acroform.insert("Fields", Object::Array(vec![Object::Reference(field_ref)]));
        patch_acroform(&mut pdf, acroform_ref, acroform);

        // Field with /T and a single widget kid.
        let mut field = Dictionary::new();
        field.insert("T", Object::String(b"signature".to_vec()));
        field.insert("FT", Object::Name(b"Sig".to_vec()));
        field.insert("Kids", Object::Array(vec![Object::Reference(widget_ref)]));
        pdf.set_object(field_ref, Object::Dictionary(field));

        // Standalone widget annotation: /Subtype /Widget + /Parent. No /T,
        // /FT, or /Kids. Must end up under annotations[], not recursed into.
        let mut widget = Dictionary::new();
        widget.insert("Subtype", Object::Name(b"Widget".to_vec()));
        widget.insert("Parent", Object::Reference(field_ref));
        pdf.set_object(widget_ref, Object::Dictionary(widget));

        let result = build_acroform_section(&mut pdf).expect("build_acroform_section failed");
        let JsonValue::Object(top) = &result else {
            panic!("expected Object");
        };
        let JsonValue::Array(fields) = &top[0].1 else {
            panic!("fields must be Array");
        };
        // Only the parent field should be in the flat list — widget must
        // NOT be recursed into.
        assert_eq!(
            fields.len(),
            1,
            "widget kid with /Parent must NOT create a second field entry"
        );

        let JsonValue::Object(field_entry) = &fields[0] else {
            panic!("field entry must be Object");
        };
        let annotations = field_entry
            .iter()
            .find(|(k, _)| k == "annotations")
            .map(|(_, v)| v.clone())
            .unwrap();
        assert_eq!(
            annotations,
            JsonValue::Array(vec![JsonValue::String("202 0 R".to_string())]),
            "the widget kid must be listed in annotations, not promoted to a field"
        );
    }

    // ── acroform: merged widget+field with /V is classified as field ─────────
    //
    // Regression for CodeRabbit's 4th-pass review on kid classification. A
    // /Subtype /Widget dictionary that also carries field entries (/V, /DV,
    // /Ff, /TU, /TM) but no /T, /FT, or /Kids is a "merged widget+field"
    // dictionary — its local field state must be reflected in the JSON
    // entry, so the serializer must classify it as a field and recurse.

    #[test]
    fn acroform_merged_widget_field_with_local_value_is_classified_as_field() {
        let mut pdf = load_one_page_pdf();

        let acroform_ref = crate::ObjectRef::new(200, 0);
        let parent_field_ref = crate::ObjectRef::new(201, 0);
        let merged_ref = crate::ObjectRef::new(202, 0);

        let mut acroform = Dictionary::new();
        acroform.insert(
            "Fields",
            Object::Array(vec![Object::Reference(parent_field_ref)]),
        );
        patch_acroform(&mut pdf, acroform_ref, acroform);

        let mut parent_field = Dictionary::new();
        parent_field.insert("T", Object::String(b"address".to_vec()));
        parent_field.insert("FT", Object::Name(b"Tx".to_vec()));
        parent_field.insert("Kids", Object::Array(vec![Object::Reference(merged_ref)]));
        pdf.set_object(parent_field_ref, Object::Dictionary(parent_field));

        // Merged widget+field: /Subtype /Widget AND local /V (and /Ff).
        // No /T, /FT, /Kids — but still a field that should appear in the
        // flat list because it carries field state.
        let mut merged = Dictionary::new();
        merged.insert("Subtype", Object::Name(b"Widget".to_vec()));
        merged.insert("Parent", Object::Reference(parent_field_ref));
        merged.insert("V", Object::String(b"42 Somewhere".to_vec()));
        merged.insert("Ff", Object::Integer(4));
        pdf.set_object(merged_ref, Object::Dictionary(merged));

        let result = build_acroform_section(&mut pdf).expect("build_acroform_section failed");
        let JsonValue::Object(top) = &result else {
            panic!("expected Object");
        };
        let JsonValue::Array(fields) = &top[0].1 else {
            panic!("fields must be Array");
        };
        // Both parent and merged widget+field must appear.
        assert_eq!(
            fields.len(),
            2,
            "merged widget+field with /V must appear as a separate field entry"
        );

        // Second entry is the merged widget+field — verify its local /V and
        // /Ff are emitted, not the parent's.
        let JsonValue::Object(merged_entry) = &fields[1] else {
            panic!("merged entry must be Object");
        };
        let value = merged_entry
            .iter()
            .find(|(k, _)| k == "value")
            .map(|(_, v)| v.clone())
            .unwrap();
        assert_eq!(value, JsonValue::String("u:42 Somewhere".to_string()));
        let flags = merged_entry
            .iter()
            .find(|(k, _)| k == "fieldflags")
            .map(|(_, v)| v.clone())
            .unwrap();
        assert_eq!(flags, JsonValue::Integer(4));

        // The merged widget must also list itself in annotations[] because
        // it carries /Subtype /Widget.
        let annotations = merged_entry
            .iter()
            .find(|(k, _)| k == "annotations")
            .map(|(_, v)| v.clone())
            .unwrap();
        assert_eq!(
            annotations,
            JsonValue::Array(vec![JsonValue::String("202 0 R".to_string())])
        );
    }

    #[test]
    fn acroform_local_field_value_overrides_parent_inheritance() {
        // Verify that a locally-present /V still wins over the parent's /V.
        let mut pdf = load_one_page_pdf();

        let acroform_ref = crate::ObjectRef::new(200, 0);
        let parent_field_ref = crate::ObjectRef::new(201, 0);
        let child_field_ref = crate::ObjectRef::new(202, 0);

        let mut acroform = Dictionary::new();
        acroform.insert(
            "Fields",
            Object::Array(vec![Object::Reference(parent_field_ref)]),
        );
        patch_acroform(&mut pdf, acroform_ref, acroform);

        let mut parent_field = Dictionary::new();
        parent_field.insert("T", Object::String(b"person".to_vec()));
        parent_field.insert("V", Object::String(b"alice".to_vec()));
        parent_field.insert(
            "Kids",
            Object::Array(vec![Object::Reference(child_field_ref)]),
        );
        pdf.set_object(parent_field_ref, Object::Dictionary(parent_field));

        let mut child_field = Dictionary::new();
        child_field.insert("T", Object::String(b"name".to_vec()));
        child_field.insert("V", Object::String(b"bob".to_vec()));
        child_field.insert("Parent", Object::Reference(parent_field_ref));
        pdf.set_object(child_field_ref, Object::Dictionary(child_field));

        let result = build_acroform_section(&mut pdf).expect("build_acroform_section failed");
        let JsonValue::Object(top) = &result else {
            panic!("expected Object");
        };
        let JsonValue::Array(fields) = &top[0].1 else {
            panic!("fields must be Array");
        };
        let JsonValue::Object(child_entry) = &fields[1] else {
            panic!("child entry must be Object");
        };
        let value = child_entry
            .iter()
            .find(|(k, _)| k == "value")
            .map(|(_, v)| v.clone())
            .unwrap();
        assert_eq!(
            value,
            JsonValue::String("u:bob".to_string()),
            "local /V must win over parent's /V"
        );
    }

    // ── acroform: indirect /Fields and /Kids arrays are resolved ──────────────
    //
    // Regression for CodeRabbit's PR #116 finding. Both /Fields (at the
    // AcroForm top level) and /Kids (per field) can be an indirect
    // Reference to an Array. Without resolving the reference, the previous
    // code returned an empty vec and silently dropped the whole subtree.

    #[test]
    fn acroform_indirect_fields_array_is_resolved() {
        let mut pdf = load_one_page_pdf();

        let acroform_ref = crate::ObjectRef::new(200, 0);
        let fields_array_ref = crate::ObjectRef::new(201, 0);
        let field_ref = crate::ObjectRef::new(202, 0);

        // /AcroForm /Fields is an indirect Reference to an Array.
        let mut acroform = Dictionary::new();
        acroform.insert("Fields", Object::Reference(fields_array_ref));
        patch_acroform(&mut pdf, acroform_ref, acroform);

        // The indirect /Fields target: an Array of references.
        pdf.set_object(
            fields_array_ref,
            Object::Array(vec![Object::Reference(field_ref)]),
        );

        // One leaf field.
        let mut field = Dictionary::new();
        field.insert("T", Object::String(b"name".to_vec()));
        field.insert("FT", Object::Name(b"Tx".to_vec()));
        pdf.set_object(field_ref, Object::Dictionary(field));

        let result = build_acroform_section(&mut pdf).expect("build_acroform_section failed");
        let JsonValue::Object(top) = &result else {
            panic!("expected Object");
        };
        let JsonValue::Array(fields) = &top[0].1 else {
            panic!("fields must be Array");
        };
        assert_eq!(
            fields.len(),
            1,
            "indirect /Fields array must be resolved, not dropped"
        );
        let JsonValue::Object(entry) = &fields[0] else {
            panic!("entry must be Object");
        };
        let fullname = entry
            .iter()
            .find(|(k, _)| k == "fullname")
            .map(|(_, v)| v.clone())
            .unwrap();
        assert_eq!(fullname, JsonValue::String("name".to_string()));
    }

    #[test]
    fn acroform_indirect_kids_array_is_resolved() {
        let mut pdf = load_one_page_pdf();

        let acroform_ref = crate::ObjectRef::new(200, 0);
        let parent_ref = crate::ObjectRef::new(201, 0);
        let kids_array_ref = crate::ObjectRef::new(202, 0);
        let child_ref = crate::ObjectRef::new(203, 0);

        let mut acroform = Dictionary::new();
        acroform.insert("Fields", Object::Array(vec![Object::Reference(parent_ref)]));
        patch_acroform(&mut pdf, acroform_ref, acroform);

        // Parent field with /Kids as an indirect Reference (not a direct Array).
        let mut parent_field = Dictionary::new();
        parent_field.insert("T", Object::String(b"group".to_vec()));
        parent_field.insert("FT", Object::Name(b"Tx".to_vec()));
        parent_field.insert("Kids", Object::Reference(kids_array_ref));
        pdf.set_object(parent_ref, Object::Dictionary(parent_field));

        pdf.set_object(
            kids_array_ref,
            Object::Array(vec![Object::Reference(child_ref)]),
        );

        let mut child = Dictionary::new();
        child.insert("T", Object::String(b"name".to_vec()));
        child.insert("Parent", Object::Reference(parent_ref));
        pdf.set_object(child_ref, Object::Dictionary(child));

        let result = build_acroform_section(&mut pdf).expect("build_acroform_section failed");
        let JsonValue::Object(top) = &result else {
            panic!("expected Object");
        };
        let JsonValue::Array(fields) = &top[0].1 else {
            panic!("fields must be Array");
        };
        // parent + child must both show up; the indirect /Kids array must
        // have been resolved.
        assert_eq!(
            fields.len(),
            2,
            "indirect /Kids array must be resolved so descendants are emitted"
        );
        let JsonValue::Object(child_entry) = &fields[1] else {
            panic!("child entry must be Object");
        };
        let child_fullname = child_entry
            .iter()
            .find(|(k, _)| k == "fullname")
            .map(|(_, v)| v.clone())
            .unwrap();
        assert_eq!(child_fullname, JsonValue::String("group.name".to_string()));
    }

    // ══════════════════════════════════════════════════════════════════════════
    // build_attachments_section tests
    // ══════════════════════════════════════════════════════════════════════════

    fn load_attachment_pdf() -> crate::Pdf<std::io::Cursor<Vec<u8>>> {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let fixture = manifest.join("../../tests/fixtures/compat/attachment-two-page.pdf");
        let bytes = std::fs::read(&fixture).unwrap_or_else(|e| {
            panic!(
                "attachment-two-page.pdf not found at {}: {e}",
                fixture.display()
            )
        });
        crate::Pdf::open_mem_owned(bytes).expect("failed to open attachment-two-page.pdf")
    }

    /// Insert a /Names/EmbeddedFiles name-tree with one entry into an existing PDF.
    fn patch_embedded_files(
        pdf: &mut crate::Pdf<std::io::Cursor<Vec<u8>>>,
        names_ref: crate::ObjectRef,
        ef_root_ref: crate::ObjectRef,
        filespec_ref: crate::ObjectRef,
        filespec: Dictionary,
        name: &[u8],
    ) {
        // Build the name tree leaf: /Names [name filespec_ref]
        let mut ef_root = Dictionary::new();
        ef_root.insert(
            "Names",
            Object::Array(vec![
                Object::String(name.to_vec()),
                Object::Reference(filespec_ref),
            ]),
        );
        pdf.set_object(ef_root_ref, Object::Dictionary(ef_root));

        // Build the /Names dict with /EmbeddedFiles
        let mut names_dict = Dictionary::new();
        names_dict.insert("EmbeddedFiles", Object::Reference(ef_root_ref));
        pdf.set_object(names_ref, Object::Dictionary(names_dict));

        // Patch the catalog
        let catalog_ref = pdf.root_ref().expect("no /Root");
        let mut catalog = match pdf.resolve_borrowed(catalog_ref).expect("resolve catalog") {
            Object::Dictionary(d) => d.clone(),
            _ => panic!("catalog is not a Dictionary"),
        };
        catalog.insert("Names", Object::Reference(names_ref));
        pdf.set_object(catalog_ref, Object::Dictionary(catalog));

        pdf.set_object(filespec_ref, Object::Dictionary(filespec));
    }

    // ── attachments Test 1: No /Names/EmbeddedFiles → empty object ───────────

    #[test]
    fn attachments_no_embedded_files_returns_empty() {
        let mut pdf = load_one_page_pdf();
        let result = build_attachments_section(&mut pdf).expect("build_attachments_section failed");
        assert_eq!(result, JsonValue::Object(vec![]), "expected empty object");
    }

    // ── attachments Test 1b: /Names present but no /EmbeddedFiles → empty ─────

    #[test]
    fn attachments_names_without_embedded_files_returns_empty() {
        // Covers the `None => return empty` branch when /Names exists but
        // carries no /EmbeddedFiles key.
        let mut pdf = load_one_page_pdf();
        let catalog_ref = pdf.root_ref().expect("no /Root");
        let mut catalog = pdf
            .resolve_borrowed(catalog_ref)
            .expect("resolve catalog")
            .as_dict()
            .expect("catalog dict")
            .clone();
        let mut names = Dictionary::new();
        names.insert("Dests", Object::Dictionary(Dictionary::new()));
        catalog.insert("Names", Object::Dictionary(names));
        pdf.set_object(catalog_ref, Object::Dictionary(catalog));

        let result = build_attachments_section(&mut pdf).expect("build_attachments_section failed");
        assert_eq!(result, JsonValue::Object(vec![]), "expected empty object");
    }

    // ── attachments Test 1c: non-ref/non-dict leaf value is skipped ──────────

    #[test]
    fn attachments_non_ref_non_dict_value_skipped() {
        // A name-tree leaf value that is neither a reference nor a dict is
        // skipped (covers the `_ => None` arm of the attachments decode hook).
        let mut pdf = load_one_page_pdf();
        let ef_root_ref = crate::ObjectRef::new(901, 0);
        let mut ef_root = Dictionary::new();
        ef_root.insert(
            "Names",
            Object::Array(vec![Object::String(b"weird".to_vec()), Object::Integer(7)]),
        );
        pdf.set_object(ef_root_ref, Object::Dictionary(ef_root));

        let names_ref = crate::ObjectRef::new(902, 0);
        let mut names = Dictionary::new();
        names.insert("EmbeddedFiles", Object::Reference(ef_root_ref));
        pdf.set_object(names_ref, Object::Dictionary(names));

        let catalog_ref = pdf.root_ref().expect("no /Root");
        let mut catalog = pdf
            .resolve_borrowed(catalog_ref)
            .expect("resolve catalog")
            .as_dict()
            .expect("catalog dict")
            .clone();
        catalog.insert("Names", Object::Reference(names_ref));
        pdf.set_object(catalog_ref, Object::Dictionary(catalog));

        let result = build_attachments_section(&mut pdf).expect("build_attachments_section failed");
        assert_eq!(
            result,
            JsonValue::Object(vec![]),
            "non-ref/non-dict leaf value must be skipped"
        );
    }

    // ── attachments Test 2: attachment-two-page.pdf → 1 entry ────────────────

    #[test]
    fn attachments_fixture_has_one_entry() {
        let mut pdf = load_attachment_pdf();
        let result = build_attachments_section(&mut pdf).expect("build_attachments_section failed");
        let JsonValue::Object(pairs) = &result else {
            panic!("expected Object, got {result:?}");
        };
        assert_eq!(
            pairs.len(),
            1,
            "attachment-two-page.pdf must have exactly 1 attachment"
        );
        assert_eq!(
            pairs[0].0, "attachment.txt",
            "attachment name must be 'attachment.txt'"
        );
    }

    // ── attachments Test 3: fixture entry filespec, preferredname, streams keys

    #[test]
    fn attachments_fixture_entry_fields() {
        let mut pdf = load_attachment_pdf();
        let result = build_attachments_section(&mut pdf).expect("build_attachments_section failed");
        let JsonValue::Object(pairs) = &result else {
            panic!("expected Object");
        };
        let JsonValue::Object(entry) = &pairs[0].1 else {
            panic!("entry must be Object");
        };

        let get = |k: &str| -> &JsonValue {
            entry
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v)
                .unwrap_or_else(|| panic!("key '{k}' not found in entry"))
        };

        // Keys must be present
        let keys: Vec<&str> = entry.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "description",
                "filespec",
                "names",
                "preferredcontents",
                "preferredname",
                "streams"
            ],
            "entry keys must be in alphabetical order"
        );

        // description: null (no /Desc in fixture)
        assert_eq!(
            *get("description"),
            JsonValue::Null,
            "description must be null"
        );

        // filespec: must be a ref string
        let JsonValue::String(filespec_str) = get("filespec") else {
            panic!("filespec must be a String");
        };
        assert!(
            filespec_str.ends_with(" R"),
            "filespec must be a ref string like 'N M R', got: {filespec_str}"
        );

        // preferredname: "attachment.txt"
        assert_eq!(
            *get("preferredname"),
            JsonValue::String("attachment.txt".to_string()),
            "preferredname must be 'attachment.txt'"
        );

        // streams: must be an Object with at least one stream entry
        let JsonValue::Object(streams) = get("streams") else {
            panic!("streams must be Object");
        };
        assert!(!streams.is_empty(), "streams must not be empty");

        // Each stream entry must have checksum, creationdate, mimetype, modificationdate
        for (stream_key, stream_val) in streams {
            let JsonValue::Object(stream_entry) = stream_val else {
                panic!("stream entry for {stream_key} must be Object");
            };
            let stream_keys: Vec<&str> = stream_entry.iter().map(|(k, _)| k.as_str()).collect();
            assert_eq!(
                stream_keys,
                vec!["checksum", "creationdate", "mimetype", "modificationdate"],
                "stream entry for {stream_key} must have 4 keys in alphabetical order"
            );
        }

        // ── qpdf-parity value assertions (matching qpdf --json output) ───────
        // names dict: /F and /UF both map to "attachment.txt"
        let JsonValue::Object(names) = get("names") else {
            panic!("names must be Object");
        };
        let name_keys: Vec<&str> = names.iter().map(|(k, _)| k.as_str()).collect();
        assert!(
            name_keys.contains(&"/F") || name_keys.contains(&"/UF"),
            "names must contain at least /F or /UF"
        );
        for (_key, val) in names {
            assert_eq!(
                *val,
                JsonValue::String("attachment.txt".to_string()),
                "each name entry must be 'attachment.txt'"
            );
        }

        // preferredcontents must be a valid ref string (not null)
        let JsonValue::String(preferred_contents_str) = get("preferredcontents") else {
            panic!("preferredcontents must be a String ref");
        };
        assert!(
            preferred_contents_str.ends_with(" R"),
            "preferredcontents must be a ref string, got: {preferred_contents_str}"
        );

        // Value-level parity with qpdf output for each stream entry:
        // checksum: 542266a1f565c3e5d8cfbd55eb7dfa40
        // creationdate: 2026-01-01T00:00:00Z
        // modificationdate: 2026-01-01T00:00:00Z
        // mimetype: null (no /Subtype in fixture)
        let JsonValue::Object(streams2) = get("streams") else {
            panic!("streams must be Object");
        };
        for (stream_key, stream_val) in streams2 {
            let JsonValue::Object(stream_entry) = stream_val else {
                panic!("stream entry for {stream_key} must be Object");
            };
            let s_get = |k: &str| -> &JsonValue {
                stream_entry
                    .iter()
                    .find(|(key, _)| key == k)
                    .map(|(_, v)| v)
                    .unwrap_or_else(|| panic!("key '{k}' not found in stream {stream_key}"))
            };
            assert_eq!(
                *s_get("checksum"),
                JsonValue::String("542266a1f565c3e5d8cfbd55eb7dfa40".to_string()),
                "checksum mismatch for stream {stream_key}"
            );
            assert_eq!(
                *s_get("creationdate"),
                JsonValue::String("2026-01-01T00:00:00Z".to_string()),
                "creationdate mismatch for stream {stream_key}"
            );
            assert_eq!(
                *s_get("modificationdate"),
                JsonValue::String("2026-01-01T00:00:00Z".to_string()),
                "modificationdate mismatch for stream {stream_key}"
            );
            assert_eq!(
                *s_get("mimetype"),
                JsonValue::Null,
                "mimetype must be null for stream {stream_key} (no /Subtype in fixture)"
            );
        }
    }

    // ── attachments Test 4: synthetic fixture — key order, priorities, values ──

    #[test]
    fn attachments_synthetic_key_order_and_priorities() {
        let mut pdf = load_one_page_pdf();

        // Refs for the embedded file stream
        let stream_f_ref = crate::ObjectRef::new(300, 0);
        let stream_uf_ref = crate::ObjectRef::new(301, 0);
        let filespec_ref = crate::ObjectRef::new(302, 0);
        let ef_root_ref = crate::ObjectRef::new(303, 0);
        let names_ref = crate::ObjectRef::new(304, 0);

        // Build the /EF/F stream with /Params
        let mut f_params = Dictionary::new();
        // 16 bytes for checksum
        let checksum_bytes: Vec<u8> = (0u8..16).collect();
        f_params.insert("CheckSum", Object::String(checksum_bytes.clone()));
        f_params.insert(
            "CreationDate",
            Object::String(b"D:20260101000000Z".to_vec()),
        );
        f_params.insert(
            "ModDate",
            Object::String(b"D:20260202120000+09'00'".to_vec()),
        );
        let mut stream_f_dict = Dictionary::new();
        stream_f_dict.insert("Type", Object::Name(b"EmbeddedFile".to_vec()));
        stream_f_dict.insert("Subtype", Object::Name(b"text/plain".to_vec()));
        stream_f_dict.insert("Params", Object::Dictionary(f_params));
        pdf.set_object(
            stream_f_ref,
            Object::Stream(crate::object::Stream::new(stream_f_dict, vec![])),
        );

        // /EF/UF stream (different stream, no /Subtype)
        let mut stream_uf_dict = Dictionary::new();
        stream_uf_dict.insert("Type", Object::Name(b"EmbeddedFile".to_vec()));
        pdf.set_object(
            stream_uf_ref,
            Object::Stream(crate::object::Stream::new(stream_uf_dict, vec![])),
        );

        // Build the /EF dict: both /F and /UF
        let mut ef_dict = Dictionary::new();
        ef_dict.insert("F", Object::Reference(stream_f_ref));
        ef_dict.insert("UF", Object::Reference(stream_uf_ref));

        // Build filespec dict
        let mut filespec = Dictionary::new();
        filespec.insert("Type", Object::Name(b"Filespec".to_vec()));
        filespec.insert("F", Object::String(b"f-name.txt".to_vec()));
        filespec.insert("UF", Object::String(b"uf-name.txt".to_vec()));
        filespec.insert("Desc", Object::String(b"My file description".to_vec()));
        filespec.insert("EF", Object::Dictionary(ef_dict));

        patch_embedded_files(
            &mut pdf,
            names_ref,
            ef_root_ref,
            filespec_ref,
            filespec,
            b"my-attachment.txt",
        );

        let result = build_attachments_section(&mut pdf).expect("build_attachments_section failed");
        let JsonValue::Object(pairs) = &result else {
            panic!("expected Object");
        };

        assert_eq!(pairs.len(), 1, "expected 1 attachment");
        assert_eq!(pairs[0].0, "my-attachment.txt", "name mismatch");

        let JsonValue::Object(entry) = &pairs[0].1 else {
            panic!("entry must be Object");
        };

        let get = |k: &str| -> &JsonValue {
            entry
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v)
                .unwrap_or_else(|| panic!("key '{k}' not found"))
        };

        // description: bare string (no u: prefix)
        assert_eq!(
            *get("description"),
            JsonValue::String("My file description".to_string()),
            "description must be bare string"
        );

        // names: /F and /UF both present
        let JsonValue::Object(names) = get("names") else {
            panic!("names must be Object");
        };
        let name_keys: Vec<&str> = names.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            name_keys,
            vec!["/F", "/UF"],
            "names keys must be /F, /UF in order"
        );
        assert_eq!(
            names[0].1,
            JsonValue::String("f-name.txt".to_string()),
            "/F name mismatch"
        );
        assert_eq!(
            names[1].1,
            JsonValue::String("uf-name.txt".to_string()),
            "/UF name mismatch"
        );

        // preferredname: /UF wins over /F
        assert_eq!(
            *get("preferredname"),
            JsonValue::String("uf-name.txt".to_string()),
            "preferredname must be /UF (uf-name.txt)"
        );

        // preferredcontents: /EF/UF wins over /EF/F
        assert_eq!(
            *get("preferredcontents"),
            JsonValue::String(format!(
                "{} {} R",
                stream_uf_ref.number, stream_uf_ref.generation
            )),
            "preferredcontents must be /EF/UF ref"
        );

        // streams: /F and /UF both present
        let JsonValue::Object(streams) = get("streams") else {
            panic!("streams must be Object");
        };
        let stream_keys: Vec<&str> = streams.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            stream_keys,
            vec!["/F", "/UF"],
            "streams keys must be /F, /UF in order"
        );

        // /F stream: check checksum hex, dates, mimetype
        let JsonValue::Object(f_stream) = &streams[0].1 else {
            panic!("/F stream entry must be Object");
        };
        let f_get = |k: &str| -> &JsonValue {
            f_stream
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v)
                .unwrap_or_else(|| panic!("key '{k}' not found in /F stream"))
        };

        // checksum: 16 bytes 0x00..0x0f → lowercase hex
        let expected_hex: String = (0u8..16).map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            *f_get("checksum"),
            JsonValue::String(expected_hex),
            "checksum must be lowercase hex"
        );

        // creationdate: D:20260101000000Z → 2026-01-01T00:00:00Z
        assert_eq!(
            *f_get("creationdate"),
            JsonValue::String("2026-01-01T00:00:00Z".to_string()),
            "creationdate mismatch"
        );

        // modificationdate: D:20260202120000+09'00' → 2026-02-02T12:00:00+09:00
        assert_eq!(
            *f_get("modificationdate"),
            JsonValue::String("2026-02-02T12:00:00+09:00".to_string()),
            "modificationdate mismatch"
        );

        // mimetype: bare "text/plain" (no "/" prefix, no "u:" prefix)
        assert_eq!(
            *f_get("mimetype"),
            JsonValue::String("text/plain".to_string()),
            "mimetype must be bare 'text/plain'"
        );

        // /UF stream: no /Subtype → mimetype null, no /Params → other fields null
        let JsonValue::Object(uf_stream) = &streams[1].1 else {
            panic!("/UF stream entry must be Object");
        };
        let uf_get = |k: &str| -> &JsonValue {
            uf_stream
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v)
                .unwrap_or_else(|| panic!("key '{k}' not found in /UF stream"))
        };
        assert_eq!(
            *uf_get("mimetype"),
            JsonValue::Null,
            "/UF mimetype must be null"
        );
        assert_eq!(
            *uf_get("checksum"),
            JsonValue::Null,
            "/UF checksum must be null"
        );
        assert_eq!(
            *uf_get("creationdate"),
            JsonValue::Null,
            "/UF creationdate must be null"
        );
        assert_eq!(
            *uf_get("modificationdate"),
            JsonValue::Null,
            "/UF modificationdate must be null"
        );
    }

    // ── attachments: direct (non-Reference) filespec value in the name tree ──
    //
    // Regression for CodeRabbit's flpdf-9hc.11.8 review: previously the name
    // tree walker only accepted Object::Reference as the leaf value, silently
    // dropping inline (direct) filespec dictionaries. They must produce an
    // entry too — the only difference is `filespec` becomes null because
    // there is no object reference to point at.

    #[test]
    fn attachments_direct_inline_filespec_dictionary_is_serialized() {
        let mut pdf = load_one_page_pdf();

        // Build an inline (direct) filespec dictionary and place it directly
        // into the /Names array — no indirect reference layer.
        let mut filespec = Dictionary::new();
        filespec.insert("F", Object::String(b"inline.txt".to_vec()));
        filespec.insert("UF", Object::String(b"inline.txt".to_vec()));

        let mut ef_root = Dictionary::new();
        ef_root.insert(
            "Names",
            Object::Array(vec![
                Object::String(b"inline.txt".to_vec()),
                Object::Dictionary(filespec), // <-- direct, not Reference
            ]),
        );
        let ef_root_ref = crate::ObjectRef::new(300, 0);
        pdf.set_object(ef_root_ref, Object::Dictionary(ef_root));

        let mut names_dict = Dictionary::new();
        names_dict.insert("EmbeddedFiles", Object::Reference(ef_root_ref));
        let names_ref = crate::ObjectRef::new(301, 0);
        pdf.set_object(names_ref, Object::Dictionary(names_dict));

        let catalog_ref = pdf.root_ref().unwrap();
        let mut catalog = match pdf.resolve_borrowed(catalog_ref).unwrap() {
            Object::Dictionary(d) => d.clone(),
            _ => panic!(),
        };
        catalog.insert("Names", Object::Reference(names_ref));
        pdf.set_object(catalog_ref, Object::Dictionary(catalog));

        let result = build_attachments_section(&mut pdf).expect("build_attachments_section failed");
        let JsonValue::Object(pairs) = &result else {
            panic!("expected Object");
        };

        // The inline filespec must produce an entry, not be silently dropped.
        assert_eq!(
            pairs.len(),
            1,
            "direct inline filespec must yield an attachments entry"
        );
        assert_eq!(pairs[0].0, "inline.txt");

        let JsonValue::Object(entry) = &pairs[0].1 else {
            panic!("entry must be Object");
        };
        let get = |k: &str| -> &JsonValue {
            entry
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v)
                .unwrap_or_else(|| panic!("key '{k}' missing"))
        };
        // No indirect reference → filespec must be null.
        assert_eq!(
            *get("filespec"),
            JsonValue::Null,
            "filespec must be null when the leaf value was a direct dictionary"
        );
        // The names sub-object still surfaces the inlined /F and /UF.
        let JsonValue::Object(names) = get("names") else {
            panic!("names must be Object");
        };
        assert_eq!(names.len(), 2);
        assert_eq!(names[0].0, "/F");
        assert_eq!(names[1].0, "/UF");
    }

    // ── parse_pdf_date unit tests ─────────────────────────────────────────────

    #[test]
    fn parse_pdf_date_utc_z() {
        assert_eq!(
            parse_pdf_date(b"D:20260101000000Z"),
            Some("2026-01-01T00:00:00Z".to_string())
        );
    }

    #[test]
    fn parse_pdf_date_plus_offset() {
        assert_eq!(
            parse_pdf_date(b"D:20260202120000+09'00'"),
            Some("2026-02-02T12:00:00+09:00".to_string())
        );
    }

    #[test]
    fn parse_pdf_date_no_tz() {
        // No timezone → Z
        assert_eq!(
            parse_pdf_date(b"D:20260101000000"),
            Some("2026-01-01T00:00:00Z".to_string())
        );
    }

    #[test]
    fn parse_pdf_date_year_only() {
        // Short date: only year
        assert_eq!(
            parse_pdf_date(b"D:2026"),
            Some("2026-01-01T00:00:00Z".to_string())
        );
    }

    #[test]
    fn parse_pdf_date_invalid_returns_none() {
        assert_eq!(parse_pdf_date(b"not-a-date"), None);
        assert_eq!(parse_pdf_date(b"D:"), None);
        assert_eq!(parse_pdf_date(b""), None);
    }

    #[test]
    fn parse_pdf_date_non_ascii_does_not_panic() {
        // Regression: previously `&s[4..6]` could slice into the middle of
        // a UTF-8 multibyte char (e.g. "é" is 0xC3 0xA9). The function now
        // rejects any non-ASCII bytes up front and returns None cleanly.
        // The test must NOT panic.
        assert_eq!(parse_pdf_date("D:20260é0101".as_bytes()), None);
        assert_eq!(parse_pdf_date("D:あいう".as_bytes()), None);
        // The non-ASCII content can appear anywhere — even after a valid
        // year prefix — and must still be rejected without panicking.
        assert_eq!(parse_pdf_date("D:2026あ".as_bytes()), None);
    }

    #[test]
    fn parse_pdf_date_non_digit_components_return_none() {
        // Components past the year that aren't digits must not blindly
        // pass through. Previously only the year was validated.
        assert_eq!(parse_pdf_date(b"D:2026XX0101000000Z"), None);
        assert_eq!(parse_pdf_date(b"D:20260101NN0000Z"), None);
    }

    #[test]
    fn parse_pdf_date_trailing_garbage_returns_none() {
        // Regression: garbage suffix after the seconds field used to default
        // to "Z" instead of failing. The function contract says unparseable
        // input -> None, so the silent default is wrong.
        assert_eq!(parse_pdf_date(b"D:20260101000000garbage"), None);
        assert_eq!(parse_pdf_date(b"D:20260101000000*"), None);
    }

    #[test]
    fn parse_pdf_date_malformed_tz_offset_returns_none() {
        // Half-formed offsets after + / - must also fail instead of falling
        // back to "Z".
        assert_eq!(parse_pdf_date(b"D:20260101000000+X"), None);
        assert_eq!(parse_pdf_date(b"D:20260101000000+0X"), None);
        assert_eq!(parse_pdf_date(b"D:20260101000000+0900XX"), None);
    }

    #[test]
    fn parse_pdf_date_partial_date_component_returns_none() {
        // Regression: previously `take` silently fell back to the default
        // whenever the input was shorter than the requested boundary, so
        // dangling partial digits ("D:20261" / "D:2026010" / "D:202601010")
        // produced a valid-looking timestamp. They must now return None.
        assert_eq!(parse_pdf_date(b"D:20261"), None);
        assert_eq!(parse_pdf_date(b"D:2026010"), None);
        assert_eq!(parse_pdf_date(b"D:202601010"), None);
        // The boundaries 4 / 6 / 8 / 10 / 12 / 14 themselves must still work.
        assert!(parse_pdf_date(b"D:2026").is_some());
        assert!(parse_pdf_date(b"D:202601").is_some());
        assert!(parse_pdf_date(b"D:20260101000000").is_some());
    }

    #[test]
    fn parse_pdf_date_out_of_range_components_return_none() {
        // Regression for CodeRabbit's range-validation finding. ISO 8601
        // parsers reject month > 12, day > 31, hour > 23, minute > 59,
        // second > 59. The PDF date parser must do the same so the function
        // never emits a malformed ISO timestamp.
        assert_eq!(parse_pdf_date(b"D:20261301000000Z"), None, "month 13");
        assert_eq!(parse_pdf_date(b"D:20260132000000Z"), None, "day 32");
        assert_eq!(parse_pdf_date(b"D:20260101240000Z"), None, "hour 24");
        assert_eq!(parse_pdf_date(b"D:20260101006000Z"), None, "minute 60");
        assert_eq!(parse_pdf_date(b"D:20260101000060Z"), None, "second 60");
        // Month 00 / day 00 are also rejected.
        assert_eq!(parse_pdf_date(b"D:20260001000000Z"), None, "month 00");
        assert_eq!(parse_pdf_date(b"D:20260100000000Z"), None, "day 00");
    }

    #[test]
    fn parse_pdf_date_out_of_range_tz_offset_returns_none() {
        // tz offsets above 23 hours or 59 minutes are not valid ISO 8601.
        assert_eq!(parse_pdf_date(b"D:20260101000000+99'00'"), None);
        assert_eq!(parse_pdf_date(b"D:20260101000000+09'99'"), None);
        assert_eq!(parse_pdf_date(b"D:20260101000000-2400"), None);
    }

    #[test]
    fn parse_pdf_date_multiple_trailing_apostrophes_in_offset_returns_none() {
        // Regression: trim_end_matches('\'') used to swallow any number of
        // trailing apostrophes, accepting "+09''", "+09'00'''" as if valid.
        // The parser now accepts only a single closing apostrophe (the
        // standard PDF date form `+HH'mm'`); anything else is rejected.
        assert_eq!(parse_pdf_date(b"D:20260101000000+09''"), None);
        assert_eq!(parse_pdf_date(b"D:20260101000000+09'00'''"), None);
        // The well-formed `+HH'mm'` still parses.
        assert_eq!(
            parse_pdf_date(b"D:20260101000000+09'00'"),
            Some("2026-01-01T00:00:00+09:00".to_string())
        );
    }

    #[test]
    fn checksum_to_hex_roundtrip() {
        let bytes: Vec<u8> = (0u8..16).collect();
        let hex = checksum_to_hex(&bytes);
        assert_eq!(hex, "000102030405060708090a0b0c0d0e0f");
        assert_eq!(hex.len(), 32);
    }

    // ══════════════════════════════════════════════════════════════════════════
    // build_encrypt_section tests (flpdf-9hc.11.9)
    // ══════════════════════════════════════════════════════════════════════════

    /// Helper: load the encrypted-r4-three-page.pdf fixture.
    fn load_encrypted_r4_pdf() -> crate::Pdf<std::io::Cursor<Vec<u8>>> {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let fixture = manifest.join("../../tests/fixtures/compat/encrypted-r4-three-page.pdf");
        let bytes = std::fs::read(&fixture).unwrap_or_else(|e| {
            panic!(
                "encrypted-r4-three-page.pdf not found at {}: {e}",
                fixture.display()
            )
        });
        // Empty password, AESv2 is not weak-crypto, so default options work.
        crate::Pdf::open_mem_owned(bytes).expect("failed to open encrypted-r4-three-page.pdf")
    }

    // ── Test 1: plaintext PDF → encrypted=false, capabilities all-true, params 0/"none" ──

    #[test]
    fn encrypt_section_plaintext_encrypted_false() {
        let mut pdf = load_one_page_pdf();
        let enc = build_encrypt_section(&mut pdf).expect("build_encrypt_section failed");
        let JsonValue::Object(ref pairs) = enc else {
            panic!("encrypt section must be an Object");
        };
        let encrypted = pairs
            .iter()
            .find(|(k, _)| k == "encrypted")
            .unwrap()
            .1
            .clone();
        assert_eq!(encrypted, JsonValue::Bool(false));
    }

    #[test]
    fn encrypt_section_plaintext_ownerpasswordmatched_false() {
        let mut pdf = load_one_page_pdf();
        let enc = build_encrypt_section(&mut pdf).expect("build_encrypt_section failed");
        let JsonValue::Object(ref pairs) = enc else {
            panic!("not an Object")
        };
        let v = pairs
            .iter()
            .find(|(k, _)| k == "ownerpasswordmatched")
            .unwrap()
            .1
            .clone();
        assert_eq!(v, JsonValue::Bool(false));
        let v2 = pairs
            .iter()
            .find(|(k, _)| k == "userpasswordmatched")
            .unwrap()
            .1
            .clone();
        assert_eq!(v2, JsonValue::Bool(false));
    }

    #[test]
    fn encrypt_section_plaintext_parameters_are_zero_and_none() {
        let mut pdf = load_one_page_pdf();
        let enc = build_encrypt_section(&mut pdf).expect("build_encrypt_section failed");
        let JsonValue::Object(ref pairs) = enc else {
            panic!("not an Object")
        };
        let params = pairs
            .iter()
            .find(|(k, _)| k == "parameters")
            .unwrap()
            .1
            .clone();
        let JsonValue::Object(ref p) = params else {
            panic!("parameters must be Object")
        };

        let get = |k: &str| p.iter().find(|(ky, _)| ky == k).unwrap().1.clone();
        assert_eq!(get("P"), JsonValue::Integer(0));
        assert_eq!(get("R"), JsonValue::Integer(0));
        assert_eq!(get("V"), JsonValue::Integer(0));
        assert_eq!(get("bits"), JsonValue::Integer(0));
        assert_eq!(get("filemethod"), JsonValue::String("none".into()));
        assert_eq!(get("method"), JsonValue::String("none".into()));
        assert_eq!(get("streammethod"), JsonValue::String("none".into()));
        assert_eq!(get("stringmethod"), JsonValue::String("none".into()));
        assert_eq!(get("key"), JsonValue::Null);
    }

    #[test]
    fn encrypt_section_plaintext_capabilities_all_true() {
        let mut pdf = load_one_page_pdf();
        let enc = build_encrypt_section(&mut pdf).expect("build_encrypt_section failed");
        let JsonValue::Object(ref pairs) = enc else {
            panic!("not Object")
        };
        let caps = pairs
            .iter()
            .find(|(k, _)| k == "capabilities")
            .unwrap()
            .1
            .clone();
        let JsonValue::Object(ref cp) = caps else {
            panic!("capabilities must be Object")
        };
        for (_, v) in cp.iter() {
            assert_eq!(
                *v,
                JsonValue::Bool(true),
                "all plaintext capabilities must be true"
            );
        }
    }

    // ── Test 2: encrypted-r4 → encrypted=true, R=4, V=4, bits=128, methods AESv2 ──

    #[test]
    fn encrypt_section_encrypted_r4_encrypted_true() {
        let mut pdf = load_encrypted_r4_pdf();
        let enc = build_encrypt_section(&mut pdf).expect("build_encrypt_section failed");
        let JsonValue::Object(ref pairs) = enc else {
            panic!("not Object")
        };
        let v = pairs
            .iter()
            .find(|(k, _)| k == "encrypted")
            .unwrap()
            .1
            .clone();
        assert_eq!(v, JsonValue::Bool(true));
    }

    // Regression for CodeRabbit's flpdf-9hc.11.9 review: previously both
    // owner and user password matched flags were derived from is_encrypted,
    // so encrypted files that only authenticated as user would falsely
    // report owner=true. The reader now tracks each independently, and
    // build_encrypt_section reads them through the new accessors.

    #[test]
    fn encrypt_section_plaintext_password_match_flags_are_both_false() {
        let mut pdf = load_one_page_pdf();
        let enc = build_encrypt_section(&mut pdf).expect("build_encrypt_section failed");
        let JsonValue::Object(ref pairs) = enc else {
            panic!("not Object")
        };
        let owner = pairs
            .iter()
            .find(|(k, _)| k == "ownerpasswordmatched")
            .unwrap()
            .1
            .clone();
        let user = pairs
            .iter()
            .find(|(k, _)| k == "userpasswordmatched")
            .unwrap()
            .1
            .clone();
        assert_eq!(owner, JsonValue::Bool(false));
        assert_eq!(user, JsonValue::Bool(false));
    }

    #[test]
    fn encrypt_section_pdf_accessor_independent_of_is_encrypted() {
        // The Pdf::owner_password_matched / user_password_matched accessors
        // must come from the authentication record, not be derived from
        // is_encrypted. For plaintext PDFs both must be false.
        let pdf = load_one_page_pdf();
        assert!(!pdf.is_encrypted());
        assert!(!pdf.owner_password_matched());
        assert!(!pdf.user_password_matched());
    }

    // Regression for CodeRabbit's PR #115 review: cf_method_string used to
    // return "none" whenever /CF was missing or the selected filter had no
    // /CFM, which silently disguised RC4/AESv2 documents as plaintext. It
    // now falls back to the same revision-based default the reader uses.

    #[test]
    fn cf_method_string_defaults_to_aesv2_for_revision_4() {
        let mut encrypt = Dictionary::new();
        encrypt.insert("R", Object::Integer(4));
        // No /CF at all.
        assert_eq!(cf_method_string(&encrypt, Some("StdCF")), "AESv2");
        // /CF exists but the selector is missing.
        let mut cf = Dictionary::new();
        cf.insert("OtherCF", Object::Dictionary(Dictionary::new()));
        encrypt.insert("CF", Object::Dictionary(cf));
        assert_eq!(cf_method_string(&encrypt, Some("StdCF")), "AESv2");
        // Selector found but its /CFM is missing.
        let mut encrypt2 = Dictionary::new();
        encrypt2.insert("R", Object::Integer(4));
        let mut cf2 = Dictionary::new();
        cf2.insert("StdCF", Object::Dictionary(Dictionary::new()));
        encrypt2.insert("CF", Object::Dictionary(cf2));
        assert_eq!(cf_method_string(&encrypt2, Some("StdCF")), "AESv2");
    }

    #[test]
    fn cf_method_string_defaults_to_aesv3_for_revision_5_and_6() {
        for r in [5i64, 6] {
            let mut encrypt = Dictionary::new();
            encrypt.insert("R", Object::Integer(r));
            assert_eq!(cf_method_string(&encrypt, Some("StdCF")), "AESv3");
            assert_eq!(cf_method_string(&encrypt, None), "AESv3");
        }
    }

    #[test]
    fn cf_method_string_defaults_to_rc4_for_legacy_revisions() {
        let mut encrypt = Dictionary::new();
        encrypt.insert("R", Object::Integer(3));
        assert_eq!(cf_method_string(&encrypt, Some("StdCF")), "RC4");
        // No /R at all -> legacy default too.
        let empty = Dictionary::new();
        assert_eq!(cf_method_string(&empty, Some("StdCF")), "RC4");
    }

    #[test]
    fn cf_method_string_identity_selector_still_returns_none() {
        // The "Identity" selector explicitly means no encryption for that
        // path and must keep its "none" behavior regardless of /R.
        let mut encrypt = Dictionary::new();
        encrypt.insert("R", Object::Integer(4));
        assert_eq!(cf_method_string(&encrypt, Some("Identity")), "none");
    }

    #[test]
    fn encrypt_section_encrypted_r4_pdf_accessors_both_true_for_empty_password() {
        // For the bundled R4 fixture, the empty password authenticates as
        // both user and owner, so both accessors should be true (qpdf does
        // the same).
        let pdf = load_encrypted_r4_pdf();
        assert!(pdf.is_encrypted());
        assert!(pdf.owner_password_matched());
        assert!(pdf.user_password_matched());
    }

    #[test]
    fn encrypt_section_encrypted_r4_ownerpasswordmatched_true() {
        let mut pdf = load_encrypted_r4_pdf();
        let enc = build_encrypt_section(&mut pdf).expect("build_encrypt_section failed");
        let JsonValue::Object(ref pairs) = enc else {
            panic!("not Object")
        };
        let v = pairs
            .iter()
            .find(|(k, _)| k == "ownerpasswordmatched")
            .unwrap()
            .1
            .clone();
        assert_eq!(v, JsonValue::Bool(true));
        let v2 = pairs
            .iter()
            .find(|(k, _)| k == "userpasswordmatched")
            .unwrap()
            .1
            .clone();
        assert_eq!(v2, JsonValue::Bool(true));
    }

    #[test]
    fn encrypt_section_encrypted_r4_parameters() {
        let mut pdf = load_encrypted_r4_pdf();
        let enc = build_encrypt_section(&mut pdf).expect("build_encrypt_section failed");
        let JsonValue::Object(ref pairs) = enc else {
            panic!("not Object")
        };
        let params = pairs
            .iter()
            .find(|(k, _)| k == "parameters")
            .unwrap()
            .1
            .clone();
        let JsonValue::Object(ref p) = params else {
            panic!("parameters must be Object")
        };

        let get = |k: &str| p.iter().find(|(ky, _)| ky == k).unwrap().1.clone();
        assert_eq!(get("P"), JsonValue::Integer(-4));
        assert_eq!(get("R"), JsonValue::Integer(4));
        assert_eq!(get("V"), JsonValue::Integer(4));
        assert_eq!(get("bits"), JsonValue::Integer(128));
        assert_eq!(get("filemethod"), JsonValue::String("AESv2".into()));
        assert_eq!(get("method"), JsonValue::String("AESv2".into()));
        assert_eq!(get("streammethod"), JsonValue::String("AESv2".into()));
        assert_eq!(get("stringmethod"), JsonValue::String("AESv2".into()));
        assert_eq!(get("key"), JsonValue::Null);
    }

    #[test]
    fn encrypt_section_encrypted_r4_capabilities_all_true() {
        // /P = -4 = 0xFFFFFFFC → all permission bits set → all capabilities true
        let mut pdf = load_encrypted_r4_pdf();
        let enc = build_encrypt_section(&mut pdf).expect("build_encrypt_section failed");
        let JsonValue::Object(ref pairs) = enc else {
            panic!("not Object")
        };
        let caps = pairs
            .iter()
            .find(|(k, _)| k == "capabilities")
            .unwrap()
            .1
            .clone();
        let JsonValue::Object(ref cp) = caps else {
            panic!("capabilities must be Object")
        };
        for (name, v) in cp.iter() {
            assert_eq!(
                *v,
                JsonValue::Bool(true),
                "capability {name} must be true for P=-4"
            );
        }
    }

    // ── Test 3: capabilities key order is alphabetical ─────────────────────────

    #[test]
    fn encrypt_section_capabilities_key_order() {
        let mut pdf = load_one_page_pdf();
        let enc = build_encrypt_section(&mut pdf).expect("build_encrypt_section failed");
        let JsonValue::Object(ref pairs) = enc else {
            panic!("not Object")
        };
        let caps = pairs
            .iter()
            .find(|(k, _)| k == "capabilities")
            .unwrap()
            .1
            .clone();
        let JsonValue::Object(ref cp) = caps else {
            panic!("capabilities must be Object")
        };
        let keys: Vec<&str> = cp.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "accessibility",
                "extract",
                "modify",
                "modifyannotations",
                "modifyassembly",
                "modifyforms",
                "modifyother",
                "printhigh",
                "printlow",
            ]
        );
    }

    // ── Test 4: parameters key order is alphabetical ───────────────────────────

    #[test]
    fn encrypt_section_parameters_key_order() {
        let mut pdf = load_one_page_pdf();
        let enc = build_encrypt_section(&mut pdf).expect("build_encrypt_section failed");
        let JsonValue::Object(ref pairs) = enc else {
            panic!("not Object")
        };
        let params = pairs
            .iter()
            .find(|(k, _)| k == "parameters")
            .unwrap()
            .1
            .clone();
        let JsonValue::Object(ref p) = params else {
            panic!("parameters must be Object")
        };
        let keys: Vec<&str> = p.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "P",
                "R",
                "V",
                "bits",
                "filemethod",
                "key",
                "method",
                "streammethod",
                "stringmethod"
            ]
        );
    }

    // ── Test 5: top-level encrypt object key order is alphabetical ─────────────

    #[test]
    fn encrypt_section_top_level_key_order() {
        let mut pdf = load_one_page_pdf();
        let enc = build_encrypt_section(&mut pdf).expect("build_encrypt_section failed");
        let JsonValue::Object(ref pairs) = enc else {
            panic!("not Object")
        };
        let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "capabilities",
                "encrypted",
                "ownerpasswordmatched",
                "parameters",
                "recovereduserpassword",
                "userpasswordmatched",
            ]
        );
    }

    // ── Test 6: recovereduserpassword is always null ───────────────────────────

    #[test]
    fn encrypt_section_recovereduserpassword_always_null() {
        let mut pdf_plain = load_one_page_pdf();
        let enc_plain = build_encrypt_section(&mut pdf_plain).expect("plain failed");
        let JsonValue::Object(ref p) = enc_plain else {
            panic!("not Object") // cov:ignore: test-shape guard
        };
        let v = p
            .iter()
            .find(|(k, _)| k == "recovereduserpassword")
            .unwrap()
            .1
            .clone();
        assert_eq!(v, JsonValue::Null);

        let mut pdf_enc = load_encrypted_r4_pdf();
        let enc_enc = build_encrypt_section(&mut pdf_enc).expect("encrypted failed");
        let JsonValue::Object(ref pe) = enc_enc else {
            panic!("not Object") // cov:ignore: test-shape guard
        };
        let ve = pe
            .iter()
            .find(|(k, _)| k == "recovereduserpassword")
            .unwrap()
            .1
            .clone();
        assert_eq!(ve, JsonValue::Null);
    }

    // ── Test 7: composite build_qpdf_json_v2 includes encrypt key ─────────────

    #[test]
    fn build_qpdf_json_v2_includes_encrypt_section() {
        let mut pdf = load_one_page_pdf();
        let v2 = build_qpdf_json_v2(&mut pdf, DecodeLevel::Generalized)
            .expect("build_qpdf_json_v2 failed");
        let JsonValue::Object(pairs) = v2 else {
            panic!("expected Object at top level")
        };
        let enc = pairs.iter().find(|(k, _)| k == "encrypt").map(|(_, v)| v);
        assert!(
            enc.is_some(),
            "encrypt key must be present in composite output"
        );
        assert!(
            matches!(enc.unwrap(), JsonValue::Object(_)),
            "encrypt must be an Object"
        );
    }

    // ══════════════════════════════════════════════════════════════════════════
    // flpdf-9hc.11.10: StreamDataMode tests
    // ══════════════════════════════════════════════════════════════════════════

    // Helper: extract the stream inner object for obj:7 0 R from the
    // build_qpdf_key_with_stream_mode result.
    fn get_obj7_stream_inner(
        pdf: &mut crate::Pdf<std::io::Cursor<Vec<u8>>>,
        decode_level: DecodeLevel,
        mode: &StreamDataMode,
    ) -> Vec<(String, JsonValue)> {
        let meta = QpdfMetadata {
            pdf_version: "1.3".to_string(),
            max_object_id: 7,
            pushed_inherited_page_resources: false,
            called_get_all_pages: true,
        };
        let JsonValue::Array(elems) =
            build_qpdf_key_with_stream_mode(pdf, meta, decode_level, mode).expect("build failed")
        else {
            panic!("expected Array");
        };
        let JsonValue::Object(map_pairs) = &elems[1] else {
            panic!("objects_map is not an Object");
        };
        let obj7 = map_pairs
            .iter()
            .find(|(k, _)| k == "obj:7 0 R")
            .map(|(_, v)| v)
            .expect("obj:7 0 R not found");
        let JsonValue::Object(obj7_pairs) = obj7 else {
            panic!("obj:7 0 R is not an Object");
        };
        assert_eq!(obj7_pairs[0].0, "stream");
        let JsonValue::Object(inner) = &obj7_pairs[0].1 else {
            panic!("stream value is not an Object");
        };
        inner.clone()
    }

    // ── Test 1: StreamDataMode::None → stream entry has dict only ────────────

    #[test]
    fn stream_data_mode_none_emits_dict_only() {
        let mut pdf = load_one_page_pdf();
        let inner =
            get_obj7_stream_inner(&mut pdf, DecodeLevel::Generalized, &StreamDataMode::None);
        // Must have exactly one key: "dict"
        assert_eq!(
            inner.len(),
            1,
            "None mode: expected 1 key (dict), got {:?}",
            inner.iter().map(|(k, _)| k).collect::<Vec<_>>()
        );
        assert_eq!(inner[0].0, "dict");
    }

    // ── Test 2: StreamDataMode::Inline → data + dict; base64 shape ──────────

    #[test]
    fn stream_data_mode_inline_emits_base64_data_and_dict() {
        let mut pdf = load_one_page_pdf();

        let inner = get_obj7_stream_inner(&mut pdf, DecodeLevel::None, &StreamDataMode::Inline);
        // Must have exactly two keys: "data", "dict" (alphabetical)
        assert_eq!(inner.len(), 2, "Inline mode: expected 2 keys");
        assert_eq!(inner[0].0, "data", "first key must be 'data'");
        assert_eq!(inner[1].0, "dict", "second key must be 'dict'");
        assert!(
            matches!(&inner[0].1, JsonValue::String(_)),
            "data must be a base64 String"
        );
    }

    // ── Test 2a: Inline + DecodeLevel::None emits the raw (filter-encoded)
    //            stream bytes — matching `qpdf --decode-level=none`. ─────────

    #[test]
    fn stream_data_mode_inline_decode_level_none_emits_raw_bytes() {
        let mut pdf = load_one_page_pdf();

        // obj:7 of one-page.pdf is an ASCII85Decode+FlateDecode content stream.
        // resolve() returns the decrypted-but-still-filter-encoded bytes.
        let oref = crate::ObjectRef::new(7, 0);
        let raw_bytes = match pdf.resolve(oref).expect("resolve obj:7") {
            Object::Stream(s) => s.data.clone(),
            other => panic!("obj:7 is not a Stream: {other:?}"),
        };

        let inner = get_obj7_stream_inner(&mut pdf, DecodeLevel::None, &StreamDataMode::Inline);
        let JsonValue::String(b64) = &inner[0].1 else {
            panic!("data is not a String");
        };
        let decoded = base64_decode_test_helper(b64);
        assert_eq!(
            decoded, raw_bytes,
            "DecodeLevel::None must emit the raw filter-encoded stream bytes"
        );
    }

    // ── Test 2b: Inline + DecodeLevel::Generalized emits the filter-decoded
    //            content — matching `qpdf --decode-level=generalized`. ───────

    #[test]
    fn stream_data_mode_inline_decode_level_generalized_emits_decoded_bytes() {
        let mut pdf = load_one_page_pdf();

        let inner =
            get_obj7_stream_inner(&mut pdf, DecodeLevel::Generalized, &StreamDataMode::Inline);
        let JsonValue::String(b64) = &inner[0].1 else {
            panic!("data is not a String");
        };
        let decoded = base64_decode_test_helper(b64);

        // Ground truth captured from:
        //   qpdf --json=2 --json-stream-data=inline --decode-level=generalized \
        //        tests/fixtures/compat/one-page.pdf
        let expected_prefix: &[u8] =
            b"1 0 0 1 0 0 cm  BT /F1 12 Tf 14.4 TL ET\nBT 1 0 0 1 72 720 Tm";
        assert!(
            decoded.starts_with(expected_prefix),
            "DecodeLevel::Generalized must emit filter-decoded content; got {:?}",
            String::from_utf8_lossy(&decoded[..decoded.len().min(64)])
        );
    }

    // ── Test 3: StreamDataMode::File → datafile path + dict ──────────────────

    #[test]
    fn stream_data_mode_file_emits_datafile_and_dict() {
        let mut pdf = load_one_page_pdf();
        let inner = get_obj7_stream_inner(
            &mut pdf,
            DecodeLevel::Generalized,
            &StreamDataMode::File {
                prefix: "out".to_string(),
            },
        );
        // Must have exactly two keys: "datafile", "dict" (alphabetical)
        assert_eq!(inner.len(), 2, "File mode: expected 2 keys");
        assert_eq!(inner[0].0, "datafile", "first key must be 'datafile'");
        assert_eq!(inner[1].0, "dict", "second key must be 'dict'");

        // datafile must be "<prefix>-<obj_num>" = "out-7" for obj:7
        assert_eq!(inner[0].1, JsonValue::String("out-7".to_string()));
    }

    // ── Test 3b: side-file naming has no zero-padding (qpdf 11.9.0) ───────────

    #[test]
    fn format_json_side_file_path_uses_bare_object_number() {
        // qpdf 11.9.0 emits "<prefix>-<obj>" with no zero-padding, for
        // single- and multi-digit object numbers alike.
        assert_eq!(format_json_side_file_path("qp", 7), "qp-7");
        assert_eq!(format_json_side_file_path("qp", 42), "qp-42");
        assert_eq!(format_json_side_file_path("qp", 100), "qp-100");
    }

    // ── Test 4: trailer is not affected by mode ───────────────────────────────

    #[test]
    fn stream_data_mode_trailer_always_has_value_wrapper() {
        let mut pdf = load_one_page_pdf();
        for mode in &[
            StreamDataMode::None,
            StreamDataMode::Inline,
            StreamDataMode::File {
                prefix: "x".to_string(),
            },
        ] {
            let meta = QpdfMetadata {
                pdf_version: "1.3".to_string(),
                max_object_id: 7,
                pushed_inherited_page_resources: false,
                called_get_all_pages: true,
            };
            let JsonValue::Array(elems) =
                build_qpdf_key_with_stream_mode(&mut pdf, meta, DecodeLevel::Generalized, mode)
                    .expect("build failed")
            else {
                panic!("expected Array");
            };
            let JsonValue::Object(map_pairs) = &elems[1] else {
                panic!("objects_map is not an Object");
            };
            let trailer = map_pairs
                .iter()
                .find(|(k, _)| k == "trailer")
                .map(|(_, v)| v)
                .expect("trailer not found");
            let JsonValue::Object(trailer_pairs) = trailer else {
                panic!("trailer is not an Object for mode {mode:?}");
            };
            assert_eq!(
                trailer_pairs[0].0, "value",
                "trailer must have 'value' key regardless of StreamDataMode ({mode:?})"
            );
        }
    }

    // ── Test 5: base64_encode unit tests ─────────────────────────────────────

    #[test]
    fn base64_encode_rfc4648_vectors() {
        // RFC 4648 test vectors
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_encode_all_byte_values() {
        // Encode all 256 byte values; verify length is correct (no panics).
        let all_bytes: Vec<u8> = (0u8..=255).collect();
        let encoded = base64_encode(&all_bytes);
        // 256 bytes → ceil(256/3)*4 = 344 chars
        assert_eq!(encoded.len(), 344);
        // Verify the round-trip using the test helper decoder.
        let decoded = base64_decode_test_helper(&encoded);
        assert_eq!(decoded, all_bytes);
    }

    // ── Test 6: build_qpdf_json_v2_with_options propagates stream_mode ────────

    #[test]
    fn build_qpdf_json_v2_with_options_inline_propagates_to_qpdf_key() {
        let mut pdf = load_one_page_pdf();
        let v2 = build_qpdf_json_v2_with_options(
            &mut pdf,
            DecodeLevel::Generalized,
            &StreamDataMode::Inline,
        )
        .expect("build failed");

        // Navigate: v2["qpdf"][1]["obj:7 0 R"]["stream"]["data"]
        let JsonValue::Object(top_pairs) = &v2 else {
            panic!("expected Object at top level");
        };
        let qpdf_val = top_pairs
            .iter()
            .find(|(k, _)| k == "qpdf")
            .map(|(_, v)| v)
            .expect("qpdf key not found");
        let JsonValue::Array(qpdf_arr) = qpdf_val else {
            panic!("qpdf is not an Array");
        };
        let JsonValue::Object(obj_map) = &qpdf_arr[1] else {
            panic!("qpdf[1] is not an Object");
        };
        let obj7 = obj_map
            .iter()
            .find(|(k, _)| k == "obj:7 0 R")
            .map(|(_, v)| v)
            .expect("obj:7 0 R not found in qpdf key");
        let JsonValue::Object(obj7_pairs) = obj7 else {
            panic!("obj:7 is not an Object");
        };
        let JsonValue::Object(stream_inner) = &obj7_pairs[0].1 else {
            panic!("stream value is not Object");
        };
        // Inline mode: first key is "data"
        assert_eq!(stream_inner[0].0, "data",
            "Inline mode must produce 'data' key in stream entry via build_qpdf_json_v2_with_options");
        assert!(
            matches!(&stream_inner[0].1, JsonValue::String(_)),
            "data must be a String"
        );
    }

    // ── Test 7: build_qpdf_json_v2_with_options threads DecodeLevel to the
    //           qpdf key — None vs Generalized yield different stream data. ──

    #[test]
    fn build_qpdf_json_v2_with_options_threads_decode_level_to_qpdf_key() {
        // Extract obj:7 0 R inline "data" base64 for a given DecodeLevel.
        fn obj7_inline_data(decode_level: DecodeLevel) -> String {
            let mut pdf = load_one_page_pdf();
            let v2 =
                build_qpdf_json_v2_with_options(&mut pdf, decode_level, &StreamDataMode::Inline)
                    .expect("build failed");
            let JsonValue::Object(top) = &v2 else {
                panic!("top is not Object");
            };
            let qpdf = top
                .iter()
                .find(|(k, _)| k == "qpdf")
                .map(|(_, v)| v)
                .expect("qpdf key");
            let JsonValue::Array(arr) = qpdf else {
                panic!("qpdf not Array");
            };
            let JsonValue::Object(obj_map) = &arr[1] else {
                panic!("qpdf[1] not Object");
            };
            let obj7 = obj_map
                .iter()
                .find(|(k, _)| k == "obj:7 0 R")
                .map(|(_, v)| v)
                .expect("obj:7");
            let JsonValue::Object(obj7_pairs) = obj7 else {
                panic!("obj:7 not Object");
            };
            let JsonValue::Object(stream_inner) = &obj7_pairs[0].1 else {
                panic!("stream not Object");
            };
            let JsonValue::String(b64) = &stream_inner[0].1 else {
                panic!("data not String");
            };
            b64.clone()
        }

        let none_b64 = obj7_inline_data(DecodeLevel::None);
        let generalized_b64 = obj7_inline_data(DecodeLevel::Generalized);
        assert_ne!(
            none_b64, generalized_b64,
            "DecodeLevel must reach the qpdf key: None and Generalized must differ \
             for a filtered stream"
        );

        let generalized = base64_decode_test_helper(&generalized_b64);
        assert!(
            generalized.starts_with(b"1 0 0 1 0 0 cm  BT /F1 12 Tf"),
            "Generalized must emit filter-decoded content via build_qpdf_json_v2_with_options"
        );
    }

    // ── Test 8: stream_payload_for_decode_level helper ──────────────────────

    #[test]
    fn stream_payload_decode_level_none_returns_raw() {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        let raw_payload = b"raw payload";
        let encoded = crate::filters::encode_stream_data(&dict, raw_payload).expect("encode");
        let stream = Stream::new(dict, encoded.clone());
        let payload = stream_payload_for_decode_level(&stream, DecodeLevel::None);
        assert!(
            matches!(payload, Cow::Borrowed(_)),
            "DecodeLevel::None must borrow stream.data, not allocate a copy"
        );
        assert_eq!(
            &*payload,
            &encoded[..],
            "DecodeLevel::None must return the raw filter-encoded bytes verbatim"
        );
    }

    #[test]
    fn stream_payload_decode_level_generalized_decodes_filters() {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        let raw_payload = b"decode me through the filter pipeline";
        let encoded = crate::filters::encode_stream_data(&dict, raw_payload).expect("encode");
        let stream = Stream::new(dict, encoded);
        assert_eq!(
            &*stream_payload_for_decode_level(&stream, DecodeLevel::Generalized),
            raw_payload,
            "DecodeLevel::Generalized must return filter-decoded content"
        );
    }

    #[test]
    fn stream_payload_unsupported_filter_falls_back_to_raw() {
        // flpdf cannot decode DCTDecode; qpdf emits the raw bytes for filters
        // it does not decode, so the helper must fall back to raw rather than
        // error out and break the whole JSON document.
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"DCTDecode".to_vec()));
        let raw_payload = b"\xff\xd8\xff\xe0 not really a jpeg";
        let stream = Stream::new(dict, raw_payload.to_vec());
        let payload = stream_payload_for_decode_level(&stream, DecodeLevel::Generalized);
        assert!(
            matches!(payload, Cow::Borrowed(_)),
            "an undecodable filter must fall back to a borrow of the raw bytes"
        );
        assert_eq!(
            &*payload, raw_payload,
            "an undecodable filter must fall back to the raw stream bytes"
        );
    }

    // ══════════════════════════════════════════════════════════════════════════
    // JsonKey / filter_json_keys unit tests  (flpdf-9hc.11.11)
    // ══════════════════════════════════════════════════════════════════════════

    // ── JsonKey::from_str: all JSON v2 names ─────────────────────────────────

    #[test]
    fn json_key_from_str_all_known() {
        assert_eq!(JsonKey::from_str("acroform"), Some(JsonKey::Acroform));
        assert_eq!(JsonKey::from_str("attachments"), Some(JsonKey::Attachments));
        assert_eq!(JsonKey::from_str("encrypt"), Some(JsonKey::Encrypt));
        assert_eq!(JsonKey::from_str("outlines"), Some(JsonKey::Outlines));
        assert_eq!(JsonKey::from_str("pagelabels"), Some(JsonKey::Pagelabels));
        assert_eq!(JsonKey::from_str("pages"), Some(JsonKey::Pages));
        assert_eq!(JsonKey::from_str("qpdf"), Some(JsonKey::Qpdf));
    }

    // ── JsonKey::from_str: unknown names return None ──────────────────────────

    #[test]
    fn json_key_from_str_unknown_returns_none() {
        assert_eq!(JsonKey::from_str(""), None);
        assert_eq!(JsonKey::from_str("Pages"), None); // case-sensitive
        assert_eq!(JsonKey::from_str("version"), None);
        assert_eq!(JsonKey::from_str("parameters"), None);
        assert_eq!(JsonKey::from_str("objectinfo"), None);
        assert_eq!(JsonKey::from_str("objects"), None);
        assert_eq!(JsonKey::from_str("bogus"), None);
    }

    // ── ALL_NAMES contains only JSON v2 keys in alphabetical order ───────────

    #[test]
    fn json_key_all_names_order() {
        assert_eq!(JsonKey::ALL_NAMES[0], "acroform");
        assert_eq!(JsonKey::ALL_NAMES[1], "attachments");
        assert_eq!(JsonKey::ALL_NAMES[2], "encrypt");
        assert_eq!(JsonKey::ALL_NAMES[3], "outlines");
        assert_eq!(JsonKey::ALL_NAMES[4], "pagelabels");
        assert_eq!(JsonKey::ALL_NAMES[5], "pages");
        assert_eq!(JsonKey::ALL_NAMES[6], "qpdf");
        assert_eq!(JsonKey::ALL_NAMES.len(), 7);
    }

    // ── output_key_name maps every v2 selector directly ──────────────────────

    #[test]
    fn json_key_output_key_name_is_direct() {
        assert_eq!(JsonKey::Acroform.output_key_name(), "acroform");
        assert_eq!(JsonKey::Attachments.output_key_name(), "attachments");
        assert_eq!(JsonKey::Encrypt.output_key_name(), "encrypt");
        assert_eq!(JsonKey::Outlines.output_key_name(), "outlines");
        assert_eq!(JsonKey::Pagelabels.output_key_name(), "pagelabels");
        assert_eq!(JsonKey::Pages.output_key_name(), "pages");
        assert_eq!(JsonKey::Qpdf.output_key_name(), "qpdf");
    }

    // ── Helper: build a minimal v2-shaped JsonValue for filter tests ──────────

    fn make_v2_doc() -> JsonValue {
        // Construct a fake but structurally correct v2 document so that
        // filter_json_keys has a real Object to work with.
        JsonValue::Object(vec![
            ("version".to_string(), JsonValue::Integer(2)),
            (
                "parameters".to_string(),
                JsonValue::Object(vec![(
                    "decodelevel".to_string(),
                    JsonValue::String("generalized".to_string()),
                )]),
            ),
            ("pages".to_string(), JsonValue::Array(vec![])),
            ("pagelabels".to_string(), JsonValue::Array(vec![])),
            ("acroform".to_string(), JsonValue::Null),
            ("attachments".to_string(), JsonValue::Object(vec![])),
            ("encrypt".to_string(), JsonValue::Null),
            ("outlines".to_string(), JsonValue::Array(vec![])),
            ("qpdf".to_string(), JsonValue::Array(vec![])),
        ])
    }

    fn key_names(v: &JsonValue) -> Vec<&str> {
        match v {
            JsonValue::Object(pairs) => pairs.iter().map(|(k, _)| k.as_str()).collect(),
            _ => panic!("expected Object"),
        }
    }

    // ── filter_json_keys: empty keys → return input unchanged ─────────────────

    #[test]
    fn filter_json_keys_empty_returns_unchanged() {
        let doc = make_v2_doc();
        let filtered = filter_json_keys(doc.clone(), &[]);
        // Should be structurally equal to the original.
        assert_eq!(key_names(&filtered), key_names(&doc));
    }

    // ── filter_json_keys: single key [Pages] → version, parameters, pages ─────

    #[test]
    fn filter_json_keys_single_pages() {
        let doc = make_v2_doc();
        let filtered = filter_json_keys(doc, &[JsonKey::Pages]);
        let names = key_names(&filtered);
        assert_eq!(names, vec!["version", "parameters", "pages"]);
    }

    // ── filter_json_keys: [Pages, Pagelabels] → 4 keys in v2 order ───────────

    #[test]
    fn filter_json_keys_pages_and_pagelabels() {
        let doc = make_v2_doc();
        let filtered = filter_json_keys(doc, &[JsonKey::Pages, JsonKey::Pagelabels]);
        let names = key_names(&filtered);
        assert_eq!(names, vec!["version", "parameters", "pages", "pagelabels"]);
    }

    // ── filter_json_keys: duplicate dedupe ([Pages, Pages]) ──────────────────

    #[test]
    fn filter_json_keys_dedupe_pages() {
        let doc = make_v2_doc();
        let filtered = filter_json_keys(doc, &[JsonKey::Pages, JsonKey::Pages]);
        let names = key_names(&filtered);
        assert_eq!(names, vec!["version", "parameters", "pages"]);
    }

    // ── filter_json_keys: key absent from input → not in output (no panic) ────

    #[test]
    fn filter_json_keys_absent_key_skipped() {
        // Build a doc that intentionally lacks the "encrypt" section.
        let partial_doc = JsonValue::Object(vec![
            ("version".to_string(), JsonValue::Integer(2)),
            (
                "parameters".to_string(),
                JsonValue::Object(vec![(
                    "decodelevel".to_string(),
                    JsonValue::String("generalized".to_string()),
                )]),
            ),
            ("pages".to_string(), JsonValue::Array(vec![])),
        ]);
        // Request both "pages" (present) and "encrypt" (absent) — must not panic.
        let filtered = filter_json_keys(partial_doc, &[JsonKey::Pages, JsonKey::Encrypt]);
        let names = key_names(&filtered);
        // Only "pages" is present; "encrypt" is silently skipped.
        assert_eq!(names, vec!["version", "parameters", "pages"]);
        assert!(!names.contains(&"encrypt"));
    }

    // ── filter_json_keys: qpdf v2 output order is preserved regardless of
    //    request order (Qpdf before Pages requested → pages still before qpdf) ─

    #[test]
    fn filter_json_keys_output_order_fixed() {
        let doc = make_v2_doc();
        // Request in reverse v2 order.
        let filtered = filter_json_keys(doc, &[JsonKey::Qpdf, JsonKey::Pages]);
        let names = key_names(&filtered);
        // Output must follow v2 order: version, parameters, pages, qpdf
        assert_eq!(names, vec!["version", "parameters", "pages", "qpdf"]);
    }

    // ── filter_json_keys: non-Object input returned as-is (no panic) ─────────

    #[test]
    fn filter_json_keys_non_object_returned_as_is() {
        let v = JsonValue::Integer(42);
        let result = filter_json_keys(v, &[JsonKey::Pages]);
        assert_eq!(result, JsonValue::Integer(42));
    }

    // ── JsonObjectSelector::from_str ──────────────────────────────────────────

    #[test]
    fn json_object_selector_from_str_trailer() {
        assert_eq!(
            JsonObjectSelector::from_str("trailer"),
            Some(JsonObjectSelector::Trailer)
        );
    }

    #[test]
    fn json_object_selector_from_str_num_only() {
        assert_eq!(
            JsonObjectSelector::from_str("3"),
            Some(JsonObjectSelector::Object {
                number: 3,
                generation: 0
            })
        );
    }

    #[test]
    fn json_object_selector_from_str_num_gen_zero() {
        assert_eq!(
            JsonObjectSelector::from_str("3,0"),
            Some(JsonObjectSelector::Object {
                number: 3,
                generation: 0
            })
        );
    }

    #[test]
    fn json_object_selector_from_str_num_gen_nonzero() {
        assert_eq!(
            JsonObjectSelector::from_str("3,5"),
            Some(JsonObjectSelector::Object {
                number: 3,
                generation: 5
            })
        );
    }

    #[test]
    fn json_object_selector_from_str_invalid_three_parts() {
        assert_eq!(JsonObjectSelector::from_str("3,5,6"), None);
    }

    #[test]
    fn json_object_selector_from_str_invalid_non_numeric() {
        assert_eq!(JsonObjectSelector::from_str("abc"), None);
    }

    #[test]
    fn json_object_selector_from_str_invalid_alpha_suffix() {
        assert_eq!(JsonObjectSelector::from_str("3a"), None);
    }

    #[test]
    fn json_object_selector_from_str_invalid_empty() {
        assert_eq!(JsonObjectSelector::from_str(""), None);
    }

    #[test]
    fn json_object_selector_from_str_invalid_negative() {
        assert_eq!(JsonObjectSelector::from_str("-3"), None);
    }

    #[test]
    fn json_object_selector_from_str_invalid_overflow_u32() {
        assert_eq!(JsonObjectSelector::from_str("999999999999"), None);
    }

    #[test]
    fn json_object_selector_from_str_invalid_gen_non_numeric() {
        assert_eq!(JsonObjectSelector::from_str("3,a"), None);
    }

    #[test]
    fn json_object_selector_from_str_invalid_gen_overflow_u16() {
        // 65536 overflows u16::MAX (65535)
        assert_eq!(JsonObjectSelector::from_str("3,65536"), None);
    }

    #[test]
    fn json_object_selector_from_str_uppercase_trailer_rejected() {
        // qpdf uses lowercase only
        assert_eq!(JsonObjectSelector::from_str("Trailer"), None);
        assert_eq!(JsonObjectSelector::from_str("TRAILER"), None);
    }

    // ── Helper: build a minimal qpdf-array-shaped JsonValue for object filter tests

    fn make_qpdf_doc_with_objects() -> JsonValue {
        // Simulate a v2 doc with qpdf: [metadata, {obj:3 0 R, obj:5 0 R, trailer}]
        let metadata = JsonValue::Object(vec![
            ("jsonversion".to_string(), JsonValue::Integer(2)),
            (
                "pdfversion".to_string(),
                JsonValue::String("1.4".to_string()),
            ),
            (
                "pushedinheritedpageresources".to_string(),
                JsonValue::Bool(false),
            ),
            ("calledgetallpages".to_string(), JsonValue::Bool(true)),
            ("maxobjectid".to_string(), JsonValue::Integer(5)),
        ]);
        let objects_map = JsonValue::Object(vec![
            (
                "obj:3 0 R".to_string(),
                JsonValue::Object(vec![("value".to_string(), JsonValue::Integer(42))]),
            ),
            (
                "obj:5 0 R".to_string(),
                JsonValue::Object(vec![("value".to_string(), JsonValue::Integer(99))]),
            ),
            (
                "trailer".to_string(),
                JsonValue::Object(vec![("value".to_string(), JsonValue::Object(vec![]))]),
            ),
        ]);
        JsonValue::Object(vec![
            ("version".to_string(), JsonValue::Integer(2)),
            (
                "parameters".to_string(),
                JsonValue::Object(vec![(
                    "decodelevel".to_string(),
                    JsonValue::String("generalized".to_string()),
                )]),
            ),
            ("pages".to_string(), JsonValue::Array(vec![])),
            (
                "qpdf".to_string(),
                JsonValue::Array(vec![metadata, objects_map]),
            ),
        ])
    }

    fn qpdf_objects_keys(v: &JsonValue) -> Vec<&str> {
        match v {
            JsonValue::Object(pairs) => {
                // find the "qpdf" key
                let qpdf_arr = pairs.iter().find(|(k, _)| k == "qpdf").map(|(_, v)| v);
                match qpdf_arr {
                    Some(JsonValue::Array(arr)) if arr.len() == 2 => match &arr[1] {
                        JsonValue::Object(obj_pairs) => {
                            obj_pairs.iter().map(|(k, _)| k.as_str()).collect()
                        }
                        _ => vec![],
                    },
                    _ => vec![],
                }
            }
            _ => panic!("expected Object"),
        }
    }

    // ── filter_json_objects: empty selectors → input unchanged ────────────────

    #[test]
    fn filter_json_objects_empty_selectors_unchanged() {
        let doc = make_qpdf_doc_with_objects();
        let filtered = filter_json_objects(doc.clone(), &[]);
        // Full equality: every key including pages must be present.
        assert_eq!(key_names(&filtered), key_names(&doc));
        assert_eq!(
            qpdf_objects_keys(&filtered),
            vec!["obj:3 0 R", "obj:5 0 R", "trailer"]
        );
    }

    // ── filter_json_objects: Object{3,0} → only obj:3 0 R in qpdf[1] ─────────

    #[test]
    fn filter_json_objects_single_object() {
        let doc = make_qpdf_doc_with_objects();
        let sel = JsonObjectSelector::Object {
            number: 3,
            generation: 0,
        };
        let filtered = filter_json_objects(doc, &[sel]);
        assert_eq!(qpdf_objects_keys(&filtered), vec!["obj:3 0 R"]);
        // pages and other top-level keys preserved
        assert!(key_names(&filtered).contains(&"pages"));
    }

    // ── filter_json_objects: Trailer → only trailer in qpdf[1] ───────────────

    #[test]
    fn filter_json_objects_trailer_only() {
        let doc = make_qpdf_doc_with_objects();
        let filtered = filter_json_objects(doc, &[JsonObjectSelector::Trailer]);
        assert_eq!(qpdf_objects_keys(&filtered), vec!["trailer"]);
    }

    // ── filter_json_objects: [Object{3,0}, Trailer] → both present ────────────

    #[test]
    fn filter_json_objects_object_and_trailer() {
        let doc = make_qpdf_doc_with_objects();
        let sels = vec![
            JsonObjectSelector::Object {
                number: 3,
                generation: 0,
            },
            JsonObjectSelector::Trailer,
        ];
        let filtered = filter_json_objects(doc, &sels);
        assert_eq!(qpdf_objects_keys(&filtered), vec!["obj:3 0 R", "trailer"]);
    }

    // ── filter_json_objects: non-existent object → qpdf[1] is empty Object ───

    #[test]
    fn filter_json_objects_missing_object_empty_result() {
        let doc = make_qpdf_doc_with_objects();
        let sel = JsonObjectSelector::Object {
            number: 999,
            generation: 0,
        };
        let filtered = filter_json_objects(doc, &[sel]);
        assert_eq!(qpdf_objects_keys(&filtered), Vec::<&str>::new());
    }

    // ── filter_json_objects: duplicate selectors → dedupe ────────────────────

    #[test]
    fn filter_json_objects_duplicate_selectors_dedupe() {
        let doc = make_qpdf_doc_with_objects();
        let sel = JsonObjectSelector::Object {
            number: 3,
            generation: 0,
        };
        let filtered = filter_json_objects(doc, &[sel, sel]);
        // dedupe: obj:3 0 R appears exactly once
        assert_eq!(qpdf_objects_keys(&filtered), vec!["obj:3 0 R"]);
    }

    // ── filter_json_objects: no "qpdf" key → input returned unchanged ─────────

    #[test]
    fn filter_json_objects_no_qpdf_key_unchanged() {
        // A doc without "qpdf" (e.g. after filter_json_keys removed it)
        let doc = JsonValue::Object(vec![
            ("version".to_string(), JsonValue::Integer(2)),
            ("pages".to_string(), JsonValue::Array(vec![])),
        ]);
        let sel = JsonObjectSelector::Object {
            number: 3,
            generation: 0,
        };
        let filtered = filter_json_objects(doc, &[sel]);
        assert_eq!(key_names(&filtered), vec!["version", "pages"]);
    }

    #[test]
    fn filter_json_objects_preserves_malformed_envelope_shapes() {
        let selector = [JsonObjectSelector::Trailer];

        assert_eq!(
            filter_json_objects(JsonValue::Null, &selector),
            JsonValue::Null
        );

        let scalar_qpdf = JsonValue::Object(vec![("qpdf".to_string(), JsonValue::Integer(7))]);
        assert_eq!(
            filter_json_objects(scalar_qpdf.clone(), &selector),
            scalar_qpdf
        );

        let scalar_map = JsonValue::Object(vec![(
            "qpdf".to_string(),
            JsonValue::Array(vec![JsonValue::Null, JsonValue::Integer(9)]),
        )]);
        assert_eq!(
            filter_json_objects(scalar_map.clone(), &selector),
            scalar_map
        );
    }

    // ── filter_json_objects: envelope and other sections preserved ─────────────

    #[test]
    fn filter_json_objects_envelope_and_pages_preserved() {
        let doc = make_qpdf_doc_with_objects();
        let sel = JsonObjectSelector::Object {
            number: 3,
            generation: 0,
        };
        let filtered = filter_json_objects(doc, &[sel]);
        // version, parameters, pages all still present
        let names = key_names(&filtered);
        assert!(names.contains(&"version"), "version missing");
        assert!(names.contains(&"parameters"), "parameters missing");
        assert!(names.contains(&"pages"), "pages missing");
        assert!(names.contains(&"qpdf"), "qpdf missing");
    }

    // ── base64 decode helper (test-only) ─────────────────────────────────────

    /// Simple base64 decoder used only in tests to verify round-trips.
    /// Panics on invalid input.
    fn base64_decode_test_helper(s: &str) -> Vec<u8> {
        fn val(c: u8) -> u8 {
            match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                b'=' => 0, // padding — value ignored
                _ => panic!("invalid base64 char: {c}"),
            }
        }
        let bytes = s.as_bytes();
        assert_eq!(bytes.len() % 4, 0, "base64 length must be multiple of 4");
        let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
        for chunk in bytes.chunks(4) {
            let (a, b, c, d) = (val(chunk[0]), val(chunk[1]), val(chunk[2]), val(chunk[3]));
            let combined = ((a as u32) << 18) | ((b as u32) << 12) | ((c as u32) << 6) | (d as u32);
            out.push(((combined >> 16) & 0xFF) as u8);
            if chunk[2] != b'=' {
                out.push(((combined >> 8) & 0xFF) as u8);
            }
            if chunk[3] != b'=' {
                out.push((combined & 0xFF) as u8);
            }
        }
        out
    }

    // ── filter_json_keys: envelope order is fixed, regardless of input order
    //
    // Regression for CodeRabbit's PR #121 finding. The two-pass envelope
    // copy used `for (k, v) in &pairs` which preserved the input order, so
    // a caller that built `{ parameters, version, ... }` would get
    // `{ parameters, version, ... }` back. The function contract is that
    // the output is *always* `{ version, parameters, ... }`.

    #[test]
    fn filter_json_keys_normalizes_envelope_to_version_then_parameters() {
        // Build the input with parameters first, version second — the
        // reverse of the canonical order.
        let v2 = JsonValue::Object(vec![
            (
                "parameters".to_string(),
                JsonValue::Object(vec![(
                    "decodelevel".to_string(),
                    JsonValue::String("generalized".to_string()),
                )]),
            ),
            ("version".to_string(), JsonValue::Integer(2)),
            ("pages".to_string(), JsonValue::Array(vec![])),
        ]);

        let filtered = filter_json_keys(v2, &[JsonKey::Pages]);
        let JsonValue::Object(pairs) = filtered else {
            panic!("expected Object");
        };
        let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec!["version", "parameters", "pages"],
            "envelope must always be emitted as version, parameters, … even when the input has the opposite order"
        );
    }

    #[test]
    fn filter_json_keys_preserves_envelope_order_when_version_only_present() {
        // Only version is present, parameters is missing — output must
        // still start with version and not panic.
        let v2 = JsonValue::Object(vec![
            ("version".to_string(), JsonValue::Integer(2)),
            ("pages".to_string(), JsonValue::Array(vec![])),
        ]);
        let filtered = filter_json_keys(v2, &[JsonKey::Pages]);
        let JsonValue::Object(pairs) = filtered else {
            panic!("expected Object");
        };
        let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["version", "pages"]);
    }
}
