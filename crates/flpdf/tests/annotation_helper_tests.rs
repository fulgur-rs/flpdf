//! Integration tests for [`flpdf::AnnotationObjectHelper`] and
//! [`flpdf::FormFieldObjectHelper`].
//!
//! All tests build minimal in-memory PDFs without touching the filesystem.
//! The PDF byte sequences are hand-crafted to exercise each typed accessor and,
//! for form fields, the `/Parent` chain inheritance behaviour.

use flpdf::{AnnotationObjectHelper, FormFieldObjectHelper, Object, ObjectRef, Pdf};
use std::collections::BTreeMap;
use std::io::Cursor;

// ── Minimal PDF builder ───────────────────────────────────────────────────────

/// Serialise an xref table and trailer, returning the complete PDF bytes.
///
/// `objects` is a list of `(object_number, serialized_object_bytes)`.
/// Objects are written in order; the trailer fixes up offsets automatically.
fn build_pdf(objects: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();

    for (num, bytes) in &objects {
        offsets.insert(*num, out.len() as u64);
        // Wrap in "N 0 obj … endobj"
        out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        out.extend_from_slice(bytes);
        out.extend_from_slice(b"\nendobj\n");
    }

    let xref_start = out.len() as u64;
    // Object count = highest number + 1 (free entry 0 is implicit).
    let count = objects.iter().map(|(n, _)| *n).max().unwrap_or(0) + 1;
    out.extend_from_slice(format!("xref\n0 {count}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for i in 1..count {
        let offset = offsets.get(&i).copied().unwrap_or(0);
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    let root_num = objects.first().map(|(n, _)| *n).unwrap_or(1);
    let trailer = format!(
        "trailer\n<< /Size {count} /Root {root_num} 0 R >>\nstartxref\n{xref_start}\n%%EOF\n"
    );
    out.extend_from_slice(trailer.as_bytes());
    out
}

/// Open a `Pdf` from raw bytes (panics on parse error — tests only).
fn open(bytes: Vec<u8>) -> Pdf<Cursor<Vec<u8>>> {
    Pdf::open(Cursor::new(bytes)).expect("Pdf::open")
}

// ── Helper: single-page PDF with one annotation ───────────────────────────────
//
// Object layout:
//   1 0 R  Catalog  (/Pages 2 0 R)
//   2 0 R  Pages    (/Kids [3 0 R])
//   3 0 R  Page     (/Annots [4 0 R])
//   4 0 R  Annotation  (the object under test)
fn build_annotation_pdf(annot_extras: &str) -> Vec<u8> {
    build_pdf(vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            b"<< /Type /Pages /Kids [ 3 0 R ] /Count 1 /MediaBox [ 0 0 612 792 ] >>".to_vec(),
        ),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /Annots [ 4 0 R ] >>".to_vec(),
        ),
        (4, format!("<< /Type /Annot {annot_extras} >>").into_bytes()),
    ])
}

// ── AnnotationObjectHelper::subtype ──────────────────────────────────────────

#[test]
fn annotation_subtype_returns_name_bytes() {
    let bytes = build_annotation_pdf("/Subtype /Highlight /Rect [10 20 200 50]");
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    let subtype = annot.subtype().expect("subtype()");
    assert_eq!(subtype, Some(b"Highlight".to_vec()));
}

#[test]
fn annotation_subtype_absent_returns_none() {
    let bytes = build_annotation_pdf("/Rect [0 0 100 100]");
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    assert_eq!(annot.subtype().expect("subtype()"), None);
}

// ── AnnotationObjectHelper::rect ─────────────────────────────────────────────

#[test]
fn annotation_rect_integers() {
    let bytes = build_annotation_pdf("/Subtype /Text /Rect [ 10 20 200 50 ]");
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    let rect = annot.rect().expect("rect()").expect("should have rect");
    assert_eq!(rect.llx, 10.0);
    assert_eq!(rect.lly, 20.0);
    assert_eq!(rect.urx, 200.0);
    assert_eq!(rect.ury, 50.0);
}

#[test]
fn annotation_rect_reals() {
    let bytes = build_annotation_pdf("/Subtype /Link /Rect [ 0.5 1.5 100.0 200.5 ]");
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    let rect = annot.rect().expect("rect()").expect("should have rect");
    assert!((rect.llx - 0.5).abs() < 1e-9);
    assert!((rect.lly - 1.5).abs() < 1e-9);
    assert!((rect.urx - 100.0).abs() < 1e-9);
    assert!((rect.ury - 200.5).abs() < 1e-9);
}

#[test]
fn annotation_rect_absent_returns_none() {
    let bytes = build_annotation_pdf("/Subtype /Text");
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    assert_eq!(annot.rect().expect("rect()"), None);
}

// ── AnnotationObjectHelper::appearance ───────────────────────────────────────

