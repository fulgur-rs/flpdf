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

#[test]
fn signatures_returns_signed_sig_fields() {
    let mut pdf = Pdf::open_mem_owned(signed_acroform_pdf()).expect("PDF should parse");

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
