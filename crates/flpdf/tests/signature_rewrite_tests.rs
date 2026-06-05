use flpdf::{
    signature_rewrite_impact, signatures, would_rewrite_invalidate_signatures, write_pdf,
    write_pdf_with_options, Error, ObjectRef, Pdf, SignatureRewriteReason, SignatureWriteMode,
    WriteOptions,
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
fn allow_signed_full_rewrite_option_bypasses_refusal() {
    let mut pdf = open(build_signed_acroform_pdf());
    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.allow_signed_full_rewrite = true;

    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, &options)
        .expect("explicit opt-in should allow destructive signed rewrite");

    assert!(!out.is_empty());
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
fn full_rewrite_without_signatures_is_not_refused_by_writer() {
    let mut pdf = open(build_unsigned_pdf());
    let mut options = WriteOptions::default();
    options.full_rewrite = true;

    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, &options)
        .expect("unsigned full rewrite should not trip signed-PDF preflight");

    assert!(!out.is_empty());
}

#[test]
fn full_rewrite_refusal_uses_object_ref_for_unnamed_signature_field() {
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
            b"<< /Type /Annot /Subtype /Widget /FT /Sig /V 6 0 R /P 3 0 R /Rect [0 0 10 10] >>",
        ),
        (
            6,
            b"<< /Type /Sig /ByteRange [0 10 20 30] /Contents <00> >>",
        ),
    ];
    let mut pdf = open(build_pdf(&objects));
    let mut options = WriteOptions::default();
    options.full_rewrite = true;

    let err = write_pdf_with_options(&mut pdf, Vec::new(), &options)
        .expect_err("full rewrite should refuse signed PDFs");

    let Error::Signed { fields, message } = err else {
        panic!("expected Error::Signed, got {err:?}");
    };
    assert_eq!(fields, vec!["5 0 R"]);
    assert!(message.contains("5 0 R"));
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

/// flpdf-9hc.22.4: the incremental write path must preserve every byte covered
/// by a signature's `/ByteRange`. Incremental update appends a new generation
/// *after* the original bytes; a signed `/ByteRange` covers only the original
/// prefix, so a benign incremental change (touching neither `/AcroForm`, the
/// signature field, nor the signature dictionary) must leave that prefix — and
/// thus the signature's covered regions — bit-identical.
///
/// External validation caveat: this fixture is SYNTHETIC. Its `/Contents` is a
/// `<00>` placeholder rather than a real PKCS#7 blob, so `pdfsig`/`qpdf`
/// cryptographic verification is not meaningful on it (pdfsig rejects the
/// placeholder regardless of byte preservation). The bit-identical prefix and
/// `/ByteRange` comparisons below ARE the verification for this invariant.
#[test]
fn incremental_write_preserves_signed_byte_range_bytes() {
    let input = build_signed_acroform_pdf();
    let mut pdf = open(input.clone());

    // Read the signed byte ranges from the source before mutating/writing.
    let ranges: Vec<[u64; 4]> = signatures(&mut pdf)
        .unwrap()
        .iter()
        .map(|s| s.byte_range)
        .collect();
    assert!(!ranges.is_empty(), "fixture must contain a signature");

    // Benign incremental change: re-set the page object (3 0 R), which is
    // neither the /AcroForm (4), the signed field (5), nor the sig dict (6).
    // This marks an object touched so the incremental path actually appends a
    // new generation (otherwise the byte-preservation check would be trivial).
    let page = pdf.resolve(ObjectRef::new(3, 0)).unwrap();
    pdf.set_object(ObjectRef::new(3, 0), page);

    let mut out = Vec::new();
    write_pdf(&mut pdf, &mut out).unwrap();

    // The incremental path must have appended a new generation, not rewritten.
    assert!(
        out.len() > input.len(),
        "incremental update should append a new generation"
    );

    // Core invariant: the entire original prefix is preserved verbatim.
    assert!(
        out.starts_with(&input),
        "incremental update must preserve the original source prefix byte-for-byte"
    );

    // Belt-and-suspenders: every region covered by a signature's /ByteRange is
    // bit-identical between input and output.
    for [o1, l1, o2, l2] in ranges {
        for (start, len) in [(o1, l1), (o2, l2)] {
            let s = start as usize;
            let e = s + len as usize;
            assert!(
                e <= input.len() && e <= out.len(),
                "signed /ByteRange region [{s}..{e}] must lie within the document"
            );
            assert_eq!(
                &out[s..e],
                &input[s..e],
                "signed /ByteRange region [{s}..{e}] must be byte-identical after incremental write"
            );
        }
    }
}
