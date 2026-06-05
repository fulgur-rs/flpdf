use flpdf::{
    signature_rewrite_impact, would_rewrite_invalidate_signatures, write_pdf_with_options, Error,
    ObjectRef, Pdf, SignatureRewriteReason, SignatureWriteMode, WriteOptions,
};
use std::collections::BTreeMap;
use std::io::Cursor;

fn build_pdf(objects: &[(u32, &[u8])]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();
    for &(num, bytes) in objects {
        offsets.insert(num, out.len() as u64);
        out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        out.extend_from_slice(bytes);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref_pos = out.len() as u64;
    let max_num = objects.iter().map(|&(n, _)| n).max().unwrap_or(0);
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", max_num + 1).as_bytes());
    for i in 1..=max_num {
        match offsets.get(&i) {
            Some(&off) => out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_pos}\n%%EOF\n",
            max_num + 1
        )
        .as_bytes(),
    );
    out
}

fn build_signed_acroform_pdf() -> Vec<u8> {
    let objects: Vec<(u32, &[u8])> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /Annots [5 0 R] >>"),
        (4, b"<< /Fields [5 0 R] /SigFlags 3 >>"),
        (
            5,
            b"<< /Type /Annot /Subtype /Widget /FT /Sig /T (Signed) /V 6 0 R /P 3 0 R /Rect [0 0 10 10] >>",
        ),
        (
            6,
            b"<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached /ByteRange [0 10 20 30] /Contents <00> >>",
        ),
    ];
    build_pdf(&objects)
}

fn build_unsigned_pdf() -> Vec<u8> {
    let objects: Vec<(u32, &[u8])> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>",
        ),
        (3, b"<< /Type /Page /Parent 2 0 R >>"),
    ];
    build_pdf(&objects)
}

fn open(bytes: Vec<u8>) -> Pdf<Cursor<Vec<u8>>> {
    Pdf::open(Cursor::new(bytes)).expect("PDF should parse")
}

#[test]
fn full_rewrite_invalidates_signed_pdf() {
    let mut pdf = open(build_signed_acroform_pdf());

    let impact = signature_rewrite_impact(&mut pdf, SignatureWriteMode::FullRewrite).unwrap();

    assert!(impact.has_signatures);
    assert!(impact.invalidates_signatures);
    assert_eq!(impact.reason, SignatureRewriteReason::FullRewrite);
}

#[test]
fn full_rewrite_of_signed_pdf_returns_structured_signed_error() {
    let mut pdf = open(build_signed_acroform_pdf());
    let mut options = WriteOptions::default();
    options.full_rewrite = true;

    let err = write_pdf_with_options(&mut pdf, Vec::new(), &options)
        .expect_err("full rewrite should refuse signed PDFs");

    let Error::Signed { fields, message } = err else {
        panic!("expected Error::Signed, got {err:?}");
    };
    assert_eq!(fields, vec!["Signed"]);
    assert!(
        message.contains("refusing full rewrite of signed PDF"),
        "unexpected message: {message}",
    );
    assert!(
        message.contains("--remove-restrictions"),
        "diagnostic should mention the override flag: {message}",
    );
    assert!(
        message.contains("incremental rewrite"),
        "diagnostic should suggest incremental rewrite: {message}",
    );
}

#[test]
fn full_rewrite_refusal_survives_malformed_signature_details() {
    let objects: Vec<(u32, &[u8])> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>",
        ),
        (3, b"<< /Type /Page /Parent 2 0 R /Annots [5 0 R] >>"),
        (4, b"<< /Fields [5 0 R] /SigFlags 3 >>"),
        (
            5,
            b"<< /Type /Annot /Subtype /Widget /FT /Sig /T (Broken) /V 6 0 R /P 3 0 R /Rect [0 0 10 10] >>",
        ),
        (
            6,
            b"<< /Type /Sig /ByteRange [0 /Bad 20 30] /Contents <00> >>",
        ),
    ];
    let mut pdf = open(build_pdf(&objects));
    let mut options = WriteOptions::default();
    options.full_rewrite = true;

    let err = write_pdf_with_options(&mut pdf, Vec::new(), &options)
        .expect_err("full rewrite should refuse before destructive output");

    let Error::Signed { fields, message } = err else {
        panic!("expected Error::Signed, got {err:?}");
    };
    assert!(
        fields
            .iter()
            .any(|field| field == "5 0 R" || field == "6 0 R"),
        "expected object-ref fallback fields, got {fields:?}",
    );
    assert!(message.contains("refusing full rewrite of signed PDF"));
}

