//! Provide live traversal and mutation for a document's page tree.
//!
//! qpdf correspondence: QPDFPageDocumentHelper.cc responsibilities split with page extraction.
//!
//! The public surface mirrors qpdf 11.9.0's seven
//! `QPDFPageDocumentHelper` operations: page enumeration, inherited-attribute
//! materialization, page insertion/removal, resource pruning, and annotation
//! flattening. The helper holds no copied page-tree state.

use crate::object_handle::DocumentResolver;
use crate::pages::tree_rebuild::{rebuild_page_tree, RebuildResult};
use crate::qpdf_obj_gen::QpdfObjGen;
use crate::{
    Error, ObjectHandle, ObjectRef, PageObjectHelper, Pdf, QpdfErrorCode, QpdfExc, Result,
};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

/// High-level page-document helper.
///
/// Construct with [`PageDocumentHelper::new`], then use the provided methods to
/// traverse or mutate the document's page list through qpdf-corresponding
/// operations. No page-tree state is cached inside this struct.
pub struct PageDocumentHelper<'a, R: Read + Seek + 'static> {
    pdf: &'a mut Pdf<R>,
}

/// An input page for [`PageDocumentHelper::add_page`] and
/// [`PageDocumentHelper::add_page_at`].
///
/// qpdf accepts a `QPDFObjectHandle`, which can be direct, target-owned, or
/// owned by another `QPDF`. Rust's handles do not retain an owning-document
/// borrow, so the foreign case explicitly carries its source document.
pub enum PageInput<'a, R: Read + Seek + 'static> {
    /// A direct page handle, which qpdf turns into a fresh indirect object.
    Direct(ObjectHandle),
    /// An indirect page already owned by the target document.
    Existing(ObjectRef),
    /// An indirect page owned by another document.
    Foreign {
        source: &'a mut Pdf<R>,
        page: ObjectRef,
    },
}

impl PageInput<'static, std::io::Cursor<Vec<u8>>> {
    /// Construct a direct page input.
    pub fn direct(page: ObjectHandle) -> Self {
        Self::Direct(page)
    }

    /// Construct an input for a page already owned by the target document.
    pub fn existing(page: ObjectRef) -> Self {
        Self::Existing(page)
    }
}

impl<'a, R: Read + Seek> PageInput<'a, R> {
    /// Construct an input page from another document.
    pub fn foreign(source: &'a mut Pdf<R>, page: ObjectRef) -> Self {
        Self::Foreign { source, page }
    }
}

impl<'a, R: Read + Seek> PageDocumentHelper<'a, R> {
    /// Create a new helper borrowing `pdf` mutably.
    pub fn new(pdf: &'a mut Pdf<R>) -> Self {
        Self { pdf }
    }

    fn prepare_all_pages(&mut self) -> Result<Option<crate::pages::repair::PreparedPages>> {
        // QPDFPageDocumentHelper::getAllPages delegates to QPDF::getAllPages,
        // whose `QPDFObjectHandle pages = getRoot().getKey("/Pages")` calls
        // `getRoot()` first and unconditionally: a missing OR non-dictionary
        // `/Root` both throw "unable to find /Root dictionary"
        // (`libqpdf/QPDF.cc:2355-2360`, `libqpdf/QPDF_pages.cc:41-47`). Keep
        // the lower-level optimization preparation helper's no-root no-op
        // contract for its other callers, but enforce the public
        // page-document boundary here via the same canonical dictionary gate
        // `pages::page_refs` used before this helper replaced it. A trailer
        // with no `/Root` key at all keeps flpdf's established
        // `Error::Missing("/Root")`; a `/Root` that resolves but is not a
        // dictionary goes through `root_handle`'s own error. `root_ref()` is
        // intentionally not used here because a direct Catalog has no
        // ObjectRef identity.
        if self.pdf.trailer_key_handle(b"Root").is_null() {
            return Err(Error::Missing("/Root"));
        }
        self.pdf.root_handle()?;
        crate::pages::repair::prepare_for_optimization(self.pdf)
    }

