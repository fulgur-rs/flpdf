//! Per-object semantic comparison matching qpdf v11.9.0's
//! `compareObjects(label, act, exp)` from `compare-for-test/qpdf-test-compare.cc`.
//!
//! Returns the empty string when the two objects match, or a diff-reason
//! string prefixed with `label` (matching qpdf's exact wording) otherwise.
//! Covers the non-stream branch (type-code gate, then `write_pdf`-bytes
//! compare) and the stream branch (`/Length`-stripped dict compare,
//! `/XRef` data skip, `/FlateDecode` decompress-and-compare).

use flpdf::{Dictionary, Object};

/// Compare two resolved [`Object`]s the way qpdf's `qpdf-test-compare` does.
///
/// Returns `""` when they match, or `"<label>: <reason>"` when they differ.
/// `reason` is one of qpdf's fixed set: `different types`, `object contents
/// differ`, `stream dictionaries differ`, `stream data size differs`,
/// `stream data differs`.
///
/// Non-stream objects are compared by their [`Object::write_pdf`] bytes,
/// which is the equivalent of qpdf's `unparseResolved()` on an already-
/// resolved handle: nested [`Object::Reference`]s render as `N G R` and are
/// not further dereferenced. Comparison by write-bytes (not [`PartialEq`])
/// matches qpdf's `unparse`-based check — e.g. two "reals" that serialize
/// the same are treated as equal even when their enum discriminants differ.
///
/// Stream objects are compared by (a) their dictionaries with `/Length`
/// stripped, then (b) their data — skipped when `/Type == /XRef`,
/// decompressed via [`flpdf::filters::decode_stream_data`] when
/// `/FlateDecode` appears in `/Filter`, otherwise raw.
pub fn compare_objects(label: &str, act: &Object, exp: &Object) -> String {
    if type_code(act) != type_code(exp) {
        return format!("{label}: different types");
    }
    if let (Object::Stream(a_s), Object::Stream(e_s)) = (act, exp) {
        return compare_streams(label, a_s, e_s);
    }
    let mut a = Vec::new();
    act.write_pdf(&mut a);
    let mut e = Vec::new();
    exp.write_pdf(&mut e);
    if a != e {
        return format!("{label}: object contents differ");
    }
    String::new()
}

fn compare_streams(label: &str, a_s: &flpdf::Stream, e_s: &flpdf::Stream) -> String {
    // Strip /Length before dict compare (Length necessarily differs between
    // two runs whose compressed payload differs — even for identical decoded
    // content). Cloning the dict is unavoidable (we need &mut Dictionary to
    // strip, but only own &Stream); the stream's `data` payload is NOT
    // cloned by this — it lives in `Stream.data`, outside the dict.
    let mut a_dict = a_s.dict.clone();
    let mut e_dict = e_s.dict.clone();
    a_dict.remove(b"Length");
    e_dict.remove(b"Length");

    // Detect /XRef and /FlateDecode against `&a_dict` BEFORE moving it into
    // Object::Dictionary for serialization — avoids a second clone.
    let is_xref = is_xref_stream(&a_dict);
    let uncompress = filter_uses_flatedecode(&a_dict);

    let mut a_dict_bytes = Vec::new();
    Object::Dictionary(a_dict).write_pdf(&mut a_dict_bytes);
    let mut e_dict_bytes = Vec::new();
    Object::Dictionary(e_dict).write_pdf(&mut e_dict_bytes);
    if a_dict_bytes != e_dict_bytes {
        return format!("{label}: stream dictionaries differ");
    }

    // qpdf skips the data body for xref streams: same dict is enough,
    // because both writers will have derived the xref-body bytes from the
    // (matching) live object set.
    if is_xref {
        return String::new();
    }

    // Compare payload as `&[u8]` slices — never clone the raw bytes. When
    // /FlateDecode is present, decompress both sides through flpdf's filter
    // chain (which honors /DecodeParms / Predictor). qpdf's oracle
    // propagates decode failures as thrown exceptions caught by main() and
    // printed as the exit-2 error. Our `compare_objects` returns String and
    // can't itself Err, so a decode failure surfaces as a diff string that
    // reaches the same exit-2 path in the CLI. Documented deviation.
    let decoded_a: Vec<u8>;
    let decoded_e: Vec<u8>;
    let (a_slice, e_slice): (&[u8], &[u8]) = if uncompress {
        decoded_a = match flpdf::filters::decode_stream_data(&a_s.dict, &a_s.data) {
            Ok(v) => v,
            Err(err) => return format!("{label}: decode error: {err}"),
        };
        decoded_e = match flpdf::filters::decode_stream_data(&e_s.dict, &e_s.data) {
            Ok(v) => v,
            Err(err) => return format!("{label}: decode error: {err}"),
        };
        (&decoded_a, &decoded_e)
    } else {
        (&a_s.data, &e_s.data)
    };
    if a_slice.len() != e_slice.len() {
        return format!("{label}: stream data size differs");
    }
    if a_slice != e_slice {
        return format!("{label}: stream data differs");
    }
    String::new()
}

