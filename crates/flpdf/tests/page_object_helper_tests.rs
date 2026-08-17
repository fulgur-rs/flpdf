//! Integration tests for [`flpdf::PageObjectHelper`].
//!
//! All tests build in-memory PDFs without touching the filesystem. They
//! exercise the per-page accessor methods (content_stream_objects, resources,
//! rotate, get_annotations, and all bounding-box variants) including
//! inheritance resolution and per-page mutation round-trips.

use flpdf::pipeline::PlString;
use flpdf::{
    apply_rotate_to_pages, pages, ContentToken, DecodeLevel, Error, Matrix, Object, ObjectHandle,
    ObjectParserCallbacks, ObjectRef, PageBox, PageObjectHelper, ParseControl, Pdf, PipelineResult,
    Rectangle, RotateMode, RotateOp, TokenFilter, TokenFilterOutput,
};
use std::cell::RefCell;
use std::io::Cursor;
use std::rc::Rc;

mod common;
use common::write_default;

// ---------------------------------------------------------------------------
// Minimal PDF builder helpers
// ---------------------------------------------------------------------------

/// Build a single-page PDF.
///
/// Layout:
///   1 0 R  Catalog
///   2 0 R  Pages  (inheritable attrs from `parent_extras`)
///   3 0 R  Page   (leaf attrs from `page_extras`)
///
/// Both `*_extras` are already-serialised PDF-dictionary key-value pairs
/// (e.g. `"/MediaBox [0 0 612 792]"`).
fn build_single_page_pdf(parent_extras: &str, page_extras: &str) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();

    let off1 = out.len() as u64;
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    let off2 = out.len() as u64;
    let pages_str =
        format!("2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 {parent_extras} >>\nendobj\n");
    out.extend_from_slice(pages_str.as_bytes());

    let off3 = out.len() as u64;
    let page_str = format!("3 0 obj\n<< /Type /Page /Parent 2 0 R {page_extras} >>\nendobj\n");
    out.extend_from_slice(page_str.as_bytes());

    let xref_start = out.len() as u64;
    out.extend_from_slice(
        format!(
            "xref\n0 4\n0000000000 65535 f \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \n",
            off1, off2, off3
        )
        .as_bytes(),
    );
    let trailer = format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
    out.extend_from_slice(trailer.as_bytes());
    out
}

/// Like `build_single_page_pdf` but allows attaching extra indirect objects.
/// `extra_objects` is a slice of `(object_number, serialized_bytes)`.
fn build_pdf_with_extras(
    parent_extras: &str,
    page_extras: &str,
    extra_objects: &[(u32, Vec<u8>)],
) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();

    let off1 = out.len() as u64;
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    let off2 = out.len() as u64;
    let pages_str =
        format!("2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 {parent_extras} >>\nendobj\n");
    out.extend_from_slice(pages_str.as_bytes());

    let off3 = out.len() as u64;
    let page_str = format!("3 0 obj\n<< /Type /Page /Parent 2 0 R {page_extras} >>\nendobj\n");
    out.extend_from_slice(page_str.as_bytes());

    let mut extra_offsets: Vec<(u32, u64)> = Vec::new();
    for (num, body) in extra_objects {
        let off = out.len() as u64;
        extra_offsets.push((*num, off));
        out.extend_from_slice(body);
    }

    let max_num = extra_offsets.iter().map(|(n, _)| *n).max().unwrap_or(3);
    let total = max_num as usize + 1;
    let xref_start = out.len() as u64;
    let mut xref = format!("xref\n0 {total}\n0000000000 65535 f \n");
    xref.push_str(&format!("{:010} 00000 n \n", off1));
    xref.push_str(&format!("{:010} 00000 n \n", off2));
    xref.push_str(&format!("{:010} 00000 n \n", off3));
    for i in 4..=max_num {
        if let Some((_, off)) = extra_offsets.iter().find(|(n, _)| *n == i) {
            xref.push_str(&format!("{:010} 00000 n \n", off));
        } else {
            xref.push_str("0000000000 65535 f \n");
        }
    }
    out.extend_from_slice(xref.as_bytes());
    let trailer =
        format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
    out.extend_from_slice(trailer.as_bytes());
    out
}

fn make_stream_object(num: u32, body: &[u8]) -> (u32, Vec<u8>) {
    let mut obj_bytes = format!("{num} 0 obj\n<< /Length {} >>\nstream\n", body.len()).into_bytes();
    obj_bytes.extend_from_slice(body);
    obj_bytes.extend_from_slice(b"\nendstream\nendobj\n");
    (num, obj_bytes)
}

fn open(bytes: Vec<u8>) -> Pdf<Cursor<Vec<u8>>> {
    Pdf::open(Cursor::new(bytes)).expect("PDF should parse")
}

struct PassthroughFilter;

impl TokenFilter for PassthroughFilter {
    fn handle_token(
        &mut self,
        token: &ContentToken,
        output: &mut TokenFilterOutput<'_>,
    ) -> PipelineResult<()> {
        output.write_token(token)
    }
}

#[derive(Default)]
struct CountingObjectCallbacks {
    objects: usize,
    eof_calls: usize,
}

impl ObjectParserCallbacks for CountingObjectCallbacks {
    fn handle_object(
        &mut self,
        _object: ObjectHandle,
        _offset: usize,
        _length: usize,
    ) -> flpdf::Result<ParseControl> {
        self.objects += 1;
        Ok(ParseControl::Continue)
    }

    fn handle_eof(&mut self) -> flpdf::Result<()> {
        self.eof_calls += 1;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// content_stream_objects()
// ---------------------------------------------------------------------------

#[test]
fn content_stream_objects_empty_when_no_contents() {
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792]", "");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let objects = helper.content_stream_objects().unwrap();
    assert!(objects.is_empty(), "expected no objects on empty page");
}

#[test]
fn content_stream_objects_parses_single_stream() {
    // Single /Contents stream: "q Q"
    let body = b"q Q";
    let (num, extra) = make_stream_object(4, body);
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 612 792]",
        "/Contents 4 0 R",
        &[(num, extra)],
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let objects = helper.content_stream_objects().unwrap();
    assert_eq!(
        objects,
        vec![
            Object::Operator(b"q".to_vec()),
            Object::Operator(b"Q".to_vec())
        ]
    );
}

#[test]
fn get_page_contents_returns_canonical_stream_handles() {
    let body = b"q Q";
    let (num, extra) = make_stream_object(4, body);
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 612 792]",
        "/Contents 4 0 R",
        &[(num, extra)],
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    let contents = helper
        .get_page_contents()
        .expect("page contents should resolve through ObjectHandle");
    assert_eq!(contents.len(), 1);
    assert_eq!(contents[0].object_ref(), Some(ObjectRef::new(4, 0)));
    assert!(contents[0].as_stream_dict().is_some());
}

#[test]
fn form_xobject_helper_reads_attributes_from_the_stream_dictionary() {
    let form = (
        4u32,
        b"4 0 obj\n<< /Type /XObject /Subtype /Form /BBox [0 0 10 20] /Resources << /XObject << >> >> /Length 1 >>\nstream\nx\nendstream\nendobj\n".to_vec(),
    );
    let bytes = build_pdf_with_extras("/MediaBox [0 0 612 792]", "", &[form]);
    let mut pdf = open(bytes);
    let form_handle = pdf.get_object_handle(ObjectRef::new(4, 0));
    let mut helper = PageObjectHelper::from_object_handle(form_handle, &mut pdf);

    let bbox = helper
        .get_attribute(b"/BBox", false)
        .expect("Form XObject attributes should use its stream dictionary");
    assert_eq!(bbox.as_array().unwrap().len(), 4);
}

