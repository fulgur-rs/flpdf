//! Integration tests for [`flpdf::AnnotationObjectHelper`] and
//! [`flpdf::FormFieldObjectHelper`].
//!
//! All tests build minimal in-memory PDFs without touching the filesystem.
//! The PDF byte sequences are hand-crafted to exercise each typed accessor and,
//! for form fields, the `/Parent` chain inheritance behaviour.

use flpdf::{AnnotationObjectHelper, FormFieldObjectHelper, ObjectRef, Pdf};
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
        match offsets.get(&i) {
            Some(offset) => {
                out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
            }
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
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

// ── AnnotationObjectHelper::get_subtype ──────────────────────────────────────

#[test]
fn annotation_subtype_returns_name_bytes() {
    let bytes = build_annotation_pdf("/Subtype /Highlight /Rect [10 20 200 50]");
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    let subtype = annot.get_subtype().expect("get_subtype()");
    assert_eq!(subtype, b"Highlight".to_vec());
}

#[test]
fn annotation_subtype_absent_returns_the_qpdf_fake_name_sentinel() {
    // qpdf's `getSubtype` unconditionally calls `getName()`, which returns
    // the dummy name `"/QPDFFakeName"` (leading `/` stripped by this
    // crate's convention) rather than an empty string when `/Subtype` is
    // absent (`libqpdf/QPDFObjectHandle.cc:634-643`).
    let bytes = build_annotation_pdf("/Rect [0 0 100 100]");
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    assert_eq!(
        annot.get_subtype().expect("get_subtype()"),
        b"QPDFFakeName".to_vec()
    );
}

#[test]
fn annotation_subtype_follows_indirect_name() {
    let bytes = build_pdf(vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [ 3 0 R ] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /Annots [ 4 0 R ] >>".to_vec(),
        ),
        (4, b"<< /Type /Annot /Subtype 5 0 R >>".to_vec()),
        (5, b"/Widget".to_vec()),
    ]);
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);

    assert_eq!(
        annot.get_subtype().expect("get_subtype()"),
        b"Widget".to_vec()
    );
}

#[test]
fn annotation_subtype_indirect_non_name_returns_the_qpdf_fake_name_sentinel() {
    let bytes = build_pdf(vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [ 3 0 R ] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /Annots [ 4 0 R ] >>".to_vec(),
        ),
        (4, b"<< /Type /Annot /Subtype 5 0 R >>".to_vec()),
        (5, b"42".to_vec()),
    ]);
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);

    assert_eq!(
        annot.get_subtype().expect("get_subtype()"),
        b"QPDFFakeName".to_vec()
    );
}

// ── AnnotationObjectHelper::get_rect ─────────────────────────────────────────

#[test]
fn annotation_rect_integers() {
    let bytes = build_annotation_pdf("/Subtype /Text /Rect [ 10 20 200 50 ]");
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    let rect = annot.get_rect().expect("get_rect()");
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
    let rect = annot.get_rect().expect("get_rect()");
    assert!((rect.llx - 0.5).abs() < 1e-9);
    assert!((rect.lly - 1.5).abs() < 1e-9);
    assert!((rect.urx - 100.0).abs() < 1e-9);
    assert!((rect.ury - 200.5).abs() < 1e-9);
}

#[test]
fn annotation_rect_resolves_indirect_array() {
    let bytes = build_pdf(vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            b"<< /Type /Pages /Kids [ 3 0 R ] /Count 1 /MediaBox [ 0 0 612 792 ] >>".to_vec(),
        ),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /Annots [ 4 0 R ] >>".to_vec(),
        ),
        (4, b"<< /Type /Annot /Subtype /Text /Rect 5 0 R >>".to_vec()),
        (5, b"[ 10 20 200 50 ]".to_vec()),
    ]);
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);

    let rect = annot.get_rect().expect("get_rect()");

    assert_eq!(rect.llx, 10.0);
    assert_eq!(rect.lly, 20.0);
    assert_eq!(rect.urx, 200.0);
    assert_eq!(rect.ury, 50.0);
}

