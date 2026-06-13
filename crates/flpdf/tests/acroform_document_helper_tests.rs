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

fn form_dr_p_font_pdf() -> Vec<u8> {
    // A /DR/Font resource is legitimately named /P (the same key the field-tree
    // walk skips as a widget's /P page back-pointer). Font 6 is reachable ONLY
    // through /DR — no field references it — so the copy walk must traverse the
    // resource dict without applying the /P skip, or the font is dropped.
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (
                4,
                "<< /Fields [5 0 R] /DA (/P 10 Tf 0 g) /DR << /Font << /P 6 0 R >> >> >>",
            ),
            (5, "<< /T (field) /FT /Tx /V (val) >>"),
            (6, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
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
    let mut pdf = Pdf::open_mem(&bytes).unwrap();

    let fields = pdf.acroform().fields().unwrap();

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
    let fields = pdf.acroform().fields().unwrap();
    assert_eq!(fields, vec![ObjectRef::new(4, 0)]);
}

#[test]
fn field_infos_materialize_inherited_values_and_full_names() {
    let bytes = inherited_field_info_pdf();
    let mut pdf = Pdf::open_mem(&bytes).unwrap();

    let fields = pdf.acroform().field_infos().unwrap();

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
    let mut pdf = Pdf::open_mem(&bytes).unwrap();

    let fields = pdf.acroform().field_infos().unwrap();

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
    let mut pdf = Pdf::open_mem(&bytes).unwrap();

    let fields = pdf.acroform().field_infos().unwrap();

    assert_eq!(fields[0].partial_name, Some(vec![0xFE, 0xFF, 0x89, 0xAA]));
    assert_eq!(fields[0].full_name, "親");
    assert_eq!(fields[1].partial_name, Some(vec![0xFE, 0xFF, 0x5B, 0x50]));
    assert_eq!(fields[1].full_name, "親.子");
}

#[test]
fn field_infos_materialize_indirect_inherited_values() {
    let bytes = indirect_field_info_values_pdf();
    let mut pdf = Pdf::open_mem(&bytes).unwrap();

    let fields = pdf.acroform().field_infos().unwrap();

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
fn copy_fields_from_errors_when_reference_chain_depth_limit_is_exceeded() {
    // Non-cyclic chain reachable from AcroForm /Fields via an arbitrary key (/X),
    // long enough to exceed DEFAULT_MAX_ACROFORM_DEPTH. The chain is acyclic so the
    // `seen` cycle guard never fires; only the new depth limit can stop the recursion.
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
        (5, "<< /T (parent) /FT /Tx /X 6 0 R >>".to_string()),
    ];
    for object_number in 6..=106 {
        let next = object_number + 1;
        objects.push((object_number, format!("<< /X {next} 0 R >>")));
    }
    objects.push((107, "<< /Leaf true >>".to_string()));
    let borrowed: Vec<(u32, &str)> = objects
        .iter()
        .map(|(object_number, body)| (*object_number, body.as_str()))
        .collect();
    let source_bytes = build_pdf(&borrowed, 1);
    let target_bytes = empty_pdf();
    let mut source = Pdf::open_mem(&source_bytes).unwrap();
    let mut target = Pdf::open_mem(&target_bytes).unwrap();

    let err = target.acroform().copy_fields_from(&mut source).unwrap_err();

    assert!(
        matches!(err, flpdf::Error::Unsupported(_)),
        "expected depth-limit Unsupported error, got {err:?}"
    );
}

#[test]
fn copy_fields_from_copies_field_appearance_stream() {
    // A field reaches an appearance stream (/AP /N -> stream) through a non-/P key, so the
    // reachable-ref walk must descend into the stream object (the `Object::Stream` arm). The
    // stream must be carried into the target and stay referenced by the copied field.
    let source_bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Fields [5 0 R] >>"),
            (5, "<< /T (widget) /FT /Tx /AP << /N 6 0 R >> >>"),
            (
                6,
                "<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] /Length 5 >>\nstream\nHELLO\nendstream",
            ),
        ],
        1,
    );
    let target_bytes = empty_pdf();
    let mut source = Pdf::open_mem(&source_bytes).unwrap();
    let mut target = Pdf::open_mem(&target_bytes).unwrap();

    let copied = target.acroform().copy_fields_from(&mut source).unwrap();
    assert_eq!(copied.len(), 1, "the single top-level field is copied");

    let Object::Dictionary(field) = target.resolve(copied[0]).unwrap() else {
        panic!("copied field should be a dictionary");
    };
    let Some(Object::Dictionary(ap)) = field.get("AP").cloned() else {
        panic!("copied field should retain its /AP dictionary");
    };
    let normal_ref = ap
        .get_ref("N")
        .expect("/AP /N should reference the copied appearance stream");
    assert!(
        matches!(target.resolve(normal_ref).unwrap(), Object::Stream(_)),
        "the appearance stream should be copied into the target as a stream object"
    );
}

