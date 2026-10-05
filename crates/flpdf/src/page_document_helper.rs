//! Provide live traversal and mutation for a document's page tree.
//!
//! qpdf correspondence: QPDFPageDocumentHelper.cc responsibilities split with page extraction.
//!
//! The public surface mirrors qpdf 11.9.0's seven
//! `QPDFPageDocumentHelper` operations: page enumeration, inherited-attribute
//! materialization, page insertion/removal, resource pruning, and annotation
//! flattening. The helper holds no copied page-tree state.

use crate::object_handle::DocumentResolver;
use crate::pages::tree_rebuild::page_tree_root_handle;
use crate::{
    Error, ObjectHandle, ObjectRef, PageObjectHelper, Pdf, QpdfErrorCode, QpdfExc, QpdfObjGen,
    Result,
};
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
/// qpdf accepts one `QPDFObjectHandle` and determines directness and document
/// ownership from that handle. Rust calls distinguish a target-owned handle
/// from a foreign handle because the foreign route also needs a mutable borrow
/// of its source document.
pub enum PageInput<'a, R: Read + Seek + 'static> {
    /// A direct value or an indirect page already owned by the target.
    Target(ObjectHandle),
    /// A page handle owned by another document.
    Foreign {
        source: &'a mut Pdf<R>,
        page: ObjectHandle,
    },
}

impl<'a> PageInput<'a, std::io::Cursor<Vec<u8>>> {
    /// Construct a page input for a direct value or target-owned handle.
    pub fn target(page: ObjectHandle) -> Self {
        Self::Target(page)
    }
}

