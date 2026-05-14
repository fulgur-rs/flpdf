//! qpdf JSON v2 inspection builders.
//!
//! Provides the structural frame for qpdf `--json` output.  Each builder
//! returns a [`JsonValue`] that the caller can extend with per-section data
//! (pages, objects, …) in later subtasks.

use crate::json::JsonValue;

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
}