#[test]
fn xobject_maps_follow_qpdf_direct_and_recursive_boundaries() {
    let image = (
        4u32,
        b"4 0 obj\n<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /Length 1 >>\nstream\nx\nendstream\nendobj\n".to_vec(),
    );
    let form = (
        5u32,
        b"5 0 obj\n<< /Type /XObject /Subtype /Form /BBox [0 0 10 20] /Resources << /XObject << /NestedImage 4 0 R /Self 5 0 R >> >> /Length 1 >>\nstream\nx\nendstream\nendobj\n".to_vec(),
    );
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 612 792] /Resources << /XObject << /PageImage 4 0 R /PageForm 5 0 R >> >>",
        "",
        &[image, form],
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    let direct_images = helper.get_images().expect("direct images should resolve");
    assert_eq!(direct_images.len(), 1);
    assert_eq!(
        direct_images
            .get(b"/PageImage".as_slice())
            .unwrap()
            .object_ref(),
        Some(ObjectRef::new(4, 0))
    );
    assert_eq!(helper.get_form_xobjects().unwrap().len(), 1);
    assert_eq!(helper.get_page_images().unwrap().len(), 1);
    assert_eq!(helper.get_form_xobjects_recursive().unwrap().len(), 2);

    let mut visited = Vec::new();
    helper
        .for_each_xobject(true, |object, resources, key| {
            visited.push((
                key,
                object.object_ref(),
                resources.as_dictionary().is_some(),
            ));
            Ok(())
        })
        .expect("canonical XObject traversal should terminate on the Form cycle");
    assert_eq!(visited.len(), 4);
    assert!(visited
        .iter()
        .all(|(_, _, resources_are_dict)| { *resources_are_dict }));

    let mut recursive_images = helper
        .get_images_recursive()
        .expect("recursive images should resolve");
    assert_eq!(
        recursive_images
            .remove(b"/PageImage".as_slice())
            .unwrap()
            .object_ref(),
        Some(ObjectRef::new(4, 0))
    );
    assert_eq!(
        recursive_images
            .remove(b"/NestedImage".as_slice())
            .unwrap()
            .object_ref(),
        Some(ObjectRef::new(4, 0))
    );
    assert!(recursive_images.is_empty());

    let direct_form = ObjectHandle::stream(
        ObjectHandle::dictionary(vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"XObject".to_vec())),
            (b"/Subtype".to_vec(), ObjectHandle::name(b"Form".to_vec())),
            (
                b"/BBox".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(1),
                    ObjectHandle::integer(1),
                ]),
            ),
        ]),
        Rc::new(Vec::new()),
    );
    let mut direct_form_helper = PageObjectHelper::from_object_handle(direct_form, &mut pdf);
    let mut no_resource_visits = 0;
    direct_form_helper
        .for_each_xobject(true, |_, _, _| {
            no_resource_visits += 1;
            Ok(())
        })
        .expect("a Form without resources should simply have no XObjects");
    assert_eq!(no_resource_visits, 0);
}

#[test]
fn get_form_xobject_for_page_builds_a_lazy_canonical_form_stream() {
    let (num, extra) = make_stream_object(4, b"q Q");
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 612 792]",
        "/TrimBox [10 20 110 220] /Contents 4 0 R /Resources << /ProcSet [/PDF] >>",
        &[(num, extra)],
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    let form = helper
        .get_form_xobject_for_page(true)
        .expect("page should become a Form XObject");
    assert!(form.is_form_xobject().unwrap());
    assert!(
        form.as_stream_data().is_none(),
        "content must remain provider-backed"
    );
    let dict = form.as_stream_dict().unwrap();
    assert_eq!(dict.get_key(b"/Type").as_name(), Some(b"XObject".to_vec()));
    assert_eq!(dict.get_key(b"/Subtype").as_name(), Some(b"Form".to_vec()));
    assert_eq!(dict.get_key(b"/BBox").as_array().unwrap().len(), 4);
}

#[test]
fn get_form_xobject_for_page_keeps_original_contents_after_page_rewrite() {
    let (num, extra) = make_stream_object(4, b"q Q");
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 612 792]",
        "/Contents 4 0 R",
        &[(num, extra)],
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    let form = helper
        .get_form_xobject_for_page(false)
        .expect("page should become a Form XObject");
    let replacement = ObjectHandle::stream(
        ObjectHandle::dictionary(Vec::new()),
        Rc::new(b"q /Fx0 Do Q".to_vec()),
    );
    helper
        .add_page_contents(replacement, false)
        .expect("rewriting the page contents should succeed");

    assert_eq!(
        form.get_stream_data(DecodeLevel::Specialized)
            .expect("Form provider should retain the original content")
            .as_slice(),
        b"q Q"
    );
}

#[test]
fn get_form_xobject_for_page_records_qpdf_invalid_bbox_warning() {
    let (num, extra) = make_stream_object(4, b"q Q");
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 612 792]",
        "/TrimBox [0 0 bad 220] /Contents 4 0 R",
        &[(num, extra)],
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    helper
        .get_form_xobject_for_page(false)
        .expect("qpdf creates the Form even with an invalid BBox");
    drop(helper);

    assert!(pdf
        .repair_diagnostics()
        .entries()
        .iter()
        .any(|diagnostic| diagnostic.message.contains("bounding box is invalid")));
}

#[test]
fn shallow_copy_page_promotes_a_canonical_dictionary_copy() {
    let (num, extra) = make_stream_object(4, b"q Q");
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 612 792]",
        "/Contents 4 0 R /Rotate 90",
        &[(num, extra)],
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    let copy = helper
        .shallow_copy_page()
        .expect("page shallow copy should allocate a new indirect object");
    let copy_ref = copy.object_ref().expect("copy should be indirect");
    assert_ne!(copy_ref, ObjectRef::new(3, 0));
    assert_eq!(
        copy.get_key(b"/Contents").object_ref(),
        Some(ObjectRef::new(4, 0))
    );
    assert_eq!(copy.get_key(b"/Rotate").as_integer(), Some(90));
}

#[test]
fn form_xobject_placement_uses_canonical_form_and_page_handles() {
    let (num, extra) = make_stream_object(4, b"q Q");
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 200 200]",
        "/TrimBox [0 0 100 100] /Contents 4 0 R",
        &[(num, extra)],
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let form = helper.get_form_xobject_for_page(false).unwrap();
    let matrix = helper
        .get_matrix_for_form_xobject_placement(
            form.clone(),
            Rectangle::new(0.0, 0.0, 200.0, 200.0),
            false,
            true,
            false,
        )
        .unwrap()
        .expect("non-degenerate Form should have a placement matrix");
    assert_eq!(matrix, Matrix::new(1.0, 0.0, 0.0, 1.0, 50.0, 50.0));

    let mut supplied = Matrix::default();
    let supplied_fragment = helper
        .place_form_xobject_with_matrix(
            form,
            "/Fx1",
            Rectangle::new(0.0, 0.0, 200.0, 200.0),
            &mut supplied,
            false,
            true,
            false,
        )
        .expect("the qpdf matrix-output overload should share placement logic");
    assert_eq!(supplied, matrix);
    assert_eq!(supplied_fragment, "q\n1 0 0 1 50 50 cm\n/Fx1 Do\nQ\n");
}

#[test]
fn form_xobject_placement_does_not_invert_page_transformations_when_disabled() {
    let bytes = build_single_page_pdf(
        "/MediaBox [0 0 100 100]",
        "/TrimBox [0 0 100 100] /Rotate 90 /UserUnit 2",
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let form = ObjectHandle::stream(
        ObjectHandle::dictionary(vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"XObject".to_vec())),
            (b"/Subtype".to_vec(), ObjectHandle::name(b"Form".to_vec())),
            (
                b"/BBox".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(100),
                    ObjectHandle::integer(100),
                ]),
            ),
        ]),
        Rc::new(Vec::new()),
    );

    assert_eq!(
        helper
            .get_matrix_for_form_xobject_placement(
                form,
                Rectangle::new(0.0, 0.0, 100.0, 100.0),
                false,
                true,
                false,
            )
            .unwrap(),
        Some(Matrix::default()),
        "qpdf skips the destination page transform when inversion is disabled"
    );
}