#[test]
fn annotation_rect_absent_returns_zero_box() {
    let bytes = build_annotation_pdf("/Subtype /Text");
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    assert_eq!(
        annot.get_rect().expect("get_rect()"),
        flpdf::PageBox::new(0.0, 0.0, 0.0, 0.0)
    );
}

#[test]
fn annotation_rect_reversed_corners_normalized() {
    // qpdf's getArrayAsRectangle normalizes llx<=urx, lly<=ury via min/max.
    let bytes = build_annotation_pdf("/Subtype /Text /Rect [ 200 50 10 20 ]");
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    let rect = annot.get_rect().expect("get_rect()");
    assert_eq!(rect.llx, 10.0);
    assert_eq!(rect.lly, 20.0);
    assert_eq!(rect.urx, 200.0);
    assert_eq!(rect.ury, 50.0);
}

// ── AnnotationObjectHelper::get_appearance_dictionary ────────────────────────

#[test]
fn annotation_appearance_indirect_dict() {
    // /AP is an indirect reference (6 0 R) so get_appearance_dictionary()
    // must resolve it. Object 6 is the appearance dict; object 5 is its /N
    // appearance stream.
    let bytes = build_pdf(vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            b"<< /Type /Pages /Kids [ 3 0 R ] /Count 1 /MediaBox [ 0 0 612 792 ] >>".to_vec(),
        ),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /Annots [ 4 0 R ] >>".to_vec(),
        ),
        (
            4,
            b"<< /Type /Annot /Subtype /Widget /Rect [0 0 10 10] /AP 6 0 R >>".to_vec(),
        ),
        (5, b"<< /Type /XObject /Subtype /Form >>".to_vec()),
        (6, b"<< /N 5 0 R >>".to_vec()),
    ]);
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    let ap = annot
        .get_appearance_dictionary()
        .expect("get_appearance_dictionary()");
    assert!(ap.as_dictionary().is_some(), "AP should resolve to a dict");
    let n = ap.get_key(b"/N");
    pdf.resolve_object_handle(&n).unwrap();
    assert_eq!(n.object_ref(), Some(ObjectRef::new(5, 0)));
}

#[test]
fn annotation_appearance_absent_returns_null_handle() {
    let bytes = build_annotation_pdf("/Subtype /Text /Rect [0 0 10 10]");
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    assert!(annot
        .get_appearance_dictionary()
        .expect("get_appearance_dictionary()")
        .is_null());
}

// ── QPDFAnnotationObjectHelper ObjectHandle boundary ───────────────────────

#[test]
fn annotation_handle_reads_qpdf_leaf_attributes() {
    let bytes = build_annotation_pdf("/Subtype /Highlight /Rect [10 20 200 50] /AS /On /F 12");
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);

    assert_eq!(annot.get_subtype().unwrap(), b"Highlight");
    assert_eq!(annot.get_appearance_state().unwrap(), b"On");
    assert_eq!(annot.get_flags().unwrap(), 12);
    assert_eq!(
        annot.get_rect().unwrap(),
        flpdf::PageBox::new(10.0, 20.0, 200.0, 50.0)
    );
    assert!(annot.get_appearance_dictionary().unwrap().is_null());
}

#[test]
fn annotation_handle_uses_direct_appearance_stream_even_with_state() {
    let bytes = build_pdf(vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [ 3 0 R ] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /Annots [ 4 0 R ] >>".to_vec(),
        ),
        (4, b"<< /Type /Annot /AS /On /AP << /N 5 0 R >> >>".to_vec()),
        (5, b"<< /Length 0 >>\nstream\n\nendstream".to_vec()),
    ]);
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    let stream = annot
        .get_appearance_stream(b"N", None)
        .expect("get_appearance_stream()");

    assert_eq!(stream.object_ref(), Some(ObjectRef::new(5, 0)));
    assert!(stream.as_stream_dict().is_some());
}

