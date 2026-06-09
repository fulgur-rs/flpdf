//! Error-path and edge-case tests for the annotation/form-field helpers.
//!
//! The happy paths live in `annotation_helper_tests.rs`. This file targets the
//! malformed-input branches of [`flpdf::AnnotationObjectHelper`] and
//! [`flpdf::FormFieldObjectHelper`]:
//!
//! - the shared resolver free functions (`resolve_to_array`,
//!   `resolve_optional_dict`, `parse_rect_array`) reached via `rect()`,
//!   `appearance()`, and `action()`;
//! - each inheritance walk's `/Parent`-chain anomalies — direct/indirect Null,
//!   wrong value type, non-dictionary node, and the depth-limit guard — across
//!   the three distinct walkers (`/FT` name, `/V` object, `/Ff` integer).

use flpdf::{AnnotationObjectHelper, Error, FormFieldObjectHelper, Object, ObjectRef, Pdf};
use std::io::Cursor;

/// Build a PDF from a set of already-serialised indirect objects.
fn build_pdf(objects: Vec<(u32, String)>, root: u32) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.5\n".to_vec();
    let max_num = objects.iter().map(|(n, _)| *n).max().unwrap_or(0);
    let mut offsets: Vec<(u32, u64)> = Vec::new();
    for (num, body) in &objects {
        let off = out.len() as u64;
        offsets.push((*num, off));
        out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        out.extend_from_slice(body.as_bytes());
        out.extend_from_slice(b"\nendobj\n");
    }
    let total = max_num as usize + 1;
    let xref_start = out.len() as u64;
    let mut xref = format!("xref\n0 {total}\n0000000000 65535 f \n");
    for i in 1..=max_num {
        if let Some((_, off)) = offsets.iter().find(|(n, _)| *n == i) {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        } else {
            xref.push_str("0000000000 65535 f \n");
        }
    }
    out.extend_from_slice(xref.as_bytes());
    let trailer =
        format!("trailer\n<< /Size {total} /Root {root} 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
    out.extend_from_slice(trailer.as_bytes());
    out
}

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
    build_pdf(base, 1)
}

fn assert_unsupported<T: std::fmt::Debug>(result: flpdf::Result<T>) {
    match result {
        Err(Error::Unsupported(_)) => {}
        other => panic!("expected Error::Unsupported, got {other:?}"),
    }
}

// ===========================================================================
// AnnotationObjectHelper — shared resolver free functions
// ===========================================================================

#[test]
fn rect_reference_not_array_errors() {
    let bytes = doc(vec![
        (10, "<< /Type /Annot /Rect 11 0 R >>".into()),
        (11, "42".into()),
    ]);
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_unsupported(annot.rect());
}

#[test]
fn rect_unexpected_type_errors() {
    let bytes = doc(vec![(10, "<< /Type /Annot /Rect 42 >>".into())]);
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_unsupported(annot.rect());
}

#[test]
fn rect_wrong_length_errors() {
    let bytes = doc(vec![(10, "<< /Type /Annot /Rect [0 0 1] >>".into())]);
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_unsupported(annot.rect());
}

#[test]
fn rect_non_numeric_element_errors() {
    let bytes = doc(vec![(10, "<< /Type /Annot /Rect [0 0 1 /X] >>".into())]);
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_unsupported(annot.rect());
}

#[test]
fn appearance_indirect_null_returns_none() {
    let bytes = doc(vec![
        (10, "<< /Type /Annot /AP 11 0 R >>".into()),
        (11, "null".into()),
    ]);
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(annot.appearance().unwrap(), None);
}

#[test]
fn appearance_reference_not_dict_errors() {
    let bytes = doc(vec![
        (10, "<< /Type /Annot /AP 11 0 R >>".into()),
        (11, "42".into()),
    ]);
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_unsupported(annot.appearance());
}

#[test]
fn action_unexpected_type_errors() {
    // /A is neither dict, reference, nor null: resolve_optional_dict errors.
    let bytes = doc(vec![(10, "<< /Type /Annot /A 42 >>".into())]);
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_unsupported(annot.action());
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
fn field_type_parent_not_dictionary_errors() {
    let bytes = doc(vec![
        (10, "<< /Type /Annot /Parent 11 0 R >>".into()),
        (11, "42".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_unsupported(field.field_type());
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
    match field.field_value().unwrap() {
        Some(Object::String(bytes)) => assert_eq!(bytes, b"inherited"),
        other => panic!("expected inherited string, got {other:?}"),
    }
}

#[test]
fn field_value_parent_not_dictionary_errors() {
    let bytes = doc(vec![
        (10, "<< /Type /Annot /Parent 11 0 R >>".into()),
        (11, "42".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_unsupported(field.field_value());
}

#[test]
fn field_value_cycle_returns_none() {
    let bytes = doc(vec![
        (10, "<< /Type /Annot /Parent 11 0 R >>".into()),
        (11, "<< /Type /Annot /Parent 10 0 R >>".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(field.field_value().unwrap(), None);
}

// ===========================================================================
// FormFieldObjectHelper — /Ff integer walker (resolve_inherited_integer)
// ===========================================================================

#[test]
fn field_flags_wrong_value_type_skipped_returns_none() {
    // /Ff is a name (not an integer): skipped, and with no parent → None.
    let bytes = doc(vec![(10, "<< /Type /Annot /Ff /Nope >>".into())]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_eq!(field.field_flags().unwrap(), None);
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
fn field_flags_parent_not_dictionary_errors() {
    let bytes = doc(vec![
        (10, "<< /Type /Annot /Parent 11 0 R >>".into()),
        (11, "42".into()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_unsupported(field.field_flags());
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
// Depth-limit guard (DEFAULT_MAX_PAGE_TREE_DEPTH) on each walker
// ===========================================================================

/// Build a /Parent chain of `len` field nodes (10, 11, ..., 10+len-1), none of
/// which carries the inheritable key, so every walker climbs to the limit.
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
fn field_type_depth_limit_errors() {
    // 130 hops exceeds DEFAULT_MAX_PAGE_TREE_DEPTH (100).
    let bytes = deep_field_chain(130);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_unsupported(field.field_type());
}

#[test]
fn field_value_depth_limit_errors() {
    let bytes = deep_field_chain(130);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_unsupported(field.field_value());
}

#[test]
fn field_flags_depth_limit_errors() {
    let bytes = deep_field_chain(130);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    assert_unsupported(field.field_flags());
}