#[test]
fn copy_fields_from_skips_field_p_reference() {
    // The reachable-ref walk skips the /P key so it never pulls a field's page (and the
    // sibling page tree) into the copy set. An object reachable *only* via /P is therefore
    // excluded; copy_objects rewrites the dangling ref to Null, proving the skip fired.
    let source_bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Fields [5 0 R] >>"),
            (5, "<< /T (widget) /FT /Tx /V (val) /P 6 0 R >>"),
            (6, "<< /Type /Page /Marker (must-not-be-copied) >>"),
        ],
        1,
    );
    let target_bytes = empty_pdf();
    let mut source = Pdf::open_mem(&source_bytes).unwrap();
    let mut target = Pdf::open_mem(&target_bytes).unwrap();

    let copied = target.acroform().copy_fields_from(&mut source).unwrap();
    assert_eq!(copied.len(), 1, "the single top-level field is copied");

    let Object::Dictionary(field) = target.resolve(copied[0]).unwrap() else {
        panic!("copied field should be a dictionary");
    };
    assert_eq!(
        field.get("P"),
        Some(&Object::Null),
        "the /P-only page must be skipped during collection and left dangling (Null)"
    );
    assert_eq!(
        field.get("V"),
        Some(&Object::String(b"val".to_vec())),
        "non-/P entries are still copied"
    );
}

#[test]
fn copy_fields_from_skips_nested_annotation_page_back_pointer() {
    // A copied widget links a nested annotation (/Popup) whose own /P is still a
    // page back-pointer. The /P skip must stay active through the annotation graph
    // — it is lifted only when the walk crosses into resource data (/Resources) —
    // so the popup's page (reachable only via /Popup -> /P) is not pulled into the
    // copy set. Regression for flpdf-4ue7 (codex review): the skip must not turn
    // off for every non-field key, only for /Resources.
    let source_bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Fields [5 0 R] >>"),
            (5, "<< /T (widget) /FT /Tx /V (val) /Popup 7 0 R >>"),
            (
                7,
                "<< /Type /Annot /Subtype /Popup /Parent 5 0 R /P 8 0 R >>",
            ),
            (8, "<< /Type /Page /Marker (must-not-be-copied) >>"),
        ],
        1,
    );
    let target_bytes = empty_pdf();
    let mut source = Pdf::open_mem(&source_bytes).unwrap();
    let mut target = Pdf::open_mem(&target_bytes).unwrap();

    let copied = target.acroform().copy_fields_from(&mut source).unwrap();
    assert_eq!(copied.len(), 1, "the single top-level field is copied");

    let Object::Dictionary(field) = target.resolve(copied[0]).unwrap() else {
        panic!("copied field should be a dictionary");
    };
    // The /Popup annotation itself is reachable via a non-/P key, so it is copied.
    let popup_ref = field
        .get_ref("Popup")
        .expect("copied field should retain its /Popup annotation reference");
    let Object::Dictionary(popup) = target.resolve(popup_ref).unwrap() else {
        panic!("copied /Popup should be a dictionary");
    };
    assert_eq!(
        popup.get("P"),
        Some(&Object::Null),
        "the nested annotation's /P page back-pointer must be skipped and left dangling (Null)"
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
fn copy_fields_from_preserves_dr_resource_named_p() {
    // Regression for flpdf-4ue7: a /DR/Font resource named /P (reachable only via
    // the inherited resource dict) must survive the field copy. The field-tree /P
    // back-pointer skip must not apply when collecting the /DR closure, or the font
    // is dropped from the copy set and remapped to Null.
    let source_bytes = form_dr_p_font_pdf();
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
    let Object::Dictionary(acroform_dict) = target.resolve(acroform_ref).unwrap() else {
        panic!("AcroForm should be a dictionary");
    };
    let Object::Dictionary(dr) = acroform_dict.get("DR").expect("copied /DR") else {
        panic!("/DR should be a direct dictionary");
    };
    let Object::Dictionary(fonts) = dr.get("Font").expect("/DR/Font") else {
        panic!("/DR/Font should be a dictionary");
    };
    let Object::Reference(font_ref) = fonts
        .get("P")
        .expect("/DR/Font/P must survive the copy, not be dropped")
    else {
        panic!("/DR/Font/P should be a live reference, not Null (the /P font was dropped)");
    };
    let Object::Dictionary(font_dict) = target.resolve(*font_ref).unwrap() else {
        panic!("copied /P font should resolve to a font dictionary");
    };
    assert_eq!(
        font_dict.get("BaseFont"),
        Some(&Object::Name(b"Helvetica".to_vec())),
        "the /P-named font must survive the field copy with its data intact"
    );
}

#[test]
fn copy_fields_from_preserves_p_named_resource_in_field_appearance_stream() {
    // A field's appearance stream (/AP /N) carries its own /Resources with a font
    // named /P. /P is a page back-pointer only at the field/widget level, not inside
    // an appearance stream's resources, so the /P skip must stop propagating once the
    // walk leaves the field tree via /AP. Font 7 is reachable ONLY through the
    // appearance stream's /Resources. Regression for flpdf-4ue7 (gemini review).
    let source_bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Fields [5 0 R] >>"),
            (5, "<< /T (widget) /FT /Tx /AP << /N 6 0 R >> >>"),
            (
                6,
                "<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] /Resources << /Font << /P 7 0 R >> >> /Length 5 >>\nstream\nHELLO\nendstream",
            ),
            (7, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
        ],
        1,
    );
    let target_bytes = empty_pdf();
    let mut source = Pdf::open_mem(&source_bytes).unwrap();
    let mut target = Pdf::open_mem(&target_bytes).unwrap();

    let copied = target.acroform().copy_fields_from(&mut source).unwrap();
    assert_eq!(copied.len(), 1, "the single top-level field is copied");

    let Object::Dictionary(field) = target.resolve(copied[0]).unwrap() else {
        panic!("copied field should be a dictionary");
    };
    let Some(Object::Dictionary(ap)) = field.get("AP").cloned() else {
        panic!("copied field should retain its /AP dictionary");
    };
    let normal_ref = ap
        .get_ref("N")
        .expect("/AP /N should reference the copied appearance stream");
    let Object::Stream(stream) = target.resolve(normal_ref).unwrap() else {
        panic!("appearance stream should be copied as a stream object");
    };
    let Some(Object::Dictionary(resources)) = stream.dict.get("Resources").cloned() else {
        panic!("appearance stream should retain its /Resources dictionary");
    };
    let Object::Dictionary(fonts) = resources.get("Font").expect("/Resources/Font") else {
        panic!("/Resources/Font should be a dictionary");
    };
    let Object::Reference(font_ref) = fonts
        .get("P")
        .expect("/Resources/Font/P must survive the copy, not be dropped")
    else {
        panic!("/Resources/Font/P should be a live reference, not Null (the /P font was dropped)");
    };
    let Object::Dictionary(font_dict) = target.resolve(*font_ref).unwrap() else {
        panic!("copied /P appearance-resource font should resolve to a font dictionary");
    };
    assert_eq!(
        font_dict.get("BaseFont"),
        Some(&Object::Name(b"Helvetica".to_vec())),
        "the /P-named appearance-stream font must survive the field copy"
    );
}