#[test]
fn annotation_handle_appearance_stream_state_dictionary_uses_as() {
    let bytes = build_pdf(vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [ 3 0 R ] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /Annots [ 4 0 R ] >>".to_vec(),
        ),
        (
            4,
            b"<< /Type /Annot /AS /On /AP << /N << /On 5 0 R /Off 6 0 R >> >> >>".to_vec(),
        ),
        (5, b"<< /Length 0 >>\nstream\n\nendstream".to_vec()),
        (6, b"<< /Length 0 >>\nstream\n\nendstream".to_vec()),
    ]);
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    let stream = annot
        .get_appearance_stream(b"N", None)
        .expect("get_appearance_stream()");
    assert_eq!(stream.object_ref(), Some(ObjectRef::new(5, 0)));
}

#[test]
fn annotation_handle_appearance_stream_explicit_state_overrides_as() {
    let bytes = build_pdf(vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [ 3 0 R ] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /Annots [ 4 0 R ] >>".to_vec(),
        ),
        (
            4,
            b"<< /Type /Annot /AS /On /AP << /N << /On 5 0 R /Off 6 0 R >> >> >>".to_vec(),
        ),
        (5, b"<< /Length 0 >>\nstream\n\nendstream".to_vec()),
        (6, b"<< /Length 0 >>\nstream\n\nendstream".to_vec()),
    ]);
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    let stream = annot
        .get_appearance_stream(b"N", Some(b"Off"))
        .expect("get_appearance_stream()");
    assert_eq!(stream.object_ref(), Some(ObjectRef::new(6, 0)));
}

#[test]
fn annotation_handle_appearance_stream_missing_state_returns_null() {
    let bytes = build_pdf(vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [ 3 0 R ] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /Annots [ 4 0 R ] >>".to_vec(),
        ),
        (
            4,
            b"<< /Type /Annot /AP << /N << /On 5 0 R >> >> >>".to_vec(),
        ),
        (5, b"<< /Length 0 >>\nstream\n\nendstream".to_vec()),
    ]);
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    // No /AS on the annotation and no explicit state: desired_state is empty,
    // so the state-dictionary branch (which requires a non-empty state) is
    // never taken.
    let stream = annot
        .get_appearance_stream(b"N", None)
        .expect("get_appearance_stream()");
    assert!(stream.is_null());
}

#[test]
fn annotation_handle_appearance_stream_state_dictionary_key_missing_returns_null() {
    let bytes = build_pdf(vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [ 3 0 R ] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /Annots [ 4 0 R ] >>".to_vec(),
        ),
        (
            4,
            b"<< /Type /Annot /AS /Off /AP << /N << /On 5 0 R >> >> >>".to_vec(),
        ),
        (5, b"<< /Length 0 >>\nstream\n\nendstream".to_vec()),
    ]);
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    // /AS selects "Off", a non-empty state, so the state-dictionary branch is
    // taken; but /N's state dictionary only has an "On" entry, so the
    // selected key doesn't resolve to a stream and the result is null.
    let stream = annot
        .get_appearance_stream(b"N", None)
        .expect("get_appearance_stream()");
    assert!(stream.is_null());
}

#[test]
fn annotation_handle_builds_qpdf_page_content_for_appearance() {
    let bytes = build_pdf(vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R >>".to_vec()),
        (
            4,
            b"<< /Type /Annot /Rect [10 20 110 40] /F 4 /AP << /N 5 0 R >> >>".to_vec(),
        ),
        (
            5,
            b"<< /Type /XObject /BBox [0 0 100 20] /Length 0 >>\nstream\n\nendstream".to_vec(),
        ),
    ]);
    let mut pdf = open(bytes);
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);

    let content = annot
        .get_page_content_for_appearance("/Fxo1", 0, 0, 0x3)
        .expect("get_page_content_for_appearance()");
    assert_eq!(content, b"q\n1 0 0 1 10 20 cm\n/Fxo1 Do\nQ\n".to_vec());

    let appearance = annot
        .get_appearance_stream(b"N", None)
        .expect("get_appearance_stream()");
    assert_eq!(
        appearance
            .as_stream_dict()
            .expect("appearance stream dictionary")
            .get_key(b"/Subtype")
            .as_name(),
        Some(b"Form".to_vec())
    );

    let skipped = annot
        .get_page_content_for_appearance("/Fxo1", 0, 8, 0)
        .expect("flag-gated appearance content");
    assert!(
        skipped.is_empty(),
        "missing required flags must suppress content"
    );
}

