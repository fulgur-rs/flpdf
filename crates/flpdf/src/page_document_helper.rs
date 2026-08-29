//! qpdf correspondence: QPDFPageDocumentHelper.cc responsibilities split with page extraction.
//! High-level page-document helper, mirroring qpdf's `QPDFPageDocumentHelper`.
//!
//! The public surface mirrors qpdf 11.9.0's seven
//! `QPDFPageDocumentHelper` operations: page enumeration, inherited-attribute
//! materialization, page insertion/removal, resource pruning, and annotation
//! flattening. The helper holds no copied page-tree state.

use crate::pages::tree_rebuild::{rebuild_page_tree, RebuildResult};
use crate::{Error, ObjectHandle, ObjectRef, PageObjectHelper, Pdf, Result};
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

    /// Return qpdf's repaired leaf-page list in document order.
    ///
    /// This mirrors `QPDFPageDocumentHelper::getAllPages()`: qpdf repairs the
    /// effective `/Pages` root and malformed leaf nodes before returning the
    /// current page list. The returned vector is an owned snapshot, so a later
    /// page insertion or removal requires a fresh call.
    pub fn get_all_pages(&mut self) -> Result<Vec<ObjectRef>> {
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
        Ok(crate::pages::repair::prepare_for_optimization(self.pdf)?
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
        let pages = self.get_all_pages()?;
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
        let mut refs = self.get_all_pages()?;
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
        let mut refs = self.get_all_pages()?;
        if idx >= refs.len() {
            return Err(Error::Unsupported(format!(
                "remove index {idx} is out of bounds (page count {})",
                refs.len()
            )));
        }
        refs.remove(idx);
        if refs.is_empty() {
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
    pub fn remove_page(&mut self, page: ObjectRef) -> Result<RebuildResult> {
        let index = self
            .get_all_pages()?
            .iter()
            .position(|&candidate| candidate == page)
            .ok_or(Error::Missing("page is not in the document"))?;
        self.remove_page_at(index)
    }

    /// Remove unused `/Font` and `/XObject` resources from each page.
    ///
    /// Mirrors `QPDFPageDocumentHelper::removeUnreferencedResources` by
    /// invoking qpdf-style page-scoped pruning once for every current page.
    pub fn remove_unreferenced_resources(&mut self) -> Result<()> {
        for page in self.get_all_pages()? {
            let mut helper = PageObjectHelper::new(page, self.pdf);
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
        let pages = self.get_all_pages()?;
        crate::page_annotation_flatten::flatten_annotations_qpdf(
            self.pdf,
            &pages,
            required_flags,
            forbidden_flags,
        )
    }

    /// Clear the live document's root page tree after qpdf-style final-page
    /// removal. `rebuild_page_tree` intentionally rejects an empty selection
    /// because page-selection callers use that as invalid input, while qpdf's
    /// `removePage` permits an empty document.
    fn clear_page_tree(&mut self) -> Result<RebuildResult> {
        let removed_pages: BTreeSet<ObjectRef> = self.get_all_pages()?.into_iter().collect();
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
        self.pdf.resolve(&root)?;
        if root.as_dictionary().is_none() {
            // cov:ignore-start: remove_page first obtains a repaired, dictionary /Pages root
            return Err(Error::Unsupported(
                "document /Pages root is not a dictionary".into(),
            ));
            // cov:ignore-end
        }

        // QPDF::removePage itself only erases the removed child and updates
        // /Count on the existing /Pages handle (QPDF_pages.cc:253-266); it
        // does not touch /Type or /Parent. /Type repair is a precondition
        // that removePage's findPage()->flattenPagesTree() call chain
        // reaches via getAllPagesInternal (QPDF_pages.cc:89-91), and a
        // correctly-identified root simply has no /Parent to begin with by
        // the time removePage runs -- getAllPages's climb-up loop
        // (QPDF_pages.cc:50-66) only ever walks up TO the true root, never
        // past it, so nothing here is "removing" a /Parent qpdf itself
        // wrote. This final-page path folds both preconditions into the
        // same live mutation rather than reaching them as a side effect of
        // walking the (now-empty) tree, and also preserves a direct catalog
        // /Pages root without re-materializing the catalog.
        root.replace_key(b"/Type", ObjectHandle::name(b"Pages".to_vec()))?;
        root.replace_key(b"/Kids", ObjectHandle::array(Vec::new()))?;
        root.replace_key(b"/Count", ObjectHandle::integer(0))?;
        root.remove_key(b"/Parent");
        self.pdf.mark_object_handle_dirty(&root)?;
        self.pdf.invalidate_page_list_cache();
        // Final-page removal is the same page mutation as the non-empty
        // rebuild above; keep the shared AcroForm analysis from observing
        // the removed page on the next helper call.
        *self.pdf.acroform_cache.borrow_mut() = None;
        Ok(RebuildResult {
            new_kids: Vec::new(),
            ref_map: BTreeMap::new(),
            removed_pages,
        })
    }
}