#[test]
fn full_rewrite_without_signatures_does_not_report_invalidation() {
    let mut pdf = open(build_unsigned_pdf());

    let impact = signature_rewrite_impact(&mut pdf, SignatureWriteMode::FullRewrite).unwrap();

    assert!(!impact.has_signatures);
    assert!(!impact.invalidates_signatures);
    assert_eq!(impact.reason, SignatureRewriteReason::NoSignatures);
}

#[test]
fn incremental_preserves_when_no_signed_or_acroform_objects_are_touched() {
    let mut pdf = open(build_signed_acroform_pdf());
    let page = pdf.resolve(ObjectRef::new(3, 0)).unwrap();
    pdf.set_object(ObjectRef::new(3, 0), page);

    let impact = signature_rewrite_impact(&mut pdf, SignatureWriteMode::Incremental).unwrap();

    assert!(impact.has_signatures);
    assert!(!impact.invalidates_signatures);
    assert_eq!(
        impact.reason,
        SignatureRewriteReason::IncrementalPreservesSignedByteRanges
    );
}

#[test]
fn incremental_invalidates_when_acroform_object_is_touched() {
    let mut pdf = open(build_signed_acroform_pdf());
    let acroform = pdf.resolve(ObjectRef::new(4, 0)).unwrap();
    pdf.set_object(ObjectRef::new(4, 0), acroform);

    let impact = signature_rewrite_impact(&mut pdf, SignatureWriteMode::Incremental).unwrap();

    assert!(impact.invalidates_signatures);
    assert_eq!(
        impact.reason,
        SignatureRewriteReason::IncrementalTouchesAcroForm
    );
    assert_eq!(impact.first_invalidating_ref, Some(ObjectRef::new(4, 0)));
}

#[test]
fn incremental_invalidates_when_signature_field_is_touched() {
    let mut pdf = open(build_signed_acroform_pdf());
    let field = pdf.resolve(ObjectRef::new(5, 0)).unwrap();
    pdf.set_object(ObjectRef::new(5, 0), field);

    let impact = signature_rewrite_impact(&mut pdf, SignatureWriteMode::Incremental).unwrap();

    assert!(impact.invalidates_signatures);
    assert_eq!(
        impact.reason,
        SignatureRewriteReason::IncrementalTouchesSignedObject
    );
    assert_eq!(impact.first_invalidating_ref, Some(ObjectRef::new(5, 0)));
}

#[test]
fn write_options_wrapper_uses_full_rewrite_flag() {
    let mut pdf = open(build_signed_acroform_pdf());
    let mut options = WriteOptions::default();
    options.full_rewrite = true;

    assert!(would_rewrite_invalidate_signatures(&mut pdf, &options).unwrap());
}

#[test]
fn catalog_perms_byte_range_dictionary_is_treated_as_signed_object_without_acroform() {
    let objects: Vec<(u32, &[u8])> = vec![
        (
            1,
            b"<< /Type /Catalog /Pages 2 0 R /Perms << /DocMDP 8 0 R >> >>",
        ),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>",
        ),
        (3, b"<< /Type /Page /Parent 2 0 R >>"),
        (
            8,
            b"<< /Type /Sig /ByteRange [0 10 20 30] /Contents <00> >>",
        ),
    ];
    let mut pdf = open(build_pdf(&objects));
    let sig = pdf.resolve(ObjectRef::new(8, 0)).unwrap();
    pdf.set_object(ObjectRef::new(8, 0), sig);

    let impact = signature_rewrite_impact(&mut pdf, SignatureWriteMode::Incremental).unwrap();

    assert!(impact.has_signatures);
    assert!(impact.signed_object_refs.contains(&ObjectRef::new(8, 0)));
    assert!(impact.invalidates_signatures);
}

#[test]
fn unreferenced_byte_range_dictionary_is_not_found_by_eager_scan() {
    let objects: Vec<(u32, &[u8])> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>",
        ),
        (3, b"<< /Type /Page /Parent 2 0 R >>"),
        (
            8,
            b"<< /Type /Sig /ByteRange [0 10 20 30] /Contents <00> >>",
        ),
    ];
    let mut pdf = open(build_pdf(&objects));

    let impact = signature_rewrite_impact(&mut pdf, SignatureWriteMode::Incremental).unwrap();

    assert!(!impact.has_signatures);
    assert!(impact.signed_object_refs.is_empty());
    assert!(!impact.invalidates_signatures);
}
