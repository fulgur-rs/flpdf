//! Integration tests for [`flpdf::AcroFormDocumentHelper`].

use flpdf::{AcroFormDocumentHelper, Object, ObjectRef, Pdf};
use std::collections::BTreeMap;

fn build_pdf(objects: &[(u32, &str)], root: u32) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();
    let max = objects.iter().map(|(n, _)| *n).max().unwrap_or(0);

    for (n, body) in objects {
        offsets.insert(*n, out.len() as u64);
        out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    }

    let xref_start = out.len() as u64;
    let size = max + 1;
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for n in 1..=max {
        match offsets.get(&n) {
            Some(offset) => out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root {root} 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );
    out
}

fn form_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (
                4,
                "<< /Fields [5 0 R] /DA (/Helv 10 Tf 0 g) /DR << /Font << /Helv 7 0 R >> >> >>",
            ),
            (5, "<< /T (parent) /FT /Tx /Kids [6 0 R] >>"),
            (6, "<< /T (child) /Parent 5 0 R /V (before) >>"),
            (7, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
        ],
        1,
    )
}

fn empty_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        ],
        1,
    )
}

fn target_form_defaults_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (
                4,
                "<< /Fields [] /DA (/Other 8 Tf 1 0 0 rg) /DR << /Font << /Other 5 0 R >> >> >>",
            ),
            (5, "<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>"),
        ],
        1,
    )
}

fn parent_da_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Fields [5 0 R] /DA (/Doc 10 Tf 0 g) >>"),
            (
                5,
                "<< /T (parent) /FT /Tx /DA (/Parent 11 Tf 1 0 0 rg) /Kids [6 0 R] >>",
            ),
            (6, "<< /T (child) /Parent 5 0 R /V (value) >>"),
        ],
        1,
    )
}

#[test]
fn fields_walks_acroform_field_tree() {
    let bytes = form_pdf();
    let mut pdf = Pdf::open_mem(&bytes).unwrap();

    let fields = pdf.acroform().fields().unwrap();

    assert_eq!(fields, vec![ObjectRef::new(5, 0), ObjectRef::new(6, 0)]);
}

#[test]
fn field_value_get_set_uses_live_document() {
    let bytes = form_pdf();
    let mut pdf = Pdf::open_mem(&bytes).unwrap();

    {
        let mut acroform = AcroFormDocumentHelper::new(&mut pdf);
        assert_eq!(
            acroform.field_value(ObjectRef::new(6, 0)).unwrap(),
            Some(Object::String(b"before".to_vec()))
        );
        acroform
            .set_field_value(ObjectRef::new(6, 0), Object::String(b"after".to_vec()))
            .unwrap();
    }

    let mut acroform = pdf.acroform();
    assert_eq!(
        acroform.field_value(ObjectRef::new(6, 0)).unwrap(),
        Some(Object::String(b"after".to_vec()))
    );
}

#[test]
fn default_appearance_is_set_and_inherited_to_fields() {
    let bytes = form_pdf();
    let mut pdf = Pdf::open_mem(&bytes).unwrap();
    let da = b"/F1 9 Tf 0 0 1 rg".to_vec();

    {
        let mut acroform = pdf.acroform();
        acroform.set_default_appearance(da.clone()).unwrap();
        acroform.fix_appearance_inheritance().unwrap();
    }

    let acroform = pdf.resolve(ObjectRef::new(4, 0)).unwrap();
    let Object::Dictionary(acroform_dict) = acroform else {
        panic!("AcroForm should be a dictionary");
    };
    assert_eq!(acroform_dict.get("DA"), Some(&Object::String(da.clone())));

    let child = pdf.resolve(ObjectRef::new(6, 0)).unwrap();
    let Object::Dictionary(child_dict) = child else {
        panic!("child field should be a dictionary");
    };
    assert_eq!(child_dict.get("DA"), Some(&Object::String(da)));
}

#[test]
fn fix_appearance_inheritance_respects_parent_field_da() {
    let bytes = parent_da_pdf();
    let mut pdf = Pdf::open_mem(&bytes).unwrap();

    pdf.acroform().fix_appearance_inheritance().unwrap();

    let child = pdf.resolve(ObjectRef::new(6, 0)).unwrap();
    let Object::Dictionary(child_dict) = child else {
        panic!("child field should be a dictionary");
    };
    assert_eq!(
        child_dict.get("DA"),
        Some(&Object::String(b"/Parent 11 Tf 1 0 0 rg".to_vec()))
    );
}

