//! Tests for /AcroForm /SigFlags reading, preservation, surfacing, and clearing.

use flpdf::{
    acroform_sig_flags, clear_sig_flags, disable_digital_signatures, remove_security_restrictions,
    signature_rewrite_impact, strip_signature_values, write_pdf, write_pdf_with_options, Object,
    ObjectRef, Pdf, SignatureWriteMode, WriteOptions, DEFAULT_MAX_SIGNATURE_FIELD_DEPTH,
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

/// Catalog with an `/AcroForm` dictionary that omits `/Fields`.
fn build_acroform_without_fields_pdf() -> Vec<u8> {
    let objects: Vec<(u32, &[u8])> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>",
        ),
        (3, b"<< /Type /Page /Parent 2 0 R >>"),
        (4, b"<< /SigFlags 3 >>"),
    ];
    build_pdf(&objects)
}

/// Nested field tree where `/FT /Sig` is inherited from the parent.
fn build_nested_signature_fields_pdf() -> Vec<u8> {
    let objects: Vec<(u32, &[u8])> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>",
        ),
        (3, b"<< /Type /Page /Parent 2 0 R /Annots [8 0 R] >>"),
        (4, b"<< /Fields [5 0 R 9 0 R 11] /SigFlags 3 >>"),
        (
            5,
            b"<< /FT /Sig /T (Parent) /V 7 0 R /Kids [6 0 R 8 0 R 10 0 R 12] >>",
        ),
        (6, b"<< /T (Child) /Parent 5 0 R /V 7 0 R >>"),
        (
            7,
            b"<< /Type /Sig /ByteRange [0 10 20 30] /Contents <00> >>",
        ),
        (
            8,
            b"<< /Type /Annot /Subtype /Widget /Parent 6 0 R /P 3 0 R /Rect [0 0 10 10] >>",
        ),
        (9, b"<< /FT /Tx /T (Text) /V (keep me) >>"),
        (10, b"(not a dictionary)"),
    ];
    build_pdf(&objects)
}

/// Signature field whose `/FT` value is an indirect name object.
fn build_indirect_ft_signature_field_pdf() -> Vec<u8> {
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
            b"<< /Type /Sig /ByteRange [0 10 20 30] /Contents <00> >>",
        ),
        (7, b"/Sig"),
    ];
    build_pdf(&objects)
}

fn has_entry(pdf: &mut Pdf<Cursor<Vec<u8>>>, object_ref: ObjectRef, key: &str) -> bool {
    match pdf.resolve(object_ref).unwrap() {
        Object::Dictionary(dict) => dict.get(key).is_some(),
        _ => false,
    }
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

/// Catalog with both a `/Perms` dictionary and an indirect `/AcroForm` that
/// carries `/SigFlags 3`, exercising the combined Perms-drop + SigFlags-zero path.
fn build_perms_and_acroform_pdf() -> Vec<u8> {
    let objects: Vec<(u32, &[u8])> = vec![
        (
            1,
            b"<< /Type /Catalog /Pages 2 0 R /Perms << /DocMDP 5 0 R >> /AcroForm 4 0 R >>",
        ),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>",
        ),
        (3, b"<< /Type /Page /Parent 2 0 R >>"),
        (4, b"<< /Fields [] /SigFlags 3 >>"),
        (5, b"<< /Type /Sig /ByteRange [0 10 20 30] >>"),
    ];
    build_pdf(&objects)
}

/// Catalog with a `/Perms` dictionary but no `/AcroForm`, exercising the
/// Perms-drop path when there is no signature form to touch.
fn build_perms_docmdp_only_pdf() -> Vec<u8> {
    let objects: Vec<(u32, &[u8])> = vec![
        (
            1,
            b"<< /Type /Catalog /Pages 2 0 R /Perms << /DocMDP 4 0 R >> >>",
        ),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>",
        ),
        (3, b"<< /Type /Page /Parent 2 0 R >>"),
        (4, b"<< /Type /Sig /ByteRange [0 10 20 30] >>"),
    ];
    build_pdf(&objects)
}