impl<'a, R: Read + Seek + 'static> PageInput<'a, R> {
    /// Construct an input page from another document.
    pub fn foreign(source: &'a mut Pdf<R>, page: ObjectHandle) -> Self {
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

    /// Ensure qpdf's lazy flattened-page-tree boundary has run and return its
    /// live ordered page list and root handle.
    fn flatten_pages_tree(
        &mut self,
    ) -> Result<(crate::pages::repair::PreparedPages, ObjectHandle)> {
        let prepared = self.prepare_all_pages()?.ok_or(Error::Missing("/Pages"))?;
        let pages_root = page_tree_root_handle(self.pdf, &prepared.root)?;

        // QPDF::flattenPagesTree uses its non-empty raw page-position map as
        // the once-per-cache-lifetime sentinel. Reuse the same live /Kids array
        // for later insertPage/removePage operations.
        if !self.pdf.page_tree_flattened {
            crate::optimization::inherited_attrs::push(self.pdf, &prepared, true, true)?;
            self.pdf.ever_pushed_inherited_attributes_to_pages = true;
            for page in &prepared.pages {
                page.replace_key(b"/Parent", pages_root.clone())?;
            }
            pages_root.replace_key(b"/Kids", ObjectHandle::array(prepared.pages.clone()))?;
            self.pdf.page_tree_flattened = !prepared.pages.is_empty();
            self.pdf.cache_page_list(&prepared);

            // qpdf checks the pre-existing /Count after replacing /Kids.
            // getUIntValue warns and returns zero for negative and non-integers.
            let count_handle = pages_root.try_get_key(b"/Count")?;
            let declared_count = match count_handle.try_as_integer()? {
                Some(count) if count >= 0 => count as u64,
                Some(_) => {
                    count_handle.warn_if_possible(
                        "unsigned value request for negative number; returning 0",
                    )?; // cov:ignore: warning-sink failure is not injectable through qpdf's successful Count conversion
                    0
                }
                None => {
                    let type_name = count_handle.type_name()?;
                    let warning = format!(
                        "operation for integer attempted on object of type {type_name}: returning 0"
                    );
                    count_handle.warn_if_possible(&warning)?;
                    0
                }
            };
            if declared_count != prepared.pages.len() as u64 {
                return Err(Error::Internal(
                    "/Count is wrong after flattening pages tree".to_owned(),
                ));
            }
        }

        Ok((prepared, pages_root))
    }

    fn page_not_in_tree_error(&self, page: QpdfObjGen) -> Error {
        let description_bytes = self.pdf.resolver.input_description();
        let object = format!("page object: object {} {}", page.get_obj(), page.get_gen());
        Error::QpdfExc(QpdfExc::new(
            QpdfErrorCode::Pages,
            description_bytes,
            object,
            0,
            b"page object not referenced in /Pages tree",
        ))
    }

    /// Add `page` at the beginning (`first == true`) or end of the document.
    ///
    /// Mirrors qpdf's `QPDFPageDocumentHelper::addPage`. If `page` already
    /// occurs in the page tree, qpdf inserts a shallow copy for the later
    /// occurrence while preserving shared page sub-objects.
    pub fn add_page<RS: Read + Seek + 'static>(
        &mut self,
        page: PageInput<'_, RS>,
        first: bool,
    ) -> Result<()> {
        // QPDF::addPage reads /Count before insertPage performs the lazy
        // flattening step (QPDF_pages.cc:287-295).
        let index = if first {
            0
        } else {
            let catalog = self.pdf.root_handle()?;
            catalog
                .try_get_key(b"/Pages")?
                .try_get_key(b"/Count")?
                .try_get_int_value_as_int()?
        };
        self.insert_page(index, page)
    }

    /// Add `page` immediately before or after `reference_page`.
    ///
    /// Mirrors qpdf's `QPDFPageDocumentHelper::addPageAt`. Membership uses
    /// the reference handle's raw QpdfObjGen identity.
    pub fn add_page_at<RS: Read + Seek + 'static>(
        &mut self,
        page: PageInput<'_, RS>,
        before: bool,
        reference_page: ObjectHandle,
    ) -> Result<()> {
        let (prepared, _) = self.flatten_pages_tree()?;
        let reference_obj_gen = reference_page.get_obj_gen();
        let index = prepared
            .pages
            .iter()
            .position(|candidate| candidate.get_obj_gen() == reference_obj_gen)
            .ok_or_else(|| self.page_not_in_tree_error(reference_obj_gen))?;
        let index = index + usize::from(!before);
        let index = match i32::try_from(index) {
            Ok(index) => index,
            Err(_) => {
                let error = Error::Unsupported("page index exceeds qpdf's signed range".into()); // cov:ignore: a page vector exceeding qpdf's i32 range cannot be allocated in process memory.
                return Err(error); // cov:ignore: this return is reached only for an impossible oversized page vector.
            }
        };
        self.insert_page(index, page)
    }

    /// Insert `page` at qpdf's 0-based array position.
    ///
    /// Follows QPDF::insertPage order: flatten, materialize direct or foreign
    /// input, bounds-check, resolve duplicates by raw QpdfObjGen, then mutate
    /// the live /Kids array.
    fn insert_page<RS: Read + Seek + 'static>(
        &mut self,
        index: i32,
        page: PageInput<'_, RS>,
    ) -> Result<()> {
        let (mut prepared, pages_root) = self.flatten_pages_tree()?;
        let mut page = self.materialize_page_input(page)?;
        let page_count = prepared.pages.len();
        if index < 0 || usize::try_from(index).map_or(true, |index| index > page_count) {
            return Err(Error::Internal(
                "QPDF::insertPage called with pos out of range".to_owned(),
            ));
        }
        let index = index as usize;

        if prepared
            .pages
            .iter()
            .any(|existing| existing.get_obj_gen() == page.get_obj_gen())
        {
            // QPDF::insertPage shallow-copies a page already in the target
            // tree before making the copied page dictionary indirect
            // (QPDF_pages.cc:233-237).
            page = self.pdf.make_indirect_object_handle(page.shallow_copy()?)?;
        }

        page.replace_key(b"/Parent", pages_root.clone())?;
        let kids = pages_root.try_get_key(b"/Kids")?;
        kids.insert_array_item(index, page.clone())?;
        let count = i64::try_from(kids.try_get_array_n_items()?).map_err(|_| {
            // cov:ignore-start: an in-memory page tree cannot allocate more than i64::MAX Kids entries.
            Error::Unsupported("page count exceeds qpdf's signed integer range".into())
            // cov:ignore-end
        })?; // cov:ignore: LLVM maps this continuation to the unreachable page-count overflow edge.
        pages_root.replace_key(b"/Count", ObjectHandle::integer(count))?;

        prepared.pages.insert(index, page);
        self.pdf.invalidate_page_list_cache();
        self.pdf.cache_page_list(&prepared);
        self.pdf.page_tree_flattened = !prepared.pages.is_empty();
        // An inserted page may carry orphan Widgets not observed by a warm
        // AcroForm analysis; qpdf's job helper is constructed per step.
        *self.pdf.acroform_cache.borrow_mut() = None;
        Ok(())
    }

    /// Remove one page from a page tree that qpdf has already flattened for
    /// `QPDFJob::handlePageSpecs`.
    ///
    /// qpdf's `findPage` calls `flattenPagesTree` once, then `removePage`
    /// erases the matching `/Kids` entry and updates `/Count` in place
    /// (`QPDF_pages.cc:253-275,303-319`). `current_pages` is the corresponding
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
        self.pdf.page_tree_flattened = !current_pages.is_empty();
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
        let page = self.materialize_page_input(page)?;
        let mut page_ref = page.object_ref().ok_or(Error::Missing(
            "page object has no ObjectRef in page-spec job",
        ))?;
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
        self.pdf.page_tree_flattened = true;
        *self.pdf.acroform_cache.borrow_mut() = None;
        Ok(page_ref)
    }

    fn materialize_page_input<RS: Read + Seek + 'static>(
        &mut self,
        input: PageInput<'_, RS>,
    ) -> Result<ObjectHandle> {
        match input {
            PageInput::Target(handle) => {
                if handle.is_direct() {
                    self.pdf.make_indirect_object_handle(handle)
                } else if handle.owning_pdf_unique_id() == Some(self.pdf.unique_id()) {
                    Ok(handle)
                } else {
                    Err(Error::Unsupported(
                        "indirect page handle is not owned by the target PDF; use Foreign input"
                            .into(),
                    ))
                }
            }
            PageInput::Foreign { source, page } => {
                if page.is_direct() {
                    // QPDF::insertPage promotes direct handles in the target
                    // before testing foreign ownership.
                    self.pdf.make_indirect_object_handle(page)
                } else if page.owning_pdf_unique_id() == Some(source.unique_id()) {
                    PageDocumentHelper::new(source).push_inherited_attributes_to_pages()?;
                    // QPDF::insertPage materializes source inheritance before
                    // copyForeignObject (QPDF_pages.cc:211-218).
                    self.pdf.copy_foreign_object(&page)
                } else {
                    Err(Error::Unsupported(
                        "foreign page handle is not owned by the source PDF".into(),
                    ))
                }
            }
        }
    }

    /// Remove `page` from the document by its raw page-object identity.
    ///
    /// Mirrors `QPDFPageDocumentHelper::removePage(QPDFPageObjectHelper)`:
    /// qpdf flattens the page tree once, locates the page through its raw
    /// `QPDFObjGen`, erases that `/Kids` item, and updates `/Count` in place
    /// (`QPDF_pages.cc:145-183,254-275,303-319`). The page object itself remains in
    /// the document object cache, as it does in qpdf.
    ///
    /// # Errors
    ///
    /// - [`Error::QpdfExc`] when `page` is not a member of the repaired page
    ///   tree; the error retains the source description and raw page identity.
    /// - Any error propagated from page-tree repair, inherited-attribute
    ///   materialization, or live page-tree mutation.
    pub fn remove_page(&mut self, page: ObjectHandle) -> Result<()> {
        let (prepared, pages_root) = self.flatten_pages_tree()?;

        let page_obj_gen = page.get_obj_gen();
        let Some(index) = prepared
            .pages
            .iter()
            .position(|candidate| candidate.get_obj_gen() == page_obj_gen)
        else {
            // qpdf's findPage sets the last object description to `page
            // object` and throws qpdf_e_pages with the owning input filename
            // (`QPDF_pages.cc:303-319`). Keep its raw object/generation pair.
            return Err(self.page_not_in_tree_error(page_obj_gen));
        };

        let kids = pages_root.try_get_key(b"/Kids")?;
        kids.erase_array_item(index)?;
        let remaining_count = i64::try_from(kids.try_get_array_n_items()?).map_err(|_| {
            // cov:ignore-start: an in-memory page tree cannot allocate more than i64::MAX Kids entries.
            Error::Unsupported("page count exceeds qpdf's signed integer range".into())
        })?; // cov:ignore-end
        pages_root.replace_key(b"/Count", ObjectHandle::integer(remaining_count))?;

        let mut remaining_pages = prepared.pages;
        remaining_pages.remove(index);
        if remaining_pages.is_empty() {
            // qpdf's empty all_pages vector is the cache sentinel, and its
            // page-position map is empty again after the last page is erased.
            self.pdf.invalidate_page_list_cache();
        } else {
            self.pdf
                .cache_page_list(&crate::pages::repair::PreparedPages {
                    root: prepared.root,
                    pages: remaining_pages,
                });
            self.pdf.page_tree_flattened = true;
        }
        *self.pdf.acroform_cache.borrow_mut() = None;
        Ok(())
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

    /// Flatten annotations into their containing pages using qpdf's defaults.
    ///
    /// This maps `QPDFPageDocumentHelper::flattenAnnotations()` to
    /// `required_flags = 0` and `forbidden_flags = 3`, the bits for
    /// `an_invisible | an_hidden`
    /// (`include/qpdf/QPDFPageDocumentHelper.hh:104`,
    /// `include/qpdf/Constants.h:220-221`).
    pub fn flatten_annotations(&mut self) -> Result<()> {
        self.flatten_annotations_with_flags(0, 0x3)
    }

    /// Flatten annotations into their containing pages with explicit flag masks.
    ///
    /// Mirrors `QPDFPageDocumentHelper::flattenAnnotations`.
    ///
    /// An annotation is drawn only when all `required_flags` are set and none
    /// of `forbidden_flags` are set. As in qpdf, annotations with an
    /// appearance dictionary are removed even if no selected appearance can
    /// be drawn; annotations without one are retained.
    pub fn flatten_annotations_with_flags(
        &mut self,
        required_flags: i64,
        forbidden_flags: i64,
    ) -> Result<()> {
        // Keep enumeration inside the document-level operation so it can
        // follow qpdf's order: AcroForm analysis and NeedAppearances warning,
        // then `getAllPages()`, then per-page flattening. That page list must
        // stay on raw handles through annotation and resource mutation.
        crate::page_annotation_flatten::flatten_annotations_qpdf(
            self.pdf,
            required_flags,
            forbidden_flags,
        )
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
        let page = pdf.get_object_handle(page);
        assert!(matches!(
            PageDocumentHelper::new(&mut pdf)
                .append_flattened_page_for_job(PageInput::target(page), &mut Vec::new(),),
            Err(Error::Unsupported(_))
        ));

        let mut pdf = three_page_pdf();
        let page = crate::pages::page_refs(&mut pdf).expect("page refs")[0];
        let pages = ObjectHandle::dictionary(vec![(b"Kids".to_vec(), ObjectHandle::integer(1))]);
        pdf.root_handle()
            .expect("catalog")
            .replace_key(b"/Pages", pages)
            .expect("replace Pages root");
        let page = pdf.get_object_handle(page);
        assert!(matches!(
            PageDocumentHelper::new(&mut pdf)
                .append_flattened_page_for_job(PageInput::target(page), &mut Vec::new(),),
            Err(Error::Unsupported(_))
        ));
    }
}