#[test]
fn annotation_handle_builds_no_rotate_page_content_for_appearance() {
    let bytes = build_annotation_pdf(
        "/Subtype /Widget /F 16 /Rect [10 20 110 40] \
         /AP << /N 5 0 R >>",
    );
    let bytes = {
        let mut pdf = open(bytes);
        let mut stream = flpdf::Dictionary::new();
        stream.insert(
            "BBox",
            flpdf::Object::Array(vec![
                flpdf::Object::Integer(0),
                flpdf::Object::Integer(0),
                flpdf::Object::Integer(100),
                flpdf::Object::Integer(20),
            ]),
        );
        pdf.set_object(
            ObjectRef::new(5, 0),
            flpdf::Object::Stream(flpdf::Stream::new(stream, Vec::new())),
        );
        pdf
    };

    // The helper owns the same qpdf NoRotate transform used by page flattening.
    let mut pdf = bytes;
    let mut annot = AnnotationObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    let content = annot
        .get_page_content_for_appearance("/Fxo1", 90, 0, 0x3)
        .expect("get_page_content_for_appearance()");
    assert_eq!(content, b"q\n0 1 -1 0 30 40 cm\n/Fxo1 Do\nQ\n".to_vec());
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
        Some(b"/Tx".to_vec())
    );
}

#[test]
fn field_value_string() {
    let bytes = build_leaf_field_pdf("/FT /Tx /V (Hello world)");
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    assert_eq!(
        field
            .field_value()
            .expect("field_value()")
            .and_then(|value| value.as_string()),
        Some(b"Hello world".to_vec())
    );
}

#[test]
fn field_default_value_name() {
    let bytes = build_leaf_field_pdf("/FT /Btn /DV /Off");
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    assert_eq!(
        field
            .field_default_value()
            .expect("field_default_value()")
            .and_then(|value| value.as_name()),
        Some(b"Off".to_vec())
    );
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
    assert!(field.field_value().expect("field_value()").is_none());
    assert!(field
        .field_default_value()
        .expect("field_default_value()")
        .is_none());
    assert_eq!(field.field_flags().expect("field_flags()"), None);
}

// ── FormFieldObjectHelper — indirect /V and /DV resolution ────────────────────
//
// /V and /DV may be stored as indirect references. field_value() /
// field_default_value() must dereference one level so the two read paths
// (FormFieldObjectHelper vs AcroFormDocumentHelper::field_infos) agree.

#[test]
fn field_value_resolves_indirect_reference() {
    // 4 0 obj << ... /V 6 0 R >> ; 6 0 obj (Paris). field_value() must return
    // the resolved String, not Object::Reference(6 0).
    let bytes = build_pdf(vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            b"<< /Type /Pages /Kids [ 3 0 R ] /Count 1 /MediaBox [ 0 0 612 792 ] >>".to_vec(),
        ),
        (3, b"<< /Type /Page /Parent 2 0 R >>".to_vec()),
        (
            4,
            b"<< /Type /Annot /Subtype /Widget /FT /Tx /T (city) /V 6 0 R >>".to_vec(),
        ),
        (6, b"(Paris)".to_vec()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    assert_eq!(
        field
            .field_value()
            .expect("field_value()")
            .and_then(|value| value.as_string()),
        Some(b"Paris".to_vec())
    );
}

#[test]
fn field_default_value_resolves_indirect_reference() {
    // /DV stored as an indirect reference must be resolved one level too.
    let bytes = build_pdf(vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            b"<< /Type /Pages /Kids [ 3 0 R ] /Count 1 /MediaBox [ 0 0 612 792 ] >>".to_vec(),
        ),
        (3, b"<< /Type /Page /Parent 2 0 R >>".to_vec()),
        (
            4,
            b"<< /Type /Annot /Subtype /Widget /FT /Tx /DV 6 0 R >>".to_vec(),
        ),
        (6, b"(default text)".to_vec()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    assert_eq!(
        field
            .field_default_value()
            .expect("field_default_value()")
            .and_then(|value| value.as_string()),
        Some(b"default text".to_vec())
    );
}

#[test]
fn field_value_indirect_null_treated_as_absent_inherits_parent() {
    // Child /V is an indirect reference that resolves to null. Per §7.3.9 a null
    // value is absent, so resolution must keep climbing the /Parent chain and
    // return the parent's /V rather than the resolved null or the raw reference.
    let bytes = build_pdf(vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            b"<< /Type /Pages /Kids [ 3 0 R ] /Count 1 /MediaBox [ 0 0 612 792 ] >>".to_vec(),
        ),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /Annots [ 5 0 R ] >>".to_vec(),
        ),
        (
            4,
            b"<< /Kids [ 5 0 R ] /FT /Tx /V (from parent) >>".to_vec(),
        ),
        (
            5,
            b"<< /Type /Annot /Subtype /Widget /Parent 4 0 R /V 6 0 R >>".to_vec(),
        ),
        (6, b"null".to_vec()),
    ]);
    let mut pdf = open(bytes);
    let mut child = FormFieldObjectHelper::new(ObjectRef::new(5, 0), &mut pdf);
    assert_eq!(
        child
            .field_value()
            .expect("field_value()")
            .and_then(|value| value.as_string()),
        Some(b"from parent".to_vec())
    );
}