    /// Return qpdf's repaired leaf-page handles in document order.
    ///
    /// This is the Rust equivalent of `QPDFPageDocumentHelper::getAllPages()`:
    /// each result retains its raw object/generation identity, including
    /// generations that are outside valid PDF `N G R` reference syntax. The
    /// page snapshot is owned, so later page-tree mutations require a fresh
    /// call.
    pub fn get_all_pages(&mut self) -> Result<Vec<ObjectHandle>> {
        Ok(self
            .prepare_all_pages()?
            .map(|prepared| prepared.pages)
            .unwrap_or_default())
    }

    /// Materialize inherited page attributes on each leaf page.
    ///
    /// Mirrors `QPDFPageDocumentHelper::pushInheritedAttributesToPage()` by
    /// first applying qpdf-compatible page-tree repair, then pushing
    /// `/CropBox`, `/MediaBox`, `/Resources`, and `/Rotate` onto leaf pages.
    pub fn push_inherited_attributes_to_pages(&mut self) -> Result<()> {
        self.pdf.mark_get_all_pages_called();
        if let Some(prepared) = crate::pages::repair::prepare_for_optimization(self.pdf)? {
            crate::optimization::inherited_attrs::push(self.pdf, &prepared, true, false)?;
        } // cov:ignore: closing brace is the already-covered successful push join
        self.pdf.ever_pushed_inherited_attributes_to_pages = true;
        Ok(())
    }

    /// Add `page` at the beginning (`first == true`) or end of the document.
    ///
    /// Mirrors `QPDFPageDocumentHelper::addPage`. If `page` already occurs in
    /// the page tree, rebuilding creates a shallow duplicate for its later
    /// occurrence, retaining shared page sub-objects.
    pub fn add_page<RS: Read + Seek>(
        &mut self,
        page: PageInput<'_, RS>,
        first: bool,
    ) -> Result<RebuildResult> {
        let index = if first {
            0
        } else {
            self.get_all_pages()?.len()
        };
        self.insert_page(index, page)
    }

    /// Add `page` immediately before or after `reference_page`.
    ///
    /// Mirrors `QPDFPageDocumentHelper::addPageAt`. The reference page must
    /// be present in the repaired current page list; a non-member is rejected
    /// before the page tree is mutated.
    pub fn add_page_at<RS: Read + Seek>(
        &mut self,
        page: PageInput<'_, RS>,
        before: bool,
        reference_page: ObjectRef,
    ) -> Result<RebuildResult> {
        let pages = crate::pages::page_refs(self.pdf)?;
        let index = pages
            .iter()
            .position(|&candidate| candidate == reference_page)
            .ok_or(Error::Missing("reference page is not in the document"))?;
        self.insert_page(index + usize::from(!before), page)
    }

