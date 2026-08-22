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

fn inherited_field_info_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (
                4,
                "<< /Fields [5 0 R] /DA (/Doc 10 Tf 0 g) /Q 1 /MaxLen 20 >>",
            ),
            (
                5,
                "<< /T (parent) /FT /Tx /DV (parent-default) /Ff 3 /Kids [6 0 R] >>",
            ),
            (
                6,
                "<< /T (child) /Parent 5 0 R /V (child-value) /DA (/Child 11 Tf 1 g) >>",
            ),
        ],
        1,
    )
}

fn field_info_widget_kids_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Fields [5 0 R] >>"),
            (
                5,
                "<< /T (field) /FT /Tx /Kids [6 0 R 7 0 R 8 0 R 9 0 R] >>",
            ),
            (
                6,
                "<< /Type /Annot /Subtype /Widget /Parent 5 0 R /Rect [0 0 10 10] >>",
            ),
            (
                7,
                "<< /Type /Annot /Subtype /Widget /Parent 5 0 R /T (merged) /V (yes) >>",
            ),
            (
                8,
                "<< /Type /Annot /Subtype /Widget /Parent 5 0 R /TU (tooltip) >>",
            ),
            (
                9,
                "<< /Type /Annot /Subtype /Widget /Parent 5 0 R /TM (mapping) >>",
            ),
        ],
        1,
    )
}

fn unicode_field_names_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Fields [5 0 R] >>"),
            (5, "<< /T <FEFF89AA> /FT /Tx /Kids [6 0 R] >>"),
            (6, "<< /T <FEFF5B50> /Parent 5 0 R /V (value) >>"),
        ],
        1,
    )
}

fn indirect_field_info_values_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (
                4,
                "<< /Fields [5 0 R] /DA 10 0 R /Q 11 0 R /MaxLen 12 0 R >>",
            ),
            (
                5,
                "<< /T 13 0 R /FT 14 0 R /DV 15 0 R /Ff 16 0 R /Kids [6 0 R] >>",
            ),
            (6, "<< /T 17 0 R /Parent 5 0 R /V 18 0 R /DA 19 0 R >>"),
            (10, "(/Doc 10 Tf 0 g)"),
            (11, "1"),
            (12, "20"),
            (13, "(parent)"),
            (14, "/Tx"),
            (15, "(parent-default)"),
            (16, "3"),
            (17, "(child)"),
            (18, "(child-value)"),
            (19, "(/Child 11 Tf 1 g)"),
        ],
        1,
    )
}

#[test]
fn fields_walks_acroform_field_tree() {
    let bytes = form_pdf();
    let mut pdf = Pdf::open_mem_owned(bytes).unwrap();

    let fields = pdf.acroform().unwrap().fields().unwrap();

    assert_eq!(fields, vec![ObjectRef::new(5, 0), ObjectRef::new(6, 0)]);
}

// A `/Fields` array carrier stored as a holder chain (`6 0 R → 7 0 R → [4 0 R]`)
// must still yield its top-level field. A one-hop carrier resolve returns the
// inner `Reference` (not an `Array`) and dropped every field; the chain resolve
// follows to the terminal array. Exercised through the public `fields()` entry,
// which routes the carrier through the same `resolve_array_value` as
// `top_level_fields`; field 4 is a leaf (`/FT /Tx`, no `/Kids`) so the walked
// result is the carrier's single top-level field.
#[test]
fn fields_follows_holder_chain_carrier() {
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 8 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /T (f1) /FT /Tx >>"),
            // Holder chain carrier: AcroForm /Fields 6 0 R -> 7 0 R -> [4 0 R].
            (6, "7 0 R"),
            (7, "[4 0 R]"),
            (8, "<< /Fields 6 0 R >>"),
        ],
        1,
    );
    let mut pdf = Pdf::open_mem_owned(bytes).unwrap();
    pdf.set_object(
        ObjectRef::new(6, 0),
        Object::Reference(ObjectRef::new(7, 0)),
    );
    let fields = pdf.acroform().unwrap().fields().unwrap();
    assert_eq!(fields, vec![ObjectRef::new(4, 0)]);
}

