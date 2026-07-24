//! Per-object semantic comparison matching qpdf v11.9.0's
//! `compareObjects(label, act, exp)` from `compare-for-test/qpdf-test-compare.cc`.
//!
//! Returns the empty string when the two objects match, or a diff-reason
//! string prefixed with `label` (matching qpdf's exact wording) otherwise.
//! The stream limb is added by Task 8; this file starts with the non-stream
//! branch and the type-code gate.

use flpdf::Object;

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
/// Stream objects go through the stream limb (dictionary compare with
/// `/Length` stripped, `/XRef` data skip, `/FlateDecode` decompress-and-
/// compare). Task 8 implements that path; Task 7 tests avoid `Stream`
/// variants entirely.
pub fn compare_objects(label: &str, act: &Object, exp: &Object) -> String {
    if type_code(act) != type_code(exp) {
        return format!("{label}: different types");
    }
    // Stream branch — Task 8. Non-stream path below.
    if matches!(act, Object::Stream(_)) {
        return String::new();
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
    use flpdf::{Dictionary, ObjectRef};

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
}