    /// Insert `page` at 0-based position `idx`, shifting existing pages to the
    /// right.
    ///
    /// `idx == 0` prepends; `idx == page_count` appends.  `page` must already
    /// exist in the document as a valid `/Page` dictionary — [`rebuild_page_tree`]
    /// will return an error otherwise.
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] when `idx > page_count`.
    /// - Any error from [`rebuild_page_tree`] (e.g. `page` is not a `/Page` dict).
    fn insert_page<RS: Read + Seek>(
        &mut self,
        idx: usize,
        page: PageInput<'_, RS>,
    ) -> Result<RebuildResult> {
        let mut refs = crate::pages::page_refs(self.pdf)?;
        if idx > refs.len() {
            return Err(Error::Unsupported(format!(
                "insert index {idx} is out of bounds (page count {})",
                refs.len()
            )));
        }
        let page = self.materialize_page_input(page)?;
        if refs.contains(&page) {
            // qpdf's QPDF::insertPage uses shallowCopy followed by
            // makeIndirectObject for a page that is already in the tree
            // (QPDF_pages.cc:233-237). Keep the duplicate on the canonical
            // handle graph so its shared indirect children retain identity.
            let copy = self.pdf.get_object_handle(page).shallow_copy()?;
            let duplicate = self.pdf.make_indirect_object_handle(copy)?;
            let duplicate_ref = duplicate
                .object_ref()
                .expect("make_indirect_object_handle returns an indirect handle");
            refs.insert(idx, duplicate_ref);
        } else {
            refs.insert(idx, page);
        }
        let result = rebuild_page_tree(self.pdf, &refs)?;
        // The inserted page may carry annotations (in particular orphan
        // Widgets not reachable through `/AcroForm/Fields`) that a shared
        // `Pdf::acroform_cache` warmed before this call has no knowledge of.
        // qpdf's own per-step `QPDFAcroFormDocumentHelper` construction
        // (`QPDFJob.cc:2141-2193`) never observes a page inserted after it
        // was built either; invalidating here reproduces that "no stale
        // analysis survives a page-tree mutation" guarantee, matching
        // `AcroFormDocumentHelper::invalidate_cache`'s own documented
        // contract ("after manually changing the field tree, AcroForm
        // dictionary, or page annotations").
        *self.pdf.acroform_cache.borrow_mut() = None;
        Ok(result)
    }

    /// Remove one page from a page tree that qpdf has already flattened for
    /// `QPDFJob::handlePageSpecs`.
    ///
    /// qpdf's `findPage` calls `flattenPagesTree` once, then `removePage`
    /// erases the matching `/Kids` entry and updates `/Count` in place
    /// (`QPDF_pages.cc:253-266,303-320`). `current_pages` is the corresponding
    /// live `m->all_pages` sequence maintained by the caller for this job.
    pub(crate) fn remove_flattened_page_for_job(
        &mut self,
        page: ObjectRef,
        current_pages: &mut Vec<ObjectRef>,
    ) -> Result<()> {
        let Some(index) = current_pages
            .iter()
            .position(|&candidate| candidate == page)
        else {
            let description_bytes = self.pdf.resolver.input_description();
            let object = format!("page object: object {} {}", page.number, page.generation);
            return Err(Error::QpdfExc(QpdfExc::new(
                QpdfErrorCode::Pages,
                description_bytes,
                object,
                0,
                b"page object not referenced in /Pages tree",
            )));
        };

        let catalog = self.pdf.root_handle()?;
        let pages = catalog.try_get_key(b"/Pages")?;
        pages.try_dereference()?;
        if !pages.try_is_dictionary()? {
            return Err(Error::Unsupported(
                "document /Pages root is not a dictionary".into(),
            ));
        }
        let kids = pages.try_get_key(b"/Kids")?;
        kids.try_erase_array_item_at(i64::try_from(index).map_err(|_| {
            // cov:ignore-start: a PDF page vector cannot exceed i64::MAX entries in process memory.
            Error::Unsupported("page index exceeds qpdf's signed array-index range".into())
            // cov:ignore-end
        })?)?; // cov:ignore: LLVM maps this continuation to the unreachable page-vector overflow edge.
        let count = i64::try_from(kids.try_array_len()?.unwrap_or_default()).map_err(|_| {
            // cov:ignore-start: a PDF Kids array cannot exceed i64::MAX entries in process memory.
            Error::Unsupported("page count exceeds qpdf's signed integer range".into())
            // cov:ignore-end
        })?; // cov:ignore: LLVM maps this continuation to the unreachable page-count overflow edge.
        pages.replace_key(b"/Count", ObjectHandle::integer(count))?;
        current_pages.remove(index);
        self.pdf.invalidate_page_list_cache();
        *self.pdf.acroform_cache.borrow_mut() = None;
        Ok(())
    }

    /// Insert one page at the end of qpdf's already flattened page tree.
    ///
    /// This is the page-spec job's direct `QPDF::insertPage` path
    /// (`QPDF_pages.cc:204-250`): foreign page graphs are copied through the
    /// canonical per-source copier, repeated page identities become shallow
    /// page-dictionary copies, and every copy allocates at the primary
    /// document's live object-cache ceiling before the next occurrence is
    /// processed.
    pub(crate) fn append_flattened_page_for_job<RS: Read + Seek>(
        &mut self,
        page: PageInput<'_, RS>,
        current_pages: &mut Vec<ObjectRef>,
    ) -> Result<ObjectRef> {
        let mut page_ref = self.materialize_page_input(page)?;
        if current_pages.contains(&page_ref) {
            let copy = self.pdf.get_object_handle(page_ref).shallow_copy()?;
            let duplicate = self.pdf.make_indirect_object_handle(copy)?;
            page_ref = duplicate
                .object_ref()
                .ok_or(Error::Missing("duplicate page did not become indirect"))?;
        }

        let catalog = self.pdf.root_handle()?;
        let pages = catalog.try_get_key(b"/Pages")?;
        pages.try_dereference()?;
        if !pages.try_is_dictionary()? {
            return Err(Error::Unsupported(
                "document /Pages root is not a dictionary".into(),
            ));
        }
        let kids = pages.try_get_key(b"/Kids")?;
        if !kids.try_is_array()? {
            return Err(Error::Unsupported(
                "document /Pages /Kids is not an array".into(),
            ));
        }
        let page_handle = self.pdf.get_object_handle(page_ref);
        page_handle.replace_key(b"/Parent", pages.clone())?;
        kids.try_append_array_item(page_handle)?;
        let count = i64::try_from(kids.try_array_len()?.unwrap_or_default()).map_err(|_| {
            // cov:ignore-start: a PDF Kids array cannot exceed i64::MAX entries in process memory.
            Error::Unsupported("page count exceeds qpdf's signed integer range".into())
            // cov:ignore-end
        })?; // cov:ignore: LLVM maps this continuation to the unreachable page-count overflow edge.
        pages.replace_key(b"/Count", ObjectHandle::integer(count))?;
        current_pages.push(page_ref);
        self.pdf.invalidate_page_list_cache();
        *self.pdf.acroform_cache.borrow_mut() = None;
        Ok(page_ref)
    }

    fn materialize_page_input<RS: Read + Seek>(
        &mut self,
        input: PageInput<'_, RS>,
    ) -> Result<ObjectRef> {
        match input {
            PageInput::Direct(handle) => {
                let indirect = self.pdf.make_indirect_object_handle(handle)?;
                Ok(indirect
                    .object_ref()
                    .expect("make_indirect_object_handle always returns an indirect handle"))
            }
            PageInput::Existing(page) => Ok(page),
            PageInput::Foreign { source, page } => {
                PageDocumentHelper::new(source).push_inherited_attributes_to_pages()?;
                // qpdf's QPDF::insertPage calls copyForeignObject directly
                // after materializing inherited attributes
                // (libqpdf/QPDF.cc:2019-2097, libqpdf/QPDF_pages.cc:213-215).
                // Keep page insertion on the canonical ObjectHandle graph
                // route so reservation, /Pages boundaries, null-aware keys,
                // and per-source identity reuse have one implementation.
                let source_page = source.get_object_handle(page);
                let copied = self.pdf.copy_foreign_object(&source_page)?;
                copied
                    .object_ref()
                    .ok_or(Error::Missing("foreign page copy was not indirect"))
            }
        }
    }

    /// Remove the page at 0-based position `idx`.
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] when `idx >= page_count`.
    /// - Any error from [`rebuild_page_tree`] when pages remain after removal.
    fn remove_page_at(&mut self, idx: usize) -> Result<RebuildResult> {
        let mut refs = crate::pages::page_refs(self.pdf)?;
        if idx >= refs.len() {
            return Err(Error::Unsupported(format!(
                "remove index {idx} is out of bounds (page count {})",
                refs.len()
            )));
        }
        let removed_page = refs[idx];
        refs.remove(idx);
        if refs.is_empty() {
            // qpdf's removePage flattens the page tree before erasing the
            // final leaf (`QPDF_pages.cc:304-306`). Do the same here so
            // skipped intermediate `/Pages` keys are warned about before the
            // empty-tree mutation takes the fast path.
            rebuild_page_tree(self.pdf, &[removed_page])?;
            return self.clear_page_tree();
        }
        let result = rebuild_page_tree(self.pdf, &refs)?;
        // The page mutation changes qpdf's page-based orphan-Widget analysis.
        // `QPDFAcroFormDocumentHelper::invalidateCache` is the explicit
        // boundary for such external mutations (qpdf/include/qpdf/
        // QPDFAcroFormDocumentHelper.hh:68-78).
        *self.pdf.acroform_cache.borrow_mut() = None;
        Ok(result)
    }

    /// Remove the specified page from the document.
    ///
    /// Mirrors `QPDFPageDocumentHelper::removePage`. qpdf permits removal of
    /// the final page, leaving an empty `/Pages` `/Kids` array and `/Count 0`.
    /// Returns [`Error::Pages`] when `page` is not in the repaired page list,
    /// preserving qpdf's source description and page-object context.
    pub fn remove_page(&mut self, page: ObjectRef) -> Result<RebuildResult> {
        let pages = crate::pages::page_refs(self.pdf)?;
        let Some(index) = pages.iter().position(|&candidate| candidate == page) else {
            // qpdf's QPDF::findPage sets the last object description to
            // `page object` and throws qpdf_e_pages with the owning input
            // filename (`QPDF_pages.cc:304-316`). Keep the complete exception
            // text on the canonical page-helper error so callers do not need
            // to reconstruct it at the driver boundary.
            let description_bytes = self.pdf.resolver.input_description();
            let object = format!("page object: object {} {}", page.number, page.generation);
            return Err(Error::QpdfExc(QpdfExc::new(
                QpdfErrorCode::Pages,
                description_bytes,
                object,
                0,
                b"page object not referenced in /Pages tree",
            )));
        };
        self.remove_page_at(index)
    }

    /// Remove unused `/Font` and `/XObject` resources from each page.
    ///
    /// Mirrors `QPDFPageDocumentHelper::removeUnreferencedResources` by
    /// invoking qpdf-style page-scoped pruning once for every current page.
    pub fn remove_unreferenced_resources(&mut self) -> Result<()> {
        for page in self.get_all_pages()? {
            let mut helper = PageObjectHelper::from_object_handle(page, self.pdf);
            helper.remove_unreferenced_resources()?;
        }
        Ok(())
    }

    /// Flatten annotations into their containing pages.
    ///
    /// Mirrors `QPDFPageDocumentHelper::flattenAnnotations`.
    ///
    /// An annotation is drawn only when all `required_flags` are set and none
    /// of `forbidden_flags` are set. As in qpdf, annotations with an
    /// appearance dictionary are removed even if no selected appearance can
    /// be drawn; annotations without one are retained.
    pub fn flatten_annotations(&mut self, required_flags: i64, forbidden_flags: i64) -> Result<()> {
        // qpdf's document helper obtains `getAllPages()` before flattening.
        // This repairs a catalog /Pages pointer that lands on a leaf, so the
        // lower-level document primitive subsequently sees every page.
        let pages = crate::pages::page_refs(self.pdf)?;
        crate::page_annotation_flatten::flatten_annotations_qpdf(
            self.pdf,
            &pages,
            required_flags,
            forbidden_flags,
        )
    }

    /// Clear the live document's root page tree after qpdf-style final-page
    /// removal. `remove_page_at` has already flattened a one-page tree through
    /// `rebuild_page_tree`, so this method only performs the final empty-tree
    /// mutation that qpdf's `removePage` leaves behind.
    fn clear_page_tree(&mut self) -> Result<RebuildResult> {
        // Keep the removed-page bookkeeping in qpdf's raw identity domain;
        // `RebuildResult` uses the same QpdfObjGen keys.
        let removed_page_objgens: BTreeSet<QpdfObjGen> = self
            .get_all_pages()?
            .into_iter()
            .map(|page| page.get_obj_gen())
            .filter(|object_gen| object_gen.is_indirect())
            .collect();
        let catalog = self.pdf.root_handle()?;
        let Some(catalog_dict) = catalog.as_dictionary() else {
            // cov:ignore-start: remove obtains pages through get_all_pages, which proves /Root is a dictionary before clear_page_tree runs
            return Err(Error::Unsupported(
                "document catalog is not a dictionary".into(),
            ));
            // cov:ignore-end
        };
        // cov:ignore-start: get_all_pages must have found the removed page through catalog /Pages before this final-page path can run
        if !catalog_dict.contains_key(b"/Pages".as_slice()) {
            return Err(Error::Missing("/Pages"));
        }
        // cov:ignore-end
        let pages = catalog.try_get_key(b"/Pages")?;
        let root = pages;
        if !root.try_is_dictionary()? {
            // cov:ignore-start: remove_page first obtains a repaired, dictionary /Pages root
            return Err(Error::Unsupported(
                "document /Pages root is not a dictionary".into(),
            ));
            // cov:ignore-end
        }

        // QPDF::removePage itself only erases the removed child and updates
        // /Count on the existing /Pages handle (QPDF_pages.cc:253-266); the
        // preceding `rebuild_page_tree` call supplied the
        // `findPage()->flattenPagesTree()` precondition
        // (`QPDF_pages.cc:304-306`). Preserve the direct catalog `/Pages`
        // root while applying the final empty-tree values in place.
        root.replace_key(b"/Type", ObjectHandle::name(b"Pages".to_vec()))?;
        root.replace_key(b"/Kids", ObjectHandle::array(Vec::new()))?;
        root.replace_key(b"/Count", ObjectHandle::integer(0))?;
        root.remove_key(b"/Parent");
        self.pdf.invalidate_page_list_cache();
        // Final-page removal is the same page mutation as the non-empty
        // rebuild above; keep the shared AcroForm analysis from observing
        // the removed page on the next helper call.
        *self.pdf.acroform_cache.borrow_mut() = None;
        Ok(RebuildResult {
            new_kids: Vec::new(),
            ref_map: BTreeMap::new(),
            removed_page_objgens,
        })
    }
}

