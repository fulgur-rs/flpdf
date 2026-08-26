//! Canonical qpdf compare-for-test cleanup helpers.
//!
//! qpdf's `cleanTrailer(QPDFObjectHandle&)` and `cleanEncryption(QPDF&)` mutate
//! the live handle graph before object comparison. Keeping these operations on
//! `ObjectHandle` avoids a second materialized object model in the harness.

use std::io::{Read, Seek};

use flpdf::{ObjectHandle, Pdf};

/// Strip the trailer fields that qpdf's compare-for-test tool masks before
/// comparing objects. The `/ID` shape guard and byte-based equality follow
/// `compare-for-test/qpdf-test-compare.cc:24-43`.
pub fn clean_trailer_handle<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    trailer: &ObjectHandle,
) -> flpdf::Result<()> {
    trailer.remove_key(b"/Length");
    if !trailer.has_key(b"/ID") {
        return Ok(());
    }
    let id = pdf.resolve_to_terminal(&trailer.get_key(b"/ID"))?;
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

/// Strip Standard-security password and permission hashes from the live
/// `/Encrypt` dictionary, mirroring qpdf's `cleanEncryption(QPDF&)`.
pub fn clean_encryption_handle<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    trailer: &ObjectHandle,
) -> flpdf::Result<()> {
    if !trailer.has_key(b"/Encrypt") {
        return Ok(());
    }
    let encrypt = pdf.resolve_to_terminal(&trailer.get_key(b"/Encrypt"))?;
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
    use flpdf::ObjectRef;

    #[test]
    fn clean_trailer_masks_second_id_half() {
        let mut pdf = Pdf::empty().expect("empty document");
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

        clean_trailer_handle(&mut pdf, &trailer).expect("cleanup succeeds");

        assert!(!trailer.has_key(b"/Length"));
        let items = trailer.get_key(b"/ID").as_array().expect("ID array");
        assert_eq!(items[0].as_string(), Some(b"first".to_vec()));
        assert_eq!(items[1].as_string(), Some(Vec::new()));
    }

    #[test]
    fn clean_trailer_masks_both_equal_id_halves() {
        let mut pdf = Pdf::empty().expect("empty document");
        let trailer = ObjectHandle::dictionary(vec![(
            b"/ID".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::string(b"same".to_vec()),
                ObjectHandle::string(b"same".to_vec()),
            ]),
        )]);

        clean_trailer_handle(&mut pdf, &trailer).expect("cleanup succeeds");

        let items = trailer.get_key(b"/ID").as_array().expect("ID array");
        assert!(items
            .iter()
            .all(|item| item.as_string() == Some(Vec::new())));
    }

    #[test]
    fn clean_trailer_leaves_non_array_and_wrong_length_ids_unchanged() {
        let mut pdf = Pdf::empty().expect("empty document");
        let scalar = ObjectHandle::dictionary(vec![(
            b"/ID".to_vec(),
            ObjectHandle::string(b"scalar".to_vec()),
        )]);
        clean_trailer_handle(&mut pdf, &scalar).expect("scalar cleanup succeeds");
        assert_eq!(scalar.get_key(b"/ID").as_string(), Some(b"scalar".to_vec()));

        let short = ObjectHandle::dictionary(vec![(
            b"/ID".to_vec(),
            ObjectHandle::array(vec![ObjectHandle::string(b"one".to_vec())]),
        )]);
        clean_trailer_handle(&mut pdf, &short).expect("short cleanup succeeds");
        assert_eq!(short.get_key(b"/ID").as_array().unwrap().len(), 1);
    }

    #[test]
    fn clean_trailer_mutates_an_indirect_id_array_in_place() {
        let mut pdf = Pdf::empty().expect("empty document");
        let id_ref = ObjectRef::new(9, 0);
        pdf.set_object_handle(
            id_ref,
            ObjectHandle::array(vec![
                ObjectHandle::string(b"first".to_vec()),
                ObjectHandle::string(b"second".to_vec()),
            ]),
        )
        .expect("install ID array");
        let id = pdf.get_object_handle(id_ref);
        let trailer = ObjectHandle::dictionary(vec![(b"/ID".to_vec(), id.clone())]);

        clean_trailer_handle(&mut pdf, &trailer).expect("cleanup succeeds");

        let items = id.as_array().expect("indirect ID array remains live");
        assert_eq!(items[1].as_string(), Some(Vec::new()));
    }

    #[test]
    fn clean_encryption_is_a_noop_without_encrypt() {
        let mut pdf = Pdf::open_mem_owned(
            include_bytes!("../../../../../tests/fixtures/compat/one-page.pdf").to_vec(),
        )
        .expect("open fixture");
        let trailer = pdf.trailer();
        clean_encryption_handle(&mut pdf, &trailer).expect("cleanup succeeds");
    }

    #[test]
    fn clean_encryption_strips_hashes_from_a_canonical_dictionary() {
        let mut pdf = Pdf::empty().expect("create an empty PDF");
        let encrypt = ObjectHandle::dictionary(vec![
            (b"Filter".to_vec(), ObjectHandle::name(b"Standard".to_vec())),
            (b"O".to_vec(), ObjectHandle::string(b"o".to_vec())),
            (b"U".to_vec(), ObjectHandle::string(b"u".to_vec())),
            (b"Perms".to_vec(), ObjectHandle::string(b"perms".to_vec())),
        ]);
        let trailer = ObjectHandle::dictionary(vec![(b"Encrypt".to_vec(), encrypt.clone())]);
        clean_encryption_handle(&mut pdf, &trailer).expect("cleanup succeeds");
        assert!(!encrypt.has_key(b"/O"));
        assert!(!encrypt.has_key(b"/U"));
        assert!(encrypt.has_key(b"/Filter"));
    }
}
