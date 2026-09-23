//! Canonical qpdf compare-for-test cleanup helpers.
//!
//! qpdf's `cleanTrailer(QPDFObjectHandle&)` and `cleanEncryption(QPDF&)` mutate
//! the live handle graph before object comparison. Keeping these operations on
//! `ObjectHandle` avoids a second materialized object model in the harness.

use flpdf::ObjectHandle;

/// Strip the trailer fields that qpdf's compare-for-test tool masks before
/// comparing objects. The `/ID` shape guard and byte-based equality follow
/// `compare-for-test/qpdf-test-compare.cc:111-126`.
pub(crate) fn clean_trailer_handle(trailer: &ObjectHandle) -> flpdf::Result<()> {
    trailer.remove_key(b"/Length");
    let id = trailer.try_get_key(b"/ID")?;
    if !id.try_is_array()? {
        return Ok(());
    }
    if id.try_get_array_n_items()? != 2 {
        return Ok(());
    }
    let first = id.try_get_array_item(0)?;
    let second = id.try_get_array_item(1)?;
    let both_equal = first.unparse() == second.unparse();
    id.try_set_array_item_at(1, ObjectHandle::string(Vec::new()))?;
    if both_equal {
        id.try_set_array_item_at(0, ObjectHandle::string(Vec::new()))?;
    }
    Ok(())
}

