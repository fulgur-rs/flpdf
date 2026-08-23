//! Error-path and edge-case tests for the annotation/form-field helpers.
//!
//! The happy paths live in `annotation_object_helper_tests.rs`. This file targets the
//! malformed-input branches of [`flpdf::AnnotationObjectHelper`] and
//! [`flpdf::FormFieldObjectHelper`]:
//!
//! - [`flpdf::AnnotationObjectHelper`]'s fail-soft defaults for a malformed
//!   `/Rect` or a non-dictionary `/AP`, mirroring qpdf's typed-getter
//!   type-warning-and-default contract rather than erroring;
//! - each inheritance walk's `/Parent`-chain anomalies — direct/indirect Null,
//!   wrong value type, non-dictionary node, cycles, and long acyclic chains —
//!   across `/FT` names, `/V` objects, and `/Ff` integers.

use flpdf::{AnnotationObjectHelper, FormFieldObjectHelper, ObjectRef, Pdf};
use std::io::Cursor;

mod common;
use common::build_pdf;

fn open(bytes: Vec<u8>) -> Pdf<Cursor<Vec<u8>>> {
    Pdf::open(Cursor::new(bytes)).expect("PDF should parse")
}

/// Prepend a minimal Catalog/Pages/Page (objects 1-3) to `objects` so the
/// document opens, then build. Field/annotation objects start at 10.
fn doc(mut objects: Vec<(u32, String)>) -> Vec<u8> {
    let mut base = vec![
        (1u32, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
        (
            2u32,
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        ),
        (
            3u32,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_string(),
        ),
    ];
    base.append(&mut objects);
    build_pdf(&base, 1)
}

// ===========================================================================
// AnnotationObjectHelper — fail-soft defaults (qpdf typed-getter contract)
// ===========================================================================

#[test]
fn rect_reference_not_array_returns_zero_box() {
    let bytes = doc(vec![
        (10, "<< /Type /Annot /Rect 11 0 R >>".into()),
        (11, "42".into()),
    ]);
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(
        annot.get_rect().unwrap(),
        flpdf::PageBox::new(0.0, 0.0, 0.0, 0.0)
    );
}

#[test]
fn rect_unexpected_type_returns_zero_box() {
    let bytes = doc(vec![(10, "<< /Type /Annot /Rect 42 >>".into())]);
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(
        annot.get_rect().unwrap(),
        flpdf::PageBox::new(0.0, 0.0, 0.0, 0.0)
    );
}

#[test]
fn rect_wrong_length_returns_zero_box() {
    let bytes = doc(vec![(10, "<< /Type /Annot /Rect [0 0 1] >>".into())]);
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(
        annot.get_rect().unwrap(),
        flpdf::PageBox::new(0.0, 0.0, 0.0, 0.0)
    );
}

#[test]
fn rect_non_numeric_element_returns_zero_box() {
    let bytes = doc(vec![(10, "<< /Type /Annot /Rect [0 0 1 /X] >>".into())]);
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(
        annot.get_rect().unwrap(),
        flpdf::PageBox::new(0.0, 0.0, 0.0, 0.0)
    );
}

#[test]
fn appearance_indirect_null_returns_null_handle() {
    let bytes = doc(vec![
        (10, "<< /Type /Annot /AP 11 0 R >>".into()),
        (11, "null".into()),
    ]);
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert!(annot.get_appearance_dictionary().unwrap().is_null());
}

#[test]
fn appearance_reference_not_dict_returns_the_resolved_value_verbatim() {
    // qpdf's getAppearanceDictionary returns getKey("/AP") verbatim, with no
    // dictionary-type check of its own.
    let bytes = doc(vec![
        (10, "<< /Type /Annot /AP 11 0 R >>".into()),
        (11, "42".into()),
    ]);
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    let ap = annot.get_appearance_dictionary().unwrap();
    assert_eq!(ap.as_integer(), Some(42));
}

// ===========================================================================
// FormFieldObjectHelper — /FT name walker (resolve_inherited_name)
// ===========================================================================

#[test]
fn field_type_wrong_value_type_skipped_returns_none() {
    // /FT is an integer (not a name): the walker skips it and, with no parent,
    // reports None rather than erroring.
    let bytes = doc(vec![(10, "<< /Type /Annot /FT 42 >>".into())]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(field.field_type().unwrap(), None);
}

#[test]
fn field_type_parent_not_dictionary_returns_none() {
    let bytes = doc(vec![
        (10, "<< /Type /Annot /Parent 11 0 R >>".into()),
        (11, "42".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(field.field_type().unwrap(), None);
}

// ===========================================================================
// FormFieldObjectHelper — /V object walker (resolve_inherited_object)
// ===========================================================================

#[test]
fn field_value_direct_null_inherits_parent() {
    // /V is a direct Null on the child (treated as absent), so the inherited
    // parent value is returned.
    let bytes = doc(vec![
        (10, "<< /Type /Annot /Parent 11 0 R /V null >>".into()),
        (11, "<< /FT /Tx /V (inherited) >>".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(
        field
            .field_value()
            .unwrap()
            .and_then(|value| value.as_string()),
        Some(b"inherited".to_vec())
    );
}

#[test]
fn field_value_parent_not_dictionary_returns_none() {
    let bytes = doc(vec![
        (10, "<< /Type /Annot /Parent 11 0 R >>".into()),
        (11, "42".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert!(field.field_value().unwrap().is_none());
}

#[test]
fn field_value_cycle_returns_none() {
    let bytes = doc(vec![
        (10, "<< /Type /Annot /Parent 11 0 R >>".into()),
        (11, "<< /Type /Annot /Parent 10 0 R >>".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert!(field.field_value().unwrap().is_none());
}

// ===========================================================================
// FormFieldObjectHelper — /Ff integer walker (resolve_inherited_integer)
// ===========================================================================

#[test]
fn field_flags_wrong_value_type_returns_zero() {
    // qpdf's getFlags converts a present non-integer /Ff to zero.
    let bytes = doc(vec![(10, "<< /Type /Annot /Ff /Nope >>".into())]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(field.field_flags().unwrap(), Some(0));
}

#[test]
fn field_flags_direct_null_inherits_parent() {
    let bytes = doc(vec![
        (10, "<< /Type /Annot /Parent 11 0 R /Ff null >>".into()),
        (11, "<< /Ff 12 >>".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(field.field_flags().unwrap(), Some(12));
}

#[test]
fn field_flags_parent_not_dictionary_returns_none() {
    let bytes = doc(vec![
        (10, "<< /Type /Annot /Parent 11 0 R >>".into()),
        (11, "42".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(field.field_flags().unwrap(), None);
}

#[test]
fn field_flags_cycle_returns_none() {
    let bytes = doc(vec![
        (10, "<< /Type /Annot /Parent 11 0 R >>".into()),
        (11, "<< /Type /Annot /Parent 10 0 R >>".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(field.field_flags().unwrap(), None);
}

// ===========================================================================
// Long acyclic inheritance walks are cycle-bounded, as in qpdf
// ===========================================================================

/// Build a /Parent chain of `len` field nodes (10, 11, ..., 10+len-1), none of
/// which carries the inheritable key, so every walker reaches the terminal.
fn deep_field_chain(len: u32) -> Vec<u8> {
    let mut objects = Vec::new();
    for i in 0..len {
        let num = 10 + i;
        let body = if i + 1 < len {
            format!("<< /Type /Annot /Parent {} 0 R >>", num + 1)
        } else {
            "<< /Type /Annot >>".to_string()
        };
        objects.push((num, body));
    }
    doc(objects)
}

#[test]
fn field_type_long_acyclic_chain_returns_none() {
    let bytes = deep_field_chain(130);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(field.field_type().unwrap(), None);
}

#[test]
fn field_value_long_acyclic_chain_returns_none() {
    let bytes = deep_field_chain(130);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert!(field.field_value().unwrap().is_none());
}

#[test]
fn field_flags_long_acyclic_chain_returns_none() {
    let bytes = deep_field_chain(130);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(field.field_flags().unwrap(), None);
}