#[test]
fn field_infos_materialize_inherited_values_and_full_names() {
    let bytes = inherited_field_info_pdf();
    let mut pdf = Pdf::open_mem_owned(bytes).unwrap();

    let fields = pdf.acroform().unwrap().field_infos().unwrap();

    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].object_ref, ObjectRef::new(5, 0));
    assert_eq!(fields[0].partial_name, Some(b"parent".to_vec()));
    assert_eq!(fields[0].full_name, "parent");
    assert_eq!(fields[0].field_type, Some(b"Tx".to_vec()));
    assert_eq!(
        fields[0].default_value,
        Some(Object::String(b"parent-default".to_vec()))
    );
    assert_eq!(fields[0].field_flags, Some(3));
    assert_eq!(
        fields[0].default_appearance,
        Some(Object::String(b"/Doc 10 Tf 0 g".to_vec()))
    );
    assert_eq!(fields[0].quadding, Some(1));
    assert_eq!(fields[0].max_len, Some(20));

    assert_eq!(fields[1].object_ref, ObjectRef::new(6, 0));
    assert_eq!(fields[1].partial_name, Some(b"child".to_vec()));
    assert_eq!(fields[1].full_name, "parent.child");
    assert_eq!(fields[1].field_type, Some(b"Tx".to_vec()));
    assert_eq!(
        fields[1].value,
        Some(Object::String(b"child-value".to_vec()))
    );
    assert_eq!(
        fields[1].default_value,
        Some(Object::String(b"parent-default".to_vec()))
    );
    assert_eq!(fields[1].field_flags, Some(3));
    assert_eq!(
        fields[1].default_appearance,
        Some(Object::String(b"/Child 11 Tf 1 g".to_vec()))
    );
    assert_eq!(fields[1].quadding, Some(1));
    assert_eq!(fields[1].max_len, Some(20));
}

#[test]
fn field_infos_skip_pure_widget_kids_but_keep_merged_widget_fields() {
    let bytes = field_info_widget_kids_pdf();
    let mut pdf = Pdf::open_mem_owned(bytes).unwrap();

    let fields = pdf.acroform().unwrap().field_infos().unwrap();

    assert_eq!(
        fields
            .iter()
            .map(|field| field.object_ref)
            .collect::<Vec<_>>(),
        vec![
            ObjectRef::new(5, 0),
            ObjectRef::new(7, 0),
            ObjectRef::new(8, 0),
            ObjectRef::new(9, 0),
        ]
    );
    assert_eq!(fields[1].full_name, "field.merged");
    assert_eq!(fields[1].value, Some(Object::String(b"yes".to_vec())));
    assert_eq!(fields[2].full_name, "field");
    assert_eq!(fields[3].full_name, "field");
}

#[test]
fn field_infos_decode_utf16be_field_name_paths() {
    let bytes = unicode_field_names_pdf();
    let mut pdf = Pdf::open_mem_owned(bytes).unwrap();

    let fields = pdf.acroform().unwrap().field_infos().unwrap();

    assert_eq!(fields[0].partial_name, Some(vec![0xFE, 0xFF, 0x89, 0xAA]));
    assert_eq!(fields[0].full_name, "親");
    assert_eq!(fields[1].partial_name, Some(vec![0xFE, 0xFF, 0x5B, 0x50]));
    assert_eq!(fields[1].full_name, "親.子");
}

#[test]
fn field_infos_materialize_indirect_inherited_values() {
    let bytes = indirect_field_info_values_pdf();
    let mut pdf = Pdf::open_mem_owned(bytes).unwrap();

    let fields = pdf.acroform().unwrap().field_infos().unwrap();

    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].partial_name, Some(b"parent".to_vec()));
    assert_eq!(fields[0].full_name, "parent");
    assert_eq!(fields[0].field_type, Some(b"Tx".to_vec()));
    assert_eq!(fields[0].value, None);
    assert_eq!(
        fields[0].default_value,
        Some(Object::String(b"parent-default".to_vec()))
    );
    assert_eq!(fields[0].field_flags, Some(3));
    assert_eq!(
        fields[0].default_appearance,
        Some(Object::String(b"/Doc 10 Tf 0 g".to_vec()))
    );
    assert_eq!(fields[0].quadding, Some(1));
    assert_eq!(fields[0].max_len, Some(20));

    assert_eq!(fields[1].partial_name, Some(b"child".to_vec()));
    assert_eq!(fields[1].full_name, "parent.child");
    assert_eq!(fields[1].field_type, Some(b"Tx".to_vec()));
    assert_eq!(
        fields[1].value,
        Some(Object::String(b"child-value".to_vec()))
    );
    assert_eq!(
        fields[1].default_value,
        Some(Object::String(b"parent-default".to_vec()))
    );
    assert_eq!(
        fields[1].default_appearance,
        Some(Object::String(b"/Child 11 Tf 1 g".to_vec()))
    );
    assert_eq!(fields[1].quadding, Some(1));
    assert_eq!(fields[1].max_len, Some(20));
    assert_eq!(fields[1].field_flags, Some(3));
}