#[test]
fn form_xobject_placement_handles_qpdf_fallback_and_scaling_boundaries() {
    let bytes = build_single_page_pdf("/MediaBox [0 0 100 100]", "/TrimBox [0 0 100 100]");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    let non_form = ObjectHandle::dictionary(vec![(
        b"/Type".to_vec(),
        ObjectHandle::name(b"Page".to_vec()),
    )]);
    assert!(helper
        .get_matrix_for_form_xobject_placement(
            non_form,
            Rectangle::new(0.0, 0.0, 100.0, 100.0),
            false,
            true,
            true,
        )
        .unwrap()
        .is_none());

    let malformed_bbox = ObjectHandle::stream(
        ObjectHandle::dictionary(vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"XObject".to_vec())),
            (b"/Subtype".to_vec(), ObjectHandle::name(b"Form".to_vec())),
            (b"/BBox".to_vec(), ObjectHandle::integer(1)),
        ]),
        Rc::new(Vec::new()),
    );
    assert!(helper
        .get_matrix_for_form_xobject_placement(
            malformed_bbox,
            Rectangle::new(0.0, 0.0, 100.0, 100.0),
            false,
            true,
            true,
        )
        .unwrap()
        .is_none());

    let short_bbox = ObjectHandle::stream(
        ObjectHandle::dictionary(vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"XObject".to_vec())),
            (b"/Subtype".to_vec(), ObjectHandle::name(b"Form".to_vec())),
            (
                b"/BBox".to_vec(),
                ObjectHandle::array(vec![ObjectHandle::integer(0); 3]),
            ),
        ]),
        Rc::new(Vec::new()),
    );
    assert!(helper
        .get_matrix_for_form_xobject_placement(
            short_bbox,
            Rectangle::new(0.0, 0.0, 100.0, 100.0),
            false,
            true,
            true,
        )
        .unwrap()
        .is_none());

    let degenerate_bbox = ObjectHandle::stream(
        ObjectHandle::dictionary(vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"XObject".to_vec())),
            (b"/Subtype".to_vec(), ObjectHandle::name(b"Form".to_vec())),
            (
                b"/BBox".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(10),
                ]),
            ),
        ]),
        Rc::new(Vec::new()),
    );
    assert!(helper
        .get_matrix_for_form_xobject_placement(
            degenerate_bbox,
            Rectangle::new(0.0, 0.0, 100.0, 100.0),
            false,
            true,
            true,
        )
        .unwrap()
        .is_none());

    let malformed_matrix = ObjectHandle::stream(
        ObjectHandle::dictionary(vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"XObject".to_vec())),
            (b"/Subtype".to_vec(), ObjectHandle::name(b"Form".to_vec())),
            (
                b"/BBox".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(10),
                    ObjectHandle::integer(10),
                ]),
            ),
            (
                b"/Matrix".to_vec(),
                ObjectHandle::array(vec![ObjectHandle::integer(1); 5]),
            ),
        ]),
        Rc::new(Vec::new()),
    );
    let matrix = helper
        .get_matrix_for_form_xobject_placement(
            malformed_matrix,
            Rectangle::new(0.0, 0.0, 20.0, 20.0),
            false,
            true,
            true,
        )
        .unwrap()
        .expect("malformed optional Matrix uses qpdf identity fallback");
    assert_eq!(matrix, Matrix::new(2.0, 0.0, 0.0, 2.0, 0.0, 0.0));

    let non_numeric_matrix = ObjectHandle::stream(
        ObjectHandle::dictionary(vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"XObject".to_vec())),
            (b"/Subtype".to_vec(), ObjectHandle::name(b"Form".to_vec())),
            (
                b"/BBox".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(10),
                    ObjectHandle::integer(10),
                ]),
            ),
            (
                b"/Matrix".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::integer(1),
                    ObjectHandle::name(b"bad".to_vec()),
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(1),
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(0),
                ]),
            ),
        ]),
        Rc::new(Vec::new()),
    );
    assert!(helper
        .get_matrix_for_form_xobject_placement(
            non_numeric_matrix,
            Rectangle::new(0.0, 0.0, 20.0, 20.0),
            false,
            true,
            true,
        )
        .unwrap()
        .is_some());

    let scaled_form = ObjectHandle::stream(
        ObjectHandle::dictionary(vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"XObject".to_vec())),
            (b"/Subtype".to_vec(), ObjectHandle::name(b"Form".to_vec())),
            (
                b"/BBox".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(10),
                    ObjectHandle::integer(10),
                ]),
            ),
        ]),
        Rc::new(Vec::new()),
    );
    let shrink_disabled = helper
        .get_matrix_for_form_xobject_placement(
            scaled_form.clone(),
            Rectangle::new(0.0, 0.0, 5.0, 5.0),
            false,
            false,
            true,
        )
        .unwrap()
        .expect("non-degenerate Forms should have a matrix");
    assert_eq!(shrink_disabled, Matrix::new(1.0, 0.0, 0.0, 1.0, -2.5, -2.5));
    let expand_disabled = helper
        .get_matrix_for_form_xobject_placement(
            scaled_form.clone(),
            Rectangle::new(0.0, 0.0, 20.0, 20.0),
            false,
            true,
            false,
        )
        .unwrap()
        .expect("expansion-disabled placement should still be valid");
    assert_eq!(expand_disabled, Matrix::new(1.0, 0.0, 0.0, 1.0, 5.0, 5.0));
    let expanded = helper
        .get_matrix_for_form_xobject_placement(
            scaled_form,
            Rectangle::new(0.0, 0.0, 20.0, 20.0),
            false,
            true,
            true,
        )
        .unwrap()
        .expect("expansion-enabled placement should scale the Form");
    assert_eq!(expanded, Matrix::new(2.0, 0.0, 0.0, 2.0, 0.0, 0.0));
}

#[test]
fn transformation_and_attribute_helpers_cover_malformed_canonical_targets() {
    let bytes = build_single_page_pdf(
        "/MediaBox [0 0 100 100]",
        "/TrimBox [0 0 100 100] /UserUnit 0 /Rotate 90",
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    assert_eq!(
        helper.get_matrix_for_transformations(true).unwrap(),
        Matrix::default()
    );

    drop(helper);

    let malformed_bytes = build_single_page_pdf("/MediaBox [0 0 100 100]", "/TrimBox 1 /Rotate 90");
    let mut malformed_pdf = open(malformed_bytes);
    let mut malformed_helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut malformed_pdf);
    assert_eq!(
        malformed_helper
            .get_matrix_for_transformations(false)
            .unwrap(),
        Matrix::default()
    );

    let direct_page = ObjectHandle::dictionary(vec![(
        b"/Type".to_vec(),
        ObjectHandle::name(b"Page".to_vec()),
    )]);
    let mut direct_page_helper = PageObjectHelper::from_object_handle(direct_page, &mut pdf);
    assert!(direct_page_helper
        .shallow_copy_page()
        .expect_err("direct pages cannot be promoted through the page-ref API")
        .to_string()
        .contains("shallowCopyPage"));
    assert!(direct_page_helper
        .remove_unreferenced_resources()
        .expect_err("resource pruning requires an indirect page reference")
        .to_string()
        .contains("operation requires a page object reference"));
    drop(direct_page_helper);

    let direct_form = ObjectHandle::stream(
        ObjectHandle::dictionary(vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"XObject".to_vec())),
            (b"/Subtype".to_vec(), ObjectHandle::name(b"Form".to_vec())),
            (
                b"/BBox".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(1),
                    ObjectHandle::integer(1),
                ]),
            ),
        ]),
        Rc::new(Vec::new()),
    );
    let mut direct_form_helper =
        PageObjectHelper::from_object_handle(direct_form, &mut malformed_pdf);
    assert!(direct_form_helper
        .shallow_copy_page()
        .expect_err("Form targets are not leaf pages")
        .to_string()
        .contains("expected /Type /Page"));

    for target in [
        ObjectHandle::integer(1),
        ObjectHandle::dictionary(Vec::new()),
        ObjectHandle::dictionary(vec![(b"/Type".to_vec(), ObjectHandle::integer(1))]),
        ObjectHandle::dictionary(vec![(
            b"/Type".to_vec(),
            ObjectHandle::name(b"Pages".to_vec()),
        )]),
    ] {
        let mut target_helper = PageObjectHelper::from_object_handle(target, &mut pdf);
        assert!(target_helper.get_attribute(b"/MediaBox", false).is_err());
    }
}

#[test]
fn transformation_matrix_uses_identity_for_non_rectangle_trim_box() {
    let bytes = build_single_page_pdf(
        "/MediaBox [0 0 200 200]",
        "/TrimBox [0 0 100 100 200] /Rotate 90",
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    assert_eq!(
        helper
            .get_matrix_for_transformations(false)
            .expect("malformed BBox should use qpdf's identity fallback"),
        Matrix::default()
    );
}

#[test]
fn place_form_xobject_builds_qpdf_content_fragment_and_matrix() {
    let (num, extra) = make_stream_object(4, b"q Q");
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 200 200]",
        "/TrimBox [0 0 100 100] /Contents 4 0 R",
        &[(num, extra)],
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let form = helper.get_form_xobject_for_page(false).unwrap();
    let (fragment, matrix) = helper
        .place_form_xobject(
            form,
            "/Fx1",
            Rectangle::new(0.0, 0.0, 200.0, 200.0),
            false,
            true,
            false,
        )
        .expect("placement should use the canonical Form/page route");

    assert_eq!(matrix, Matrix::new(1.0, 0.0, 0.0, 1.0, 50.0, 50.0));
    assert_eq!(fragment, "q\n1 0 0 1 50 50 cm\n/Fx1 Do\nQ\n");
}

