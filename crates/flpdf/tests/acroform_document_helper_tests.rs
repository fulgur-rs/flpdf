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

fn form_indirect_dr_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Fields [5 0 R] /DA (/Helv 10 Tf 0 g) /DR 8 0 R >>"),
            (5, "<< /T (parent) /FT /Tx /Kids [6 0 R] >>"),
            (6, "<< /T (child) /Parent 5 0 R /V (before) >>"),
            (7, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
            (8, "<< /Font << /Helv 7 0 R >> >>"),
        ],
        1,
    )
}

fn form_indirect_dr_category_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (
                4,
                "<< /Fields [5 0 R] /DA (/Helv 10 Tf 0 g) /DR << /Font 8 0 R >> >>",
            ),
            (5, "<< /T (parent) /FT /Tx /Kids [6 0 R] >>"),
            (6, "<< /T (child) /Parent 5 0 R /V (before) >>"),
            (7, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
            (8, "<< /Helv 7 0 R >>"),
        ],
        1,
    )
}

fn form_direct_field_da_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Fields [5 0 R] /DR << /Font << /Helv 7 0 R >> >> >>"),
            (
                5,
                "<< /T (parent) /FT /Tx /DA (/Helv 11 Tf 0 g) /Kids [6 0 R] >>",
            ),
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

fn direct_acroform_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [] >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        ],
        1,
    )
}

fn malformed_acroform_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm /Bad >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        ],
        1,
    )
}

fn malformed_fields_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Fields /Bad /DA (/Doc 10 Tf 0 g) >>"),
        ],
        1,
    )
}

fn no_default_appearance_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Fields [/Ignored 5 0 R] >>"),
            (5, "<< /T (field) /FT /Tx >>"),
        ],
        1,
    )
}

fn indirect_malformed_fields_pdf(fields: &str) -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Fields 5 0 R /DA (/Doc 10 Tf 0 g) >>"),
            (5, fields),
        ],
        1,
    )
}

fn source_without_defaults_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Fields [5 0 R] >>"),
            (5, "<< /T (parent) /FT /Tx /Kids [6 0 R /Ignored] >>"),
            (6, "<< /T (child) /Parent 5 0 R /V (before) >>"),
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

fn target_indirect_dr_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Fields [] /DA (/Other 8 Tf 1 0 0 rg) /DR 6 0 R >>"),
            (5, "<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>"),
            (6, "<< /Font << /Other 5 0 R >> >>"),
        ],
        1,
    )
}

fn target_indirect_dr_category_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (
                4,
                "<< /Fields [] /DA (/Other 8 Tf 1 0 0 rg) /DR << /Font 6 0 R >> >>",
            ),
            (5, "<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>"),
            (6, "<< /Other 5 0 R >>"),
        ],
        1,
    )
}