/// Catalog `/Root` resolves to a non-dictionary object, exercising the guard
/// that bails out when the catalog is not a dictionary.
fn build_nondict_root_pdf() -> Vec<u8> {
    let objects: Vec<(u32, &[u8])> = vec![(1, b"[1 2 3]")];
    build_pdf(&objects)
}

/// Trailer without a `/Root`, so `root_ref()` is `None` and removal is a no-op.
fn build_no_root_pdf() -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let off1 = out.len() as u64;
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let xref_pos = out.len() as u64;
    out.extend_from_slice(
        format!(
            "xref\n0 2\n0000000000 65535 f \n{off1:010} 00000 n \ntrailer\n<< /Size 2 >>\nstartxref\n{xref_pos}\n%%EOF\n"
        )
        .as_bytes(),
    );
    out
}

fn open(bytes: Vec<u8>) -> Pdf<Cursor<Vec<u8>>> {
    Pdf::open(Cursor::new(bytes)).expect("PDF should parse")
}

/// Read the top-level `/AcroForm /Fields` array via public API.
fn acroform_fields(pdf: &mut Pdf<Cursor<Vec<u8>>>) -> Vec<Object> {
    let root_ref = pdf.root_ref().unwrap();
    let Object::Dictionary(cat) = pdf.resolve(root_ref).unwrap() else {
        return Vec::new();
    };
    let Some(af) = cat.get("AcroForm").cloned() else {
        return Vec::new();
    };
    let af = match af {
        Object::Reference(r) => pdf.resolve(r).unwrap(),
        other => other,
    };
    let Object::Dictionary(afd) = af else {
        return Vec::new();
    };
    match afd.get("Fields").cloned() {
        Some(Object::Array(a)) => a,
        Some(Object::Reference(r)) => match pdf.resolve(r).unwrap() {
            Object::Array(a) => a,
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// A single top-level `/Sig` field referenced only from `/AcroForm /Fields`
/// (not from any page `/Annots`), so on rewrite it becomes unreferenced.
fn build_disable_sig_field_only_pdf() -> Vec<u8> {
    let objects: Vec<(u32, &[u8])> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>",
        ),
        (3, b"<< /Type /Page /Parent 2 0 R >>"),
        (4, b"<< /Fields [5 0 R] /SigFlags 3 >>"),
        (
            5,
            b"<< /FT /Sig /T (Approval) /V 6 0 R /SV << /Type /SV >> /Lock << /Type /SigFieldLock >> /Rect [0 0 0 0] >>",
        ),
        (6, b"<< /Type /Sig /ByteRange [0 10 20 30] >>"),
    ];
    build_pdf(&objects)
}

/// A `/Sig` field that is also a page `/Annots` widget, so on rewrite the
/// stripped field survives as a plain annotation.
fn build_disable_sig_widget_pdf() -> Vec<u8> {
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
            b"<< /Type /Annot /Subtype /Widget /FT /Sig /T (Approval) /V 6 0 R /Rect [10 20 30 40] /P 3 0 R >>",
        ),
        (6, b"<< /Type /Sig /ByteRange [0 10 20 30] >>"),
    ];
    build_pdf(&objects)
}

/// A top-level field with no local `/FT` whose type must be resolved up a
/// `/Parent` chain longer than the depth limit, so the walk over the top-level
/// `/Fields` array propagates the depth-limit error out of
/// `disable_digital_signatures`.
fn build_deep_parent_chain_top_field_pdf() -> Vec<u8> {
    let hops = DEFAULT_MAX_SIGNATURE_FIELD_DEPTH as u32 + 30;
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
        (4, b"<< /Fields [5 0 R] >>".to_vec()),
    ];
    // Field 5 (top-level) has no /FT; its /FT is sought up 5 -> 6 -> 7 -> ...
    for level in 0..hops {
        let obj = 5 + level;
        let next = obj + 1;
        objects.push((obj, format!("<< /Parent {next} 0 R >>").into_bytes()));
    }
    let tail = 5 + hops;
    objects.push((tail, b"<< >>".to_vec()));
    let borrowed: Vec<(u32, &[u8])> = objects.iter().map(|(n, b)| (*n, b.as_slice())).collect();
    build_pdf(&borrowed)
}

