//! Normalize a PDF before per-object comparison, mirroring qpdf's
//! `cleanTrailer(QPDFObjectHandle&)` and `cleanEncryption(QPDF&)` helpers in
//! `compare-for-test/qpdf-test-compare.cc`.
//!
//! The compare tool masks values that legitimately differ between two runs
//! of qpdf on the same input (the trailer's `/Length` and `/ID` halves, and
//! the encryption-dict password/permission hashes) so that a true byte-for-
//! byte object diff can be reported for everything else.

use flpdf::{Dictionary, Object};

/// Strip fields from the trailer that qpdf's compare-for-test tool masks
/// before diffing.
///
/// - Remove `/Length` (varies with xref/stream layout).
/// - Blank the second half of `/ID` (always regenerated per run).
/// - Blank the first half of `/ID` too when the two halves serialize to the
///   same bytes (qpdf treats matching halves as a deterministic-`/ID` marker
///   and hides both).
///
/// The `/ID` rewrite only runs when the value is a 2-element array. Any other
/// shape (missing, non-array, wrong length) is left alone — matching qpdf's
/// same shape guard on `QPDFObjectHandle::isArray() && getArrayNItems() == 2`.
///
/// # Notes for reviewers
///
/// Equality of the two `/ID` halves is decided by comparing their [`Object::write_pdf`]
/// bytes, not by [`PartialEq`]. That matches qpdf's `unparse()`-based check and
/// (for example) treats an [`Object::Real`] and an [`Object::RealLiteral`] that
/// serialize identically as equal even though their enum variants differ.
pub fn clean_trailer(trailer: &mut Dictionary) {
    // flpdf's `Dictionary` keys and `Object::Name` bytes are stored WITHOUT
    // the leading `/`. `write_pdf` reinserts the `/` on emission.
    trailer.remove(b"Length");
    let Some(items) = trailer.get(b"ID").and_then(Object::as_array) else {
        return;
    };
    if items.len() != 2 {
        return;
    }
    let mut id0_bytes = Vec::new();
    items[0].write_pdf(&mut id0_bytes);
    let mut id1_bytes = Vec::new();
    items[1].write_pdf(&mut id1_bytes);
    let both_equal = id0_bytes == id1_bytes;
    // `to_vec()` clones the two `Object` handles; the underlying String /
    // Reference / etc. content is not deep-cloned beyond that pair.
    let mut new_items = items.to_vec();
    new_items[1] = Object::String(Vec::new());
    if both_equal {
        new_items[0] = Object::String(Vec::new());
    }
    trailer.insert(b"ID", Object::Array(new_items));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn as_id_array(dict: &Dictionary) -> Option<&[Object]> {
        dict.get(b"ID").and_then(Object::as_array)
    }

    #[test]
    fn clean_trailer_removes_length_key() {
        let mut trailer = Dictionary::new();
        trailer.insert(b"Length", Object::Integer(42));
        trailer.insert(b"Size", Object::Integer(9));
        clean_trailer(&mut trailer);
        assert!(trailer.get(b"Length").is_none(), "/Length should be gone");
        assert_eq!(
            trailer.get(b"Size").and_then(Object::as_integer),
            Some(9),
            "unrelated keys must be preserved"
        );
    }

    #[test]
    fn clean_trailer_blanks_second_id_slot_when_halves_differ() {
        let mut trailer = Dictionary::new();
        trailer.insert(
            b"ID",
            Object::Array(vec![
                Object::String(b"abc".to_vec()),
                Object::String(b"xyz".to_vec()),
            ]),
        );
        clean_trailer(&mut trailer);
        let items = as_id_array(&trailer).expect("/ID still an array");
        assert_eq!(items.len(), 2, "/ID array length preserved");
        assert_eq!(
            items[0].as_string(),
            Some(b"abc".as_ref()),
            "first half unchanged when halves differ"
        );
        assert_eq!(
            items[1].as_string(),
            Some(b"".as_ref()),
            "second half blanked"
        );
    }

    #[test]
    fn clean_trailer_blanks_both_id_slots_when_halves_equal() {
        let mut trailer = Dictionary::new();
        trailer.insert(
            b"ID",
            Object::Array(vec![
                Object::String(b"abc".to_vec()),
                Object::String(b"abc".to_vec()),
            ]),
        );
        clean_trailer(&mut trailer);
        let items = as_id_array(&trailer).expect("/ID still an array");
        assert_eq!(items.len(), 2, "/ID array length preserved");
        assert_eq!(
            items[0].as_string(),
            Some(b"".as_ref()),
            "first half blanked when halves match"
        );
        assert_eq!(
            items[1].as_string(),
            Some(b"".as_ref()),
            "second half blanked when halves match"
        );
    }

    #[test]
    fn clean_trailer_uses_write_pdf_bytes_for_equality_not_partialeq() {
        // Real(1.5) and RealLiteral { value: 1.5, literal: b"1.5" } are NOT
        // equal under PartialEq (different enum variants) but their
        // `write_pdf` bytes are both b"1.5". A qpdf-parity implementation
        // treats them as equal and blanks BOTH slots; a naive PartialEq
        // implementation would only blank the second.
        let real = Object::Real(1.5);
        let real_literal = Object::RealLiteral {
            value: 1.5,
            literal: b"1.5".to_vec(),
        };
        // Sanity: bytes match, PartialEq disagrees.
        assert_ne!(real, real_literal, "test premise: enum variants differ");
        let (mut a, mut b) = (Vec::new(), Vec::new());
        real.write_pdf(&mut a);
        real_literal.write_pdf(&mut b);
        assert_eq!(a, b, "test premise: write_pdf bytes match");

        let mut trailer = Dictionary::new();
        trailer.insert(b"ID", Object::Array(vec![real, real_literal]));
        clean_trailer(&mut trailer);
        let items = as_id_array(&trailer).expect("/ID still an array");
        assert_eq!(
            items[0].as_string(),
            Some(b"".as_ref()),
            "write_pdf-equal halves must both be blanked"
        );
        assert_eq!(
            items[1].as_string(),
            Some(b"".as_ref()),
            "second half always blanked"
        );
    }

    #[test]
    fn clean_trailer_noop_when_id_missing() {
        let mut trailer = Dictionary::new();
        trailer.insert(b"Size", Object::Integer(3));
        clean_trailer(&mut trailer);
        assert!(trailer.get(b"ID").is_none(), "no /ID key added");
        assert_eq!(
            trailer.get(b"Size").and_then(Object::as_integer),
            Some(3),
            "other keys preserved"
        );
    }

    #[test]
    fn clean_trailer_noop_when_id_array_length_not_two() {
        let mut trailer = Dictionary::new();
        trailer.insert(
            b"ID",
            Object::Array(vec![Object::String(b"only-one".to_vec())]),
        );
        clean_trailer(&mut trailer);
        let items = as_id_array(&trailer).expect("/ID still an array");
        assert_eq!(items.len(), 1, "length-1 array left untouched");
        assert_eq!(
            items[0].as_string(),
            Some(b"only-one".as_ref()),
            "content untouched"
        );
    }

    #[test]
    fn clean_trailer_noop_when_id_not_an_array() {
        let mut trailer = Dictionary::new();
        trailer.insert(b"ID", Object::Integer(0));
        clean_trailer(&mut trailer);
        assert_eq!(
            trailer.get(b"ID").and_then(Object::as_integer),
            Some(0),
            "non-array /ID is left as-is"
        );
    }
}
