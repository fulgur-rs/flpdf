use flpdf::{
    signature_rewrite_impact, signatures, would_rewrite_invalidate_signatures, write_pdf,
    write_pdf_with_options, ObjectRef, Pdf, SignatureRewriteReason, SignatureWriteMode,
    WriteOptions, DEFAULT_MAX_SIGNATURE_FIELD_DEPTH,
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

fn build_signed_acroform_indirect_ft_pdf() -> Vec<u8> {
    // Same as build_signed_acroform_pdf, but the field's /FT is stored as an
    // indirect reference (7 0 R -> /Sig) instead of a direct name. The only
    // /ByteRange lives in the /V signature dict (obj 6), reachable only once
    // /FT resolves to /Sig — so detection hinges entirely on resolving /FT.
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
            b"<< /Type /Annot /Subtype /Widget /FT 7 0 R /T (Signed) /V 6 0 R /P 3 0 R /Rect [0 0 10 10] >>",
        ),
        (
            6,
            b"<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached /ByteRange [0 10 20 30] /Contents <00> >>",
        ),
        (7, b"/Sig"),
    ];
    build_pdf(&objects)
}

fn build_shared_kids_signature_pdf(depth: u32) -> Vec<u8> {
    let mut objects: Vec<(u32, Vec<u8>)> = vec![
        (
            1,
            b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>".to_vec(),
        ),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>".to_vec(),
        ),
        (3, b"<< /Type /Page /Parent 2 0 R >>".to_vec()),
        (4, b"<< /Fields [5 0 R] /SigFlags 3 >>".to_vec()),
    ];

    for level in 0..depth {
        let object_num = 5 + level;
        let next_num = object_num + 1;
        objects.push((
            object_num,
            format!("<< /FT /Sig /Kids [{next_num} 0 R {next_num} 0 R] >>").into_bytes(),
        ));
    }

    let leaf_num = 5 + depth;
    let sig_num = leaf_num + 1;
    objects.push((
        leaf_num,
        format!("<< /FT /Sig /V {sig_num} 0 R >>").into_bytes(),
    ));
    objects.push((
        sig_num,
        b"<< /Type /Sig /ByteRange [0 10 20 30] /Contents <00> >>".to_vec(),
    ));

    let borrowed: Vec<(u32, &[u8])> = objects
        .iter()
        .map(|(object_num, bytes)| (*object_num, bytes.as_slice()))
        .collect();
    build_pdf(&borrowed)
}

fn build_shared_child_mixed_ft_pdf() -> Vec<u8> {
    // AcroForm where the same leaf field (obj 7) is a child of both a /Sig
    // parent (obj 5) and a non-/Sig parent (obj 6).  The leaf has no /FT of
    // its own, so whether it looks like a signature field depends entirely on
    // which parent's inherited_ft reaches it first.
    //
    // /Fields [5 0 R, 6 0 R]
    //   5: /FT /Sig  /Kids [7 0 R]
    //   6: (no /FT)  /Kids [7 0 R]
    //   7: (no /FT)  /V 8 0 R   ← signature dict
    let objects: Vec<(u32, &[u8])> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>",
        ),
        (3, b"<< /Type /Page /Parent 2 0 R >>"),
        (4, b"<< /Fields [5 0 R 6 0 R] /SigFlags 3 >>"),
        (5, b"<< /FT /Sig /Kids [7 0 R] >>"),
        (6, b"<< /Kids [7 0 R] >>"),
        (7, b"<< /V 8 0 R >>"),
        (
            8,
            b"<< /Type /Sig /ByteRange [0 10 20 30] /Contents <00> >>",
        ),
    ];
    build_pdf(&objects)
}

fn build_deep_signature_chain_pdf(depth: u32) -> Vec<u8> {
    // A linear chain of `depth` unique intermediate fields (no /FT) leading to
    // a /Sig leaf. Unique nodes mean the seen-set never short-circuits, so the
    // depth counter reaches the limit and the error arm of `?` fires.
    let mut objects: Vec<(u32, Vec<u8>)> = vec![
        (
            1,
            b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>".to_vec(),
        ),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>".to_vec(),
        ),
        (3, b"<< /Type /Page /Parent 2 0 R >>".to_vec()),
        (4, b"<< /Fields [5 0 R] /SigFlags 3 >>".to_vec()),
    ];
    for level in 0..depth {
        let obj = 5 + level;
        let next = obj + 1;
        objects.push((obj, format!("<< /Kids [{next} 0 R] >>").into_bytes()));
    }
    let leaf = 5 + depth;
    let sig = leaf + 1;
    objects.push((leaf, format!("<< /FT /Sig /V {sig} 0 R >>").into_bytes()));
    objects.push((
        sig,
        b"<< /Type /Sig /ByteRange [0 10 20 30] /Contents <00> >>".to_vec(),
    ));
    let borrowed: Vec<(u32, &[u8])> = objects.iter().map(|(n, b)| (*n, b.as_slice())).collect();
    build_pdf(&borrowed)
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
fn rewrite_impact_errors_on_signature_field_depth_exceeded() {
    // A chain of unique nodes longer than the depth limit triggers the Err arm
    // inside walk_signature_rewrite_field (and the `?` propagation path).
    let depth = (DEFAULT_MAX_SIGNATURE_FIELD_DEPTH + 1) as u32;
    let mut pdf = open(build_deep_signature_chain_pdf(depth));
    let result = signature_rewrite_impact(&mut pdf, SignatureWriteMode::FullRewrite);
    assert!(result.is_err(), "depth-exceeded chain must return Err");
}

