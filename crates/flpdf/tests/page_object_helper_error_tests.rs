//! Error-path and edge-case tests for [`flpdf::PageObjectHelper`].
//!
//! The happy paths (content streams, resources, rotate, annotations, and the
//! bounding-box accessors) live in `page_object_helper_tests.rs`. This file
//! targets the malformed-input branches: bad `/Annots` shapes, malformed
//! rectangle arrays, `/Parent`-chain anomalies (cycle, non-dictionary node,
//! non-reference parent), and the leaf-only box accessors' error arms.
//!
//! All PDFs are built in memory. The builder gives full control over every
//! indirect object — including each page's `/Parent` — which the shared
//! single-page builder does not, so the parent-chain branches are reachable.

use flpdf::{
    ContentToken, Matrix, ObjectHandle, ObjectRef, PageObjectHelper, Pdf, PipelineResult,
    Rectangle, TokenFilter, TokenFilterOutput,
};
use std::io::Cursor;
use std::rc::Rc;

mod common;
use common::build_pdf;

fn open(bytes: Vec<u8>) -> Pdf<Cursor<Vec<u8>>> {
    Pdf::open(Cursor::new(bytes)).expect("PDF should parse")
}

/// Build a minimal Catalog + Pages + single Page, with the page body supplied
/// verbatim. `page_body` is the full `<< ... >>` dictionary for object 3.
fn single_page(page_body: &str, extras: &[(u32, String)]) -> Vec<u8> {
    let mut objects = vec![
        (1u32, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
        (
            2u32,
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        ),
        (3u32, page_body.to_string()),
    ];
    objects.extend(extras.iter().cloned());
    build_pdf(&objects, 1)
}

fn helper_for(bytes: Vec<u8>) -> (Pdf<Cursor<Vec<u8>>>, ObjectRef) {
    (open(bytes), ObjectRef::new(3, 0))
}

fn page_helper_for_ref(
    pdf: &mut Pdf<Cursor<Vec<u8>>>,
    page_ref: ObjectRef,
) -> PageObjectHelper<'_, Cursor<Vec<u8>>> {
    let page = pdf.get_object_handle(page_ref);
    PageObjectHelper::from_object_handle(page, pdf)
}

#[derive(Default)]
struct CountingTokenFilter {
    tokens: usize,
    eof_calls: usize,
}

impl TokenFilter for CountingTokenFilter {
    fn handle_token(
        &mut self,
        token: &ContentToken,
        output: &mut TokenFilterOutput<'_>,
    ) -> PipelineResult<()> {
        self.tokens += 1;
        output.write_token(token)
    }

    fn handle_eof(&mut self, output: &mut TokenFilterOutput<'_>) -> PipelineResult<()> {
        self.eof_calls += 1;
        output.write(b"eof")
    }
}

#[test]
fn content_helpers_use_qpdf_default_pipeline_and_inline_image_options() {
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /Resources << >> /Contents 4 0 R >>",
        &[(4, "<< /Length 6 >>\nstream\n1 2 cm\nendstream".into())],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let original_contents = pdf
        .get_object_handle(page_ref)
        .try_get_key(b"/Contents")
        .unwrap();
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);

    let mut filter = CountingTokenFilter::default();
    helper.filter_contents(&mut filter).unwrap();
    assert!(filter.tokens > 0);
    assert!(filter.eof_calls > 0);

    let mut old_name_filter = CountingTokenFilter::default();
    helper.filter_page_contents(&mut old_name_filter).unwrap();
    assert!(old_name_filter.tokens > 0);
    assert!(old_name_filter.eof_calls > 0);

    helper.externalize_inline_images().unwrap();
    drop(helper);

    let page = pdf.get_object_handle(page_ref);
    assert!(page
        .try_get_key(b"/Contents")
        .unwrap()
        .is_same_object_as(&original_contents));
}

#[test]
fn transform_and_placement_helpers_use_qpdf_default_options() {
    let mut pdf = Pdf::empty().unwrap();
    let page = ObjectHandle::dictionary(vec![
        (
            b"/MediaBox".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::integer(0),
                ObjectHandle::integer(0),
                ObjectHandle::integer(100),
                ObjectHandle::integer(200),
            ]),
        ),
        (b"/Rotate".to_vec(), ObjectHandle::integer(90)),
    ]);
    let page = pdf.make_indirect_object_handle(page).unwrap();
    let mut helper = PageObjectHelper::from_object_handle(page, &mut pdf);

    assert_eq!(
        helper.get_matrix_for_transformations().unwrap(),
        Matrix::new(0.0, -1.0, 1.0, 0.0, 0.0, 100.0)
    );
    let form = helper.get_form_xobject_for_page().unwrap();
    assert!(!form
        .as_stream_dict()
        .unwrap()
        .try_get_key(b"/Matrix")
        .unwrap()
        .try_is_null()
        .unwrap());

    let rect = Rectangle::new(0.0, 0.0, 300.0, 300.0);
    let default_matrix = helper
        .get_matrix_for_form_xobject_placement(form.clone(), rect)
        .unwrap();
    let explicit_matrix = helper
        .get_matrix_for_form_xobject_placement_with_options(form.clone(), rect, true, true, false)
        .unwrap();
    assert_eq!(default_matrix, explicit_matrix);

    let (default_content, default_matrix) = helper
        .place_form_xobject(form.clone(), "/Selected", rect)
        .unwrap();
    let (explicit_content, explicit_matrix) = helper
        .place_form_xobject_with_options(form.clone(), "/Selected", rect, true, true, false)
        .unwrap();
    assert_eq!(default_content, explicit_content);
    assert_eq!(default_matrix, explicit_matrix);

    let mut default_output = Matrix::default();
    let default_content = helper
        .place_form_xobject_with_matrix(form.clone(), "/Selected", rect, &mut default_output)
        .unwrap();
    let mut explicit_output = Matrix::default();
    let explicit_content = helper
        .place_form_xobject_with_matrix_with_options(
            form,
            "/Selected",
            rect,
            &mut explicit_output,
            true,
            true,
            false,
        )
        .unwrap();
    assert_eq!(default_content, explicit_content);
    assert_eq!(default_output, explicit_output);
}