#[test]
fn missing_or_malformed_acroform_shapes_are_noops() {
    let empty_bytes = empty_pdf();
    let mut empty = Pdf::open_mem_owned(empty_bytes).unwrap();
    assert!(empty.acroform().unwrap().fields().unwrap().is_empty());

    let malformed_bytes = malformed_acroform_pdf();
    let mut malformed = Pdf::open_mem_owned(malformed_bytes).unwrap();
    assert!(malformed.acroform().unwrap().fields().unwrap().is_empty());
}

#[test]
fn malformed_fields_are_ignored_for_listing() {
    let bytes = malformed_fields_pdf();
    let mut pdf = Pdf::open_mem_owned(bytes).unwrap();

    assert!(pdf.acroform().unwrap().fields().unwrap().is_empty());
}

#[test]
fn missing_default_appearance_is_noop_but_fields_still_walk() {
    let bytes = no_default_appearance_pdf();
    let mut pdf = Pdf::open_mem_owned(bytes).unwrap();

    assert_eq!(
        pdf.acroform().unwrap().fields().unwrap(),
        vec![ObjectRef::new(5, 0)]
    );

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
        let mut pdf = Pdf::open_mem_owned(bytes).unwrap();

        assert!(pdf.acroform().unwrap().fields().unwrap().is_empty());
    }
}

#[test]
fn field_value_get_set_uses_live_document() {
    let bytes = form_pdf();
    let mut pdf = Pdf::open_mem_owned(bytes).unwrap();

    {
        let mut acroform = AcroFormDocumentHelper::new(&mut pdf).unwrap();
        assert_eq!(
            acroform
                .field_value(ObjectRef::new(6, 0))
                .unwrap()
                .and_then(|value| value.as_string()),
            Some(b"before".to_vec())
        );
        acroform
            .set_field_value(
                ObjectRef::new(6, 0),
                flpdf::ObjectHandle::string(b"after".to_vec()),
            )
            .unwrap();
    }

    let mut acroform = pdf.acroform().unwrap();
    assert_eq!(
        acroform
            .field_value(ObjectRef::new(6, 0))
            .unwrap()
            .and_then(|value| value.as_string()),
        Some(b"after".to_vec())
    );
}

#[test]
fn default_appearance_materializes_direct_catalog_acroform() {
    let bytes = direct_acroform_pdf();
    let mut pdf = Pdf::open_mem_owned(bytes).unwrap();
    let da = b"/Helv 12 Tf 0 g".to_vec();

    pdf.acroform()
        .unwrap()
        .set_default_appearance(da.clone())
        .unwrap();

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
fn default_appearance_is_read_as_inherited_without_materializing_fields() {
    let bytes = form_pdf();
    let mut pdf = Pdf::open_mem_owned(bytes).unwrap();
    let da = b"/F1 9 Tf 0 0 1 rg".to_vec();

    pdf.acroform()
        .unwrap()
        .set_default_appearance(da.clone())
        .unwrap();

    let acroform = pdf.resolve(ObjectRef::new(4, 0)).unwrap();
    let Object::Dictionary(acroform_dict) = acroform else {
        panic!("AcroForm should be a dictionary");
    };
    assert_eq!(acroform_dict.get("DA"), Some(&Object::String(da.clone())));

    let fields = pdf.acroform().unwrap().field_infos().unwrap();
    assert_eq!(fields[1].object_ref, ObjectRef::new(6, 0));
    assert_eq!(fields[1].default_appearance, Some(Object::String(da)));

    let child = pdf.resolve(ObjectRef::new(6, 0)).unwrap();
    let Object::Dictionary(child_dict) = child else {
        panic!("child field should be a dictionary");
    };
    assert!(
        child_dict.get("DA").is_none(),
        "qpdf resolves inherited /DA lazily and does not stamp same-document fields"
    );
}

#[test]
fn parent_field_appearance_is_read_as_inherited_without_materialization() {
    let bytes = parent_da_pdf();
    let mut pdf = Pdf::open_mem_owned(bytes).unwrap();

    let fields = pdf.acroform().unwrap().field_infos().unwrap();
    assert_eq!(fields[1].object_ref, ObjectRef::new(6, 0));
    assert_eq!(
        fields[1].default_appearance,
        Some(Object::String(b"/Parent 11 Tf 1 0 0 rg".to_vec()))
    );

    let child = pdf.resolve(ObjectRef::new(6, 0)).unwrap();
    let Object::Dictionary(child_dict) = child else {
        panic!("child field should be a dictionary");
    };
    assert!(child_dict.get("DA").is_none());
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
    let mut pdf = Pdf::open_mem_owned(bytes).unwrap();

    let err = pdf.acroform().unwrap().fields().unwrap_err();

    assert!(
        matches!(err, flpdf::Error::Unsupported(_)),
        "expected depth-limit Unsupported error, got {err:?}"
    );
}

#[test]
fn need_appearances_reads_boolean_values_and_ignores_other_types() {
    let true_pdf = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R >>"),
            (4, "<< /Fields [] /NeedAppearances true >>"),
        ],
        1,
    );
    let false_pdf = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R >>"),
            (4, "<< /Fields [] /NeedAppearances (true) >>"),
        ],
        1,
    );

    let mut true_pdf = Pdf::open_mem_owned(true_pdf).unwrap();
    let mut false_pdf = Pdf::open_mem_owned(false_pdf).unwrap();
    assert!(true_pdf.acroform().unwrap().has_acro_form().unwrap());
    assert!(true_pdf.acroform().unwrap().get_need_appearances().unwrap());
    assert!(!false_pdf
        .acroform()
        .unwrap()
        .get_need_appearances()
        .unwrap());
}

