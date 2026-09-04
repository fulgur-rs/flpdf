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
//! Production object and stream values route through [`ObjectHandle`] and
//! [`crate::document_json`].

pub(crate) fn qpdf_resolve_top_level_object<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    start: ObjectRef,
) -> Result<ObjectHandle, ConvertError> {
    // JSON object keys already name the canonical indirect object. The final
    // raw-object route no longer has a separate `ObjectValue::Reference`
    // carrier to chase, so resolving once is both sufficient and preserves
    // the document-owned cache identity.
    Ok(pdf.resolve_qpdf_json_handle(start)?)
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
    let stream = qpdf_resolve_top_level_object(pdf, object_ref)?;
    if stream.as_stream_dict().is_none() {
        return Ok(None);
    }
    Ok(Some(
        stream_payload_with_decode_status(&stream, decode_level)?
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
/// (NaN or infinity), or a [`ConvertError::PdfError`] when `handle` exceeds the
/// maximum nesting depth. Direct `ObjectHandle` graphs can be cyclic, so the
/// serializer bounds recursion rather than assuming acyclic input.
pub fn pdf_object_to_json(handle: &ObjectHandle) -> Result<Json, ConvertError> {
    pdf_object_to_json_with_version(handle, QPDF_JSON_VERSION)
}

/// Convert a PDF object handle using the requested qpdf JSON object encoding.
pub(crate) fn pdf_object_to_json_with_version(
    handle: &ObjectHandle,
    version: i32,
) -> Result<Json, ConvertError> {
    handle
        .get_json(version, false)
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
pub(crate) fn pdf_dest_to_json_with_version(
    handle: &ObjectHandle,
    version: i32,
) -> Result<Json, ConvertError> {
    handle
        .get_json(version, true)
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
    File { prefix: Vec<u8> },
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
/// The supplied handle is a resolved stream whose payload is still
/// filter-encoded.
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
/// The returned [`Cow`] retains the historical API shape. A handle owns its
/// payload behind an `Rc`, so raw fallback paths clone that payload rather than
/// returning a borrow whose owner would be dropped at this function boundary.
///
/// # Errors
///
/// Returns an error when the stream's raw bytes cannot be fetched at all
/// (non-stream handle, or original-source I/O/parse failure) — this mirrors
/// qpdf's `QPDF_Stream::writeStreamJSON`, which throws `std::logic_error`
/// when even the undecoded raw-bytes retry attempt fails
/// (`libqpdf/QPDF_Stream.cc:250-269`). A *filter decode* failure is not an
/// error here: it falls back to the raw bytes, matching that same qpdf
/// function's retry-with-`qpdf_dl_none` behavior for a filtered attempt.
pub fn stream_payload_for_decode_level(
    stream: &ObjectHandle,
    decode_level: DecodeLevel,
) -> crate::Result<Cow<'_, [u8]>> {
    Ok(stream_payload_with_decode_status(stream, decode_level)?.bytes)
}

pub(crate) struct StreamPayload<'a> {
    pub(crate) bytes: Cow<'a, [u8]>,
}

pub(crate) fn stream_payload_with_decode_status(
    stream: &ObjectHandle,
    decode_level: DecodeLevel,
) -> crate::Result<StreamPayload<'_>> {
    // qpdf `QPDF_Stream::getRawStreamData` (`libqpdf/QPDF_Stream.cc:362-376`):
    // replaced data is returned directly, and original data is read lazily
    // through the owning document at the parsed offset. Unlike
    // `ObjectHandle::as_stream_data`, this also covers the lazy-original
    // case rather than only the replaced-data case, so a freshly parsed
    // stream that was never `replaceStreamData`-d still yields its real
    // content instead of an empty payload.
    let raw_data = stream.get_raw_stream_data()?;
    if matches!(decode_level, DecodeLevel::None) {
        return Ok(StreamPayload {
            bytes: Cow::Owned(raw_data.as_ref().clone()),
        });
    }

    // `get_raw_stream_data` above only succeeds for a handle whose resolved
    // value is `ObjectValue::Stream`, and that success dereferences `stream`
    // as a side effect — so `as_stream_dict` is guaranteed `Some` here, the
    // same as qpdf's `QPDF_Stream` always carrying its own `stream_dict`
    // member once a `QPDF_Stream` instance exists at all.
    let stream_dict = stream
        .as_stream_dict()
        .expect("get_raw_stream_data succeeded, so stream must have a stream dict");
    let Some(capabilities) = crate::filters::stream_filter_capabilities(&stream_dict) else {
        return Ok(StreamPayload {
            bytes: Cow::Owned(raw_data.as_ref().clone()),
        });
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
        return Ok(StreamPayload {
            bytes: Cow::Owned(raw_data.as_ref().clone()),
        });
    }

    match crate::filters::decode_stream_data(&stream_dict, raw_data.as_ref()) {
        Ok(decoded) => Ok(StreamPayload {
            bytes: Cow::Owned(decoded),
        }),
        Err(_) => Ok(StreamPayload {
            bytes: Cow::Owned(raw_data.as_ref().clone()),
        }),
    }
}

// ── JsonKey ────────────────────────────────────────────────

/// A top-level qpdf JSON key that the caller may request via `--json-key`.
///
/// `Objects` and `Objectinfo` are retained for JSON v1; qpdf replaces those
/// two top-level sections with `Qpdf` in JSON v2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JsonKey {
    Acroform,
    Attachments,
    Encrypt,
    Outlines,
    Pagelabels,
    Pages,
    Qpdf,
    Objects,
    Objectinfo,
}

impl JsonKey {
    /// All qpdf JSON key names in alphabetical order.
    pub const ALL_NAMES: &'static [&'static str] = &[
        "acroform",
        "attachments",
        "encrypt",
        "objectinfo",
        "objects",
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
            "objects" => Some(JsonKey::Objects),
            "objectinfo" => Some(JsonKey::Objectinfo),
            _ => None,
        }
    }

    /// The top-level key name represented by this selector.
    pub(crate) fn output_key_name(self) -> &'static str {
        match self {
            JsonKey::Acroform => "acroform",
            JsonKey::Attachments => "attachments",
            JsonKey::Encrypt => "encrypt",
            JsonKey::Outlines => "outlines",
            JsonKey::Pagelabels => "pagelabels",
            JsonKey::Pages => "pages",
            JsonKey::Qpdf => "qpdf",
            JsonKey::Objects => "objects",
            JsonKey::Objectinfo => "objectinfo",
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
    /// - The qpdf object-reference spelling `"N G R"` is also accepted; qpdf
    ///   uses this form when an argument is passed as one token from its test
    ///   driver (`QPDFJob.cc:929-952`).
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
        let reference_parts: Vec<&str> = s.split_whitespace().collect();
        if reference_parts.len() == 3 && reference_parts[2] == "R" {
            let number = reference_parts[0].parse::<u32>().ok()?;
            let generation = reference_parts[1].parse::<u16>().ok()?;
            return Some(JsonObjectSelector::Object { number, generation });
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

use crate::json::Json;
use crate::object_handle::{ObjectHandle, ObjectJsonError};
use crate::object_ref::ObjectRef;
use crate::pipeline::PipelineError;
use crate::Pdf;
use std::borrow::Cow;
use std::io::{Read, Seek, Write};

pub(crate) use crate::pdf_string::decode_pdf_text_string;

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
mod tests {
    use super::{
        convert_object_json_error, pdf_object_to_json, qpdf_raw_stream_payload,
        stream_payload_for_decode_level, ConvertError, DecodeLevel, JsonObjectSelector,
        JsonOutputError,
    };
    use crate::object_handle::{ObjectHandle, ObjectJsonError};
    use crate::pipeline::PipelineError;
    use crate::Pdf;
    use std::io::Cursor;
    use std::rc::Rc;

    fn stream(data: Vec<u8>, filter: Option<&[u8]>) -> ObjectHandle {
        let mut entries = vec![(
            b"/Length".to_vec(),
            ObjectHandle::integer(data.len() as i64),
        )];
        if let Some(filter) = filter {
            entries.push((b"/Filter".to_vec(), ObjectHandle::name(filter.to_vec())));
        }
        ObjectHandle::stream(ObjectHandle::dictionary(entries), Rc::new(data))
    }

    fn stream_pdf() -> Pdf<Cursor<Vec<u8>>> {
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let objects = [
            b"<< /Type /Catalog /Pages 2 0 R >>".as_slice(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".as_slice(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 4 0 R >>".as_slice(),
            b"<< /Length 5 >>\nstream\nhello\nendstream".as_slice(),
        ];
        let mut offsets = Vec::new();
        for (index, body) in objects.iter().enumerate() {
            offsets.push(bytes.len());
            bytes.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
            bytes.extend_from_slice(body);
            bytes.extend_from_slice(b"\nendobj\n");
        }
        let xref = bytes.len();
        bytes.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        for offset in offsets {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
        );
        Pdf::open(Cursor::new(bytes)).expect("stream fixture opens")
    }

    #[test]
    fn conversion_errors_keep_their_public_text_and_categories() {
        assert_eq!(
            ConvertError::NonFiniteFloat.to_string(),
            "non-finite float cannot be serialized as JSON"
        );
        assert_eq!(
            ConvertError::PdfError("broken PDF".to_owned()).to_string(),
            "PDF error: broken PDF"
        );
        assert_eq!(
            ConvertError::JsonError("bad JSON".to_owned()).to_string(),
            "JSON error: bad JSON"
        );

        assert!(matches!(
            JsonOutputError::from(ObjectJsonError::Pipeline(PipelineError::logic("pipeline"))),
            JsonOutputError::Pipeline(_)
        ));
        assert!(matches!(
            JsonOutputError::from(ObjectJsonError::NonFiniteFloat),
            JsonOutputError::Convert(ConvertError::NonFiniteFloat)
        ));
        assert!(matches!(
            JsonOutputError::from(ObjectJsonError::Json("json".to_owned())),
            JsonOutputError::Convert(ConvertError::JsonError(message)) if message == "json"
        ));
        assert!(matches!(
            JsonOutputError::from(ObjectJsonError::Pdf("pdf".to_owned())),
            JsonOutputError::Convert(ConvertError::PdfError(message)) if message == "pdf"
        ));
        assert!(matches!(
            JsonOutputError::from(ObjectJsonError::UnsupportedVersion(3)),
            JsonOutputError::Convert(ConvertError::PdfError(message))
                if message.contains("only version 1 or 2")
        ));
    }

    #[test]
    fn standalone_json_and_raw_stream_routes_use_canonical_handles() {
        let scalar = ObjectHandle::integer(7);
        assert_eq!(
            pdf_object_to_json(&scalar)
                .expect("scalar JSON")
                .unparse()
                .expect("scalar JSON bytes"),
            b"7"
        );
        assert!(matches!(
            pdf_object_to_json(&ObjectHandle::real(f64::NAN)),
            Err(ConvertError::NonFiniteFloat)
        ));

        let mut pdf = stream_pdf();
        assert_eq!(
            qpdf_raw_stream_payload(&mut pdf, crate::ObjectRef::new(4, 0), DecodeLevel::None)
                .expect("raw stream payload")
                .expect("stream object"),
            b"hello"
        );
        assert_eq!(
            qpdf_raw_stream_payload(&mut pdf, crate::ObjectRef::new(1, 0), DecodeLevel::All)
                .expect("non-stream payload lookup"),
            None
        );
    }

    #[test]
    fn stream_decode_levels_cover_raw_specialized_unsupported_and_corrupt_paths() {
        let plain = stream(b"plain".to_vec(), None);
        assert_eq!(
            stream_payload_for_decode_level(&plain, DecodeLevel::None)
                .expect("raw payload")
                .as_ref(),
            b"plain"
        );
        assert_eq!(
            stream_payload_for_decode_level(&plain, DecodeLevel::Generalized)
                .expect("plain decoded payload")
                .as_ref(),
            b"plain"
        );

        let unknown = stream(b"raw".to_vec(), Some(b"UnknownDecode"));
        assert_eq!(
            stream_payload_for_decode_level(&unknown, DecodeLevel::All)
                .expect("unknown filter raw fallback")
                .as_ref(),
            b"raw"
        );

        let specialized = stream(b"raw".to_vec(), Some(b"RunLengthDecode"));
        assert_eq!(
            stream_payload_for_decode_level(&specialized, DecodeLevel::Generalized)
                .expect("specialized filter remains raw")
                .as_ref(),
            b"raw"
        );
        assert!(
            !stream_payload_for_decode_level(&specialized, DecodeLevel::Specialized)
                .expect("specialized filter is selected at specialized level")
                .is_empty()
        );

        let corrupt = stream(b"not-flate".to_vec(), Some(b"FlateDecode"));
        assert_eq!(
            stream_payload_for_decode_level(&corrupt, DecodeLevel::All)
                .expect("corrupt filter raw fallback")
                .as_ref(),
            b"not-flate"
        );
    }

    #[test]
    fn selectors_and_object_error_conversion_cover_all_public_boundaries() {
        assert_eq!(DecodeLevel::None.as_qpdf_str(), "none");
        assert_eq!(DecodeLevel::Generalized.as_qpdf_str(), "generalized");
        assert_eq!(DecodeLevel::Specialized.as_qpdf_str(), "specialized");
        assert_eq!(DecodeLevel::All.as_qpdf_str(), "all");
        assert_eq!(
            JsonObjectSelector::from_str("trailer"),
            Some(JsonObjectSelector::Trailer)
        );
        assert_eq!(
            JsonObjectSelector::from_str("12,3"),
            Some(JsonObjectSelector::Object {
                number: 12,
                generation: 3
            })
        );
        assert!(JsonObjectSelector::from_str("").is_none());
        assert!(JsonObjectSelector::from_str("1,2,3").is_none());
        assert!(JsonObjectSelector::from_str("1,").is_none());

        assert!(matches!(
            convert_object_json_error(ObjectJsonError::NonFiniteFloat),
            ConvertError::NonFiniteFloat
        ));
        assert!(matches!(
            convert_object_json_error(ObjectJsonError::Json("json".to_owned())),
            ConvertError::JsonError(message) if message == "json"
        ));
        assert!(matches!(
            convert_object_json_error(ObjectJsonError::Pipeline(PipelineError::logic("pipeline"))),
            ConvertError::PdfError(message) if message.contains("pipeline")
        ));
        assert!(matches!(
            convert_object_json_error(ObjectJsonError::Pdf("pdf".to_owned())),
            ConvertError::PdfError(message) if message == "pdf"
        ));
        assert!(matches!(
            convert_object_json_error(ObjectJsonError::UnsupportedVersion(4)),
            ConvertError::PdfError(message) if message.contains("only version 1 or 2")
        ));
    }
}