#[test]
fn page_box_getters_use_qpdf_default_copy_options() {
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /MediaBox 5 0 R >>",
        &[(5, "[0 0 20 30]".into())],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);

    let media = helper.get_media_box().unwrap();
    assert_eq!(media.object_ref(), Some(ObjectRef::new(5, 0)));
    for fallback in [
        helper.get_crop_box().unwrap(),
        helper.get_bleed_box().unwrap(),
        helper.get_trim_box().unwrap(),
        helper.get_art_box().unwrap(),
    ] {
        assert!(fallback.is_same_object_as(&media));
    }

    drop(helper);
    let page = pdf.get_object_handle(page_ref);
    for key in [
        b"/CropBox".as_slice(),
        b"/BleedBox",
        b"/TrimBox",
        b"/ArtBox",
    ] {
        assert!(page.try_get_key(key).unwrap().try_is_null().unwrap());
    }
}

#[test]
fn media_box_copy_option_copies_an_inherited_direct_value_like_qpdf() {
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (
                2,
                "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 20 30] >>".into(),
            ),
            (3, "<< /Type /Page /Parent 2 0 R >>".into()),
        ],
        1,
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let parent = pdf.get_object_handle(ObjectRef::new(2, 0));
    let original = parent.try_get_key(b"/MediaBox").unwrap();
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);

    let media = helper.get_media_box_with_options(true).unwrap();
    assert!(!media.is_same_object_as(&original));
    drop(helper);

    let page = pdf.get_object_handle(page_ref);
    let copied = page.try_get_key(b"/MediaBox").unwrap();
    assert!(copied.is_same_object_as(&media));
    assert!(parent
        .try_get_key(b"/MediaBox")
        .unwrap()
        .is_same_object_as(&original));
}

#[test]
fn media_box_copy_option_copies_an_indirect_value_like_qpdf() {
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /MediaBox 5 0 R >>",
        &[(5, "[0 0 20 30]".into())],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let original = pdf
        .get_object_handle(page_ref)
        .try_get_key(b"/MediaBox")
        .unwrap();
    assert_eq!(original.object_ref(), Some(ObjectRef::new(5, 0)));
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);

    let media = helper.get_media_box_with_options(true).unwrap();
    assert_eq!(media.object_ref(), None);
    assert!(!media.is_same_object_as(&original));
    drop(helper);

    let page = pdf.get_object_handle(page_ref);
    let copied = page.try_get_key(b"/MediaBox").unwrap();
    assert!(copied.is_same_object_as(&media));
    assert_eq!(original.object_ref(), Some(ObjectRef::new(5, 0)));
}

#[test]
fn page_helper_get_object_handle_preserves_the_wrapped_direct_handle() {
    let mut pdf = Pdf::empty().unwrap();
    let page =
        ObjectHandle::dictionary(vec![(b"/Annots".to_vec(), ObjectHandle::array(Vec::new()))]);
    let helper = PageObjectHelper::from_object_handle(page.clone(), &mut pdf);

    let returned = helper.get_object_handle();
    assert!(returned.is_same_object_as(&page));
    assert_eq!(returned.object_ref(), None);
}

#[test]
fn page_helper_get_object_handle_preserves_the_wrapped_indirect_handle() {
    let mut pdf = open(single_page("<< /Type /Page /Parent 2 0 R >>", &[]));
    let page_ref = ObjectRef::new(3, 0);
    let page = pdf.get_object_handle(page_ref);
    assert_eq!(page.object_ref(), Some(page_ref));

    let helper = PageObjectHelper::from_object_handle(page.clone(), &mut pdf);
    let returned = helper.get_object_handle();

    assert!(returned.is_same_object_as(&page));
    assert_eq!(returned.object_ref(), Some(page_ref));
}

// ---------------------------------------------------------------------------
// qpdf's getAnnotations() fail-soft malformed-shape handling
// ---------------------------------------------------------------------------

#[test]
fn get_annotations_reference_to_non_array_returns_empty_like_qpdf() {
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /Annots 5 0 R >>",
        &[(5, "42".into())],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);
    assert!(helper.get_annotations().unwrap().is_empty());
}

#[test]
fn get_annotations_non_array_returns_empty_like_qpdf() {
    let bytes = single_page("<< /Type /Page /Parent 2 0 R /Annots 42 >>", &[]);
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);
    assert!(helper.get_annotations().unwrap().is_empty());
}

#[test]
fn get_annotations_skips_non_dictionary_array_items_like_qpdf() {
    // /Annots contains an inline integer instead of an annotation dictionary.
    let bytes = single_page("<< /Type /Page /Parent 2 0 R /Annots [42] >>", &[]);
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);
    assert!(helper.get_annotations().unwrap().is_empty());
}

#[test]
fn get_annotations_null_returns_empty_like_qpdf() {
    // /Annots explicitly null is treated as no annotations.
    let bytes = single_page("<< /Type /Page /Parent 2 0 R /Annots null >>", &[]);
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);
    assert!(helper.get_annotations().unwrap().is_empty());
}