#[test]
fn page_helper_remove_unreferenced_resources_prunes_canonical_categories() {
    let (num, extra) = make_stream_object(4, b"BT /F1 12 Tf ET");
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 612 792]",
        "/Contents 4 0 R /Resources << /Font << /F1 << >> /F2 << >> >> >>",
        &[(num, extra)],
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    helper
        .remove_unreferenced_resources()
        .expect("page helper should prune through canonical handles");

    let resources = helper
        .get_resources(false)
        .expect("resources should remain available");
    let fonts = resources
        .get_key(b"/Font")
        .as_dictionary()
        .expect("font category should remain a dictionary");
    assert!(fonts.iter().any(|(key, _)| key == b"/F1"));
    assert!(!fonts.iter().any(|(key, _)| key == b"/F2"));
}

#[test]
fn page_helper_resource_pruning_preserves_resources_when_content_is_incomplete() {
    let (num, extra) = make_stream_object(4, b"/F1 12 Tf [");
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 612 792]",
        "/Contents 4 0 R /Resources << /Font << /F1 << >> /F2 << >> >> >>",
        &[(num, extra)],
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    helper
        .remove_unreferenced_resources()
        .expect("incomplete content is a warning-only resource-pruning case");
    let fonts = helper
        .get_resources(false)
        .unwrap()
        .get_key(b"/Font")
        .as_dictionary()
        .unwrap();
    assert_eq!(fonts.len(), 2, "unknown usage must retain every font");
}

#[test]
fn page_helper_resource_pruning_shallow_copies_indirect_categories() {
    let (num, extra) = make_stream_object(4, b"BT /F1 12 Tf ET");
    let fonts = (
        5u32,
        b"5 0 obj\n<< /F1 << >> /F2 << >> >>\nendobj\n".to_vec(),
    );
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 612 792]",
        "/Contents 4 0 R /Resources << /Font 5 0 R >>",
        &[(num, extra), fonts],
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    helper
        .remove_unreferenced_resources()
        .expect("indirect resource categories should be copied before pruning");
    let font_handle = helper.get_resources(false).unwrap().get_key(b"/Font");
    assert!(font_handle.is_direct());
    let fonts = font_handle.as_dictionary().unwrap();
    assert!(fonts.iter().any(|(key, _)| key == b"/F1"));
    assert!(!fonts.iter().any(|(key, _)| key == b"/F2"));
}

#[test]
fn form_helper_remove_unreferenced_resources_prunes_form_categories() {
    let form_body = b"BT /F1 12 Tf ET";
    let mut form = format!(
        "5 0 obj\n<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] \
         /Resources << /Font << /F1 << >> /F2 << >> >> >> /Length {} >>\nstream\n",
        form_body.len()
    )
    .into_bytes();
    form.extend_from_slice(form_body);
    form.extend_from_slice(b"\nendstream\nendobj\n");
    let bytes = build_pdf_with_extras("/MediaBox [0 0 100 100]", "", &[(5, form)]);
    let mut pdf = open(bytes);
    let form_handle = pdf.get_object_handle(ObjectRef::new(5, 0));
    let mut helper = PageObjectHelper::from_object_handle(form_handle, &mut pdf);

    helper
        .remove_unreferenced_resources()
        .expect("Form helper should prune through canonical handles");

    let resources = helper.get_resources(false).unwrap();
    let fonts = resources
        .get_key(b"/Font")
        .as_dictionary()
        .expect("Form font category should remain a dictionary");
    assert!(fonts.iter().any(|(key, _)| key == b"/F1"));
    assert!(!fonts.iter().any(|(key, _)| key == b"/F2"));
}

#[test]
fn form_helper_resource_pruning_handles_missing_nested_and_indirect_resources() {
    let nested_body = b"BT /F1 12 Tf ET";
    let mut nested = format!(
        "6 0 obj\n<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] \
         /Resources << /Font << /F1 << >> /F2 << >> >> /XObject 8 0 R >> /Length {} >>\nstream\n",
        nested_body.len()
    )
    .into_bytes();
    nested.extend_from_slice(nested_body);
    nested.extend_from_slice(b"\nendstream\nendobj\n");

    let root_body = b"BT /F1 12 Tf ET";
    let mut root = format!(
        "5 0 obj\n<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] \
         /Resources << /Font 7 0 R /XObject << /Nested 6 0 R >> >> /Length {} >>\nstream\n",
        root_body.len()
    )
    .into_bytes();
    root.extend_from_slice(root_body);
    root.extend_from_slice(b"\nendstream\nendobj\n");

    let fonts = (
        7u32,
        b"7 0 obj\n<< /F1 << >> /F2 << >> >>\nendobj\n".to_vec(),
    );
    let malformed_category = (8u32, b"8 0 obj\n1\nendobj\n".to_vec());
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 100 100]",
        "",
        &[(5, root), (6, nested), fonts, malformed_category],
    );
    let mut pdf = open(bytes);
    let form_handle = pdf.get_object_handle(ObjectRef::new(5, 0));
    let mut helper = PageObjectHelper::from_object_handle(form_handle, &mut pdf);
    helper
        .remove_unreferenced_resources()
        .expect("Form resource pruning should cover nested and malformed categories");

    let resources = helper.get_resources(false).unwrap();
    let fonts = resources.get_key(b"/Font");
    assert!(fonts.is_direct());
    assert!(fonts
        .as_dictionary()
        .unwrap()
        .iter()
        .any(|(key, _)| key == b"/F1"));
    assert!(!fonts
        .as_dictionary()
        .unwrap()
        .iter()
        .any(|(key, _)| key == b"/F2"));
}

#[test]
fn form_helper_resource_pruning_preserves_resources_when_a_name_is_unresolved() {
    let body = b"BT /Fmissing 12 Tf (text) Tj ET";
    let mut form = format!(
        "5 0 obj\n<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] \
         /Resources << /Font << /F1 << >> /F2 << >> >> >> /Length {} >>\nstream\n",
        body.len()
    )
    .into_bytes();
    form.extend_from_slice(body);
    form.extend_from_slice(b"\nendstream\nendobj\n");
    let bytes = build_pdf_with_extras("/MediaBox [0 0 100 100]", "", &[(5, form)]);
    let mut pdf = open(bytes);
    let form_handle = pdf.get_object_handle(ObjectRef::new(5, 0));
    let mut helper = PageObjectHelper::from_object_handle(form_handle, &mut pdf);

    helper
        .remove_unreferenced_resources()
        .expect("an unresolved Form resource name is warning-free and must veto pruning");
    let fonts = helper
        .get_resources(false)
        .unwrap()
        .get_key(b"/Font")
        .as_dictionary()
        .unwrap();
    assert!(fonts.iter().any(|(key, _)| key == b"/F1"));
    assert!(fonts.iter().any(|(key, _)| key == b"/F2"));
}

#[test]
fn form_helper_resource_pruning_preserves_resources_for_null_resource_names() {
    let body = b"BT /Fmissing 12 Tf /Fmissing2 12 Tf ET";
    let mut form = format!(
        "5 0 obj\n<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] \
         /Resources << /Font << /Fmissing null /Fmissing2 7 0 R /Funused << >> >> >> \
         /Length {} >>\nstream\n",
        body.len()
    )
    .into_bytes();
    form.extend_from_slice(body);
    form.extend_from_slice(b"\nendstream\nendobj\n");
    let null_object = (7u32, b"7 0 obj\nnull\nendobj\n".to_vec());
    let bytes = build_pdf_with_extras("/MediaBox [0 0 100 100]", "", &[(5, form), null_object]);
    let mut pdf = open(bytes);
    let form_handle = pdf.get_object_handle(ObjectRef::new(5, 0));
    let mut helper = PageObjectHelper::from_object_handle(form_handle, &mut pdf);

    helper
        .remove_unreferenced_resources()
        .expect("null resource entries must not hide unresolved names");
    let fonts = helper
        .get_resources(false)
        .unwrap()
        .get_key(b"/Font")
        .as_dictionary()
        .unwrap();
    assert!(fonts.iter().any(|(key, _)| key == b"/Fmissing"));
    assert!(fonts.iter().any(|(key, _)| key == b"/Fmissing2"));
    assert!(fonts.iter().any(|(key, _)| key == b"/Funused"));
}

