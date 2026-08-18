//! qpdf correspondence: `QPDFObjectHandle::getJSON` / `writeJSON` object serialization and `QPDF_Stream::writeStreamJSON` payload/dictionary normalization.
//! qpdf JSON v2 value conversion and canonical object/stream serialization.
//!
//! Provides the generic value-conversion frame for qpdf `--json` output.
//! Section builders and command-boundary output selection are owned by
//! [`crate::job`].
//!
//! The `qpdf` top-level key is not built here: qpdf serializes it in
//! `QPDF_json.cc`, and [`crate::document_json`] holds that boundary. The
//! canonical ordinary-object serializer is in [`ObjectHandle`]::write_json.
//! The former raw-Object JSON bridge is retained only in test builds for
//! byte-boundary regression coverage. Production object and stream values
//! route through [`ObjectHandle`] and [`crate::document_json`].

use crate::json::Json;
#[cfg(test)]
use crate::object::Dictionary;
use crate::object::{Object, ObjectRef, Stream};
use crate::object_handle::{ObjectHandle, ObjectJsonError};
#[cfg(test)]
use crate::pipeline::Pipeline;
use crate::pipeline::PipelineError;
use crate::Pdf;
use std::borrow::Cow;
use std::io::{Read, Seek, Write};

pub(crate) use crate::pdf_string::decode_pdf_text_string;
#[cfg(test)]
pub(crate) use crate::pdf_string::lossy_utf16_to_utf8;

// Compatibility re-exports: the qpdf JSON v2 section builders and command
// writer now live under `job/`, but their historical public paths remain
// available to library and test callers while the staged migration proceeds.
pub(crate) use crate::job::checksum_to_hex;
pub use crate::job::{
    build_acroform_section, build_attachments_section, build_encrypt_section,
    build_outlines_section, build_pagelabels_section, build_pages_section,
    write_qpdf_json_v2_selected_objects_to_output_with_options,
    write_qpdf_json_v2_selected_objects_with_options,
};
#[cfg(test)]
pub(crate) use crate::job::{
    cf_method_string, collect_content_refs, collect_image_refs, parse_pdf_date,
};

/// The qpdf JSON version these builders emit.
///
/// qpdf selects the version from `--json=N`; flpdf accepts only version 2, so
/// the value is fixed here and passed on to the writers that qpdf also
/// parameterizes by version.
pub(crate) const QPDF_JSON_VERSION: i32 = 2;

// ── ConvertError ──────────────────────────────────────────────────────────────

/// Errors that can occur when converting PDF objects to JSON values.
#[derive(Debug, Clone, PartialEq)]
pub enum ConvertError {
    /// A non-finite float (NaN or infinity) was encountered.
    NonFiniteFloat,
    /// An underlying PDF read/parse error.
    PdfError(String),
    /// A shared JSON value could not be mutated as the requested container.
    JsonError(String),
}

impl std::fmt::Display for ConvertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConvertError::NonFiniteFloat => {
                write!(f, "non-finite float cannot be serialized as JSON")
            }
            ConvertError::PdfError(msg) => write!(f, "PDF error: {msg}"),
            ConvertError::JsonError(msg) => write!(f, "JSON error: {msg}"),
        }
    }
}

impl std::error::Error for ConvertError {}

impl From<crate::Error> for ConvertError {
    fn from(err: crate::Error) -> Self {
        ConvertError::PdfError(err.to_string())
    }
}

impl From<ObjectJsonError> for JsonOutputError {
    fn from(error: ObjectJsonError) -> Self {
        match error {
            ObjectJsonError::Pipeline(error) => Self::Pipeline(error),
            ObjectJsonError::NonFiniteFloat => Self::Convert(ConvertError::NonFiniteFloat),
            ObjectJsonError::Json(message) => Self::Convert(ConvertError::JsonError(message)),
            ObjectJsonError::Pdf(message) => Self::Convert(ConvertError::PdfError(message)),
            other => Self::Convert(ConvertError::PdfError(other.to_string())),
        }
    }
}

/// Failure while converting or incrementally writing a qpdf JSON document.
#[derive(Debug, thiserror::Error)]
pub enum JsonOutputError {
    #[error(transparent)]
    Convert(#[from] ConvertError),
    #[error(transparent)]
    Pipeline(#[from] PipelineError),
    #[error("{operation} {path}: {message}")]
    SideFileIo {
        operation: &'static str,
        path: String,
        message: String,
        #[source]
        source: std::io::Error,
    },
    /// A qpdf JSON version other than 2 was requested.
    #[error("QPDF::writeJSON: only version 2 is supported")]
    UnsupportedVersion,
}

/// Ordinary output handle supplied at the qpdf JSON command boundary.
pub enum JsonOutput<'a> {
    /// Standard output, which is finished at the command boundary.
    Stdout(&'a mut dyn Write),
    /// A top-level output file, whose terminal stage is not explicitly finished.
    File(&'a mut dyn Write),
}

pub(crate) fn side_file_io_error(
    operation: &'static str,
    path: &str,
    source: std::io::Error,
) -> JsonOutputError {
    let rendered = source.to_string();
    let message = source
        .raw_os_error()
        .and_then(|code| rendered.strip_suffix(&format!(" (os error {code})")))
        .unwrap_or(&rendered)
        .to_owned();
    JsonOutputError::SideFileIo {
        operation,
        path: path.to_owned(),
        message,
        source,
    }
}

pub(crate) fn json_array(values: impl IntoIterator<Item = Json>) -> Result<Json, ConvertError> {
    let array = Json::make_array();
    for value in values {
        array
            .add_array_element(value)
            .map_err(|error| ConvertError::JsonError(error.to_string()))?;
    }
    Ok(array)
}

pub(crate) fn json_dictionary<K: AsRef<[u8]>>(
    pairs: impl IntoIterator<Item = (K, Json)>,
) -> Result<Json, ConvertError> {
    let dictionary = Json::make_dictionary();
    for (key, value) in pairs {
        dictionary
            .add_dictionary_member(key, value)
            .map_err(|error| ConvertError::JsonError(error.to_string()))?;
    }
    Ok(dictionary)
}

#[cfg(test)]
fn test_value(value: &Json) -> Result<serde_json::Value, ConvertError> {
    let encoded = value
        .unparse()
        .map_err(|error| ConvertError::JsonError(error.to_string()))?;
    serde_json::from_slice(&encoded).map_err(|error| ConvertError::JsonError(error.to_string()))
}

// ── pdf_object_to_json ────────────────────────────────────────────────────────

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
#[cfg(test)]
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
#[cfg(test)]
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

/// Convert flpdf's decoded name bytes to qpdf JSON v2's name representation.
///
/// qpdf emits a valid UTF-8 name directly, including the leading slash. The
/// JSON writer itself escapes controls and quotes. Only a name that is not
/// valid UTF-8 gets the `n:` prefix and PDF `#xx` normalization.
#[cfg(test)]
fn qpdf_name_to_json_string(bytes: &[u8]) -> String {
    let mut raw = Vec::with_capacity(bytes.len() + 1);
    raw.push(b'/');
    raw.extend_from_slice(bytes);
    if let Ok(name) = String::from_utf8(raw) {
        return name;
    }

    let mut normalized = String::with_capacity(bytes.len() + 3);
    normalized.push_str("n:/");
    for &byte in bytes {
        if byte == 0 {
            normalized.push('#');
        } else if !(33..=126).contains(&byte)
            || matches!(
                byte,
                b'#' | b'/' | b'(' | b')' | b'{' | b'}' | b'<' | b'>' | b'[' | b']' | b'%'
            )
        {
            use std::fmt::Write as _;
            let _ = write!(normalized, "#{byte:02x}");
        } else {
            normalized.push(char::from(byte));
        }
    }
    normalized
}

/// A prepared raw-PDF value that retains both container order and PDF scalar
/// type until emission. This is intentionally separate from generic [`Json`],
/// whose scalar writer coalesces each value into one pipeline write.
///
/// qpdf 11.9.0 dispatches every PDF object to its type-specific `writeJSON`
/// implementation through one shared `JSON::Writer`. That writer's start,
/// next, and end methods also define observable pipeline-write boundaries.
#[cfg(test)]
pub(crate) enum OrderedPdfJson {
    Scalar(RawPdfJsonScalar),
    Array(Vec<OrderedPdfJson>),
    Dictionary(Vec<(RawPdfJsonKey, OrderedPdfJson)>),
}

#[cfg(test)]
pub(crate) enum RawPdfJsonScalar {
    Null,
    Boolean(bool),
    Integer(i64),
    Real(Vec<u8>),
    Name(RawPdfName),
    String(RawPdfString),
    Reference(Vec<u8>),
}

#[cfg(test)]
pub(crate) enum RawPdfJsonKey {
    PdfName(RawPdfName),
    Literal(Vec<u8>),
}

#[cfg(test)]
pub(crate) struct RawPdfName {
    non_utf8: bool,
    encoded: Vec<u8>,
}

#[cfg(test)]
pub(crate) struct RawPdfString {
    unicode: bool,
    encoded: Vec<u8>,
}

#[cfg(test)]
impl RawPdfName {
    fn from_decoded(bytes: &[u8]) -> Self {
        let rendered = qpdf_name_to_json_string(bytes).into_bytes();
        let (non_utf8, value) = match rendered.strip_prefix(b"n:") {
            Some(value) => (true, value),
            None => (false, rendered.as_slice()),
        };
        Self {
            non_utf8,
            encoded: encode_raw_pdf_json_string(value),
        }
    }
}

#[cfg(test)]
impl RawPdfString {
    fn from_pdf_bytes(bytes: &[u8]) -> Self {
        let rendered = pdf_string_to_json_string(bytes).into_bytes();
        let (unicode, value) = if let Some(value) = rendered.strip_prefix(b"u:") {
            (true, value)
        } else {
            (
                false,
                rendered
                    .strip_prefix(b"b:")
                    .expect("PDF JSON strings always have a u: or b: prefix"),
            )
        };
        Self {
            unicode,
            encoded: encode_raw_pdf_json_string(value),
        }
    }
}

#[cfg(test)]
fn encode_raw_pdf_json_string(value: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(value.len());
    for &byte in value {
        match byte {
            b'\\' => encoded.extend_from_slice(b"\\\\"),
            b'"' => encoded.extend_from_slice(b"\\\""),
            b'\x08' => encoded.extend_from_slice(b"\\b"),
            b'\x0c' => encoded.extend_from_slice(b"\\f"),
            b'\n' => encoded.extend_from_slice(b"\\n"),
            b'\r' => encoded.extend_from_slice(b"\\r"),
            b'\t' => encoded.extend_from_slice(b"\\t"),
            0x00..=0x1f => {
                encoded.extend_from_slice(if byte < 0x10 { b"\\u000" } else { b"\\u001" });
                encoded.push(b"0123456789abcdef"[(byte & 0x0f) as usize]);
            }
            _ => encoded.push(byte),
        }
    }
    encoded
}

#[cfg(test)]
struct RawPdfJsonWriter<'a> {
    out: &'a mut dyn Pipeline,
    first: bool,
    indent: usize,
}

#[cfg(test)]
impl<'a> RawPdfJsonWriter<'a> {
    const SPACES: &'static [u8; 52] = b",\n                                                  ";
    const SPACE_BLOCK: usize = Self::SPACES.len() - 2;

    fn new(out: &'a mut dyn Pipeline, depth: usize) -> Self {
        Self {
            out,
            first: true,
            indent: 2 * depth,
        }
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), PipelineError> {
        self.out.write(bytes)
    }

    fn write_next(&mut self) -> Result<(), PipelineError> {
        let spaces = self.indent;
        let remainder = spaces % Self::SPACE_BLOCK;
        if self.first {
            self.first = false;
            self.write(&Self::SPACES[1..remainder + 2])?;
        } else {
            self.write(&Self::SPACES[..remainder + 2])?;
        }
        let mut remaining = spaces;
        while remaining >= Self::SPACE_BLOCK {
            self.write(&Self::SPACES[2..])?;
            remaining -= Self::SPACE_BLOCK;
        }
        Ok(())
    }

    fn write_start(&mut self, delimiter: u8) -> Result<(), PipelineError> {
        self.write(&[delimiter])?;
        self.first = true;
        self.indent += 2;
        Ok(())
    }

    fn write_end(&mut self, delimiter: u8) -> Result<(), PipelineError> {
        if self.indent > 1 {
            self.indent -= 2;
        }
        if !self.first {
            self.first = true;
            self.write_next()?;
        }
        self.first = false;
        self.write(&[delimiter])
    }

    fn write_name(&mut self, name: &RawPdfName, suffix: &[u8]) -> Result<(), PipelineError> {
        self.write(if name.non_utf8 { b"\"n:" } else { b"\"" })?;
        self.write(&name.encoded)?;
        self.write(suffix)
    }

    fn write_key(&mut self, key: &RawPdfJsonKey) -> Result<(), PipelineError> {
        self.write_next()?;
        match key {
            RawPdfJsonKey::PdfName(name) => self.write_name(name, b"\": "),
            RawPdfJsonKey::Literal(encoded) => {
                self.write(b"\"")?;
                self.write(encoded)?;
                self.write(b"\": ")
            }
        }
    }

    fn write_scalar(&mut self, scalar: &RawPdfJsonScalar) -> Result<(), PipelineError> {
        match scalar {
            RawPdfJsonScalar::Null => self.write(b"null"),
            RawPdfJsonScalar::Boolean(value) => self.write(if *value { b"true" } else { b"false" }),
            RawPdfJsonScalar::Integer(value) => self.write(value.to_string().as_bytes()),
            RawPdfJsonScalar::Real(value) => {
                if let Some(tail) = value.strip_prefix(b"-.") {
                    self.write(b"-0.")?;
                    self.write(tail)
                } else if value.starts_with(b".") {
                    self.write(b"0")?;
                    self.write(value)
                } else {
                    self.write(value)
                }
            }
            RawPdfJsonScalar::Name(name) => self.write_name(name, b"\""),
            RawPdfJsonScalar::String(value) => {
                self.write(if value.unicode { b"\"u:" } else { b"\"b:" })?;
                self.write(&value.encoded)?;
                self.write(b"\"")
            }
            RawPdfJsonScalar::Reference(reference) => {
                self.write(b"\"")?;
                self.write(reference)?;
                self.write(b"\"")
            }
        }
    }

    fn write_value(&mut self, value: &OrderedPdfJson) -> Result<(), PipelineError> {
        match value {
            OrderedPdfJson::Scalar(value) => self.write_scalar(value)?,
            OrderedPdfJson::Array(values) => {
                self.write_start(b'[')?;
                for value in values {
                    self.write_next()?;
                    self.write_value(value)?;
                }
                self.write_end(b']')?;
            }
            OrderedPdfJson::Dictionary(entries) => {
                self.write_start(b'{')?;
                for (key, value) in entries {
                    self.write_key(key)?;
                    self.write_value(value)?;
                }
                self.write_end(b'}')?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
impl OrderedPdfJson {
    pub(crate) fn write(
        &self,
        out: &mut dyn Pipeline,
        depth: usize,
    ) -> Result<(), JsonOutputError> {
        RawPdfJsonWriter::new(out, depth).write_value(self)?;
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn ordered_qpdf_dict<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict: &Dictionary,
) -> Result<OrderedPdfJson, ConvertError> {
    let mut entries = Vec::new();
    for (raw_key, value) in dict.iter() {
        if crate::qpdf_null::value_is_null(pdf, value)? {
            continue;
        }
        entries.push((
            RawPdfJsonKey::PdfName(RawPdfName::from_decoded(raw_key)),
            ordered_qpdf_object(pdf, value)?,
        ));
    }
    Ok(OrderedPdfJson::Dictionary(entries))
}

#[cfg(test)]
pub(crate) fn ordered_qpdf_object<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    object: &Object,
) -> Result<OrderedPdfJson, ConvertError> {
    match object {
        Object::Reference(reference) if !crate::qpdf_null::reference_is_valid(*reference) => {
            Ok(OrderedPdfJson::Scalar(RawPdfJsonScalar::Null))
        }
        Object::Reference(reference) => Ok(OrderedPdfJson::Scalar(RawPdfJsonScalar::Reference(
            format!("{} {} R", reference.number, reference.generation).into_bytes(),
        ))),
        Object::Array(items) => Ok(OrderedPdfJson::Array(
            items
                .iter()
                .map(|item| ordered_qpdf_object(pdf, item))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        Object::Dictionary(dict) => ordered_qpdf_dict(pdf, dict),
        Object::Stream(stream) => Ok(OrderedPdfJson::Dictionary(vec![(
            RawPdfJsonKey::Literal(b"stream".to_vec()),
            OrderedPdfJson::Dictionary(vec![(
                RawPdfJsonKey::Literal(b"dict".to_vec()),
                ordered_qpdf_dict(pdf, &stream.dict)?,
            )]),
        )])),
        Object::Null | Object::Operator(_) | Object::InlineImage(_) => {
            Ok(OrderedPdfJson::Scalar(RawPdfJsonScalar::Null))
        }
        Object::Boolean(value) => Ok(OrderedPdfJson::Scalar(RawPdfJsonScalar::Boolean(*value))),
        Object::Integer(value) => Ok(OrderedPdfJson::Scalar(RawPdfJsonScalar::Integer(*value))),
        Object::Real(value) => {
            if !value.is_finite() {
                return Err(ConvertError::NonFiniteFloat);
            }
            Ok(OrderedPdfJson::Scalar(RawPdfJsonScalar::Real(
                value.to_string().into_bytes(),
            )))
        }
        Object::RealLiteral { value, .. } => {
            if !value.is_finite() {
                return Err(ConvertError::NonFiniteFloat);
            }
            let mut encoded = Vec::new();
            object.write_pdf(&mut encoded);
            Ok(OrderedPdfJson::Scalar(RawPdfJsonScalar::Real(encoded)))
        }
        Object::Name(value) => Ok(OrderedPdfJson::Scalar(RawPdfJsonScalar::Name(
            RawPdfName::from_decoded(value),
        ))),
        Object::String(value) => Ok(OrderedPdfJson::Scalar(RawPdfJsonScalar::String(
            RawPdfString::from_pdf_bytes(value),
        ))),
    }
}

pub(crate) fn qpdf_resolve_top_level_object<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    start: ObjectRef,
) -> Result<Object, ConvertError> {
    let mut current = start;
    let mut visited = std::collections::BTreeSet::new();
    loop {
        if !visited.insert(current) {
            return Ok(Object::Null);
        }
        match pdf.resolve_qpdf_json_object(current)? {
            Object::Reference(next) => current = next,
            terminal => return Ok(terminal),
        }
    }
}

/// Return the qpdf JSON payload for a selected top-level stream object.
///
/// Unlike [`Pdf::resolve`](crate::Pdf::resolve), this intentionally uses the
/// qpdf JSON object-cache view, which can retain a historical xref stream that
/// a newer xref section has freed. The owned return value lets callers inspect
/// or persist the exact payload named by a qpdf JSON file-mode `datafile`
/// entry while keeping that resolver and its cache semantics encapsulated.
pub fn qpdf_raw_stream_payload<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    object_ref: ObjectRef,
    decode_level: DecodeLevel,
) -> Result<Option<Vec<u8>>, ConvertError> {
    let Object::Stream(stream) = qpdf_resolve_top_level_object(pdf, object_ref)? else {
        return Ok(None);
    };
    Ok(Some(
        stream_payload_with_decode_status(&stream, decode_level)
            .bytes
            .into_owned(),
    ))
}

/// Convert a PDF object handle into the qpdf v2 JSON value form.
///
/// For streams, only the dictionary part is included; the stream data body is
/// handled through a separate path. The canonical ObjectHandle writer emits
/// the stream dictionary directly, matching `QPDF_Stream::writeJSON`
/// (`libqpdf/QPDF_Stream.cc:181-184`); the document-level `{"stream":{"dict":...}}`
/// wrapper remains the separate `writeStreamJSON` consumer.
///
/// The top-level `handle` is never resolved: an indirect `handle` (whether or
/// not it has already been resolved elsewhere) is always rendered as its own
/// `"N G R"` reference string, matching qpdf's non-dereferenced `getJSON`/
/// `writeJSON` contract (`include/qpdf/QPDFObjectHandle.hh:1166-1219`,
/// `libqpdf/QPDFObjectHandle.cc:1605-1659`) — the check for
/// [`ObjectHandle::object_ref`] runs before any type-specific accessor for
/// exactly this reason: [`ObjectHandle::as_array`]/[`ObjectHandle::as_dictionary`]
/// etc. return the *resolved* value for an indirect handle that some other
/// code path already resolved, which would otherwise inline it instead of
/// reporting the reference.
///
/// A direct dictionary is the qpdf exception at the child boundary:
/// `QPDF_Dictionary::writeJSON` calls `isNull()` on each child before writing
/// its key (`libqpdf/QPDF_Dictionary.cc:75-76`), and `isNull()` dereferences
/// an indirect child (`libqpdf/QPDFObjectHandle.cc:353-356`) to decide whether
/// to omit it. A non-null child is still rendered as its own reference string
/// because the child writer retains `dereference_indirect=false`; resolver I/O
/// or resolver failures can nevertheless occur while making that null check.
///
/// # Errors
///
/// Returns [`ConvertError::NonFiniteFloat`] when a real value is non-finite
/// (NaN or infinity), or a [`ConvertError::PdfError`] when `handle` nests
/// past an internal depth bound — unlike the legacy `Object` tree, a caller
/// can build a cyclic `ObjectHandle` graph directly (two direct dictionaries
/// linked to each other via `replace_key`, with no indirect object involved
/// at all), so this bounds recursion the same way
/// [`ObjectHandle::materialize`] does rather than assuming acyclic input.
pub fn pdf_object_to_json(handle: &ObjectHandle) -> Result<Json, ConvertError> {
    handle
        .get_json(QPDF_JSON_VERSION, false)
        .map_err(convert_object_json_error)
}

fn convert_object_json_error(error: ObjectJsonError) -> ConvertError {
    match error {
        ObjectJsonError::NonFiniteFloat => ConvertError::NonFiniteFloat,
        ObjectJsonError::Json(message) => ConvertError::JsonError(message),
        ObjectJsonError::Pipeline(error) => ConvertError::PdfError(error.to_string()),
        ObjectJsonError::Pdf(message) => ConvertError::PdfError(message),
        other => ConvertError::PdfError(other.to_string()),
    }
}

/// Convert an outline `/Dest` handle into the qpdf v2 JSON value form,
/// first resolving *this* handle's own indirect identity (if any) to its
/// value before dispatching by type — qpdf's
/// `getJSON(json_version, /* dereference_indirect = */ true)` contract
/// (`libqpdf/QPDFObjectHandle.cc:1614-1634`), used for outline `/Dest`
/// (`libqpdf/QPDFJob.cc:1086`, `:1126`: `oiter.getDest().getJSON(m->json_version,
/// true)`, contrasted with the `object` field's plain
/// `getJSON(m->json_version)` immediately above each of those calls, at the
/// default `dereference_indirect=false` that [`pdf_object_to_json`] itself
/// always uses). Nested indirect children — for example the page operand of
/// a `[3 0 R /Fit]` destination array — are still rendered as their own
/// `"N G R"` reference strings: `dereference_indirect` never cascades past
/// the handle it is called on directly (`libqpdf/QPDF_Array.cc:153-187`,
/// `libqpdf/QPDF_Dictionary.cc:72-96` test each child's own
/// `getObjGen().isIndirect()` unconditionally, ignoring the flag their
/// parent was serialized with). Confirmed against live qpdf 11.9.0: `/Dest 8
/// 0 R` with object 8 holding `[3 0 R /Fit]` emits `"dest": ["3 0 R",
/// "/Fit"]`.
pub(crate) fn pdf_dest_to_json(handle: &ObjectHandle) -> Result<Json, ConvertError> {
    handle
        .get_json(QPDF_JSON_VERSION, true)
        .map_err(convert_object_json_error)
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
    /// The JSON writer opens the named file after the outer `stream` key and
    /// writes its decoded bytes before emitting the stream dictionary.
    File { prefix: String },
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
/// filter-encoded) bytes returned by [`Pdf::resolve`](crate::Pdf::resolve).
///
/// - [`DecodeLevel::None`] → the raw filter-encoded bytes, verbatim.
/// - Any other level → the filter-decoded content, computed via
///   crate::filters::decode_stream_data when qpdf's filterability and
///   decode-level gate permit a filter path implemented by flpdf.
///
/// qpdf leaves specialized and lossy streams raw until the requested decode
/// level includes them. When the filter pipeline cannot decode a stream — e.g.
/// an unsupported filter such as DCTDecode — this falls back to the raw bytes
/// rather than erroring, matching qpdf, which emits the raw payload for filters
/// it does not decode rather than failing the whole document. qpdf filter types
/// not implemented by flpdf, such as DCTDecode, remain raw even at All.
///
/// Returns a [`Cow`] so the raw-bytes paths ([`DecodeLevel::None`] and the
/// decode-error fallback) borrow `stream.data` instead of copying it — only the
/// successful decode path allocates (it must: the decoded bytes are new).
pub fn stream_payload_for_decode_level(
    stream: &Stream,
    decode_level: DecodeLevel,
) -> Cow<'_, [u8]> {
    stream_payload_with_decode_status(stream, decode_level).bytes
}

pub(crate) struct StreamPayload<'a> {
    pub(crate) bytes: Cow<'a, [u8]>,
    #[allow(dead_code)]
    pub(crate) decode_succeeded: bool,
}

pub(crate) fn stream_payload_with_decode_status(
    stream: &Stream,
    decode_level: DecodeLevel,
) -> StreamPayload<'_> {
    if matches!(decode_level, DecodeLevel::None) {
        return StreamPayload {
            bytes: Cow::Borrowed(&stream.data),
            decode_succeeded: false,
        };
    }

    let Some(capabilities) = crate::filters::stream_filter_capabilities(&stream.dict) else {
        return StreamPayload {
            bytes: Cow::Borrowed(&stream.data),
            decode_succeeded: false,
        };
    };
    let can_filter = if decode_level == DecodeLevel::Generalized {
        !capabilities.specialized_compression && !capabilities.lossy_compression
    } else if decode_level == DecodeLevel::Specialized {
        !capabilities.lossy_compression
    } else {
        // DecodeLevel::None returned above, so this remaining level is All.
        true
    };

    if !can_filter {
        return StreamPayload {
            bytes: Cow::Borrowed(&stream.data),
            decode_succeeded: false,
        };
    }

    match crate::filters::decode_stream_data(&stream.dict, &stream.data) {
        Ok(decoded) => StreamPayload {
            bytes: Cow::Owned(decoded),
            decode_succeeded: true,
        },
        Err(_) => StreamPayload {
            bytes: Cow::Borrowed(&stream.data),
            decode_succeeded: false,
        },
    }
}

// ── JsonKey ────────────────────────────────────────────────

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
    pub(crate) fn output_key_name(self) -> &'static str {
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

// ── JsonObjectSelector ────────────────────────────────────────────────────────

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

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document_json::{format_json_side_file_path, write_json_stream_file};
    use crate::pipeline::test_support::{shared_trace, RecordingSink, TraceCall};
    use crate::pipeline::{Pipeline, PipelineError, PipelineResult, PlString};
    use crate::{Dictionary, Object, Stream};
    use std::rc::Rc;

    fn number(value: impl ToString) -> serde_json::Value {
        serde_json::from_str(&value.to_string()).expect("number must serialize")
    }

    fn object(pairs: Vec<(String, serde_json::Value)>) -> serde_json::Value {
        serde_json::Value::Object(pairs.into_iter().collect())
    }

    fn object_pairs(
        value: impl std::borrow::Borrow<serde_json::Value>,
    ) -> Vec<(String, serde_json::Value)> {
        value
            .borrow()
            .as_object()
            .expect("expected JSON object")
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }

    fn stream_handle<R: Read + Seek>(pdf: &mut Pdf<R>, stream: Stream) -> ObjectHandle {
        pdf.lift_object_to_handle(&Object::Stream(stream))
            .expect("legacy test stream must lift to a canonical handle")
    }

    struct FailAfterWriter {
        remaining: usize,
        bytes: Vec<u8>,
    }

    impl std::io::Write for FailAfterWriter {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            if self.remaining == 0 {
                return Err(std::io::Error::other("sink full"));
            }
            let written = self.remaining.min(buffer.len());
            self.bytes.extend_from_slice(&buffer[..written]);
            self.remaining -= written;
            Ok(written)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct SingleAttemptInterruptedWriter {
        write_lengths: Vec<usize>,
    }

    impl std::io::Write for SingleAttemptInterruptedWriter {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.write_lengths.push(buffer.len());
            assert_eq!(
                self.write_lengths.len(),
                1,
                "Interrupted stdio write must not be retried"
            );
            Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "Interrupted system call",
            ))
        }

        // cov:ignore-start: the regressions require an interrupted drain to stop before inner flush
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        // cov:ignore-end
    }

    #[derive(Default)]
    struct FlushProbe {
        bytes: Vec<u8>,
        flushes: usize,
        write_lengths: Vec<usize>,
        rejected_length: Option<usize>,
        remaining: Option<usize>,
        errno: Option<i32>,
        overflow_errno: Option<i32>,
    }

    impl std::io::Write for FlushProbe {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.write_lengths.push(buffer.len());
            if let (Some(remaining), Some(errno)) = (self.remaining, self.overflow_errno) {
                if buffer.len() > remaining {
                    return Err(std::io::Error::from_raw_os_error(errno));
                }
            }
            if let Some(errno) = self.errno {
                return Err(std::io::Error::from_raw_os_error(errno));
            }
            if self.rejected_length == Some(buffer.len()) {
                return Err(std::io::Error::other("sink full"));
            }
            if let Some(remaining) = self.remaining.as_mut() {
                let written = (*remaining).min(buffer.len());
                self.bytes.extend_from_slice(&buffer[..written]);
                *remaining -= written;
                return Ok(written);
            }
            self.bytes.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    struct FailAfterPipeline {
        remaining: usize,
        bytes: Vec<u8>,
        finishes: usize,
    }

    impl Pipeline for FailAfterPipeline {
        fn identifier(&self) -> &str {
            "fail-after"
        }

        fn write(&mut self, buffer: &[u8]) -> PipelineResult<()> {
            let written = self.remaining.min(buffer.len());
            self.bytes.extend_from_slice(&buffer[..written]);
            self.remaining -= written;
            if written != buffer.len() {
                return Err(PipelineError::runtime("sink full"));
            }
            Ok(())
        }

        // cov:ignore-start: tests assert the raw writer leaves this caller-owned finish boundary untouched
        fn finish(&mut self) -> PipelineResult<()> {
            self.finishes += 1;
            Ok(())
        }
        // cov:ignore-end
    }

    struct FailOnParameters {
        bytes: Vec<u8>,
        category: ErrorCategory,
    }

    struct FailOnExactChunk {
        bytes: Vec<u8>,
        chunks: Vec<Vec<u8>>,
        fail_on: &'static [u8],
        category: ErrorCategory,
        finishes: usize,
    }

    #[derive(Clone, Copy)]
    enum ErrorCategory {
        Logic,
        Runtime,
    }

    impl Pipeline for FailOnParameters {
        fn identifier(&self) -> &str {
            "fail-on-parameters"
        }

        fn write(&mut self, buffer: &[u8]) -> PipelineResult<()> {
            if buffer.starts_with(b"\"parameters\"") {
                return Err(match self.category {
                    ErrorCategory::Logic => PipelineError::logic("raw writer logic failure"),
                    ErrorCategory::Runtime => PipelineError::runtime("raw writer runtime failure"),
                });
            }
            self.bytes.extend_from_slice(buffer);
            Ok(())
        }

        // cov:ignore-start: tests assert the raw writer leaves this caller-owned finish boundary untouched
        fn finish(&mut self) -> PipelineResult<()> {
            Ok(())
        }
        // cov:ignore-end
    }

    impl Pipeline for FailOnExactChunk {
        fn identifier(&self) -> &str {
            "fail-on-exact-chunk"
        }

        fn write(&mut self, buffer: &[u8]) -> PipelineResult<()> {
            self.chunks.push(buffer.to_vec());
            if buffer == self.fail_on {
                return Err(match self.category {
                    ErrorCategory::Logic => PipelineError::logic("raw key logic failure"),
                    ErrorCategory::Runtime => PipelineError::runtime("raw key runtime failure"),
                });
            }
            self.bytes.extend_from_slice(buffer);
            Ok(())
        }

        // cov:ignore-start: tests assert the raw writer leaves this caller-owned finish boundary untouched
        fn finish(&mut self) -> PipelineResult<()> {
            self.finishes += 1;
            Ok(())
        }
        // cov:ignore-end
    }

    const COMPLETE_SIDE_FILE_STREAM_JSON: &[u8] =
        b"{\n          \"datafile\": \"side-file\",\n          \"dict\": {}\n        }";

    fn project(value: Json) -> Result<serde_json::Value, ConvertError> {
        test_value(&value)
    }

    // Test-only convenience: lift a literal `Object` tree through a scratch,
    // otherwise-unused `Pdf` (so an `Object::Reference` still becomes a real
    // canonical `ObjectHandle`, exactly as `Pdf::lift_object_to_handle`'s own
    // production callers get one) and run it through the real
    // `pdf_object_to_json`. Keeps every existing test literal unchanged while
    // exercising the genuine `ObjectHandle`-native dispatch, including its
    // `object_ref()`-first reference handling.
    fn pdf_object_to_json(object: &Object) -> Result<serde_json::Value, ConvertError> {
        let mut pdf = empty_pdf();
        seed_detached_reference_targets(&mut pdf, object);
        let handle = pdf
            .lift_object_to_handle(object)
            .map_err(ConvertError::from)?;
        project(super::pdf_object_to_json(&handle)?)
    }

    // A qpdf dictionary checks each indirect child with isNull(), which
    // resolves it before writeJSON emits the child's identity. The selected
    // JSON tests compare a detached Object tree with a live document object;
    // install non-null placeholder targets in the scratch document so those
    // references exercise the same non-null branch instead of looking like
    // missing objects in the empty resolver.
    fn seed_detached_reference_targets<R: Read + Seek>(pdf: &mut Pdf<R>, object: &Object) {
        match object {
            Object::Reference(object_ref) => {
                pdf.set_object(*object_ref, Object::Integer(0));
            }
            Object::Array(items) => {
                for item in items {
                    seed_detached_reference_targets(pdf, item);
                }
            }
            Object::Dictionary(dictionary) => {
                for (_, item) in dictionary.iter() {
                    seed_detached_reference_targets(pdf, item);
                }
            }
            Object::Stream(stream) => {
                for (_, item) in stream.dict.iter() {
                    seed_detached_reference_targets(pdf, item);
                }
            }
            Object::Null
            | Object::Boolean(_)
            | Object::Integer(_)
            | Object::Real(_)
            | Object::RealLiteral { .. }
            | Object::Name(_)
            | Object::String(_)
            | Object::Operator(_)
            | Object::InlineImage(_) => {}
        }
    }

    // Serialize one PDF object the way the raw object map does, then re-read it
    // with serde_json so tests can assert on structure. Emitted key order is
    // not preserved by this projection; order is pinned by byte-level tests.
    fn qpdf_object_projection<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        object: &Object,
    ) -> Result<serde_json::Value, ConvertError> {
        let ordered = ordered_qpdf_object(pdf, object)?;
        let mut bytes = Vec::new();
        {
            let mut out = PlString::new("object", None, &mut bytes);
            ordered
                .write(&mut out, 0)
                .expect("a string sink accepts every write");
        }
        Ok(serde_json::from_slice(&bytes).expect("the raw object projection must be valid JSON"))
    }

    // The `qpdf` key's value as the production writer emits it, re-read with
    // serde_json. Metadata comes from the document itself, so tests see the
    // same values a caller would.
    fn qpdf_key_value<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        decode_level: DecodeLevel,
        stream_mode: &StreamDataMode,
    ) -> serde_json::Value {
        let mut bytes = Vec::new();
        {
            let mut out = PlString::new("qpdf key", None, &mut bytes);
            crate::document_json::write_json(pdf, 2, &mut out, decode_level, stream_mode, &[])
                .expect("the qpdf key must be written");
        }
        let document: serde_json::Value =
            serde_json::from_slice(&bytes).expect("qpdf JSON v2 output must be valid JSON");
        document["qpdf"].clone()
    }