/// A top-level `/Sig` field whose kid has no local `/FT` and a `/Parent` chain
/// longer than the depth limit, so the error is raised while descending into
/// `/Kids` and propagates back out through the kids walker.
fn build_deep_parent_chain_kid_pdf() -> Vec<u8> {
    let hops = DEFAULT_MAX_SIGNATURE_FIELD_DEPTH as u32 + 30;
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
        (4, b"<< /Fields [5 0 R] >>".to_vec()),
        (5, b"<< /FT /Sig /Kids [6 0 R] >>".to_vec()),
    ];
    // Kid 6 has no /FT; its /FT is sought up 6 -> 7 -> 8 -> ...
    for level in 0..hops {
        let obj = 6 + level;
        let next = obj + 1;
        objects.push((obj, format!("<< /Parent {next} 0 R >>").into_bytes()));
    }
    let tail = 6 + hops;
    objects.push((tail, b"<< >>".to_vec()));
    let borrowed: Vec<(u32, &[u8])> = objects.iter().map(|(n, b)| (*n, b.as_slice())).collect();
    build_pdf(&borrowed)
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
    // The full-rewrite path proceeds on signed PDFs (qpdf-compatible; the
    // signed-rewrite refusal was removed pre-v1.0). This exercises the
    // SigFlags-preservation path through that rewrite.

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
fn strip_signature_values_removes_direct_signature_field_value() {
    let mut pdf = open(build_signed_acroform_pdf());

    let changed = strip_signature_values(&mut pdf).unwrap();

    assert!(changed);
    assert!(!has_entry(&mut pdf, ObjectRef::new(5, 0), "V"));
    assert_eq!(pdf.resolve(ObjectRef::new(6, 0)).unwrap(), Object::Null);
    assert!(pdf.signatures().unwrap().is_empty());
    assert_eq!(acroform_sig_flags(&mut pdf).unwrap(), Some(3));
}

#[test]
fn strip_signature_values_removes_inherited_signature_field_value() {
    let mut pdf = open(build_nested_signature_fields_pdf());

    let changed = strip_signature_values(&mut pdf).unwrap();

    assert!(changed);
    assert!(!has_entry(&mut pdf, ObjectRef::new(5, 0), "V"));
    assert!(!has_entry(&mut pdf, ObjectRef::new(6, 0), "V"));
    assert_eq!(pdf.resolve(ObjectRef::new(7, 0)).unwrap(), Object::Null);
    assert!(has_entry(&mut pdf, ObjectRef::new(9, 0), "V"));
    assert!(pdf.signatures().unwrap().is_empty());
}

#[test]
fn strip_signature_values_resolves_indirect_field_type() {
    let mut pdf = open(build_indirect_ft_signature_field_pdf());

    let changed = strip_signature_values(&mut pdf).unwrap();

    assert!(changed);
    assert!(!has_entry(&mut pdf, ObjectRef::new(5, 0), "V"));
    assert_eq!(pdf.resolve(ObjectRef::new(6, 0)).unwrap(), Object::Null);
    assert!(pdf.signatures().unwrap().is_empty());
}