#[test]
fn field_default_value_indirect_null_treated_as_absent_inherits_parent() {
    // Symmetric to the /V case: /DV shares the same resolution path, so an
    // indirect /DV resolving to null must also be treated as absent and inherit
    // the parent's /DV.
    let bytes = build_pdf(vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            b"<< /Type /Pages /Kids [ 3 0 R ] /Count 1 /MediaBox [ 0 0 612 792 ] >>".to_vec(),
        ),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /Annots [ 5 0 R ] >>".to_vec(),
        ),
        (4, b"<< /Kids [ 5 0 R ] /FT /Btn /DV /Off >>".to_vec()),
        (
            5,
            b"<< /Type /Annot /Subtype /Widget /Parent 4 0 R /DV 6 0 R >>".to_vec(),
        ),
        (6, b"null".to_vec()),
    ]);
    let mut pdf = open(bytes);
    let mut child = FormFieldObjectHelper::new(ObjectRef::new(5, 0), &mut pdf);
    assert_eq!(
        child
            .field_default_value()
            .expect("field_default_value()")
            .and_then(|value| value.as_name()),
        Some(b"Off".to_vec())
    );
}

#[test]
fn field_value_resolves_indirect_name() {
    // /V values are not always strings (e.g. checkbox/radio fields use a Name).
    // An indirect /V resolving to a Name must be dereferenced to the Name, not
    // returned as a bare Reference.
    let bytes = build_pdf(vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            b"<< /Type /Pages /Kids [ 3 0 R ] /Count 1 /MediaBox [ 0 0 612 792 ] >>".to_vec(),
        ),
        (3, b"<< /Type /Page /Parent 2 0 R >>".to_vec()),
        (
            4,
            b"<< /Type /Annot /Subtype /Widget /FT /Btn /V 6 0 R >>".to_vec(),
        ),
        (6, b"/Yes".to_vec()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    assert_eq!(
        field
            .field_value()
            .expect("field_value()")
            .and_then(|value| value.as_name()),
        Some(b"Yes".to_vec())
    );
}

#[test]
fn field_type_resolves_indirect_reference() {
    // /FT stored as an indirect reference must be dereferenced; otherwise the
    // field type is silently dropped (inconsistent with field_infos()).
    let bytes = build_pdf(vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            b"<< /Type /Pages /Kids [ 3 0 R ] /Count 1 /MediaBox [ 0 0 612 792 ] >>".to_vec(),
        ),
        (3, b"<< /Type /Page /Parent 2 0 R >>".to_vec()),
        (4, b"<< /Type /Annot /Subtype /Widget /FT 6 0 R >>".to_vec()),
        (6, b"/Tx".to_vec()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    assert_eq!(
        field.field_type().expect("field_type()"),
        Some(b"/Tx".to_vec())
    );
}

#[test]
fn field_flags_resolves_indirect_reference() {
    // /Ff stored as an indirect reference must be dereferenced.
    let bytes = build_pdf(vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            b"<< /Type /Pages /Kids [ 3 0 R ] /Count 1 /MediaBox [ 0 0 612 792 ] >>".to_vec(),
        ),
        (3, b"<< /Type /Page /Parent 2 0 R >>".to_vec()),
        (
            4,
            b"<< /Type /Annot /Subtype /Widget /FT /Tx /Ff 6 0 R >>".to_vec(),
        ),
        (6, b"1".to_vec()),
    ]);
    let mut pdf = open(bytes);
    let mut field = FormFieldObjectHelper::new(ObjectRef::new(4, 0), &mut pdf);
    assert_eq!(field.field_flags().expect("field_flags()"), Some(1));
}