#[test]
fn form_helper_resource_pruning_is_a_noop_without_resources() {
    let form_body = b"q Q";
    let mut form = format!(
        "5 0 obj\n<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] /Length {} >>\nstream\n",
        form_body.len()
    )
    .into_bytes();
    form.extend_from_slice(form_body);
    form.extend_from_slice(b"\nendstream\nendobj\n");
    let bytes = build_pdf_with_extras("/MediaBox [0 0 100 100]", "", &[(5, form)]);
    let mut pdf = open(bytes);
    let form_handle = pdf.get_object_handle(ObjectRef::new(5, 0));
    let mut helper = PageObjectHelper::from_object_handle(form_handle, &mut pdf);

    helper
        .remove_unreferenced_resources()
        .expect("a Form without /Resources should be a no-op");
    assert!(helper.get_resources(false).unwrap().is_null());
}

#[test]
fn form_helper_resource_pruning_preserves_resources_on_incomplete_content() {
    let body = b"/F1 12 Tf [";
    let mut form = format!(
        "5 0 obj\n<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] \
         /Resources << /Font << /F1 << >> /F2 << >> >> >> /Length {} >>\nstream\n",
        body.len()
    )
    .into_bytes();
    form.extend_from_slice(body);
    form.extend_from_slice(b"\nendstream\nendobj\n");
    let bytes = build_pdf_with_extras("/MediaBox [0 0 100 100]", "", &[(5, form)]);
    let mut pdf = open(bytes);
    let form_handle = pdf.get_object_handle(ObjectRef::new(5, 0));
    let mut helper = PageObjectHelper::from_object_handle(form_handle, &mut pdf);

    helper
        .remove_unreferenced_resources()
        .expect("incomplete Form content is warning-only");
    let fonts = helper
        .get_resources(false)
        .unwrap()
        .get_key(b"/Font")
        .as_dictionary()
        .unwrap();
    assert_eq!(fonts.len(), 2);
}

#[test]
fn add_page_contents_uses_canonical_handles_and_preserves_order() {
    let first_body = b"Q";
    let (num, extra) = make_stream_object(4, first_body);
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 612 792]",
        "/Contents 4 0 R",
        &[(num, extra)],
    );
    let mut pdf = open(bytes);
    let new_contents =
        ObjectHandle::stream(ObjectHandle::dictionary(Vec::new()), Rc::new(b"q".to_vec()));
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    helper
        .add_page_contents(new_contents.clone(), true)
        .expect("adding a stream should use the live page handle");
    let contents = helper.get_page_contents().unwrap();
    assert_eq!(contents.len(), 2);
    assert!(contents[0].is_same_object_as(&new_contents));
    assert_eq!(contents[1].object_ref(), Some(ObjectRef::new(4, 0)));
}

#[test]
fn page_helper_canonical_content_aliases_cover_page_and_form_routes() {
    let first = make_stream_object(4, b"q");
    let second = make_stream_object(5, b"Q");
    let form_body = b"q Q";
    let form = (
        6u32,
        format!(
            "6 0 obj\n<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] /Resources << >> /Length {} >>\nstream\n",
            form_body.len()
        )
        .into_bytes(),
    );
    let mut form_bytes = form.1;
    form_bytes.extend_from_slice(form_body);
    form_bytes.extend_from_slice(b"\nendstream\nendobj\n");
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 100 100]",
        "/Contents [4 0 R 5 0 R]",
        &[first, second, (6, form_bytes)],
    );
    let mut pdf = open(bytes);

    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let mut callbacks = CountingObjectCallbacks::default();
    helper
        .parse_page_contents(&mut callbacks)
        .expect("page parser alias should use canonical callbacks");
    assert!(callbacks.objects > 0);
    assert_eq!(callbacks.eof_calls, 1);

    let mut filter = PassthroughFilter;
    let mut filtered = Vec::new();
    let mut sink = PlString::new("page helper filter", None, &mut filtered);
    helper
        .filter_page_contents(&mut filter, Some(&mut sink))
        .expect("page filter alias should use the canonical page route");
    assert!(!filtered.is_empty());

    let mut piped = Vec::new();
    let mut pipeline = PlString::new("page helper pipe", None, &mut piped);
    helper
        .pipe_page_contents(&mut pipeline)
        .expect("page pipe alias should decode canonical page contents");
    assert_eq!(piped, b"q\nQ");

    helper
        .coalesce_content_streams()
        .expect("page coalesce wrapper should retain a canonical provider");
    helper
        .rotate_page(90, false)
        .expect("page rotate wrapper should mutate the live page");
    assert_eq!(helper.rotate().unwrap(), 90);

    helper
        .add_content_token_filter(Rc::new(RefCell::new(PassthroughFilter)))
        .expect("page token-filter wrapper should attach lazily");
    drop(helper);

    let form_handle = pdf.get_object_handle(ObjectRef::new(6, 0));
    let mut form_helper = PageObjectHelper::from_object_handle(form_handle, &mut pdf);
    let mut form_callbacks = CountingObjectCallbacks::default();
    form_helper
        .parse_contents(&mut form_callbacks)
        .expect("Form parser should use its content-stream route");
    assert!(form_callbacks.objects > 0);

    let mut form_filter = PassthroughFilter;
    form_helper
        .filter_contents(&mut form_filter, None)
        .expect("Form filter should use stream-content decoding");
    let mut form_filter_alias = PassthroughFilter;
    form_helper
        .filter_page_contents(&mut form_filter_alias, None)
        .expect("old Form filter alias should share the canonical route");

    let mut form_output = Vec::new();
    let mut form_pipeline = PlString::new("Form helper pipe", None, &mut form_output);
    form_helper
        .pipe_contents(&mut form_pipeline)
        .expect("Form pipe should decode the stream data");
    assert_eq!(form_output, b"q Q");

    let mut form_output_alias = Vec::new();
    let mut form_pipeline_alias =
        PlString::new("Form helper pipe alias", None, &mut form_output_alias);
    form_helper
        .pipe_page_contents(&mut form_pipeline_alias)
        .expect("old Form pipe alias should share the canonical route");
    assert_eq!(form_output_alias, b"q Q");

    form_helper
        .add_content_token_filter(Rc::new(RefCell::new(PassthroughFilter)))
        .expect("Form token-filter wrapper should attach lazily");
}

#[test]
fn form_pipe_reports_a_provider_failure_through_the_helper_route() {
    let mut pdf = open(build_single_page_pdf("/MediaBox [0 0 100 100]", ""));
    let form = pdf.new_stream().unwrap();
    let dict = form.as_stream_dict().unwrap();
    dict.replace_key(b"/Type", ObjectHandle::name(b"XObject".to_vec()))
        .unwrap();
    dict.replace_key(b"/Subtype", ObjectHandle::name(b"Form".to_vec()))
        .unwrap();
    dict.replace_key(
        b"/BBox",
        ObjectHandle::array(vec![
            ObjectHandle::integer(0),
            ObjectHandle::integer(0),
            ObjectHandle::integer(10),
            ObjectHandle::integer(10),
        ]),
    )
    .unwrap();
    form.replace_stream_data_with_retry_callback(|_, _, _| Ok(false), None, None)
        .unwrap();

    let mut helper = PageObjectHelper::from_object_handle(form, &mut pdf);
    let mut output = Vec::new();
    let mut pipeline = PlString::new("failing Form content", None, &mut output);
    let error = helper
        .pipe_contents(&mut pipeline)
        .expect_err("failed Form providers should cross PageObjectHelper");
    assert!(error
        .to_string()
        .contains("errors while decoding content stream"));
}

#[test]
fn content_stream_objects_concatenates_array_contents() {
    // Two-element /Contents array — objects from both streams appear.
    let body1 = b"q";
    let body2 = b"Q";
    let extra1 = make_stream_object(4, body1);
    let extra2 = make_stream_object(5, body2);
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 612 792]",
        "/Contents [4 0 R 5 0 R]",
        &[extra1, extra2],
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let objects = helper.content_stream_objects().unwrap();
    assert_eq!(
        objects,
        vec![
            Object::Operator(b"q".to_vec()),
            Object::Operator(b"Q".to_vec())
        ]
    );
}

#[test]
fn content_stream_objects_preserves_inline_image_as_a_separate_event() {
    let body = b"BI /W 1 /H 1 ID x EI Q";
    let (num, extra) = make_stream_object(4, body);
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 612 792]",
        "/Contents 4 0 R",
        &[(num, extra)],
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    assert_eq!(
        helper.content_stream_objects().unwrap(),
        vec![
            Object::Operator(b"BI".to_vec()),
            Object::Name(b"W".to_vec()),
            Object::Integer(1),
            Object::Name(b"H".to_vec()),
            Object::Integer(1),
            Object::Operator(b"ID".to_vec()),
            Object::InlineImage(b"x ".to_vec()),
            Object::Operator(b"EI".to_vec()),
            Object::Operator(b"Q".to_vec()),
        ]
    );
}