/// Strip Standard-security password and permission hashes from the live
/// `/Encrypt` dictionary, mirroring qpdf's `cleanEncryption(QPDF&)`
/// (`compare-for-test/qpdf-test-compare.cc:31-43`).
pub(crate) fn clean_encryption_handle(trailer: &ObjectHandle) -> flpdf::Result<()> {
    let encrypt = trailer.try_get_key(b"/Encrypt")?;
    if !encrypt.try_is_dictionary()? {
        return Ok(());
    }
    for key in [b"/O".as_ref(), b"/OE", b"/U", b"/UE", b"/Perms"] {
        encrypt.remove_key(key);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flpdf::{ObjectRef, Pdf};
    use std::io::Cursor;

    fn parsed_indirect_id_pdf() -> Pdf<Cursor<Vec<u8>>> {
        let bodies: [&[u8]; 4] = [
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
            b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n",
            b"3 0 obj\n[ (first) (second) ]\nendobj\n",
            b"4 0 obj\n<< /Filter /Standard /O (o) /OE (oe) /U (u) /UE (ue) /Perms (perms) >>\nendobj\n",
        ];
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for body in bodies {
            offsets.push(bytes.len());
            bytes.extend_from_slice(body);
        }
        let xref_offset = bytes.len();
        bytes.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        for offset in offsets {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!(
                "trailer\n<< /Size 5 /Root 1 0 R /ID 3 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n"
            )
            .as_bytes(),
        );
        Pdf::open_mem_owned(bytes).expect("open indirect-ID fixture")
    }

    #[test]
    fn clean_trailer_masks_second_id_half() {
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

        clean_trailer_handle(&trailer).expect("cleanup succeeds through the live trailer owner");

        assert!(!trailer.has_key(b"/Length"));
        let items = trailer.get_key(b"/ID").as_array().expect("ID array");
        assert_eq!(items[0].as_string(), Some(b"first".to_vec()));
        assert_eq!(items[1].as_string(), Some(Vec::new()));
    }

    #[test]
    fn clean_trailer_masks_both_equal_id_halves() {
        let trailer = ObjectHandle::dictionary(vec![(
            b"/ID".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::string(b"same".to_vec()),
                ObjectHandle::string(b"same".to_vec()),
            ]),
        )]);

        clean_trailer_handle(&trailer).expect("cleanup resolves through the live trailer owner");

        let items = trailer.get_key(b"/ID").as_array().expect("ID array");
        assert!(items
            .iter()
            .all(|item| item.as_string() == Some(Vec::new())));
    }

    #[test]
    fn clean_trailer_leaves_non_array_and_wrong_length_ids_unchanged() {
        let scalar = ObjectHandle::dictionary(vec![(
            b"/ID".to_vec(),
            ObjectHandle::string(b"scalar".to_vec()),
        )]);
        clean_trailer_handle(&scalar).expect("scalar cleanup succeeds");
        assert_eq!(scalar.get_key(b"/ID").as_string(), Some(b"scalar".to_vec()));

        let short = ObjectHandle::dictionary(vec![(
            b"/ID".to_vec(),
            ObjectHandle::array(vec![ObjectHandle::string(b"one".to_vec())]),
        )]);
        clean_trailer_handle(&short).expect("short cleanup succeeds");
        assert_eq!(short.get_key(b"/ID").as_array().unwrap().len(), 1);
    }

    #[test]
    fn clean_trailer_without_id_still_removes_length() {
        let trailer =
            ObjectHandle::dictionary(vec![(b"/Length".to_vec(), ObjectHandle::integer(42))]);

        clean_trailer_handle(&trailer).expect("cleanup succeeds without /ID");

        assert!(!trailer.has_key(b"/Length"));
        assert!(!trailer.has_key(b"/ID"));
    }

    #[test]
    fn clean_trailer_mutates_an_indirect_id_array_in_place() {
        let mut pdf = parsed_indirect_id_pdf();
        let id = pdf.get_object_handle(ObjectRef::new(3, 0));
        let trailer = pdf.trailer();

        clean_trailer_handle(&trailer).expect("cleanup resolves through the live trailer owner");

        let items = id.as_array().expect("indirect ID array remains live");
        assert_eq!(items[1].as_string(), Some(Vec::new()));
    }

    #[test]
    fn clean_encryption_is_a_noop_without_encrypt() {
        let mut pdf = Pdf::open_mem_owned(
            include_bytes!("../../../tests/fixtures/compat/one-page.pdf").to_vec(),
        )
        .expect("open fixture");
        let trailer = pdf.trailer();
        clean_encryption_handle(&trailer).expect("cleanup succeeds through the live trailer owner");
        assert!(!trailer.has_key(b"/Encrypt"));
    }

    #[test]
    fn clean_encryption_is_a_noop_for_a_non_dictionary_value() {
        let encrypt = ObjectHandle::string(b"not a dictionary".to_vec());
        let trailer = ObjectHandle::dictionary(vec![(b"/Encrypt".to_vec(), encrypt.clone())]);

        clean_encryption_handle(&trailer).expect("non-dictionary cleanup succeeds");

        assert_eq!(
            encrypt.unparse(),
            ObjectHandle::string(b"not a dictionary".to_vec()).unparse()
        );
    }

    #[test]
    fn clean_encryption_strips_hashes_from_a_canonical_dictionary() {
        let encrypt = ObjectHandle::dictionary(vec![
            (b"Filter".to_vec(), ObjectHandle::name(b"Standard".to_vec())),
            (b"O".to_vec(), ObjectHandle::string(b"o".to_vec())),
            (b"OE".to_vec(), ObjectHandle::string(b"oe".to_vec())),
            (b"U".to_vec(), ObjectHandle::string(b"u".to_vec())),
            (b"UE".to_vec(), ObjectHandle::string(b"ue".to_vec())),
            (b"Perms".to_vec(), ObjectHandle::string(b"perms".to_vec())),
        ]);
        let trailer = ObjectHandle::dictionary(vec![(b"Encrypt".to_vec(), encrypt.clone())]);
        clean_encryption_handle(&trailer).expect("cleanup resolves through the live trailer owner");
        for key in [b"/O".as_ref(), b"/OE", b"/U", b"/UE", b"/Perms"] {
            assert!(!encrypt.has_key(key));
        }
        assert!(encrypt.has_key(b"/Filter"));
    }

    #[test]
    fn clean_encryption_resolves_and_mutates_the_indirect_dictionary() {
        let mut pdf = parsed_indirect_id_pdf();
        let encrypt = pdf.get_object_handle(ObjectRef::new(4, 0));
        let trailer = pdf.trailer();
        trailer
            .replace_key(b"/Encrypt", encrypt.clone())
            .expect("attach the live indirect encryption dictionary");

        clean_encryption_handle(&trailer).expect("cleanup resolves through the trailer owner");

        for key in [b"/O".as_ref(), b"/OE", b"/U", b"/UE", b"/Perms"] {
            assert!(!encrypt.has_key(key));
        }
        assert!(encrypt.has_key(b"/Filter"));
    }
}