#[test]
fn field_type_indirect_null_treated_as_absent_inherits_parent() {
    // An indirect /FT resolving to null is absent (§7.3.9), so the /Parent
    // chain is climbed and the parent's /FT is returned.
    let bytes = build_pdf(vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            b"<< /Type /Pages /Kids [ 3 0 R ] /Count 1 /MediaBox [ 0 0 612 792 ] >>".to_vec(),
        ),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /Annots [ 5 0 R ] >>".to_vec(),
        ),
        (4, b"<< /Kids [ 5 0 R ] /FT /Tx >>".to_vec()),
        (
            5,
            b"<< /Type /Annot /Subtype /Widget /Parent 4 0 R /FT 6 0 R >>".to_vec(),
        ),
        (6, b"null".to_vec()),
    ]);
    let mut pdf = open(bytes);
    let mut child = FormFieldObjectHelper::new(ObjectRef::new(5, 0), &mut pdf);
    assert_eq!(
        child.field_type().expect("field_type()"),
        Some(b"/Tx".to_vec())
    );
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
        Some(b"/Tx".to_vec())
    );
}

#[test]
fn field_value_inherited_from_parent() {
    let bytes = build_parent_child_field_pdf("/FT /Tx /V (from parent)", "");
    let mut pdf = open(bytes);
    let mut child = FormFieldObjectHelper::new(ObjectRef::new(5, 0), &mut pdf);
    assert_eq!(
        child
            .field_value()
            .expect("field_value()")
            .and_then(|value| value.as_string()),
        Some(b"from parent".to_vec())
    );
}

#[test]
fn field_default_value_inherited_from_parent() {
    let bytes = build_parent_child_field_pdf("/FT /Btn /DV /Off", "");
    let mut pdf = open(bytes);
    let mut child = FormFieldObjectHelper::new(ObjectRef::new(5, 0), &mut pdf);
    assert_eq!(
        child
            .field_default_value()
            .expect("field_default_value()")
            .and_then(|value| value.as_name()),
        Some(b"Off".to_vec())
    );
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
    assert_eq!(
        child
            .field_value()
            .expect("field_value()")
            .and_then(|value| value.as_string()),
        Some(b"child value".to_vec())
    );
}

/// When child has /FT directly, parent /FT is not consulted.
#[test]
fn field_type_child_overrides_parent() {
    let bytes = build_parent_child_field_pdf("/FT /Tx", "/FT /Btn");
    let mut pdf = open(bytes);
    let mut child = FormFieldObjectHelper::new(ObjectRef::new(5, 0), &mut pdf);
    assert_eq!(
        child.field_type().expect("field_type()"),
        Some(b"/Btn".to_vec())
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
fn annotation_helper_on_non_dict_returns_defaults() {
    // Object 4 is an integer, not a dictionary. qpdf's QPDFObjectHandle::
    // getKey on a non-dictionary handle type-warns and returns null rather
    // than throwing, so every accessor falls back to its default.
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
    assert_eq!(
        annot.get_subtype().expect("get_subtype()"),
        b"QPDFFakeName".to_vec()
    );
    assert_eq!(
        annot.get_rect().expect("get_rect()"),
        flpdf::PageBox::new(0.0, 0.0, 0.0, 0.0)
    );
    assert_eq!(annot.get_flags().expect("get_flags()"), 0);
}