    fn build_pages_section<R: Read + Seek>(
        pdf: &mut Pdf<R>,
    ) -> Result<serde_json::Value, ConvertError> {
        project(super::build_pages_section(pdf)?)
    }

    fn build_pagelabels_section<R: Read + Seek>(
        pdf: &mut Pdf<R>,
    ) -> Result<serde_json::Value, ConvertError> {
        project(super::build_pagelabels_section(pdf)?)
    }

    fn build_outlines_section<R: Read + Seek>(
        pdf: &mut Pdf<R>,
    ) -> Result<serde_json::Value, ConvertError> {
        project(super::build_outlines_section(pdf)?)
    }

    fn build_acroform_section<R: Read + Seek>(
        pdf: &mut Pdf<R>,
    ) -> Result<serde_json::Value, ConvertError> {
        project(crate::job::build_acroform_section(pdf)?)
    }

    fn build_attachments_section<R: Read + Seek>(
        pdf: &mut Pdf<R>,
    ) -> Result<serde_json::Value, ConvertError> {
        project(super::build_attachments_section(pdf)?)
    }

    fn build_encrypt_section<R: Read + Seek>(
        pdf: &mut Pdf<R>,
    ) -> Result<serde_json::Value, ConvertError> {
        project(super::build_encrypt_section(pdf)?)
    }

