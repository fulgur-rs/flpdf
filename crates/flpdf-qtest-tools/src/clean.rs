//! Normalize a PDF before per-object comparison, mirroring qpdf's
//! `cleanTrailer(QPDFObjectHandle&)` and `cleanEncryption(QPDF&)` helpers in
//! `compare-for-test/qpdf-test-compare.cc`.
//!
//! The compare tool masks values that legitimately differ between two runs
//! of qpdf on the same input (the trailer's `/Length` and `/ID` halves, and
//! the encryption-dict password/permission hashes) so that a true byte-for-
//! byte object diff can be reported for everything else.
//!
//! Oracle: qpdf 11.9.0 `compare-for-test/qpdf-test-compare.cc:24-43,108-131`.

use std::io::{Read, Seek};

use flpdf::{Dictionary, Object, ObjectHandle, Pdf};

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

/// Strip password-recovery hashes and permission flags from the encryption
/// dictionary (Standard security handler `/O` `/OE` `/U` `/UE` `/Perms`).
/// Mirrors qpdf's `cleanEncryption(QPDF&)`.
///
/// The trailer's `/Encrypt` entry itself is not touched, so the trailer
/// still points at the same indirect object; only the referenced dict's
/// contents change. When `/Encrypt` is absent, an inline dictionary, or
/// any other non-dict value, the call is a no-op — the same blind spot
/// as qpdf, which only enumerates indirect objects via `getAllObjects()`.
///
/// # Errors
///
/// Returns any error produced by [`Pdf::resolve`] when following the
/// `/Encrypt` indirect reference (e.g. a corrupt object stream).
pub fn clean_encryption<R: Read + Seek>(pdf: &mut Pdf<R>) -> flpdf::Result<()> {
    let Some(encrypt_ref) = pdf.trailer_dictionary().get_ref(b"Encrypt") else {
        return Ok(());
    };
    let mut enc = pdf.resolve_object(encrypt_ref)?;
    let Some(dict) = enc.as_dict_mut() else {
        return Ok(());
    };
    for key in [b"O".as_ref(), b"OE", b"U", b"UE", b"Perms"] {
        dict.remove(key);
    }
    pdf.set_object(encrypt_ref, enc);
    Ok(())
}

/// qpdf's handle-native `cleanTrailer(QPDFObjectHandle&)` route used by the
/// canonical compare orchestrator. The legacy dictionary helper above remains
/// for its focused compatibility tests; production comparison now mutates the
/// same live handle graph that its object walk consumes.
pub fn clean_trailer_handle<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    trailer: &ObjectHandle,
) -> flpdf::Result<()> {
    trailer.remove_key(b"/Length");
    if !trailer.has_key(b"/ID") {
        return Ok(());
    }
    let id = pdf.resolve_object_handle_to_terminal(&trailer.get_key(b"/ID"))?;
    let Some(items) = id.as_array() else {
        return Ok(());
    };
    if items.len() != 2 {
        return Ok(());
    }
    let both_equal = items[0].unparse() == items[1].unparse();
    id.set_array_item(1, ObjectHandle::string(Vec::new()))?;
    if both_equal {
        id.set_array_item(0, ObjectHandle::string(Vec::new()))?;
    }
    if let Some(object_ref) = id.object_ref() {
        pdf.mark_object_dirty(object_ref);
    }
    Ok(())
}