#[test]
fn get_annotations_accepts_an_untyped_target_and_preserves_handle_identity_like_qpdf() {
    let mut pdf = Pdf::empty().unwrap();
    let annotation = ObjectHandle::dictionary(vec![(
        b"/Subtype".to_vec(),
        ObjectHandle::name(b"Text".to_vec()),
    )]);
    let object = ObjectHandle::dictionary(vec![(
        b"/Annots".to_vec(),
        ObjectHandle::array(vec![annotation.clone()]),
    )]);
    let mut helper = PageObjectHelper::from_object_handle(object, &mut pdf);

    let annotations = helper.get_annotations().unwrap();
    assert_eq!(annotations.len(), 1);
    assert!(annotations[0]
        .get_object_handle()
        .is_same_object_as(&annotation));
    assert_eq!(
        helper.get_annotations_with_subtype(b"Text").unwrap().len(),
        1
    );
    assert!(helper
        .get_annotations_with_subtype(b"Link")
        .unwrap()
        .is_empty());
}

#[test]
fn get_annotations_default_and_subtype_filter_match_qpdf_exact_name_bytes() {
    let annotation = |subtype: &[u8]| {
        ObjectHandle::dictionary(vec![(
            b"/Subtype".to_vec(),
            ObjectHandle::name(subtype.to_vec()),
        )])
    };
    let page = ObjectHandle::dictionary(vec![(
        b"/Annots".to_vec(),
        ObjectHandle::array(vec![
            annotation(b"Widget"),
            annotation(b"Text"),
            annotation(b"Link"),
            annotation(b""),
        ]),
    )]);
    let mut pdf = Pdf::empty().unwrap();
    let mut helper = PageObjectHelper::from_object_handle(page, &mut pdf);

    assert_eq!(helper.get_annotations().unwrap().len(), 4);
    for (subtype, expected) in [
        (b"".as_slice(), 4),
        (b"Widget", 1),
        (b"/Widget", 0),
        (b"/", 0),
        (b"Text", 1),
        (b"/Text", 0),
        (b"Link", 1),
        (b"/Link", 0),
        (b"Nope", 0),
    ] {
        assert_eq!(
            helper.get_annotations_with_subtype(subtype).unwrap().len(),
            expected,
            "qpdf getAnnotations({:?}) result",
            String::from_utf8_lossy(subtype)
        );
    }
}

#[test]
fn get_page_contents_accepts_an_untyped_dictionary_like_qpdf() {
    let bytes = single_page(
        "<< /Parent 2 0 R /MediaBox [0 0 20 30] /Contents 4 0 R >>",
        &[(4, "<< /Length 3 >>\nstream\nabc\nendstream".into())],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);

    let contents = helper.get_page_contents().unwrap();
    assert_eq!(contents.len(), 1);
    assert_eq!(contents[0].object_ref(), Some(ObjectRef::new(4, 0)));
}

#[test]
fn flatten_rotation_accepts_a_form_target_like_qpdf() {
    let mut pdf = Pdf::empty().unwrap();
    let form = pdf.new_stream_with_data(Rc::new(Vec::new())).unwrap();
    let form_dict = form.as_stream_dict().unwrap();
    form_dict
        .replace_key(b"/Type", ObjectHandle::name(b"XObject".to_vec()))
        .unwrap();
    form_dict
        .replace_key(b"/Subtype", ObjectHandle::name(b"Form".to_vec()))
        .unwrap();
    let mut helper = PageObjectHelper::from_object_handle(form, &mut pdf);

    helper
        .flatten_rotation()
        .expect("qpdf accepts the Form handle and returns when it has no page rotation");
}

#[test]
fn get_form_xobject_for_form_target_uses_qpdf_contents_lookup() {
    let mut pdf = Pdf::empty().unwrap();
    let form = pdf
        .new_stream_with_data(Rc::new(b"q Q\n".to_vec()))
        .unwrap();
    let form_dict = form.as_stream_dict().unwrap();
    form_dict
        .replace_key(b"/Type", ObjectHandle::name(b"XObject".to_vec()))
        .unwrap();
    form_dict
        .replace_key(b"/Subtype", ObjectHandle::name(b"Form".to_vec()))
        .unwrap();
    form_dict
        .replace_key(
            b"/BBox",
            ObjectHandle::array(vec![
                ObjectHandle::integer(0),
                ObjectHandle::integer(0),
                ObjectHandle::integer(10),
                ObjectHandle::integer(10),
            ]),
        )
        .unwrap();
    form_dict
        .replace_key(b"/Resources", ObjectHandle::dictionary(Vec::new()))
        .unwrap();
    let mut helper = PageObjectHelper::from_object_handle(form, &mut pdf);

    let wrapped = helper
        .get_form_xobject_for_page_with_options(false)
        .expect("qpdf accepts a Form target and wraps its stream");
    assert!(wrapped.is_form_xobject().unwrap());
    assert_eq!(
        wrapped
            .get_stream_data(flpdf::DecodeLevel::Generalized)
            .unwrap()
            .as_slice(),
        b""
    );
}

#[test]
fn get_attribute_does_not_inherit_rotate_for_form_xobject_like_qpdf() {
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 20 30] >>",
        &[
            (
                4,
                "<< /Type /XObject /Subtype /Form /Parent 5 0 R /BBox [0 0 10 10] /Resources << >> /Length 0 >>\nstream\n\nendstream".into(),
            ),
            (5, "<< /Rotate 90 >>".into()),
        ],
    );
    let (mut pdf, _) = helper_for(bytes);
    let form = pdf.get_object_handle(ObjectRef::new(4, 0));
    let mut helper = PageObjectHelper::from_object_handle(form, &mut pdf);

    let rotation = helper.get_attribute(b"/Rotate", false).unwrap();
    assert!(rotation.try_is_null().unwrap());
}

#[test]
fn get_attribute_inherits_rotate_from_page_parent_like_qpdf() {
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (
                2,
                "<< /Type /Pages /Kids [3 0 R] /Count 1 /Rotate 180 >>".into(),
            ),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 20 30] >>".into(),
            ),
        ],
        1,
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);

    let rotation = helper.get_attribute(b"/Rotate", false).unwrap();
    assert_eq!(rotation.try_get_int_value_as_int().unwrap(), 180);
}

