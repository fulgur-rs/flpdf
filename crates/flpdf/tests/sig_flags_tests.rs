//! Tests for /AcroForm /SigFlags reading, preservation, surfacing, and clearing.

use flpdf::{
    acroform_sig_flags, clear_sig_flags, signature_rewrite_impact, write_pdf,
    write_pdf_with_options, Object, ObjectRef, Pdf, SignatureWriteMode, WriteOptions,
    SIG_FLAGS_APPEND_ONLY, SIG_FLAGS_SIGNATURES_EXIST,
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

/// Signed AcroForm with `/SigFlags 3` (SignaturesExist | AppendOnly).
fn build_signed_acroform_pdf() -> Vec<u8> {
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
            b"<< /Type /Annot /Subtype /Widget /FT /Sig /T (Signed) /V 6 0 R /P 3 0 R /Rect [0 0 10 10] >>",
        ),
        (
            6,
            b"<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached /ByteRange [0 10 20 30] /Contents <00> >>",
        ),
    ];
    build_pdf(&objects)
}

/// Catalog with an *inline* `/AcroForm << ... /SigFlags 3 >>` dictionary
/// (no indirect reference), exercising the direct-dict code paths.
fn build_inline_acroform_pdf() -> Vec<u8> {
    let objects: Vec<(u32, &[u8])> = vec![
        (
            1,
            b"<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [5 0 R] /SigFlags 3 >> >>",
        ),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>",
        ),
        (3, b"<< /Type /Page /Parent 2 0 R /Annots [5 0 R] >>"),
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

/// Catalog with an `/AcroForm` (indirect) that has `/Fields` but no `/SigFlags`.
fn build_acroform_without_sig_flags_pdf() -> Vec<u8> {
    let objects: Vec<(u32, &[u8])> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>",
        ),
        (3, b"<< /Type /Page /Parent 2 0 R >>"),
        (4, b"<< /Fields [] >>"),
    ];
    build_pdf(&objects)
}

/// Catalog whose `/AcroForm` indirectly references a non-dictionary object.
fn build_acroform_indirect_non_dict_pdf() -> Vec<u8> {
    let objects: Vec<(u32, &[u8])> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>",
        ),
        (3, b"<< /Type /Page /Parent 2 0 R >>"),
        (4, b"[1 2 3]"),
    ];
    build_pdf(&objects)
}

/// Catalog whose `/AcroForm` is neither a reference nor a dictionary.
fn build_acroform_non_dict_inline_pdf() -> Vec<u8> {
    let objects: Vec<(u32, &[u8])> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 5 >>"),
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
fn reads_sig_flags_bitfield() {
    let mut pdf = open(build_signed_acroform_pdf());

    let flags = acroform_sig_flags(&mut pdf).unwrap();

    assert_eq!(flags, Some(3));
    let flags = flags.unwrap();
    assert_ne!(flags & SIG_FLAGS_SIGNATURES_EXIST, 0);
    assert_ne!(flags & SIG_FLAGS_APPEND_ONLY, 0);
}

#[test]
fn sig_flags_absent_without_acroform() {
    let mut pdf = open(build_unsigned_pdf());

    assert_eq!(acroform_sig_flags(&mut pdf).unwrap(), None);
}

#[test]
fn impact_surfaces_sig_flags_and_append_only() {
    let mut pdf = open(build_signed_acroform_pdf());

    let impact = signature_rewrite_impact(&mut pdf, SignatureWriteMode::Incremental).unwrap();

    assert_eq!(impact.sig_flags, Some(3));
    assert!(impact.signatures_exist());
    assert!(impact.append_only());
}

#[test]
fn impact_without_acroform_has_no_sig_flags() {
    let mut pdf = open(build_unsigned_pdf());

    let impact = signature_rewrite_impact(&mut pdf, SignatureWriteMode::FullRewrite).unwrap();

    assert_eq!(impact.sig_flags, None);
    assert!(!impact.signatures_exist());
    assert!(!impact.append_only());
}

#[test]
fn full_rewrite_round_trip_preserves_sig_flags() {
    let mut pdf = open(build_signed_acroform_pdf());
    let mut options = WriteOptions::default();
    options.full_rewrite = true;

    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, &options).unwrap();

    let mut reopened = open(out);
    assert_eq!(acroform_sig_flags(&mut reopened).unwrap(), Some(3));
}

#[test]
fn incremental_round_trip_preserves_sig_flags() {
    let mut pdf = open(build_signed_acroform_pdf());

    let mut out = Vec::new();
    write_pdf(&mut pdf, &mut out).unwrap();

    let mut reopened = open(out);
    assert_eq!(acroform_sig_flags(&mut reopened).unwrap(), Some(3));
}

#[test]
fn clear_sig_flags_clears_signature_bits() {
    let mut pdf = open(build_signed_acroform_pdf());

    let changed = clear_sig_flags(&mut pdf).unwrap();

    assert!(changed);
    assert_eq!(acroform_sig_flags(&mut pdf).unwrap(), Some(0));
}

#[test]
fn clear_sig_flags_is_noop_without_acroform() {
    let mut pdf = open(build_unsigned_pdf());

    assert!(!clear_sig_flags(&mut pdf).unwrap());
}

#[test]
fn sig_flags_absent_when_acroform_has_no_sig_flags() {
    let mut pdf = open(build_acroform_without_sig_flags_pdf());

    assert_eq!(acroform_sig_flags(&mut pdf).unwrap(), None);
}

#[test]
fn clear_sig_flags_is_noop_when_acroform_has_no_bits() {
    let mut pdf = open(build_acroform_without_sig_flags_pdf());

    assert!(!clear_sig_flags(&mut pdf).unwrap());
}

#[test]
fn handles_acroform_indirectly_referencing_non_dictionary() {
    let mut pdf = open(build_acroform_indirect_non_dict_pdf());

    assert_eq!(acroform_sig_flags(&mut pdf).unwrap(), None);
    assert!(!clear_sig_flags(&mut pdf).unwrap());
}

#[test]
fn handles_acroform_that_is_not_a_dictionary() {
    let mut pdf = open(build_acroform_non_dict_inline_pdf());

    assert_eq!(acroform_sig_flags(&mut pdf).unwrap(), None);
    assert!(!clear_sig_flags(&mut pdf).unwrap());
}

#[test]
fn reads_sig_flags_from_inline_acroform() {
    let mut pdf = open(build_inline_acroform_pdf());

    assert_eq!(acroform_sig_flags(&mut pdf).unwrap(), Some(3));
}

#[test]
fn clear_sig_flags_clears_inline_acroform_without_clobbering_catalog() {
    let mut pdf = open(build_inline_acroform_pdf());

    let changed = clear_sig_flags(&mut pdf).unwrap();

    assert!(changed);
    assert_eq!(acroform_sig_flags(&mut pdf).unwrap(), Some(0));

    // The catalog itself must be intact, not replaced by the AcroForm sub-dict.
    let root_ref = pdf.root_ref().unwrap();
    let Object::Dictionary(catalog) = pdf.resolve(root_ref).unwrap() else {
        panic!("catalog should still be a dictionary");
    };
    assert_eq!(
        catalog.get("Type"),
        Some(&Object::Name(b"Catalog".to_vec()))
    );
    assert_eq!(
        catalog.get("Pages"),
        Some(&Object::Reference(ObjectRef::new(2, 0)))
    );
}