#[test]
fn has_acro_form_reports_present_non_dictionary_entries() {
    let mut malformed = Pdf::open_mem_owned(malformed_acroform_pdf()).unwrap();
    let mut absent = Pdf::open_mem_owned(empty_pdf()).unwrap();

    assert!(malformed.acroform().unwrap().has_acro_form().unwrap());
    assert!(!absent.acroform().unwrap().has_acro_form().unwrap());
}

#[test]
fn set_need_appearances_replaces_true_and_removes_false() {
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R >>"),
            (4, "<< /Fields [] /NeedAppearances false >>"),
        ],
        1,
    );
    let mut pdf = Pdf::open_mem_owned(bytes).unwrap();

    pdf.acroform().unwrap().set_need_appearances(true).unwrap();
    assert!(pdf.acroform().unwrap().get_need_appearances().unwrap());

    pdf.acroform().unwrap().set_need_appearances(false).unwrap();
    assert!(!pdf.acroform().unwrap().get_need_appearances().unwrap());
    let acroform = pdf.resolve(ObjectRef::new(4, 0)).unwrap();
    assert_eq!(acroform.as_dict().unwrap().get("NeedAppearances"), None);

    // qpdf's removeKey is also a no-op when the key is already absent.
    pdf.acroform().unwrap().set_need_appearances(false).unwrap();
}

#[test]
fn generate_appearances_if_needed_updates_widgets_and_clears_marker() {
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [6 0 R] >>",
            ),
            (
                4,
                "<< /Fields [5 0 R] /NeedAppearances true /DA (/Helv 10 Tf 0 g) /DR << /Font << /Helv 7 0 R >> >> >>",
            ),
            (5, "<< /FT /Tx /V (value) /Kids [6 0 R] >>"),
            (
                6,
                "<< /Subtype /Widget /Parent 5 0 R /Rect [0 0 100 20] >>",
            ),
            (7, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
        ],
        1,
    );
    let mut pdf = Pdf::open_mem_owned(bytes).unwrap();

    pdf.acroform()
        .unwrap()
        .generate_appearances_if_needed()
        .unwrap();

    assert!(!pdf.acroform().unwrap().get_need_appearances().unwrap());
    let widget = pdf.resolve(ObjectRef::new(6, 0)).unwrap();
    let normal = widget
        .as_dict()
        .and_then(|dict| dict.get("AP"))
        .and_then(Object::as_dict)
        .and_then(|dict| dict.get("N"))
        .and_then(Object::as_ref_id)
        .expect("generated widget normal appearance");
    assert!(pdf.resolve(normal).unwrap().as_stream().is_some());
}

#[test]
fn generate_appearances_if_needed_synchronizes_checkbox_value() {
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>",
            ),
            (4, "<< /Fields [5 0 R] /NeedAppearances true >>"),
            (
                5,
                "<< /Subtype /Widget /FT /Btn /Ff 0 /V /On /AS /Off /Rect [0 0 20 20] /P 3 0 R >>",
            ),
        ],
        1,
    );
    let mut pdf = Pdf::open_mem_owned(bytes).unwrap();

    pdf.acroform()
        .unwrap()
        .generate_appearances_if_needed()
        .unwrap();

    assert!(!pdf.acroform().unwrap().get_need_appearances().unwrap());
    let widget = pdf.resolve(ObjectRef::new(5, 0)).unwrap();
    let widget = widget.as_dict().unwrap();
    assert!(matches!(widget.get("V"), Some(Object::Name(name)) if name == b"Yes"));
    assert!(matches!(widget.get("AS"), Some(Object::Name(name)) if name == b"Off"));
}