#[test]
fn get_attribute_preserves_nonstandard_page_rotate_value_like_qpdf() {
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 20 30] /Rotate 45 >>",
        &[],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);

    let rotation = helper.get_attribute(b"/Rotate", false).unwrap();
    assert_eq!(rotation.try_get_int_value_as_int().unwrap(), 45);
}

#[test]
fn form_provider_describes_page_contents_with_qpdf_obj_gen() {
    // qpdf's ContentProvider labels the page contents as
    // "contents from page object " + getObjGen().unparse(' ')
    // (`libqpdf/QPDFPageObjectHelper.cc:35`), i.e. "3 0" without " R".
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 20 30] /Resources << >> /Contents [4 0 R 7] >>",
        &[(4, "<< /Length 3 >>\nstream\nq Q\nendstream".into())],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    pdf.set_suppress_warnings(true);
    let page = pdf.get_object_handle(page_ref);
    let wrapped = {
        let mut helper = PageObjectHelper::from_object_handle(page, &mut pdf);
        helper
            .get_form_xobject_for_page_with_options(false)
            .unwrap()
    };
    wrapped.get_raw_stream_data().unwrap();

    let messages: Vec<String> = pdf
        .get_warnings()
        .entries()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert!(
        messages
            .iter()
            .any(|message| message.contains("contents from page object 3 0: item index 1 (from 0)")),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .all(|message| !message.contains("contents from page object 3 0 R")),
        "{messages:?}"
    );
}

#[test]
fn form_provider_reads_live_page_contents_when_materialized_like_qpdf() {
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 20 30] /Resources << >> /Contents 4 0 R >>",
        &[(4, "<< /Length 3 >>\nstream\nold\nendstream".into())],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let page = pdf.get_object_handle(page_ref);
    let wrapped = {
        let mut helper = PageObjectHelper::from_object_handle(page.clone(), &mut pdf);
        helper
            .get_form_xobject_for_page_with_options(false)
            .unwrap()
    };

    let replacement = pdf.new_stream_with_data(Rc::new(b"new".to_vec())).unwrap();
    page.replace_key(b"/Contents", replacement).unwrap();

    assert_eq!(
        wrapped
            .get_stream_data(flpdf::DecodeLevel::Generalized)
            .unwrap()
            .as_slice(),
        b"new"
    );
}

// ---------------------------------------------------------------------------
// media_box() — /Parent chain anomalies and value resolution
// ---------------------------------------------------------------------------

#[test]
fn get_media_box_accepts_an_untyped_dictionary_like_qpdf() {
    let mut pdf = Pdf::empty().unwrap();
    let object = ObjectHandle::dictionary(vec![(
        b"/MediaBox".to_vec(),
        ObjectHandle::array(vec![
            ObjectHandle::integer(0),
            ObjectHandle::integer(0),
            ObjectHandle::integer(20),
            ObjectHandle::integer(30),
        ]),
    )]);
    let mut helper = PageObjectHelper::from_object_handle(object, &mut pdf);

    let media_box = helper.get_media_box().unwrap();
    assert!(media_box.try_is_array().unwrap());
    assert_eq!(media_box.try_get_array_n_items().unwrap(), 4);
}

#[test]
fn remove_unreferenced_resources_accepts_direct_dictionary_target_like_qpdf() {
    let mut pdf = Pdf::empty().unwrap();
    let xobjects = ObjectHandle::dictionary(vec![(
        b"/Unused".to_vec(),
        ObjectHandle::dictionary(Vec::new()),
    )]);
    let target = ObjectHandle::dictionary(vec![(
        b"/Resources".to_vec(),
        ObjectHandle::dictionary(vec![(b"/XObject".to_vec(), xobjects.clone())]),
    )]);
    let mut helper = PageObjectHelper::from_object_handle(target.clone(), &mut pdf);

    helper
        .remove_unreferenced_resources()
        .expect("qpdf removes unused resources from a direct dictionary target");
    let pruned = target
        .try_get_key(b"/Resources")
        .unwrap()
        .try_get_key(b"/XObject")
        .unwrap();
    assert!(!pruned.try_has_key(b"/Unused").unwrap());
    // qpdf replaces even a direct category with a shallow copy
    // (`libqpdf/QPDFPageObjectHelper.cc:576-585`), so the caller's original
    // dictionary is left untouched.
    assert!(!pruned.is_same_object_as(&xobjects));
    assert!(xobjects.try_has_key(b"/Unused").unwrap());
}

#[test]
fn remove_unreferenced_resources_preserves_unused_fonts_when_content_name_is_unresolved_like_qpdf()
{
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 20 30] /Resources << /Font << /Unused 4 0 R >> >> /Contents 5 0 R >>",
        &[
            (4, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into()),
            (5, "<< /Length 15 >>\nstream\n/Missing 12 Tf\nendstream".into()),
        ],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let page = pdf.get_object_handle(page_ref);
    let mut helper = PageObjectHelper::from_object_handle(page.clone(), &mut pdf);

    helper.remove_unreferenced_resources().unwrap();

    let resources = page.try_get_key(b"/Resources").unwrap();
    let fonts = resources.try_get_key(b"/Font").unwrap();
    assert!(fonts.try_has_key(b"/Unused").unwrap());
}

#[test]
fn remove_unreferenced_resources_prunes_after_an_unconsumed_name_like_qpdf() {
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 20 30] /Resources << /Font << /Unused 4 0 R >> >> /Contents 5 0 R >>",
        &[
            (4, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into()),
            (5, "<< /Length 10 >>\nstream\n/Dangling\nendstream".into()),
        ],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let page = pdf.get_object_handle(page_ref);
    let mut helper = PageObjectHelper::from_object_handle(page.clone(), &mut pdf);

    helper.remove_unreferenced_resources().unwrap();

    let resources = page.try_get_key(b"/Resources").unwrap();
    let fonts = resources.try_get_key(b"/Font").unwrap();
    assert!(!fonts.try_has_key(b"/Unused").unwrap());
}