#[test]
fn externalize_inline_images_uses_canonical_stream_and_resource_handles() {
    let (num, extra) = make_stream_object(4, b"BI /W 1 /H 1 /BPC 8 /CS /RGB ID abc EI Q");
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 612 792] /Resources << /XObject << >> >>",
        "/Contents 4 0 R",
        &[(num, extra)],
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    helper
        .externalize_inline_images(0, true)
        .expect("inline image should be externalized");

    let contents = helper
        .content_stream_objects()
        .expect("rewritten page contents should parse");
    assert_eq!(
        contents,
        vec![
            Object::Name(b"IIm1".to_vec()),
            Object::Operator(b"Do".to_vec()),
            Object::Operator(b"Q".to_vec()),
        ]
    );

    let images = helper
        .get_images()
        .expect("externalized image should be in page resources");
    let image = images
        .get(b"/IIm1".as_slice())
        .expect("qpdf's first inline-image name is /IIm1");
    assert_eq!(image.as_stream_data().unwrap().as_slice(), b"abc ");
    let dict = image.as_stream_dict().unwrap();
    assert_eq!(dict.get_key(b"/Type").as_name(), Some(b"XObject".to_vec()));
    assert_eq!(dict.get_key(b"/Subtype").as_name(), Some(b"Image".to_vec()));
    assert_eq!(
        dict.get_key(b"/ColorSpace").as_name(),
        Some(b"DeviceRGB".to_vec())
    );
    assert_eq!(dict.get_key(b"/Width").as_integer(), Some(1));
    assert_eq!(dict.get_key(b"/Height").as_integer(), Some(1));
}

#[test]
fn externalize_inline_images_warns_for_unknown_named_colorspace() {
    let (num, extra) = make_stream_object(4, b"BI /W 1 /H 1 /CS /Missing ID abc EI");
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 612 792] /Resources << /XObject << >> >>",
        "/Contents 4 0 R",
        &[(num, extra)],
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    helper
        .externalize_inline_images(0, true)
        .expect("unknown colorspace should be warning-only");
    drop(helper);

    assert!(pdf
        .repair_diagnostics()
        .entries()
        .iter()
        .any(|diagnostic| diagnostic
            .message
            .contains("unable to resolve colorspace /Missing")));
}

#[test]
fn externalize_inline_images_warns_and_preserves_content_on_filter_error() {
    let malformed = (
        4u32,
        b"4 0 obj\n<< /Length 3 /Filter /ASCIIHexDecode >>\nstream\n0g\nendstream\nendobj\n"
            .to_vec(),
    );
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 612 792] /Resources << /XObject << >> >>",
        "/Contents 4 0 R",
        &[malformed],
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    helper
        .externalize_inline_images(0, true)
        .expect("qpdf's filter failure is warning-only for externalization");
    assert!(helper.get_images().unwrap().is_empty());
    drop(helper);
    assert!(pdf
        .repair_diagnostics()
        .entries()
        .iter()
        .any(|diagnostic| diagnostic
            .message
            .contains("Unable to filter content stream")));
}

#[test]
fn externalize_inline_images_respects_minimum_payload_size() {
    let (num, extra) = make_stream_object(4, b"BI /W 1 /H 1 ID x EI");
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 612 792] /Resources << /XObject << >> >>",
        "/Contents 4 0 R",
        &[(num, extra)],
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    helper
        .externalize_inline_images(3, true)
        .expect("small inline image should be retained without error");

    assert!(helper.get_images().unwrap().is_empty());
    assert!(helper
        .content_stream_objects()
        .unwrap()
        .iter()
        .any(|object| matches!(object, Object::InlineImage(_))));
}

#[test]
fn externalize_inline_images_recurses_through_form_content_handles() {
    let form_body = b"BI /W 1 /H 1 /BPC 8 /CS /RGB ID abc EI Q";
    let form = (
        5u32,
        format!(
            "5 0 obj\n<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] \
             /Resources << /XObject << >> >> /Length {} >>\nstream\n",
            form_body.len()
        )
        .into_bytes(),
    );
    let mut form_bytes = form.1;
    form_bytes.extend_from_slice(form_body);
    form_bytes.extend_from_slice(b"\nendstream\nendobj\n");
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 100 100] /Resources << /XObject << /NestedForm 5 0 R >> >>",
        "",
        &[(5, form_bytes)],
    );
    let mut pdf = open(bytes);

    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    helper
        .externalize_inline_images(0, false)
        .expect("recursive externalization should process Form content");
    drop(helper);

    let form_handle = pdf.get_object_handle(ObjectRef::new(5, 0));
    let mut form_helper = PageObjectHelper::from_object_handle(form_handle, &mut pdf);
    assert_eq!(
        form_helper.content_stream_objects().unwrap(),
        vec![
            Object::Name(b"IIm1".to_vec()),
            Object::Operator(b"Do".to_vec()),
            Object::Operator(b"Q".to_vec()),
        ]
    );
    let images = form_helper.get_images().unwrap();
    assert_eq!(images.len(), 1);
}

// ---------------------------------------------------------------------------
// resources()
// ---------------------------------------------------------------------------

#[test]
fn resources_returns_direct_resources_on_page() {
    // /Resources directly on the leaf page.
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792]", "/Resources << /Font << >> >>");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let res = helper.resources().unwrap();
    assert!(res.is_some(), "expected /Resources on leaf page");
    assert!(res.unwrap().get("Font").is_some(), "expected /Font key");
}

#[test]
fn resources_inherits_from_parent() {
    // /Resources only on the /Pages node — must be inherited.
    let bytes = build_single_page_pdf(
        "/MediaBox [0 0 612 792] /Resources << /ProcSet [/PDF] >>",
        "",
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let res = helper.resources().unwrap();
    assert!(res.is_some(), "expected inherited /Resources from parent");
    assert!(
        res.unwrap().get("ProcSet").is_some(),
        "expected /ProcSet in inherited Resources"
    );
}

#[test]
fn get_resources_returns_the_live_inherited_handle() {
    let resources = (4u32, b"4 0 obj\n<< /ProcSet [/PDF] >>\nendobj\n".to_vec());
    let bytes = build_pdf_with_extras("/MediaBox [0 0 612 792] /Resources 4 0 R", "", &[resources]);
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    let handle = helper
        .get_resources(false)
        .expect("inherited resources should resolve");
    assert_eq!(handle.object_ref(), Some(ObjectRef::new(4, 0)));
    assert!(handle.as_dictionary().is_some());
}

#[test]
fn resources_returns_none_when_absent() {
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792]", "");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let res = helper.resources().unwrap();
    assert!(
        res.is_none(),
        "expected Ok(None) when no /Resources anywhere"
    );
}

#[test]
fn resources_rejects_a_non_dictionary_value() {
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792]", "/Resources 42");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    let error = helper
        .resources()
        .expect_err("resources() must reject a non-dictionary terminal value");
    assert!(error.to_string().contains("unexpected type integer"));
}

// ---------------------------------------------------------------------------
// rotate() — getter only
// ---------------------------------------------------------------------------

#[test]
fn rotate_returns_direct_rotate_on_page() {
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792]", "/Rotate 90");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    assert_eq!(helper.rotate().unwrap(), 90);
}

#[test]
fn rotate_inherits_from_parent() {
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792] /Rotate 180", "");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    assert_eq!(helper.rotate().unwrap(), 180);
}

#[test]
fn rotate_inherits_indirect_integer_from_parent() {
    let rotate = (4u32, b"4 0 obj\n270\nendobj\n".to_vec());
    let bytes = build_pdf_with_extras("/Rotate 4 0 R /MediaBox [0 0 612 792]", "", &[rotate]);
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    assert_eq!(helper.rotate().unwrap(), 270);
}

#[test]
fn rotate_defaults_to_zero_when_absent() {
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792]", "");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    assert_eq!(helper.rotate().unwrap(), 0);
}

