//! Shared helpers for flpdf-cli integration tests.
//!
//! Lives in `tests/common/mod.rs` (not `tests/common.rs`) so Cargo does not
//! treat it as its own test binary; each test file pulls it in with
//! `mod common;`.

use flpdf::{ObjectRef, Pdf};

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
    let widgets: Vec<_> = flpdf::enumerate_page_annotations(pdf, page_ref)
        .unwrap()
        .into_iter()
        .filter(|a| a.is_widget)
        .collect();
    assert_eq!(
        widgets.len(),
        1,
        "fixture must have exactly one Widget annotation, found {}",
        widgets.len()
    );
    widgets[0].annot_ref
}
