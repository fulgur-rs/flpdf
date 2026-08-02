//! Integration coverage for the public qpdf-shaped form-field helper.

use flpdf::form_field_object_helper::FormFieldObjectHelper;
use flpdf::{Object, ObjectRef, Pdf};
use std::io::Cursor;

mod common;
use common::build_pdf;

fn open(bytes: Vec<u8>) -> Pdf<Cursor<Vec<u8>>> {
    Pdf::open(Cursor::new(bytes)).expect("PDF should parse")
}

fn doc(mut objects: Vec<(u32, String)>) -> Vec<u8> {
    let mut base = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()),
        (3, "<< /Type /Page /Parent 2 0 R >>".to_string()),
    ];
    base.append(&mut objects);
    build_pdf(&base, 1)
}

fn doc_with_acroform(mut objects: Vec<(u32, String)>) -> Vec<u8> {
    let mut base = vec![
        (
            1,
            "<< /Type /Catalog /Pages 2 0 R /AcroForm 20 0 R >>".to_string(),
        ),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()),
        (3, "<< /Type /Page /Parent 2 0 R >>".to_string()),
    ];
    base.append(&mut objects);
    build_pdf(&base, 1)
}

#[test]
fn exposes_qpdf_form_field_helper_from_its_own_module() {
    let _ = std::any::type_name::<FormFieldObjectHelper<'static, Cursor<Vec<u8>>>>();
}