#[test]
fn remove_unreferenced_resources_does_not_visit_child_removed_by_parent_like_qpdf() {
    let outer_form = "<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] /Resources 7 0 R /Length 3 >>\nstream\nq Q\nendstream";
    let malformed_child = "<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] /Resources << >> /Length 9 /Filter /FlateDecode >>\nstream\nnot-flate\nendstream";
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 20 30] /Resources 6 0 R >>",
        &[
            (4, outer_form.into()),
            (5, malformed_child.into()),
            (6, "<< /XObject << /Outer 4 0 R >> >>".into()),
            (7, "<< /XObject << /Child 5 0 R >> >>".into()),
        ],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let page = pdf.get_object_handle(page_ref);
    let warnings_before = pdf.repair_diagnostics().entries().len();
    let mut helper = PageObjectHelper::from_object_handle(page.clone(), &mut pdf);

    helper.remove_unreferenced_resources().unwrap();

    let resources = page.try_get_key(b"/Resources").unwrap();
    let xobjects = resources.try_get_key(b"/XObject").unwrap();
    assert!(!xobjects.try_has_key(b"/Outer").unwrap());
    assert_eq!(pdf.repair_diagnostics().entries().len(), warnings_before);
}

#[test]
fn remove_unreferenced_resources_runs_the_action_for_each_form_reference_like_qpdf() {
    let malformed_form = "<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] /Resources << >> /Length 4 >>\nstream\n<0g>\nendstream";
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 20 30] /Resources << /XObject << /First 4 0 R /Second 4 0 R >> >> >>",
        &[(4, malformed_form.into())],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);

    helper.remove_unreferenced_resources().unwrap();

    let warnings = pdf
        .repair_diagnostics()
        .entries()
        .iter()
        .map(|diagnostic| String::from_utf8_lossy(diagnostic.get_message_detail()).into_owned())
        .filter(|message| {
            message.contains("invalid character (g) in hexstring")
                || message.contains("EOF while reading token")
                || message.contains("Bad token found while scanning content stream")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        warnings.iter().map(String::as_str).collect::<Vec<_>>(),
        [
            "invalid character (g) in hexstring",
            "EOF while reading token",
            "Bad token found while scanning content stream; not attempting to remove unreferenced objects from this object",
            "invalid character (g) in hexstring",
            "EOF while reading token",
            "Bad token found while scanning content stream; not attempting to remove unreferenced objects from this object",
        ]
    );
}

/// qpdf snapshots `numWarnings` only through the receiver's owning QPDF
/// (`q ? q->numWarnings() : 0`, `libqpdf/QPDFPageObjectHelper.cc:550-553`).
/// A contextless direct receiver (here a shallow copy of the page) therefore
/// never takes the bad-token veto even though the content parser warned;
/// probed with the public C++ API on the same shape.
#[test]
fn remove_unreferenced_resources_ignores_parser_warnings_for_contextless_receiver_like_qpdf() {
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 20 30] /Resources << /Font << /F1 4 0 R /Unused 4 0 R >> >> /Contents 5 0 R >>",
        &[
            (4, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into()),
            (5, "<< /Length 14 >>\nstream\n<0g> /F1 12 Tf\nendstream".into()),
        ],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let receiver = pdf.get_object_handle(page_ref).shallow_copy().unwrap();
    let warnings_before = pdf.num_warnings();
    let mut helper = PageObjectHelper::from_object_handle(receiver.clone(), &mut pdf);

    helper.remove_unreferenced_resources().unwrap();

    assert!(
        pdf.num_warnings() > warnings_before,
        "the content parser still warns"
    );
    let fonts = receiver
        .try_get_key(b"/Resources")
        .unwrap()
        .try_get_key(b"/Font")
        .unwrap();
    assert!(fonts.try_has_key(b"/F1").unwrap());
    assert!(!fonts.try_has_key(b"/Unused").unwrap());
}

/// qpdf's `forEachXObject` walks `xobj_dict.getKeys()`, which drops keys whose
/// values are null (`libqpdf/QPDFPageObjectHelper.cc:335-338`,
/// `libqpdf/QPDF_Dictionary.cc:118-127`); probed with the public C++ API on the
/// same shape (only `/Image` is enumerated).
#[test]
fn for_each_xobject_skips_keys_whose_values_resolve_to_null_like_qpdf() {
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 20 30] /Resources << /XObject << /N null /Dang 99 0 R /NR 4 0 R /Image 5 0 R >> >> >>",
        &[
            (4, "null".into()),
            (
                5,
                "<< /Type /XObject /Subtype /Image /Width 2 /Height 3 /Length 0 >>\nstream\n\nendstream"
                    .into(),
            ),
        ],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let target = pdf.get_object_handle(page_ref);
    let mut helper = PageObjectHelper::from_object_handle(target, &mut pdf);
    let mut names = Vec::new();
    helper
        .for_each_xobject(false, |_, _, key| {
            names.push(key);
            Ok(())
        })
        .unwrap();
    assert_eq!(names, vec![b"/Image".to_vec()]);
}

