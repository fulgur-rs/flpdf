use flpdf::{ObjectHandle, Pdf, QpdfObjGen};
use std::io::Cursor;

fn raw_generation_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let catalog_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let raw_offset = bytes.len();
    bytes.extend_from_slice(b"17 65535 obj\n7\nendobj\n");
    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 18\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
    for _ in 2..17 {
        bytes.extend_from_slice(b"0000000000 00000 f \n");
    }
    bytes.extend_from_slice(format!("{raw_offset:010} 65535 n \n").as_bytes());
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 18 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
            .as_bytes(),
    );
    bytes
}

#[test]
fn qpdf_obj_gen_public_value_matches_qpdf_signed_identity_contract() {
    let zero = QpdfObjGen::default();
    assert_eq!(zero, QpdfObjGen::new(0, 0));
    assert_eq!(zero.unparse(), "0,0");
    assert!(!zero.is_indirect());

    let raw = QpdfObjGen::new(17, 65_535);
    assert_eq!(raw.get_obj(), 17);
    assert_eq!(raw.get_gen(), 65_535);
    assert!(raw.is_indirect());
    assert_eq!(raw.unparse(), "17,65535");
    assert_eq!(raw.unparse_with_separator('/'), "17/65535");
    assert_eq!(raw.to_string(), "17,65535");
    assert!(QpdfObjGen::new(17, 4) < QpdfObjGen::new(18, 0));
    assert!(QpdfObjGen::new(17, 4) < QpdfObjGen::new(17, 5));
    assert!(!QpdfObjGen::new(0, 65_535).is_indirect());

    let signed = QpdfObjGen::new(-17, -1);
    assert_eq!(signed.get_obj(), -17);
    assert_eq!(signed.get_gen(), -1);
    assert!(signed.is_indirect());
}

#[test]
fn object_handle_get_obj_gen_preserves_raw_generation_and_direct_zero_identity() {
    let mut pdf = Pdf::open(Cursor::new(raw_generation_pdf())).expect("open raw-generation PDF");
    let direct = ObjectHandle::integer(7);
    let raw = pdf.get_object_handle_by_raw_identity(17, 65_535);

    assert_eq!(direct.get_obj_gen(), QpdfObjGen::default());
    assert_eq!(direct.object_ref(), None);
    assert_eq!(raw.get_obj_gen(), QpdfObjGen::new(17, 65_535));
    assert!(raw.get_obj_gen().is_indirect());
    assert_eq!(raw.object_ref(), None);
}