/// Round-trip: apply_rotate_to_pages materializes inherited rotation on the
/// leaf; PageObjectHelper::rotate() should then read the materialized value
/// after write + re-open.
#[test]
fn rotate_round_trip_after_mutation() {
    // Parent has /Rotate 90; leaf has none.  Add 90 → leaf should become 180.
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792] /Rotate 90", "");
    let mut pdf = open(bytes);
    let page_ref = ObjectRef::new(3, 0);

    let op = RotateOp {
        mode: RotateMode::Add,
        degrees: 90,
    };
    apply_rotate_to_pages(&mut pdf, &[page_ref], &op).unwrap();

    // Serialize and re-open.
    let mut serialized: Vec<u8> = Vec::new();
    write_default(&mut pdf, &mut serialized).unwrap();
    let mut pdf2 = open(serialized);

    let page_refs = pages::page_refs(&mut pdf2).unwrap();
    let mut helper = PageObjectHelper::new(page_refs[0], &mut pdf2);
    assert_eq!(
        helper.rotate().unwrap(),
        180,
        "materialized rotation must be readable after round-trip"
    );
}

// ---------------------------------------------------------------------------
// get_annotations()
// ---------------------------------------------------------------------------

#[test]
fn get_annotations_empty_when_no_annots() {
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792]", "");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let annots = helper.get_annotations().unwrap();
    assert!(annots.is_empty(), "expected no annotations");
}

#[test]
fn get_annotations_returns_refs() {
    // Build a PDF with an /Annots array containing two indirect refs.
    // We re-use object numbers 4 and 5 for the annotation dicts.
    let annot4 = (
        4u32,
        b"4 0 obj\n<< /Type /Annot /Subtype /Text >>\nendobj\n".to_vec(),
    );
    let annot5 = (
        5u32,
        b"5 0 obj\n<< /Type /Annot /Subtype /Link >>\nendobj\n".to_vec(),
    );
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 612 792]",
        "/Annots [4 0 R 5 0 R]",
        &[annot4, annot5],
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let annots = helper.get_annotations().unwrap();
    assert_eq!(annots.len(), 2, "expected 2 annotation refs");
    assert_eq!(annots[0], ObjectRef::new(4, 0));
    assert_eq!(annots[1], ObjectRef::new(5, 0));
}

#[test]
fn get_annotations_filtered_by_subtype_uses_canonical_annotation_handles() {
    let annot4 = (
        4u32,
        b"4 0 obj\n<< /Type /Annot /Subtype /Text >>\nendobj\n".to_vec(),
    );
    let annot5 = (
        5u32,
        b"5 0 obj\n<< /Type /Annot /Subtype /Link >>\nendobj\n".to_vec(),
    );
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 612 792]",
        "/Annots [4 0 R 5 0 R]",
        &[annot4, annot5],
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    assert_eq!(
        helper
            .get_annotations_filtered(Some(b"Text"))
            .expect("filtered annotation enumeration should resolve handles"),
        vec![ObjectRef::new(4, 0)]
    );
}

#[test]
fn get_annotation_handles_preserves_direct_annotations() {
    let annot4 = (
        4u32,
        b"4 0 obj\n<< /Type /Annot /Subtype /Link >>\nendobj\n".to_vec(),
    );
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 612 792]",
        "/Annots [<< /Type /Annot /Subtype /Text >> 4 0 R]",
        &[annot4],
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    let handles = helper
        .get_annotation_handles(None)
        .expect("qpdf annotation enumeration should retain direct handles");
    assert_eq!(handles.len(), 2);
    assert!(handles[0].is_direct());
    assert_eq!(
        handles[0].get_key(b"/Subtype").as_name(),
        Some(b"Text".to_vec())
    );
    assert_eq!(handles[1].object_ref(), Some(ObjectRef::new(4, 0)));
}

#[test]
fn get_annotation_handles_skips_non_dictionary_members() {
    let scalar = (5u32, b"5 0 obj\n7\nendobj\n".to_vec());
    let bytes = build_pdf_with_extras("/MediaBox [0 0 612 792]", "/Annots [5 0 R]", &[scalar]);
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    assert!(helper
        .get_annotation_handles(None)
        .expect("malformed annotation members are skipped")
        .is_empty());
}

#[test]
fn get_annotations_resolves_indirect_array() {
    let annot4 = (
        4u32,
        b"4 0 obj\n<< /Type /Annot /Subtype /Text >>\nendobj\n".to_vec(),
    );
    let annot_array = (5u32, b"5 0 obj\n[4 0 R]\nendobj\n".to_vec());
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 612 792]",
        "/Annots 5 0 R",
        &[annot4, annot_array],
    );
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    let annots = helper.get_annotations().unwrap();

    assert_eq!(annots, vec![ObjectRef::new(4, 0)]);
}

#[test]
fn get_annotations_follows_holder_chain() {
    // /Annots is stored behind a two-hop holder chain:
    //   page /Annots -> 5 0 R -> 6 0 R -> [4 0 R]
    // A single resolve hop would stop at the intermediate reference 6 0 R
    // (not an array) and error; the chain must be followed to the terminal.
    let annot4 = (
        4u32,
        b"4 0 obj\n<< /Type /Annot /Subtype /Text >>\nendobj\n".to_vec(),
    );
    let carrier = (5u32, b"5 0 obj\n6 0 R\nendobj\n".to_vec());
    let annot_array = (6u32, b"6 0 obj\n[4 0 R]\nendobj\n".to_vec());
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 612 792]",
        "/Annots 5 0 R",
        &[annot4, carrier, annot_array],
    );
    let mut pdf = open(bytes);
    pdf.set_object(
        ObjectRef::new(5, 0),
        Object::Reference(ObjectRef::new(6, 0)),
    );
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    let annots = helper.get_annotations().unwrap();

    assert_eq!(annots, vec![ObjectRef::new(4, 0)]);
}

#[test]
fn get_annotations_reference_terminal_not_array_errors() {
    // /Annots is an indirect reference whose terminal is NOT an array.
    // The chain is followed to object 5 (a dictionary), and the helper must
    // surface the specific "does not resolve to an array" error rather than a
    // generic failure.
    let non_array = (5u32, b"5 0 obj\n<< >>\nendobj\n".to_vec());
    let bytes = build_pdf_with_extras("/MediaBox [0 0 612 792]", "/Annots 5 0 R", &[non_array]);
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    match helper.get_annotations() {
        Err(Error::Unsupported(msg)) => {
            assert!(
                msg.contains("does not resolve to an array"),
                "expected 'does not resolve to an array' message, got: {msg}"
            );
        }
        other => panic!("expected Error::Unsupported, got {other:?}"),
    }
}

