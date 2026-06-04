use flpdf::{ObjectRef, Pdf};
use std::collections::BTreeMap;

fn build_pdf(objects: &[(u32, &[u8])]) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = BTreeMap::new();

    for (number, body) in objects {
        offsets.insert(*number, out.len() as u64);
        out.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }

    let max_number = objects.iter().map(|(number, _)| *number).max().unwrap();
    let xref_start = out.len() as u64;
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", max_number + 1).as_bytes());
    for number in 1..=max_number {
        out.extend_from_slice(format!("{:010} 00000 n \n", offsets[&number]).as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n",
            max_number + 1
        )
        .as_bytes(),
    );
    out
}

fn signed_acroform_pdf() -> Vec<u8> {
    build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
        ),
        (4, b"<< /Fields [5 0 R] >>"),
        (
            5,
            b"<< /FT /Sig /T (Approval) /V 6 0 R /Kids [7 0 R] >>",
        ),
        (
            6,
            b"<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached /ByteRange [0 42 128 256] /Name (Alice) /M (D:20260604000000Z) /Reason (approved) /ContactInfo (alice@example.test) /Cert <010203> >>",
        ),
        (7, b"<< /Subtype /Widget /Parent 5 0 R >>"),
    ])
}

fn signed_acroform_pdf_with_indirect_signature_entries() -> Vec<u8> {
    build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
        ),
        (4, b"<< /Fields [5 0 R] >>"),
        (
            5,
            b"<< /FT 8 0 R /T 9 0 R /V 6 0 R /Kids [7 0 R] >>",
        ),
        (
            6,
            b"<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter 10 0 R /ByteRange 11 0 R /Name 12 0 R /M 13 0 R /Reason 14 0 R /Location 15 0 R /ContactInfo 16 0 R /Cert [17 0 R] >>",
        ),
        (7, b"<< /Subtype /Widget /Parent 5 0 R >>"),
        (8, b"/Sig"),
        (9, b"(Indirect Approval)"),
        (10, b"/adbe.pkcs7.detached"),
        (11, b"[0 42 128 256]"),
        (12, b"(Alice)"),
        (13, b"(D:20260604000000Z)"),
        (14, b"(approved)"),
        (15, b"(Tokyo)"),
        (16, b"(alice@example.test)"),
        (17, b"<010203>"),
    ])
}

fn open(bytes: Vec<u8>) -> Pdf<std::io::Cursor<Vec<u8>>> {
    Pdf::open_mem_owned(bytes).expect("PDF should parse")
}

#[test]
fn signatures_returns_signed_sig_fields() {
    let mut pdf = open(signed_acroform_pdf());

    let signatures = pdf.signatures().expect("signature scan should succeed");

    assert_eq!(signatures.len(), 1);
    let sig = &signatures[0];
    assert_eq!(sig.field_ref, ObjectRef::new(5, 0));
    assert_eq!(sig.signature_ref, Some(ObjectRef::new(6, 0)));
    assert_eq!(sig.field_name, "Approval");
    assert_eq!(sig.byte_range, [0, 42, 128, 256]);
    assert_eq!(sig.sub_filter.as_deref(), Some("adbe.pkcs7.detached"));
    assert_eq!(sig.signer_name.as_deref(), Some("Alice"));
    assert_eq!(sig.signing_time.as_deref(), Some("D:20260604000000Z"));
    assert_eq!(sig.reason.as_deref(), Some("approved"));
    assert_eq!(sig.contact_info.as_deref(), Some("alice@example.test"));
    assert_eq!(sig.certificate.as_deref(), Some(&[1, 2, 3][..]));
}

#[test]
fn signatures_resolves_indirect_field_and_signature_entries() {
    let mut pdf = open(signed_acroform_pdf_with_indirect_signature_entries());

    let signatures = pdf.signatures().expect("signature scan should succeed");

    assert_eq!(signatures.len(), 1);
    let sig = &signatures[0];
    assert_eq!(sig.field_name, "Indirect Approval");
    assert_eq!(sig.byte_range, [0, 42, 128, 256]);
    assert_eq!(sig.sub_filter.as_deref(), Some("adbe.pkcs7.detached"));
    assert_eq!(sig.signer_name.as_deref(), Some("Alice"));
    assert_eq!(sig.signing_time.as_deref(), Some("D:20260604000000Z"));
    assert_eq!(sig.reason.as_deref(), Some("approved"));
    assert_eq!(sig.location.as_deref(), Some("Tokyo"));
    assert_eq!(sig.contact_info.as_deref(), Some("alice@example.test"));
    assert_eq!(sig.certificate.as_deref(), Some(&[1, 2, 3][..]));
}