#[test]
fn generate_appearances_if_needed_handles_a_direct_orphan_widget() {
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [<< /Subtype /Widget /FT /Tx /V (direct) /Rect [0 0 100 20] /DA (/Helv 10 Tf 0 g) >>] >>",
            ),
            (
                4,
                "<< /Fields [] /NeedAppearances true /DA (/Helv 10 Tf 0 g) /DR << /Font << /Helv 7 0 R >> >> >>",
            ),
            (7, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
        ],
        1,
    );
    let mut pdf = Pdf::open_mem_owned(bytes).unwrap();

    pdf.acroform()
        .unwrap()
        .generate_appearances_if_needed()
        .unwrap();

    let widget = {
        let mut page = flpdf::PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
        page.get_annotation_handles(Some(b"/Widget"))
            .unwrap()
            .pop()
            .expect("direct orphan widget")
    };
    let widget = pdf.resolve_object_handle_to_terminal(&widget).unwrap();
    let ap = pdf
        .resolve_object_handle_to_terminal(&widget.get_key(b"/AP"))
        .unwrap();
    assert!(ap.as_dictionary().is_some());
    let normal = pdf
        .resolve_object_handle_to_terminal(&ap.get_key(b"/N"))
        .unwrap();
    assert!(normal.as_stream_dict().is_some());
}

#[test]
fn direct_non_indirect_pages_does_not_fail_eager_analyze() {
    // A catalog that embeds /Pages as a direct (non-indirect) dictionary is
    // malformed-but-readable: qpdf's getAllPages tolerates it, but flpdf's
    // public page_refs requires /Pages to be an indirect reference
    // (PageWalk::with_max_depth). Because analyze() now runs eagerly in the
    // constructor, that mismatch used to fail the entire helper -- not just
    // the orphan-widget fallback that triggers it -- for a document real
    // qpdf 11.9.0 opens cleanly (verified live).
    let bytes = build_pdf(
        &[(
            1,
            "<< /Type /Catalog /Pages << /Type /Pages /Kids [] /Count 0 >> /AcroForm << /Fields [] >> >>",
        )],
        1,
    );
    let mut pdf = Pdf::open_mem_owned(bytes).unwrap();

    let mut acroform = pdf
        .acroform()
        .expect("direct /Pages must not fail AcroFormDocumentHelper construction");
    assert!(acroform.fields().unwrap().is_empty());
}

#[test]
fn other_page_refs_errors_still_propagate_from_analyze() {
    // Only page_refs()'s Error::Missing("/Pages") is swallowed by the
    // fallback above (a malformed-but-qpdf-tolerated direct /Pages shape).
    // Any other page_refs() failure -- here, an indirect /Pages chain
    // deeper than DEFAULT_MAX_PAGE_TREE_DEPTH -- must still fail analyze(),
    // matching qpdf's own page-tree depth guard.
    const FIRST_PAGES_NODE: u32 = 2;
    let last_pages_node = FIRST_PAGES_NODE + flpdf::pages::DEFAULT_MAX_PAGE_TREE_DEPTH as u32 + 1;

    let mut objects = vec![(
        1u32,
        format!("<< /Type /Catalog /Pages {FIRST_PAGES_NODE} 0 R /AcroForm << /Fields [] >> >>"),
    )];
    for node in FIRST_PAGES_NODE..last_pages_node {
        objects.push((node, format!("<< /Type /Pages /Kids [{} 0 R] >>", node + 1)));
    }
    objects.push((last_pages_node, "<< /Type /Pages /Kids [] >>".to_string()));

    let object_refs: Vec<(u32, &str)> = objects
        .iter()
        .map(|(n, body)| (*n, body.as_str()))
        .collect();
    let bytes = build_pdf(&object_refs, 1);
    let mut pdf = Pdf::open_mem_owned(bytes).unwrap();

    let error = match pdf.acroform() {
        Ok(_) => {
            panic!("a page-tree depth overflow from page_refs() must still fail eager analyze()")
        }
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("depth exceeds"),
        "unexpected error: {error}"
    );
}