#[test]
fn fields_errors_when_field_tree_depth_limit_is_exceeded() {
    let mut objects = vec![
        (
            1,
            "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>".to_string(),
        ),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_string(),
        ),
        (4, "<< /Fields [5 0 R] >>".to_string()),
    ];
    for object_number in 5..=106 {
        let kid = object_number + 1;
        objects.push((
            object_number,
            format!("<< /T (f{object_number}) /Kids [{kid} 0 R] >>"),
        ));
    }
    objects.push((107, "<< /T (leaf) >>".to_string()));
    let borrowed: Vec<(u32, &str)> = objects
        .iter()
        .map(|(object_number, body)| (*object_number, body.as_str()))
        .collect();
    let bytes = build_pdf(&borrowed, 1);
    let mut pdf = Pdf::open_mem(&bytes).unwrap();

    let err = pdf.acroform().fields().unwrap_err();

    assert!(
        matches!(err, flpdf::Error::Unsupported(_)),
        "expected depth-limit Unsupported error, got {err:?}"
    );
}

#[test]
fn copy_fields_from_appends_copied_fields_to_target_acroform() {
    let source_bytes = form_pdf();
    let target_bytes = empty_pdf();
    let mut source = Pdf::open_mem(&source_bytes).unwrap();
    let mut target = Pdf::open_mem(&target_bytes).unwrap();

    let copied = target.acroform().copy_fields_from(&mut source).unwrap();

    assert_eq!(copied.len(), 1, "only top-level fields are appended");
    let fields = target.acroform().fields().unwrap();
    assert_eq!(
        fields.len(),
        2,
        "top field plus copied child should be reachable"
    );

    let value = target.acroform().field_value(fields[1]).unwrap();
    assert_eq!(value, Some(Object::String(b"before".to_vec())));
}

#[test]
fn copy_fields_from_copies_acroform_da_and_dr_defaults() {
    let source_bytes = form_pdf();
    let target_bytes = empty_pdf();
    let mut source = Pdf::open_mem(&source_bytes).unwrap();
    let mut target = Pdf::open_mem(&target_bytes).unwrap();

    target.acroform().copy_fields_from(&mut source).unwrap();

    let catalog = target.resolve(ObjectRef::new(1, 0)).unwrap();
    let Object::Dictionary(catalog_dict) = catalog else {
        panic!("catalog should be a dictionary");
    };
    let acroform_ref = catalog_dict
        .get_ref("AcroForm")
        .expect("target catalog should reference AcroForm");
    let acroform = target.resolve(acroform_ref).unwrap();
    let Object::Dictionary(acroform_dict) = acroform else {
        panic!("AcroForm should be a dictionary");
    };

    assert_eq!(
        acroform_dict.get("DA"),
        Some(&Object::String(b"/Helv 10 Tf 0 g".to_vec()))
    );
    let Object::Dictionary(dr) = acroform_dict.get("DR").expect("copied /DR") else {
        panic!("/DR should be a direct dictionary");
    };
    let Object::Dictionary(fonts) = dr.get("Font").expect("/DR/Font") else {
        panic!("/DR/Font should be a dictionary");
    };
    let Object::Reference(font_ref) = fonts.get("Helv").expect("/DR/Font/Helv") else {
        panic!("Helv font should be remapped as a reference");
    };
    assert!(
        font_ref.number > 3,
        "copied font should use a fresh target object number"
    );

    let font = target.resolve(*font_ref).unwrap();
    let Object::Dictionary(font_dict) = font else {
        panic!("copied font should be a dictionary");
    };
    assert_eq!(
        font_dict.get("BaseFont"),
        Some(&Object::Name(b"Helvetica".to_vec()))
    );
}

#[test]
fn copy_fields_from_materializes_source_defaults_when_target_has_defaults() {
    let source_bytes = form_pdf();
    let target_bytes = target_form_defaults_pdf();
    let mut source = Pdf::open_mem(&source_bytes).unwrap();
    let mut target = Pdf::open_mem(&target_bytes).unwrap();

    let copied = target.acroform().copy_fields_from(&mut source).unwrap();

    let top = target.resolve(copied[0]).unwrap();
    let Object::Dictionary(top_dict) = top else {
        panic!("copied top field should be a dictionary");
    };
    assert_eq!(
        top_dict.get("DA"),
        Some(&Object::String(b"/Helv 10 Tf 0 g".to_vec())),
        "copied field should inherit source, not target, AcroForm /DA"
    );

    let acroform = target.resolve(ObjectRef::new(4, 0)).unwrap();
    let Object::Dictionary(acroform_dict) = acroform else {
        panic!("target AcroForm should be a dictionary");
    };
    assert_eq!(
        acroform_dict.get("DA"),
        Some(&Object::String(b"/Other 8 Tf 1 0 0 rg".to_vec())),
        "target AcroForm /DA should remain unchanged"
    );

    let Object::Dictionary(dr) = acroform_dict.get("DR").expect("target /DR") else {
        panic!("/DR should be a dictionary");
    };
    let Object::Dictionary(fonts) = dr.get("Font").expect("/DR/Font") else {
        panic!("/DR/Font should be a dictionary");
    };
    assert!(fonts.get("Other").is_some(), "target font should remain");
    assert!(fonts.get("Helv").is_some(), "source font should be merged");
}
