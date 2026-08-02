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
fn decodes_pdf_text_strings_in_field_names() {
    // qpdf's getUTF8Value() decodes PDFDocEncoding and UTF-16BE before
    // assembling the public field-name strings.
    let bytes = doc(vec![(
        10,
        "<< /T <FEFF006300680069006C0064> /TU <FEFF30E630FC30B630FC> /TM <FEFF30DE30C330D7> >>"
            .into(),
    )]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(field.partial_name().unwrap(), Some("child".into()));
    assert_eq!(field.alternative_name().unwrap(), Some("ユーザー".into()));
    assert_eq!(field.mapping_name().unwrap(), Some("マップ".into()));
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
fn mutating_a_non_dictionary_field_is_a_qpdf_style_no_op() {
    // QPDFObjectHandle::replaceKey warns and returns when the target is not a
    // dictionary; public field mutation follows the same no-op boundary.
    let bytes = doc(vec![(10, "42".into())]);
    let mut pdf = open(bytes);
    let before = pdf.resolve(ObjectRef::new(10, 0)).expect("field");

    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value_string("value", true)
        .expect("non-dictionary field mutation is ignored");

    assert_eq!(pdf.resolve(ObjectRef::new(10, 0)).expect("field"), before);
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
fn field_type_follows_multi_hop_reference_holders_before_testing_its_type() {
    // qpdf's `getKey` dereferences an indirect holder chain before
    // `getFieldType` decides whether the inherited value is a name.
    let bytes = doc(vec![
        (10, "<< /FT 20 0 R /Parent 11 0 R >>".into()),
        (11, "<< /FT /Ch >>".into()),
        (20, "null".into()),
        (21, "/Tx".into()),
    ]);
    let mut pdf = open(bytes);
    pdf.set_object(
        ObjectRef::new(20, 0),
        Object::Reference(ObjectRef::new(21, 0)),
    );
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);

    assert_eq!(field.field_type().unwrap(), Some(b"/Tx".to_vec()));
}

#[test]
fn value_reference_skips_a_child_reference_that_resolves_to_null() {
    let bytes = doc(vec![
        (10, "<< /V 20 0 R /Parent 11 0 R >>".into()),
        (11, "<< /V 21 0 R >>".into()),
        (20, "null".into()),
        (21, "<< /ByteRange [0 1 2 3] >>".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(
        field.field_value_reference().unwrap(),
        Some(ObjectRef::new(21, 0))
    );
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
            (10, "<< /Parent 11 0 R >>".to_string()),
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

#[test]
fn set_field_attribute_string_writes_a_qpdf_unicode_string_on_the_field() {
    let bytes = doc(vec![
        (10, "<< /Parent 11 0 R >>".into()),
        (11, "<< >>".into()),
    ]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_field_attribute_string(b"TU", "日本語")
        .expect("attribute set");

    let field = pdf.resolve(ObjectRef::new(10, 0)).expect("field");
    let Object::Dictionary(field) = field else {
        panic!("field must stay a dictionary");
    };
    assert_eq!(
        field.get(b"TU".as_slice()),
        Some(&Object::String(flpdf::pdf_string::new_unicode_string(
            "日本語".as_bytes()
        )))
    );
}

#[test]
fn set_value_marks_text_and_choice_fields_as_needing_appearances() {
    for field_type in ["/Tx", "/Ch"] {
        let bytes = doc_with_acroform(vec![
            (10, format!("<< /FT {field_type} >>")),
            (20, "<< >>".into()),
        ]);
        let mut pdf = open(bytes);
        FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
            .set_value_string("日本語", true)
            .expect("set text value");

        let field = pdf.resolve(ObjectRef::new(10, 0)).expect("field");
        let Object::Dictionary(field) = field else {
            panic!("field must be a dictionary");
        };
        assert_eq!(
            field.get(b"V".as_slice()),
            Some(&Object::String(flpdf::pdf_string::new_unicode_string(
                "日本語".as_bytes()
            )))
        );
        let acroform = pdf.resolve(ObjectRef::new(20, 0)).expect("acroform");
        let Object::Dictionary(acroform) = acroform else {
            panic!("AcroForm must be a dictionary");
        };
        assert_eq!(
            acroform.get(b"NeedAppearances".as_slice()),
            Some(&Object::Boolean(true))
        );
    }
}

#[test]
fn set_value_marks_the_terminal_acroform_reference_as_needing_appearances() {
    // qpdf mutates the AcroForm dictionary reached through its indirect
    // holder, leaving the holder itself intact.
    let bytes = doc_with_acroform(vec![
        (10, "<< /FT /Tx >>".into()),
        (20, "null".into()),
        (21, "<< >>".into()),
    ]);
    let mut pdf = open(bytes);
    pdf.set_object(
        ObjectRef::new(20, 0),
        Object::Reference(ObjectRef::new(21, 0)),
    );

    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value_string("value", true)
        .expect("set text value");

    assert_eq!(
        pdf.resolve(ObjectRef::new(20, 0)).expect("AcroForm holder"),
        Object::Reference(ObjectRef::new(21, 0))
    );
    let acroform = pdf.resolve(ObjectRef::new(21, 0)).expect("AcroForm");
    assert_eq!(
        acroform
            .as_dict()
            .and_then(|dictionary| dictionary.get("NeedAppearances")),
        Some(&Object::Boolean(true))
    );
}

#[test]
fn set_value_updates_checkbox_and_radio_widget_states_without_need_appearances() {
    let bytes = doc_with_acroform(vec![
        (
            10,
            "<< /FT /Btn /AP << /N << /Off null /Chosen null >> >> >>".into(),
        ),
        (20, "<< >>".into()),
    ]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(Object::Name(b"anything-but-Off".to_vec()), true)
        .expect("set checkbox value");
    let checkbox = pdf.resolve(ObjectRef::new(10, 0)).expect("checkbox");
    let Object::Dictionary(checkbox) = checkbox else {
        panic!("checkbox must be a dictionary");
    };
    assert_eq!(
        checkbox.get(b"V".as_slice()),
        Some(&Object::Name(b"Chosen".to_vec()))
    );
    assert_eq!(
        checkbox.get(b"AS".as_slice()),
        Some(&Object::Name(b"Chosen".to_vec()))
    );

    let bytes = doc_with_acroform(vec![
        (10, "<< /FT /Btn /Ff 32768 /Kids [11 0 R 12 0 R] >>".into()),
        (11, "<< /AP << /N << /Off null /First null >> >> >>".into()),
        (12, "<< /AP << /N << /Off null /Second null >> >> >>".into()),
        (20, "<< >>".into()),
    ]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(Object::Name(b"Second".to_vec()), true)
        .expect("set radio value");
    for (reference, expected) in [(11, b"Off".as_slice()), (12, b"Second".as_slice())] {
        let widget = pdf.resolve(ObjectRef::new(reference, 0)).expect("widget");
        let Object::Dictionary(widget) = widget else {
            panic!("widget must be a dictionary");
        };
        assert_eq!(
            widget.get(b"AS".as_slice()),
            Some(&Object::Name(expected.to_vec()))
        );
    }
    let acroform = pdf.resolve(ObjectRef::new(20, 0)).expect("acroform");
    let Object::Dictionary(acroform) = acroform else {
        panic!("AcroForm must be a dictionary");
    };
    assert_eq!(acroform.get(b"NeedAppearances".as_slice()), None);
}

#[test]
fn set_value_turns_an_existing_checkbox_off_and_leaves_pushbuttons_unchanged() {
    let bytes = doc(vec![(
        10,
        "<< /FT /Btn /V /Chosen /AS /Chosen /AP << /N << /Off null /Chosen null >> >> >>".into(),
    )]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(Object::Name(b"Off".to_vec()), true)
        .expect("turn checkbox off");
    let checkbox = pdf.resolve(ObjectRef::new(10, 0)).expect("checkbox");
    let Object::Dictionary(checkbox) = checkbox else {
        panic!("checkbox must be a dictionary");
    };
    assert_eq!(
        checkbox.get(b"V".as_slice()),
        Some(&Object::Name(b"Off".to_vec()))
    );
    assert_eq!(
        checkbox.get(b"AS".as_slice()),
        Some(&Object::Name(b"Off".to_vec()))
    );

    let bytes = doc(vec![(
        10,
        "<< /FT /Btn /Ff 65536 /V /Existing /AS /Existing /AP << /N /Existing >> >>".into(),
    )]);
    let mut pdf = open(bytes);
    let before = pdf.resolve(ObjectRef::new(10, 0)).expect("pushbutton");
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(Object::Name(b"New".to_vec()), true)
        .expect("pushbutton value is ignored");
    assert_eq!(
        pdf.resolve(ObjectRef::new(10, 0)).expect("pushbutton"),
        before,
        "qpdf ignores pushbutton value updates"
    );
}

#[test]
fn set_value_updates_a_checkbox_direct_kid_widget() {
    // qpdf calls `getKey("/AP")` on each /Kids array item, including direct
    // dictionaries, and writes both the field value and that widget's state.
    let bytes = doc(vec![(
        10,
        "<< /FT /Btn /Kids [ << /AP << /N << /Off null /Chosen null >> >> >> ] >>".into(),
    )]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(Object::Name(b"On".to_vec()), true)
        .expect("set direct-widget checkbox value");

    let field = pdf.resolve(ObjectRef::new(10, 0)).expect("checkbox field");
    let Object::Dictionary(field) = field else {
        panic!("checkbox field must be a dictionary");
    };
    assert_eq!(
        field.get(b"V".as_slice()),
        Some(&Object::Name(b"Chosen".to_vec()))
    );
    let Some(Object::Array(kids)) = field.get(b"Kids".as_slice()) else {
        panic!("checkbox must retain direct widget child");
    };
    let Some(Object::Dictionary(widget)) = kids.first() else {
        panic!("widget must be a direct dictionary");
    };
    assert_eq!(
        widget.get(b"AS".as_slice()),
        Some(&Object::Name(b"Chosen".to_vec()))
    );
}

#[test]
fn set_value_preserves_kids_order_when_direct_widget_precedes_a_reference() {
    let bytes = doc(vec![
        (
            10,
            "<< /FT /Btn /Kids [ << /AP << /N << /Off null /Direct null >> >> >> 11 0 R ] >>"
                .into(),
        ),
        (
            11,
            "<< /AP << /N << /Off null /Indirect null >> >> >>".into(),
        ),
    ]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(Object::Name(b"On".to_vec()), false)
        .expect("set checkbox value");

    let field = pdf.resolve(ObjectRef::new(10, 0)).expect("field");
    let Object::Dictionary(field) = field else {
        panic!("field must be a dictionary");
    };
    assert_eq!(
        field.get(b"V".as_slice()),
        Some(&Object::Name(b"Direct".to_vec()))
    );
    let Some(Object::Array(kids)) = field.get(b"Kids".as_slice()) else {
        panic!("kids must be an array");
    };
    let Object::Dictionary(direct) = &kids[0] else {
        panic!("first kid must stay direct");
    };
    assert_eq!(
        direct.get(b"AS".as_slice()),
        Some(&Object::Name(b"Direct".to_vec()))
    );
    let indirect = pdf.resolve(ObjectRef::new(11, 0)).expect("indirect widget");
    let Object::Dictionary(indirect) = indirect else {
        panic!("indirect widget must be a dictionary");
    };
    assert_eq!(indirect.get(b"AS".as_slice()), None);
}

#[test]
fn set_value_skips_non_dictionary_indirect_checkbox_kids_before_a_valid_widget() {
    // qpdf's `getKey` on a malformed indirect kid yields null; it continues
    // scanning `/Kids` and updates a later widget dictionary.
    let bytes = doc(vec![
        (10, "<< /FT /Btn /Kids [11 0 R 12 0 R] >>".into()),
        (11, "null".into()),
        (12, "<< /AP << /N << /Off null /Chosen null >> >> >>".into()),
    ]);
    let mut pdf = open(bytes);

    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(Object::Name(b"On".to_vec()), false)
        .expect("set checkbox value");

    let field = pdf.resolve(ObjectRef::new(10, 0)).expect("field");
    assert_eq!(
        field.as_dict().and_then(|dictionary| dictionary.get("V")),
        Some(&Object::Name(b"Chosen".to_vec()))
    );
    let widget = pdf.resolve(ObjectRef::new(12, 0)).expect("widget");
    assert_eq!(
        widget.as_dict().and_then(|dictionary| dictionary.get("AS")),
        Some(&Object::Name(b"Chosen".to_vec()))
    );
}

#[test]
fn set_value_updates_checkbox_state_for_a_non_dictionary_direct_appearance() {
    // qpdf treats any non-null field-level `/AP` as the annotation to update;
    // only its on-state lookup requires a dictionary, so `/Yes` is the
    // fallback when `/AP` itself is malformed.
    let bytes = doc(vec![(10, "<< /FT /Btn /AP 42 >>".into())]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(Object::Name(b"On".to_vec()), false)
        .expect("set checkbox value with malformed appearance");
    let field = pdf.resolve(ObjectRef::new(10, 0)).expect("checkbox field");
    let Object::Dictionary(field) = field else {
        panic!("checkbox field must be a dictionary");
    };
    assert_eq!(
        field.get(b"V".as_slice()),
        Some(&Object::Name(b"Yes".to_vec()))
    );
    assert_eq!(
        field.get(b"AS".as_slice()),
        Some(&Object::Name(b"Yes".to_vec()))
    );
}

#[test]
fn set_value_resolves_null_parent_and_indirect_kids_for_button_widgets() {
    // qpdf's `getKey("/Parent")` returns a null object for an explicit
    // `/Parent null`, so `setRadioButtonValue` treats this as a top-level
    // field. `getKey("/Kids")` also resolves its indirect array.
    let bytes = doc(vec![
        (
            10,
            "<< /FT /Btn /Ff 32768 /Parent null /Kids 13 0 R >>".into(),
        ),
        (11, "<< /AP << /N << /Off null /First null >> >> >>".into()),
        (12, "<< /AP << /N << /Off null /Second null >> >> >>".into()),
        (13, "[11 0 R 12 0 R]".into()),
    ]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(Object::Name(b"Second".to_vec()), true)
        .expect("set indirect-kids radio value");
    let field = pdf.resolve(ObjectRef::new(10, 0)).expect("radio field");
    let Object::Dictionary(field) = field else {
        panic!("radio field must be a dictionary");
    };
    assert_eq!(
        field.get(b"V".as_slice()),
        Some(&Object::Name(b"Second".to_vec()))
    );
    for (reference, expected) in [(11, b"Off".as_slice()), (12, b"Second".as_slice())] {
        let widget = pdf.resolve(ObjectRef::new(reference, 0)).expect("widget");
        let Object::Dictionary(widget) = widget else {
            panic!("widget must be a dictionary");
        };
        assert_eq!(
            widget.get(b"AS".as_slice()),
            Some(&Object::Name(expected.to_vec()))
        );
    }

    let bytes = doc(vec![
        (10, "<< /FT /Btn /Kids 12 0 R >>".into()),
        (11, "<< /AP << /N << /Off null /Chosen null >> >> >>".into()),
        (12, "[11 0 R]".into()),
    ]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(Object::Name(b"On".to_vec()), true)
        .expect("set indirect-kids checkbox value");
    let widget = pdf.resolve(ObjectRef::new(11, 0)).expect("widget");
    let Object::Dictionary(widget) = widget else {
        panic!("widget must be a dictionary");
    };
    assert_eq!(
        widget.get(b"AS".as_slice()),
        Some(&Object::Name(b"Chosen".to_vec()))
    );
}

#[test]
fn set_value_updates_a_radio_grandchild_widget_and_direct_kid_dictionaries() {
    // `setRadioButtonValue` examines only one `/Kids` level when a radio
    // child field has no `/AP`. qpdf object handles allow both that child and
    // its widget to be direct dictionaries in their respective arrays.
    let bytes = doc(vec![
        (
            10,
            "<< /FT /Btn /Ff 32768 /Parent null /Kids [ << /Kids [ << /AP << /N << /Off null /On null >> >> >> ] >> ] >>"
                .into(),
        ),
    ]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(Object::Name(b"On".to_vec()), true)
        .expect("set nested radio value");

    let field = pdf.resolve(ObjectRef::new(10, 0)).expect("radio field");
    let Object::Dictionary(field) = field else {
        panic!("radio field must be a dictionary");
    };
    assert_eq!(
        field.get(b"V".as_slice()),
        Some(&Object::Name(b"On".to_vec()))
    );
    let Some(Object::Array(children)) = field.get(b"Kids".as_slice()) else {
        panic!("radio field must retain direct children");
    };
    let Some(Object::Dictionary(child)) = children.first() else {
        panic!("child field must be a direct dictionary");
    };
    let Some(Object::Array(widgets)) = child.get(b"Kids".as_slice()) else {
        panic!("child field must retain direct widget children");
    };
    let Some(Object::Dictionary(widget)) = widgets.first() else {
        panic!("widget must be a direct dictionary");
    };
    assert_eq!(
        widget.get(b"AS".as_slice()),
        Some(&Object::Name(b"On".to_vec()))
    );
}

#[test]
fn set_value_turns_on_state_off_when_radio_appearance_is_non_null_but_not_a_dictionary() {
    // qpdf selects a radio kid as soon as `/AP` is non-null. It still writes
    // `/AS /Off` when that appearance cannot have an `/N` dictionary, whether
    // `/AP` is direct or indirect.
    let bytes = doc(vec![
        (10, "<< /FT /Btn /Ff 32768 /Kids [11 0 R 12 0 R] >>".into()),
        (11, "<< /AP /Bogus >>".into()),
        (12, "<< /AP 13 0 R >>".into()),
        (13, "/AlsoBogus".into()),
    ]);
    let mut pdf = open(bytes);
    FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .set_value(Object::Name(b"On".to_vec()), true)
        .expect("set radio value with malformed appearances");

    for reference in [11, 12] {
        let widget = pdf.resolve(ObjectRef::new(reference, 0)).expect("widget");
        let Object::Dictionary(widget) = widget else {
            panic!("widget must be a dictionary");
        };
        assert_eq!(
            widget.get(b"AS".as_slice()),
            Some(&Object::Name(b"Off".to_vec()))
        );
    }
}

#[test]
fn generates_a_field_value_on_its_separate_widget() {
    let bytes = doc_with_acroform(vec![
        (
            10,
            "<< /FT /Tx /V (value) /DA (/Helv 12 Tf 0 g) /Kids [11 0 R] >>".into(),
        ),
        (
            11,
            "<< /Subtype /Widget /Parent 12 0 R /Rect [0 0 100 20] >>".into(),
        ),
        (12, "<< /FT /Tx /V (wrong widget value) >>".into()),
        (20, "<< >>".into()),
    ]);
    let mut pdf = open(bytes);

    assert!(FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .generate_appearance_for(ObjectRef::new(11, 0))
        .unwrap()
        .is_some());
    assert!(
        pdf.resolve(ObjectRef::new(10, 0))
            .unwrap()
            .as_dict()
            .unwrap()
            .get("AP")
            .is_none(),
        "terminal field must not receive its widget's appearance"
    );
    assert!(pdf
        .resolve(ObjectRef::new(11, 0))
        .unwrap()
        .as_dict()
        .unwrap()
        .get("AP")
        .is_some());

    let widget = pdf.resolve(ObjectRef::new(11, 0)).unwrap();
    let ap_ref = widget
        .as_dict()
        .and_then(|widget| widget.get("AP"))
        .and_then(|ap| ap.as_dict())
        .and_then(|ap| ap.get_ref("N"))
        .expect("widget normal appearance reference");
    let appearance = pdf.resolve(ap_ref).unwrap();
    let Object::Stream(appearance) = appearance else {
        panic!("normal appearance must be a stream");
    };
    assert!(appearance
        .data
        .windows(b"(value)".len())
        .any(|w| w == b"(value)"));
    assert!(!appearance
        .data
        .windows(b"(wrong widget value)".len())
        .any(|w| w == b"(wrong widget value)"));
}

#[test]
fn generate_appearance_dispatches_only_text_and_choice_fields() {
    let bytes = doc_with_acroform(vec![
        (
            10,
            "<< /FT /Tx /V (value) /Rect [0 0 100 20] /DA (/Helv 12 Tf 0 g) >>".into(),
        ),
        (20, "<< >>".into()),
    ]);
    let mut pdf = open(bytes);
    assert!(FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .generate_appearance_for(ObjectRef::new(10, 0))
        .expect("text appearance")
        .is_some());

    let bytes = doc_with_acroform(vec![
        (
            10,
            "<< /FT /Ch /V (value) /Opt [(value)] /Rect [0 0 100 20] /DA (/Helv 12 Tf 0 g) >>"
                .into(),
        ),
        (20, "<< >>".into()),
    ]);
    let mut pdf = open(bytes);
    assert!(FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
        .generate_appearance_for(ObjectRef::new(10, 0))
        .expect("choice appearance")
        .is_some());

    let bytes = doc(vec![(10, "<< /FT /Btn >>".into())]);
    let mut pdf = open(bytes);
    assert_eq!(
        FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf)
            .generate_appearance_for(ObjectRef::new(10, 0))
            .expect("button is skipped"),
        None
    );
}

#[test]
fn consumer_clear_need_appearances_uses_the_form_field_helper_boundary() {
    let bytes = doc_with_acroform(vec![
        (
            10,
            "<< /FT /Tx /V (value) /Rect [0 0 100 20] /DA (/Helv 12 Tf 0 g) >>".into(),
        ),
        (20, "<< /NeedAppearances true >>".into()),
    ]);
    let mut pdf = open(bytes);

    FormFieldObjectHelper::clear_need_appearances_after_generation(&mut pdf)
        .expect("clear generated-appearance marker");

    let acroform = pdf.resolve(ObjectRef::new(20, 0)).expect("AcroForm");
    let Object::Dictionary(acroform) = acroform else {
        panic!("AcroForm must be a dictionary");
    };
    assert_eq!(acroform.get(b"NeedAppearances".as_slice()), None);
}