#[test]
fn copy_fields_from_preserves_p_named_resource_in_shared_dr_dictionary() {
    // The AcroForm /DR and a field's appearance-stream /Resources reference the SAME
    // indirect object (8 0 R). The field-tree walk reaches 8 first via /AP; if the /P
    // skip still applied there, 8 would be marked seen with its /P font skipped, and
    // the later /DR walk — sharing the same `seen` — would not re-collect it, leaving
    // /DR/Font/P dropped to Null. Regression for flpdf-4ue7 (codex review): the shared
    // resource must be walked without the /P skip on its first (appearance) visit.
    let source_bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Fields [5 0 R] /DA (/P 10 Tf 0 g) /DR 8 0 R >>"),
            (5, "<< /T (widget) /FT /Tx /AP << /N 6 0 R >> >>"),
            (
                6,
                "<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] /Resources 8 0 R /Length 5 >>\nstream\nHELLO\nendstream",
            ),
            (8, "<< /Font << /P 9 0 R >> >>"),
            (9, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
        ],
        1,
    );
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
    let Object::Dictionary(acroform_dict) = target.resolve(acroform_ref).unwrap() else {
        panic!("AcroForm should be a dictionary");
    };
    let Object::Dictionary(dr) = acroform_dict.get("DR").expect("copied /DR") else {
        panic!("/DR should be a direct dictionary");
    };
    let Object::Dictionary(fonts) = dr.get("Font").expect("/DR/Font") else {
        panic!("/DR/Font should be a dictionary");
    };
    let Object::Reference(font_ref) = fonts
        .get("P")
        .expect("/DR/Font/P must survive even when /DR is shared with an appearance stream")
    else {
        panic!("/DR/Font/P should be a live reference, not Null (shared-resource /P was dropped)");
    };
    let Object::Dictionary(font_dict) = target.resolve(*font_ref).unwrap() else {
        panic!("copied shared /P font should resolve to a font dictionary");
    };
    assert_eq!(
        font_dict.get("BaseFont"),
        Some(&Object::Name(b"Helvetica".to_vec())),
        "the /P-named font in the shared /DR dictionary must survive the field copy"
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