    fn write_selected_to_vec<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        decode_level: DecodeLevel,
        stream_mode: &StreamDataMode,
        keys: &[JsonKey],
        objects: &[JsonObjectSelector],
    ) -> Result<Vec<u8>, JsonOutputError> {
        let mut bytes = Vec::new();
        {
            let mut output = PlString::new("json test output", None, &mut bytes);
            write_qpdf_json_v2_selected_objects_with_options(
                pdf,
                decode_level,
                stream_mode,
                keys,
                objects,
                &mut output,
            )?;
        }
        Ok(bytes)
    }

    fn pdf_with_selected_string(payload_length: usize) -> Pdf<std::io::Cursor<Vec<u8>>> {
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(2, 0),
            Object::String(vec![b'x'; payload_length]),
        );
        pdf
    }

    fn selected_string_output(payload_length: usize) -> Vec<u8> {
        let mut pdf = pdf_with_selected_string(payload_length);
        write_selected_to_vec(
            &mut pdf,
            DecodeLevel::Generalized,
            &StreamDataMode::None,
            &[JsonKey::Qpdf],
            &[JsonObjectSelector::Object {
                number: 2,
                generation: 0,
            }],
        )
        .unwrap()
    }

    fn build_test_document_selected_objects<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        decode_level: DecodeLevel,
        stream_mode: &StreamDataMode,
        keys: &[JsonKey],
        objects: &[JsonObjectSelector],
    ) -> Result<serde_json::Value, String> {
        let out = write_selected_to_vec(pdf, decode_level, stream_mode, keys, objects)
            .map_err(|error| error.to_string())?;
        serde_json::from_slice(&out).map_err(|error| error.to_string())
    }

    fn build_test_document_selected<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        decode_level: DecodeLevel,
        stream_mode: &StreamDataMode,
        keys: &[JsonKey],
    ) -> Result<serde_json::Value, String> {
        build_test_document_selected_objects(pdf, decode_level, stream_mode, keys, &[])
    }

    fn build_test_document_with_options<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        decode_level: DecodeLevel,
        stream_mode: &StreamDataMode,
    ) -> Result<serde_json::Value, String> {
        build_test_document_selected(pdf, decode_level, stream_mode, &[])
    }

    fn build_test_document<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        decode_level: DecodeLevel,
    ) -> Result<serde_json::Value, String> {
        build_test_document_with_options(pdf, decode_level, &StreamDataMode::None)
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

    fn no_root_pdf() -> Pdf<std::io::Cursor<Vec<u8>>> {
        use std::io::Cursor;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"%PDF-1.4\n");
        let xref = bytes.len() as u64;
        bytes.extend_from_slice(
            format!(
                "xref\n0 1\n0000000000 65535 f \ntrailer\n<< /Size 1 >>\nstartxref\n{xref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        Pdf::open(Cursor::new(bytes)).expect("open PDF without /Root")
    }

    fn escaped_raw_dictionary_names_pdf() -> Pdf<std::io::Cursor<Vec<u8>>> {
        use std::io::Cursor;
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let off1 = bytes.len();
        bytes.extend_from_slice(
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /#22 1 /A 2 /Nested << /#22 3 /A 4 >> >>\nendobj\n",
        );
        let off2 = bytes.len();
        bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let off3 = bytes.len();
        bytes.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n",
        );
        let off4 = bytes.len();
        bytes.extend_from_slice(
            b"4 0 obj\n<< /Length 0 /#22 5 /A 6 /Nested << /#22 7 /A 8 >> >>\nstream\n\nendstream\nendobj\n",
        );
        let xref = bytes.len();
        bytes.extend_from_slice(
            format!(
                "xref\n0 5\n\
                 0000000000 65535 f \n\
                 {off1:010} 00000 n \n\
                 {off2:010} 00000 n \n\
                 {off3:010} 00000 n \n\
                 {off4:010} 00000 n \n\
                 trailer\n<< /Size 5 /Root 1 0 R /#22 9 /A 10 >>\n\
                 startxref\n{xref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        Pdf::open(Cursor::new(bytes)).expect("open raw-name-order PDF")
    }

    fn positions(haystack: &[u8], needle: &[u8]) -> Vec<usize> {
        haystack
            .windows(needle.len())
            .enumerate()
            .filter_map(|(position, window)| (window == needle).then_some(position))
            .collect()
    }

    #[test]
    fn coordinator_stdout_matches_raw_pipeline_bytes() {
        let mut expected_pdf = load_one_page_pdf();
        let expected = write_selected_to_vec(
            &mut expected_pdf,
            DecodeLevel::Generalized,
            &StreamDataMode::None,
            &[JsonKey::Pages],
            &[],
        )
        .unwrap();
        let mut actual_pdf = load_one_page_pdf();
        let mut actual = Vec::new();

        write_qpdf_json_v2_selected_objects_to_output_with_options(
            &mut actual_pdf,
            DecodeLevel::Generalized,
            &StreamDataMode::None,
            &[JsonKey::Pages],
            &[],
            JsonOutput::Stdout(&mut actual),
        )
        .unwrap();

        assert_eq!(actual, expected);
    }

    #[test]
    fn coordinator_file_matches_raw_pipeline_bytes() {
        let mut expected_pdf = load_one_page_pdf();
        let expected = write_selected_to_vec(
            &mut expected_pdf,
            DecodeLevel::Generalized,
            &StreamDataMode::None,
            &[JsonKey::Pages],
            &[],
        )
        .unwrap();
        let mut actual_pdf = load_one_page_pdf();
        let mut actual = Vec::new();

        write_qpdf_json_v2_selected_objects_to_output_with_options(
            &mut actual_pdf,
            DecodeLevel::Generalized,
            &StreamDataMode::None,
            &[JsonKey::Pages],
            &[],
            JsonOutput::File(&mut actual),
        )
        .unwrap();

        assert_eq!(actual, expected);
    }

    #[test]
    fn coordinator_stdout_still_uses_ostream_finish() {
        let mut pdf = load_one_page_pdf();
        let mut output = FlushProbe::default();

        write_qpdf_json_v2_selected_objects_to_output_with_options(
            &mut pdf,
            DecodeLevel::Generalized,
            &StreamDataMode::None,
            &[JsonKey::Pages],
            &[],
            JsonOutput::Stdout(&mut output),
        )
        .unwrap();

        assert_eq!(output.flushes, 1);
    }

    #[test]
    fn coordinator_file_does_not_call_pipeline_finish() {
        let mut pdf = load_one_page_pdf();
        let mut output = FlushProbe::default();

        write_qpdf_json_v2_selected_objects_to_output_with_options(
            &mut pdf,
            DecodeLevel::Generalized,
            &StreamDataMode::None,
            &[JsonKey::Pages],
            &[],
            JsonOutput::File(&mut output),
        )
        .unwrap();

        assert_eq!(output.flushes, 0);
    }

    #[test]
    fn coordinator_file_batches_inline_stream_output() {
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(2, 0),
            Object::Stream(Stream::new(Dictionary::new(), vec![b'x'; 12 * 1024])),
        );
        let mut output = FlushProbe::default();

        write_qpdf_json_v2_selected_objects_to_output_with_options(
            &mut pdf,
            DecodeLevel::None,
            &StreamDataMode::Inline,
            &[JsonKey::Qpdf],
            &[JsonObjectSelector::Object {
                number: 2,
                generation: 0,
            }],
            JsonOutput::File(&mut output),
        )
        .unwrap();

        let write_count = output.write_lengths.len();
        assert!(
            write_count <= 6,
            "expected buffered file writes, got {write_count} calls"
        );
        serde_json::from_slice::<serde_json::Value>(&output.bytes).unwrap();
    }

    #[test]
    fn coordinator_file_buffered_drop_does_not_retry_short_write() {
        let empty_output = selected_string_output(0);
        let payload_length = 4095usize
            .checked_sub(empty_output.len())
            .expect("selected-string JSON envelope must fit in 4095 bytes");
        let expected = selected_string_output(payload_length);
        assert_eq!(expected.len(), 4095);

        let mut pdf = pdf_with_selected_string(payload_length);
        let mut output = FlushProbe {
            remaining: Some(24),
            ..FlushProbe::default()
        };

        write_qpdf_json_v2_selected_objects_to_output_with_options(
            &mut pdf,
            DecodeLevel::Generalized,
            &StreamDataMode::None,
            &[JsonKey::Qpdf],
            &[JsonObjectSelector::Object {
                number: 2,
                generation: 0,
            }],
            JsonOutput::File(&mut output),
        )
        .unwrap();

        assert_eq!(output.bytes, expected[..24]);
        assert_eq!(output.write_lengths, [4095]);
        assert_eq!(output.flushes, 0);
    }

    #[test]
    fn coordinator_file_buffered_drop_does_not_retry_interrupted_write() {
        let empty_output = selected_string_output(0);
        let payload_length = 4095usize
            .checked_sub(empty_output.len())
            .expect("selected-string JSON envelope must fit in 4095 bytes");
        assert_eq!(selected_string_output(payload_length).len(), 4095);

        let mut pdf = pdf_with_selected_string(payload_length);
        let mut output = SingleAttemptInterruptedWriter::default();

        write_qpdf_json_v2_selected_objects_to_output_with_options(
            &mut pdf,
            DecodeLevel::Generalized,
            &StreamDataMode::None,
            &[JsonKey::Qpdf],
            &[JsonObjectSelector::Object {
                number: 2,
                generation: 0,
            }],
            JsonOutput::File(&mut output),
        )
        .unwrap();

        assert_eq!(output.write_lengths, [4095]);
    }

    #[test]
    fn coordinator_file_4096_write_failure_is_pipeline_runtime_error() {
        let mut pdf = pdf_with_selected_string(4095);
        let mut output = FlushProbe {
            rejected_length: Some(4096),
            ..FlushProbe::default()
        };

        let error = write_qpdf_json_v2_selected_objects_to_output_with_options(
            &mut pdf,
            DecodeLevel::Generalized,
            &StreamDataMode::None,
            &[JsonKey::Qpdf],
            &[JsonObjectSelector::Object {
                number: 2,
                generation: 0,
            }],
            JsonOutput::File(&mut output),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            JsonOutputError::Pipeline(PipelineError::Runtime(ref message))
                if message.as_bytes().starts_with(b"json output: Pl_StdioFile::write: ")
        ));
        assert_eq!(output.flushes, 0);
    }

    #[test]
    fn coordinator_ostream_failure_is_nonfatal_and_preserves_prefix() {
        let mut expected_pdf = load_one_page_pdf();
        let expected = write_selected_to_vec(
            &mut expected_pdf,
            DecodeLevel::Generalized,
            &StreamDataMode::None,
            &[JsonKey::Pages],
            &[],
        )
        .unwrap();
        let mut pdf = load_one_page_pdf();
        let mut output = FailAfterWriter {
            remaining: 24,
            bytes: Vec::new(),
        };

        write_qpdf_json_v2_selected_objects_to_output_with_options(
            &mut pdf,
            DecodeLevel::Generalized,
            &StreamDataMode::None,
            &[JsonKey::Pages],
            &[],
            JsonOutput::Stdout(&mut output),
        )
        .unwrap();

        assert_eq!(output.bytes, expected[..24]);
    }

    #[test]
    fn selected_sink_writer_emits_envelope_then_selected_section() {
        let mut pdf = load_one_page_pdf();
        let out = write_selected_to_vec(
            &mut pdf,
            DecodeLevel::Generalized,
            &StreamDataMode::None,
            &[JsonKey::Pages],
            &[],
        )
        .unwrap();

        assert!(serde_json::from_slice::<serde_json::Value>(&out).is_ok());
        let version = out
            .windows(b"\"version\"".len())
            .position(|window| window == b"\"version\"")
            .unwrap();
        let parameters = out
            .windows(b"\"parameters\"".len())
            .position(|window| window == b"\"parameters\"")
            .unwrap();
        let pages = out
            .windows(b"\"pages\"".len())
            .position(|window| window == b"\"pages\"")
            .unwrap();
        assert!(version < parameters && parameters < pages);
    }

    #[test]
    fn sink_writer_preserves_qpdf_metadata_field_order() {
        let mut pdf = load_one_page_pdf();
        let out = write_selected_to_vec(
            &mut pdf,
            DecodeLevel::Generalized,
            &StreamDataMode::None,
            &[JsonKey::Qpdf],
            &[JsonObjectSelector::Trailer],
        )
        .unwrap();

        let positions = [
            b"\"jsonversion\"".as_slice(),
            b"\"pdfversion\"".as_slice(),
            b"\"pushedinheritedpageresources\"".as_slice(),
            b"\"calledgetallpages\"".as_slice(),
            b"\"maxobjectid\"".as_slice(),
        ]
        .map(|needle| {
            out.windows(needle.len())
                .position(|window| window == needle)
                .unwrap()
        });
        assert!(
            positions.windows(2).all(|pair| pair[0] < pair[1]),
            "{positions:?}"
        );
    }

    #[test]
    fn sink_writer_keeps_missing_selector_object_map_expanded() {
        let mut pdf = load_one_page_pdf();
        let out = write_selected_to_vec(
            &mut pdf,
            DecodeLevel::Generalized,
            &StreamDataMode::None,
            &[JsonKey::Qpdf],
            &[JsonObjectSelector::Object {
                number: 999,
                generation: 0,
            }],
        )
        .unwrap();

        assert_eq!(
            out,
            b"{\n  \"version\": 2,\n  \"parameters\": {\n    \"decodelevel\": \"generalized\"\n  },\n  \"qpdf\": [\n    {\n      \"jsonversion\": 2,\n      \"pdfversion\": \"1.3\",\n      \"pushedinheritedpageresources\": false,\n      \"calledgetallpages\": false,\n      \"maxobjectid\": 7\n    },\n    {\n    }\n  ]\n}\n"
        );
    }

    #[test]
    fn sink_writer_orders_raw_stream_and_trailer_names_before_escaping_in_all_modes() {
        let temp = tempfile::tempdir().unwrap();
        let prefix = temp.path().join("side").to_string_lossy().into_owned();
        for stream_mode in [
            StreamDataMode::None,
            StreamDataMode::Inline,
            StreamDataMode::File {
                prefix: prefix.clone(),
            },
        ] {
            let mut pdf = escaped_raw_dictionary_names_pdf();
            let out = write_selected_to_vec(
                &mut pdf,
                DecodeLevel::Generalized,
                &stream_mode,
                &[JsonKey::Qpdf],
                &[
                    JsonObjectSelector::Object {
                        number: 4,
                        generation: 0,
                    },
                    JsonObjectSelector::Trailer,
                ],
            )
            .unwrap();

            let quote_keys = positions(&out, br#""/\"""#);
            let a_keys = positions(&out, br#""/A""#);
            assert_eq!(quote_keys.len(), 3, "{stream_mode:?}: {out:?}");
            assert_eq!(a_keys.len(), 3, "{stream_mode:?}: {out:?}");
            assert!(
                quote_keys
                    .iter()
                    .zip(a_keys.iter())
                    .all(|(quote, a)| quote < a),
                "{stream_mode:?}: quote={quote_keys:?}, A={a_keys:?}"
            );
        }
    }

    #[test]
    fn ordered_qpdf_object_preserves_nested_stream_shape_and_raw_dictionary_order() {
        let mut dict = Dictionary::new();
        dict.insert(b"\"", Object::Integer(1));
        dict.insert(b"A", Object::Integer(2));
        let object = Object::Array(vec![Object::Stream(Stream::new(dict, Vec::new()))]);
        let mut pdf = empty_pdf();

        let ordered = super::ordered_qpdf_object(&mut pdf, &object).unwrap();
        let out = {
            let mut bytes = Vec::new();
            {
                let mut output = PlString::new("ordered qpdf object", None, &mut bytes);
                ordered.write(&mut output, 0).unwrap();
            }
            bytes
        };

        assert_eq!(
            out,
            b"[\n  {\n    \"stream\": {\n      \"dict\": {\n        \"/\\\"\": 1,\n        \"/A\": 2\n      }\n    }\n  }\n]"
        );
    }

    #[test]
    fn ordered_qpdf_raw_container_end_and_empty_chunks_match_writer_literals() {
        let mut pdf = empty_pdf();
        let nonempty =
            super::ordered_qpdf_object(&mut pdf, &Object::Array(vec![Object::Null])).unwrap();
        let mut out = FailOnExactChunk {
            bytes: Vec::new(),
            chunks: Vec::new(),
            fail_on: b"not a qpdf chunk",
            category: ErrorCategory::Runtime,
            finishes: 0,
        };

        nonempty.write(&mut out, 0).unwrap();

        assert_eq!(
            out.chunks,
            [
                b"[".to_vec(),
                b"\n  ".to_vec(),
                b"null".to_vec(),
                b"\n".to_vec(),
                b"]".to_vec(),
            ]
        );
        assert_eq!(out.bytes, b"[\n  null\n]");
        assert_eq!(out.finishes, 0);

        let empty = super::ordered_qpdf_object(&mut pdf, &Object::Array(Vec::new())).unwrap();
        let mut out = FailOnExactChunk {
            bytes: Vec::new(),
            chunks: Vec::new(),
            fail_on: b"not a qpdf chunk",
            category: ErrorCategory::Runtime,
            finishes: 0,
        };

        empty.write(&mut out, 0).unwrap();

        assert_eq!(out.chunks, [b"[".to_vec(), b"]".to_vec()]);
        assert_eq!(out.bytes, b"[]");
        assert_eq!(out.finishes, 0);
    }

    #[test]
    fn ordered_qpdf_raw_container_end_failure_keeps_layout_prefix_and_category() {
        for (category, expected_message) in [
            (ErrorCategory::Logic, "raw key logic failure"),
            (ErrorCategory::Runtime, "raw key runtime failure"),
        ] {
            let mut pdf = empty_pdf();
            let value =
                super::ordered_qpdf_object(&mut pdf, &Object::Array(vec![Object::Null])).unwrap();
            let mut out = FailOnExactChunk {
                bytes: Vec::new(),
                chunks: Vec::new(),
                fail_on: b"]",
                category,
                finishes: 0,
            };

            let error = value.write(&mut out, 0).unwrap_err();

            assert_eq!(error.to_string(), expected_message);
            match category {
                ErrorCategory::Logic => assert!(matches!(
                    error,
                    JsonOutputError::Pipeline(PipelineError::Logic(_))
                )),
                ErrorCategory::Runtime => assert!(matches!(
                    error,
                    JsonOutputError::Pipeline(PipelineError::Runtime(_))
                )),
            }
            assert_eq!(
                out.chunks,
                [
                    b"[".to_vec(),
                    b"\n  ".to_vec(),
                    b"null".to_vec(),
                    b"\n".to_vec(),
                    b"]".to_vec(),
                ]
            );
            assert_eq!(out.bytes, b"[\n  null\n");
            assert_eq!(out.finishes, 0);
        }
    }

    #[test]
    fn ordered_qpdf_raw_next_uses_fifty_space_blocks_at_reachable_depth() {
        const FIFTY_SPACES: &[u8; 50] = b"                                                  ";

        let mut object = Object::Array(vec![Object::Null, Object::Null]);
        for _ in 1..25 {
            object = Object::Array(vec![object]);
        }
        let mut pdf = empty_pdf();
        let value = super::ordered_qpdf_object(&mut pdf, &object).unwrap();
        let mut out = FailOnExactChunk {
            bytes: Vec::new(),
            chunks: Vec::new(),
            fail_on: b"not a qpdf chunk",
            category: ErrorCategory::Runtime,
            finishes: 0,
        };

        value.write(&mut out, 0).unwrap();

        let boundary = [
            b"[".to_vec(),
            b"\n".to_vec(),
            FIFTY_SPACES.to_vec(),
            b"null".to_vec(),
            b",\n".to_vec(),
            FIFTY_SPACES.to_vec(),
            b"null".to_vec(),
            b"\n                                                ".to_vec(),
            b"]".to_vec(),
        ];
        assert!(out
            .chunks
            .windows(boundary.len())
            .any(|window| window == boundary));
        assert_eq!(out.finishes, 0);
    }

    #[test]
    fn ordered_qpdf_raw_next_block_failure_keeps_reachable_depth_prefix() {
        const FIFTY_SPACES: &[u8; 50] = b"                                                  ";

        let mut object = Object::Array(vec![Object::Null]);
        for _ in 1..25 {
            object = Object::Array(vec![object]);
        }
        let mut pdf = empty_pdf();
        let value = super::ordered_qpdf_object(&mut pdf, &object).unwrap();
        let mut out = FailOnExactChunk {
            bytes: Vec::new(),
            chunks: Vec::new(),
            fail_on: FIFTY_SPACES,
            category: ErrorCategory::Runtime,
            finishes: 0,
        };

        let error = value.write(&mut out, 0).unwrap_err();

        assert!(matches!(
            error,
            JsonOutputError::Pipeline(PipelineError::Runtime(_))
        ));
        assert_eq!(error.to_string(), "raw key runtime failure");
        assert_eq!(out.chunks.last(), Some(&FIFTY_SPACES.to_vec()));
        assert!(out.bytes.ends_with(b"[\n"));
        assert_eq!(out.finishes, 0);
    }

    #[test]
    fn ordered_qpdf_pdf_scalar_chunks_match_type_specific_write_json_literals() {
        let cases = [
            (Object::Null, vec![b"null".to_vec()]),
            (Object::Boolean(true), vec![b"true".to_vec()]),
            (Object::Boolean(false), vec![b"false".to_vec()]),
            (Object::Integer(-42), vec![b"-42".to_vec()]),
            (Object::Real(1.5), vec![b"1.5".to_vec()]),
            (
                Object::RealLiteral {
                    value: 1.5,
                    literal: b"1.500".to_vec(),
                },
                vec![b"1.500".to_vec()],
            ),
            (
                Object::RealLiteral {
                    value: 0.4,
                    literal: b".400".to_vec(),
                },
                vec![b"0".to_vec(), b".400".to_vec()],
            ),
            (
                Object::RealLiteral {
                    value: -0.4,
                    literal: b"-.400".to_vec(),
                },
                vec![b"-0.".to_vec(), b"400".to_vec()],
            ),
            (
                Object::Name(b"Type".to_vec()),
                vec![b"\"".to_vec(), b"/Type".to_vec(), b"\"".to_vec()],
            ),
            (
                Object::Name(b"line\n".to_vec()),
                vec![b"\"".to_vec(), b"/line\\n".to_vec(), b"\"".to_vec()],
            ),
            (
                Object::Name(b"\\\"\x08\x0c\n\r\t\x01\x1f".to_vec()),
                vec![
                    b"\"".to_vec(),
                    b"/\\\\\\\"\\b\\f\\n\\r\\t\\u0001\\u001f".to_vec(),
                    b"\"".to_vec(),
                ],
            ),
            (
                Object::Name(vec![0xff]),
                vec![b"\"n:".to_vec(), b"/#ff".to_vec(), b"\"".to_vec()],
            ),
            (
                Object::String(b"hello".to_vec()),
                vec![b"\"u:".to_vec(), b"hello".to_vec(), b"\"".to_vec()],
            ),
            (
                Object::String(vec![0x01]),
                vec![b"\"b:".to_vec(), b"01".to_vec(), b"\"".to_vec()],
            ),
            (
                Object::Reference(crate::ObjectRef::new(2, 0)),
                vec![b"\"".to_vec(), b"2 0 R".to_vec(), b"\"".to_vec()],
            ),
            (
                Object::Reference(crate::ObjectRef::new(0, 0)),
                vec![b"null".to_vec()],
            ),
            (Object::Operator(b"cm".to_vec()), vec![b"null".to_vec()]),
            (
                Object::InlineImage(b"BI ID EI".to_vec()),
                vec![b"null".to_vec()],
            ),
        ];

        for (object, expected_chunks) in cases {
            let mut pdf = empty_pdf();
            let value = super::ordered_qpdf_object(&mut pdf, &object).unwrap();
            let mut out = FailOnExactChunk {
                bytes: Vec::new(),
                chunks: Vec::new(),
                fail_on: b"not a qpdf chunk",
                category: ErrorCategory::Runtime,
                finishes: 0,
            };

            value.write(&mut out, 0).unwrap();

            assert_eq!(out.chunks, expected_chunks, "object={object:?}");
            assert_eq!(out.finishes, 0, "object={object:?}");
        }
    }

    #[test]
    fn ordered_qpdf_non_finite_scalars_fail_during_raw_conversion() {
        for object in [
            Object::Real(f64::NAN),
            Object::RealLiteral {
                value: f64::NEG_INFINITY,
                literal: b"-inf".to_vec(),
            },
        ] {
            let mut pdf = empty_pdf();
            let error = super::ordered_qpdf_object(&mut pdf, &object).err();

            assert_eq!(
                error,
                Some(ConvertError::NonFiniteFloat),
                "object={object:?}"
            );
        }
    }

    #[test]
    fn ordered_qpdf_pdf_scalar_failure_keeps_type_prefix_category_and_no_finish() {
        let cases = [
            (
                Object::Name(b"Type".to_vec()),
                b"/Type".as_slice(),
                ErrorCategory::Logic,
                b"\"".as_slice(),
                "raw key logic failure",
            ),
            (
                Object::String(b"hello".to_vec()),
                b"hello".as_slice(),
                ErrorCategory::Runtime,
                b"\"u:".as_slice(),
                "raw key runtime failure",
            ),
            (
                Object::Reference(crate::ObjectRef::new(2, 0)),
                b"2 0 R".as_slice(),
                ErrorCategory::Logic,
                b"\"".as_slice(),
                "raw key logic failure",
            ),
            (
                Object::RealLiteral {
                    value: 0.4,
                    literal: b".400".to_vec(),
                },
                b".400".as_slice(),
                ErrorCategory::Runtime,
                b"0".as_slice(),
                "raw key runtime failure",
            ),
        ];

        for (object, fail_on, category, expected_prefix, expected_message) in cases {
            let mut pdf = empty_pdf();
            let value = super::ordered_qpdf_object(&mut pdf, &object).unwrap();
            let mut out = FailOnExactChunk {
                bytes: Vec::new(),
                chunks: Vec::new(),
                fail_on,
                category,
                finishes: 0,
            };

            let error = value.write(&mut out, 0).unwrap_err();

            assert_eq!(error.to_string(), expected_message, "object={object:?}");
            match category {
                ErrorCategory::Logic => assert!(matches!(
                    error,
                    JsonOutputError::Pipeline(PipelineError::Logic(_))
                )),
                ErrorCategory::Runtime => assert!(matches!(
                    error,
                    JsonOutputError::Pipeline(PipelineError::Runtime(_))
                )),
            }
            assert_eq!(out.bytes, expected_prefix, "object={object:?}");
            assert_eq!(out.finishes, 0, "object={object:?}");
        }
    }

    #[test]
    fn ordered_qpdf_dictionary_key_chunks_split_utf8_and_non_utf8_literals() {
        let mut dictionary = Dictionary::new();
        dictionary.insert(b"A", Object::Integer(1));
        dictionary.insert(b"line\n", Object::Integer(2));
        dictionary.insert(vec![0xff], Object::Integer(3));
        let mut pdf = empty_pdf();
        let value = super::ordered_qpdf_object(&mut pdf, &Object::Dictionary(dictionary)).unwrap();
        let mut out = FailOnExactChunk {
            bytes: Vec::new(),
            chunks: Vec::new(),
            fail_on: b"not a qpdf chunk",
            category: ErrorCategory::Runtime,
            finishes: 0,
        };

        value.write(&mut out, 0).unwrap();

        assert_eq!(
            out.chunks,
            [
                b"{".to_vec(),
                b"\n  ".to_vec(),
                b"\"".to_vec(),
                b"/A".to_vec(),
                b"\": ".to_vec(),
                b"1".to_vec(),
                b",\n  ".to_vec(),
                b"\"".to_vec(),
                b"/line\\n".to_vec(),
                b"\": ".to_vec(),
                b"2".to_vec(),
                b",\n  ".to_vec(),
                b"\"n:".to_vec(),
                b"/#ff".to_vec(),
                b"\": ".to_vec(),
                b"3".to_vec(),
                b"\n".to_vec(),
                b"}".to_vec(),
            ]
        );
        assert_eq!(
            out.bytes,
            b"{\n  \"/A\": 1,\n  \"/line\\n\": 2,\n  \"n:/#ff\": 3\n}"
        );
        assert_eq!(out.finishes, 0);
    }

    #[test]
    fn ordered_qpdf_nested_dictionary_keys_use_qpdf_write_chunks() {
        let mut inner = Dictionary::new();
        inner.insert(b"\"", Object::Integer(1));
        inner.insert(b"A", Object::Integer(2));
        let mut outer = Dictionary::new();
        outer.insert(b"Nested", Object::Dictionary(inner));
        let mut pdf = empty_pdf();
        let ordered = super::ordered_qpdf_object(&mut pdf, &Object::Dictionary(outer)).unwrap();
        let mut out = FailOnExactChunk {
            bytes: Vec::new(),
            chunks: Vec::new(),
            fail_on: b"not a qpdf chunk",
            category: ErrorCategory::Runtime,
            finishes: 0,
        };
        assert_eq!(out.identifier(), "fail-on-exact-chunk");

        ordered.write(&mut out, 0).unwrap();

        assert_eq!(
            out.chunks,
            vec![
                b"{".to_vec(),
                b"\n  ".to_vec(),
                b"\"".to_vec(),
                b"/Nested".to_vec(),
                b"\": ".to_vec(),
                b"{".to_vec(),
                b"\n    ".to_vec(),
                b"\"".to_vec(),
                b"/\\\"".to_vec(),
                b"\": ".to_vec(),
                b"1".to_vec(),
                b",\n    ".to_vec(),
                b"\"".to_vec(),
                b"/A".to_vec(),
                b"\": ".to_vec(),
                b"2".to_vec(),
                b"\n  ".to_vec(),
                b"}".to_vec(),
                b"\n".to_vec(),
                b"}".to_vec(),
            ]
        );
        assert_eq!(
            out.bytes,
            b"{\n  \"/Nested\": {\n    \"/\\\"\": 1,\n    \"/A\": 2\n  }\n}"
        );
        assert_eq!(out.finishes, 0);
    }

    #[test]
    fn ordered_qpdf_trailer_dictionary_keys_use_qpdf_write_chunks() {
        let mut pdf = empty_pdf();
        let trailer_dictionary = pdf.trailer().clone();
        let trailer = super::ordered_qpdf_dict(&mut pdf, &trailer_dictionary).unwrap();
        let mut out = FailOnExactChunk {
            bytes: Vec::new(),
            chunks: Vec::new(),
            fail_on: b"not a qpdf chunk",
            category: ErrorCategory::Runtime,
            finishes: 0,
        };

        trailer.write(&mut out, 4).unwrap();

        assert_eq!(
            out.chunks,
            [
                b"{".to_vec(),
                b"\n          ".to_vec(),
                b"\"".to_vec(),
                b"/Root".to_vec(),
                b"\": ".to_vec(),
                b"\"".to_vec(),
                b"1 0 R".to_vec(),
                b"\"".to_vec(),
                b",\n          ".to_vec(),
                b"\"".to_vec(),
                b"/Size".to_vec(),
                b"\": ".to_vec(),
                b"2".to_vec(),
                b"\n        ".to_vec(),
                b"}".to_vec(),
            ]
        );
        assert_eq!(
            out.bytes,
            b"{\n          \"/Root\": \"1 0 R\",\n          \"/Size\": 2\n        }"
        );
        assert_eq!(out.finishes, 0);
    }

    #[test]
    fn ordered_qpdf_key_suffix_error_keeps_exact_prefix_and_category() {
        for (category, expected_message) in [
            (ErrorCategory::Logic, "raw key logic failure"),
            (ErrorCategory::Runtime, "raw key runtime failure"),
        ] {
            let mut dictionary = Dictionary::new();
            dictionary.insert(b"line\n", Object::Integer(1));
            let mut pdf = empty_pdf();
            let value =
                super::ordered_qpdf_object(&mut pdf, &Object::Dictionary(dictionary)).unwrap();
            let mut out = FailOnExactChunk {
                bytes: Vec::new(),
                chunks: Vec::new(),
                fail_on: b"\": ",
                category,
                finishes: 0,
            };

            let error = value.write(&mut out, 0).unwrap_err();

            assert_eq!(error.to_string(), expected_message);
            match category {
                ErrorCategory::Logic => assert!(matches!(
                    error,
                    JsonOutputError::Pipeline(PipelineError::Logic(_))
                )),
                ErrorCategory::Runtime => assert!(matches!(
                    error,
                    JsonOutputError::Pipeline(PipelineError::Runtime(_))
                )),
            }
            assert_eq!(
                out.chunks,
                [
                    b"{".to_vec(),
                    b"\n  ".to_vec(),
                    b"\"".to_vec(),
                    b"/line\\n".to_vec(),
                    b"\": ".to_vec(),
                ]
            );
            assert_eq!(out.bytes, b"{\n  \"/line\\n");
            assert_eq!(out.finishes, 0);
        }
    }

    #[test]
    fn tree_qpdf_projection_keeps_invalid_references_null() {
        let mut pdf = empty_pdf();
        assert_eq!(
            qpdf_object_projection(&mut pdf, &Object::Reference(crate::ObjectRef::new(0, 0)))
                .unwrap(),
            serde_json::Value::Null
        );
    }

    #[test]
    fn qpdf_projection_maps_content_only_tokens_to_null_without_lifting() {
        // Object::Operator/InlineImage have no ObjectValue representation;
        // qpdf_object_projection maps both to null directly, matching the
        // legacy JSON projection without routing through a handle lift.
        let mut pdf = empty_pdf();
        for object in [
            Object::Operator(b"cm".to_vec()),
            Object::InlineImage(b"\x00EI\xff".to_vec()),
        ] {
            assert_eq!(
                qpdf_object_projection(&mut pdf, &object).unwrap(),
                serde_json::Value::Null
            );
        }
    }

    #[test]
    fn side_file_success_writes_exact_payload_and_complete_json() {
        let mut pdf = empty_pdf();
        pdf.set_object(
            crate::ObjectRef::new(7, 0),
            Object::Stream(Stream::new(Dictionary::new(), b"payload".to_vec())),
        );
        let temp = tempfile::tempdir().unwrap();
        let prefix = temp.path().join("side").to_string_lossy().into_owned();
        let out = write_selected_to_vec(
            &mut pdf,
            DecodeLevel::None,
            &StreamDataMode::File {
                prefix: prefix.clone(),
            },
            &[JsonKey::Qpdf],
            &[JsonObjectSelector::Object {
                number: 7,
                generation: 0,
            }],
        )
        .unwrap();

        let side_path = format_json_side_file_path(&prefix, 7);
        assert_eq!(std::fs::read(&side_path).unwrap(), b"payload");
        assert!(out.ends_with(b"}\n"));
        assert!(!out.ends_with(b"}\n\n"));
        let document: serde_json::Value = serde_json::from_slice(&out).unwrap();
        let objects = document["qpdf"][1].as_object().unwrap();
        assert_eq!(
            objects.keys().map(String::as_str).collect::<Vec<_>>(),
            ["obj:7 0 R"]
        );
        assert_eq!(
            objects["obj:7 0 R"]["stream"]["datafile"],
            serde_json::Value::String(side_path)
        );
        assert_eq!(
            objects["obj:7 0 R"]["stream"]["dict"],
            serde_json::json!({})
        );
    }

    #[test]
    fn side_file_open_failure_keeps_main_json_prefix_and_path_context() {
        let mut pdf = empty_pdf();
        pdf.set_object(
            crate::ObjectRef::new(7, 0),
            Object::Stream(Stream::new(Dictionary::new(), b"payload".to_vec())),
        );
        let temp = tempfile::tempdir().unwrap();
        let prefix = temp
            .path()
            .join("missing")
            .join("side")
            .to_string_lossy()
            .into_owned();
        let side_path = format_json_side_file_path(&prefix, 7);
        let mut out = Vec::new();
        let error = {
            let mut output = PlString::new("file-mode main output", None, &mut out);
            write_qpdf_json_v2_selected_objects_with_options(
                &mut pdf,
                DecodeLevel::None,
                &StreamDataMode::File { prefix },
                &[JsonKey::Qpdf],
                &[JsonObjectSelector::Object {
                    number: 7,
                    generation: 0,
                }],
                &mut output,
            )
            .unwrap_err()
        };

        assert!(matches!(
            error,
            JsonOutputError::SideFileIo {
                operation: "open",
                ref path,
                ref message,
                ref source,
                ..
            } if path == &side_path
                && source.kind() == std::io::ErrorKind::NotFound
                && error.to_string() == format!("open {side_path}: {message}")
        ));
        assert!(out.ends_with(b"\"stream\": "), "{out:?}");
        assert!(
            out.windows(br#""obj:7 0 R""#.len())
                .any(|window| window == br#""obj:7 0 R""#),
            "{out:?}"
        );
    }

    #[test]
    fn file_mode_sink_failure_after_open_leaves_empty_side_file_before_stream_value() {
        let temp = tempfile::tempdir().unwrap();
        let reference_prefix = temp.path().join("reference").to_string_lossy().into_owned();
        let prefix = temp.path().join("actual___").to_string_lossy().into_owned();
        assert_eq!(reference_prefix.len(), prefix.len());

        let mut complete_pdf = load_one_page_pdf();
        let complete = write_selected_to_vec(
            &mut complete_pdf,
            DecodeLevel::None,
            &StreamDataMode::File {
                prefix: reference_prefix,
            },
            &[JsonKey::Qpdf],
            &[JsonObjectSelector::Object {
                number: 7,
                generation: 0,
            }],
        )
        .unwrap();
        let stream_key = b"\"stream\": ";
        let fail_after = complete
            .windows(stream_key.len())
            .position(|window| window == stream_key)
            .expect("successful file-mode JSON must contain the stream key")
            + stream_key.len();

        let side_path = format_json_side_file_path(&prefix, 7);
        let mut pdf = load_one_page_pdf();
        let mut out = FailAfterPipeline {
            remaining: fail_after,
            bytes: Vec::new(),
            finishes: 0,
        };
        assert_eq!(out.identifier(), "fail-after");
        let result = write_qpdf_json_v2_selected_objects_with_options(
            &mut pdf,
            DecodeLevel::None,
            &StreamDataMode::File { prefix },
            &[JsonKey::Qpdf],
            &[JsonObjectSelector::Object {
                number: 7,
                generation: 0,
            }],
            &mut out,
        );

        assert!(matches!(
            result,
            Err(JsonOutputError::Pipeline(PipelineError::Runtime(ref message)))
                if message.as_bytes() == b"sink full"
        ));
        assert!(out.bytes.ends_with(stream_key), "{:?}", out.bytes);
        assert_eq!(std::fs::read(&side_path).unwrap(), b"");
    }

    #[test]
    fn file_mode_sink_failure_during_datafile_leaves_empty_side_file() {
        let temp = tempfile::tempdir().unwrap();
        let reference_prefix = temp.path().join("reference").to_string_lossy().into_owned();
        let prefix = temp.path().join("actual___").to_string_lossy().into_owned();
        assert_eq!(reference_prefix.len(), prefix.len());

        let mut complete_pdf = load_one_page_pdf();
        let complete = write_selected_to_vec(
            &mut complete_pdf,
            DecodeLevel::None,
            &StreamDataMode::File {
                prefix: reference_prefix,
            },
            &[JsonKey::Qpdf],
            &[JsonObjectSelector::Object {
                number: 7,
                generation: 0,
            }],
        )
        .unwrap();
        let stream_key = b"\"stream\": ";
        let fail_after = complete
            .windows(stream_key.len())
            .position(|window| window == stream_key)
            .expect("successful file-mode JSON must contain the stream key")
            + stream_key.len()
            + 1;

        let side_path = format_json_side_file_path(&prefix, 7);
        let mut pdf = load_one_page_pdf();
        let mut out = FailAfterPipeline {
            remaining: fail_after,
            bytes: Vec::new(),
            finishes: 0,
        };
        let result = write_qpdf_json_v2_selected_objects_with_options(
            &mut pdf,
            DecodeLevel::None,
            &StreamDataMode::File { prefix },
            &[JsonKey::Qpdf],
            &[JsonObjectSelector::Object {
                number: 7,
                generation: 0,
            }],
            &mut out,
        );

        assert!(matches!(
            result,
            Err(JsonOutputError::Pipeline(PipelineError::Runtime(ref message)))
                if message.as_bytes() == b"sink full"
        ));
        assert!(out.bytes.ends_with(b"\"stream\": {"), "{:?}", out.bytes);
        assert_eq!(std::fs::read(&side_path).unwrap(), b"");
    }

    #[test]
    fn side_file_pipeline_write_failure_keeps_datafile_prefix() {
        let mut pdf = load_one_page_pdf();
        let stream = Stream::new(Dictionary::new(), vec![b'x'; 4096]);
        let handle = stream_handle(&mut pdf, stream);
        let mut side_file = FailAfterWriter {
            remaining: 0,
            bytes: Vec::new(),
        };
        let mut out = Vec::new();
        let result = {
            let mut output = PlString::new("file-mode main output", None, &mut out);
            write_json_stream_file(
                &handle,
                DecodeLevel::None,
                "side-file",
                &mut side_file,
                &mut output,
            )
        };

        let error = result.unwrap_err();
        assert_eq!(
            error.to_string(),
            "stream data: Pl_StdioFile::write: sink full"
        );
        assert!(matches!(
            error,
            JsonOutputError::Pipeline(PipelineError::Runtime(_))
        ));
        assert!(
            out.windows(br#""datafile": "side-file""#.len())
                .any(|window| window == br#""datafile": "side-file""#),
            "{out:?}"
        );
        assert!(
            !out.windows(br#""dict""#.len())
                .any(|window| window == br#""dict""#),
            "{out:?}"
        );
    }

    #[test]
    fn side_file_explicit_finish_ignores_enospc() {
        let mut pdf = empty_pdf();
        let stream = Stream::new(Dictionary::new(), b"small payload".to_vec());
        let handle = stream_handle(&mut pdf, stream);
        let mut side_file = FlushProbe {
            errno: Some(28),
            ..FlushProbe::default()
        };
        let trace = shared_trace();
        let mut out = RecordingSink::with_trace(trace.clone(), &[], &[]);

        write_json_stream_file(
            &handle,
            DecodeLevel::None,
            "side-file",
            &mut side_file,
            &mut out,
        )
        .unwrap();

        let trace = trace.borrow();
        assert_eq!(trace.output, COMPLETE_SIDE_FILE_STREAM_JSON);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&trace.output).unwrap(),
            serde_json::json!({
                "datafile": "side-file",
                "dict": {},
            })
        );
        assert!(!trace
            .calls
            .iter()
            .any(|call| matches!(call, TraceCall::Finish { .. })));
        assert_eq!(side_file.flushes, 0);
    }

    #[test]
    fn side_file_4097_byte_tail_failure_occurs_after_stream_dictionary() {
        let mut pdf = empty_pdf();
        let payload = vec![b'x'; 4097];
        let stream = Stream::new(Dictionary::new(), payload.clone());
        let handle = stream_handle(&mut pdf, stream);
        let mut side_file = FlushProbe {
            remaining: Some(4096),
            overflow_errno: Some(28),
            ..FlushProbe::default()
        };
        let mut out = Vec::new();
        {
            let mut output = PlString::new("file-mode main output", None, &mut out);
            write_json_stream_file(
                &handle,
                DecodeLevel::None,
                "side-file",
                &mut side_file,
                &mut output,
            )
            .unwrap();
        }

        assert_eq!(side_file.bytes, payload[..4096]);
        assert_eq!(side_file.write_lengths, [4096, 1]);
        assert_eq!(out, COMPLETE_SIDE_FILE_STREAM_JSON);
    }

    #[test]
    fn side_file_buffer_does_not_retry_interrupted_finish_write() {
        let mut pdf = empty_pdf();
        let stream = Stream::new(Dictionary::new(), vec![b'x'; 4095]);
        let handle = stream_handle(&mut pdf, stream);
        let mut side_file = SingleAttemptInterruptedWriter::default();
        let mut out = Vec::new();
        {
            let mut output = PlString::new("file-mode main output", None, &mut out);
            write_json_stream_file(
                &handle,
                DecodeLevel::None,
                "side-file",
                &mut side_file,
                &mut output,
            )
            .unwrap();
        }

        assert_eq!(side_file.write_lengths, [4095]);
        assert_eq!(out, COMPLETE_SIDE_FILE_STREAM_JSON);
    }

    #[test]
    fn side_file_explicit_finish_flushes_empty_payload() {
        let mut pdf = empty_pdf();
        let stream = Stream::new(Dictionary::new(), Vec::new());
        let handle = stream_handle(&mut pdf, stream);
        let mut side_file = FlushProbe::default();
        let mut out = Vec::new();
        {
            let mut output = PlString::new("file-mode main output", None, &mut out);
            write_json_stream_file(
                &handle,
                DecodeLevel::None,
                "side-file",
                &mut side_file,
                &mut output,
            )
            .unwrap();
        }

        assert!(side_file.bytes.is_empty());
        assert_eq!(side_file.flushes, 1);
        assert_eq!(out, COMPLETE_SIDE_FILE_STREAM_JSON);
    }

    #[test]
    fn side_file_explicit_finish_reports_ebadf_logic_error() {
        let mut pdf = empty_pdf();
        let stream = Stream::new(Dictionary::new(), b"small payload".to_vec());
        let handle = stream_handle(&mut pdf, stream);
        let mut side_file = FlushProbe {
            errno: Some(9),
            ..FlushProbe::default()
        };
        let trace = shared_trace();
        let mut out = RecordingSink::with_trace(trace.clone(), &[], &[]);

        let error = write_json_stream_file(
            &handle,
            DecodeLevel::None,
            "side-file",
            &mut side_file,
            &mut out,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            JsonOutputError::Pipeline(PipelineError::Logic(_))
        ));
        assert_eq!(
            error.to_string(),
            "stream data: Pl_StdioFile::finish: stream already closed"
        );
        let trace = trace.borrow();
        assert_eq!(trace.output, COMPLETE_SIDE_FILE_STREAM_JSON);
        assert!(!trace
            .calls
            .iter()
            .any(|call| matches!(call, TraceCall::Finish { .. })));
        assert_eq!(side_file.flushes, 0);
    }

    #[test]
    fn later_sink_failure_keeps_earlier_file_mode_payload() {
        let temp = tempfile::tempdir().unwrap();
        let reference_prefix = temp.path().join("reference").to_string_lossy().into_owned();
        let prefix = temp.path().join("actual___").to_string_lossy().into_owned();
        assert_eq!(reference_prefix.len(), prefix.len());
        let mut complete_pdf = load_one_page_pdf();
        complete_pdf.set_object(
            crate::ObjectRef::new(8, 0),
            Object::Stream(Stream::new(
                Dictionary::new(),
                b"second stream payload".to_vec(),
            )),
        );
        let complete = write_selected_to_vec(
            &mut complete_pdf,
            DecodeLevel::None,
            &StreamDataMode::File {
                prefix: reference_prefix,
            },
            &[JsonKey::Qpdf],
            &[
                JsonObjectSelector::Object {
                    number: 7,
                    generation: 0,
                },
                JsonObjectSelector::Object {
                    number: 8,
                    generation: 0,
                },
            ],
        )
        .unwrap();
        let second_object_marker = b"\"obj:8 0 R\"";
        let fail_after = complete
            .windows(second_object_marker.len())
            .position(|window| window == second_object_marker)
            .expect("successful two-stream JSON must contain the second object key")
            + second_object_marker.len();
        let first_path = format_json_side_file_path(&prefix, 7);
        let second_path = format_json_side_file_path(&prefix, 8);
        assert!(!std::path::Path::new(&first_path).exists());
        assert!(!std::path::Path::new(&second_path).exists());
        let mut expected_pdf = load_one_page_pdf();
        let expected_first_payload = qpdf_raw_stream_payload(
            &mut expected_pdf,
            crate::ObjectRef::new(7, 0),
            DecodeLevel::None,
        )
        .unwrap()
        .unwrap();

        let mut pdf = load_one_page_pdf();
        pdf.set_object(
            crate::ObjectRef::new(8, 0),
            Object::Stream(Stream::new(
                Dictionary::new(),
                b"second stream payload".to_vec(),
            )),
        );
        let mut out = FailAfterPipeline {
            remaining: fail_after,
            bytes: Vec::new(),
            finishes: 0,
        };
        let result = write_qpdf_json_v2_selected_objects_with_options(
            &mut pdf,
            DecodeLevel::None,
            &StreamDataMode::File {
                prefix: prefix.clone(),
            },
            &[JsonKey::Qpdf],
            &[
                JsonObjectSelector::Object {
                    number: 7,
                    generation: 0,
                },
                JsonObjectSelector::Object {
                    number: 8,
                    generation: 0,
                },
            ],
            &mut out,
        );

        assert!(matches!(
            result,
            Err(JsonOutputError::Pipeline(PipelineError::Runtime(ref message)))
                if message.as_bytes() == b"sink full"
        ));
        assert!(out
            .bytes
            .windows(second_object_marker.len())
            .any(|window| window == second_object_marker));
        assert!(!out.bytes.ends_with(b"}\n"));
        assert_eq!(std::fs::read(first_path).unwrap(), expected_first_payload);
        assert!(!std::path::Path::new(&second_path).exists());
    }

    #[test]
    fn file_mode_processes_multiple_payloads_incrementally() {
        let mut pdf = load_one_page_pdf();
        let second_ref = crate::ObjectRef::new(8, 0);
        let second_payload = b"second stream payload";
        pdf.set_object(
            second_ref,
            Object::Stream(Stream::new(Dictionary::new(), second_payload.to_vec())),
        );
        let mut expected_pdf = load_one_page_pdf();
        let expected_first_payload = qpdf_raw_stream_payload(
            &mut expected_pdf,
            crate::ObjectRef::new(7, 0),
            DecodeLevel::None,
        )
        .unwrap()
        .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let prefix = temp.path().join("side").to_string_lossy().into_owned();

        let out = write_selected_to_vec(
            &mut pdf,
            DecodeLevel::None,
            &StreamDataMode::File {
                prefix: prefix.clone(),
            },
            &[JsonKey::Qpdf],
            &[
                JsonObjectSelector::Object {
                    number: 7,
                    generation: 0,
                },
                JsonObjectSelector::Object {
                    number: 8,
                    generation: 0,
                },
            ],
        )
        .unwrap();

        let first_path = format_json_side_file_path(&prefix, 7);
        let second_path = format_json_side_file_path(&prefix, 8);
        assert_eq!(std::fs::read(&first_path).unwrap(), expected_first_payload);
        assert_eq!(std::fs::read(&second_path).unwrap(), second_payload);
        let document: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            document["qpdf"][1]["obj:7 0 R"]["stream"]["datafile"],
            serde_json::Value::String(first_path)
        );
        assert_eq!(
            document["qpdf"][1]["obj:8 0 R"]["stream"]["datafile"],
            serde_json::Value::String(second_path)
        );
    }

    #[test]
    fn selected_sink_writer_inlines_stream() {
        let mut pdf = load_one_page_pdf();
        let out = write_selected_to_vec(
            &mut pdf,
            DecodeLevel::None,
            &StreamDataMode::Inline,
            &[JsonKey::Qpdf],
            &[JsonObjectSelector::Object {
                number: 7,
                generation: 0,
            }],
        )
        .unwrap();

        let document: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert!(document["qpdf"][1]["obj:7 0 R"]["stream"]["data"]
            .as_str()
            .is_some_and(|data| !data.is_empty()));
    }

    #[test]
    fn full_sink_writer_emits_all_sections_in_qpdf_order() {
        let mut pdf = load_one_page_pdf();
        let out = write_selected_to_vec(
            &mut pdf,
            DecodeLevel::Generalized,
            &StreamDataMode::None,
            &[],
            &[],
        )
        .unwrap();

        let positions = [
            b"\n  \"version\"".as_slice(),
            b"\n  \"parameters\"".as_slice(),
            b"\n  \"pages\"".as_slice(),
            b"\n  \"pagelabels\"".as_slice(),
            b"\n  \"acroform\"".as_slice(),
            b"\n  \"attachments\"".as_slice(),
            b"\n  \"encrypt\"".as_slice(),
            b"\n  \"outlines\"".as_slice(),
            b"\n  \"qpdf\"".as_slice(),
        ]
        .map(|needle| {
            out.windows(needle.len())
                .position(|window| window == needle)
                .unwrap()
        });
        assert!(
            positions.windows(2).all(|pair| pair[0] < pair[1]),
            "{positions:?}"
        );
        let document: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            document["qpdf"][0]["calledgetallpages"],
            serde_json::Value::Bool(true)
        );
    }

    #[test]
    fn sink_writer_conversion_error_stops_before_selected_section_key() {
        let mut pdf = empty_pdf();
        let mut out = Vec::new();
        let error = {
            let mut output = PlString::new("conversion error output", None, &mut out);
            write_qpdf_json_v2_selected_objects_with_options(
                &mut pdf,
                DecodeLevel::Generalized,
                &StreamDataMode::None,
                &[JsonKey::Pages],
                &[],
                &mut output,
            )
            .unwrap_err()
        };

        assert!(matches!(
            error,
            JsonOutputError::Convert(ConvertError::PdfError(_))
        ));
        assert!(!out
            .windows(b"\"pages\"".len())
            .any(|window| window == b"\"pages\""));
    }

    #[test]
    fn selected_vector_writer_propagates_conversion_errors() {
        let mut pdf = empty_pdf();

        let error = write_selected_to_vec(
            &mut pdf,
            DecodeLevel::Generalized,
            &StreamDataMode::None,
            &[JsonKey::Pages],
            &[],
        )
        .unwrap_err();

        assert!(matches!(
            error,
            JsonOutputError::Convert(ConvertError::PdfError(_))
        ));
    }

    #[test]
    fn sink_writer_pipeline_error_stops_without_finishing_document() {
        let mut pdf = load_one_page_pdf();
        let mut out = FailAfterPipeline {
            remaining: 24,
            bytes: Vec::new(),
            finishes: 0,
        };
        let error = write_qpdf_json_v2_selected_objects_with_options(
            &mut pdf,
            DecodeLevel::Generalized,
            &StreamDataMode::None,
            &[JsonKey::Pages],
            &[],
            &mut out,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            JsonOutputError::Pipeline(PipelineError::Runtime(ref message))
                if message.as_bytes() == b"sink full"
        ));
        assert!(!out.bytes.ends_with(b"\n}\n"));
        assert_eq!(out.finishes, 0);
    }

    #[test]
    fn sink_writer_propagates_failures_at_every_incremental_write_boundary() {
        let mut complete_pdf = load_one_page_pdf();
        let complete = write_selected_to_vec(
            &mut complete_pdf,
            DecodeLevel::Generalized,
            &StreamDataMode::None,
            &[],
            &[],
        )
        .unwrap();

        for remaining in 0..complete.len() {
            let mut pdf = load_one_page_pdf();
            let mut out = FailAfterPipeline {
                remaining,
                bytes: Vec::new(),
                finishes: 0,
            };
            let error = write_qpdf_json_v2_selected_objects_with_options(
                &mut pdf,
                DecodeLevel::Generalized,
                &StreamDataMode::None,
                &[],
                &[],
                &mut out,
            )
            .unwrap_err();
            assert!(
                matches!(
                    error,
                    JsonOutputError::Pipeline(PipelineError::Runtime(ref message))
                        if message.as_bytes() == b"sink full"
                ),
                "remaining={remaining}: {error}"
            );
            assert_eq!(&out.bytes, &complete[..remaining]);
            assert_eq!(out.finishes, 0);
        }
    }

    #[test]
    fn raw_writer_does_not_finish_supplied_pipeline() {
        let mut pdf = load_one_page_pdf();
        let trace = shared_trace();
        let mut out = RecordingSink::with_trace(trace.clone(), &[], &[]);

        write_qpdf_json_v2_selected_objects_with_options(
            &mut pdf,
            DecodeLevel::Generalized,
            &StreamDataMode::None,
            &[JsonKey::Pages],
            &[],
            &mut out,
        )
        .unwrap();

        assert!(trace.borrow().output.ends_with(b"}\n"));
        assert!(!trace
            .borrow()
            .calls
            .iter()
            .any(|call| matches!(call, TraceCall::Finish { .. })));
    }

    #[test]
    fn raw_writer_retains_prefix_on_pipeline_runtime_error() {
        let mut pdf = load_one_page_pdf();
        let mut out = FailOnParameters {
            bytes: Vec::new(),
            category: ErrorCategory::Runtime,
        };
        assert_eq!(out.identifier(), "fail-on-parameters");

        let error = write_qpdf_json_v2_selected_objects_with_options(
            &mut pdf,
            DecodeLevel::Generalized,
            &StreamDataMode::None,
            &[],
            &[],
            &mut out,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            JsonOutputError::Pipeline(PipelineError::Runtime(ref message))
                if message.as_bytes() == b"raw writer runtime failure"
        ));
        assert_eq!(out.bytes, b"{\n  \"version\": 2,\n  ");
    }

    #[test]
    fn raw_writer_retains_pipeline_error_category() {
        for (category, expected_message) in [
            (ErrorCategory::Logic, "raw writer logic failure"),
            (ErrorCategory::Runtime, "raw writer runtime failure"),
        ] {
            let mut pdf = load_one_page_pdf();
            let mut out = FailOnParameters {
                bytes: Vec::new(),
                category,
            };

            let error = write_qpdf_json_v2_selected_objects_with_options(
                &mut pdf,
                DecodeLevel::Generalized,
                &StreamDataMode::None,
                &[],
                &[],
                &mut out,
            )
            .unwrap_err();

            assert_eq!(error.to_string(), expected_message);
            match category {
                ErrorCategory::Logic => assert!(matches!(
                    error,
                    JsonOutputError::Pipeline(PipelineError::Logic(_))
                )),
                ErrorCategory::Runtime => assert!(matches!(
                    error,
                    JsonOutputError::Pipeline(PipelineError::Runtime(_))
                )),
            }
            assert_eq!(out.bytes, b"{\n  \"version\": 2,\n  ");
        }
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
            serde_json::Value::Bool(true)
        );
    }

    #[test]
    fn object_bool_false_to_json() {
        assert_eq!(
            pdf_object_to_json(&Object::Boolean(false)).unwrap(),
            serde_json::Value::Bool(false)
        );
    }

    // ── 8. Integer conversion ─────────────────────────────────────────────────

    #[test]
    fn object_integer_to_json() {
        fn assert_json(_: &crate::json::Json) {}

        let converted = super::pdf_object_to_json(&ObjectHandle::integer(42)).unwrap();
        assert_json(&converted);
        assert_eq!(converted.get_number().as_deref(), Some(b"42".as_slice()));
    }

    #[test]
    fn object_handle_write_json_uses_qpdf_scalar_pipeline_without_finishing_it() {
        let trace = shared_trace();
        let mut output = RecordingSink::with_trace(trace.clone(), &[], &[]);

        ObjectHandle::integer(42)
            .write_json(2, &mut output, false, 0)
            .unwrap();

        assert_eq!(trace.borrow().output, b"42");
        assert_eq!(
            trace.borrow().calls,
            [TraceCall::Write {
                data: b"42".to_vec(),
                failed: false,
            }]
        );
    }

    #[test]
    fn object_handle_write_json_keeps_indirect_reference_before_value_dispatch() {
        let mut pdf = empty_pdf();
        let object_ref = ObjectRef::new(2, 0);
        pdf.set_object(object_ref, Object::Array(vec![Object::Integer(1)]));
        pdf.resolve(object_ref).unwrap();
        let handle = pdf.get_object_handle(object_ref);
        let mut bytes = Vec::new();
        let mut output = PlString::new("object-handle-json", None, &mut bytes);

        handle
            .write_json(2, &mut output, false, 0)
            .expect("resolved indirect handles still serialize as references");

        assert_eq!(bytes, b"\"2 0 R\"");
    }

    #[test]
    fn object_handle_write_json_uses_qpdf_nested_indentation_and_child_reference_rules() {
        let value = ObjectHandle::array(vec![
            ObjectHandle::integer(1),
            ObjectHandle::new_indirect_unresolved(ObjectRef::new(2, 0), 0),
        ]);
        let mut bytes = Vec::new();
        let mut output = PlString::new("object-handle-json", None, &mut bytes);

        value
            .write_json(2, &mut output, true, 0)
            .expect("nested object handles serialize");

        assert_eq!(bytes, b"[\n  1,\n  \"2 0 R\"\n]");
    }

    #[test]
    fn object_handle_write_json_does_not_count_stream_dispatch_as_extra_depth() {
        // QPDF_Stream::writeJSON delegates transparently to its dictionary.
        // Build 251 dictionary containers separated by stream dispatches:
        // qpdf's JSON parser accepts this, while counting each transparent
        // stream as another level would reject it at the 500-level writer cap.
        let mut nested = ObjectHandle::dictionary(vec![]);
        for _ in 0..250 {
            nested = ObjectHandle::stream(
                ObjectHandle::dictionary(vec![(b"Next".to_vec(), nested)]),
                Rc::new(Vec::new()),
            );
        }

        nested
            .get_json(2, false)
            .expect("transparent stream dispatches must not consume JSON depth");
    }

    #[test]
    fn object_handle_write_json_true_resolves_only_the_outer_indirect_handle() {
        let mut pdf = empty_pdf();
        let outer_ref = ObjectRef::new(8, 0);
        pdf.set_object(
            outer_ref,
            Object::Array(vec![
                Object::Reference(ObjectRef::new(3, 0)),
                Object::Name(b"Fit".to_vec()),
            ]),
        );
        let handle = pdf.get_object_handle(outer_ref);
        let mut bytes = Vec::new();
        let mut output = PlString::new("object-handle-json", None, &mut bytes);

        handle
            .write_json(2, &mut output, true, 0)
            .expect("the canonical resolver supplies the outer value");

        assert_eq!(bytes, b"[\n  \"3 0 R\",\n  \"/Fit\"\n]");
    }

    #[test]
    fn object_handle_write_json_reports_qpdf_uninitialized_error_for_true_mode() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), 0);
        let mut bytes = Vec::new();
        let mut output = PlString::new("object-handle-json", None, &mut bytes);

        let error = handle
            .write_json(2, &mut output, true, 0)
            .expect_err("an uninitialized handle cannot be dereferenced");

        assert_eq!(
            error.to_string(),
            "attempted to dereference an uninitialized QPDFObjectHandle"
        );
        assert!(bytes.is_empty(), "the error occurs before scalar output");
    }

    #[test]
    fn object_handle_write_json_reports_reserved_error_only_after_true_dispatch() {
        let pdf = empty_pdf();
        let handle = pdf.new_reserved().expect("reserved object construction");

        let mut reference_bytes = Vec::new();
        let mut reference_output = PlString::new("object-handle-json", None, &mut reference_bytes);
        handle
            .write_json(2, &mut reference_output, false, 0)
            .expect("non-dereferenced reserved handles retain their identity");
        assert_eq!(
            reference_bytes,
            format!("\"{}\"", handle.object_ref().unwrap()).as_bytes()
        );

        let mut value_bytes = Vec::new();
        let mut value_output = PlString::new("object-handle-json", None, &mut value_bytes);
        let error = handle
            .write_json(2, &mut value_output, true, 0)
            .expect_err("reserved values cannot be dispatched as JSON");
        assert_eq!(
            error.to_string(),
            "QPDFObjectHandle: attempting to get JSON from a reserved object"
        );
        assert!(value_bytes.is_empty());
    }

    #[test]
    fn object_handle_write_json_rejects_versions_other_than_qpdf_one_or_two() {
        let mut bytes = Vec::new();
        let mut output = PlString::new("object-handle-json", None, &mut bytes);

        let error = ObjectHandle::null()
            .write_json(3, &mut output, false, 0)
            .expect_err("qpdf only accepts JSON versions 1 and 2");

        assert!(matches!(error, ObjectJsonError::UnsupportedVersion(3)));
        assert!(bytes.is_empty());
    }

    #[test]
    fn object_handle_write_json_maps_outer_resolver_failure() {
        let (handle, _resolver) =
            crate::object_handle::identity_tests::error_resolving_handle(ObjectRef::new(12, 0));
        let mut bytes = Vec::new();
        let mut output = PlString::new("object-handle-json", None, &mut bytes);

        let error = handle
            .write_json(2, &mut output, true, 0)
            .expect_err("the resolver failure must reach the JSON boundary");

        assert!(matches!(error, ObjectJsonError::Pdf(message) if message == "resolver failed"));
        assert!(bytes.is_empty());
    }

    #[test]
    fn object_handle_write_json_scalar_chunks_match_qpdf_writer_boundaries() {
        let cases = [
            (ObjectHandle::null(), vec![b"null".to_vec()]),
            (ObjectHandle::boolean(true), vec![b"true".to_vec()]),
            (ObjectHandle::integer(-42), vec![b"-42".to_vec()]),
            (ObjectHandle::real(1.5), vec![b"1.5".to_vec()]),
            (
                ObjectHandle::real_literal(0.4, b".400".to_vec()),
                vec![b"0".to_vec(), b".400".to_vec()],
            ),
            (
                ObjectHandle::name(b"line\n".to_vec()),
                vec![b"\"".to_vec(), b"/line\\n".to_vec(), b"\"".to_vec()],
            ),
            (
                ObjectHandle::string(b"hello".to_vec()),
                vec![b"\"u:".to_vec(), b"hello".to_vec(), b"\"".to_vec()],
            ),
            (
                ObjectHandle::new_indirect_unresolved(ObjectRef::new(2, 0), 0),
                vec![b"\"".to_vec(), b"2 0".to_vec(), b" R\"".to_vec()],
            ),
            (
                ObjectHandle::dictionary(vec![(b"Plain".to_vec(), ObjectHandle::integer(1))]),
                vec![
                    b"{".to_vec(),
                    b"\n  ".to_vec(),
                    b"\"".to_vec(),
                    b"/Plain".to_vec(),
                    b"\": ".to_vec(),
                    b"1".to_vec(),
                    b"\n".to_vec(),
                    b"}".to_vec(),
                ],
            ),
        ];

        for (handle, expected_chunks) in cases {
            let mut output = FailOnExactChunk {
                bytes: Vec::new(),
                chunks: Vec::new(),
                fail_on: b"not a qpdf chunk",
                category: ErrorCategory::Runtime,
                finishes: 0,
            };
            handle
                .write_json(2, &mut output, false, 0)
                .expect("scalar writer accepts every chunk");
            assert_eq!(output.chunks, expected_chunks);
            assert_eq!(output.finishes, 0);
        }
    }

    #[test]
    fn object_handle_write_json_skips_a_dictionary_child_that_resolves_to_missing() {
        let mut pdf = empty_pdf();
        let missing = pdf.get_object_handle(ObjectRef::new(9, 0));
        let value = ObjectHandle::dictionary(vec![
            (b"/Missing".to_vec(), missing),
            (b"/Value".to_vec(), ObjectHandle::integer(1)),
        ]);
        let mut bytes = Vec::new();
        let mut output = PlString::new("object-handle-json", None, &mut bytes);

        value
            .write_json(2, &mut output, false, 0)
            .expect("missing dictionary values are qpdf null entries");

        assert_eq!(bytes, b"{\n  \"/Value\": 1\n}");
    }

    #[test]
    fn object_handle_write_json_reports_uninitialized_dictionary_children() {
        let value = ObjectHandle::dictionary(vec![(
            b"/Broken".to_vec(),
            ObjectHandle::new_indirect_unresolved(ObjectRef::new(13, 0), 0),
        )]);
        let mut bytes = Vec::new();
        let mut output = PlString::new("object-handle-json", None, &mut bytes);

        let error = value
            .write_json(2, &mut output, false, 0)
            .expect_err("qpdf resolves a dictionary child before isNull");

        assert!(matches!(error, ObjectJsonError::Uninitialized));
        assert_eq!(bytes, b"{");
    }

    #[test]
    fn object_handle_write_json_maps_dictionary_child_resolver_failure() {
        let (child, _resolver) =
            crate::object_handle::identity_tests::error_resolving_handle(ObjectRef::new(14, 0));
        let value = ObjectHandle::dictionary(vec![(b"/Broken".to_vec(), child)]);
        let mut bytes = Vec::new();
        let mut output = PlString::new("object-handle-json", None, &mut bytes);

        let error = value
            .write_json(2, &mut output, false, 0)
            .expect_err("the child resolver failure must reach the JSON boundary");

        assert!(matches!(error, ObjectJsonError::Pdf(message) if message == "resolver failed"));
        assert_eq!(bytes, b"{");
    }

    #[test]
    fn json_error_conversion_preserves_the_pdf_error_body_once() {
        let converted =
            super::convert_object_json_error(ObjectJsonError::Pdf("resolver failed".to_string()));
        assert_eq!(
            converted,
            ConvertError::PdfError("resolver failed".to_string())
        );
        assert_eq!(converted.to_string(), "PDF error: resolver failed");

        let output = JsonOutputError::from(ObjectJsonError::Pdf("resolver failed".to_string()));
        assert_eq!(output.to_string(), "PDF error: resolver failed");
    }

    #[test]
    fn object_handle_write_json_reports_destroyed_handles() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(15, 0), 0);
        handle.set_resolved(crate::object_handle::ObjectValue::Integer(1));
        handle.disconnect();

        let mut bytes = Vec::new();
        let mut output = PlString::new("object-handle-json", None, &mut bytes);
        let error = handle
            .write_json(2, &mut output, false, 0)
            .expect_err("a handle outliving its PDF is destroyed");

        assert!(matches!(error, ObjectJsonError::Destroyed));
        assert!(bytes.is_empty());
    }

    #[test]
    fn object_handle_write_json_matches_qpdf_v1_and_v2_string_name_forms() {
        let cases = [
            (
                ObjectHandle::string(b"hello".to_vec()),
                1,
                b"\"hello\"".as_slice(),
            ),
            (
                ObjectHandle::name(b"Fit".to_vec()),
                1,
                b"\"/Fit\"".as_slice(),
            ),
            (
                ObjectHandle::string(b"\xef\xbb\xbfhello".to_vec()),
                2,
                b"\"u:hello\"".as_slice(),
            ),
            (
                ObjectHandle::string(b"\xef\xbb\xbf\xff".to_vec()),
                2,
                b"\"b:efbbbfff\"".as_slice(),
            ),
            (
                ObjectHandle::string(b"     \x7f".to_vec()),
                2,
                b"\"b:20202020207f\"".as_slice(),
            ),
            (ObjectHandle::string(vec![0x08]), 2, b"\"u:\\b\"".as_slice()),
            (
                ObjectHandle::dictionary(vec![(b"Plain".to_vec(), ObjectHandle::integer(1))]),
                2,
                b"{\n  \"/Plain\": 1\n}".as_slice(),
            ),
        ];

        for (handle, version, expected) in cases {
            let mut bytes = Vec::new();
            let mut output = PlString::new("object-handle-json", None, &mut bytes);
            handle
                .write_json(version, &mut output, false, 0)
                .expect("the qpdf JSON form is serializable");
            assert_eq!(bytes, expected);
        }
    }

    // ── 9. Real (float) conversion ────────────────────────────────────────────

    #[test]
    fn object_real_to_json() {
        assert_eq!(pdf_object_to_json(&Object::Real(1.5)).unwrap(), number(1.5));
    }

    #[test]
    fn object_real_json_preserves_qpdf_number_tokens() {
        let cases = [
            (
                ObjectHandle::real_literal(0.4, b".400".to_vec()),
                b"0.400".as_slice(),
            ),
            (
                ObjectHandle::real_literal(-0.4, b"-.400".to_vec()),
                b"-0.400".as_slice(),
            ),
            (ObjectHandle::real(1.23456789), b"1.23456789".as_slice()),
            (ObjectHandle::real(-0.0), b"-0".as_slice()),
            (
                ObjectHandle::real_literal(1.0, b"1 true".to_vec()),
                b"1".as_slice(),
            ),
            (
                ObjectHandle::real_literal(2.0, b"1.0".to_vec()),
                b"2".as_slice(),
            ),
        ];

        for (handle, expected) in cases {
            let json = super::pdf_object_to_json(&handle).unwrap();
            assert_eq!(json.unparse().unwrap(), expected);
        }
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
        assert_eq!(
            pdf_object_to_json(&Object::RealLiteral {
                value: f64::NEG_INFINITY,
                literal: b"-inf".to_vec(),
            }),
            Err(ConvertError::NonFiniteFloat)
        );
    }

    #[test]
    fn convert_error_json_variant_has_specific_display() {
        let error = ConvertError::JsonError("container mismatch".to_string());
        assert_eq!(error.to_string(), "JSON error: container mismatch");
    }

    // ── 10. Name conversion ───────────────────────────────────────────────────

    #[test]
    fn object_name_to_json() {
        assert_eq!(
            pdf_object_to_json(&Object::Name(b"Type".to_vec())).unwrap(),
            serde_json::Value::String("/Type".to_string())
        );
        assert_eq!(
            pdf_object_to_json(&Object::Name(b"Font".to_vec())).unwrap(),
            serde_json::Value::String("/Font".to_string())
        );
    }

    #[test]
    fn object_name_json_preserves_a_slash_in_the_decoded_name_body() {
        assert_eq!(
            pdf_object_to_json(&Object::Name(b"/leading".to_vec())).unwrap(),
            serde_json::Value::String("//leading".to_string())
        );
    }

    #[test]
    fn object_string_ascii_text_has_u_prefix() {
        let result = pdf_object_to_json(&Object::String(b"hello".to_vec())).unwrap();
        assert_eq!(result, serde_json::Value::String("u:hello".to_string()));
    }

    #[test]
    fn object_string_binary_has_b_prefix() {
        // 0x01 is unassigned in PDFDocEncoding (no UTF-16 BOM either), so the
        // string is not decodable as PDF text and must fall back to "b:" hex.
        let result = pdf_object_to_json(&Object::String(vec![0x2d, 0x01, 0x80])).unwrap();
        assert_eq!(result, serde_json::Value::String("b:2d0180".to_string()));
    }

    #[test]
    fn object_string_pdfdoc_high_byte_too_dense_falls_back_to_binary() {
        // 0xC7 ("Ç" in PDFDocEncoding) counts as non-ASCII under qpdf's
        // useHexString() heuristic. With non_ascii=1 and len=3, 5*1 > 3 so
        // qpdf emits b:<hex>; flpdf matches that.
        let result = pdf_object_to_json(&Object::String(vec![b'A', 0xC7, b'B'])).unwrap();
        assert_eq!(result, serde_json::Value::String("b:41c742".to_string()));
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
            serde_json::Value::String("u:the quick\u{00C7}brown!!".to_string())
        );
    }

    #[test]
    fn object_string_utf16be_bom_decodes_to_unicode() {
        // FEFF + 0041 + 0042 → "u:AB"
        let bytes = vec![0xFE, 0xFF, 0x00, 0x41, 0x00, 0x42];
        let result = pdf_object_to_json(&Object::String(bytes)).unwrap();
        assert_eq!(result, serde_json::Value::String("u:AB".to_string()));
    }

    #[test]
    fn object_string_utf16le_bom_decodes_to_unicode() {
        // FFFE + 41 00 + 42 00 → "u:AB"
        let bytes = vec![0xFF, 0xFE, 0x41, 0x00, 0x42, 0x00];
        let result = pdf_object_to_json(&Object::String(bytes)).unwrap();
        assert_eq!(result, serde_json::Value::String("u:AB".to_string()));
    }

    #[test]
    fn object_string_utf16be_japanese_decodes_to_unicode() {
        // FEFF + 3042 (あ) + 3044 (い) → "u:あい"
        let bytes = vec![0xFE, 0xFF, 0x30, 0x42, 0x30, 0x44];
        let result = pdf_object_to_json(&Object::String(bytes)).unwrap();
        assert_eq!(result, serde_json::Value::String("u:あい".to_string()));
    }

    #[test]
    fn object_string_utf16be_with_odd_length_drops_trailing_byte() {
        // FEFF + 0041 + 00 (truncated last unit). qpdf silently ignores
        // the trailing odd byte and emits u:A; flpdf matches that.
        let bytes = vec![0xFE, 0xFF, 0x00, 0x41, 0x00];
        let result = pdf_object_to_json(&Object::String(bytes)).unwrap();
        assert_eq!(result, serde_json::Value::String("u:A".to_string()));
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
            serde_json::Value::String("b:d41d8cd98f00b204e9800998ecf8427e".to_string())
        );
    }

    #[test]
    fn object_string_single_x1e_forces_hex() {
        // 0x1E falls in the 0x18..=0x1F range that useHexString counts as
        // non_ascii; with len=3 the 5*non_ascii > len threshold triggers.
        let result = pdf_object_to_json(&Object::String(vec![b'A', 0x1E, b'B'])).unwrap();
        assert_eq!(result, serde_json::Value::String("b:411e42".to_string()));
    }

    #[test]
    fn object_string_del_0x7f_forces_hex() {
        // 0x7F (DEL) is counted as non_ascii by qpdf.
        let result = pdf_object_to_json(&Object::String(vec![b'A', 0x7F, b'B'])).unwrap();
        assert_eq!(result, serde_json::Value::String("b:417f42".to_string()));
    }

    #[test]
    fn object_string_explicit_utf8_bom_decodes() {
        // EF BB BF + "AB" → u:AB. The BOM is stripped; the remainder is
        // emitted as a UTF-8 string.
        let bytes = vec![0xEF, 0xBB, 0xBF, b'A', b'B'];
        let result = pdf_object_to_json(&Object::String(bytes)).unwrap();
        assert_eq!(result, serde_json::Value::String("u:AB".to_string()));
    }

    #[test]
    fn object_string_explicit_utf8_bom_non_ascii() {
        // EF BB BF + "café" (UTF-8 bytes for café) → u:café.
        let bytes = vec![0xEF, 0xBB, 0xBF, b'c', b'a', b'f', 0xC3, 0xA9];
        let result = pdf_object_to_json(&Object::String(bytes)).unwrap();
        assert_eq!(result, serde_json::Value::String("u:café".to_string()));
    }

    #[test]
    fn object_string_with_nul_falls_back_to_binary() {
        // 0x00 (NUL) is below 0x20 and not one of the allowed control bytes
        // (\b \t \n \f \r), so use_hex_string short-circuits to true and the
        // whole string is emitted as b:<hex> — the qpdf-equivalent treatment
        // for an /ID array element that contains a NUL.
        let bytes = vec![0xab, 0xcd, 0x00, 0xef];
        let result = pdf_object_to_json(&Object::String(bytes)).unwrap();
        assert_eq!(result, serde_json::Value::String("b:abcd00ef".to_string()));
    }

    #[test]
    fn object_string_empty_is_text() {
        let result = pdf_object_to_json(&Object::String(vec![])).unwrap();
        assert_eq!(result, serde_json::Value::String("u:".to_string()));
    }

    // ── 12. Reference conversion ──────────────────────────────────────────────

    #[test]
    fn object_reference_to_json() {
        use crate::ObjectRef;
        let result = pdf_object_to_json(&Object::Reference(ObjectRef::new(2, 0))).unwrap();
        assert_eq!(result, serde_json::Value::String("2 0 R".to_string()));
    }

    #[test]
    fn already_resolved_indirect_child_still_reports_n_g_r() {
        // Regression coverage for pdf_object_to_json's object_ref()-first
        // dispatch order: an indirect handle that some *other* code path
        // already resolved must still serialize as "N G R", not get inlined
        // as its resolved value. Object couldn't represent this state at all
        // (Object::Reference is always unresolved), so nothing in the legacy
        // suite could exercise it — see this function's own doc.
        use crate::ObjectRef;
        let mut pdf = empty_pdf();
        let child_ref = ObjectRef::new(2, 0);
        pdf.set_object(child_ref, Object::Array(vec![Object::Integer(1)]));
        pdf.resolve(child_ref).unwrap();
        let child_handle = pdf.get_object_handle(child_ref);
        assert!(
            child_handle.as_array().is_some(),
            "sanity: the canonical handle for child_ref is already resolved to an array"
        );

        let container = ObjectHandle::array(vec![child_handle]);
        let result = project(super::pdf_object_to_json(&container).unwrap()).unwrap();
        assert_eq!(
            result,
            serde_json::Value::Array(vec![serde_json::Value::String("2 0 R".to_string())])
        );
    }

    #[test]
    fn direct_handle_holding_a_shallow_copied_reference_still_reports_n_g_r() {
        // Regression coverage: Pdf::set_object(holder, Object::Reference(target))
        // resolves holder's indirect slot to ObjectValue::Reference(target) in
        // place (the redirect mechanic); ObjectHandle::shallow_copy on that
        // handle privatizes it into a *direct* handle that still carries the
        // same ObjectValue::Reference payload verbatim
        // (shallow_copy_value's catch-all `other => other.clone()` arm). This
        // handle's object_ref() is None (it is direct), so only the
        // as_reference() check catches it before the type_code() match.
        use crate::ObjectRef;
        let mut pdf = empty_pdf();
        let holder = ObjectRef::new(2, 0);
        let target = ObjectRef::new(5, 0);
        pdf.set_object(holder, Object::Reference(target));
        let redirected = pdf.get_object_handle(holder);
        let direct_reference_handle = redirected.shallow_copy().expect("reference copy");
        assert!(
            direct_reference_handle.is_direct(),
            "sanity: shallow_copy always returns a direct handle"
        );

        let result = project(super::pdf_object_to_json(&direct_reference_handle).unwrap()).unwrap();
        assert_eq!(result, serde_json::Value::String("5 0 R".to_string()));
    }

    #[test]
    fn pdf_dest_to_json_dereferences_a_resolved_indirect_array() {
        let mut pdf = empty_pdf();
        let dest_ref = ObjectRef::new(9, 0);
        pdf.set_object(
            dest_ref,
            Object::Array(vec![
                Object::Reference(ObjectRef::new(3, 0)),
                Object::Name(b"Fit".to_vec()),
            ]),
        );
        pdf.resolve(dest_ref).unwrap();
        let handle = pdf.get_object_handle(dest_ref);

        let result = project(super::pdf_dest_to_json(&handle).unwrap()).unwrap();
        assert_eq!(
            result,
            serde_json::Value::Array(vec![
                serde_json::Value::String("3 0 R".to_string()),
                serde_json::Value::String("/Fit".to_string()),
            ])
        );
    }

    #[test]
    fn pdf_dest_to_json_resolves_an_unresolved_outer_handle_once() {
        // qpdf's `getJSON(..., true)` resolves the outer indirect handle, while
        // the array writer keeps its own indirect child identity unchanged.
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat/qdf-contents-ref-array.pdf");
        let mut pdf = Pdf::open(std::io::Cursor::new(
            std::fs::read(fixture).expect("array fixture must exist"),
        ))
        .expect("array fixture must open");
        let dest_ref = ObjectRef::new(5, 0);
        let handle = pdf.get_object_handle(dest_ref);
        assert!(!handle.is_resolved(), "sanity: still unresolved");
        let result = project(super::pdf_dest_to_json(&handle).unwrap()).unwrap();
        assert_eq!(
            result,
            serde_json::Value::Array(vec![
                serde_json::Value::String("6 0 R".to_string()),
                serde_json::Value::String("7 0 R".to_string()),
            ])
        );
    }

    #[test]
    fn pdf_dest_to_json_reports_qpdf_reserved_handle_error() {
        // qpdf's true mode dispatches through dereference() before the
        // type-specific writer. A Reserved sentinel has no value to dispatch
        // on, so it is an error rather than a reference fallback.
        let pdf = empty_pdf();
        let handle = pdf.new_reserved().unwrap();
        assert!(handle.is_reserved(), "sanity: freshly reserved");
        let error = super::pdf_dest_to_json(&handle).expect_err("reserved must be rejected");
        assert!(
            matches!(error, ConvertError::PdfError(message) if message == "QPDFObjectHandle: attempting to get JSON from a reserved object")
        );
    }

    #[test]
    fn a_cycle_in_a_directly_constructed_handle_graph_errors_instead_of_overflowing_the_stack() {
        // Regression coverage: unlike the legacy Object tree, two direct
        // dictionaries can be linked to each other via replace_key with no
        // indirect object involved at all — pdf_object_to_json's recursion
        // has no object_ref() to short-circuit on for either one, so without
        // a depth bound this would recurse until the process aborts.
        let a = ObjectHandle::dictionary(vec![]);
        let b = ObjectHandle::dictionary(vec![]);
        a.replace_key(b"/Loop", b.clone()).unwrap();
        b.replace_key(b"/Loop", a.clone()).unwrap();

        let result = super::pdf_object_to_json(&a);
        assert!(
            matches!(result, Err(ConvertError::PdfError(_))),
            "expected a PdfError from exceeding the depth bound, got {result:?}"
        );
    }

    #[test]
    fn a_qpdf_depth_bound_rejects_get_json_at_the_parser_limit() {
        // QPDFObjectHandle::getJSON serializes through JSON::parse. A handle
        // tree with MAX_PARSE_DEPTH wrappers around a terminal empty array
        // therefore produces MAX_PARSE_DEPTH + 1 JSON containers and is
        // rejected by qpdf's JSON.cc:1335-1338 stack bound.
        let mut inner = ObjectHandle::array(vec![]);
        for _ in 0..crate::parser::MAX_PARSE_DEPTH {
            inner = ObjectHandle::array(vec![inner]);
        }
        let result = super::pdf_object_to_json(&inner);
        assert!(
            matches!(result, Err(ConvertError::JsonError(_))),
            "qpdf's JSON parser must reject the extra terminal container, got {result:?}"
        );
    }

    // ── 13. Null conversion ───────────────────────────────────────────────────

    #[test]
    fn object_null_to_json() {
        assert_eq!(
            pdf_object_to_json(&Object::Null).unwrap(),
            serde_json::Value::Null
        );
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
            serde_json::Value::Array(vec![
                number(1),
                serde_json::Value::Bool(true),
                serde_json::Value::Null,
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
        let pairs = object_pairs(result);
        // Keys should be in alphabetical order: /Apple, /Mango, /Zebra
        let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["/Apple", "/Mango", "/Zebra"]);
    }

    // ── 16. QPDF_Stream::writeJSON emits its dictionary directly ─────────────

    #[test]
    fn object_stream_write_json_emits_only_the_stream_dictionary() {
        use crate::object::{Dictionary, Stream};
        let stream = Object::Stream(Stream::new(Dictionary::new(), vec![]));
        let result = pdf_object_to_json(&stream).unwrap();
        let pairs = object_pairs(&result);
        assert!(pairs.is_empty());
    }

    #[test]
    fn stream_with_non_dictionary_dict_handle_delegates_to_the_stream_dict_writer() {
        // Unlike the legacy Object::Stream(Stream { dict: Dictionary, .. }),
        // ObjectHandle::stream's public constructor does not validate its
        // dict argument's own type — qpdf's QPDF_Stream::writeJSON simply
        // delegates to that handle, so a direct scalar is serialized as-is.
        let malformed = ObjectHandle::stream(ObjectHandle::integer(1), Rc::new(vec![]));
        let result = project(super::pdf_object_to_json(&malformed).unwrap()).unwrap();
        assert_eq!(result, number(1));
    }

    #[test]
    fn detached_stream_reference_seeding_walks_stream_dictionary_entries() {
        use crate::object::{Dictionary, Stream};

        let object_ref = ObjectRef::new(16, 0);
        let mut dictionary = Dictionary::new();
        dictionary.insert("Child", Object::Reference(object_ref));
        let object = Object::Stream(Stream::new(dictionary, vec![]));
        let mut pdf = empty_pdf();

        seed_detached_reference_targets(&mut pdf, &object);

        assert!(!pdf.get_object_handle(object_ref).is_null());
    }

    #[test]
    fn object_json_error_conversions_preserve_pipeline_and_conversion_categories() {
        let pipeline = JsonOutputError::from(ObjectJsonError::Pipeline(PipelineError::runtime(
            "sink failed",
        )));
        assert!(matches!(pipeline, JsonOutputError::Pipeline(_)));

        let non_finite = JsonOutputError::from(ObjectJsonError::NonFiniteFloat);
        assert!(matches!(
            non_finite,
            JsonOutputError::Convert(ConvertError::NonFiniteFloat)
        ));

        let json = JsonOutputError::from(ObjectJsonError::Json("invalid".to_string()));
        assert!(matches!(
            json,
            JsonOutputError::Convert(ConvertError::JsonError(_))
        ));

        let other = JsonOutputError::from(ObjectJsonError::UnsupportedVersion(3));
        assert!(matches!(
            other,
            JsonOutputError::Convert(ConvertError::PdfError(_))
        ));

        assert!(matches!(
            convert_object_json_error(ObjectJsonError::NonFiniteFloat),
            ConvertError::NonFiniteFloat
        ));
        assert!(matches!(
            convert_object_json_error(ObjectJsonError::Json("invalid".to_string())),
            ConvertError::JsonError(_)
        ));
        assert!(matches!(
            convert_object_json_error(ObjectJsonError::Pipeline(PipelineError::runtime(
                "sink failed",
            ))),
            ConvertError::PdfError(_)
        ));
        assert!(matches!(
            convert_object_json_error(ObjectJsonError::UnsupportedVersion(3)),
            ConvertError::PdfError(_)
        ));
    }

    // ══════════════════════════════════════════════════════════════════════════
    // qpdf-key writer integration tests (one-page.pdf fixture)
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

    // ── 17. the qpdf key is a 2-element array ──────────────────────────

    #[test]
    fn qpdf_key_is_a_two_element_array() {
        let mut pdf = load_one_page_pdf();
        let result = qpdf_key_value(&mut pdf, DecodeLevel::Generalized, &StreamDataMode::None);
        let serde_json::Value::Array(elems) = result else {
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
    fn qpdf_key_metadata_keys_and_values() {
        let mut pdf = load_one_page_pdf();
        let serde_json::Value::Array(elems) =
            qpdf_key_value(&mut pdf, DecodeLevel::Generalized, &StreamDataMode::None)
        else {
            panic!("expected Array");
        };
        assert_eq!(elems[0]["jsonversion"], number(2));
        assert_eq!(elems[0]["pdfversion"], "1.3");
        assert_eq!(elems[0]["pushedinheritedpageresources"], false);
        // Writing the qpdf key alone never walks the page tree, so qpdf
        // reports calledgetallpages as false here.
        assert_eq!(elems[0]["calledgetallpages"], false);
        assert_eq!(elems[0]["maxobjectid"], number(7));
    }

    #[test]
    fn qpdf_key_metadata_reports_a_page_walk_that_already_happened() {
        // qpdf emits the pages section before the qpdf key, and that walk is
        // what flips calledgetallpages — `qpdf --json=2` reports true while
        // `qpdf --json=2 --json-key=qpdf` reports false on the same file.
        let mut pdf = load_one_page_pdf();
        build_pages_section(&mut pdf).expect("pages section must build");

        let qpdf_key = qpdf_key_value(&mut pdf, DecodeLevel::Generalized, &StreamDataMode::None);
        assert_eq!(qpdf_key[0]["calledgetallpages"], true);
    }

    // ── 19. objects_map has the expected keys ─────────────────────────────────

    #[test]
    fn qpdf_key_objects_map_has_expected_keys() {
        let mut pdf = load_one_page_pdf();
        let serde_json::Value::Array(elems) =
            qpdf_key_value(&mut pdf, DecodeLevel::Generalized, &StreamDataMode::None)
        else {
            panic!("expected Array");
        };
        let map_pairs = object_pairs(&elems[1]);
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
    fn qpdf_key_obj7_is_stream() {
        let mut pdf = load_one_page_pdf();
        let serde_json::Value::Array(elems) =
            qpdf_key_value(&mut pdf, DecodeLevel::Generalized, &StreamDataMode::None)
        else {
            panic!("expected Array");
        };
        let map_pairs = object_pairs(&elems[1]);
        let obj7 = map_pairs
            .iter()
            .find(|(k, _)| k == "obj:7 0 R")
            .map(|(_, v)| v)
            .expect("obj:7 0 R not found");
        // Must be { "stream": { "dict": { ... } } }
        let obj7_pairs = object_pairs(obj7);
        assert_eq!(obj7_pairs[0].0, "stream", "obj:7 must have 'stream' key");
        let stream_inner = object_pairs(&obj7_pairs[0].1);
        assert_eq!(stream_inner[0].0, "dict", "stream must have 'dict' key");
    }

    // ── 21. trailer has a value wrapper ──────────────────────────────────────

    #[test]
    fn qpdf_key_trailer_has_value_wrapper() {
        let mut pdf = load_one_page_pdf();
        let serde_json::Value::Array(elems) =
            qpdf_key_value(&mut pdf, DecodeLevel::Generalized, &StreamDataMode::None)
        else {
            panic!("expected Array");
        };
        let map_pairs = object_pairs(&elems[1]);
        let trailer = map_pairs
            .iter()
            .find(|(k, _)| k == "trailer")
            .map(|(_, v)| v)
            .expect("trailer not found");
        let trailer_pairs = object_pairs(trailer);
        assert_eq!(trailer_pairs[0].0, "value", "trailer must have 'value' key");
        // /Size should be Integer(8)
        let trailer_dict = object_pairs(&trailer_pairs[0].1);
        let size = trailer_dict
            .iter()
            .find(|(k, _)| k == "/Size")
            .map(|(_, v)| v)
            .expect("/Size not found in trailer");
        assert_eq!(*size, number(8), "/Size should be 8");
    }

    // ── 22. Live null indirect object is emitted, not silently dropped ────────
    //
    // Regression test for the earlier `Object::Null => continue` bug: a live
    // indirect object that *is* null (e.g. `1 0 obj null endobj`) must appear
    // in objects_map as `{ "value": null }`, just like a non-null live object.
    // qpdf does the same.

    #[test]
    fn qpdf_key_live_null_indirect_object_is_emitted_with_value_null() {
        let mut pdf = load_one_page_pdf();

        // Patch obj 2 (the Font dictionary in one-page.pdf) to a live null
        // indirect object. The xref entry remains live; only the resolved
        // value becomes Null. The writer must still emit obj:2 0 R.
        pdf.set_object(crate::ObjectRef::new(2, 0), Object::Null);

        let serde_json::Value::Array(elems) =
            qpdf_key_value(&mut pdf, DecodeLevel::Generalized, &StreamDataMode::None)
        else {
            panic!("expected Array");
        };
        let map_pairs = object_pairs(&elems[1]);

        let obj2 = map_pairs
            .iter()
            .find(|(k, _)| k == "obj:2 0 R")
            .map(|(_, v)| v)
            .expect("obj:2 0 R must remain in objects_map when it is live and null");
        let obj2_pairs = object_pairs(obj2);
        assert_eq!(
            obj2_pairs.len(),
            1,
            "live null indirect must have a single 'value' key"
        );
        assert_eq!(obj2_pairs[0].0, "value");
        assert_eq!(obj2_pairs[0].1, serde_json::Value::Null);
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
        assert_eq!(json, serde_json::Value::String("n:/A#ffB".to_string()));
    }

    #[test]
    fn pdf_object_to_json_non_utf8_name_restores_tokenizer_null_marker() {
        let obj = Object::Name(vec![0, 0xff]);
        let json = pdf_object_to_json(&obj).unwrap();
        assert_eq!(json, serde_json::Value::String("n:/##ff".to_string()));
    }

    #[test]
    fn pdf_object_to_json_valid_utf8_name_uses_decoded_bytes() {
        // qpdf JSON v2 emits decoded valid UTF-8 name bytes directly. JSON
        // escaping, rather than PDF #xx escaping, protects delimiters.
        let obj = Object::Name(b"a b#(c)".to_vec());
        let json = pdf_object_to_json(&obj).unwrap();
        assert_eq!(
            json,
            serde_json::Value::String("/a b#(c)".to_string()),
            "valid UTF-8 name bytes must stay decoded"
        );
    }

    #[test]
    fn pdf_object_to_json_name_with_only_safe_bytes_is_passthrough() {
        // Plain ASCII names are emitted unchanged.
        let obj = Object::Name(b"Helvetica".to_vec());
        let json = pdf_object_to_json(&obj).unwrap();
        assert_eq!(json, serde_json::Value::String("/Helvetica".to_string()));
    }

    #[test]
    fn dict_to_json_keys_with_non_utf8_bytes_use_hex_escape() {
        // Dictionary keys go through the same escape path as Object::Name.
        let mut dict = Dictionary::new();
        dict.insert(b"K\xffey", Object::Integer(7));
        let json = pdf_object_to_json(&Object::Dictionary(dict)).unwrap();
        let pairs = object_pairs(json);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "n:/K#ffey");
        assert_eq!(pairs[0].1, number(7));
    }

    #[test]
    fn object_handle_dictionary_json_emits_one_canonical_key_slash() {
        let handle = ObjectHandle::dictionary(vec![(
            b"Type".to_vec(),
            ObjectHandle::name(b"Catalog".to_vec()),
        )]);
        let json = project(super::pdf_object_to_json(&handle).unwrap()).unwrap();
        assert_eq!(
            object_pairs(json),
            vec![(
                "/Type".to_string(),
                serde_json::Value::String("/Catalog".to_string())
            )]
        );
    }

    #[test]
    fn pdf_object_to_json_name_with_control_byte_escapes() {
        // Control bytes are valid UTF-8; serde_json::Value retains the byte and the
        // final JSON serializer emits the required JSON escape.
        let obj = Object::Name(b"x\x01y".to_vec());
        let json = pdf_object_to_json(&obj).unwrap();
        assert_eq!(json, serde_json::Value::String("/x\u{1}y".to_string()));
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
    fn get_page_entry(pages: &serde_json::Value, idx: usize) -> Vec<(String, serde_json::Value)> {
        let serde_json::Value::Array(arr) = pages else {
            panic!("pages section is not an Array");
        };
        object_pairs(&arr[idx])
    }

    // ── 26. one-page.pdf: pages array length ─────────────────────────────────

    #[test]
    fn build_pages_section_one_page_length() {
        let mut pdf = load_fixture_pdf("one-page.pdf");
        let pages = build_pages_section(&mut pdf).expect("build_pages_section failed");
        let serde_json::Value::Array(arr) = &pages else {
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
            serde_json::Value::Array(vec![serde_json::Value::String("7 0 R".to_string())]),
            "contents mismatch"
        );
        // images = []
        assert_eq!(
            pairs[1].1,
            serde_json::Value::Array(vec![]),
            "images must be empty"
        );
        // label = null
        assert_eq!(pairs[2].1, serde_json::Value::Null, "label must be null");
        // object = "3 0 R"
        assert_eq!(
            pairs[3].1,
            serde_json::Value::String("3 0 R".to_string()),
            "object mismatch"
        );
        // outlines = []
        assert_eq!(
            pairs[4].1,
            serde_json::Value::Array(vec![]),
            "outlines must be empty"
        );
        // pageposfrom1 = 1
        assert_eq!(pairs[5].1, number(1), "pageposfrom1 must be 1");
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
        let no_subtype_ref = ObjectRef::new(100, 0);
        pdf.set_object(
            no_subtype_ref,
            Object::Stream(Stream::new(Dictionary::new(), Vec::new())),
        );
        xobjects.insert("NoSubtype", Object::Reference(no_subtype_ref));
        let form_ref = ObjectRef::new(101, 0);
        let mut form_dict = Dictionary::new();
        form_dict.insert("Subtype", Object::Name(b"Form".to_vec()));
        pdf.set_object(form_ref, Object::Stream(Stream::new(form_dict, Vec::new())));
        xobjects.insert("Form", Object::Reference(form_ref));
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

    #[test]
    fn collect_image_refs_xobject_resolving_to_non_dictionary_returns_empty() {
        let mut pdf = load_one_page_pdf();
        let page_ref = ObjectRef::new(3, 0);

        let mut resources = Dictionary::new();
        resources.insert("XObject", Object::Integer(7));

        let Object::Dictionary(mut page) = pdf.resolve(page_ref).expect("resolve page") else {
            panic!("page must be a dictionary"); // cov:ignore: fixture-shape guard
        };
        page.insert("Resources", Object::Dictionary(resources));
        pdf.set_object(page_ref, Object::Dictionary(page));

        assert!(collect_image_refs(&mut pdf, page_ref)
            .expect("collect images")
            .is_empty());
    }

    #[test]
    fn collect_image_refs_xobject_entry_resolving_to_non_stream_is_skipped() {
        let mut pdf = load_one_page_pdf();
        let page_ref = ObjectRef::new(3, 0);
        let non_stream_ref = ObjectRef::new(99, 0);
        pdf.set_object(non_stream_ref, Object::Integer(1));

        let mut xobjects = Dictionary::new();
        xobjects.insert("Im", Object::Reference(non_stream_ref));
        let mut resources = Dictionary::new();
        resources.insert("XObject", Object::Dictionary(xobjects));

        let Object::Dictionary(mut page) = pdf.resolve(page_ref).expect("resolve page") else {
            panic!("page must be a dictionary"); // cov:ignore: fixture-shape guard
        };
        page.insert("Resources", Object::Dictionary(resources));
        pdf.set_object(page_ref, Object::Dictionary(page));

        assert!(collect_image_refs(&mut pdf, page_ref)
            .expect("collect images")
            .is_empty());
    }

    #[test]
    fn collect_image_refs_propagates_invalid_resources_error() {
        let mut pdf = load_one_page_pdf();
        let page_ref = ObjectRef::new(3, 0);

        let Object::Dictionary(mut page) = pdf.resolve(page_ref).expect("resolve page") else {
            panic!("page must be a dictionary"); // cov:ignore: fixture-shape guard
        };
        page.insert("Resources", Object::Integer(7));
        pdf.set_object(page_ref, Object::Dictionary(page));

        let error = collect_image_refs(&mut pdf, page_ref).expect_err("invalid resources");
        assert!(
            matches!(&error, ConvertError::PdfError(message) if message.contains("/Resources entry")),
            "unexpected error: {error:?}"
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
        let serde_json::Value::Array(arr) = &pages else {
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
                serde_json::Value::Array(vec![serde_json::Value::String(exp_contents.to_string())]),
                "page {i} contents mismatch"
            );
            assert_eq!(
                pairs[3].1,
                serde_json::Value::String(exp_obj.to_string()),
                "page {i} object mismatch"
            );
            assert_eq!(
                pairs[5].1,
                number(*exp_pos),
                "page {i} pageposfrom1 mismatch"
            );
            // label and outlines are placeholders
            assert_eq!(
                pairs[2].1,
                serde_json::Value::Null,
                "page {i} label must be null"
            );
            assert_eq!(
                pairs[4].1,
                serde_json::Value::Array(vec![]),
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
        let serde_json::Value::Array(arr) = &pages else {
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
                serde_json::Value::Array(vec![serde_json::Value::String(exp_contents.to_string())]),
                "page {i} contents mismatch"
            );
            assert_eq!(
                pairs[3].1,
                serde_json::Value::String(exp_obj.to_string()),
                "page {i} object mismatch"
            );
            assert_eq!(
                pairs[5].1,
                number(*exp_pos),
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
        let handle = pdf.get_object_handle(crate::ObjectRef::new(7, 0));
        let refs = collect_content_refs(&mut pdf, &handle).expect("collect_content_refs failed");
        assert_eq!(refs, vec!["7 0 R".to_string()]);
    }

    // ── 32. collect_content_refs: Array of References ─────────────────────────

    #[test]
    fn collect_content_refs_array_of_refs() {
        let mut pdf = load_one_page_pdf();
        let handle = ObjectHandle::array(vec![
            pdf.get_object_handle(crate::ObjectRef::new(4, 0)),
            pdf.get_object_handle(crate::ObjectRef::new(5, 0)),
        ]);
        let refs = collect_content_refs(&mut pdf, &handle).expect("collect_content_refs failed");
        assert_eq!(refs, vec!["4 0 R".to_string(), "5 0 R".to_string()]);
    }

    // ── 33. collect_content_refs: Null → empty ────────────────────────────────

    #[test]
    fn collect_content_refs_null_returns_empty() {
        let mut pdf = load_one_page_pdf();
        let handle = ObjectHandle::null();
        let refs = collect_content_refs(&mut pdf, &handle).expect("collect_content_refs failed");
        assert!(refs.is_empty());
    }

    // ── 34. collect_content_refs: Array with mixed types skips non-refs ───────

    #[test]
    fn collect_content_refs_array_skips_non_refs() {
        let mut pdf = load_one_page_pdf();
        let handle = ObjectHandle::array(vec![
            pdf.get_object_handle(crate::ObjectRef::new(3, 0)),
            ObjectHandle::integer(99), // not a ref — must be skipped
            pdf.get_object_handle(crate::ObjectRef::new(5, 0)),
        ]);
        let refs = collect_content_refs(&mut pdf, &handle).expect("collect_content_refs failed");
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

        let handle = pdf.get_object_handle(crate::ObjectRef::new(2, 0));
        let refs = collect_content_refs(&mut pdf, &handle).expect("collect_content_refs failed");
        assert_eq!(
            refs,
            vec!["4 0 R".to_string(), "5 0 R".to_string()],
            "indirect Array of refs must be unwrapped, not emitted as the array's ref number"
        );
    }

    #[test]
    fn collect_content_refs_indirect_resolving_to_non_stream_non_array_returns_empty() {
        let mut pdf = load_one_page_pdf();
        let non_stream_non_array_ref = crate::ObjectRef::new(50, 0);
        pdf.set_object(non_stream_non_array_ref, Object::Integer(1));

        let handle = pdf.get_object_handle(non_stream_non_array_ref);
        let refs = collect_content_refs(&mut pdf, &handle).expect("collect_content_refs failed");
        assert!(refs.is_empty());
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

    // ── 35b. /PageLabels present on a zero-page document → one fabricated
    //         leading entry, not an empty array ────────────────────────────
    //
    // QPDFPageLabelDocumentHelper::getLabelsForPageRange(0, -1, 0, ...)
    // unconditionally pushes a reconstructed label for index 0 *before* its
    // per-page loop runs (QPDFPageLabelDocumentHelper.cc:65-90); the loop
    // itself (`for i = start_idx+1; i <= end_idx; ++i`) simply never
    // executes when end_idx (npages - 1 = -1) < start_idx + 1. A page-count
    // shortcut in the JSON builder must not skip that leading entry.

    #[test]
    fn pagelabels_present_on_zero_page_document_yields_one_entry() {
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let off1 = bytes.len();
        bytes.extend_from_slice(
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /PageLabels 3 0 R >>\nendobj\n",
        );
        let off2 = bytes.len();
        bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");
        let off3 = bytes.len();
        bytes.extend_from_slice(b"3 0 obj\n<< /Nums [0 << /S /D /St 1 >>] >>\nendobj\n");
        let xref_start = bytes.len();
        let xref = format!(
            "xref\n0 4\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n"
        );
        bytes.extend_from_slice(xref.as_bytes());
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        let mut pdf = crate::Pdf::open_mem_owned(bytes).expect("open zero-page fixture");

        let result = build_pagelabels_section(&mut pdf).expect("build_pagelabels_section failed");
        let serde_json::Value::Array(arr) = &result else {
            panic!("expected Array, got {result:?}"); // cov:ignore: build_pagelabels_section always returns json_array(..)
        };
        assert_eq!(
            arr.len(),
            1,
            "a zero-page document with /PageLabels must still yield qpdf's \
             one fabricated leading entry, got {arr:?}"
        );
        let entry = object_pairs(&arr[0]);
        assert_eq!(entry[0], ("index".to_string(), number(0)));
    }

    // ── 36. No /PageLabels → empty array ─────────────────────────────────────

    #[test]
    fn pagelabels_missing_returns_empty_array() {
        // one-page.pdf has no /PageLabels — must return [].
        let mut pdf = load_one_page_pdf();
        let result = build_pagelabels_section(&mut pdf).expect("build_pagelabels_section failed");
        assert_eq!(
            result,
            serde_json::Value::Array(vec![]),
            "missing /PageLabels must yield empty array"
        );
    }

    #[test]
    fn pagelabels_repairs_direct_number_tree_kid() {
        let mut pdf = load_one_page_pdf();
        let mut label = Dictionary::new();
        label.insert("S", Object::Name(b"D".to_vec()));
        label.insert("St", Object::Integer(3));
        let mut leaf = Dictionary::new();
        leaf.insert(
            "Limits",
            Object::Array(vec![Object::Integer(0), Object::Integer(0)]),
        );
        leaf.insert(
            "Nums",
            Object::Array(vec![Object::Integer(0), Object::Dictionary(label)]),
        );
        let mut root = Dictionary::new();
        root.insert("Kids", Object::Array(vec![Object::Dictionary(leaf)]));
        patch_pagelabels(&mut pdf, Object::Dictionary(root));

        let result = build_pagelabels_section(&mut pdf).expect("build pagelabels");

        let entries = result.as_array().expect("expected array");
        assert_eq!(entries.len(), 1);
        let entry = object_pairs(&entries[0]);
        assert_eq!(entry[0], ("index".to_string(), number(0)));
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
        let serde_json::Value::Array(arr) = &result else {
            panic!("expected Array, got {result:?}");
        };
        assert_eq!(arr.len(), 1, "expected 1 entry");

        let entry = object_pairs(&arr[0]);
        // Key order: index, label
        assert_eq!(entry[0].0, "index");
        assert_eq!(entry[0].1, number(0));
        assert_eq!(entry[1].0, "label");

        let label_pairs = object_pairs(&entry[1].1);
        assert_eq!(
            label_pairs[0],
            (
                "/S".to_string(),
                serde_json::Value::String("/D".to_string())
            )
        );
        assert_eq!(label_pairs[1], ("/St".to_string(), number(1)));
    }

    #[test]
    fn pagelabels_read_does_not_dirty_a_valid_tree() {
        let mut pdf = load_one_page_pdf();
        let mut label = Dictionary::new();
        label.insert("S", Object::Name(b"D".to_vec()));
        let mut root = Dictionary::new();
        root.insert(
            "Nums",
            Object::Array(vec![Object::Integer(0), Object::Dictionary(label)]),
        );
        patch_pagelabels(&mut pdf, Object::Dictionary(root));
        for object_ref in pdf.dirty_object_refs() {
            pdf.clear_dirty(object_ref);
        }

        build_pagelabels_section(&mut pdf).expect("build page labels");

        assert!(pdf.dirty_object_refs().is_empty());
    }

    // ── 38. Multiple ranges ────────────────────────────────────────────────

    #[test]
    fn pagelabels_multiple_ranges() {
        // /Nums [0 << /S /D >> 1 << /S /R /P "Appx" /St 1 >> 2 << /S /a >>]
        let mut pdf = load_fixture_pdf("three-page.pdf");

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
                    Object::Integer(1),
                    Object::Dictionary(label5),
                    Object::Integer(2),
                    Object::Dictionary(label10),
                ]),
            );
            d
        });
        patch_pagelabels(&mut pdf, pagelabels);

        let result = build_pagelabels_section(&mut pdf).expect("build_pagelabels_section failed");
        let serde_json::Value::Array(arr) = &result else {
            panic!("expected Array"); // cov:ignore: test-shape guard
        };
        assert_eq!(arr.len(), 3, "expected 3 entries");

        // Check indices
        let get_index = |i: usize| {
            let e = object_pairs(&arr[i]);
            value_for_key(&e, "index").as_i64().unwrap()
        };
        assert_eq!(get_index(0), 0);
        assert_eq!(get_index(1), 1);
        assert_eq!(get_index(2), 2);

        // Check styles
        let get_style = |i: usize| {
            let e = object_pairs(&arr[i]);
            let lp = object_pairs(&e[1].1);
            value_for_key(&lp, "/S").clone()
        };
        assert_eq!(get_style(0), serde_json::Value::String("/D".to_string()));
        assert_eq!(get_style(1), serde_json::Value::String("/R".to_string()));
        assert_eq!(get_style(2), serde_json::Value::String("/a".to_string()));

        // Check prefix on entry 1
        let e1 = object_pairs(&arr[1]);
        let lp1 = object_pairs(&e1[1].1);
        assert_eq!(
            value_for_key(&lp1, "/P").clone(),
            serde_json::Value::String("u:Appx".to_string())
        );
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
            matches!(&result, serde_json::Value::Array(arr) if arr.len() == 1),
            "indirect label value must resolve to one entry, got {result:?}"
        );
    }

    // ── 38c. Non-dict label value uses qpdf's fabricated default ─────────────

    #[test]
    fn pagelabels_non_dict_value_fabricates_default() {
        // QPDFPageLabelDocumentHelper ignores the non-dictionary entry, then
        // getLabelsForPageRange fabricates the default /St 1 label.
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
        let serde_json::Value::Array(entries) = result else {
            panic!("expected array"); // cov:ignore: test-shape guard
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(
            object_pairs(&object_pairs(&entries[0])[1].1),
            vec![("/St".to_string(), number(1))]
        );
    }

    // ── 39. /S absent → qpdf omits the null dictionary key ──────────────────

    #[test]
    fn pagelabels_no_style_omits_null_key() {
        // Label dict with only /P — qpdf omits null /S and adds /St.
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
        let serde_json::Value::Array(arr) = &result else {
            panic!("expected Array"); // cov:ignore: test-shape guard
        };
        assert_eq!(arr.len(), 1);
        let entry = object_pairs(&arr[0]);
        let label_pairs = object_pairs(&entry[1].1);
        assert_eq!(
            label_pairs,
            vec![
                (
                    "/P".to_string(),
                    serde_json::Value::String("u:App".to_string())
                ),
                ("/St".to_string(), number(1)),
            ]
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
        let serde_json::Value::Array(arr) = &result else {
            panic!("expected Array"); // cov:ignore: test-shape guard
        };
        assert_eq!(arr.len(), 1, "expected 1 entry from /Kids walk");
        let entry = object_pairs(&arr[0]);
        assert_eq!(entry[0].1, number(0));
        let lp = object_pairs(&entry[1].1);
        assert_eq!(
            lp,
            vec![
                (
                    "/S".to_string(),
                    serde_json::Value::String("/r".to_string())
                ),
                ("/St".to_string(), number(1)),
            ]
        );
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
                serde_json::Value::Array(vec![]),
                "{name}: expected empty pagelabels array"
            );
        }
    }

    // ── build_test_document (top-level composite output) ───────────────────────

    #[test]
    fn build_test_document_has_expected_top_level_keys_in_order() {
        let mut pdf = load_one_page_pdf();
        let v2 = build_test_document(&mut pdf, DecodeLevel::Generalized)
            .expect("build_test_document failed");
        // qpdf-style fixed order: version, parameters, pages, pagelabels, acroform, attachments, encrypt, outlines, qpdf
        let keys = top_level_key_names(&v2);
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

    fn top_level_key_names(value: &serde_json::Value) -> Vec<String> {
        const QPDF_ORDER: [&str; 9] = [
            "version",
            "parameters",
            "pages",
            "pagelabels",
            "acroform",
            "attachments",
            "encrypt",
            "outlines",
            "qpdf",
        ];
        QPDF_ORDER
            .into_iter()
            .filter(|key| value.get(*key).is_some())
            .map(str::to_string)
            .collect()
    }

    fn qpdf_object_value<'a>(value: &'a serde_json::Value, object: &str) -> &'a serde_json::Value {
        &value["qpdf"][1][object]["value"]
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

        let json = build_test_document_selected(
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

        let json = build_test_document_selected(
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

        let json = build_test_document_selected(
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
            let json = build_test_document_selected(
                &mut pdf,
                DecodeLevel::Generalized,
                &StreamDataMode::None,
                &keys,
            )
            .unwrap();
            assert_eq!(top_level_key_names(&json), expected, "keys={keys:?}");
        }
    }

    fn selected_qpdf_metadata(json: &serde_json::Value) -> &serde_json::Value {
        &json["qpdf"][0]
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
            let json = build_test_document_selected(
                &mut pdf,
                DecodeLevel::Generalized,
                &StreamDataMode::None,
                &keys,
            )
            .unwrap();

            assert_eq!(
                selected_qpdf_metadata(&json),
                &object(vec![
                    ("jsonversion".to_string(), number(2)),
                    (
                        "pdfversion".to_string(),
                        serde_json::Value::String(pdf_version)
                    ),
                    (
                        "pushedinheritedpageresources".to_string(),
                        serde_json::Value::Bool(false),
                    ),
                    (
                        "calledgetallpages".to_string(),
                        serde_json::Value::Bool(called_get_all_pages),
                    ),
                    ("maxobjectid".to_string(), number(i64::from(max_object_id)),),
                ]),
                "{label}: keys={keys:?}"
            );
        }
    }

    #[test]
    fn qpdf_dangling_body_reference_participates_in_maxobjectid_for_trailer_selection() {
        let mut pdf = load_fixture_pdf("dangling-body-one-page.pdf");
        let json = build_test_document_selected(
            &mut pdf,
            DecodeLevel::Generalized,
            &StreamDataMode::None,
            &[JsonKey::Qpdf],
        )
        .expect("build selected qpdf JSON");

        assert_eq!(
            selected_qpdf_metadata(&json),
            &object(vec![
                ("jsonversion".to_string(), number(2)),
                (
                    "pdfversion".to_string(),
                    serde_json::Value::String("1.3".to_string()),
                ),
                (
                    "pushedinheritedpageresources".to_string(),
                    serde_json::Value::Bool(false),
                ),
                (
                    "calledgetallpages".to_string(),
                    serde_json::Value::Bool(false),
                ),
                ("maxobjectid".to_string(), number(99)),
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

    fn last_startxref(bytes: &[u8]) -> u64 {
        let marker = b"startxref\n";
        let start = bytes
            .windows(marker.len())
            .rposition(|window| window == marker)
            .expect("startxref marker")
            + marker.len();
        let end = bytes[start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|length| start + length)
            .expect("startxref newline");
        std::str::from_utf8(&bytes[start..end])
            .unwrap()
            .parse()
            .unwrap()
    }

    fn append_classic_increment(
        bytes: &mut Vec<u8>,
        objects: &[(u32, u16, &str)],
        free_entries: &[(u32, u16)],
        size: u32,
        trailer_extra: &str,
        previous_xref: u64,
    ) -> u64 {
        let mut offsets = Vec::new();
        for (number, generation, body) in objects {
            offsets.push((*number, *generation, bytes.len()));
            bytes.extend_from_slice(
                format!("{number} {generation} obj\n{body}\nendobj\n").as_bytes(),
            );
        }
        let xref = bytes.len() as u64;
        bytes.extend_from_slice(b"xref\n");
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
                "trailer\n<< /Size {size} /Root 1 0 R /Prev {previous_xref} {trailer_extra} >>\nstartxref\n{xref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        xref
    }

    fn two_revision_historical_trailer_pdf() -> Vec<u8> {
        let mut bytes = build_qpdf_dangling_xref_pdf_with_trailer("", "/Info 99 0 R", &[], &[], 4);
        let previous = last_startxref(&bytes);
        append_classic_increment(&mut bytes, &[(4, 0, "null")], &[], 5, "", previous);
        bytes
    }

    fn three_revision_historical_trailer_pdf() -> Vec<u8> {
        let mut bytes = build_qpdf_dangling_xref_pdf_with_trailer(
            "",
            "/Info 99 0 R /OldGen 88 4 R /Freed 20 7 R /Zero 0 0 R /BadGen 77 65535 R",
            &[],
            &[],
            4,
        );
        let oldest = last_startxref(&bytes);
        let middle = append_classic_increment(
            &mut bytes,
            &[(4, 0, "null")],
            &[],
            5,
            "/Info 60 1 R /Middle 70 3 R",
            oldest,
        );
        append_classic_increment(
            &mut bytes,
            &[(5, 0, "null")],
            &[(20, 7), (200, 7)],
            201,
            "/Newest 50 2 R",
            middle,
        );
        bytes
    }

    fn historical_xref_stream_trailer_pdf_with_free_generation(free_generation: u16) -> Vec<u8> {
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (number, body) in [
            (1u32, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>"),
        ] {
            offsets.push(bytes.len() as u32);
            bytes.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
        }
        let xref_stream_offset = bytes.len() as u32;
        let mut entries = Vec::new();
        for (kind, offset, generation) in [
            (0u8, 0u32, u16::MAX),
            (1, offsets[0], 0),
            (1, offsets[1], 0),
            (1, offsets[2], 0),
            (1, xref_stream_offset, 0),
        ] {
            entries.push(kind);
            entries.extend_from_slice(&offset.to_be_bytes());
            entries.extend_from_slice(&generation.to_be_bytes());
        }
        bytes.extend_from_slice(
            format!(
                "4 0 obj\n<< /Type /XRef /Size 5 /Root 1 0 R /Info 99 0 R /Gen 88 4 R /W [1 4 2] /Index [0 5] /Length {} >>\nstream\n",
                entries.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(&entries);
        bytes.extend_from_slice(
            format!("\nendstream\nendobj\nstartxref\n{xref_stream_offset}\n%%EOF\n").as_bytes(),
        );
        append_classic_increment(
            &mut bytes,
            &[(5, 0, "null")],
            &[(4, free_generation)],
            6,
            "",
            u64::from(xref_stream_offset),
        );
        bytes
    }

    fn historical_xref_stream_trailer_pdf() -> Vec<u8> {
        historical_xref_stream_trailer_pdf_with_free_generation(1)
    }

    #[test]
    fn historical_xref_stream_is_resolved_in_the_canonical_cache_at_open() {
        let mut pdf = crate::Pdf::open_mem_owned_with_options(
            historical_xref_stream_trailer_pdf(),
            crate::PdfOpenOptions {
                repair: false,
                ..crate::PdfOpenOptions::default()
            },
        )
        .expect("open strict mixed xref fixture");
        let stream_ref = crate::ObjectRef::new(4, 0);

        let handle = pdf.get_object_handle(stream_ref);

        assert!(
            handle.is_resolved(),
            "qpdf reads the historical xref stream into obj_cache while loading /Prev"
        );
        assert_eq!(
            handle.type_code(),
            10,
            "historical object must remain a stream"
        );
    }

    fn latest_xref_stream_pdf() -> Vec<u8> {
        let bytes = historical_xref_stream_trailer_pdf();
        let eof = bytes
            .windows(b"%%EOF\n".len())
            .position(|window| window == b"%%EOF\n")
            .expect("first xref stream eof")
            + b"%%EOF\n".len();
        bytes[..eof].to_vec()
    }

    fn reused_historical_xref_stream_pdf() -> Vec<u8> {
        let mut bytes = latest_xref_stream_pdf();
        let previous = last_startxref(&bytes);
        append_classic_increment(
            &mut bytes,
            &[(4, 0, "<< /Marker /New >>"), (5, 0, "null")],
            &[],
            6,
            "",
            previous,
        );
        bytes
    }

    fn repeated_historical_xref_stream_pdf() -> Vec<u8> {
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (number, body) in [
            (1u32, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>"),
        ] {
            offsets.push(bytes.len() as u32);
            bytes.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
        }

        let append_xref_stream = |bytes: &mut Vec<u8>,
                                  marker: &str,
                                  info: u32,
                                  previous: Option<u64>| {
            let xref_stream_offset = bytes.len() as u32;
            let mut entries = Vec::new();
            for (kind, offset, generation) in [
                (0u8, 0u32, u16::MAX),
                (1, offsets[0], 0),
                (1, offsets[1], 0),
                (1, offsets[2], 0),
                (1, xref_stream_offset, 0),
            ] {
                entries.push(kind);
                entries.extend_from_slice(&offset.to_be_bytes());
                entries.extend_from_slice(&generation.to_be_bytes());
            }
            let prev = previous.map_or_else(String::new, |value| format!("/Prev {value} "));
            bytes.extend_from_slice(
                    format!(
                        "4 0 obj\n<< /Type /XRef /Size 5 /Root 1 0 R /Marker /{marker} /Info {info} 0 R {prev}/W [1 4 2] /Index [0 5] /Length {} >>\nstream\n",
                        entries.len()
                    )
                    .as_bytes(),
                );
            bytes.extend_from_slice(&entries);
            bytes.extend_from_slice(
                format!("\nendstream\nendobj\nstartxref\n{xref_stream_offset}\n%%EOF\n").as_bytes(),
            );
            u64::from(xref_stream_offset)
        };

        let oldest = append_xref_stream(&mut bytes, "Old", 91, None);
        let nearest = append_xref_stream(&mut bytes, "Near", 92, Some(oldest));
        append_classic_increment(&mut bytes, &[(5, 0, "null")], &[(4, 1)], 6, "", nearest);
        bytes
    }

    fn circular_historical_trailer_pdf() -> Vec<u8> {
        let mut bytes = build_qpdf_dangling_xref_pdf_with_trailer(
            "",
            "/Info 99 0 R /Prev 0000000000",
            &[],
            &[],
            4,
        );
        let old_xref = last_startxref(&bytes);
        let latest_xref =
            append_classic_increment(&mut bytes, &[(4, 0, "null")], &[], 5, "", old_xref);
        let marker = b"/Prev 0000000000";
        let start = bytes
            .windows(marker.len())
            .position(|window| window == marker)
            .expect("old /Prev placeholder")
            + b"/Prev ".len();
        bytes[start..start + 10].copy_from_slice(format!("{latest_xref:010}").as_bytes());
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
        assert!(!pdf.object_refs().contains(&crate::ObjectRef::new(200, 7)));
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
        assert!(!pdf.object_refs().contains(&crate::ObjectRef::new(200, 7)));
    }

    #[test]
    fn qpdf_preparation_collects_refs_from_an_older_omitted_trailer() {
        let mut pdf = crate::Pdf::open_mem_owned(two_revision_historical_trailer_pdf())
            .expect("open two-revision fixture");
        assert!(pdf.trailer().get("Info").is_none());

        let prepared = pdf
            .prepare_qpdf_json_objects()
            .expect("prepare qpdf objects");

        let historical = crate::ObjectRef::new(99, 0);
        assert_eq!(prepared.max_object_id, 99);
        assert!(prepared.refs.contains(&historical));
        assert!(!pdf.object_refs().contains(&historical));
        assert!(!pdf.live_object_refs().contains(&historical));
    }

    #[test]
    fn qpdf_preparation_unions_replaced_multigeneration_trailer_refs() {
        let mut pdf = crate::Pdf::open_mem_owned(three_revision_historical_trailer_pdf())
            .expect("open three-revision fixture");
        assert!(pdf.trailer().get("Info").is_none());
        assert_eq!(
            pdf.trailer().get_ref("Newest"),
            Some(crate::ObjectRef::new(50, 2))
        );

        let prepared = pdf
            .prepare_qpdf_json_objects()
            .expect("prepare qpdf objects");

        for historical in [
            crate::ObjectRef::new(99, 0),
            crate::ObjectRef::new(88, 4),
            crate::ObjectRef::new(20, 7),
            crate::ObjectRef::new(60, 1),
            crate::ObjectRef::new(70, 3),
            crate::ObjectRef::new(50, 2),
        ] {
            assert!(prepared.refs.contains(&historical), "{historical:?}");
            assert!(
                !pdf.live_object_refs().contains(&historical),
                "{historical:?}"
            );
        }
        assert_eq!(prepared.max_object_id, 99);
        assert!(!prepared.refs.contains(&crate::ObjectRef::new(0, 0)));
        assert!(!prepared.refs.contains(&crate::ObjectRef::new(77, u16::MAX)));
        assert!(!prepared
            .refs
            .iter()
            .any(|reference| reference.number == 200));
        assert!(!pdf.object_refs().contains(&crate::ObjectRef::new(200, 7)));
    }

    #[test]
    fn qpdf_preparation_collects_refs_from_a_freed_old_xref_stream() {
        let mut pdf = crate::Pdf::open_mem_owned_with_options(
            historical_xref_stream_trailer_pdf(),
            crate::PdfOpenOptions {
                repair: false,
                ..crate::PdfOpenOptions::default()
            },
        )
        .expect("open strict mixed xref fixture");
        assert!(pdf.trailer().get("Info").is_none());

        let prepared = pdf
            .prepare_qpdf_json_objects()
            .expect("prepare qpdf objects");

        assert_eq!(prepared.max_object_id, 99);
        assert!(prepared.refs.contains(&crate::ObjectRef::new(99, 0)));
        assert!(prepared.refs.contains(&crate::ObjectRef::new(88, 4)));
        let stream_ref = crate::ObjectRef::new(4, 0);
        assert!(prepared.refs.contains(&stream_ref));
        assert!(pdf.object_refs().contains(&stream_ref));
        assert!(!pdf.live_object_refs().contains(&stream_ref));
        assert_eq!(pdf.resolve(stream_ref).unwrap(), crate::Object::Null);
    }

    #[test]
    fn qpdf_raw_stream_payload_uses_historical_view_and_rejects_non_streams() {
        let mut pdf = crate::Pdf::open_mem_owned(historical_xref_stream_trailer_pdf())
            .expect("open mixed xref fixture");

        let payload = qpdf_raw_stream_payload(
            &mut pdf,
            crate::ObjectRef::new(4, 0),
            DecodeLevel::Generalized,
        )
        .unwrap()
        .unwrap();
        assert_eq!(payload.len(), 35);
        assert_eq!(
            qpdf_raw_stream_payload(
                &mut pdf,
                crate::ObjectRef::new(99, 0),
                DecodeLevel::Generalized,
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn qpdf_preparation_keeps_old_xref_streams_freed_at_the_same_generation() {
        let mut pdf = crate::Pdf::open_mem_owned_with_options(
            historical_xref_stream_trailer_pdf_with_free_generation(0),
            crate::PdfOpenOptions {
                repair: false,
                ..crate::PdfOpenOptions::default()
            },
        )
        .expect("open strict same-generation free xref stream fixture");
        let stream_ref = crate::ObjectRef::new(4, 0);

        let prepared = pdf
            .prepare_qpdf_json_objects()
            .expect("prepare qpdf objects");

        assert!(prepared.refs.contains(&stream_ref));
        assert!(matches!(
            qpdf_resolve_top_level_object(&mut pdf, stream_ref).unwrap(),
            crate::Object::Stream(_)
        ));
        assert_eq!(pdf.resolve(stream_ref).unwrap(), crate::Object::Null);
    }

    #[test]
    fn qpdf_preparation_prefers_a_new_live_object_reusing_an_xref_stream_ref() {
        let mut pdf = crate::Pdf::open_mem_owned(reused_historical_xref_stream_pdf())
            .expect("open live-reused xref stream fixture");
        let stream_ref = crate::ObjectRef::new(4, 0);

        let prepared = pdf
            .prepare_qpdf_json_objects()
            .expect("prepare qpdf objects");
        let replacement = qpdf_resolve_top_level_object(&mut pdf, stream_ref).unwrap();

        assert_eq!(prepared.max_object_id, 99);
        assert_eq!(
            replacement.as_dict().and_then(|dict| dict.get("Marker")),
            Some(&crate::Object::Name(b"New".to_vec()))
        );
    }

    #[test]
    fn qpdf_parsed_xref_streams_do_not_shadow_set_or_delete() {
        for bytes in [
            latest_xref_stream_pdf(),
            historical_xref_stream_trailer_pdf(),
        ] {
            let mut pdf = crate::Pdf::open_mem_owned(bytes).expect("open xref stream fixture");
            let stream_ref = crate::ObjectRef::new(4, 0);
            pdf.set_object(stream_ref, crate::Object::Name(b"Mutated".to_vec()));

            assert_eq!(
                qpdf_resolve_top_level_object(&mut pdf, stream_ref).unwrap(),
                crate::Object::Name(b"Mutated".to_vec())
            );

            pdf.delete_object(stream_ref);
            assert_eq!(
                qpdf_resolve_top_level_object(&mut pdf, stream_ref).unwrap(),
                crate::Object::Null
            );
        }
    }

    #[test]
    fn qpdf_removed_refs_stay_removed_across_repeated_preparation_until_set() {
        let cases = [
            (
                historical_xref_stream_trailer_pdf(),
                crate::ObjectRef::new(4, 0),
            ),
            (
                historical_xref_stream_trailer_pdf(),
                crate::ObjectRef::new(99, 0),
            ),
        ];

        for (bytes, removed) in cases {
            let mut pdf = crate::Pdf::open_mem_owned(bytes).expect("open historical fixture");
            assert!(pdf
                .prepare_qpdf_json_objects()
                .unwrap()
                .refs
                .contains(&removed));

            pdf.delete_object(removed);
            for _ in 0..2 {
                assert!(!pdf
                    .prepare_qpdf_json_objects()
                    .unwrap()
                    .refs
                    .contains(&removed));
            }

            pdf.set_object(removed, crate::Object::Name(b"Restored".to_vec()));
            assert!(pdf
                .prepare_qpdf_json_objects()
                .unwrap()
                .refs
                .contains(&removed));
            assert_eq!(
                qpdf_resolve_top_level_object(&mut pdf, removed).unwrap(),
                crate::Object::Name(b"Restored".to_vec())
            );
        }
    }

    #[test]
    fn qpdf_removed_body_discovery_does_not_resurrect_a_missing_ref() {
        let mut pdf = load_fixture_pdf("dangling-body-one-page.pdf");
        let removed = crate::ObjectRef::new(99, 0);
        assert!(pdf
            .prepare_qpdf_json_objects()
            .unwrap()
            .refs
            .contains(&removed));

        pdf.delete_object(removed);
        for _ in 0..2 {
            assert!(!pdf
                .prepare_qpdf_json_objects()
                .unwrap()
                .refs
                .contains(&removed));
        }
    }

    #[test]
    fn qpdf_preparation_keeps_the_nearest_parsed_xref_stream_generation() {
        let mut pdf = crate::Pdf::open_mem_owned(repeated_historical_xref_stream_pdf())
            .expect("open repeated xref stream fixture");
        let stream_ref = crate::ObjectRef::new(4, 0);

        let prepared = pdf
            .prepare_qpdf_json_objects()
            .expect("prepare qpdf objects");

        assert!(prepared.refs.contains(&stream_ref));
        assert!(pdf.object_refs().contains(&stream_ref));
        assert!(!pdf.live_object_refs().contains(&stream_ref));
    }

    #[test]
    fn qpdf_preparation_deduplicates_historical_refs_in_a_repaired_prev_cycle() {
        let bytes = circular_historical_trailer_pdf();
        assert!(crate::Pdf::open_mem_owned_with_options(
            bytes.clone(),
            crate::PdfOpenOptions {
                repair: false,
                ..crate::PdfOpenOptions::default()
            },
        )
        .is_err());
        let mut pdf = crate::Pdf::open_mem_owned_with_options(
            bytes,
            crate::PdfOpenOptions {
                repair: true,
                ..crate::PdfOpenOptions::default()
            },
        )
        .expect("repair circular /Prev fixture");
        assert!(pdf.trailer().get("Info").is_none());

        let prepared = pdf
            .prepare_qpdf_json_objects()
            .expect("prepare qpdf objects");

        assert_eq!(prepared.max_object_id, 99);
        assert_eq!(
            prepared
                .refs
                .iter()
                .filter(|reference| **reference == crate::ObjectRef::new(99, 0))
                .count(),
            1
        );
    }

    fn selected_qpdf_object_map(
        json: &serde_json::Value,
    ) -> &serde_json::Map<String, serde_json::Value> {
        json["qpdf"][1].as_object().expect("qpdf object map")
    }

    #[test]
    fn qpdf_dangling_raw_projection_matches_qpdf_container_null_rules() {
        let mut pdf = load_fixture_pdf("dangling-body-one-page.pdf");
        let json = build_test_document_selected(
            &mut pdf,
            DecodeLevel::Generalized,
            &StreamDataMode::None,
            &[JsonKey::Qpdf],
        )
        .expect("build qpdf JSON");
        let map = selected_qpdf_object_map(&json);

        let obj4 = map
            .iter()
            .find(|(key, _)| *key == "obj:4 0 R")
            .map(|(_, value)| value)
            .expect("catalog object");
        assert_eq!(
            obj4,
            &object(vec![(
                "value".to_string(),
                object(vec![
                    (
                        "/ArrZero".to_string(),
                        serde_json::Value::Array(vec![serde_json::Value::Null]),
                    ),
                    ("/Nested".to_string(), object(Vec::new()),),
                    (
                        "/PageMode".to_string(),
                        serde_json::Value::String("/UseNone".to_string()),
                    ),
                    (
                        "/Pages".to_string(),
                        serde_json::Value::String("6 0 R".to_string()),
                    ),
                    (
                        "/Type".to_string(),
                        serde_json::Value::String("/Catalog".to_string()),
                    ),
                ]),
            )])
        );
        assert_eq!(
            map.iter()
                .find(|(key, _)| *key == "obj:99 0 R")
                .map(|(_, value)| value),
            Some(&object(vec![(
                "value".to_string(),
                serde_json::Value::Null,
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
            let json = build_test_document_selected_objects(
                &mut pdf,
                DecodeLevel::Generalized,
                &StreamDataMode::None,
                &[JsonKey::Qpdf],
                &selectors,
            )
            .expect("build selected objects");

            let metadata = object_pairs(selected_qpdf_metadata(&json));
            assert_eq!(
                metadata
                    .iter()
                    .find(|(key, _)| key == "maxobjectid")
                    .map(|(_, value)| value),
                Some(&number(99)),
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

        assert!(crate::qpdf_null::reference_is_null(&mut pdf, first).unwrap());
        assert_eq!(
            qpdf_resolve_top_level_object(&mut pdf, first).unwrap(),
            Object::Null
        );

        let mut nested_dict = Dictionary::new();
        nested_dict.insert("Drop", Object::Null);
        let nested_stream = Object::Stream(Stream::new(nested_dict, Vec::new()));
        assert_eq!(
            qpdf_object_projection(&mut pdf, &nested_stream).unwrap(),
            object(vec![(
                "stream".to_string(),
                object(vec![("dict".to_string(), object(Vec::new()),)]),
            )])
        );
    }

    #[test]
    fn build_test_document_includes_pagelabels_section() {
        // Regression for CodeRabbit's flpdf-9hc.11.5 finding: the
        // pagelabels builder was added but never wired into the top-level
        // JSON, so users would never see the section. This test fails if
        // the wiring is dropped again.
        let mut pdf = load_one_page_pdf();
        let v2 = build_test_document(&mut pdf, DecodeLevel::Generalized)
            .expect("build_test_document failed");
        let pairs = object_pairs(v2);
        let pagelabels = pairs
            .iter()
            .find(|(k, _)| k == "pagelabels")
            .map(|(_, v)| v)
            .expect("pagelabels key must be present in the composite output");
        assert!(
            matches!(pagelabels, serde_json::Value::Array(_)),
            "pagelabels must be an Array"
        );
    }

    #[test]
    fn build_test_document_pages_count_matches_fixture() {
        let mut pdf = load_three_page_pdf();
        let v2 = build_test_document(&mut pdf, DecodeLevel::Generalized)
            .expect("build_test_document failed");
        let pairs = object_pairs(v2);
        let pages = pairs
            .iter()
            .find(|(k, _)| k == "pages")
            .map(|(_, v)| v)
            .expect("pages key missing");
        let serde_json::Value::Array(page_entries) = pages else {
            panic!("pages must be Array"); // cov:ignore: test-shape guard
        };
        assert_eq!(
            page_entries.len(),
            3,
            "three-page.pdf must produce 3 page entries"
        );
    }

    #[test]
    fn build_test_document_qpdf_metadata_uses_actual_pdf_version() {
        let mut pdf = load_one_page_pdf();
        let v2 = build_test_document(&mut pdf, DecodeLevel::Generalized)
            .expect("build_test_document failed");
        let pairs = object_pairs(v2);
        let qpdf = pairs.iter().find(|(k, _)| k == "qpdf").unwrap().1.clone();
        let serde_json::Value::Array(qpdf_arr) = qpdf else {
            panic!("qpdf must be Array");
        };
        let meta_pairs = object_pairs(&qpdf_arr[0]);
        let pdf_version = meta_pairs
            .iter()
            .find(|(k, _)| k == "pdfversion")
            .map(|(_, v)| v)
            .expect("pdfversion missing");
        // one-page.pdf header is "%PDF-1.3".
        assert_eq!(*pdf_version, serde_json::Value::String("1.3".to_string()));
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

    fn value_for_key<'a>(
        pairs: &'a [(String, serde_json::Value)],
        key: &str,
    ) -> &'a serde_json::Value {
        &pairs.iter().find(|(name, _)| name == key).unwrap().1
    }

    fn json_array(value: &serde_json::Value) -> &[serde_json::Value] {
        match value {
            serde_json::Value::Array(items) => items,
            _ => panic!("expected JSON array"), // cov:ignore: test-shape guard
        }
    }

    fn json_object(value: &serde_json::Value) -> Vec<(String, serde_json::Value)> {
        object_pairs(value)
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
            value_for_key(&first, "dest"),
            &serde_json::Value::Array(vec![
                serde_json::Value::String("8 0 R".into()),
                serde_json::Value::String("/XYZ".into()),
                serde_json::Value::Null,
                serde_json::Value::Null,
                serde_json::Value::Null,
            ])
        );
        assert_eq!(value_for_key(&first, "destpageposfrom1"), &number(6));
        assert_eq!(
            value_for_key(&first, "object"),
            &serde_json::Value::String("96 0 R".into())
        );
        assert_eq!(
            value_for_key(&first, "open"),
            &serde_json::Value::Bool(true)
        );
        assert_eq!(
            value_for_key(&first, "title"),
            &serde_json::Value::String("Isís 1 -> 5: /XYZ null null null".into())
        );

        let kids = json_array(value_for_key(&first, "kids"));
        assert_eq!(kids.len(), 2);
        let first_kid = json_object(&kids[0]);
        let second_kid = json_object(&kids[1]);
        assert_eq!(
            value_for_key(&first_kid, "title"),
            &serde_json::Value::String("Amanda 1.1 -> 11: /Fit".into())
        );
        assert_eq!(
            value_for_key(&first_kid, "open"),
            &serde_json::Value::Bool(false)
        );
        assert_eq!(
            value_for_key(&second_kid, "title"),
            &serde_json::Value::String("Sandy ÷Σανδι÷ 1.2 -> 13: /FitH 792".into())
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
            value_for_key(&first, "object"),
            &object(vec![
                ("/Count".into(), number(-1)),
                (
                    "/Dest".into(),
                    serde_json::Value::Array(vec![
                        serde_json::Value::String(page_ref.to_string()),
                        serde_json::Value::String("/Fit".into()),
                    ]),
                ),
                (
                    "/Next".into(),
                    object(vec![(
                        "/Title".into(),
                        serde_json::Value::String("u:Direct B".into()),
                    )]),
                ),
                (
                    "/Title".into(),
                    serde_json::Value::String("u:Direct A".into())
                ),
            ])
        );
        assert_eq!(value_for_key(&first, "destpageposfrom1"), &number(1));
        assert_eq!(
            value_for_key(&first, "open"),
            &serde_json::Value::Bool(false)
        );
        assert_eq!(
            value_for_key(&first, "title"),
            &serde_json::Value::String("Direct A".into())
        );

        assert_eq!(value_for_key(&second, "dest"), &serde_json::Value::Null);
        assert_eq!(
            value_for_key(&second, "destpageposfrom1"),
            &serde_json::Value::Null
        );
        assert_eq!(
            value_for_key(&second, "object"),
            &object(vec![(
                "/Title".into(),
                serde_json::Value::String("u:Direct B".into()),
            )])
        );
        assert_eq!(
            value_for_key(&second, "open"),
            &serde_json::Value::Bool(true)
        );
        assert_eq!(
            value_for_key(&second, "title"),
            &serde_json::Value::String("Direct B".into())
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
        assert_eq!(
            value_for_key(&parent, "kids"),
            &serde_json::Value::Array(Vec::new())
        );
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
            value_for_key(&item, "dest"),
            &serde_json::Value::Array(vec![
                serde_json::Value::String(page_ref.to_string()),
                serde_json::Value::String("/Fit".into()),
            ])
        );
        assert_eq!(value_for_key(&item, "destpageposfrom1"), &number(1));
    }

    /// An indirect `/Dest 8 0 R` where object 8 resolves to `[3 0 R /Fit]`.
    /// qpdf's own `--json=2 --json-key=outlines` dereferences the `/Dest`
    /// holder itself (`oiter.getDest().getJSON(m->json_version, true)`,
    /// `libqpdf/QPDFJob.cc:1126`) while leaving the nested page reference as
    /// its own `"N G R"` string — confirmed against live qpdf 11.9.0 on this
    /// exact object shape, which emits `"dest": ["3 0 R", "/Fit"]`. Contrast
    /// with `object`, which stays as the bare holder reference
    /// (`oiter.getObjectHandle().getJSON(m->json_version)`, no dereference).
    #[test]
    fn outline_json_v2_dereferences_an_indirect_dest_but_not_its_page_child() {
        let mut pdf = load_one_page_pdf();
        let page_ref = crate::pages::page_refs(&mut pdf).unwrap()[0];
        let outline_root_ref = crate::ObjectRef::new(100, 0);
        let item_ref = crate::ObjectRef::new(101, 0);
        let dest_array_ref = crate::ObjectRef::new(102, 0);

        let mut outline_root = Dictionary::new();
        outline_root.insert("First", Object::Reference(item_ref));
        patch_outline_root(&mut pdf, outline_root_ref, outline_root);

        let mut item = Dictionary::new();
        item.insert("Title", Object::String(b"Indirect Dest".to_vec()));
        item.insert("Dest", Object::Reference(dest_array_ref));
        pdf.set_object(item_ref, Object::Dictionary(item));
        pdf.set_object(
            dest_array_ref,
            Object::Array(vec![
                Object::Reference(page_ref),
                Object::Name(b"Fit".to_vec()),
            ]),
        );

        let result = build_outlines_section(&mut pdf).unwrap();
        let entries = json_array(&result);
        let item = json_object(&entries[0]);

        assert_eq!(
            value_for_key(&item, "dest"),
            &serde_json::Value::Array(vec![
                serde_json::Value::String(page_ref.to_string()),
                serde_json::Value::String("/Fit".into()),
            ])
        );
        assert_eq!(value_for_key(&item, "destpageposfrom1"), &number(1));
        assert_eq!(
            value_for_key(&item, "object"),
            &serde_json::Value::String(item_ref.to_string())
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
            serde_json::Value::Array(vec![]),
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
                serde_json::Value::Array(vec![]),
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
        let serde_json::Value::Array(entries) = &result else {
            panic!("expected Array, got {result:?}");
        };
        assert_eq!(entries.len(), 1, "expected 1 outline entry");

        let entry = object_pairs(&entries[0]);

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

        assert_eq!(entry[0].1, serde_json::Value::Null, "dest must be Null");
        assert_eq!(
            entry[1].1,
            serde_json::Value::Null,
            "page position must be Null"
        );
        // kids = [] (no /First in item)
        assert_eq!(
            entry[2].1,
            serde_json::Value::Array(vec![]),
            "kids must be empty"
        );
        // object = "101 0 R"
        assert_eq!(
            entry[3].1,
            serde_json::Value::String("101 0 R".to_string()),
            "object mismatch"
        );
        assert_eq!(
            entry[4].1,
            serde_json::Value::Bool(true),
            "open must default true"
        );
        // title = bare "Chapter 1" (no u: prefix)
        assert_eq!(
            entry[5].1,
            serde_json::Value::String("Chapter 1".to_string()),
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
        let serde_json::Value::Array(root_entries) = &result else {
            panic!("expected Array");
        };
        assert_eq!(root_entries.len(), 1, "root chain has 1 entry (parent)");

        let parent_entry = object_pairs(&root_entries[0]);
        // kids should contain 2 children.
        let kids_val = &parent_entry.iter().find(|(k, _)| k == "kids").unwrap().1;
        let serde_json::Value::Array(kids) = kids_val else {
            panic!("kids is not an Array");
        };
        assert_eq!(kids.len(), 2, "parent must have 2 children in kids");

        // Verify child titles.
        let get_title = |entry: &serde_json::Value| {
            let pairs = object_pairs(entry);
            pairs.iter().find(|(k, _)| k == "title").unwrap().1.clone()
        };
        assert_eq!(
            get_title(&kids[0]),
            serde_json::Value::String("Chapter 1".to_string())
        );
        assert_eq!(
            get_title(&kids[1]),
            serde_json::Value::String("Chapter 2".to_string())
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
        let serde_json::Value::Array(entries) = &result else {
            panic!("expected Array");
        };
        assert_eq!(
            entries.len(),
            1,
            "cycle guard must stop after 1 entry, got {}",
            entries.len()
        );
    }

    #[test]
    fn outlines_nested_child_next_cycle_guard_prevents_infinite_loop() {
        let mut pdf = load_one_page_pdf();

        let outline_root_ref = crate::ObjectRef::new(100, 0);
        let parent_ref = crate::ObjectRef::new(101, 0);
        let child_ref = crate::ObjectRef::new(102, 0);

        let mut outline_root = Dictionary::new();
        outline_root.insert("Type", Object::Name(b"Outlines".to_vec()));
        outline_root.insert("First", Object::Reference(parent_ref));
        outline_root.insert("Last", Object::Reference(parent_ref));
        patch_outline_root(&mut pdf, outline_root_ref, outline_root);

        let mut parent = Dictionary::new();
        parent.insert("Title", Object::String(b"Parent".to_vec()));
        parent.insert("Parent", Object::Reference(outline_root_ref));
        parent.insert("First", Object::Reference(child_ref));
        parent.insert("Last", Object::Reference(child_ref));
        parent.insert("Count", Object::Integer(1));
        pdf.set_object(parent_ref, Object::Dictionary(parent));

        let mut child = Dictionary::new();
        child.insert("Title", Object::String(b"Loop".to_vec()));
        child.insert("Parent", Object::Reference(parent_ref));
        child.insert("Next", Object::Reference(child_ref));
        pdf.set_object(child_ref, Object::Dictionary(child));

        let result = build_outlines_section(&mut pdf).expect("build_outlines_section failed");
        let serde_json::Value::Array(entries) = &result else {
            panic!("expected Array"); // cov:ignore: test-shape guard
        };
        assert_eq!(entries.len(), 1, "root chain must contain the parent");
        let parent_entry = object_pairs(&entries[0]);
        let kids = parent_entry
            .iter()
            .find(|(key, _)| key == "kids")
            .map(|(_, value)| value)
            .expect("parent must contain kids");
        let serde_json::Value::Array(kids) = kids else {
            panic!("kids is not an Array"); // cov:ignore: test-shape guard
        };
        assert_eq!(
            kids.len(),
            1,
            "nested /Next cycle guard must stop before a duplicate child"
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
        let serde_json::Value::Array(entries) = &result else {
            panic!("expected Array");
        };
        assert_eq!(entries.len(), 1, "expected 1 entry despite broken /Parent");
        let entry = object_pairs(&entries[0]);
        let title = entry.iter().find(|(k, _)| k == "title").unwrap().1.clone();
        assert_eq!(
            title,
            serde_json::Value::String("Broken Parent".to_string())
        );
    }

    // ── Test 6: build_test_document includes outlines section ─────────────────

    #[test]
    fn build_test_document_includes_outlines_section() {
        let mut pdf = load_one_page_pdf();
        let v2 = build_test_document(&mut pdf, DecodeLevel::Generalized)
            .expect("build_test_document failed");
        let pairs = object_pairs(v2);
        let outlines = pairs
            .iter()
            .find(|(k, _)| k == "outlines")
            .map(|(_, v)| v)
            .expect("outlines key must be present in composite output");
        assert!(
            matches!(outlines, serde_json::Value::Array(_)),
            "outlines must be an Array"
        );
        // one-page.pdf has no /Outlines → must be empty array
        assert_eq!(
            *outlines,
            serde_json::Value::Array(vec![]),
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
        let serde_json::Value::Array(entries) = &result else {
            panic!("expected Array");
        };
        let entry = object_pairs(&entries[0]);
        let title = entry.iter().find(|(k, _)| k == "title").unwrap().1.clone();
        // Must be bare "AB" — no "u:" prefix.
        assert_eq!(title, serde_json::Value::String("AB".to_string()));
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
        let serde_json::Value::Array(entries) = &result else {
            panic!("expected Array");
        };
        let entry = object_pairs(&entries[0]);
        assert_eq!(value_for_key(&entry, "dest"), &serde_json::Value::Null);
        assert_eq!(
            value_for_key(&entry, "object"),
            &serde_json::Value::String("101 0 R".to_string())
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
        let serde_json::Value::Array(entries) = result else {
            panic!("expected Array");
        };
        let entry = object_pairs(&entries[0]);
        assert_eq!(value_for_key(&entry, "dest"), &serde_json::Value::Null);
        assert_eq!(
            value_for_key(&entry, "object"),
            &serde_json::Value::String("101 0 R".to_string())
        );
    }

    // ══════════════════════════════════════════════════════════════════════════
    // build_acroform_section tests (flpdf-9hc.11.7)
    // ══════════════════════════════════════════════════════════════════════════

    #[test]
    fn acroform_section_emits_one_entry_per_page_widget_with_qpdf_schema() {
        let mut pdf = load_fixture_pdf("form-fields-and-annotations.pdf");

        let result = build_acroform_section(&mut pdf).expect("build_acroform_section failed");
        let pairs = object_pairs(&result);
        let fields = value_for_key(&pairs, "fields")
            .as_array()
            .expect("fields must be an array");

        assert_eq!(fields.len(), 5, "qpdf emits one entry per page Widget");
        let entry = object_pairs(&fields[0]);
        let mut keys: Vec<_> = entry.iter().map(|(key, _)| key.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "alternativename",
                "annotation",
                "choices",
                "defaultvalue",
                "fieldflags",
                "fieldtype",
                "fullname",
                "ischeckbox",
                "ischoice",
                "isradiobutton",
                "istext",
                "mappingname",
                "object",
                "pageposfrom1",
                "parent",
                "partialname",
                "quadding",
                "value",
            ]
        );
        assert_eq!(
            value_for_key(&entry, "object").clone(),
            serde_json::Value::String("3 0 R".to_string())
        );
        assert_eq!(value_for_key(&entry, "pageposfrom1").clone(), number(1));
        assert_eq!(
            value_for_key(&entry, "annotation").clone(),
            object(vec![
                ("annotationflags".to_string(), number(0),),
                (
                    "appearancestate".to_string(),
                    serde_json::Value::String(String::new()),
                ),
                (
                    "object".to_string(),
                    serde_json::Value::String("3 0 R".to_string()),
                ),
            ])
        );
    }

    #[test]
    fn acroform_section_includes_orphan_page_widget_as_its_own_field() {
        let mut pdf = load_fixture_pdf("acroform-sig-orphan-widget.pdf");

        let result = build_acroform_section(&mut pdf).expect("build_acroform_section failed");
        let pairs = object_pairs(&result);
        let fields = value_for_key(&pairs, "fields")
            .as_array()
            .expect("fields must be an array");

        assert_eq!(fields.len(), 1);
        let entry = object_pairs(&fields[0]);
        assert_eq!(
            value_for_key(&entry, "object").clone(),
            serde_json::Value::String("5 0 R".to_string())
        );
        assert_eq!(
            value_for_key(&entry, "fieldtype").clone(),
            serde_json::Value::String("/Sig".to_string())
        );
        assert_eq!(
            value_for_key(&entry, "annotation")["object"],
            serde_json::Value::String("5 0 R".to_string())
        );
    }

    #[test]
    fn acroform_section_serializes_widget_appearance_state_as_qpdf_name() {
        let mut pdf = load_fixture_pdf("form-fields-and-annotations.pdf");

        let result = build_acroform_section(&mut pdf).expect("build_acroform_section failed");
        let pairs = object_pairs(&result);
        let fields = value_for_key(&pairs, "fields")
            .as_array()
            .expect("fields must be an array");
        let entry = fields
            .iter()
            .map(object_pairs)
            .find(|entry| {
                value_for_key(entry, "object") == &serde_json::Value::String("13 0 R".to_string())
            })
            .expect("radio widget field 13 0 R must be present");
        let annotation = object_pairs(value_for_key(&entry, "annotation"));

        assert_eq!(
            value_for_key(&annotation, "appearancestate"),
            &serde_json::Value::String("/1".to_string())
        );
    }

    #[test]
    fn acroform_section_preserves_non_utf8_appearance_state_bytes() {
        // qpdf's JSON::Writer::encode_string operates byte-wise and never
        // validates UTF-8 (libqpdf/JSON.cc:216-271): a PDF Name's raw bytes
        // pass through unescaped except for control chars and quote/backslash.
        // `String::from_utf8_lossy` would replace an invalid sequence with
        // U+FFFD before serialization, corrupting the output relative to
        // qpdf. 0xE9 alone is not valid UTF-8 (it is a 3-byte sequence lead
        // byte with no continuation bytes).
        let mut pdf = load_fixture_pdf("form-fields-and-annotations.pdf");
        let widget_ref = ObjectRef::new(13, 0);
        let mut widget_dict = pdf
            .resolve(widget_ref)
            .expect("resolve widget 13 0")
            .into_dict()
            .expect("widget 13 0 is a dictionary");
        widget_dict.insert("AS", Object::Name(vec![b'A', 0xE9]));
        pdf.set_object(widget_ref, Object::Dictionary(widget_dict));

        let result =
            crate::job::build_acroform_section(&mut pdf).expect("build_acroform_section failed");
        let encoded = result.unparse().expect("unparse failed");

        assert!(
            encoded.windows(5).any(|w| w == b"\"/A\xe9\""),
            "expected raw byte 0xE9 preserved after the leading slash, got: {}",
            String::from_utf8_lossy(&encoded) // cov:ignore: assert! failure-message arm, only evaluated when this passing test fails
        );
        assert!(
            !encoded.windows(3).any(|w| w == [0xEF, 0xBF, 0xBD]),
            "output must not contain a U+FFFD replacement character: {}",
            String::from_utf8_lossy(&encoded) // cov:ignore: assert! failure-message arm, only evaluated when this passing test fails
        );
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
        assert_eq!(result, object(vec![]), "expected empty object");
    }

    #[test]
    fn attachments_read_does_not_dirty_a_valid_tree() {
        let mut pdf = load_attachment_pdf();
        assert!(pdf.dirty_object_refs().is_empty());

        build_attachments_section(&mut pdf).expect("build attachments");

        assert!(pdf.dirty_object_refs().is_empty());
    }

    #[test]
    fn attachments_without_root_returns_empty() {
        let mut pdf = no_root_pdf();
        let result = build_attachments_section(&mut pdf).expect("build_attachments_section failed");
        assert_eq!(result, object(vec![]));
    }

    #[test]
    fn attachments_non_dictionary_catalog_returns_empty() {
        let mut pdf = load_one_page_pdf();
        let catalog_ref = pdf.root_ref().expect("no /Root");
        pdf.set_object(catalog_ref, Object::Integer(7));

        let result = build_attachments_section(&mut pdf).expect("build_attachments_section failed");
        assert_eq!(result, object(vec![]));
    }

    #[test]
    fn attachments_non_dictionary_indirect_names_returns_empty() {
        let mut pdf = load_one_page_pdf();
        let names_ref = crate::ObjectRef::new(900, 0);
        pdf.set_object(names_ref, Object::Integer(7));
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
        assert_eq!(result, object(vec![]));
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
        assert_eq!(result, object(vec![]), "expected empty object");
    }

    #[test]
    fn attachments_indirect_names_terminating_at_non_dictionary_returns_empty() {
        let mut pdf = load_one_page_pdf();
        let names_ref = crate::ObjectRef::new(902, 0);
        pdf.set_object(names_ref, Object::Integer(7));
        let catalog_ref = pdf.root_ref().expect("no /Root");
        let mut catalog = pdf
            .resolve_borrowed(catalog_ref)
            .expect("resolve catalog")
            .as_dict()
            .expect("catalog dict")
            .clone();
        catalog.insert("Names", Object::Reference(names_ref));
        pdf.set_object(catalog_ref, Object::Dictionary(catalog));

        let result = build_attachments_section(&mut pdf).expect("build attachments");

        assert_eq!(result, object(vec![]));
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
            object(vec![]),
            "non-ref/non-dict leaf value must be skipped"
        );
    }

    #[test]
    fn attachments_repairs_direct_name_tree_kid() {
        let mut pdf = load_one_page_pdf();
        let mut filespec = Dictionary::new();
        filespec.insert("F", Object::String(b"inline.txt".to_vec()));
        filespec.insert("UF", Object::String(b"inline.txt".to_vec()));
        let mut leaf = Dictionary::new();
        leaf.insert(
            "Limits",
            Object::Array(vec![
                Object::String(b"inline".to_vec()),
                Object::String(b"inline".to_vec()),
            ]),
        );
        leaf.insert(
            "Names",
            Object::Array(vec![
                Object::String(b"inline".to_vec()),
                Object::Dictionary(filespec),
            ]),
        );
        let mut tree = Dictionary::new();
        tree.insert("Kids", Object::Array(vec![Object::Dictionary(leaf)]));
        let mut names = Dictionary::new();
        names.insert("EmbeddedFiles", Object::Dictionary(tree));
        let catalog_ref = pdf.root_ref().expect("catalog ref");
        let mut catalog = pdf
            .resolve_borrowed(catalog_ref)
            .expect("catalog")
            .as_dict()
            .expect("catalog dict")
            .clone();
        catalog.insert("Names", Object::Dictionary(names));
        pdf.set_object(catalog_ref, Object::Dictionary(catalog));

        let result = build_attachments_section(&mut pdf).expect("build attachments");

        let entries = object_pairs(&result);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "inline");
    }

    #[test]
    fn attachments_repairs_direct_kid_without_collapsing_names_holder_chain() {
        let mut pdf = load_one_page_pdf();
        let mut filespec = Dictionary::new();
        filespec.insert("F", Object::String(b"inline.txt".to_vec()));
        filespec.insert("UF", Object::String(b"inline.txt".to_vec()));
        let mut leaf = Dictionary::new();
        leaf.insert(
            "Limits",
            Object::Array(vec![
                Object::String(b"inline".to_vec()),
                Object::String(b"inline".to_vec()),
            ]),
        );
        leaf.insert(
            "Names",
            Object::Array(vec![
                Object::String(b"inline".to_vec()),
                Object::Dictionary(filespec),
            ]),
        );
        let mut tree = Dictionary::new();
        tree.insert("Kids", Object::Array(vec![Object::Dictionary(leaf)]));
        let mut names = Dictionary::new();
        names.insert("EmbeddedFiles", Object::Dictionary(tree));
        let names_holder_ref = crate::ObjectRef::new(902, 0);
        let names_ref = crate::ObjectRef::new(903, 0);
        pdf.set_object(names_holder_ref, Object::Reference(names_ref));
        pdf.set_object(names_ref, Object::Dictionary(names));
        let catalog_ref = pdf.root_ref().expect("catalog ref");
        let mut catalog = pdf
            .resolve_borrowed(catalog_ref)
            .expect("catalog")
            .as_dict()
            .expect("catalog dict")
            .clone();
        catalog.insert("Names", Object::Reference(names_holder_ref));
        pdf.set_object(catalog_ref, Object::Dictionary(catalog));
        for object_ref in pdf.dirty_object_refs() {
            pdf.clear_dirty(object_ref);
        }

        let result = build_attachments_section(&mut pdf).expect("build attachments");

        let entries = object_pairs(&result);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "inline");
        let catalog = pdf
            .resolve_borrowed(catalog_ref)
            .expect("catalog")
            .as_dict()
            .expect("catalog dict");
        assert_eq!(
            catalog.get("Names"),
            Some(&Object::Reference(names_holder_ref))
        );
        assert!(!pdf.is_dirty(catalog_ref));
        assert!(pdf.is_dirty(names_ref));
    }

    #[test]
    fn attachments_indirect_non_dictionary_filespec_is_minimal() {
        let mut pdf = load_one_page_pdf();
        let names_ref = crate::ObjectRef::new(910, 0);
        let ef_root_ref = crate::ObjectRef::new(911, 0);
        let filespec_ref = crate::ObjectRef::new(912, 0);
        patch_embedded_files(
            &mut pdf,
            names_ref,
            ef_root_ref,
            filespec_ref,
            Dictionary::new(),
            b"broken",
        );
        pdf.set_object(filespec_ref, Object::Integer(7));

        let result = build_attachments_section(&mut pdf).expect("build_attachments_section failed");
        let pairs = object_pairs(&result);
        assert_eq!(pairs.len(), 1);
        let entry = object_pairs(&pairs[0].1);
        assert_eq!(entry[0].1, serde_json::Value::Null);
        assert_eq!(entry[1].1, serde_json::Value::String("912 0 R".to_string()));
        assert_eq!(entry[2].1, object(vec![]));
        assert_eq!(entry[3].1, serde_json::Value::Null);
        assert_eq!(entry[4].1, serde_json::Value::Null);
        assert_eq!(entry[5].1, object(vec![]));
    }

    // ── attachments Test 2: attachment-two-page.pdf → 1 entry ────────────────

    #[test]
    fn attachments_fixture_has_one_entry() {
        let mut pdf = load_attachment_pdf();
        let result = build_attachments_section(&mut pdf).expect("build_attachments_section failed");
        let pairs = object_pairs(&result);
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
        let pairs = object_pairs(&result);
        let entry = object_pairs(&pairs[0].1);

        let get = |k: &str| -> &serde_json::Value {
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
            serde_json::Value::Null,
            "description must be null"
        );

        // filespec: must be a ref string
        let serde_json::Value::String(filespec_str) = get("filespec") else {
            panic!("filespec must be a String");
        };
        assert!(
            filespec_str.ends_with(" R"),
            "filespec must be a ref string like 'N M R', got: {filespec_str}"
        );

        // preferredname: "attachment.txt"
        assert_eq!(
            *get("preferredname"),
            serde_json::Value::String("attachment.txt".to_string()),
            "preferredname must be 'attachment.txt'"
        );

        // streams: must be an Object with at least one stream entry
        let streams = object_pairs(get("streams"));
        assert!(!streams.is_empty(), "streams must not be empty");

        // Each stream entry must have checksum, creationdate, mimetype, modificationdate
        for (stream_key, stream_val) in streams {
            let stream_entry = object_pairs(stream_val);
            let stream_keys: Vec<&str> = stream_entry.iter().map(|(k, _)| k.as_str()).collect();
            assert_eq!(
                stream_keys,
                vec!["checksum", "creationdate", "mimetype", "modificationdate"],
                "stream entry for {stream_key} must have 4 keys in alphabetical order"
            );
        }

        // ── qpdf-parity value assertions (matching qpdf --json output) ───────
        // names dict: /F and /UF both map to "attachment.txt"
        let names = object_pairs(get("names"));
        let name_keys: Vec<&str> = names.iter().map(|(k, _)| k.as_str()).collect();
        assert!(
            name_keys.contains(&"/F") || name_keys.contains(&"/UF"),
            "names must contain at least /F or /UF"
        );
        for (_key, val) in names {
            assert_eq!(
                val,
                serde_json::Value::String("attachment.txt".to_string()),
                "each name entry must be 'attachment.txt'"
            );
        }

        // preferredcontents must be a valid ref string (not null)
        let serde_json::Value::String(preferred_contents_str) = get("preferredcontents") else {
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
        let streams2 = object_pairs(get("streams"));
        for (stream_key, stream_val) in streams2 {
            let stream_entry = object_pairs(stream_val);
            let s_get = |k: &str| -> &serde_json::Value {
                stream_entry
                    .iter()
                    .find(|(key, _)| key == k)
                    .map(|(_, v)| v)
                    .unwrap_or_else(|| panic!("key '{k}' not found in stream {stream_key}"))
            };
            assert_eq!(
                *s_get("checksum"),
                serde_json::Value::String("542266a1f565c3e5d8cfbd55eb7dfa40".to_string()),
                "checksum mismatch for stream {stream_key}"
            );
            assert_eq!(
                *s_get("creationdate"),
                serde_json::Value::String("2026-01-01T00:00:00Z".to_string()),
                "creationdate mismatch for stream {stream_key}"
            );
            assert_eq!(
                *s_get("modificationdate"),
                serde_json::Value::String("2026-01-01T00:00:00Z".to_string()),
                "modificationdate mismatch for stream {stream_key}"
            );
            assert_eq!(
                *s_get("mimetype"),
                serde_json::Value::Null,
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
        let pairs = object_pairs(&result);

        assert_eq!(pairs.len(), 1, "expected 1 attachment");
        assert_eq!(pairs[0].0, "my-attachment.txt", "name mismatch");

        let entry = object_pairs(&pairs[0].1);

        let get = |k: &str| -> &serde_json::Value {
            entry
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v)
                .unwrap_or_else(|| panic!("key '{k}' not found"))
        };

        // description: bare string (no u: prefix)
        assert_eq!(
            *get("description"),
            serde_json::Value::String("My file description".to_string()),
            "description must be bare string"
        );

        // names: /F and /UF both present
        let names = object_pairs(get("names"));
        let name_keys: Vec<&str> = names.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            name_keys,
            vec!["/F", "/UF"],
            "names keys must be /F, /UF in order"
        );
        assert_eq!(
            names[0].1,
            serde_json::Value::String("f-name.txt".to_string()),
            "/F name mismatch"
        );
        assert_eq!(
            names[1].1,
            serde_json::Value::String("uf-name.txt".to_string()),
            "/UF name mismatch"
        );

        // preferredname: /UF wins over /F
        assert_eq!(
            *get("preferredname"),
            serde_json::Value::String("uf-name.txt".to_string()),
            "preferredname must be /UF (uf-name.txt)"
        );

        // preferredcontents: /EF/UF wins over /EF/F
        assert_eq!(
            *get("preferredcontents"),
            serde_json::Value::String(format!(
                "{} {} R",
                stream_uf_ref.number, stream_uf_ref.generation
            )),
            "preferredcontents must be /EF/UF ref"
        );

        // streams: /F and /UF both present
        let streams = object_pairs(get("streams"));
        let stream_keys: Vec<&str> = streams.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            stream_keys,
            vec!["/F", "/UF"],
            "streams keys must be /F, /UF in order"
        );

        // /F stream: check checksum hex, dates, mimetype
        let f_stream = object_pairs(&streams[0].1);
        let f_get = |k: &str| -> &serde_json::Value {
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
            serde_json::Value::String(expected_hex),
            "checksum must be lowercase hex"
        );

        // creationdate: D:20260101000000Z → 2026-01-01T00:00:00Z
        assert_eq!(
            *f_get("creationdate"),
            serde_json::Value::String("2026-01-01T00:00:00Z".to_string()),
            "creationdate mismatch"
        );

        // modificationdate: qpdf's QPDFJob::doJSONAttachments reads
        // /Params/CreationDate for *both* creationdate and modificationdate
        // (a copy-paste bug in qpdf 11.9.0, QPDFJob.cc:1319-1322, still
        // present on qpdf main) — /ModDate ("2026-02-02T12:00:00+09:00" if
        // it were read) is never actually reached.
        assert_eq!(
            *f_get("modificationdate"),
            serde_json::Value::String("2026-01-01T00:00:00Z".to_string()),
            "modificationdate must replicate qpdf's CreationDate/ModDate copy-paste bug"
        );

        // mimetype: bare "text/plain" (no "/" prefix, no "u:" prefix)
        assert_eq!(
            *f_get("mimetype"),
            serde_json::Value::String("text/plain".to_string()),
            "mimetype must be bare 'text/plain'"
        );

        // /UF stream: no /Subtype → mimetype null, no /Params → other fields null
        let uf_stream = object_pairs(&streams[1].1);
        let uf_get = |k: &str| -> &serde_json::Value {
            uf_stream
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v)
                .unwrap_or_else(|| panic!("key '{k}' not found in /UF stream"))
        };
        assert_eq!(
            *uf_get("mimetype"),
            serde_json::Value::Null,
            "/UF mimetype must be null"
        );
        assert_eq!(
            *uf_get("checksum"),
            serde_json::Value::Null,
            "/UF checksum must be null"
        );
        assert_eq!(
            *uf_get("creationdate"),
            serde_json::Value::Null,
            "/UF creationdate must be null"
        );
        assert_eq!(
            *uf_get("modificationdate"),
            serde_json::Value::Null,
            "/UF modificationdate must be null"
        );
    }

    #[test]
    fn attachments_preferredcontents_skips_a_non_reference_ef_entry() {
        let mut pdf = load_one_page_pdf();

        let stream_f_ref = crate::ObjectRef::new(930, 0);
        let filespec_ref = crate::ObjectRef::new(931, 0);
        let ef_root_ref = crate::ObjectRef::new(932, 0);
        let names_ref = crate::ObjectRef::new(933, 0);

        let mut stream_f_dict = Dictionary::new();
        stream_f_dict.insert("Type", Object::Name(b"EmbeddedFile".to_vec()));
        pdf.set_object(
            stream_f_ref,
            Object::Stream(crate::object::Stream::new(stream_f_dict, vec![])),
        );

        // /EF/UF is a direct (non-reference) value. qpdf's
        // getEmbeddedFileStream() requires an indirect stream (isStream()),
        // so a direct entry must be skipped in favor of the next-priority
        // key rather than accepted as the preferred contents.
        let mut ef_dict = Dictionary::new();
        ef_dict.insert("UF", Object::Boolean(true));
        ef_dict.insert("F", Object::Reference(stream_f_ref));

        let mut filespec = Dictionary::new();
        filespec.insert("Type", Object::Name(b"Filespec".to_vec()));
        filespec.insert("F", Object::String(b"f-name.txt".to_vec()));
        filespec.insert("EF", Object::Dictionary(ef_dict));

        patch_embedded_files(
            &mut pdf,
            names_ref,
            ef_root_ref,
            filespec_ref,
            filespec,
            b"attachment.txt",
        );

        let result = build_attachments_section(&mut pdf).expect("build_attachments_section failed");
        let pairs = object_pairs(&result);
        let entry = object_pairs(&pairs[0].1);
        let preferredcontents = entry
            .iter()
            .find(|(k, _)| k == "preferredcontents")
            .unwrap()
            .1
            .clone();
        assert_eq!(
            preferredcontents,
            serde_json::Value::String(format!(
                "{} {} R",
                stream_f_ref.number, stream_f_ref.generation
            )),
            "a non-reference /EF/UF entry must be skipped in favor of /EF/F"
        );
    }

    #[test]
    fn attachments_preferredname_skips_a_non_string_name_entry() {
        let mut pdf = load_one_page_pdf();

        let filespec_ref = crate::ObjectRef::new(934, 0);
        let ef_root_ref = crate::ObjectRef::new(935, 0);
        let names_ref = crate::ObjectRef::new(936, 0);

        // /UF is present but not a string. qpdf's getFilename() requires
        // isString(), so a wrong-type /UF must be skipped in favor of the
        // next-priority key (/F) rather than accepted as the preferred name.
        let mut filespec = Dictionary::new();
        filespec.insert("Type", Object::Name(b"Filespec".to_vec()));
        filespec.insert("UF", Object::Integer(42));
        filespec.insert("F", Object::String(b"f-name.txt".to_vec()));

        patch_embedded_files(
            &mut pdf,
            names_ref,
            ef_root_ref,
            filespec_ref,
            filespec,
            b"attachment.txt",
        );

        let result = build_attachments_section(&mut pdf).expect("build_attachments_section failed");
        let pairs = object_pairs(&result);
        let entry = object_pairs(&pairs[0].1);
        let preferredname = entry
            .iter()
            .find(|(k, _)| k == "preferredname")
            .unwrap()
            .1
            .clone();
        assert_eq!(
            preferredname,
            serde_json::Value::String("f-name.txt".to_string()),
            "a non-string /UF entry must be skipped in favor of /F"
        );
    }

    #[test]
    fn attachments_params_missing_and_invalid_values_become_null() {
        let mut pdf = load_one_page_pdf();
        let stream_f_ref = crate::ObjectRef::new(920, 0);
        let stream_uf_ref = crate::ObjectRef::new(921, 0);
        let filespec_ref = crate::ObjectRef::new(922, 0);
        let ef_root_ref = crate::ObjectRef::new(923, 0);
        let names_ref = crate::ObjectRef::new(924, 0);

        let mut f_dict = Dictionary::new();
        f_dict.insert("Params", Object::Dictionary(Dictionary::new()));
        pdf.set_object(
            stream_f_ref,
            Object::Stream(crate::object::Stream::new(f_dict, vec![])),
        );

        let mut uf_params = Dictionary::new();
        uf_params.insert("CreationDate", Object::String(b"invalid".to_vec()));
        uf_params.insert("ModDate", Object::String(b"invalid".to_vec()));
        let mut uf_dict = Dictionary::new();
        uf_dict.insert("Params", Object::Dictionary(uf_params));
        pdf.set_object(
            stream_uf_ref,
            Object::Stream(crate::object::Stream::new(uf_dict, vec![])),
        );

        let mut ef_dict = Dictionary::new();
        ef_dict.insert("F", Object::Reference(stream_f_ref));
        ef_dict.insert("UF", Object::Reference(stream_uf_ref));
        let mut filespec = Dictionary::new();
        filespec.insert("EF", Object::Dictionary(ef_dict));
        patch_embedded_files(
            &mut pdf,
            names_ref,
            ef_root_ref,
            filespec_ref,
            filespec,
            b"dates",
        );

        let result = build_attachments_section(&mut pdf).expect("build_attachments_section failed");
        let entry = object_pairs(&object_pairs(&result)[0].1);
        let streams = object_pairs(
            entry
                .iter()
                .find(|(key, _)| key == "streams")
                .expect("streams")
                .1
                .clone(),
        );
        for (_, stream) in streams {
            for (_, value) in object_pairs(stream) {
                assert_eq!(value, serde_json::Value::Null);
            }
        }
    }

    #[test]
    fn attachments_ef_entry_resolving_to_non_stream_is_skipped() {
        let mut pdf = load_one_page_pdf();
        let non_stream_ref = crate::ObjectRef::new(920, 0);
        let filespec_ref = crate::ObjectRef::new(922, 0);
        let ef_root_ref = crate::ObjectRef::new(923, 0);
        let names_ref = crate::ObjectRef::new(924, 0);

        pdf.set_object(non_stream_ref, Object::Integer(1));

        let mut ef_dict = Dictionary::new();
        ef_dict.insert("F", Object::Reference(non_stream_ref));
        let mut filespec = Dictionary::new();
        filespec.insert("EF", Object::Dictionary(ef_dict));
        patch_embedded_files(
            &mut pdf,
            names_ref,
            ef_root_ref,
            filespec_ref,
            filespec,
            b"nonstream",
        );

        let result = build_attachments_section(&mut pdf).expect("build_attachments_section failed");
        let entry = object_pairs(&object_pairs(&result)[0].1);
        let streams = object_pairs(
            entry
                .iter()
                .find(|(key, _)| key == "streams")
                .expect("streams")
                .1
                .clone(),
        );
        assert!(streams.is_empty(), "non-stream /EF entry must be skipped");
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
        let pairs = object_pairs(&result);

        // The inline filespec must produce an entry, not be silently dropped.
        assert_eq!(
            pairs.len(),
            1,
            "direct inline filespec must yield an attachments entry"
        );
        assert_eq!(pairs[0].0, "inline.txt");

        let entry = object_pairs(&pairs[0].1);
        let get = |k: &str| -> &serde_json::Value {
            entry
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v)
                .unwrap_or_else(|| panic!("key '{k}' missing"))
        };
        // No indirect reference → filespec must be null.
        assert_eq!(
            *get("filespec"),
            serde_json::Value::Null,
            "filespec must be null when the leaf value was a direct dictionary"
        );
        // The names sub-object still surfaces the inlined /F and /UF.
        let names = object_pairs(get("names"));
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
    fn parse_pdf_date_zero_offset_is_utc() {
        assert_eq!(
            parse_pdf_date(b"D:20260202120000+00'00'"),
            Some("2026-02-02T12:00:00Z".to_string())
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
        let pairs = object_pairs(enc);
        let encrypted = pairs
            .iter()
            .find(|(k, _)| k == "encrypted")
            .unwrap()
            .1
            .clone();
        assert_eq!(encrypted, serde_json::Value::Bool(false));
    }

    #[test]
    fn encrypt_section_plaintext_ownerpasswordmatched_false() {
        let mut pdf = load_one_page_pdf();
        let enc = build_encrypt_section(&mut pdf).expect("build_encrypt_section failed");
        let pairs = object_pairs(enc);
        let v = pairs
            .iter()
            .find(|(k, _)| k == "ownerpasswordmatched")
            .unwrap()
            .1
            .clone();
        assert_eq!(v, serde_json::Value::Bool(false));
        let v2 = pairs
            .iter()
            .find(|(k, _)| k == "userpasswordmatched")
            .unwrap()
            .1
            .clone();
        assert_eq!(v2, serde_json::Value::Bool(false));
    }

    #[test]
    fn encrypt_section_plaintext_parameters_are_zero_and_none() {
        let mut pdf = load_one_page_pdf();
        let enc = build_encrypt_section(&mut pdf).expect("build_encrypt_section failed");
        let pairs = object_pairs(enc);
        let params = pairs
            .iter()
            .find(|(k, _)| k == "parameters")
            .unwrap()
            .1
            .clone();
        let p = object_pairs(params);

        let get = |k: &str| p.iter().find(|(ky, _)| ky == k).unwrap().1.clone();
        assert_eq!(get("P"), number(0));
        assert_eq!(get("R"), number(0));
        assert_eq!(get("V"), number(0));
        assert_eq!(get("bits"), number(0));
        assert_eq!(get("filemethod"), serde_json::Value::String("none".into()));
        assert_eq!(get("method"), serde_json::Value::String("none".into()));
        assert_eq!(
            get("streammethod"),
            serde_json::Value::String("none".into())
        );
        assert_eq!(
            get("stringmethod"),
            serde_json::Value::String("none".into())
        );
        assert_eq!(get("key"), serde_json::Value::Null);
    }

    #[test]
    fn encrypt_section_plaintext_capabilities_all_true() {
        let mut pdf = load_one_page_pdf();
        let enc = build_encrypt_section(&mut pdf).expect("build_encrypt_section failed");
        let pairs = object_pairs(enc);
        let caps = pairs
            .iter()
            .find(|(k, _)| k == "capabilities")
            .unwrap()
            .1
            .clone();
        let cp = object_pairs(caps);
        for (_, v) in cp.iter() {
            assert_eq!(
                *v,
                serde_json::Value::Bool(true),
                "all plaintext capabilities must be true"
            );
        }
    }

    // ── Test 2: encrypted-r4 → encrypted=true, R=4, V=4, bits=128, methods AESv2 ──

    #[test]
    fn encrypt_section_encrypted_r4_encrypted_true() {
        let mut pdf = load_encrypted_r4_pdf();
        let enc = build_encrypt_section(&mut pdf).expect("build_encrypt_section failed");
        let pairs = object_pairs(enc);
        let v = pairs
            .iter()
            .find(|(k, _)| k == "encrypted")
            .unwrap()
            .1
            .clone();
        assert_eq!(v, serde_json::Value::Bool(true));
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
        let pairs = object_pairs(enc);
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
        assert_eq!(owner, serde_json::Value::Bool(false));
        assert_eq!(user, serde_json::Value::Bool(false));
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

    /// Build an [`ObjectHandle`] dictionary from `(name, value)` pairs, for
    /// tests exercising the `&BTreeMap<Vec<u8>, ObjectHandle>` lookups
    /// `cf_method_string`/`dict_name_str` perform on an already-resolved
    /// dictionary.
    fn oh_dict(entries: Vec<(&str, ObjectHandle)>) -> ObjectHandle {
        ObjectHandle::dictionary(
            entries
                .into_iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v))
                .collect(),
        )
    }

    #[test]
    fn cf_method_string_defaults_to_aesv2_for_revision_4() {
        let mut pdf = load_one_page_pdf();

        // No /CF at all.
        let encrypt = oh_dict(vec![("R", ObjectHandle::integer(4))])
            .as_dictionary()
            .unwrap();
        assert_eq!(
            cf_method_string(&mut pdf, &encrypt, Some("StdCF")).unwrap(),
            "AESv2"
        );

        // /CF exists but the selector is missing.
        let cf = oh_dict(vec![("OtherCF", oh_dict(vec![]))]);
        let encrypt = oh_dict(vec![("R", ObjectHandle::integer(4)), ("CF", cf)])
            .as_dictionary()
            .unwrap();
        assert_eq!(
            cf_method_string(&mut pdf, &encrypt, Some("StdCF")).unwrap(),
            "AESv2"
        );

        // Selector found but its /CFM is missing.
        let cf2 = oh_dict(vec![("StdCF", oh_dict(vec![]))]);
        let encrypt2 = oh_dict(vec![("R", ObjectHandle::integer(4)), ("CF", cf2)])
            .as_dictionary()
            .unwrap();
        assert_eq!(
            cf_method_string(&mut pdf, &encrypt2, Some("StdCF")).unwrap(),
            "AESv2"
        );
    }

    #[test]
    fn cf_method_string_defaults_to_aesv3_for_revision_5_and_6() {
        let mut pdf = load_one_page_pdf();
        for r in [5i64, 6] {
            let encrypt = oh_dict(vec![("R", ObjectHandle::integer(r))])
                .as_dictionary()
                .unwrap();
            assert_eq!(
                cf_method_string(&mut pdf, &encrypt, Some("StdCF")).unwrap(),
                "AESv3"
            );
            assert_eq!(cf_method_string(&mut pdf, &encrypt, None).unwrap(), "AESv3");
        }
    }

    #[test]
    fn cf_method_string_defaults_to_rc4_for_legacy_revisions() {
        let mut pdf = load_one_page_pdf();
        let encrypt = oh_dict(vec![("R", ObjectHandle::integer(3))])
            .as_dictionary()
            .unwrap();
        assert_eq!(
            cf_method_string(&mut pdf, &encrypt, Some("StdCF")).unwrap(),
            "RC4"
        );
        // No /R at all -> legacy default too.
        let empty = oh_dict(vec![]).as_dictionary().unwrap();
        assert_eq!(
            cf_method_string(&mut pdf, &empty, Some("StdCF")).unwrap(),
            "RC4"
        );
    }

    #[test]
    fn cf_method_string_identity_selector_still_returns_none() {
        // The "Identity" selector explicitly means no encryption for that
        // path and must keep its "none" behavior regardless of /R.
        let mut pdf = load_one_page_pdf();
        let encrypt = oh_dict(vec![("R", ObjectHandle::integer(4))])
            .as_dictionary()
            .unwrap();
        assert_eq!(
            cf_method_string(&mut pdf, &encrypt, Some("Identity")).unwrap(),
            "none"
        );
    }

    #[test]
    fn cf_method_string_maps_v2_and_none_cfm_values_and_falls_back_on_unrecognized() {
        let mut pdf = load_one_page_pdf();

        let cf_v2 = oh_dict(vec![(
            "StdCF",
            oh_dict(vec![("CFM", ObjectHandle::name(b"V2".to_vec()))]),
        )]);
        let encrypt_v2 = oh_dict(vec![("R", ObjectHandle::integer(4)), ("CF", cf_v2)])
            .as_dictionary()
            .unwrap();
        assert_eq!(
            cf_method_string(&mut pdf, &encrypt_v2, Some("StdCF")).unwrap(),
            "RC4",
            "/CFM /V2 must map to RC4"
        );

        let cf_none = oh_dict(vec![(
            "StdCF",
            oh_dict(vec![("CFM", ObjectHandle::name(b"None".to_vec()))]),
        )]);
        let encrypt_none = oh_dict(vec![("R", ObjectHandle::integer(4)), ("CF", cf_none)])
            .as_dictionary()
            .unwrap();
        assert_eq!(
            cf_method_string(&mut pdf, &encrypt_none, Some("StdCF")).unwrap(),
            "none",
            "/CFM /None must map to none"
        );

        // An unrecognized /CFM value falls back to the revision-based default.
        let cf_unknown = oh_dict(vec![(
            "StdCF",
            oh_dict(vec![("CFM", ObjectHandle::name(b"Unknown".to_vec()))]),
        )]);
        let encrypt_unknown = oh_dict(vec![("R", ObjectHandle::integer(5)), ("CF", cf_unknown)])
            .as_dictionary()
            .unwrap();
        assert_eq!(
            cf_method_string(&mut pdf, &encrypt_unknown, Some("StdCF")).unwrap(),
            "AESv3",
            "unrecognized /CFM must fall back to the revision-based default"
        );
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
        let pairs = object_pairs(enc);
        let v = pairs
            .iter()
            .find(|(k, _)| k == "ownerpasswordmatched")
            .unwrap()
            .1
            .clone();
        assert_eq!(v, serde_json::Value::Bool(true));
        let v2 = pairs
            .iter()
            .find(|(k, _)| k == "userpasswordmatched")
            .unwrap()
            .1
            .clone();
        assert_eq!(v2, serde_json::Value::Bool(true));
    }

    #[test]
    fn encrypt_section_encrypted_r4_parameters() {
        let mut pdf = load_encrypted_r4_pdf();
        let enc = build_encrypt_section(&mut pdf).expect("build_encrypt_section failed");
        let pairs = object_pairs(enc);
        let params = pairs
            .iter()
            .find(|(k, _)| k == "parameters")
            .unwrap()
            .1
            .clone();
        let p = object_pairs(params);

        let get = |k: &str| p.iter().find(|(ky, _)| ky == k).unwrap().1.clone();
        assert_eq!(get("P"), number(-4));
        assert_eq!(get("R"), number(4));
        assert_eq!(get("V"), number(4));
        assert_eq!(get("bits"), number(128));
        assert_eq!(get("filemethod"), serde_json::Value::String("AESv2".into()));
        assert_eq!(get("method"), serde_json::Value::String("AESv2".into()));
        assert_eq!(
            get("streammethod"),
            serde_json::Value::String("AESv2".into())
        );
        assert_eq!(
            get("stringmethod"),
            serde_json::Value::String("AESv2".into())
        );
        assert_eq!(get("key"), serde_json::Value::Null);
    }

    #[test]
    fn encrypt_section_legacy_v2_uses_rc4_methods() {
        let mut pdf = load_encrypted_r4_pdf();
        let encrypt_ref = match pdf.trailer().get("Encrypt") {
            Some(Object::Reference(reference)) => *reference,
            other => panic!("expected /Encrypt to be an indirect reference, got {other:?}"), // cov:ignore: fixture-shape guard
        };
        let mut encrypt = Dictionary::new();
        encrypt.insert("V", Object::Integer(2));
        encrypt.insert("R", Object::Integer(3));
        encrypt.insert("P", Object::Integer(-4));
        encrypt.insert("Length", Object::Integer(128));
        pdf.set_object(encrypt_ref, Object::Dictionary(encrypt));

        let section = build_encrypt_section(&mut pdf).expect("build_encrypt_section failed");
        let parameters = object_pairs(section)
            .into_iter()
            .find(|(key, _)| key == "parameters")
            .map(|(_, value)| object_pairs(value))
            .expect("parameters");
        let get = |key: &str| {
            parameters
                .iter()
                .find(|(candidate, _)| candidate == key)
                .map(|(_, value)| value)
                .unwrap_or_else(|| panic!("parameter {key} missing"))
        };
        for key in ["filemethod", "method", "streammethod", "stringmethod"] {
            assert_eq!(
                get(key),
                &serde_json::Value::String("RC4".into()),
                "legacy /V=2 {key} must use RC4"
            );
        }
    }

    #[test]
    fn encrypt_section_missing_v_r_p_default_to_zero() {
        // A spec-invalid /Encrypt dictionary missing /V, /R, and /P: each
        // must default to 0 rather than erroring, matching /Length's
        // existing "absent -> default" behavior.
        let mut pdf = load_encrypted_r4_pdf();
        let encrypt_ref = match pdf.trailer().get("Encrypt") {
            Some(Object::Reference(r)) => *r,
            other => panic!("expected /Encrypt to be an indirect reference, got {other:?}"), // cov:ignore: fixture-shape guard
        };
        // Authentication already happened at open time; replacing the
        // referenced object post-open only affects build_encrypt_section's
        // fresh re-read of /V, /R, /P.
        pdf.set_object(encrypt_ref, Object::Dictionary(Dictionary::new()));

        let enc = build_encrypt_section(&mut pdf).expect("build_encrypt_section failed");
        let pairs = object_pairs(enc);
        let params = pairs
            .iter()
            .find(|(k, _)| k == "parameters")
            .unwrap()
            .1
            .clone();
        let p = object_pairs(params);
        let get = |k: &str| p.iter().find(|(ky, _)| ky == k).unwrap().1.clone();
        assert_eq!(get("V"), number(0), "/V absent must default to 0");
        assert_eq!(get("R"), number(0), "/R absent must default to 0");
        assert_eq!(get("P"), number(0), "/P absent must default to 0");
    }

    #[test]
    fn encrypt_section_encrypted_r4_capabilities_all_true() {
        // /P = -4 = 0xFFFFFFFC → all permission bits set → all capabilities true
        let mut pdf = load_encrypted_r4_pdf();
        let enc = build_encrypt_section(&mut pdf).expect("build_encrypt_section failed");
        let pairs = object_pairs(enc);
        let caps = pairs
            .iter()
            .find(|(k, _)| k == "capabilities")
            .unwrap()
            .1
            .clone();
        let cp = object_pairs(caps);
        for (name, v) in cp.iter() {
            assert_eq!(
                *v,
                serde_json::Value::Bool(true),
                "capability {name} must be true for P=-4"
            );
        }
    }

    // ── Test 3: capabilities key order is alphabetical ─────────────────────────

    #[test]
    fn encrypt_section_capabilities_key_order() {
        let mut pdf = load_one_page_pdf();
        let enc = build_encrypt_section(&mut pdf).expect("build_encrypt_section failed");
        let pairs = object_pairs(enc);
        let caps = pairs
            .iter()
            .find(|(k, _)| k == "capabilities")
            .unwrap()
            .1
            .clone();
        let cp = object_pairs(caps);
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
        let pairs = object_pairs(enc);
        let params = pairs
            .iter()
            .find(|(k, _)| k == "parameters")
            .unwrap()
            .1
            .clone();
        let p = object_pairs(params);
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
        let pairs = object_pairs(enc);
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
        let p = object_pairs(enc_plain);
        let v = p
            .iter()
            .find(|(k, _)| k == "recovereduserpassword")
            .unwrap()
            .1
            .clone();
        assert_eq!(v, serde_json::Value::Null);

        let mut pdf_enc = load_encrypted_r4_pdf();
        let enc_enc = build_encrypt_section(&mut pdf_enc).expect("encrypted failed");
        let pe = object_pairs(enc_enc);
        let ve = pe
            .iter()
            .find(|(k, _)| k == "recovereduserpassword")
            .unwrap()
            .1
            .clone();
        assert_eq!(ve, serde_json::Value::Null);
    }

    // ── Test 7: composite build_test_document includes encrypt key ─────────────

    #[test]
    fn build_test_document_includes_encrypt_section() {
        let mut pdf = load_one_page_pdf();
        let v2 = build_test_document(&mut pdf, DecodeLevel::Generalized)
            .expect("build_test_document failed");
        let pairs = object_pairs(v2);
        let enc = pairs.iter().find(|(k, _)| k == "encrypt").map(|(_, v)| v);
        assert!(
            enc.is_some(),
            "encrypt key must be present in composite output"
        );
        assert!(
            matches!(enc.unwrap(), serde_json::Value::Object(_)),
            "encrypt must be an Object"
        );
    }

    // ══════════════════════════════════════════════════════════════════════════
    // flpdf-9hc.11.10: StreamDataMode tests
    // ══════════════════════════════════════════════════════════════════════════

    // Helper: extract the stream inner object for obj:7 0 R from the
    // qpdf key's raw object map.
    fn get_obj7_stream_inner(
        pdf: &mut crate::Pdf<std::io::Cursor<Vec<u8>>>,
        decode_level: DecodeLevel,
        mode: &StreamDataMode,
    ) -> Vec<(String, serde_json::Value)> {
        let serde_json::Value::Array(elems) = qpdf_key_value(pdf, decode_level, mode) else {
            panic!("expected Array");
        };
        let map_pairs = object_pairs(&elems[1]);
        let obj7 = map_pairs
            .iter()
            .find(|(k, _)| k == "obj:7 0 R")
            .map(|(_, v)| v)
            .expect("obj:7 0 R not found");
        let obj7_pairs = object_pairs(obj7);
        assert_eq!(obj7_pairs[0].0, "stream");
        let inner = object_pairs(&obj7_pairs[0].1);
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
            matches!(&inner[0].1, serde_json::Value::String(_)),
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
        let serde_json::Value::String(b64) = &inner[0].1 else {
            panic!("data is not a String");
        };
        let decoded = base64_decode_test_helper(b64);
        assert_eq!(
            decoded, raw_bytes,
            "DecodeLevel::None must emit the raw filter-encoded stream bytes"
        );
    }

    fn assert_decode_level_none_dict_is_normalized(inner: &[(String, serde_json::Value)]) {
        let dict = inner
            .iter()
            .find(|(key, _)| key == "dict")
            .map(|(_, value)| value)
            .expect("stream dict");
        let pairs = object_pairs(dict);
        assert!(
            !pairs.iter().any(|(key, _)| key == "/Length"),
            "payload emission must remove /Length at DecodeLevel::None"
        );
        assert!(
            pairs.iter().any(|(key, _)| key == "/Filter"),
            "DecodeLevel::None must preserve /Filter"
        );
    }

    #[test]
    fn stream_data_mode_inline_decode_level_none_normalizes_dict() {
        let mut pdf = load_one_page_pdf();
        let inner = get_obj7_stream_inner(&mut pdf, DecodeLevel::None, &StreamDataMode::Inline);

        assert_decode_level_none_dict_is_normalized(&inner);
    }

    #[test]
    fn stream_data_mode_file_decode_level_none_normalizes_dict() {
        let mut pdf = load_one_page_pdf();
        let temp = tempfile::tempdir().unwrap();
        let prefix = temp.path().join("out").to_string_lossy().into_owned();
        let inner = get_obj7_stream_inner(
            &mut pdf,
            DecodeLevel::None,
            &StreamDataMode::File { prefix },
        );

        assert_decode_level_none_dict_is_normalized(&inner);
    }

    // ── Test 2b: Inline + DecodeLevel::Generalized emits the filter-decoded
    //            content — matching `qpdf --decode-level=generalized`. ───────

    #[test]
    fn stream_data_mode_inline_decode_level_generalized_emits_decoded_bytes() {
        let mut pdf = load_one_page_pdf();

        let inner =
            get_obj7_stream_inner(&mut pdf, DecodeLevel::Generalized, &StreamDataMode::Inline);
        let serde_json::Value::String(b64) = &inner[0].1 else {
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
        // The writer creates the side file, so keep it out of the source tree.
        let temp = tempfile::tempdir().unwrap();
        let prefix = temp.path().join("out").to_string_lossy().into_owned();
        let inner = get_obj7_stream_inner(
            &mut pdf,
            DecodeLevel::Generalized,
            &StreamDataMode::File {
                prefix: prefix.clone(),
            },
        );
        // Must have exactly two keys: "datafile", "dict" (alphabetical)
        assert_eq!(inner.len(), 2, "File mode: expected 2 keys");
        assert_eq!(inner[0].0, "datafile", "first key must be 'datafile'");
        assert_eq!(inner[1].0, "dict", "second key must be 'dict'");

        // datafile must be "<prefix>-<obj_num>" for obj:7
        assert_eq!(
            inner[0].1,
            serde_json::Value::String(format_json_side_file_path(&prefix, 7))
        );
        assert!(std::path::Path::new(&format_json_side_file_path(&prefix, 7)).is_file());
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
        let temp = tempfile::tempdir().unwrap();
        for mode in &[
            StreamDataMode::None,
            StreamDataMode::Inline,
            StreamDataMode::File {
                prefix: temp.path().join("x").to_string_lossy().into_owned(),
            },
        ] {
            let serde_json::Value::Array(elems) =
                qpdf_key_value(&mut pdf, DecodeLevel::Generalized, mode)
            else {
                panic!("expected Array");
            };
            let map_pairs = object_pairs(&elems[1]);
            let trailer = map_pairs
                .iter()
                .find(|(k, _)| k == "trailer")
                .map(|(_, v)| v)
                .expect("trailer not found");
            let trailer_pairs = object_pairs(trailer);
            assert_eq!(
                trailer_pairs[0].0, "value",
                "trailer must have 'value' key regardless of StreamDataMode ({mode:?})"
            );
        }
    }

    // ── Test 5: build_test_document_with_options propagates stream_mode ────────

    #[test]
    fn build_test_document_with_options_inline_propagates_to_qpdf_key() {
        let mut pdf = load_one_page_pdf();
        let v2 = build_test_document_with_options(
            &mut pdf,
            DecodeLevel::Generalized,
            &StreamDataMode::Inline,
        )
        .expect("build failed");

        // Navigate: v2["qpdf"][1]["obj:7 0 R"]["stream"]["data"]
        let top_pairs = object_pairs(&v2);
        let qpdf_val = top_pairs
            .iter()
            .find(|(k, _)| k == "qpdf")
            .map(|(_, v)| v)
            .expect("qpdf key not found");
        let serde_json::Value::Array(qpdf_arr) = qpdf_val else {
            panic!("qpdf is not an Array");
        };
        let obj_map = object_pairs(&qpdf_arr[1]);
        let obj7 = obj_map
            .iter()
            .find(|(k, _)| k == "obj:7 0 R")
            .map(|(_, v)| v)
            .expect("obj:7 0 R not found in qpdf key");
        let obj7_pairs = object_pairs(obj7);
        let stream_inner = object_pairs(&obj7_pairs[0].1);
        // Inline mode: first key is "data"
        assert_eq!(stream_inner[0].0, "data",
            "Inline mode must produce 'data' key in stream entry via build_test_document_with_options");
        assert!(
            matches!(&stream_inner[0].1, serde_json::Value::String(_)),
            "data must be a String"
        );
    }

    // ── Test 7: build_test_document_with_options threads DecodeLevel to the
    //           qpdf key — None vs Generalized yield different stream data. ──

    #[test]
    fn build_test_document_with_options_threads_decode_level_to_qpdf_key() {
        // Extract obj:7 0 R inline "data" base64 for a given DecodeLevel.
        fn obj7_inline_data(decode_level: DecodeLevel) -> String {
            let mut pdf = load_one_page_pdf();
            let v2 =
                build_test_document_with_options(&mut pdf, decode_level, &StreamDataMode::Inline)
                    .expect("build failed");
            let top = object_pairs(&v2);
            let qpdf = top
                .iter()
                .find(|(k, _)| k == "qpdf")
                .map(|(_, v)| v)
                .expect("qpdf key");
            let serde_json::Value::Array(arr) = qpdf else {
                panic!("qpdf not Array");
            };
            let obj_map = object_pairs(&arr[1]);
            let obj7 = obj_map
                .iter()
                .find(|(k, _)| k == "obj:7 0 R")
                .map(|(_, v)| v)
                .expect("obj:7");
            let obj7_pairs = object_pairs(obj7);
            let stream_inner = object_pairs(&obj7_pairs[0].1);
            let serde_json::Value::String(b64) = &stream_inner[0].1 else {
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
            "Generalized must emit filter-decoded content via build_test_document_with_options"
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

    #[test]
    fn stream_payload_specialized_filter_obeys_decode_level() {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"RunLengthDecode".to_vec()));
        let raw_payload = b"specialized decode level";
        let encoded = crate::filters::encode_stream_data(&dict, raw_payload).expect("encode");
        let stream = Stream::new(dict, encoded.clone());

        let generalized = stream_payload_with_decode_status(&stream, DecodeLevel::Generalized);
        assert!(
            matches!(generalized.bytes, Cow::Borrowed(_)),
            "generalized decoding must preserve raw bytes for a specialized stream"
        );
        assert!(!generalized.decode_succeeded);
        assert_eq!(&*generalized.bytes, encoded.as_slice());

        let specialized = stream_payload_with_decode_status(&stream, DecodeLevel::Specialized);
        assert!(
            matches!(specialized.bytes, Cow::Owned(_)),
            "specialized decoding must decode RunLengthDecode"
        );
        assert!(specialized.decode_succeeded);
        assert_eq!(&*specialized.bytes, raw_payload);

        let all = stream_payload_with_decode_status(&stream, DecodeLevel::All);
        assert!(
            matches!(all.bytes, Cow::Owned(_)),
            "all decoding must include specialized filters"
        );
        assert!(all.decode_succeeded);
        assert_eq!(&*all.bytes, raw_payload);
    }

    #[test]
    fn stream_payload_decode_error_falls_back_to_raw() {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        let raw_payload = b"not a deflate stream";
        let stream = Stream::new(dict, raw_payload.to_vec());

        let payload = stream_payload_with_decode_status(&stream, DecodeLevel::Generalized);
        assert!(
            matches!(payload.bytes, Cow::Borrowed(_)),
            "a registered filter decode error must borrow the raw bytes"
        );
        assert!(!payload.decode_succeeded);
        assert_eq!(&*payload.bytes, raw_payload);
    }

    #[test]
    fn stream_payload_invalid_decode_params_falls_back_to_raw() {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        let mut decode_params = Dictionary::new();
        decode_params.insert("Predictor", Object::Integer(9));
        dict.insert("DecodeParms", Object::Dictionary(decode_params));
        let raw_payload = b"not a deflate stream";
        let stream = Stream::new(dict, raw_payload.to_vec());

        let payload = stream_payload_with_decode_status(&stream, DecodeLevel::Generalized);
        assert!(
            matches!(payload.bytes, Cow::Borrowed(_)),
            "an unfilterable decode-parameter set must borrow raw bytes"
        );
        assert!(!payload.decode_succeeded);
        assert_eq!(&*payload.bytes, raw_payload);
    }

    // ══════════════════════════════════════════════════════════════════════════
    // JsonKey unit tests  (flpdf-9hc.11.11)
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
}