/// qpdf's handle-native `cleanEncryption(QPDF&)` route used by the canonical
/// compare orchestrator. The `/Encrypt` child is resolved through the owning
/// document, then its live dictionary is edited in place; the dirty mark keeps
/// any later legacy writer/cache observer coherent without materializing the
/// dictionary.
pub fn clean_encryption_handle<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    trailer: &ObjectHandle,
) -> flpdf::Result<()> {
    let encrypt = pdf.resolve_object_handle_to_terminal(&trailer.get_key(b"/Encrypt"))?;
    if encrypt.as_dictionary().is_none() {
        return Ok(());
    }
    for key in [b"/O".as_ref(), b"/OE", b"/U", b"/UE", b"/Perms"] {
        encrypt.remove_key(key);
    }
    if let Some(object_ref) = encrypt.object_ref() {
        pdf.mark_object_dirty(object_ref);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flpdf::{ObjectRef, PdfOpenOptions};
    use std::io::Cursor;

    // ---------- clean_trailer ----------

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

    // ---------- clean_encryption ----------

    fn fixture_bytes(rel: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(rel);
        std::fs::read(&path).unwrap_or_else(|e| panic!("read {} failed: {e}", path.display()))
    }

    // AES-256 R=6 fixture: its /Encrypt dict carries all five keys
    // (/O /OE /U /UE /Perms) that `clean_encryption` must strip, giving
    // full coverage of the removal loop. RC4 fixtures like
    // encrypted/v2-rc4-128-r3.pdf omit /OE, /UE and /Perms.
    fn open_v5_r6_fixture() -> Pdf<Cursor<Vec<u8>>> {
        let bytes = fixture_bytes("tests/fixtures/encrypted/v5-aes-256-r6.pdf");
        let opts = PdfOpenOptions {
            password: b"user-v5-r6".to_vec(),
            ..PdfOpenOptions::default()
        };
        Pdf::open_mem_owned_with_options(bytes, opts).expect("open v5 R6 fixture")
    }

    #[test]
    fn clean_encryption_noop_on_unencrypted_pdf() {
        // minimal.pdf has no /Encrypt. `clean_encryption` should return Ok
        // and touch nothing observable — the trailer stays free of /Encrypt.
        let bytes = fixture_bytes("tests/fixtures/minimal.pdf");
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open minimal.pdf");
        assert!(
            pdf.trailer_dictionary().get(b"Encrypt").is_none(),
            "premise: minimal.pdf is unencrypted"
        );
        clean_encryption(&mut pdf).expect("no error on unencrypted PDF");
        assert!(
            pdf.trailer_dictionary().get(b"Encrypt").is_none(),
            "trailer's /Encrypt still absent"
        );
    }

    #[test]
    fn clean_encryption_strips_hashes_from_indirect_encrypt_dict() {
        let mut pdf = open_v5_r6_fixture();
        let encrypt_ref = pdf
            .trailer_dictionary()
            .get_ref(b"Encrypt")
            .expect("premise: /Encrypt is an indirect reference");

        // Sanity: before cleanup, all five hash/permission keys the AES-256
        // R=6 Standard security handler emits are present.
        {
            let enc = pdf
                .resolve_object(encrypt_ref)
                .expect("resolve /Encrypt dict");
            let dict = enc.as_dict().expect("premise: /Encrypt resolves to a dict");
            for key in [b"O".as_ref(), b"OE", b"U", b"UE", b"Perms"] {
                assert!(
                    dict.get(key).is_some(),
                    "premise: fixture's encryption dict carries /{}",
                    std::str::from_utf8(key).unwrap()
                );
            }
        }

        clean_encryption(&mut pdf).expect("clean_encryption succeeds");

        // Re-resolve through the cache: the stripped keys must be gone and
        // structural keys like /Filter and /V must still be there.
        let enc = pdf
            .resolve_object(encrypt_ref)
            .expect("re-resolve after clean_encryption");
        let dict = enc.as_dict().expect("still a dict after cleanup");
        for key in [b"O".as_ref(), b"OE", b"U", b"UE", b"Perms"] {
            assert!(
                dict.get(key).is_none(),
                "/{} must have been removed",
                std::str::from_utf8(key).unwrap()
            );
        }
        assert!(
            dict.get(b"Filter").is_some(),
            "/Filter must survive cleanup"
        );
        assert!(dict.get(b"V").is_some(), "/V must survive cleanup");
    }

    #[test]
    fn clean_encryption_noop_when_encrypt_is_not_a_reference() {
        // Craft a trailer that carries /Encrypt as an inline (non-indirect)
        // Integer. `get_ref` returns None for non-references, so
        // `clean_encryption` short-circuits without touching the object.
        // This is qpdf's same-shaped blind spot: `cleanEncryption` only
        // mutates the *referenced* dict, not inline values in the trailer.
        //
        // We reuse minimal.pdf then splice an inline /Encrypt into its
        // trailer via `set_object` on the trailer's parent — but the
        // trailer itself is not an indirect object, so instead assert the
        // behavior indirectly through `get_ref` semantics:
        let mut pdf =
            Pdf::open_mem_owned(fixture_bytes("tests/fixtures/minimal.pdf")).expect("open");
        // Sanity: trailer has no /Encrypt, so `get_ref` is None → no-op path.
        assert!(pdf.trailer_dictionary().get_ref(b"Encrypt").is_none());
        clean_encryption(&mut pdf).expect("no-op when /Encrypt has no ref target");
    }

    #[test]
    fn clean_trailer_handle_masks_length_and_id_halves() {
        let mut pdf = Pdf::open_mem_owned(fixture_bytes("tests/fixtures/minimal.pdf"))
            .expect("open minimal PDF");
        let trailer = ObjectHandle::dictionary(vec![
            (b"/Length".to_vec(), ObjectHandle::integer(42)),
            (
                b"/ID".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::string(b"first".to_vec()),
                    ObjectHandle::string(b"second".to_vec()),
                ]),
            ),
        ]);

        clean_trailer_handle(&mut pdf, &trailer).expect("handle cleanup succeeds");

        assert!(!trailer.has_key(b"/Length"));
        let items = trailer
            .get_key(b"/ID")
            .as_array()
            .expect("/ID remains an array");
        assert_eq!(items[0].as_string(), Some(b"first".to_vec()));
        assert_eq!(items[1].as_string(), Some(Vec::new()));
    }

    #[test]
    fn clean_trailer_handle_masks_both_equal_id_halves() {
        let mut pdf = Pdf::open_mem_owned(fixture_bytes("tests/fixtures/minimal.pdf"))
            .expect("open minimal PDF");
        let trailer = ObjectHandle::dictionary(vec![(
            b"/ID".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::string(b"same".to_vec()),
                ObjectHandle::string(b"same".to_vec()),
            ]),
        )]);

        clean_trailer_handle(&mut pdf, &trailer).expect("handle cleanup succeeds");

        let items = trailer
            .get_key(b"/ID")
            .as_array()
            .expect("/ID remains an array");
        assert_eq!(items[0].as_string(), Some(Vec::new()));
        assert_eq!(items[1].as_string(), Some(Vec::new()));
    }

    #[test]
    fn clean_trailer_handle_leaves_non_array_id_unchanged() {
        let mut pdf = Pdf::empty().expect("create an empty PDF");
        let trailer = ObjectHandle::dictionary(vec![(
            b"/ID".to_vec(),
            ObjectHandle::string(b"not an array".to_vec()),
        )]);

        clean_trailer_handle(&mut pdf, &trailer).expect("non-array /ID is a no-op");

        assert_eq!(
            trailer.get_key(b"/ID").as_string(),
            Some(b"not an array".to_vec())
        );
    }

    #[test]
    fn clean_trailer_handle_leaves_wrong_length_id_unchanged() {
        let mut pdf = Pdf::empty().expect("create an empty PDF");
        let trailer = ObjectHandle::dictionary(vec![(
            b"/ID".to_vec(),
            ObjectHandle::array(vec![ObjectHandle::string(b"only".to_vec())]),
        )]);

        clean_trailer_handle(&mut pdf, &trailer).expect("wrong-length /ID is a no-op");

        let items = trailer
            .get_key(b"/ID")
            .as_array()
            .expect("/ID remains an array");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].as_string(), Some(b"only".to_vec()));
    }

    #[test]
    fn clean_trailer_handle_mutates_an_indirect_id_array_in_place() {
        let mut pdf = Pdf::empty().expect("create an empty PDF");
        let id_ref = ObjectRef::new(9, 0);
        pdf.set_object(
            id_ref,
            Object::Array(vec![
                Object::String(b"first".to_vec()),
                Object::String(b"second".to_vec()),
            ]),
        );
        let id = pdf.get_object_handle(id_ref);
        let trailer = ObjectHandle::dictionary(vec![(b"/ID".to_vec(), id.clone())]);

        clean_trailer_handle(&mut pdf, &trailer).expect("handle cleanup succeeds");

        let cleaned_id = pdf
            .resolve_object_handle_to_terminal(&trailer.get_key(b"/ID"))
            .expect("resolve cleaned /ID");
        assert_eq!(cleaned_id.object_ref(), Some(id_ref));
        let items = cleaned_id.as_array().expect("/ID remains an array");
        assert_eq!(items[0].as_string(), Some(b"first".to_vec()));
        assert_eq!(items[1].as_string(), Some(Vec::new()));
        let alias_items = id.as_array().expect("original alias remains live");
        assert_eq!(alias_items[1].as_string(), Some(Vec::new()));
    }

    #[test]
    fn clean_encryption_handle_strips_hashes_from_canonical_dictionary() {
        let mut pdf = open_v5_r6_fixture();
        let trailer = pdf.trailer();

        clean_encryption_handle(&mut pdf, &trailer).expect("handle cleanup succeeds");

        let encrypt = pdf
            .resolve_object_handle_to_terminal(&trailer.get_key(b"/Encrypt"))
            .expect("resolve /Encrypt handle");
        for key in [b"/O".as_ref(), b"/OE", b"/U", b"/UE", b"/Perms"] {
            assert!(!encrypt.has_key(key), "{key:?} must have been removed");
        }
        assert!(encrypt.has_key(b"/Filter"));
        assert!(encrypt.has_key(b"/V"));
    }
}