#[test]
fn for_each_xobject_selector_filters_callbacks_without_pruning_form_recursion_like_qpdf() {
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] /Resources 8 0 R >>",
        &[
            (
                4,
                "<< /Type /XObject /Subtype /Form /BBox [0 0 1 1] /Resources 9 0 R /Length 0 >>\nstream\n\nendstream".into(),
            ),
            (
                6,
                "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /Length 0 >>\nstream\n\nendstream".into(),
            ),
            (
                8,
                "<< /XObject << /Pick 6 0 R /RejectForm 4 0 R >> >>".into(),
            ),
            (9, "<< /XObject << /Pick 6 0 R >> >>".into()),
        ],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);
    let image_ref = ObjectRef::new(6, 0);
    let mut candidates = Vec::new();
    let mut visits = Vec::new();

    helper
        .for_each_xobject_with_selector(
            true,
            |object, xobject_dict, key| {
                visits.push((object, xobject_dict, key));
                Ok(())
            },
            |object| {
                let object_ref = object.object_ref();
                candidates.push(object_ref);
                Ok(object_ref == Some(image_ref))
            },
        )
        .unwrap();

    assert!(candidates.contains(&Some(ObjectRef::new(4, 0))));
    assert_eq!(
        visits.len(),
        2,
        "selector filters actions, not Form recursion"
    );
    assert!(visits
        .iter()
        .all(|(object, _, _)| object.object_ref() == Some(image_ref)));
    assert_eq!(visits[0].2, b"/Pick");
    assert_eq!(visits[1].2, b"/Pick");
    assert!(!visits[0].1.is_same_object_as(&visits[1].1));
}

#[test]
fn remove_unreferenced_resources_copies_categories_before_unresolved_veto_like_qpdf() {
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 20 30] /Resources << /Font 4 0 R >> /Contents 5 0 R >>",
        &[
            (4, "<< /Unused 6 0 R >>".into()),
            (5, "<< /Length 14 >>\nstream\n/Missing 12 Tf\nendstream".into()),
            (6, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into()),
        ],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let page = pdf.get_object_handle(page_ref);
    let mut helper = PageObjectHelper::from_object_handle(page.clone(), &mut pdf);

    helper.remove_unreferenced_resources().unwrap();

    let resources = page.try_get_key(b"/Resources").unwrap();
    let fonts = resources.try_get_key(b"/Font").unwrap();
    assert!(!fonts.is_indirect());
    assert!(fonts.try_has_key(b"/Unused").unwrap());
    assert!(pdf
        .get_object_handle(ObjectRef::new(4, 0))
        .try_has_key(b"/Unused")
        .unwrap());
}

/// qpdf's `getAttribute` copies a fallback box into the supplied handle without
/// a `/Type /Page` check (`libqpdf/QPDFPageObjectHelper.cc:224-262`); probed
/// with the public C++ API on an untyped direct and indirect dictionary and on
/// a `/Type /Pages` node, where `getCropBox(false, true)` copies `/MediaBox`
/// into `/CropBox` and `getTrimBox(false, true)` then copies that into
/// `/TrimBox`.
#[test]
fn fallback_boxes_are_copied_into_untyped_and_non_page_dictionaries_like_qpdf() {
    fn media_box_dictionary() -> ObjectHandle {
        ObjectHandle::dictionary(vec![(
            b"/MediaBox".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::integer(0),
                ObjectHandle::integer(0),
                ObjectHandle::integer(20),
                ObjectHandle::integer(30),
            ]),
        )])
    }
    fn assert_copied(target: &ObjectHandle, result: &ObjectHandle, key: &[u8]) {
        assert_eq!(result.try_get_array_n_items().unwrap(), 4);
        let stored = target.try_get_key(key).unwrap();
        assert_eq!(stored.try_get_array_n_items().unwrap(), 4, "{key:?} copied");
    }

    // Direct untyped dictionary.
    let mut pdf = Pdf::empty().unwrap();
    let target = media_box_dictionary();
    let mut helper = PageObjectHelper::from_object_handle(target.clone(), &mut pdf);
    let crop = helper.get_crop_box_with_options(false, true).unwrap();
    assert_copied(&target, &crop, b"/CropBox");
    let trim = helper.get_trim_box_with_options(false, true).unwrap();
    assert_copied(&target, &trim, b"/TrimBox");
    let art = helper.get_art_box_with_options(false, true).unwrap();
    assert_copied(&target, &art, b"/ArtBox");

    // Indirect untyped dictionary and a `/Type /Pages` node.
    for body in [
        "<< /MediaBox [0 0 20 30] >>",
        "<< /Type /Pages /MediaBox [0 0 20 30] >>",
    ] {
        let bytes = single_page(body, &[]);
        let (mut pdf, page_ref) = helper_for(bytes);
        let target = pdf.get_object_handle(page_ref);
        let mut helper = PageObjectHelper::from_object_handle(target.clone(), &mut pdf);
        let bleed = helper.get_bleed_box_with_options(true, true).unwrap();
        assert_copied(&target, &bleed, b"/BleedBox");
        assert_copied(
            &target,
            &target.try_get_key(b"/CropBox").unwrap(),
            b"/CropBox",
        );
    }
}

#[test]
fn get_media_box_accepts_an_untyped_indirect_dictionary_like_qpdf() {
    let bytes = single_page("<< /Parent 2 0 R /MediaBox [0 0 20 30] >>", &[]);
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);

    let media_box = helper.get_media_box().unwrap();
    assert!(media_box.try_is_array().unwrap());
    assert_eq!(media_box.try_get_array_n_items().unwrap(), 4);
}

#[test]
fn get_media_box_preserves_raw_shape_and_rectangle_projection_matches_qpdf() {
    let extra_item = single_page(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 20 30] >>",
        &[],
    );
    let (mut pdf, page_ref) = helper_for(extra_item);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);
    let raw = helper.get_media_box().unwrap();
    assert_eq!(raw.try_get_array_n_items().unwrap(), 5);
    assert_eq!(
        raw.try_get_array_as_rectangle().unwrap(),
        Rectangle::default()
    );

    let reversed = single_page("<< /Type /Page /Parent 2 0 R /MediaBox [10 20 0 0] >>", &[]);
    let (mut pdf, page_ref) = helper_for(reversed);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);
    let raw = helper.get_media_box().unwrap();
    assert_eq!(
        raw.try_get_array_as_rectangle().unwrap(),
        Rectangle::new(0.0, 0.0, 10.0, 20.0)
    );
}