#[test]
fn signatures_returns_empty_for_missing_or_malformed_acroform_shapes() {
    let cases = [
        build_pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
            ),
        ]),
        build_pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
            ),
            (4, b"42"),
        ]),
        build_pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
            ),
            (4, b"<< /Fields [null 5 0 R] >>"),
            (5, b"42"),
        ]),
    ];

    for bytes in cases {
        let mut pdf = open(bytes);
        assert!(pdf
            .signatures()
            .expect("signature scan should succeed")
            .is_empty());
    }
}

#[test]
fn signatures_walks_nested_fields_and_handles_cycles() {
    let mut pdf = open(build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
        ),
        (4, b"<< /Fields [5 0 R 8 0 R] >>"),
        (5, b"<< /T (Root) /Kids [6 0 R null 7 0 R] >>"),
        (6, b"<< /FT /Sig /T (Child) /Parent 5 0 R /V 9 0 R >>"),
        (7, b"null"),
        (8, b"<< /T (Cycle) /Kids [8 0 R] >>"),
        (9, b"<< /Type /Sig /ByteRange [0 1 2 3] >>"),
    ]));

    let signatures = pdf.signatures().expect("signature scan should succeed");

    assert_eq!(signatures.len(), 1);
    assert_eq!(signatures[0].field_name, "Root.Child");
}

#[test]
fn signatures_honors_explicit_depth_limit() {
    let mut pdf = open(build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
        ),
        (4, b"<< /Fields [5 0 R] >>"),
        (5, b"<< /T (Root) /Kids [6 0 R] >>"),
        (6, b"<< /FT /Sig /T (Child) /Parent 5 0 R /V 7 0 R >>"),
        (7, b"<< /Type /Sig /ByteRange [0 1 2 3] >>"),
    ]));

    let signatures = flpdf::signatures::signatures_with_max_depth(&mut pdf, 0)
        .expect("signature scan should succeed");

    assert!(signatures.is_empty());
}

#[test]
fn signatures_uses_inherited_signature_field_values() {
    let mut pdf = open(build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
        ),
        (4, b"<< /Fields [5 0 R] >>"),
        (5, b"<< /T (Parent) /FT /Sig /V 7 0 R /Kids [6 0 R] >>"),
        (6, b"<< /T (Child) /Parent 5 0 R >>"),
        (7, b"<< /Type /Sig /ByteRange [0 1 2 3] >>"),
    ]));

    let signatures = pdf.signatures().expect("signature scan should succeed");

    assert_eq!(signatures.len(), 2);
    assert_eq!(signatures[0].field_name, "Parent");
    assert_eq!(signatures[1].field_name, "Parent.Child");
}

#[test]
fn unsigned_or_incomplete_signature_fields_are_ignored() {
    let mut pdf = open(build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
        ),
        (4, b"<< /Fields [5 0 R 6 0 R 7 0 R] >>"),
        (5, b"<< /FT /Sig /T (NoValue) >>"),
        (6, b"<< /FT /Sig /T (BadValue) /V 42 >>"),
        (7, b"<< /FT /Sig /T (NoByteRange) /V 8 0 R >>"),
        (8, b"<< /Type /Sig /Name (Alice) >>"),
    ]));

    assert!(pdf
        .signatures()
        .expect("signature scan should succeed")
        .is_empty());
}

#[test]
fn invalid_byte_ranges_return_parse_errors() {
    let invalid_ranges: [&[u8]; 5] = [b"42", b"8 0 R", b"[0 1 2]", b"[0 1 2 (bad)]", b"[0 1 2 -3]"];

    for byte_range in invalid_ranges {
        let body = format!(
            "<< /Type /Sig /ByteRange {} >>",
            std::str::from_utf8(byte_range).unwrap()
        );
        let mut objects: Vec<(u32, Vec<u8>)> = vec![
            (
                1,
                b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>".to_vec(),
            ),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_vec(),
            ),
            (4, b"<< /Fields [5 0 R] >>".to_vec()),
            (5, b"<< /FT /Sig /T (Broken) /V 6 0 R >>".to_vec()),
            (6, body.into_bytes()),
            (7, b"42".to_vec()),
        ];
        objects.sort_by_key(|(number, _)| *number);
        let borrowed: Vec<(u32, &[u8])> = objects
            .iter()
            .map(|(number, body)| (*number, body.as_slice()))
            .collect();
        let mut pdf = open(build_pdf(&borrowed));

        let err = pdf.signatures().expect_err("invalid ByteRange should fail");
        assert!(
            err.to_string().contains("invalid signature /ByteRange"),
            "unexpected error: {err}"
        );
    }
}
