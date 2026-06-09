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

use flpdf::{Error, ObjectRef, PageBox, PageObjectHelper, Pdf};
use std::io::Cursor;

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

fn assert_unsupported<T: std::fmt::Debug>(result: flpdf::Result<T>) {
    match result {
        Err(Error::Unsupported(_)) => {}
        other => panic!("expected Error::Unsupported, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// get_annotations() malformed shapes
// ---------------------------------------------------------------------------

#[test]
fn get_annotations_reference_not_array_errors() {
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /Annots 5 0 R >>",
        &[(5, "42".into())],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert_unsupported(helper.get_annotations());
}

#[test]
fn get_annotations_unexpected_type_errors() {
    let bytes = single_page("<< /Type /Page /Parent 2 0 R /Annots 42 >>", &[]);
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert_unsupported(helper.get_annotations());
}

#[test]
fn get_annotations_non_reference_element_errors() {
    // /Annots array element is an inline integer instead of a reference.
    let bytes = single_page("<< /Type /Page /Parent 2 0 R /Annots [42] >>", &[]);
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert_unsupported(helper.get_annotations());
}

#[test]
fn get_annotations_null_returns_empty() {
    // /Annots explicitly null is treated as no annotations.
    let bytes = single_page("<< /Type /Page /Parent 2 0 R /Annots null >>", &[]);
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert!(helper.get_annotations().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// media_box() — /Parent chain anomalies and value resolution
// ---------------------------------------------------------------------------

#[test]
fn media_box_accepts_real_coordinates() {
    // Rectangle elements may be reals, not just integers (ISO 32000-1 §7.9.5).
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0.0 0.5 612.25 792.75] >>",
        &[],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert_eq!(
        helper.media_box().unwrap(),
        Some(PageBox::new(0.0, 0.5, 612.25, 792.75))
    );
}

#[test]
fn media_box_depth_limit_errors() {
    // A /Parent chain deeper than DEFAULT_MAX_PAGE_TREE_DEPTH (100) with no
    // MediaBox must surface an Unsupported error rather than spinning.
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
    assert_unsupported(helper.media_box());
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
        helper.media_box().unwrap(),
        Some(PageBox::new(0.0, 0.0, 200.0, 300.0))
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
        helper.media_box().unwrap(),
        Some(PageBox::new(0.0, 0.0, 11.0, 22.0))
    );
}

#[test]
fn media_box_reference_not_array_errors() {
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /MediaBox 5 0 R >>",
        &[(5, "42".into())],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert_unsupported(helper.media_box());
}

#[test]
fn media_box_unexpected_type_errors() {
    let bytes = single_page("<< /Type /Page /Parent 2 0 R /MediaBox 42 >>", &[]);
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert_unsupported(helper.media_box());
}

#[test]
fn media_box_too_few_elements_errors() {
    let bytes = single_page("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612] >>", &[]);
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert_unsupported(helper.media_box());
}

#[test]
fn media_box_non_numeric_element_errors() {
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 /Nope] >>",
        &[],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert_unsupported(helper.media_box());
}

#[test]
fn media_box_parent_not_reference_returns_none() {
    // /Parent is a direct value (not an indirect reference): the walk stops and
    // reports no inherited box.
    let bytes = single_page("<< /Type /Page /Parent 42 >>", &[]);
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert_eq!(helper.media_box().unwrap(), None);
}

#[test]
fn media_box_parent_not_dictionary_returns_none() {
    // /Parent resolves to a non-dictionary object: the walk stops gracefully.
    let bytes = single_page("<< /Type /Page /Parent 5 0 R >>", &[(5, "42".into())]);
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert_eq!(helper.media_box().unwrap(), None);
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
    assert_eq!(helper.media_box().unwrap(), None);
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
        helper.trim_box().unwrap(),
        Some(PageBox::new(1.0, 2.0, 3.0, 4.0))
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
        helper.art_box().unwrap(),
        Some(PageBox::new(5.0, 6.0, 7.0, 8.0))
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
        helper.bleed_box().unwrap(),
        Some(PageBox::new(0.0, 0.0, 50.0, 60.0))
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
        helper.trim_box().unwrap(),
        Some(PageBox::new(0.0, 0.0, 50.0, 60.0))
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
        helper.art_box().unwrap(),
        Some(PageBox::new(9.0, 9.0, 19.0, 19.0))
    );
}

#[test]
fn bleed_box_reference_not_array_errors() {
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /BleedBox 5 0 R >>",
        &[(5, "42".into())],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert_unsupported(helper.bleed_box());
}

#[test]
fn bleed_box_unexpected_type_errors() {
    let bytes = single_page(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /BleedBox 42 >>",
        &[],
    );
    let (mut pdf, page_ref) = helper_for(bytes);
    let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    assert_unsupported(helper.bleed_box());
}

// ---------------------------------------------------------------------------
// ensure_leaf_page guard: non-Page object is rejected by every accessor
// ---------------------------------------------------------------------------

#[test]
fn accessors_reject_non_page_object() {
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
    assert_unsupported(helper.media_box());
    assert_unsupported(helper.get_annotations());
}