#[test]
fn rewrite_impact_detects_sig_via_sig_parent_when_non_sig_parent_visited_first() {
    // Regression: keying the seen set only on ObjectRef caused a false negative
    // when the same leaf (no /FT) is a child of both a /Sig parent and a
    // non-/Sig parent.  The non-/Sig path visits the leaf first (inserting its
    // ref into seen), then the /Sig path skips it → /V never collected →
    // incremental rewrite reported as preserving signatures when it shouldn't.
    let mut pdf = open(build_shared_child_mixed_ft_pdf());

    let impact = signature_rewrite_impact(&mut pdf, SignatureWriteMode::FullRewrite).unwrap();

    assert!(
        impact.has_signatures,
        "signature leaf reachable only via /Sig parent must be detected even if non-/Sig parent visited first"
    );
}

#[test]
fn rewrite_impact_deduplicates_shared_acroform_kids() {
    // Regression: walk_signature_rewrite_field lacked a seen-set, so a shared
    // /Kids graph (each node's /Kids lists the same child twice) caused
    // exponential traversal. Depth 24 timed out before the fix.
    let mut pdf = open(build_shared_kids_signature_pdf(24));

    let impact = signature_rewrite_impact(&mut pdf, SignatureWriteMode::FullRewrite).unwrap();

    assert!(impact.has_signatures);
    assert!(impact.invalidates_signatures);
    assert_eq!(impact.reason, SignatureRewriteReason::FullRewrite);
    // leaf field obj (5 + 24 = 29) and its /V sig dict (30) must be collected
    assert!(impact.signed_object_refs.contains(&ObjectRef::new(29, 0)));
    assert!(impact.signed_object_refs.contains(&ObjectRef::new(30, 0)));
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
fn full_rewrite_invalidates_signed_pdf_with_indirect_ft() {
    // Regression (flpdf-967): a signature field whose /FT is an indirect
    // reference must still be detected, so a full rewrite is flagged as
    // signature-invalidating instead of silently destroying the signature.
    let mut pdf = open(build_signed_acroform_indirect_ft_pdf());

    let impact = signature_rewrite_impact(&mut pdf, SignatureWriteMode::FullRewrite).unwrap();

    assert!(
        impact.has_signatures,
        "signature field with indirect /FT must be detected"
    );
    assert!(impact.invalidates_signatures);
    assert_eq!(impact.reason, SignatureRewriteReason::FullRewrite);
}

#[test]
fn full_rewrite_of_signed_pdf_proceeds_and_preserves_signatures() {
    // qpdf does NOT refuse a full rewrite of a signed PDF — it proceeds, leaving
    // the signature objects present-but-invalid (verified, qpdf 11.9.0: both
    // `qpdf signed.pdf out.pdf` and `qpdf signed.pdf --pages signed.pdf 1 -- out`
    // exit 0 with /FT /Sig + /ByteRange preserved, no stderr warning). flpdf
    // matches this pre-v1.0 (the signed-full-rewrite refusal was removed; signed
    // preserve-by-default protection is deferred post-v1.0 — flpdf-hn1g.13/.14).
    let mut pdf = open(build_signed_acroform_pdf());
    let mut options = WriteOptions::default();
    options.full_rewrite = true;

    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, &options)
        .expect("signed full rewrite proceeds (qpdf-compatible, no refusal)");

    // Reopen the output: the signature is still detectable, i.e. the /FT /Sig
    // field and its /ByteRange signature dict were preserved (not stripped, not
    // nulled) — matching qpdf's posture.
    let mut re = open(out);
    let sigs = signatures(&mut re).expect("inspect signatures in output");
    assert_eq!(
        sigs.len(),
        1,
        "signature object must be preserved by the full rewrite, not stripped"
    );
}

#[test]
fn full_rewrite_of_signed_pdf_with_malformed_signature_proceeds() {
    // Even with malformed signature details (a non-numeric /ByteRange), the
    // full-rewrite path proceeds (qpdf-compatible: no refusal).
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

    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, &options)
        .expect("signed full rewrite proceeds even with malformed signature details");
    assert!(!out.is_empty());
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
