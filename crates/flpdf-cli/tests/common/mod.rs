//! Shared helpers for flpdf-cli integration tests.
//!
//! Lives in `tests/common/mod.rs` (not `tests/common.rs`) so Cargo does not
//! treat it as its own test binary; each test file pulls it in with
//! `mod common;`.

#![allow(dead_code)]

use flpdf::{ObjectHandle, ObjectRef, PageDocumentHelper, PageObjectHelper, Pdf, Result};

/// Enumerate qpdf's repaired page list while preserving each raw page handle.
pub fn raw_page_handles<R: std::io::Read + std::io::Seek>(
    pdf: &mut Pdf<R>,
) -> Result<Vec<ObjectHandle>> {
    PageDocumentHelper::new(pdf).get_all_pages()
}

/// Count qpdf's repaired raw page list without requiring indirect references.
pub fn raw_page_count<R: std::io::Read + std::io::Seek>(pdf: &mut Pdf<R>) -> Result<usize> {
    Ok(raw_page_handles(pdf)?.len())
}

/// Project one raw handle only for test operations whose input is an `ObjectRef`.
pub fn checked_page_ref(page: &ObjectHandle) -> ObjectRef {
    page.object_ref()
        .expect("fixture page must have valid N G R syntax")
}

/// Return the first page's raw handle from qpdf's repaired page list.
pub fn first_page_handle<R: std::io::Read + std::io::Seek>(pdf: &mut Pdf<R>) -> ObjectHandle {
    raw_page_handles(pdf)
        .expect("enumerate page tree")
        .into_iter()
        .next()
        .expect("fixture must have at least one page")
}

/// Canonical object access for integration assertions after the library's
/// owned raw-object resolver was removed.
pub trait PdfCanonicalTestExt {
    fn resolve_canonical_object(&mut self, object_ref: ObjectRef) -> Result<ObjectHandle>;
}

impl<R: std::io::Read + std::io::Seek + 'static> PdfCanonicalTestExt for Pdf<R> {
    fn resolve_canonical_object(&mut self, object_ref: ObjectRef) -> Result<ObjectHandle> {
        let handle = self.get_object_handle(object_ref);
        handle.try_is_scalar()?;
        Ok(handle)
    }
}

/// Enumerate the canonical qpdf object cache for integration assertions.
pub fn canonical_object_refs<R: std::io::Read + std::io::Seek + 'static>(
    pdf: &mut Pdf<R>,
) -> Vec<ObjectRef> {
    pdf.get_all_objects()
        .expect("enumerate canonical qpdf object cache")
        .into_iter()
        .filter_map(|handle| handle.object_ref())
        .collect()
}

/// Return the canonical annotation handles listed by a page.
pub fn page_annotation_handles<R: std::io::Read + std::io::Seek>(
    pdf: &mut Pdf<R>,
    page: ObjectHandle,
) -> Vec<ObjectHandle> {
    PageObjectHelper::from_object_handle(page, pdf)
        .get_annotation_handles(None)
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
    let page = first_page_handle(pdf);
    let widgets: Vec<_> = PageObjectHelper::from_object_handle(page, pdf)
        .get_annotation_handles(Some(b"/Widget"))
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
