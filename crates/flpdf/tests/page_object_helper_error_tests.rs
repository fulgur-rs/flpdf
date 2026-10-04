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

use flpdf::{ObjectHandle, ObjectRef, PageObjectHelper, Pdf, Rectangle};
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

// ---------------------------------------------------------------------------
// qpdf's getAnnotations() fail-soft malformed-shape handling
// ---------------------------------------------------------------------------

#[test]
fn get_annotation_handles_reference_to_non_array_returns_empty_like_qpdf() {
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /Annots 5 0 R >>",
        &[(5, "42".into())],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert!(helper.get_annotation_handles(None).unwrap().is_empty());
}

#[test]
fn get_annotation_handles_non_array_returns_empty_like_qpdf() {
    let bytes = single_page("<< /Type /Page /Parent 2 0 R /Annots 42 >>", &[]);
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert!(helper.get_annotation_handles(None).unwrap().is_empty());
}

#[test]
fn get_annotation_handles_skips_non_dictionary_array_items_like_qpdf() {
    // /Annots contains an inline integer instead of an annotation dictionary.
    let bytes = single_page("<< /Type /Page /Parent 2 0 R /Annots [42] >>", &[]);
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert!(helper.get_annotation_handles(None).unwrap().is_empty());
}

#[test]
fn get_annotation_handles_null_returns_empty_like_qpdf() {
    // /Annots explicitly null is treated as no annotations.
    let bytes = single_page("<< /Type /Page /Parent 2 0 R /Annots null >>", &[]);
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert!(helper.get_annotation_handles(None).unwrap().is_empty());
}

#[test]
fn get_annotation_handles_accepts_an_untyped_target_like_qpdf() {
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

    let annotations = helper.get_annotation_handles(None).unwrap();
    assert_eq!(annotations.len(), 1);
    assert!(annotations[0].is_same_object_as(&annotation));
    assert_eq!(
        helper.get_annotation_handles(Some(b"/Text")).unwrap().len(),
        1
    );
    assert!(helper
        .get_annotation_handles(Some(b"/Link"))
        .unwrap()
        .is_empty());
}

#[test]
fn get_page_contents_accepts_an_untyped_dictionary_like_qpdf() {
    let bytes = single_page(
        "<< /Parent 2 0 R /MediaBox [0 0 20 30] /Contents 4 0 R >>",
        &[(4, "<< /Length 3 >>\nstream\nabc\nendstream".into())],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);

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
        .get_form_xobject_for_page(false)
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
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);

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
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);

    let rotation = helper.get_attribute(b"/Rotate", false).unwrap();
    assert_eq!(rotation.try_get_int_value_as_int().unwrap(), 45);
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
        helper.get_form_xobject_for_page(false).unwrap()
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

    let media_box = helper.get_media_box(false).unwrap();
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
    let mut helper = PageObjectHelper::from_object_handle(target, &mut pdf);

    helper
        .remove_unreferenced_resources()
        .expect("qpdf removes unused resources from a direct dictionary target");
    assert!(!xobjects.try_has_key(b"/Unused").unwrap());
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
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);

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
    let crop = helper.get_crop_box(false, true).unwrap();
    assert_copied(&target, &crop, b"/CropBox");
    let trim = helper.get_trim_box(false, true).unwrap();
    assert_copied(&target, &trim, b"/TrimBox");

    // Indirect untyped dictionary and a `/Type /Pages` node.
    for body in [
        "<< /MediaBox [0 0 20 30] >>",
        "<< /Type /Pages /MediaBox [0 0 20 30] >>",
    ] {
        let bytes = single_page(body, &[]);
        let (mut pdf, page_ref) = helper_for(bytes);
        let target = pdf.get_object_handle(page_ref);
        let mut helper = PageObjectHelper::from_object_handle(target.clone(), &mut pdf);
        let bleed = helper.get_bleed_box(true, true).unwrap();
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
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);

    let media_box = helper.get_media_box(false).unwrap();
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
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    let raw = helper.get_media_box(false).unwrap();
    assert_eq!(raw.try_get_array_n_items().unwrap(), 5);
    assert_eq!(
        raw.try_get_array_as_rectangle().unwrap(),
        Rectangle::default()
    );

    let reversed = single_page("<< /Type /Page /Parent 2 0 R /MediaBox [10 20 0 0] >>", &[]);
    let (mut pdf, page_ref) = helper_for(reversed);
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    let raw = helper.get_media_box(false).unwrap();
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
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    let media_box = helper.get_media_box(false).unwrap();
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
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert!(helper.get_media_box(false).unwrap().try_is_null().unwrap());
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
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert_eq!(
        helper
            .get_media_box(false)
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
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert_eq!(
        helper
            .get_media_box(false)
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
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert_eq!(
        helper
            .get_media_box(false)
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
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert_eq!(
        helper
            .get_media_box(false)
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
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    let raw = helper.get_media_box(false).unwrap();
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
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    let raw = helper.get_media_box(false).unwrap();
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
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert!(helper.get_media_box(false).unwrap().try_is_null().unwrap());
}

#[test]
fn media_box_parent_not_dictionary_returns_none() {
    // /Parent resolves to a non-dictionary object: the walk stops gracefully.
    let bytes = single_page("<< /Type /Page /Parent 5 0 R >>", &[(5, "42".into())]);
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert!(helper.get_media_box(false).unwrap().try_is_null().unwrap());
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
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert!(helper.get_media_box(false).unwrap().try_is_null().unwrap());
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
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert_eq!(
        helper
            .get_trim_box(false, false)
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
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert_eq!(
        helper
            .get_art_box(false, false)
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
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert_eq!(
        helper
            .get_bleed_box(false, false)
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
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert_eq!(
        helper
            .get_trim_box(false, false)
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
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert_eq!(
        helper
            .get_art_box(false, false)
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
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert_eq!(
        helper
            .get_bleed_box(false, false)
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
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert_eq!(
        helper
            .get_bleed_box(false, false)
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
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert!(helper.get_media_box(false).unwrap().try_is_null().unwrap());
    assert!(helper.get_annotation_handles(None).unwrap().is_empty());
}

#[test]
fn media_box_ignores_a_non_name_page_type_like_qpdf() {
    let bytes = single_page("<< /Type 42 /Parent 2 0 R /MediaBox [0 0 612 792] >>", &[]);
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert_eq!(
        helper
            .get_media_box(false)
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

    let mut direct = PageObjectHelper::new(page_ref, &mut pdf);
    let mut direct_names = Vec::new();
    direct
        .for_each_image(false, |_, _, key| {
            direct_names.push(key);
            Ok(())
        })
        .unwrap();
    assert_eq!(direct_names, vec![b"/Image".to_vec()]);

    let mut recursive = PageObjectHelper::new(page_ref, &mut pdf);
    let mut recursive_names = Vec::new();
    recursive
        .for_each_image(true, |_, _, key| {
            recursive_names.push(key);
            Ok(())
        })
        .unwrap();
    assert_eq!(recursive_names, vec![b"/Image".to_vec()]);

    let mut maps = PageObjectHelper::new(page_ref, &mut pdf);
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
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    let mut names = Vec::new();
    helper
        .for_each_xobject(false, |_, _, key| {
            names.push(key);
            Ok(())
        })
        .unwrap();
    assert_eq!(names, vec![b"/Image".to_vec()]);
}