#[test]
fn get_media_box_preserves_real_coordinates_as_a_raw_handle_like_qpdf() {
    // The PageObjectHelper qpdf API returns the raw attribute handle. Typed
    // projection belongs to QPDFObjectHandle::getArrayAsRectangle.
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0.0 0.5 612.25 792.75] >>",
        &[],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);
    let media_box = helper.get_media_box().unwrap();
    assert_eq!(
        media_box.try_get_array_as_rectangle().unwrap(),
        Rectangle::new(0.0, 0.5, 612.25, 792.75)
    );
}

#[test]
fn media_box_beyond_the_former_depth_limit_returns_none_when_absent() {
    // qpdf has no numeric /Parent depth cap. A long acyclic chain with no
    // MediaBox terminates at the root and returns null.
    let mut objects = vec![
        (1u32, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
        (
            2u32,
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        ),
    ];
    // Leaf page is object 3; parents are 4..=133 (130 hops), none with MediaBox.
    objects.push((3, "<< /Type /Page /Parent 4 0 R >>".to_string()));
    for i in 4..=132 {
        objects.push((i, format!("<< /Type /Pages /Parent {} 0 R >>", i + 1)));
    }
    objects.push((133, "<< /Type /Pages >>".to_string()));
    let bytes = build_pdf(&objects, 1);
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);
    assert!(helper.get_media_box().unwrap().try_is_null().unwrap());
}

#[test]
fn media_box_value_null_climbs_to_parent() {
    // /MediaBox explicitly null on the leaf is treated as absent (§7.3.9), so
    // the walk climbs to the parent which carries the real box.
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (
                2,
                "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 300] >>".into(),
            ),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox null >>".into()),
        ],
        1,
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);
    assert_eq!(
        helper
            .get_media_box()
            .unwrap()
            .try_get_array_as_rectangle()
            .unwrap(),
        Rectangle::new(0.0, 0.0, 200.0, 300.0)
    );
}

#[test]
fn media_box_indirect_null_climbs_to_parent() {
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (
                2,
                "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 11 22] >>".into(),
            ),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox 5 0 R >>".into()),
            (5, "null".into()),
        ],
        1,
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);
    assert_eq!(
        helper
            .get_media_box()
            .unwrap()
            .try_get_array_as_rectangle()
            .unwrap(),
        Rectangle::new(0.0, 0.0, 11.0, 22.0)
    );
}

#[test]
fn get_media_box_returns_a_referenced_scalar_verbatim_like_qpdf() {
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /MediaBox 5 0 R >>",
        &[(5, "42".into())],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);
    assert_eq!(
        helper
            .get_media_box()
            .unwrap()
            .try_get_value_as_int()
            .unwrap(),
        Some(42)
    );
}

#[test]
fn get_media_box_returns_a_direct_scalar_verbatim_like_qpdf() {
    let bytes = single_page("<< /Type /Page /Parent 2 0 R /MediaBox 42 >>", &[]);
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);
    assert_eq!(
        helper
            .get_media_box()
            .unwrap()
            .try_get_value_as_int()
            .unwrap(),
        Some(42)
    );
}

#[test]
fn get_media_box_returns_a_short_array_without_projection_like_qpdf() {
    let bytes = single_page("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612] >>", &[]);
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);
    let raw = helper.get_media_box().unwrap();
    assert_eq!(raw.try_get_array_n_items().unwrap(), 3);
    assert_eq!(
        raw.try_get_array_as_rectangle().unwrap(),
        Rectangle::default()
    );
}

#[test]
fn get_media_box_returns_a_non_numeric_array_without_projection_like_qpdf() {
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 /Nope] >>",
        &[],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);
    let raw = helper.get_media_box().unwrap();
    assert_eq!(raw.try_get_array_n_items().unwrap(), 4);
    assert_eq!(
        raw.try_get_array_as_rectangle().unwrap(),
        Rectangle::default()
    );
}

#[test]
fn media_box_parent_not_reference_returns_none() {
    // /Parent is a direct value (not an indirect reference): the walk stops and
    // reports no inherited box.
    let bytes = single_page("<< /Type /Page /Parent 42 >>", &[]);
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);
    assert!(helper.get_media_box().unwrap().try_is_null().unwrap());
}

#[test]
fn media_box_parent_not_dictionary_returns_none() {
    // /Parent resolves to a non-dictionary object: the walk stops gracefully.
    let bytes = single_page("<< /Type /Page /Parent 5 0 R >>", &[(5, "42".into())]);
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);
    assert!(helper.get_media_box().unwrap().try_is_null().unwrap());
}

#[test]
fn media_box_parent_cycle_returns_none() {
    // 3 -> 5 -> 3 forms a cycle with no MediaBox anywhere; the visited guard
    // breaks the loop and returns None rather than spinning forever.
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (3, "<< /Type /Page /Parent 5 0 R >>".into()),
            (5, "<< /Type /Pages /Kids [3 0 R] /Parent 3 0 R >>".into()),
        ],
        1,
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);
    assert!(helper.get_media_box().unwrap().try_is_null().unwrap());
}

// ---------------------------------------------------------------------------
// Leaf-only boxes (bleed/trim/art): explicit, indirect, null, and error arms
// ---------------------------------------------------------------------------

#[test]
fn trim_box_explicit_on_leaf() {
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /TrimBox [1 2 3 4] >>",
        &[],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);
    assert_eq!(
        helper
            .get_trim_box()
            .unwrap()
            .try_get_array_as_rectangle()
            .unwrap(),
        Rectangle::new(1.0, 2.0, 3.0, 4.0)
    );
}