// Direct match on /Type without resolving through the pdf. This mirrors
// qpdf's `isNameAndEquals` and stays consistent with
// `flpdf::filters::decode_stream_data`, which also reads `dict.get("Filter")`
// without resolving. A detector that resolved indirect refs while the
// decoder did not would diverge from the decode path this predicate gates.
fn is_xref_stream(d: &Dictionary) -> bool {
    matches!(d.get(b"Type"), Some(Object::Name(n)) if n.as_slice() == b"XRef")
}

// Same rationale as `is_xref_stream`: no indirect-ref resolution here.
fn filter_uses_flatedecode(d: &Dictionary) -> bool {
    match d.get(b"Filter") {
        Some(Object::Name(n)) => n.as_slice() == b"FlateDecode",
        Some(Object::Array(items)) => items
            .iter()
            .any(|it| matches!(it, Object::Name(n) if n.as_slice() == b"FlateDecode")),
        _ => false,
    }
}

// Numeric equivalence relation over Object variants — the actual numbers do
// not need to match qpdf's `QPDFObject::object_type_e` enum values, only the
// same→same relation. `Real` and `RealLiteral` share one code because both
// are PDF reals (qpdf's `ot_real`); flpdf splits them internally so it can
// preserve the source literal for byte-identical parity.
fn type_code(o: &Object) -> u8 {
    match o {
        Object::Null => 0,
        Object::Boolean(_) => 1,
        Object::Integer(_) => 2,
        Object::Real(_) | Object::RealLiteral { .. } => 3,
        Object::Name(_) => 4,
        Object::String(_) => 5,
        Object::Array(_) => 6,
        Object::Dictionary(_) => 7,
        Object::Stream(_) => 8,
        Object::Reference(_) => 9,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use flpdf::{ObjectRef, Stream};
    use std::io::Write;

    #[test]
    fn identical_integers_match() {
        assert_eq!(
            compare_objects("obj", &Object::Integer(42), &Object::Integer(42)),
            ""
        );
    }

    #[test]
    fn different_integers_report_object_contents_differ() {
        assert_eq!(
            compare_objects("obj", &Object::Integer(1), &Object::Integer(2)),
            "obj: object contents differ"
        );
    }

    #[test]
    fn different_type_codes_report_different_types() {
        assert_eq!(
            compare_objects("obj", &Object::Integer(1), &Object::Name(b"n".to_vec())),
            "obj: different types"
        );
    }

    #[test]
    fn equal_dictionaries_with_nested_references_match() {
        // Nested `Object::Reference` renders as "N G R" via `write_pdf` and
        // is NOT further dereferenced. Two dicts with the same shape and the
        // same references therefore serialize identically.
        let build = || {
            let mut d = Dictionary::new();
            d.insert(b"Type", Object::Name(b"Catalog".to_vec()));
            d.insert(b"Pages", Object::Reference(ObjectRef::new(3, 0)));
            d.insert(b"Version", Object::Name(b"1.7".to_vec()));
            Object::Dictionary(d)
        };
        assert_eq!(compare_objects("obj", &build(), &build()), "");
    }

    #[test]
    fn dictionary_insert_order_does_not_matter() {
        // `Dictionary` is BTreeMap-backed so `write_pdf` output is sorted by
        // key. Two dicts populated in different orders still compare equal.
        let mut a = Dictionary::new();
        a.insert(b"A", Object::Integer(1));
        a.insert(b"B", Object::Integer(2));
        a.insert(b"C", Object::Integer(3));
        let mut b = Dictionary::new();
        b.insert(b"C", Object::Integer(3));
        b.insert(b"A", Object::Integer(1));
        b.insert(b"B", Object::Integer(2));
        assert_eq!(
            compare_objects("obj", &Object::Dictionary(a), &Object::Dictionary(b)),
            ""
        );
    }

    #[test]
    fn reference_vs_integer_report_different_types() {
        assert_eq!(
            compare_objects(
                "obj",
                &Object::Reference(ObjectRef::new(3, 0)),
                &Object::Integer(3),
            ),
            "obj: different types"
        );
    }

    #[test]
    fn equal_strings_match() {
        assert_eq!(
            compare_objects(
                "obj",
                &Object::String(b"hello".to_vec()),
                &Object::String(b"hello".to_vec()),
            ),
            ""
        );
    }

    #[test]
    fn real_and_real_literal_with_equal_write_bytes_match() {
        // `Object::Real(1.5)` and `Object::RealLiteral { value: 1.5, literal:
        // b"1.5" }` are NOT equal under `PartialEq` (different enum variants)
        // but both `write_pdf` to `b"1.5"`. A qpdf-parity implementation must
        // treat them as equal (matches qpdf's `unparse`-based comparison).
        let real = Object::Real(1.5);
        let real_literal = Object::RealLiteral {
            value: 1.5,
            literal: b"1.5".to_vec(),
        };
        // Sanity: PartialEq disagrees but write_pdf agrees.
        assert_ne!(real, real_literal);
        let (mut a, mut b) = (Vec::new(), Vec::new());
        real.write_pdf(&mut a);
        real_literal.write_pdf(&mut b);
        assert_eq!(a, b);

        // Both are PDF reals → same type_code → falls into the write-bytes
        // compare path, which reports no diff.
        assert_eq!(compare_objects("obj", &real, &real_literal), "");
    }

    // ---------- stream branch ----------

    fn zlib(bytes: &[u8], level: Compression) -> Vec<u8> {
        let mut e = ZlibEncoder::new(Vec::new(), level);
        e.write_all(bytes).unwrap();
        e.finish().unwrap()
    }

    fn raw_stream(len: i64, data: Vec<u8>) -> Stream {
        let mut d = Dictionary::new();
        d.insert(b"Length", Object::Integer(len));
        Stream::new(d, data)
    }

    #[test]
    fn identical_streams_match() {
        let a = Object::Stream(raw_stream(10, b"0123456789".to_vec()));
        let e = Object::Stream(raw_stream(10, b"0123456789".to_vec()));
        assert_eq!(compare_objects("1 0", &a, &e), "");
    }

    #[test]
    fn stream_length_only_diff_matches_when_data_equal() {
        // Same raw data but different /Length values → Length is stripped
        // before dict compare, so the dicts compare equal and the raw data
        // compare succeeds. (Yes, /Length disagreeing with data length is
        // "invalid" PDF, but the compare tool must not care — that's the
        // whole point of stripping it.)
        let a = Object::Stream(raw_stream(1, b"same".to_vec()));
        let e = Object::Stream(raw_stream(999, b"same".to_vec()));
        assert_eq!(compare_objects("2 0", &a, &e), "");
    }

    #[test]
    fn stream_dict_type_diff_reports_stream_dictionaries_differ() {
        let mut ad = Dictionary::new();
        ad.insert(b"Length", Object::Integer(3));
        ad.insert(b"Type", Object::Name(b"Foo".to_vec()));
        let mut ed = Dictionary::new();
        ed.insert(b"Length", Object::Integer(3));
        ed.insert(b"Type", Object::Name(b"Bar".to_vec()));
        let a = Object::Stream(Stream::new(ad, b"abc".to_vec()));
        let e = Object::Stream(Stream::new(ed, b"abc".to_vec()));
        assert_eq!(
            compare_objects("3 0", &a, &e),
            "3 0: stream dictionaries differ"
        );
    }

    #[test]
    fn xref_stream_skips_data_compare() {
        // /Type /XRef with the same dict but wildly differing data should
        // still match — qpdf skips xref-stream data validation entirely.
        // The two sides only differ in .data (raw payload); /Length is set
        // to the same placeholder on both sides so the pre-strip dict-bytes
        // compare doesn't reveal the payload difference through /Length.
        let make = |data: Vec<u8>| {
            let mut d = Dictionary::new();
            d.insert(b"Type", Object::Name(b"XRef".to_vec()));
            d.insert(b"Length", Object::Integer(0));
            Object::Stream(Stream::new(d, data))
        };
        let a = make(b"totally different bytes".to_vec());
        let e = make(b"and yet still matches".to_vec());
        assert_eq!(compare_objects("4 0", &a, &e), "");
    }

    #[test]
    fn flate_same_decoded_different_compressed_matches() {
        // Same source payload, encoded at different compression levels →
        // compressed bytes differ, decoded bytes match, `/FlateDecode` in
        // /Filter routes through decompress path.
        let source = b"the quick brown fox jumps over the lazy dog. \
                       the quick brown fox jumps over the lazy dog. \
                       the quick brown fox jumps over the lazy dog.";
        let compressed_a = zlib(source, Compression::none());
        let compressed_e = zlib(source, Compression::best());
        assert_ne!(
            compressed_a, compressed_e,
            "test premise: compressed bytes differ"
        );

        let make = |data: Vec<u8>| {
            let mut d = Dictionary::new();
            d.insert(b"Filter", Object::Name(b"FlateDecode".to_vec()));
            d.insert(b"Length", Object::Integer(data.len() as i64));
            Object::Stream(Stream::new(d, data))
        };
        let a = make(compressed_a);
        let e = make(compressed_e);
        assert_eq!(compare_objects("5 0", &a, &e), "");
    }

    #[test]
    fn filter_array_containing_flatedecode_triggers_decompress() {
        // Direct unit test of the detector (rather than crafting a genuine
        // multi-filter round-trip in an e2e test). An Array /Filter whose
        // first element is /FlateDecode must route through the decompress
        // path.
        let mut d = Dictionary::new();
        d.insert(
            b"Filter",
            Object::Array(vec![
                Object::Name(b"FlateDecode".to_vec()),
                Object::Name(b"ASCIIHexDecode".to_vec()),
            ]),
        );
        assert!(
            filter_uses_flatedecode(&d),
            "FlateDecode-first Array must trigger decompress"
        );
        // And a positional variant: FlateDecode not first.
        let mut d2 = Dictionary::new();
        d2.insert(
            b"Filter",
            Object::Array(vec![
                Object::Name(b"ASCIIHexDecode".to_vec()),
                Object::Name(b"FlateDecode".to_vec()),
            ]),
        );
        assert!(
            filter_uses_flatedecode(&d2),
            "FlateDecode anywhere in Array must trigger decompress"
        );
        // Negative: no FlateDecode.
        let mut d3 = Dictionary::new();
        d3.insert(
            b"Filter",
            Object::Array(vec![Object::Name(b"ASCIIHexDecode".to_vec())]),
        );
        assert!(!filter_uses_flatedecode(&d3));
    }

    #[test]
    fn non_flate_filter_with_diff_raw_reports_stream_data_differs() {
        // /Filter /ASCIIHexDecode is not FlateDecode → raw compare, same
        // size but different bytes → "stream data differs".
        let make = |data: Vec<u8>| {
            let mut d = Dictionary::new();
            d.insert(b"Filter", Object::Name(b"ASCIIHexDecode".to_vec()));
            d.insert(b"Length", Object::Integer(data.len() as i64));
            Object::Stream(Stream::new(d, data))
        };
        // Same length, different content (hex-alphabet bytes so nothing
        // downstream is tempted to decode them).
        let a = make(b"41 42 43>".to_vec());
        let e = make(b"44 45 46>".to_vec());
        assert_eq!(compare_objects("6 0", &a, &e), "6 0: stream data differs");
    }

    #[test]
    fn uncompressed_size_diff_reports_stream_data_size_differs() {
        // No /Filter, different payload lengths, /Length stripped → matching
        // dicts, then the size-differs branch fires before the byte compare.
        let mut ad = Dictionary::new();
        ad.insert(b"Length", Object::Integer(3));
        let mut ed = Dictionary::new();
        ed.insert(b"Length", Object::Integer(4));
        let a = Object::Stream(Stream::new(ad, b"abc".to_vec()));
        let e = Object::Stream(Stream::new(ed, b"abcd".to_vec()));
        assert_eq!(
            compare_objects("7 0", &a, &e),
            "7 0: stream data size differs"
        );
    }
}
