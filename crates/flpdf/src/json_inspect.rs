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

/// Classify a PDF string as either a "u:" text string or "b:" binary string.
///
/// A string is considered text (printable ASCII, no NUL) when every byte is
/// in 0x20..=0x7e and contains no NUL byte.  Otherwise it's binary.
///
/// This is a simplified rule — it does not decode PDFDocEncoding or UTF-16BE.
fn pdf_string_to_json_string(bytes: &[u8]) -> String {
    let is_text = bytes.iter().all(|&b| (0x20..=0x7e).contains(&b));
    if is_text {
        let text = std::str::from_utf8(bytes).unwrap_or("");
        format!("u:{text}")
    } else {
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        format!("b:{hex}")
    }
}

/// Convert a PDF [`Dictionary`] to a JSON object, with keys sorted alphabetically
/// (with the `/` prefix included in the sort key).
fn dict_to_json(dict: &Dictionary) -> Result<JsonValue, ConvertError> {
    // Dictionary::iter() already yields entries in lexicographic order of raw
    // bytes (BTreeMap). We prefix each key with "/" to match qpdf's JSON output.
    let mut pairs = Vec::new();
    for (raw_key, value) in dict.iter() {
        let key_str = format!("/{}", String::from_utf8_lossy(raw_key));
        let json_val = pdf_object_to_json(value)?;
        pairs.push((key_str, json_val));
    }
    // The pairs from BTreeMap iteration are already in raw-byte order, which is
    // equivalent to alphabetical on the /Name strings since `/` (0x2F) is fixed.
    // Sort by full "/Name" string to be explicit.
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
        Object::Name(bytes) => {
            let name = format!("/{}", String::from_utf8_lossy(bytes));
            Ok(JsonValue::String(name))
        }
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
    // Collect all object refs from the xref table (skip object 0).
    // Free/deleted entries will resolve() to Object::Null and be skipped below.
    let all_refs: Vec<crate::ObjectRef> = pdf
        .object_refs()
        .into_iter()
        .filter(|r| r.number != 0)
        .collect();

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
            Object::Null => {
                // Free/missing objects resolve to Null — skip them.
                continue;
            }
            other => {
                // Non-stream: emit { "value": <json> }
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
        // Binary bytes with high-bit set.
        let result = pdf_object_to_json(&Object::String(vec![0x2d, 0xc7, 0x80])).unwrap();
        assert_eq!(result, JsonValue::String("b:2dc780".to_string()));
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
}