#[test]
fn strip_signature_values_is_noop_without_acroform_fields() {
    let mut no_acroform = open(build_unsigned_pdf());
    assert!(!strip_signature_values(&mut no_acroform).unwrap());

    let mut no_fields = open(build_acroform_without_fields_pdf());
    assert!(!strip_signature_values(&mut no_fields).unwrap());

    let mut empty_fields = open(build_acroform_without_sig_flags_pdf());
    assert!(!strip_signature_values(&mut empty_fields).unwrap());
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

#[test]
fn remove_security_restrictions_drops_perms_and_zeros_sigflags() {
    let mut pdf = open(build_perms_and_acroform_pdf());
    assert!(remove_security_restrictions(&mut pdf).unwrap());
    let root_ref = pdf.root_ref().unwrap();
    let Object::Dictionary(cat) = pdf.resolve(root_ref).unwrap() else {
        panic!("catalog")
    };
    assert!(cat.get("Perms").is_none(), "/Perms must be removed");
    assert_eq!(acroform_sig_flags(&mut pdf).unwrap(), Some(0));
}

#[test]
fn remove_security_restrictions_removes_perms_when_acroform_absent() {
    let mut pdf = open(build_perms_docmdp_only_pdf());
    assert!(remove_security_restrictions(&mut pdf).unwrap());
    let root_ref = pdf.root_ref().unwrap();
    let Object::Dictionary(cat) = pdf.resolve(root_ref).unwrap() else {
        panic!("catalog")
    };
    assert!(cat.get("Perms").is_none());
}

#[test]
fn remove_security_restrictions_is_noop_without_perms_or_sigflags() {
    let mut pdf = open(build_unsigned_pdf());
    assert!(!remove_security_restrictions(&mut pdf).unwrap());
}

#[test]
fn remove_security_restrictions_zeros_sigflags_without_perms() {
    // AcroForm /SigFlags present but no catalog /Perms -> changed via SigFlags only.
    let mut pdf = open(build_signed_acroform_pdf());
    assert!(remove_security_restrictions(&mut pdf).unwrap());
    assert_eq!(acroform_sig_flags(&mut pdf).unwrap(), Some(0));
}

#[test]
fn remove_security_restrictions_is_noop_when_root_missing() {
    let mut pdf = open(build_no_root_pdf());
    assert!(!remove_security_restrictions(&mut pdf).unwrap());
}

#[test]
fn remove_security_restrictions_is_noop_when_root_not_dictionary() {
    let mut pdf = open(build_nondict_root_pdf());
    assert!(!remove_security_restrictions(&mut pdf).unwrap());
}

#[test]
fn disable_digital_signatures_strips_sig_field_keys_and_erases_from_fields() {
    let mut pdf = open(build_disable_sig_field_only_pdf());
    assert!(disable_digital_signatures(&mut pdf).unwrap());
    // field 5: /FT /V removed, /T kept
    let f5 = ObjectRef::new(5, 0);
    assert!(!has_entry(&mut pdf, f5, "FT"));
    assert!(!has_entry(&mut pdf, f5, "V"));
    assert!(!has_entry(&mut pdf, f5, "SV"));
    assert!(!has_entry(&mut pdf, f5, "Lock"));
    assert!(
        has_entry(&mut pdf, f5, "T"),
        "/T (field name) must be preserved"
    );
    // the /V signature dictionary is deleted
    assert_eq!(pdf.resolve(ObjectRef::new(6, 0)).unwrap(), Object::Null);
    // top-level /Fields is now empty
    assert!(acroform_fields(&mut pdf).is_empty());
    assert_eq!(acroform_sig_flags(&mut pdf).unwrap(), Some(0));
    assert!(pdf.signatures().unwrap().is_empty());
}

#[test]
fn disable_digital_signatures_keeps_widget_but_strips_sig_keys() {
    let mut pdf = open(build_disable_sig_widget_pdf());
    assert!(disable_digital_signatures(&mut pdf).unwrap());
    let f5 = ObjectRef::new(5, 0);
    assert!(
        has_entry(&mut pdf, f5, "Subtype"),
        "widget annotation survives"
    );
    assert!(has_entry(&mut pdf, f5, "T"));
    assert!(!has_entry(&mut pdf, f5, "FT"));
    assert!(!has_entry(&mut pdf, f5, "V"));
    assert!(acroform_fields(&mut pdf).is_empty());
    assert_eq!(acroform_sig_flags(&mut pdf).unwrap(), Some(0));
}

#[test]
fn disable_digital_signatures_docmdp_only_removes_perms() {
    let mut pdf = open(build_perms_docmdp_only_pdf());
    assert!(disable_digital_signatures(&mut pdf).unwrap());
    let root_ref = pdf.root_ref().unwrap();
    let Object::Dictionary(cat) = pdf.resolve(root_ref).unwrap() else {
        panic!()
    };
    assert!(cat.get("Perms").is_none());
}

#[test]
fn disable_digital_signatures_is_noop_on_unsigned() {
    let mut pdf = open(build_unsigned_pdf());
    assert!(!disable_digital_signatures(&mut pdf).unwrap());
}

#[test]
fn disable_digital_signatures_zeros_sigflags_when_fields_absent() {
    // /AcroForm carries /SigFlags but no /Fields: removeSecurityRestrictions
    // zeros /SigFlags (changed = true) and the /Fields early-return leaves the
    // form otherwise untouched.
    let mut pdf = open(build_acroform_without_fields_pdf());
    assert!(disable_digital_signatures(&mut pdf).unwrap());
    assert_eq!(acroform_sig_flags(&mut pdf).unwrap(), Some(0));
}

#[test]
fn disable_digital_signatures_is_noop_when_fields_have_no_signatures() {
    // /AcroForm with an empty /Fields, no /SigFlags, no /Perms: nothing to
    // strip, so `to_remove` stays empty and the function reports no change.
    let mut pdf = open(build_acroform_without_sig_flags_pdf());
    assert!(!disable_digital_signatures(&mut pdf).unwrap());
    assert!(acroform_fields(&mut pdf).is_empty());
}

#[test]
fn disable_digital_signatures_strips_nested_sig_fields_and_keeps_non_sig() {
    let mut pdf = open(build_nested_signature_fields_pdf());
    assert!(disable_digital_signatures(&mut pdf).unwrap());

    // Parent /Sig field (5): /FT /V stripped, /T preserved.
    let f5 = ObjectRef::new(5, 0);
    assert!(!has_entry(&mut pdf, f5, "FT"));
    assert!(!has_entry(&mut pdf, f5, "V"));
    assert!(has_entry(&mut pdf, f5, "T"));
    // Child field (6) inherits /Sig and loses /V.
    assert!(!has_entry(&mut pdf, ObjectRef::new(6, 0), "V"));
    // Non-/Sig text field (9) is untouched.
    assert!(has_entry(&mut pdf, ObjectRef::new(9, 0), "FT"));
    assert!(has_entry(&mut pdf, ObjectRef::new(9, 0), "V"));

    // The orphaned signature dictionary (7) is deleted.
    assert_eq!(pdf.resolve(ObjectRef::new(7, 0)).unwrap(), Object::Null);
    assert!(pdf.signatures().unwrap().is_empty());

    // Only the top-level /Sig ref (5) is erased; the text field (9) and the
    // bare integer entry (11) remain.
    let fields = acroform_fields(&mut pdf);
    assert_eq!(
        fields.len(),
        2,
        "only field 5 erased from top-level /Fields"
    );
    assert!(fields.contains(&Object::Reference(ObjectRef::new(9, 0))));
    assert!(fields.contains(&Object::Integer(11)));
    assert_eq!(acroform_sig_flags(&mut pdf).unwrap(), Some(0));
}

#[test]
fn disable_digital_signatures_propagates_depth_error_from_top_field() {
    // A top-level field whose /FT resolution overflows the /Parent depth limit
    // surfaces the error out of the top-level /Fields walk.
    let mut pdf = open(build_deep_parent_chain_top_field_pdf());
    assert!(disable_digital_signatures(&mut pdf).is_err());
}

#[test]
fn disable_digital_signatures_propagates_depth_error_from_kid() {
    // The same depth-limit error, but raised while descending into a /Sig
    // field's /Kids, must propagate back through the kids walker.
    let mut pdf = open(build_deep_parent_chain_kid_pdf());
    assert!(disable_digital_signatures(&mut pdf).is_err());
}