#[test]
fn annotation_appearance_inline_dict() {
    // AP is a direct dictionary (N stream reference not needed for this test).
    let bytes = build_annotation_pdf("/Subtype /Widget /Rect [0 0 10 10] /AP << /N 5 0 R >>");
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    let ap = annot
        .appearance()
        .expect("appearance()")
        .expect("should have AP");
    // /N should be present as a reference to object 5.
    assert!(ap.get("N").is_some());
}

#[test]
fn annotation_appearance_absent_returns_none() {
    let bytes = build_annotation_pdf("/Subtype /Text /Rect [0 0 10 10]");
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    assert_eq!(annot.appearance().expect("appearance()"), None);
}

// ── AnnotationObjectHelper::action ───────────────────────────────────────────

#[test]
fn annotation_action_inline_dict() {
    let bytes = build_annotation_pdf(
        "/Subtype /Link /Rect [0 0 100 20] /A << /Type /Action /S /URI /URI (https://example.com) >>",
    );
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    let action = annot.action().expect("action()").expect("should have /A");
    // /S should be the action subtype.
    match action.get("S") {
        Some(Object::Name(s)) => assert_eq!(s.as_slice(), b"URI"),
        other => panic!("expected Name for /S, got {other:?}"),
    }
}

#[test]
fn annotation_action_absent_returns_none() {
    let bytes = build_annotation_pdf("/Subtype /Text /Rect [0 0 10 10]");
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    assert_eq!(annot.action().expect("action()"), None);
}

// ── FormFieldObjectHelper — leaf field (no /Parent) ───────────────────────────
//
// Object layout:
//   1 0 R  Catalog
//   2 0 R  Pages
//   3 0 R  Page
//   4 0 R  Field with /FT /V /DV /Ff directly on it (no /Parent)

fn build_leaf_field_pdf(field_extras: &str) -> Vec<u8> {
    build_pdf(vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            b"<< /Type /Pages /Kids [ 3 0 R ] /Count 1 /MediaBox [ 0 0 612 792 ] >>".to_vec(),
        ),
        (3, b"<< /Type /Page /Parent 2 0 R >>".to_vec()),
        (
            4,
            format!("<< /Type /Annot /Subtype /Widget {field_extras} >>").into_bytes(),
        ),
    ])
}

#[test]
fn field_type_direct_on_widget() {
    let bytes = build_leaf_field_pdf("/FT /Tx /V (Hello) /DV () /Ff 0");
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    assert_eq!(
        field.field_type().expect("field_type()"),
        Some(b"Tx".to_vec())
    );
}

#[test]
fn field_value_string() {
    let bytes = build_leaf_field_pdf("/FT /Tx /V (Hello world)");
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    match field.field_value().expect("field_value()") {
        Some(Object::String(bytes)) => assert_eq!(bytes, b"Hello world"),
        other => panic!("expected String, got {other:?}"),
    }
}

#[test]
fn field_default_value_name() {
    let bytes = build_leaf_field_pdf("/FT /Btn /DV /Off");
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    match field.field_default_value().expect("field_default_value()") {
        Some(Object::Name(bytes)) => assert_eq!(bytes, b"Off"),
        other => panic!("expected Name, got {other:?}"),
    }
}

#[test]
fn field_flags_integer() {
    // Ff = 1 (ReadOnly bit)
    let bytes = build_leaf_field_pdf("/FT /Tx /Ff 1");
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    assert_eq!(field.field_flags().expect("field_flags()"), Some(1));
}

#[test]
fn field_absent_returns_none() {
    let bytes = build_leaf_field_pdf("");
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    assert_eq!(field.field_type().expect("field_type()"), None);
    assert_eq!(field.field_value().expect("field_value()"), None);
    assert_eq!(
        field.field_default_value().expect("field_default_value()"),
        None
    );
    assert_eq!(field.field_flags().expect("field_flags()"), None);
}

// ── FormFieldObjectHelper — /Parent chain inheritance ─────────────────────────
//
// Object layout:
//   1 0 R  Catalog
//   2 0 R  Pages
//   3 0 R  Page
//   4 0 R  Parent field  — carries /FT /V /DV /Ff
//   5 0 R  Child widget  — /Parent 4 0 R; lacks /FT /V /DV /Ff
//
// The child helper must resolve all four values from the parent.

fn build_parent_child_field_pdf(parent_field_extras: &str, child_widget_extras: &str) -> Vec<u8> {
    build_pdf(vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            b"<< /Type /Pages /Kids [ 3 0 R ] /Count 1 /MediaBox [ 0 0 612 792 ] >>".to_vec(),
        ),
        (3, b"<< /Type /Page /Parent 2 0 R /Annots [ 5 0 R ] >>".to_vec()),
        // Parent field (non-terminal, non-widget): carries inheritable attrs.
        (
            4,
            format!("<< /Kids [ 5 0 R ] {parent_field_extras} >>").into_bytes(),
        ),
        // Child widget: points back to parent via /Parent.
        (
            5,
            format!(
                "<< /Type /Annot /Subtype /Widget /Parent 4 0 R /Rect [ 72 700 300 720 ] {child_widget_extras} >>"
            )
            .into_bytes(),
        ),
    ])
}

