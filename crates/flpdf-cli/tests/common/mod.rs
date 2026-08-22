//! Shared helpers for flpdf-cli integration tests.
//!
//! Lives in `tests/common/mod.rs` (not `tests/common.rs`) so Cargo does not
//! treat it as its own test binary; each test file pulls it in with
//! `mod common;`.

use flpdf::{ObjectHandle, ObjectRef, PageObjectHelper, Pdf};

/// Return the canonical annotation handles listed by a page.
pub fn page_annotation_handles<R: std::io::Read + std::io::Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Vec<ObjectHandle> {
    PageObjectHelper::new(page_ref, pdf)
        .get_annotations_filtered(None)
        .unwrap()
}

/// Find the single Widget annotation on the first page by structure.
///
/// Full-rewrite output is renumbered Catalog-first, so the widget no longer has
/// a stable object number; navigate to it via `/Annots` rather than hardcoding
/// a number. Each fixture used here has exactly one merged widget, so its
/// `annot_ref` is the dict that holds `/AP`. Panics unless the first page
/// carries exactly one Widget annotation, so a fixture change is caught.
pub fn first_widget_ref<R: std::io::Read + std::io::Seek>(pdf: &mut Pdf<R>) -> ObjectRef {
    let page_ref = *flpdf::pages::page_refs(pdf)
        .unwrap()
        .first()
        .expect("fixture must have at least one page");
    let widgets: Vec<_> = PageObjectHelper::new(page_ref, pdf)
        .get_annotations_filtered(Some(b"/Widget"))
        .unwrap();
    assert_eq!(
        widgets.len(),
        1,
        "fixture must have exactly one Widget annotation, found {}",
        widgets.len()
    );
    widgets[0]
        .object_ref()
        .expect("fixture Widget annotation must be indirect")
}
