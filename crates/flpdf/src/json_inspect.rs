//! qpdf JSON v2 inspection builders.
//!
//! Provides the structural frame for qpdf `--json` output.  Each builder
//! returns a [`JsonValue`] that the caller can extend with per-section data
//! (pages, objects, …) in later subtasks.

use crate::json::JsonValue;
use crate::object::{Dictionary, Object, Stream};
use crate::reader::Pdf;
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

/// Decode a PDF text string (ISO 32000-1 §7.9.2) into a Rust `String`.
///
/// Returns `Some(text)` when the byte sequence is valid as a PDF text string
/// (UTF-16BE/UTF-16LE BOM-prefixed UTF-16, or PDFDocEncoding-mapped bytes).
/// Returns `None` when the byte sequence cannot be safely interpreted as
/// text — at which point the caller falls back to the `b:` hex representation.
fn decode_pdf_text_string(bytes: &[u8]) -> Option<String> {
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

/// Classify a PDF string as either a `u:` text string or `b:` binary string.
///
/// Text bytes are decoded through [`decode_pdf_text_string`]; only when the
/// byte sequence cannot be decoded as PDF text do we fall back to the
/// hex-encoded `b:` form.  This matches qpdf JSON v2 behavior for
/// UTF-16-marked text strings, PDFDocEncoded metadata, and binary IDs.
fn pdf_string_to_json_string(bytes: &[u8]) -> String {
    if let Some(text) = decode_pdf_text_string(bytes) {
        format!("u:{text}")
    } else {
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        format!("b:{hex}")
    }
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

/// Convert a stream's dict to a JSON stream-shape object: `{ "stream": { "dict": ... } }`.
fn stream_to_json(stream: &Stream) -> Result<JsonValue, ConvertError> {
    let dict_json = dict_to_json(&stream.dict)?;
    Ok(JsonValue::Object(vec![(
        "stream".to_string(),
        JsonValue::Object(vec![("dict".to_string(), dict_json)]),
    )]))
}

/// PDF オブジェクトを qpdf v2 JSON value 形式に変換する。
///
/// Stream は dict 部分のみが含まれ、stream data 本体は別経路で扱う（.11.10）。
/// Stream が他のオブジェクト内にネストしている場合も `{"stream":{"dict":...}}` を返す。
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
        Object::Real(f) => {
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
    /// Whether inherited page resources were pushed.  Pass `false` for now.
    pub pushed_inherited_page_resources: bool,
    /// Whether `getAllPages()` was called.  Pass `true` for now.
    pub called_get_all_pages: bool,
    // jsonversion is always 2 in v2 output.
}

// ── build_qpdf_key ────────────────────────────────────────────────────────────

/// `qpdf` トップレベル key の中身 (`[metadata, objects_map]`) を構築する。
///
/// Returns a [`JsonValue::Array`] of exactly two elements:
/// 1. The metadata object with fixed key order.
/// 2. The objects map with all indirect objects and the trailer, sorted alphabetically
///    by key.
///
/// # Errors
///
/// Returns a [`ConvertError`] if any object cannot be converted to JSON.
pub fn build_qpdf_key<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    metadata: QpdfMetadata,
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
    // live_object_refs() already excludes Free / Deleted / Missing / Reserved
    // entries at the xref-status level, so any ref returned here is a real
    // indirect object that we should emit. A live indirect object that *is*
    // null (e.g. `1 0 obj null endobj`) is emitted as `{ "value": null }`,
    // matching qpdf.
    let all_refs: Vec<crate::ObjectRef> = pdf.live_object_refs();

    let mut map_pairs: Vec<(String, JsonValue)> = Vec::new();

    for oref in all_refs {
        let key = format!("obj:{} {} R", oref.number, oref.generation);
        let obj = pdf.resolve(oref)?;
        let json_val = match &obj {
            Object::Stream(stream) => {
                // Stream: emit { "stream": { "dict": <dict> } }
                let dict_json = dict_to_json(&stream.dict)?;
                JsonValue::Object(vec![(
                    "stream".to_string(),
                    JsonValue::Object(vec![("dict".to_string(), dict_json)]),
                )])
            }
            other => {
                // Non-stream (including live Object::Null): emit { "value": <json> }.
                let val = pdf_object_to_json(other)?;
                JsonValue::Object(vec![("value".to_string(), val)])
            }
        };
        map_pairs.push((key, json_val));
    }

    // ── 3. Add trailer ─────────────────────────────────────────────────────
    {
        let trailer_dict = pdf.trailer().clone();
        let trailer_json = dict_to_json(&trailer_dict)?;
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
            let resolved = pdf.resolve(*r).map_err(ConvertError::from)?;
            match resolved {
                Object::Stream(_) => Ok(vec![ref_string(r)]),
                Object::Array(_) => {
                    // Recurse so a nested indirect array is also unwrapped.
                    collect_content_refs(pdf, &resolved)
                }
                // /Contents pointing at anything else (Null, missing) → empty.
                _ => Ok(vec![]),
            }
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
            let resolved = pdf.resolve(*r).map_err(ConvertError::from)?;
            match resolved {
                Object::Dictionary(d) => d,
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
        let resolved = pdf.resolve(xobj_ref).map_err(ConvertError::from)?;
        if let Object::Stream(stream) = &resolved {
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
/// - `label` is always `null` (placeholder until flpdf-9hc.11.5).
/// - `outlines` is always `[]` (placeholder until flpdf-9hc.11.6).
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
        let page_obj = pdf.resolve(page_ref).map_err(ConvertError::from)?;
        let contents_obj = match &page_obj {
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
    // first: /St integer, default 1
    let first = match dict.get("St") {
        Some(Object::Integer(n)) => *n,
        _ => 1,
    };

    // prefix: /P text string, decoded as PDF text string without u:/b: decoration.
    // Absent → "".  Undecodable bytes → lossy UTF-8 replacement.
    let prefix = match dict.get("P") {
        Some(Object::String(bytes)) => decode_pdf_text_string(bytes)
            .unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned()),
        _ => String::new(),
    };

    // style: /S name, or null if absent / not a recognised name.
    let style = match dict.get("S") {
        Some(Object::Name(bytes)) => {
            let s = match bytes.as_slice() {
                b"D" => "D",
                b"R" => "R",
                b"r" => "r",
                b"A" => "A",
                b"a" => "a",
                _ => "",
            };
            if s.is_empty() {
                JsonValue::Null
            } else {
                JsonValue::String(s.to_string())
            }
        }
        _ => JsonValue::Null,
    };

    // Key order: alphabetical → first, prefix, style
    JsonValue::Object(vec![
        ("first".to_string(), JsonValue::Integer(first)),
        ("prefix".to_string(), JsonValue::String(prefix)),
        ("style".to_string(), style),
    ])
}

/// Walk a number-tree node for `/PageLabels`, collecting `(page_index, label_dict)` pairs.
///
/// Handles both leaf nodes (`/Nums`) and intermediate nodes (`/Kids`).  When both are
/// present (spec violation), `/Nums` takes priority.  The `seen` set prevents infinite
/// loops on cyclic indirect references.
fn walk_pagelabels<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    node: Object,
    entries: &mut Vec<(i64, Dictionary)>,
    seen: &mut std::collections::BTreeSet<crate::ObjectRef>,
    depth: usize,
    max_depth: usize,
) -> Result<(), ConvertError> {
    if depth > max_depth {
        return Ok(()); // silently truncate to avoid unbounded recursion
    }

    let dict = match node {
        Object::Dictionary(d) => d,
        _ => return Ok(()), // unexpected node type — skip
    };

    // /Nums takes priority over /Kids (spec §7.9.7 leaf vs. intermediate).
    if let Some(Object::Array(nums)) = dict.get("Nums") {
        let nums = nums.clone();
        let mut iter = nums.into_iter();
        while let (Some(idx_obj), Some(label_obj)) = (iter.next(), iter.next()) {
            let idx = match idx_obj {
                Object::Integer(n) => n,
                _ => continue, // malformed — skip pair
            };
            // Label value may be a direct Dictionary or an indirect Reference.
            let label_dict = match label_obj {
                Object::Dictionary(d) => d,
                Object::Reference(r) => match pdf.resolve(r).map_err(ConvertError::from)? {
                    Object::Dictionary(d) => d,
                    _ => continue,
                },
                _ => continue,
            };
            entries.push((idx, label_dict));
        }
        return Ok(());
    }

    // No /Nums — try /Kids (intermediate node).
    if let Some(Object::Array(kids)) = dict.get("Kids") {
        let kids = kids.clone();
        for kid in kids {
            let kid_ref = match kid {
                Object::Reference(r) => r,
                _ => continue,
            };
            if !seen.insert(kid_ref) {
                continue; // cycle — skip
            }
            let child = pdf.resolve(kid_ref).map_err(ConvertError::from)?;
            walk_pagelabels(pdf, child, entries, seen, depth + 1, max_depth)?;
        }
    }

    Ok(())
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

    // Resolve the Catalog.
    let catalog_ref = match pdf.root_ref() {
        Some(r) => r,
        None => return Ok(JsonValue::Array(vec![])),
    };
    let catalog = pdf.resolve(catalog_ref).map_err(ConvertError::from)?;
    let catalog_dict = match catalog {
        Object::Dictionary(d) => d,
        _ => return Ok(JsonValue::Array(vec![])),
    };

    // Look up /PageLabels.  May be absent, a direct Dictionary, or a Reference.
    let pagelabels_val = match catalog_dict.get("PageLabels") {
        Some(v) => v.clone(),
        None => return Ok(JsonValue::Array(vec![])),
    };

    // Resolve indirect reference if needed.
    let root_node = match pagelabels_val {
        Object::Reference(r) => pdf.resolve(r).map_err(ConvertError::from)?,
        other => other,
    };

    let mut entries: Vec<(i64, Dictionary)> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    walk_pagelabels(
        pdf,
        root_node,
        &mut entries,
        &mut seen,
        0,
        DEFAULT_MAX_PAGE_TREE_DEPTH,
    )?;

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

/// Walk a single outline item and return its JSON representation.
///
/// Each item has alphabetically-ordered keys: `action`, `count`, `dest`,
/// `flags`, `kids`, `object`, `structureelement`, `title`.
///
/// The `seen` set prevents infinite loops due to cyclic `/First`/`/Next`
/// links. `depth` / `max_depth` prevent unbounded recursion on deep trees.
fn outline_entry_to_json<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    item_ref: crate::ObjectRef,
    seen: &mut std::collections::BTreeSet<crate::ObjectRef>,
    depth: usize,
    max_depth: usize,
) -> Result<JsonValue, ConvertError> {
    let item_obj = pdf.resolve(item_ref).map_err(ConvertError::from)?;
    let item_dict = match item_obj {
        Object::Dictionary(d) => d,
        _ => {
            // Not a dictionary — return a minimal entry with empty kids.
            let object_str = format!("{} {} R", item_ref.number, item_ref.generation);
            return Ok(JsonValue::Object(vec![
                ("action".to_string(), JsonValue::Null),
                ("count".to_string(), JsonValue::Null),
                ("dest".to_string(), JsonValue::Null),
                ("flags".to_string(), JsonValue::Null),
                ("kids".to_string(), JsonValue::Array(vec![])),
                ("object".to_string(), JsonValue::String(object_str)),
                ("structureelement".to_string(), JsonValue::Null),
                ("title".to_string(), JsonValue::Null),
            ]));
        }
    };

    let object_str = format!("{} {} R", item_ref.number, item_ref.generation);

    // action: /A — only emit ref string when /A is an indirect Reference.
    let action = match item_dict.get("A") {
        Some(Object::Reference(r)) => JsonValue::String(format!("{} {} R", r.number, r.generation)),
        _ => JsonValue::Null,
    };

    // count: /Count integer.
    let count = match item_dict.get("Count") {
        Some(Object::Integer(n)) => JsonValue::Integer(*n),
        _ => JsonValue::Null,
    };

    // dest: /Dest — any value, converted via pdf_object_to_json.
    let dest = match item_dict.get("Dest") {
        Some(v) => pdf_object_to_json(v)?,
        None => JsonValue::Null,
    };

    // flags: /F integer.
    let flags = match item_dict.get("F") {
        Some(Object::Integer(n)) => JsonValue::Integer(*n),
        _ => JsonValue::Null,
    };

    // structureelement: /SE — only emit ref string when indirect Reference.
    let structureelement = match item_dict.get("SE") {
        Some(Object::Reference(r)) => JsonValue::String(format!("{} {} R", r.number, r.generation)),
        _ => JsonValue::Null,
    };

    // title: /Title — decode as PDF text string, bare (no u:/b: prefix).
    let title = match item_dict.get("Title") {
        Some(Object::String(bytes)) => match decode_pdf_text_string(bytes) {
            Some(s) => JsonValue::String(s),
            None => JsonValue::Null,
        },
        _ => JsonValue::Null,
    };

    // kids: walk /First → /Next chain if depth allows.
    let kids = if depth >= max_depth {
        vec![]
    } else {
        // Get the /First reference for this item's children.
        let first_ref = match item_dict.get("First") {
            Some(Object::Reference(r)) => Some(*r),
            _ => None,
        };
        collect_outline_chain(pdf, first_ref, seen, depth + 1, max_depth)?
    };

    Ok(JsonValue::Object(vec![
        ("action".to_string(), action),
        ("count".to_string(), count),
        ("dest".to_string(), dest),
        ("flags".to_string(), flags),
        ("kids".to_string(), JsonValue::Array(kids)),
        ("object".to_string(), JsonValue::String(object_str)),
        ("structureelement".to_string(), structureelement),
        ("title".to_string(), title),
    ]))
}

/// Walk an outline item chain starting at `first_ref`, following `/Next`
/// links, and return JSON entries for each item (recursively expanding kids).
///
/// `seen` is a shared cycle guard across the entire outline tree.
fn collect_outline_chain<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    first_ref: Option<crate::ObjectRef>,
    seen: &mut std::collections::BTreeSet<crate::ObjectRef>,
    depth: usize,
    max_depth: usize,
) -> Result<Vec<JsonValue>, ConvertError> {
    let mut entries = Vec::new();
    let mut current = first_ref;

    while let Some(item_ref) = current {
        // Cycle guard: skip if already visited.
        if !seen.insert(item_ref) {
            break;
        }

        let entry = outline_entry_to_json(pdf, item_ref, seen, depth, max_depth)?;
        entries.push(entry);

        // Advance to the next sibling — must re-resolve to get /Next
        // (outline_entry_to_json consumed the object).
        let item_obj = pdf.resolve(item_ref).map_err(ConvertError::from)?;
        current = match &item_obj {
            Object::Dictionary(d) => match d.get("Next") {
                Some(Object::Reference(r)) => Some(*r),
                _ => None,
            },
            _ => None,
        };
    }

    Ok(entries)
}

/// Build the qpdf JSON v2 `"outlines"` section.
///
/// Returns a [`JsonValue::Array`] where each element is a JSON object
/// representing one root-level outline item (with `kids` recursively
/// expanded).  Returns `JsonValue::Array(vec![])` when the document has no
/// `/Outlines` entry or the outline dictionary has no `/First` child.
///
/// Each entry has keys in alphabetical order:
/// `action`, `count`, `dest`, `flags`, `kids`, `object`,
/// `structureelement`, `title`.
///
/// # Errors
///
/// Returns a [`ConvertError`] if any indirect object resolution fails.
pub fn build_outlines_section<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<JsonValue, ConvertError> {
    use crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH;

    // Resolve the Catalog.
    let catalog_ref = match pdf.root_ref() {
        Some(r) => r,
        None => return Ok(JsonValue::Array(vec![])),
    };
    let catalog = pdf.resolve(catalog_ref).map_err(ConvertError::from)?;
    let catalog_dict = match catalog {
        Object::Dictionary(d) => d,
        _ => return Ok(JsonValue::Array(vec![])),
    };

    // Look up /Outlines. May be absent, a direct Dictionary, or a Reference.
    let outlines_val = match catalog_dict.get("Outlines") {
        Some(v) => v.clone(),
        None => return Ok(JsonValue::Array(vec![])),
    };

    // Resolve indirect reference if needed.
    let outlines_dict = match outlines_val {
        Object::Reference(r) => match pdf.resolve(r).map_err(ConvertError::from)? {
            Object::Dictionary(d) => d,
            _ => return Ok(JsonValue::Array(vec![])),
        },
        Object::Dictionary(d) => d,
        _ => return Ok(JsonValue::Array(vec![])),
    };

    // Get the /First reference for the root outline chain.
    let first_ref = match outlines_dict.get("First") {
        Some(Object::Reference(r)) => Some(*r),
        _ => return Ok(JsonValue::Array(vec![])),
    };

    let mut seen = std::collections::BTreeSet::new();
    let entries = collect_outline_chain(pdf, first_ref, &mut seen, 0, DEFAULT_MAX_PAGE_TREE_DEPTH)?;

    Ok(JsonValue::Array(entries))
}

// ── build_qpdf_json_v2 (top-level composite) ─────────────────────────────────

/// Build the full qpdf JSON v2 document for `pdf`, combining the envelope
/// (`version`, `parameters`) with every section that flpdf currently
/// implements.
///
/// As of flpdf-9hc.11.6 this is: `version`, `parameters`, `pages`,
/// `pagelabels`, `outlines`, `qpdf`. The remaining qpdf v2 sections
/// (`acroform`, `attachments`, `encrypt`) will be inserted here as the
/// respective subtasks (flpdf-9hc.11.7 / .8 / .9) land.
///
/// Key order matches qpdf v2 output: top-level keys are emitted in the
/// fixed order shown in qpdf's `--json=2` output, not alphabetical.
///
/// # Errors
///
/// Returns a [`ConvertError`] if any section builder fails.
pub fn build_qpdf_json_v2<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    decode_level: DecodeLevel,
) -> Result<JsonValue, ConvertError> {
    let mut pairs = match build_envelope(decode_level) {
        JsonValue::Object(p) => p,
        _ => unreachable!("build_envelope always returns an Object"),
    };

    let pages = build_pages_section(pdf)?;
    pairs.push(("pages".to_string(), pages));

    let pagelabels = build_pagelabels_section(pdf)?;
    pairs.push(("pagelabels".to_string(), pagelabels));

    let outlines = build_outlines_section(pdf)?;
    pairs.push(("outlines".to_string(), outlines));

    // qpdf metadata: maxobjectid is the highest object id present in the
    // xref table, *including* deleted/free entries — qpdf's JSON v2 spec
    // wants the highest ID ever assigned in the file, not the highest live
    // ID. pushedinheritedpageresources / calledgetallpages mirror qpdf's
    // defaults.
    let max_object_id = pdf
        .object_refs()
        .iter()
        .map(|r| r.number)
        .max()
        .unwrap_or(0);
    let qpdf_metadata = QpdfMetadata {
        pdf_version: pdf.version().to_string(),
        max_object_id,
        pushed_inherited_page_resources: false,
        called_get_all_pages: true,
    };
    let qpdf = build_qpdf_key(pdf, qpdf_metadata)?;
    pairs.push(("qpdf".to_string(), qpdf));

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
    fn object_string_pdfdoc_high_byte_decodes_as_text() {
        // 0xC7 is "Ç" (LATIN CAPITAL LETTER C WITH CEDILLA) in PDFDocEncoding
        // == ISO 8859-1. qpdf treats this as a text string.
        let result = pdf_object_to_json(&Object::String(vec![b'A', 0xC7, b'B'])).unwrap();
        assert_eq!(result, JsonValue::String("u:A\u{00C7}B".to_string()));
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
    fn object_string_utf16be_with_odd_length_falls_back_to_binary() {
        // FEFF + 0041 + 00 (truncated last unit) → binary fallback.
        let bytes = vec![0xFE, 0xFF, 0x00, 0x41, 0x00];
        let result = pdf_object_to_json(&Object::String(bytes)).unwrap();
        assert_eq!(result, JsonValue::String("b:feff004100".to_string()));
    }

    #[test]
    fn object_string_undefined_pdfdoc_byte_falls_back_to_binary() {
        // 0x00 (NUL) is unassigned in PDFDocEncoding; the whole string falls
        // through to the binary path. This is the qpdf-equivalent treatment
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
        let mut catalog = match pdf.resolve(catalog_ref).expect("resolve catalog") {
            Object::Dictionary(d) => d,
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
            panic!("expected Array");
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
            panic!("expected Array");
        };
        assert_eq!(arr.len(), 1);
        let JsonValue::Object(entry) = &arr[0] else {
            panic!()
        };
        let JsonValue::Object(label_pairs) = &entry[1].1 else {
            panic!()
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
            panic!("expected Array");
        };
        assert_eq!(arr.len(), 1, "expected 1 entry from /Kids walk");
        let JsonValue::Object(entry) = &arr[0] else {
            panic!()
        };
        assert_eq!(entry[0].1, JsonValue::Integer(0));
        let JsonValue::Object(lp) = &entry[1].1 else {
            panic!()
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
            panic!("expected Object at top level");
        };
        // qpdf-style fixed order: version, parameters, pages, pagelabels, outlines, qpdf
        let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "version",
                "parameters",
                "pages",
                "pagelabels",
                "outlines",
                "qpdf"
            ]
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
            panic!("expected Object at top level");
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
            panic!("expected Object at top level");
        };
        let pages = pairs
            .iter()
            .find(|(k, _)| k == "pages")
            .map(|(_, v)| v)
            .expect("pages key missing");
        let JsonValue::Array(page_entries) = pages else {
            panic!("pages must be Array");
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
            panic!("expected Object");
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
        let mut catalog = match pdf.resolve(catalog_ref).expect("resolve catalog") {
            Object::Dictionary(d) => d,
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

        // Key order: action, count, dest, flags, kids, object, structureelement, title
        let keys: Vec<&str> = entry.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "action",
                "count",
                "dest",
                "flags",
                "kids",
                "object",
                "structureelement",
                "title"
            ],
            "key order must be alphabetical"
        );

        // action, count, dest, flags, structureelement = Null (absent in item)
        assert_eq!(entry[0].1, JsonValue::Null, "action must be Null");
        assert_eq!(entry[1].1, JsonValue::Null, "count must be Null");
        assert_eq!(entry[2].1, JsonValue::Null, "dest must be Null");
        assert_eq!(entry[3].1, JsonValue::Null, "flags must be Null");
        // kids = [] (no /First in item)
        assert_eq!(entry[4].1, JsonValue::Array(vec![]), "kids must be empty");
        // object = "101 0 R"
        assert_eq!(
            entry[5].1,
            JsonValue::String("101 0 R".to_string()),
            "object mismatch"
        );
        assert_eq!(entry[6].1, JsonValue::Null, "structureelement must be Null");
        // title = bare "Chapter 1" (no u: prefix)
        assert_eq!(
            entry[7].1,
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
}