#[test]
fn art_box_explicit_on_leaf() {
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /ArtBox [5 6 7 8] >>",
        &[],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);
    assert_eq!(
        helper
            .get_art_box()
            .unwrap()
            .try_get_array_as_rectangle()
            .unwrap(),
        Rectangle::new(5.0, 6.0, 7.0, 8.0)
    );
}

#[test]
fn bleed_box_null_falls_back_to_crop() {
    // /BleedBox null is treated as absent, so it defaults to CropBox -> MediaBox.
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 50 60] /BleedBox null >>",
        &[],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);
    assert_eq!(
        helper
            .get_bleed_box()
            .unwrap()
            .try_get_array_as_rectangle()
            .unwrap(),
        Rectangle::new(0.0, 0.0, 50.0, 60.0)
    );
}

#[test]
fn trim_box_indirect_null_falls_back_to_crop() {
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 50 60] /TrimBox 5 0 R >>",
        &[(5, "null".into())],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);
    assert_eq!(
        helper
            .get_trim_box()
            .unwrap()
            .try_get_array_as_rectangle()
            .unwrap(),
        Rectangle::new(0.0, 0.0, 50.0, 60.0)
    );
}

#[test]
fn art_box_indirect_array_resolved() {
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /ArtBox 5 0 R >>",
        &[(5, "[9 9 19 19]".into())],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);
    assert_eq!(
        helper
            .get_art_box()
            .unwrap()
            .try_get_array_as_rectangle()
            .unwrap(),
        Rectangle::new(9.0, 9.0, 19.0, 19.0)
    );
}

#[test]
fn get_bleed_box_returns_a_referenced_scalar_verbatim_like_qpdf() {
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /BleedBox 5 0 R >>",
        &[(5, "42".into())],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);
    assert_eq!(
        helper
            .get_bleed_box()
            .unwrap()
            .try_get_value_as_int()
            .unwrap(),
        Some(42)
    );
}

#[test]
fn get_bleed_box_returns_a_direct_scalar_verbatim_like_qpdf() {
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /BleedBox 42 >>",
        &[],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);
    assert_eq!(
        helper
            .get_bleed_box()
            .unwrap()
            .try_get_value_as_int()
            .unwrap(),
        Some(42)
    );
}

// ---------------------------------------------------------------------------
// qpdf-delegating accessors do not require /Type /Page
// ---------------------------------------------------------------------------

#[test]
fn box_and_annotation_accessors_do_not_require_page_type() {
    // Object 3 is a /Pages tree node, not a leaf /Type /Page.
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (3, "<< /Type /Pages /Parent 2 0 R /Kids [] >>".into()),
        ],
        1,
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);
    assert!(helper.get_media_box().unwrap().try_is_null().unwrap());
    assert!(helper.get_annotations().unwrap().is_empty());
}

#[test]
fn media_box_ignores_a_non_name_page_type_like_qpdf() {
    let bytes = single_page("<< /Type 42 /Parent 2 0 R /MediaBox [0 0 612 792] >>", &[]);
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);
    assert_eq!(
        helper
            .get_media_box()
            .unwrap()
            .try_get_array_as_rectangle()
            .unwrap(),
        Rectangle::new(0.0, 0.0, 612.0, 792.0)
    );
}

// ---------------------------------------------------------------------------
// XObject image classification
// ---------------------------------------------------------------------------

fn image_mask_page() -> Vec<u8> {
    single_page(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] /Resources 4 0 R >>",
        &[
            (
                4,
                "<< /XObject << /Mask 5 0 R /Image 6 0 R >> >>".into(),
            ),
            (
                5,
                "<< /Type /XObject /Subtype /Image /ImageMask true /Width 1 /Height 1 /Length 0 >>\nstream\nendstream".into(),
            ),
            (
                6,
                "<< /Type /XObject /Subtype /Image /Width 2 /Height 3 /Length 0 >>\nstream\nendstream".into(),
            ),
        ],
    )
}

#[test]
fn image_enumeration_excludes_image_masks_like_qpdf() {
    let (mut pdf, page_ref) = helper_for(image_mask_page());

    let mut direct = page_helper_for_ref(&mut pdf, page_ref);
    let mut direct_names = Vec::new();
    direct
        .for_each_image(false, |_, _, key| {
            direct_names.push(key);
            Ok(())
        })
        .unwrap();
    assert_eq!(direct_names, vec![b"/Image".to_vec()]);

    let mut recursive = page_helper_for_ref(&mut pdf, page_ref);
    let mut recursive_names = Vec::new();
    recursive
        .for_each_image(true, |_, _, key| {
            recursive_names.push(key);
            Ok(())
        })
        .unwrap();
    assert_eq!(recursive_names, vec![b"/Image".to_vec()]);

    let mut maps = page_helper_for_ref(&mut pdf, page_ref);
    assert_eq!(
        maps.get_images().unwrap().into_keys().collect::<Vec<_>>(),
        vec![b"/Image".to_vec()]
    );
}

#[test]
fn xobject_enumeration_uses_inherited_resources() {
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (
                2,
                "<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources 4 0 R >>".into(),
            ),
            (3, "<< /Type /Page /Parent 2 0 R >>".into()),
            (
                4,
                "<< /XObject << /Image 5 0 R >> >>".into(),
            ),
            (
                5,
                "<< /Type /XObject /Subtype /Image /Width 2 /Height 3 /Length 0 >>\nstream\nendstream".into(),
            ),
        ],
        1,
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = page_helper_for_ref(&mut pdf, page_ref);
    let mut names = Vec::new();
    helper
        .for_each_xobject(false, |_, _, key| {
            names.push(key);
            Ok(())
        })
        .unwrap();
    assert_eq!(names, vec![b"/Image".to_vec()]);
}