/// Core inheritance test: /FT and /V live on the parent; child widget inherits both.
#[test]
fn field_type_inherited_from_parent() {
    let bytes = build_parent_child_field_pdf("/FT /Tx /V (inherited value)", "");
    let mut pdf = open(bytes);

    // The child (5 0 R) has no /FT, so it must be read from parent (4 0 R).
    let mut child = FormFieldObjectHelper::new(ObjectRef::new(5, 0), &mut pdf);
    assert_eq!(
        child.field_type().expect("field_type()"),
        Some(b"Tx".to_vec())
    );
}

#[test]
fn field_value_inherited_from_parent() {
    let bytes = build_parent_child_field_pdf("/FT /Tx /V (from parent)", "");
    let mut pdf = open(bytes);
    let mut child = FormFieldObjectHelper::new(ObjectRef::new(5, 0), &mut pdf);
    match child.field_value().expect("field_value()") {
        Some(Object::String(bytes)) => assert_eq!(bytes, b"from parent"),
        other => panic!("expected String, got {other:?}"),
    }
}

#[test]
fn field_default_value_inherited_from_parent() {
    let bytes = build_parent_child_field_pdf("/FT /Btn /DV /Off", "");
    let mut pdf = open(bytes);
    let mut child = FormFieldObjectHelper::new(ObjectRef::new(5, 0), &mut pdf);
    match child.field_default_value().expect("field_default_value()") {
        Some(Object::Name(name)) => assert_eq!(name, b"Off"),
        other => panic!("expected Name, got {other:?}"),
    }
}

#[test]
fn field_flags_inherited_from_parent() {
    // Ff = 4096 (Combo bit for Ch fields, just a non-trivial value).
    let bytes = build_parent_child_field_pdf("/FT /Ch /Ff 4096", "");
    let mut pdf = open(bytes);
    let mut child = FormFieldObjectHelper::new(ObjectRef::new(5, 0), &mut pdf);
    assert_eq!(child.field_flags().expect("field_flags()"), Some(4096));
}

/// Child value takes priority over parent value (self-value wins).
#[test]
fn field_value_child_overrides_parent() {
    let bytes = build_parent_child_field_pdf("/FT /Tx /V (parent value)", "/V (child value)");
    let mut pdf = open(bytes);
    let mut child = FormFieldObjectHelper::new(ObjectRef::new(5, 0), &mut pdf);
    match child.field_value().expect("field_value()") {
        Some(Object::String(bytes)) => assert_eq!(bytes, b"child value"),
        other => panic!("expected String 'child value', got {other:?}"),
    }
}

/// When child has /FT directly, parent /FT is not consulted.
#[test]
fn field_type_child_overrides_parent() {
    let bytes = build_parent_child_field_pdf("/FT /Tx", "/FT /Btn");
    let mut pdf = open(bytes);
    let mut child = FormFieldObjectHelper::new(ObjectRef::new(5, 0), &mut pdf);
    assert_eq!(
        child.field_type().expect("field_type()"),
        Some(b"Btn".to_vec())
    );
}

// ── Cycle guard ───────────────────────────────────────────────────────────────
//
// Object 10 → /Parent 11; Object 11 → /Parent 10 (cycle).
// The helper must terminate without panicking and return None.

#[test]
fn field_cycle_guard_does_not_loop_forever() {
    let bytes = build_pdf(vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            b"<< /Type /Pages /Kids [ 3 0 R ] /Count 1 /MediaBox [ 0 0 612 792 ] >>".to_vec(),
        ),
        (3, b"<< /Type /Page /Parent 2 0 R >>".to_vec()),
        // Cycle: 10 → parent 11, 11 → parent 10.
        (10, b"<< /Type /Annot /Parent 11 0 R >>".to_vec()),
        (11, b"<< /Type /Annot /Parent 10 0 R >>".to_vec()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    // Should not loop — cycle guard returns None.
    let result = field.field_type().expect("field_type() should not error");
    assert_eq!(result, None);
}

// ── AnnotationObjectHelper — non-dictionary object ───────────────────────────

#[test]
fn annotation_helper_on_non_dict_returns_error() {
    // Object 4 is an integer, not a dictionary.
    let bytes = build_pdf(vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            b"<< /Type /Pages /Kids [ 3 0 R ] /Count 1 /MediaBox [ 0 0 612 792 ] >>".to_vec(),
        ),
        (3, b"<< /Type /Page /Parent 2 0 R >>".to_vec()),
        (4, b"42".to_vec()),
    ]);
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    // Any accessor must fail because the object is not a dictionary.
    assert!(annot.subtype().is_err());
}