#[cfg(test)]
mod job_flattened_page_tests {
    use super::*;
    use crate::PageInput;
    use std::io::Cursor;

    fn three_page_pdf() -> Pdf<Cursor<Vec<u8>>> {
        Pdf::open_mem_owned(
            include_bytes!("../../../tests/fixtures/compat/three-page.pdf").to_vec(),
        )
        .expect("open three-page fixture")
    }

    #[test]
    fn remove_flattened_page_reports_missing_page_and_malformed_pages_root() {
        let mut pdf = three_page_pdf();
        let page = crate::pages::page_refs(&mut pdf).expect("page refs")[0];
        assert!(matches!(
            PageDocumentHelper::new(&mut pdf).remove_flattened_page_for_job(page, &mut Vec::new()),
            Err(Error::QpdfExc(_))
        ));

        let mut pdf = three_page_pdf();
        let page = crate::pages::page_refs(&mut pdf).expect("page refs")[0];
        pdf.root_handle()
            .expect("catalog")
            .replace_key(b"/Pages", ObjectHandle::integer(1))
            .expect("replace Pages root");
        assert!(matches!(
            PageDocumentHelper::new(&mut pdf).remove_flattened_page_for_job(page, &mut vec![page]),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn remove_flattened_page_rejects_non_array_kids() {
        let mut pdf = three_page_pdf();
        let page = crate::pages::page_refs(&mut pdf).expect("page refs")[0];
        let pages = ObjectHandle::dictionary(vec![(b"Kids".to_vec(), ObjectHandle::integer(1))]);
        pdf.root_handle()
            .expect("catalog")
            .replace_key(b"/Pages", pages)
            .expect("replace Pages root");

        assert!(PageDocumentHelper::new(&mut pdf)
            .remove_flattened_page_for_job(page, &mut vec![page])
            .is_err());
    }

    #[test]
    fn append_flattened_page_rejects_malformed_pages_root_and_kids() {
        let mut pdf = three_page_pdf();
        let page = crate::pages::page_refs(&mut pdf).expect("page refs")[0];
        pdf.root_handle()
            .expect("catalog")
            .replace_key(b"/Pages", ObjectHandle::integer(1))
            .expect("replace Pages root");
        assert!(matches!(
            PageDocumentHelper::new(&mut pdf)
                .append_flattened_page_for_job(PageInput::existing(page), &mut Vec::new(),),
            Err(Error::Unsupported(_))
        ));

        let mut pdf = three_page_pdf();
        let page = crate::pages::page_refs(&mut pdf).expect("page refs")[0];
        let pages = ObjectHandle::dictionary(vec![(b"Kids".to_vec(), ObjectHandle::integer(1))]);
        pdf.root_handle()
            .expect("catalog")
            .replace_key(b"/Pages", pages)
            .expect("replace Pages root");
        assert!(matches!(
            PageDocumentHelper::new(&mut pdf)
                .append_flattened_page_for_job(PageInput::existing(page), &mut Vec::new(),),
            Err(Error::Unsupported(_))
        ));
    }
}