fn target_conflicting_font_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (
                4,
                "<< /Fields [] /DA (/Helv 8 Tf 1 0 0 rg) /DR << /Font << /Helv 5 0 R >> >> >>",
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
fn missing_or_malformed_acroform_shapes_are_noops() {
    let empty_bytes = empty_pdf();
    let mut empty = Pdf::open_mem(&empty_bytes).unwrap();
    assert!(empty.acroform().fields().unwrap().is_empty());
    empty.acroform().fix_appearance_inheritance().unwrap();

    let malformed_bytes = malformed_acroform_pdf();
    let mut malformed = Pdf::open_mem(&malformed_bytes).unwrap();
    assert!(malformed.acroform().fields().unwrap().is_empty());
    malformed.acroform().fix_appearance_inheritance().unwrap();
}

#[test]
fn malformed_fields_are_ignored_for_listing_and_appearance_fixup() {
    let bytes = malformed_fields_pdf();
    let mut pdf = Pdf::open_mem(&bytes).unwrap();

    assert!(pdf.acroform().fields().unwrap().is_empty());
    pdf.acroform().fix_appearance_inheritance().unwrap();
}

#[test]
fn missing_default_appearance_is_noop_but_fields_still_walk() {
    let bytes = no_default_appearance_pdf();
    let mut pdf = Pdf::open_mem(&bytes).unwrap();

    assert_eq!(pdf.acroform().fields().unwrap(), vec![ObjectRef::new(5, 0)]);
    pdf.acroform().fix_appearance_inheritance().unwrap();

    let field = pdf.resolve(ObjectRef::new(5, 0)).unwrap();
    let Object::Dictionary(field_dict) = field else {
        panic!("field should be a dictionary");
    };
    assert!(field_dict.get("DA").is_none());
}

#[test]
fn indirect_malformed_fields_are_ignored() {
    for fields in ["null", "/Bad"] {
        let bytes = indirect_malformed_fields_pdf(fields);
        let mut pdf = Pdf::open_mem(&bytes).unwrap();

        assert!(pdf.acroform().fields().unwrap().is_empty());
        pdf.acroform().fix_appearance_inheritance().unwrap();
    }
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
fn default_appearance_materializes_direct_catalog_acroform() {
    let bytes = direct_acroform_pdf();
    let mut pdf = Pdf::open_mem(&bytes).unwrap();
    let da = b"/Helv 12 Tf 0 g".to_vec();

    pdf.acroform().set_default_appearance(da.clone()).unwrap();

    let catalog = pdf.resolve(ObjectRef::new(1, 0)).unwrap();
    let Object::Dictionary(catalog_dict) = catalog else {
        panic!("catalog should be a dictionary");
    };
    let acroform_ref = catalog_dict
        .get_ref("AcroForm")
        .expect("direct AcroForm should be materialized as an indirect object");
    let acroform = pdf.resolve(acroform_ref).unwrap();
    let Object::Dictionary(acroform_dict) = acroform else {
        panic!("AcroForm should be a dictionary");
    };
    assert_eq!(acroform_dict.get("DA"), Some(&Object::String(da)));
    assert_eq!(
        acroform_dict.get("Fields"),
        Some(&Object::Array(Vec::new()))
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
fn copy_fields_from_empty_or_defaultless_source_preserves_target_defaults() {
    let empty_source_bytes = empty_pdf();
    let target_bytes = target_form_defaults_pdf();
    let mut empty_source = Pdf::open_mem(&empty_source_bytes).unwrap();
    let mut empty_target = Pdf::open_mem(&target_bytes).unwrap();

    assert!(empty_target
        .acroform()
        .copy_fields_from(&mut empty_source)
        .unwrap()
        .is_empty());

    let source_bytes = source_without_defaults_pdf();
    let target_bytes = target_form_defaults_pdf();
    let mut source = Pdf::open_mem(&source_bytes).unwrap();
    let mut target = Pdf::open_mem(&target_bytes).unwrap();

    let copied = target.acroform().copy_fields_from(&mut source).unwrap();

    assert_eq!(copied.len(), 1);
    let fields = target.acroform().fields().unwrap();
    assert_eq!(
        fields.len(),
        2,
        "non-reference kids should be ignored while copied fields remain reachable"
    );

    let acroform = target.resolve(ObjectRef::new(4, 0)).unwrap();
    let Object::Dictionary(acroform_dict) = acroform else {
        panic!("target AcroForm should be a dictionary");
    };
    assert_eq!(
        acroform_dict.get("DA"),
        Some(&Object::String(b"/Other 8 Tf 1 0 0 rg".to_vec()))
    );
    let top = target.resolve(copied[0]).unwrap();
    let Object::Dictionary(top_dict) = top else {
        panic!("copied top field should be a dictionary");
    };
    assert!(
        top_dict.get("DA").is_none(),
        "source without /DA should not materialize target defaults onto copied fields"
    );
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

#[test]
fn copy_fields_from_merges_indirect_source_default_resources() {
    let source_bytes = form_indirect_dr_pdf();
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
        "copied field should keep the source /DA that references source /DR"
    );

    let acroform = target.resolve(ObjectRef::new(4, 0)).unwrap();
    let Object::Dictionary(acroform_dict) = acroform else {
        panic!("target AcroForm should be a dictionary");
    };
    let Object::Dictionary(dr) = acroform_dict.get("DR").expect("target /DR") else {
        panic!("/DR should be a dictionary");
    };
    let Object::Dictionary(fonts) = dr.get("Font").expect("/DR/Font") else {
        panic!("/DR/Font should be a dictionary");
    };
    assert!(fonts.get("Other").is_some(), "target font should remain");
    assert!(
        fonts.get("Helv").is_some(),
        "source font from indirect /DR should be merged"
    );
}

#[test]
fn copy_fields_from_merges_indirect_default_resource_categories() {
    let source_bytes = form_indirect_dr_category_pdf();
    let target_bytes = target_indirect_dr_category_pdf();
    let mut source = Pdf::open_mem(&source_bytes).unwrap();
    let mut target = Pdf::open_mem(&target_bytes).unwrap();

    target.acroform().copy_fields_from(&mut source).unwrap();

    let acroform = target.resolve(ObjectRef::new(4, 0)).unwrap();
    let Object::Dictionary(acroform_dict) = acroform else {
        panic!("target AcroForm should be a dictionary");
    };
    let Object::Dictionary(dr) = acroform_dict.get("DR").expect("target /DR") else {
        panic!("/DR should be a dictionary");
    };
    let Object::Dictionary(fonts) = dr.get("Font").expect("/DR/Font") else {
        panic!("/DR/Font should be materialized as a dictionary");
    };
    assert!(fonts.get("Other").is_some(), "target font should remain");
    assert!(
        fonts.get("Helv").is_some(),
        "source font from indirect /DR/Font should be merged"
    );
}

#[test]
fn copy_fields_from_merges_into_indirect_target_default_resources() {
    let source_bytes = form_pdf();
    let target_bytes = target_indirect_dr_pdf();
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
        "copied field should keep the source /DA"
    );

    let acroform = target.resolve(ObjectRef::new(4, 0)).unwrap();
    let Object::Dictionary(acroform_dict) = acroform else {
        panic!("target AcroForm should be a dictionary");
    };
    let Object::Dictionary(dr) = acroform_dict.get("DR").expect("target /DR") else {
        panic!("/DR should be materialized as a dictionary");
    };
    let Object::Dictionary(fonts) = dr.get("Font").expect("/DR/Font") else {
        panic!("/DR/Font should be a dictionary");
    };
    assert!(fonts.get("Other").is_some(), "target font should remain");
    assert!(
        fonts.get("Helv").is_some(),
        "source font should be merged into indirect target /DR"
    );
}

#[test]
fn copy_fields_from_renames_conflicting_default_font_resources() {
    let source_bytes = form_pdf();
    let target_bytes = target_conflicting_font_pdf();
    let mut source = Pdf::open_mem(&source_bytes).unwrap();
    let mut target = Pdf::open_mem(&target_bytes).unwrap();

    let copied = target.acroform().copy_fields_from(&mut source).unwrap();

    let top = target.resolve(copied[0]).unwrap();
    let Object::Dictionary(top_dict) = top else {
        panic!("copied top field should be a dictionary");
    };
    let da = top_dict
        .get("DA")
        .and_then(Object::as_string)
        .expect("copied field should have materialized /DA");
    assert!(
        da.starts_with(b"/Helv_flpdf"),
        "conflicting source font should be renamed in copied /DA, got {}",
        String::from_utf8_lossy(da)
    );

    let acroform = target.resolve(ObjectRef::new(4, 0)).unwrap();
    let Object::Dictionary(acroform_dict) = acroform else {
        panic!("target AcroForm should be a dictionary");
    };
    let Object::Dictionary(dr) = acroform_dict.get("DR").expect("target /DR") else {
        panic!("/DR should be a dictionary");
    };
    let Object::Dictionary(fonts) = dr.get("Font").expect("/DR/Font") else {
        panic!("/DR/Font should be a dictionary");
    };
    assert_eq!(
        fonts.get("Helv"),
        Some(&Object::Reference(ObjectRef::new(5, 0))),
        "target /Helv font should remain intact"
    );
    let renamed_ref = fonts
        .iter()
        .find_map(|(name, value)| (name.starts_with(b"Helv_flpdf")).then(|| value.as_ref_id()))
        .flatten()
        .expect("renamed source font should be present");
    let renamed_font = target.resolve(renamed_ref).unwrap();
    let Object::Dictionary(font_dict) = renamed_font else {
        panic!("renamed source font should be a dictionary");
    };
    assert_eq!(
        font_dict.get("BaseFont"),
        Some(&Object::Name(b"Helvetica".to_vec()))
    );
}

#[test]
fn copy_fields_from_renames_conflicting_direct_field_da_resources() {
    let source_bytes = form_direct_field_da_pdf();
    let target_bytes = target_conflicting_font_pdf();
    let mut source = Pdf::open_mem(&source_bytes).unwrap();
    let mut target = Pdf::open_mem(&target_bytes).unwrap();

    let copied = target.acroform().copy_fields_from(&mut source).unwrap();

    let top = target.resolve(copied[0]).unwrap();
    let Object::Dictionary(top_dict) = top else {
        panic!("copied top field should be a dictionary");
    };
    let da = top_dict
        .get("DA")
        .and_then(Object::as_string)
        .expect("copied field should keep direct /DA");
    assert!(
        da.starts_with(b"/Helv_flpdf"),
        "conflicting direct field /DA should be renamed, got {}",
        String::from_utf8_lossy(da)
    );
}