#[test]
fn reads_indirect_field_attributes_and_names() {
    // qpdf's getKey dereferences leaf objects for the field type, value, and
    // all three field-name accessors.
    let bytes = doc(vec![
        (
            10,
            "<< /FT 20 0 R /V 21 0 R /DV 22 0 R /Ff 23 0 R /T 24 0 R /TU 25 0 R /TM 26 0 R >>"
                .into(),
        ),
        (20, "/Tx".into()),
        (21, "(current)".into()),
        (22, "(default)".into()),
        (23, "4097".into()),
        (24, "(partial)".into()),
        (25, "(alternative)".into()),
        (26, "(mapping)".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);

    assert_eq!(field.field_type().unwrap(), Some(b"/Tx".to_vec()));
    assert_eq!(
        field.field_value().unwrap(),
        Some(Object::String(b"current".to_vec()))
    );
    assert_eq!(
        field.field_default_value().unwrap(),
        Some(Object::String(b"default".to_vec()))
    );
    assert_eq!(field.field_flags().unwrap(), Some(4097));
    assert_eq!(field.partial_name().unwrap(), Some(b"partial".to_vec()));
    assert_eq!(
        field.alternative_name().unwrap(),
        Some(b"alternative".to_vec())
    );
    assert_eq!(field.mapping_name().unwrap(), Some(b"mapping".to_vec()));
}

#[test]
fn qualifies_names_from_the_parent_chain() {
    let bytes = doc(vec![
        (10, "<< /T (child) /Parent 11 0 R >>".into()),
        (11, "<< /T (group) /Parent 12 0 R >>".into()),
        (12, "<< /T (top) >>".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(
        field.fully_qualified_name().unwrap(),
        Some(b"top.group.child".to_vec())
    );
}

#[test]
fn mapping_name_falls_back_to_alternative_then_qualified_name() {
    let bytes = doc(vec![
        (10, "<< /T (child) /Parent 11 0 R >>".into()),
        (11, "<< /T (parent) >>".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(
        field.mapping_name().unwrap(),
        Some(b"parent.child".to_vec())
    );
    drop(field);

    let bytes = doc(vec![(10, "<< /T (child) /TU (alt) >>".into())]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(field.mapping_name().unwrap(), Some(b"alt".to_vec()));
}

#[test]
fn name_walkers_terminate_on_parent_cycles() {
    let bytes = doc(vec![
        (10, "<< /T (child) /Parent 11 0 R >>".into()),
        (11, "<< /T (parent) /Parent 10 0 R >>".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(
        field.fully_qualified_name().unwrap(),
        Some(b"parent.child".to_vec())
    );
}

#[test]
fn non_dictionary_field_has_no_readable_attributes() {
    let bytes = doc(vec![(10, "42".into())]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(field.field_type().unwrap(), None);
    assert_eq!(field.field_value().unwrap(), None);
    assert_eq!(field.field_default_value().unwrap(), None);
    assert_eq!(field.field_flags().unwrap(), None);
    assert_eq!(field.fully_qualified_name().unwrap(), None);
    assert_eq!(field.alternative_name().unwrap(), None);
    assert_eq!(field.mapping_name().unwrap(), None);
}

#[test]
fn field_type_wrong_type_on_child_stops_parent_inheritance() {
    // qpdf's getInheritableFieldValue stops at a present, non-null `/FT` even
    // when getFieldType then rejects the value for not being a name.
    let bytes = doc(vec![
        (10, "<< /FT 42 /Parent 11 0 R >>".into()),
        (11, "<< /FT /Tx >>".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(field.field_type().unwrap(), None);
}

#[test]
fn field_flags_wrong_type_on_child_stops_parent_inheritance_with_zero() {
    // qpdf's getFlags converts a non-integer inheritable `/Ff` to zero rather
    // than consulting an ancestor's integer flag value.
    let bytes = doc(vec![
        (10, "<< /Ff /Nope /Parent 11 0 R >>".into()),
        (11, "<< /Ff 1 >>".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(field.field_flags().unwrap(), Some(0));
}

#[test]
fn classifies_qpdf_field_types_from_inherited_type_and_flags() {
    let cases = [
        ("/Tx", 0, true, false, false, false, false),
        ("/Btn", 0, false, true, false, false, false),
        ("/Btn", 1 << 15, false, false, true, false, false),
        ("/Btn", 1 << 16, false, false, false, true, false),
        ("/Ch", 0, false, false, false, false, true),
    ];

    for (field_type, flags, text, checkbox, radio, pushbutton, choice) in cases {
        let bytes = doc(vec![
            (10, format!("<< /Parent 11 0 R >>")),
            (11, format!("<< /FT {field_type} /Ff {flags} >>")),
        ]);
        let mut pdf = open(bytes);
        let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);

        assert_eq!(field.is_text().unwrap(), text, "{field_type} /Ff {flags}");
        assert_eq!(
            field.is_checkbox().unwrap(),
            checkbox,
            "{field_type} /Ff {flags}"
        );
        assert_eq!(
            field.is_radio_button().unwrap(),
            radio,
            "{field_type} /Ff {flags}"
        );
        assert_eq!(
            field.is_pushbutton().unwrap(),
            pushbutton,
            "{field_type} /Ff {flags}"
        );
        assert_eq!(
            field.is_choice().unwrap(),
            choice,
            "{field_type} /Ff {flags}"
        );
    }
}

#[test]
fn choices_returns_only_string_options_from_an_indirect_inherited_array() {
    // qpdf's getChoices() (`QPDFFormFieldObjectHelper.cc:268-285`) accepts
    // only string array items. A two-string export/display pair is ignored.
    let bytes = doc(vec![
        (10, "<< /FT /Ch /Parent 11 0 R >>".into()),
        (11, "<< /Opt 12 0 R >>".into()),
        (12, "[(one) [(export) (display)] 42 (two)]".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(field.choices().unwrap(), vec!["one", "two"]);

    let bytes = doc(vec![
        (10, "<< /FT /Ch /Opt [12 0 R (direct)] >>".into()),
        (12, "(indirect)".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(field.choices().unwrap(), vec!["indirect", "direct"]);

    let bytes = doc(vec![(10, "<< /FT /Tx /Opt [(one)] >>".into())]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert!(field.choices().unwrap().is_empty());
}

#[test]
fn reads_metadata_from_field_inheritance_then_acroform() {
    let bytes = doc_with_acroform(vec![
        (10, "<< /Parent 11 0 R >>".into()),
        (11, "<< /DA 12 0 R /Q 2 >>".into()),
        (20, "<< /DR 21 0 R /DA 22 0 R /Q 1 >>".into()),
        (12, "(/Helv 9 Tf 0 g)".into()),
        (21, "<< /Font << /Helv 23 0 R >> >>".into()),
        (22, "(/Helv 8 Tf 0 g)".into()),
        (
            23,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        ),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);

    assert_eq!(field.default_appearance().unwrap(), "/Helv 9 Tf 0 g");
    assert_eq!(field.quadding().unwrap(), 2);
    assert!(matches!(
        field.default_resources().unwrap(),
        Some(Object::Dictionary(_))
    ));

    let bytes = doc_with_acroform(vec![
        (10, "<< >>".into()),
        (20, "<< /DA 22 0 R /Q 1 >>".into()),
        (22, "(/Helv 8 Tf 0 g)".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(field.default_appearance().unwrap(), "/Helv 8 Tf 0 g");
    assert_eq!(field.quadding().unwrap(), 1);
    assert_eq!(field.default_resources().unwrap(), None);

    let bytes = doc_with_acroform(vec![
        (10, "<< /Parent 11 0 R /DA /Wrong /Q /Wrong >>".into()),
        (11, "<< /DA (ignored) /Q 2 >>".into()),
        (20, "<< /DA (fallback) /Q 1 >>".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(field.default_appearance().unwrap(), "fallback");
    assert_eq!(field.quadding().unwrap(), 1);
}

#[test]
fn checked_requires_checkbox_and_an_on_name_value() {
    let cases = [
        ("/Btn", 0, "/On", true),
        ("/Btn", 0, "/Off", false),
        ("/Btn", 1 << 15, "/On", false),
        ("/Btn", 0, "(not-a-name)", false),
    ];
    for (field_type, flags, value, expected) in cases {
        let bytes = doc(vec![
            (10, format!("<< /FT {field_type} /Ff {flags} /V 11 0 R >>")),
            (11, value.into()),
        ]);
        let mut pdf = open(bytes);
        let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
        assert_eq!(field.is_checked().unwrap(), expected);
    }
}

#[test]
fn checked_inherits_parent_value_and_honors_child_override() {
    let bytes = doc(vec![
        (10, "<< /Parent 11 0 R >>".into()),
        (11, "<< /FT /Btn /Ff 0 /V /On >>".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert!(field.is_checked().unwrap());

    let bytes = doc(vec![
        (10, "<< /Parent 11 0 R /V /Off >>".into()),
        (11, "<< /FT /Btn /Ff 0 /V /On >>".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert!(!field.is_checked().unwrap());
}

#[test]
fn exposes_remaining_qpdf_read_and_traversal_accessors() {
    let bytes = doc(vec![
        (10, "<< /Parent 11 0 R /V 13 0 R /DV 14 0 R >>".into()),
        (
            11,
            "<< /Parent 12 0 R /CustomString 15 0 R /CustomName 16 0 R >>".into(),
        ),
        (12, "<< >>".into()),
        (13, "(current)".into()),
        (14, "(default)".into()),
        (15, "(inherited)".into()),
        (16, "/InheritedName".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);

    assert!(!field.is_null().unwrap());
    assert_eq!(field.parent().unwrap(), Some(ObjectRef::new(11, 0)));
    assert_eq!(
        field.top_level_field().unwrap(),
        (ObjectRef::new(12, 0), true)
    );
    assert_eq!(
        field.inheritable_value(b"CustomString").unwrap(),
        Some(Object::String(b"inherited".to_vec()))
    );
    assert_eq!(
        field.inheritable_string(b"CustomString").unwrap(),
        "inherited"
    );
    assert_eq!(
        field.inheritable_name(b"CustomName").unwrap(),
        b"/InheritedName"
    );
    assert_eq!(
        field.value().unwrap(),
        Some(Object::String(b"current".to_vec()))
    );
    assert_eq!(field.value_as_string().unwrap(), "current");
    assert_eq!(
        field.default_value().unwrap(),
        Some(Object::String(b"default".to_vec()))
    );
    assert_eq!(field.default_value_as_string().unwrap(), "default");
    assert_eq!(field.flags().unwrap(), 0);

    let bytes = doc(vec![(10, "null".into())]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert!(field.is_null().unwrap());
}
