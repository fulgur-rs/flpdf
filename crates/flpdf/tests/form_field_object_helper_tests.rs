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