#[test]
fn get_annotations_chain_terminal_not_array_errors() {
    // /Annots is stored behind a two-hop holder chain whose terminal is NOT an
    // array:
    //   page /Annots -> 5 0 R -> 6 0 R -> << >>
    // The chain must be followed past the intermediate reference to its
    // non-array terminal, then surface the specific error. A single resolve hop
    // would stop at 6 0 R (still a reference) and never reach the dictionary.
    let carrier = (5u32, b"5 0 obj\n6 0 R\nendobj\n".to_vec());
    let non_array = (6u32, b"6 0 obj\n<< >>\nendobj\n".to_vec());
    let bytes = build_pdf_with_extras(
        "/MediaBox [0 0 612 792]",
        "/Annots 5 0 R",
        &[carrier, non_array],
    );
    let mut pdf = open(bytes);
    pdf.set_object(
        ObjectRef::new(5, 0),
        Object::Reference(ObjectRef::new(6, 0)),
    );
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    match helper.get_annotations() {
        Err(Error::Unsupported(msg)) => {
            assert!(
                msg.contains("does not resolve to an array"),
                "expected 'does not resolve to an array' message, got: {msg}"
            );
        }
        other => panic!("expected Error::Unsupported, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// media_box() — inheritable
// ---------------------------------------------------------------------------

#[test]
fn media_box_on_leaf_page() {
    let bytes = build_single_page_pdf("", "/MediaBox [0 0 612 792]");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let mb = helper.media_box().unwrap();
    let mb = mb.expect("expected /MediaBox on leaf page");
    assert_eq!(mb, PageBox::new(0.0, 0.0, 612.0, 792.0));
}

#[test]
fn media_box_inherited_from_parent() {
    // /MediaBox only on the /Pages node — must be inherited.
    let bytes = build_single_page_pdf("/MediaBox [0 0 595 842]", "");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let mb = helper.media_box().unwrap();
    let mb = mb.expect("expected inherited /MediaBox");
    assert_eq!(mb, PageBox::new(0.0, 0.0, 595.0, 842.0));
}

#[test]
fn media_box_inherits_indirect_array_from_parent() {
    let rect = (4u32, b"4 0 obj\n[0 0 400 500]\nendobj\n".to_vec());
    let bytes = build_pdf_with_extras("/MediaBox 4 0 R", "", &[rect]);
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    let mb = helper.media_box().unwrap().expect("expected /MediaBox");

    assert_eq!(mb, PageBox::new(0.0, 0.0, 400.0, 500.0));
}

#[test]
fn get_attribute_uses_live_inherited_handle_and_copies_when_requested() {
    let rect = (4u32, b"4 0 obj\n[0 0 400 500]\nendobj\n".to_vec());
    let bytes = build_pdf_with_extras("/MediaBox 4 0 R", "", &[rect]);
    let mut pdf = open(bytes);
    let page_ref = ObjectRef::new(3, 0);

    let inherited = {
        let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
        helper
            .get_attribute(b"/MediaBox", false)
            .expect("inherited attribute should resolve")
    };
    assert_eq!(
        inherited.object_ref(),
        Some(ObjectRef::new(4, 0)),
        "copy_if_shared=false must retain the inherited indirect identity"
    );

    let copied = {
        let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
        helper
            .get_attribute(b"/MediaBox", true)
            .expect("copying an inherited attribute should resolve")
    };
    assert!(
        copied.is_direct(),
        "copy_if_shared=true must return the shallow copy, not the shared indirect value"
    );
    assert_eq!(copied.as_array().expect("copied box array").len(), 4);

    let page = pdf.get_object_handle(page_ref);
    pdf.resolve_object_handle(&page).unwrap();
    let page_media_box = page.get_key(b"/MediaBox");
    pdf.resolve_object_handle(&page_media_box).unwrap();
    assert!(
        page_media_box.is_direct(),
        "copy_if_shared=true must materialize the copied value on the page"
    );
}

#[test]
fn get_crop_box_copies_a_media_box_fallback_when_requested() {
    let bytes = build_single_page_pdf("/MediaBox [0 0 400 500]", "");
    let mut pdf = open(bytes);
    let page_ref = ObjectRef::new(3, 0);

    let crop = {
        let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
        helper
            .get_crop_box(false, true)
            .expect("CropBox fallback should resolve")
    };
    assert!(crop.is_direct(), "fallback copy must be a direct handle");

    let page = pdf.get_object_handle(page_ref);
    pdf.resolve_object_handle(&page).unwrap();
    let page_crop_box = page.get_key(b"/CropBox");
    pdf.resolve_object_handle(&page_crop_box).unwrap();
    assert!(
        page_crop_box.is_direct(),
        "copy_if_fallback=true must materialize /CropBox on the page"
    );
    assert_eq!(page_crop_box.as_array().unwrap().len(), 4);
}

#[test]
fn media_box_leaf_overrides_parent() {
    // Parent has A4; leaf has letter — leaf must win.
    let bytes = build_single_page_pdf("/MediaBox [0 0 595 842]", "/MediaBox [0 0 612 792]");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let mb = helper.media_box().unwrap().expect("expected /MediaBox");
    assert_eq!(mb.urx, 612.0, "leaf MediaBox must override parent");
}

#[test]
fn media_box_absent_returns_none() {
    let bytes = build_single_page_pdf("", "");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    assert!(helper.media_box().unwrap().is_none(), "expected None");
}

// ---------------------------------------------------------------------------
// crop_box() — inheritable, defaults to media_box
// ---------------------------------------------------------------------------

#[test]
fn crop_box_explicit_on_leaf() {
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792]", "/CropBox [10 10 600 780]");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let cb = helper.crop_box().unwrap().expect("expected /CropBox");
    assert_eq!(cb, PageBox::new(10.0, 10.0, 600.0, 780.0));
}

#[test]
fn crop_box_defaults_to_media_box_when_absent() {
    // No /CropBox anywhere — should fall back to /MediaBox.
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792]", "");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let cb = helper
        .crop_box()
        .unwrap()
        .expect("expected fallback to MediaBox");
    assert_eq!(cb, PageBox::new(0.0, 0.0, 612.0, 792.0));
}

// ---------------------------------------------------------------------------
// bleed_box / trim_box / art_box — leaf-only, fall back to crop_box
// ---------------------------------------------------------------------------

#[test]
fn bleed_box_explicit_on_leaf() {
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792]", "/BleedBox [5 5 607 787]");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let bb = helper.bleed_box().unwrap().expect("expected /BleedBox");
    assert_eq!(bb.llx, 5.0);
    assert_eq!(bb.lly, 5.0);
}

#[test]
fn bleed_box_resolves_indirect_leaf_array() {
    let rect = (4u32, b"4 0 obj\n[5 6 607 787]\nendobj\n".to_vec());
    let bytes = build_pdf_with_extras("/MediaBox [0 0 612 792]", "/BleedBox 4 0 R", &[rect]);
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);

    let bb = helper.bleed_box().unwrap().expect("expected /BleedBox");

    assert_eq!(bb, PageBox::new(5.0, 6.0, 607.0, 787.0));
}

#[test]
fn bleed_box_falls_back_to_crop_box() {
    // No BleedBox, CropBox [10 10 600 780] → bleed_box() == crop_box().
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792]", "/CropBox [10 10 600 780]");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let bb = helper
        .bleed_box()
        .unwrap()
        .expect("expected fallback bleed_box");
    assert_eq!(bb, PageBox::new(10.0, 10.0, 600.0, 780.0));
}

#[test]
fn trim_box_falls_back_to_media_box_when_no_crop_box() {
    // No TrimBox, no CropBox → falls back all the way to MediaBox.
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792]", "");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let tb = helper
        .trim_box()
        .unwrap()
        .expect("expected fallback trim_box");
    assert_eq!(tb, PageBox::new(0.0, 0.0, 612.0, 792.0));
}

#[test]
fn art_box_falls_back_to_crop_box() {
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792]", "/CropBox [20 20 590 770]");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(3, 0), &mut pdf);
    let ab = helper
        .art_box()
        .unwrap()
        .expect("expected fallback art_box");
    assert_eq!(ab, PageBox::new(20.0, 20.0, 590.0, 770.0));
}

/// Box inheritance round-trip: set MediaBox via set_object, write, re-open,
/// read back via PageObjectHelper.
#[test]
fn media_box_round_trip_after_mutation() {
    // Start with /MediaBox only on parent.
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792]", "");
    let mut pdf = open(bytes);

    // Materialize a different MediaBox directly on the leaf page.
    let page_obj = pdf.resolve(ObjectRef::new(3, 0)).unwrap();
    let Object::Dictionary(mut page_dict) = page_obj else {
        panic!("expected page dict")
    };
    page_dict.insert(
        "MediaBox",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(500),
            Object::Integer(700),
        ]),
    );
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page_dict));

    // Serialize and re-open.
    let mut serialized: Vec<u8> = Vec::new();
    write_default(&mut pdf, &mut serialized).unwrap();
    let mut pdf2 = open(serialized);

    let page_refs = pages::page_refs(&mut pdf2).unwrap();
    let mut helper = PageObjectHelper::new(page_refs[0], &mut pdf2);
    let mb = helper
        .media_box()
        .unwrap()
        .expect("expected MediaBox after round-trip");
    assert_eq!(mb.urx, 500.0, "updated MediaBox must survive round-trip");
    assert_eq!(mb.ury, 700.0, "updated MediaBox must survive round-trip");
}

// ---------------------------------------------------------------------------
// PageBox type
// ---------------------------------------------------------------------------

#[test]
fn page_box_fields_are_accessible() {
    let b = PageBox::new(1.0, 2.0, 3.0, 4.0);
    assert_eq!(b.llx, 1.0);
    assert_eq!(b.lly, 2.0);
    assert_eq!(b.urx, 3.0);
    assert_eq!(b.ury, 4.0);
}

// ---------------------------------------------------------------------------
// Regression: accessors must reject a non-leaf /Type /Pages node
// ---------------------------------------------------------------------------

#[test]
fn accessors_reject_pages_tree_node() {
    // Object 2 0 R is the `/Type /Pages` node (not a leaf `/Page`).
    let bytes = build_single_page_pdf("/MediaBox [0 0 612 792]", "");
    let mut pdf = open(bytes);
    let mut helper = PageObjectHelper::new(ObjectRef::new(2, 0), &mut pdf);

    assert!(
        helper.media_box().is_err(),
        "media_box() must reject a /Pages node"
    );
    assert!(
        helper.rotate().is_err(),
        "rotate() must reject a /Pages node"
    );
    assert!(
        helper.resources().is_err(),
        "resources() must reject a /Pages node"
    );
    assert!(
        helper.get_annotations().is_err(),
        "get_annotations() must reject a /Pages node"
    );
    assert!(
        helper.content_stream_objects().is_err(),
        "content_stream_objects() must reject a /Pages node"
    );
}
