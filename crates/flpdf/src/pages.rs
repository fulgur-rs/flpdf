//! Page-tree traversal helpers.
//!
//! qpdf correspondence: `QPDF::getAllPages` / `getAllPagesInternal` page-list repair and cache responsibilities.
//!
//! Returns the document's qpdf-ordered page list through
//! [`crate::PageDocumentHelper::get_all_pages`], including qpdf's page-tree repairs.

#[cfg(not(feature = "qtest-driver"))]
pub(crate) mod repair;
#[cfg(feature = "qtest-driver")]
#[doc(hidden)]
pub mod repair;
pub mod tree_rebuild;

use crate::pipeline::buffer::Buffer;
use crate::{Error, ObjectHandle, ObjectRef, Pdf, Result};
use std::fmt;
use std::io::{Read, Seek};

/// Default depth bound used by flpdf page-tree ancestor and mutation helpers.
///
/// qpdf's `getAllPages` walk is unbounded and uses visited-object detection instead.
/// This constant remains for flpdf APIs that expose their own depth-limited operations.
pub const DEFAULT_MAX_PAGE_TREE_DEPTH: usize = 100;

/// A qpdf-style page-tree dictionary handle while following `/Parent`.
///
/// `QPDFObjectHandle` keeps direct dictionaries addressable just like indirect
/// objects. Keep the handle itself so direct parents retain their live identity
/// and indirect parents retain their canonical object reference while walking.
#[derive(Debug, Clone)]
pub(crate) struct PageParentCursor {
    handle: ObjectHandle,
}

impl PageParentCursor {
    pub(crate) fn from_handle(handle: ObjectHandle) -> Self {
        Self { handle }
    }

    pub(crate) fn handle(&self) -> ObjectHandle {
        self.handle.clone()
    }
}

impl fmt::Display for PageParentCursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.handle.object_ref() {
            Some(reference) => write!(f, "{reference}"),
            None => f.write_str("direct page-tree dictionary"),
        }
    }
}

/// Snapshot `key` and `/Parent` from a page-tree dictionary cursor.
///
/// Always attempts both lookups, even on a non-dictionary node: qpdf's own
/// loop (`QPDFPageObjectHelper.cc:236-247`) calls `node.getKey(name)`
/// unconditionally after advancing, and `QPDFObjectHandle::getKey`
/// (`libqpdf/QPDFObjectHandle.cc:978-989`) reports a type warning and
/// returns null on a non-dictionary receiver rather than silently skipping
/// the access. [`ObjectHandle::try_get_key`] carries that same behavior, so
/// short-circuiting here would drop the diagnostic.
pub(crate) fn page_parent_entries(
    cursor: &PageParentCursor,
    key: &[u8],
) -> Result<Option<(ObjectHandle, ObjectHandle)>> {
    let dict = cursor.handle();
    dict.try_dereference()?;
    Ok(Some((
        dict.try_get_key(key)?,
        dict.try_get_key(b"/Parent")?,
    )))
}

/// Advance a page-tree parent cursor when `/Parent` is a dictionary handle.
pub(crate) fn next_page_parent(parent: ObjectHandle) -> Result<Option<PageParentCursor>> {
    // Only a *direct* null terminates here. Resolving an indirect parent at
    // this point would parse one node past the caller's depth guard, so the
    // deferral below has to see it still unresolved. qpdf has no depth bound
    // in `getAttribute` at all — it terminates on its `QPDFObjGen::set seen`
    // (`libqpdf/QPDFPageObjectHelper.cc:236-247`) — so this budget is a
    // flpdf-only guard whose strength depends on not resolving early.
    if !parent.is_indirect() && parent.try_is_null()? {
        return Ok(None);
    }
    // Keep only a genuinely unresolved indirect parent as a cursor, so the
    // caller's next loop iteration can apply its depth guard before
    // resolving the boundary node. `is_indirect()` reflects identity, not
    // resolution state, so an indirect handle already resolved (from an
    // earlier, unrelated read) to a non-dictionary value must be rejected
    // here rather than deferred — deferring it would let a malformed
    // chain surface as a depth-limit error instead of terminating cleanly,
    // and whether that happens would depend on incidental cache state.
    if parent.is_indirect() && !parent.is_resolved() {
        return Ok(Some(PageParentCursor::from_handle(parent)));
    }
    if !parent.try_is_dictionary()? {
        return Ok(None);
    }
    Ok(Some(PageParentCursor::from_handle(parent)))
}

/// Return whether qpdf permits `key` to inherit through a page `/Parent` chain.
///
/// qpdf's `QPDFPageObjectHelper::getAttribute` permits inheritance only for
/// `/MediaBox`, `/CropBox`, `/Resources`, and `/Rotate`
/// (`libqpdf/QPDFPageObjectHelper.cc:224-237`).
pub(crate) fn is_inheritable_page_attribute(key: &[u8]) -> bool {
    matches!(key, b"/MediaBox" | b"/CropBox" | b"/Resources" | b"/Rotate")
}

/// Resolve a page attribute from a live page-tree node and its ancestors.
///
/// This is the shared qpdf-shaped parent walk used by both page-tree
/// consumers and [`crate::PageObjectHelper`]. The caller supplies the starting
/// node so Form XObjects can keep qpdf's non-inheriting `getAttribute` path.
pub(crate) fn resolve_inherited_handle_from_node_with_max_depth(
    node: ObjectHandle,
    key: &[u8],
    max_depth: usize,
) -> Result<Option<ObjectHandle>> {
    let mut seen: Vec<ObjectHandle> = Vec::new();
    let mut current = PageParentCursor::from_handle(node);
    let mut depth = 0usize;
    let inheritable = is_inheritable_page_attribute(key);

    loop {
        if depth >= max_depth {
            return Err(Error::Unsupported(format!(
                "page tree depth exceeds maximum of {max_depth} at {current}"
            )));
        }
        let current_handle = current.handle();
        if seen
            .iter()
            .any(|seen_handle| seen_handle.is_same_object_as(&current_handle))
        {
            return Ok(None);
        }
        seen.push(current_handle);

        let Some((value, parent)) = page_parent_entries(&current, key)? else {
            return Ok(None);
        };
        // Resolve the live value before classifying it under qpdf's
        // null-as-absent inheritance rule.
        let terminal = value.clone();
        terminal.try_dereference()?;
        if !terminal.try_is_null()? {
            return Ok(Some(value));
        }

        if !inheritable {
            return Ok(None);
        }
        let Some(parent) = next_page_parent(parent)? else {
            return Ok(None);
        };
        current = parent;
        depth += 1;
    }
}

/// Resolve the first non-null inherited value for an indirect page object.
///
/// This is the canonical shared parent walk. The legacy public resource helper
/// below still materializes its return type for existing callers; new page
/// consumers must use this handle-native boundary instead.
pub(crate) fn resolve_inherited_handle_with_max_depth<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    key: &[u8],
    max_depth: usize,
) -> Result<Option<ObjectHandle>> {
    let page = pdf.get_object_handle(page_ref);
    resolve_inherited_handle_from_node_with_max_depth(page, key, max_depth)
}

/// Return every `Page` object in document order using qpdf's unbounded default walk.
///
/// # Errors
///
/// - [`Error::Missing`] when the trailer has no `/Root` entry.
/// - [`Error::Unsupported`] when `/Root` is not a dictionary.
/// - A missing `/Pages` entry yields an empty list after qpdf's containment warning.
/// - An explicit null or other invalid `/Pages` value propagates its qpdf type error.
/// - Any [`Error`] propagated from canonical ObjectHandle resolution while repairing the tree.
///
/// # Examples
///
/// ```no_run
/// use std::fs::File;
/// use std::io::BufReader;
/// use flpdf::{pages, Pdf};
///
/// let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
/// let pages = pages::page_refs(&mut pdf)?;
/// println!("{} pages", pages.len());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn page_refs<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<Vec<ObjectRef>> {
    if let Some(prepared) = pdf.cached_page_list() {
        pdf.mark_get_all_pages_called();
        return prepared.page_refs();
    }
    crate::PageDocumentHelper::new(pdf).get_all_pages()
}

/// Return the decoded content-stream bytes for a single `Page` object.
///
/// The page's `/Contents` entry may be absent or null (returns `Ok(Vec::new())`),
/// a single `Stream` or `Reference → Stream`, or an `Array` of such references.
/// Content is decoded and coalesced through the canonical
/// [`crate::ObjectHandle::pipe_page_contents`] route, which resolves indirect
/// `/Filter` and `/DecodeParms` values at the same boundary as qpdf. A single
/// `\n` is inserted before a stream only when the previous decoded stream did not
/// already end in a newline (an empty stream, whose last byte is treated as 0,
/// still forces the separator). No trailing newline is appended.
///
/// # Errors
///
/// - [`Error::Unsupported`] when `page_ref` does not resolve to a dictionary with
///   `/Type /Page`, or when a content stream cannot be decoded.
/// - Any [`Error`] that canonical ObjectHandle resolution or the canonical content
///   pipeline may return.
///
/// # Examples
///
/// ```no_run
/// use std::fs::File;
/// use std::io::BufReader;
/// use flpdf::{pages, Pdf};
///
/// let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
/// let page_refs = pages::page_refs(&mut pdf)?;
/// if let Some(&page_ref) = page_refs.first() {
///     let content = pages::page_content_bytes(&mut pdf, page_ref)?;
///     println!("{} content bytes", content.len());
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn page_content_bytes<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<Vec<u8>> {
    let page = pdf.get_object_handle(page_ref);
    page.try_dereference()?;
    if !page.try_is_dictionary()? {
        return Err(Error::Unsupported(format!(
            "object {page_ref} is not a dictionary, cannot extract /Contents"
        )));
    }

    // Verify the /Type is /Page.
    let page_type = page.try_get_key(b"/Type")?;
    match page_type.try_as_name()? {
        Some(name) if name.as_slice() == b"Page" => {}
        Some(name) => {
            return Err(Error::Unsupported(format!(
                "object {page_ref} has /Type /{}, expected /Page",
                String::from_utf8_lossy(&name)
            )));
        }
        None if page.try_has_key(b"/Type")? => {
            return Err(Error::Unsupported(format!(
                "object {page_ref} has a non-name /Type entry"
            )));
        }
        None => {
            return Err(Error::Unsupported(format!(
                "object {page_ref} has no /Type entry"
            )));
        }
    }

    let streams = page.get_page_contents()?;
    if streams.is_empty() {
        return Ok(Vec::new());
    }

    let normalized_contents = ObjectHandle::array(streams);
    let mut buffer = Buffer::new("page content bytes", None);
    let mut all_description = String::new();
    normalized_contents.pipe_content_streams(
        &mut buffer,
        &format!("page object {page_ref}"),
        &mut all_description,
    )?;
    Ok(buffer.take_buffer()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pdf_with_objects(objects: &[&str]) -> Vec<u8> {
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::with_capacity(objects.len());
        for (index, object) in objects.iter().enumerate() {
            offsets.push(bytes.len());
            bytes.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
        }
        let xref_offset = bytes.len();
        bytes.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        bytes
    }

    fn pdf_with_catalog(catalog: &str) -> Vec<u8> {
        pdf_with_objects(&[catalog])
    }

    #[test]
    fn page_refs_returns_an_empty_list_when_catalog_has_no_pages_entry() {
        let mut pdf = Pdf::open_mem_owned(pdf_with_catalog("<< /Type /Catalog >>"))
            .expect("open Catalog without /Pages");

        let actual = page_refs(&mut pdf).expect("qpdf returns an empty page list");

        assert!(actual.is_empty());
        let warnings = pdf.repair_diagnostics();
        assert_eq!(warnings.len(), 1);
        assert_eq!(
            warnings.entries()[0].get_message_detail(),
            b"operation for dictionary attempted on object of type null: returning false for a key containment request"
        );
    }

    #[test]
    fn page_refs_preserves_qpdf_type_error_for_a_direct_null_pages_entry() {
        let mut pdf = Pdf::open_mem_owned(pdf_with_catalog("<< /Type /Catalog /Pages null >>"))
            .expect("open Catalog with direct null /Pages");

        let error = page_refs(&mut pdf).expect_err("qpdf rejects direct null /Pages");

        match error {
            Error::QpdfExc(error) => assert_eq!(
                error.get_message_detail(),
                b"operation for dictionary attempted on object of type null: returning false for a key containment request"
            ),
            other => panic!("expected qpdf type error, got {other:?}"), // cov:ignore: this is the failing arm of the qpdf error-type regression assertion
        }
    }

    #[test]
    fn page_refs_keeps_qpdfs_nonempty_cache_until_explicit_refresh() {
        let mut pdf = Pdf::open_mem_owned(
            include_bytes!("../../../tests/fixtures/compat/one-page.pdf").to_vec(),
        )
        .expect("one-page fixture");
        let prepared = crate::pages::repair::prepare_for_optimization(&mut pdf)
            .expect("prepare pages")
            .expect("prepared page list");
        let original = prepared
            .pages
            .iter()
            .map(|page| page.object_ref().expect("page identity"))
            .collect::<Vec<_>>();
        let pages = pdf
            .root_handle()
            .expect("Catalog")
            .try_get_key(b"/Pages")
            .expect("page-tree root");
        pages
            .replace_key(b"/Kids", ObjectHandle::array(Vec::new()))
            .expect("mutate the live page tree without refreshing its cache");

        let actual = page_refs(&mut pdf).expect("cached page refs");

        assert_eq!(actual, original);
    }

    #[test]
    fn page_refs_expands_a_page_typed_node_with_kids_and_repairs_its_type() {
        let mut pdf = Pdf::empty().expect("empty PDF");
        let pages = pdf
            .root_handle()
            .expect("empty catalog")
            .try_get_key(b"/Pages")
            .expect("empty /Pages");
        let leaf = pdf
            .make_indirect_object_handle(ObjectHandle::dictionary(vec![(
                b"/Type".to_vec(),
                ObjectHandle::name(b"Page".to_vec()),
            )]))
            .expect("indirect page leaf");
        let page_typed_subtree = pdf
            .make_indirect_object_handle(ObjectHandle::dictionary(vec![
                (b"/Type".to_vec(), ObjectHandle::name(b"Page".to_vec())),
                (b"/Kids".to_vec(), ObjectHandle::array(vec![leaf.clone()])),
                (b"/Count".to_vec(), ObjectHandle::integer(1)),
            ]))
            .expect("indirect subtree mislabeled as a page");
        let leaf_ref = leaf.object_ref().expect("page leaf identity");
        let subtree_ref = page_typed_subtree.object_ref().expect("subtree identity");
        pages
            .replace_key(
                b"/Kids",
                ObjectHandle::array(vec![page_typed_subtree.clone()]),
            )
            .expect("install subtree");
        pages
            .replace_key(b"/Count", ObjectHandle::integer(1))
            .expect("install page count");

        let actual = page_refs(&mut pdf).expect("enumerate repaired page tree");

        assert_eq!(actual, vec![leaf_ref]);
        assert!(!actual.contains(&subtree_ref));
        assert_eq!(
            page_typed_subtree
                .try_get_key(b"/Type")
                .expect("repaired subtree type")
                .try_as_name()
                .expect("name type"),
            Some(b"Pages".to_vec())
        );
        assert!(pdf.repair_diagnostics().entries().iter().any(|warning| {
            warning.get_message_detail() == b"/Type key should be /Pages but is not; overriding"
        }));
    }

    #[test]
    fn page_refs_repairs_a_type_less_dictionary_leaf_before_returning_it() {
        let mut pdf = Pdf::empty().expect("empty PDF");
        let pages = pdf
            .root_handle()
            .expect("empty catalog")
            .try_get_key(b"/Pages")
            .expect("empty /Pages");
        pages.remove_key(b"/MediaBox");
        let leaf = pdf
            .make_indirect_object_handle(ObjectHandle::dictionary(Vec::new()))
            .expect("indirect dictionary leaf");
        let leaf_ref = leaf.object_ref().expect("page leaf identity");
        pages
            .replace_key(b"/Kids", ObjectHandle::array(vec![leaf.clone()]))
            .expect("install leaf");
        pages
            .replace_key(b"/Count", ObjectHandle::integer(1))
            .expect("install page count");

        let actual = page_refs(&mut pdf).expect("enumerate and repair page tree");

        assert_eq!(actual, vec![leaf_ref]);
        assert_eq!(
            leaf.try_get_key(b"/Type")
                .expect("repaired leaf type")
                .try_as_name()
                .expect("name type"),
            Some(b"Page".to_vec())
        );
        let media_box = leaf
            .try_get_key(b"/MediaBox")
            .expect("default leaf MediaBox")
            .try_as_array()
            .expect("array MediaBox")
            .expect("default rectangle");
        let rectangle = media_box
            .iter()
            .map(|coordinate| coordinate.try_as_integer().expect("integer coordinate"))
            .collect::<Vec<_>>();
        assert_eq!(rectangle, vec![Some(0), Some(0), Some(612), Some(792)]);
        let warnings = pdf.repair_diagnostics();
        let details = warnings
            .entries()
            .iter()
            .map(|warning| warning.get_message_detail())
            .collect::<Vec<_>>();
        let media_warning = details
            .iter()
            .position(|detail| detail.starts_with(b"kid 0 (from 0) MediaBox is undefined"))
            .expect("qpdf MediaBox warning");
        let type_warning = details
            .iter()
            .position(|detail| *detail == b"/Type key should be /Page but is not; overriding")
            .expect("qpdf leaf type warning");
        assert!(
            media_warning < type_warning,
            "qpdf repairs MediaBox before /Type"
        );
    }

    #[test]
    fn page_refs_clones_each_occurrence_of_a_duplicate_scalar_leaf() {
        let mut pdf = Pdf::empty().expect("empty PDF");
        let pages = pdf
            .root_handle()
            .expect("empty catalog")
            .try_get_key(b"/Pages")
            .expect("empty /Pages");
        let scalar = pdf
            .make_indirect_object_handle(ObjectHandle::integer(42))
            .expect("indirect scalar leaf");
        let first_ref = scalar.object_ref().expect("original leaf identity");
        let kids = ObjectHandle::array(vec![scalar.clone(), scalar.clone()]);
        pages
            .replace_key(b"/Kids", kids.clone())
            .expect("install duplicate leaves");
        pages
            .replace_key(b"/Count", ObjectHandle::integer(2))
            .expect("install page count");

        let actual = page_refs(&mut pdf).expect("enumerate duplicate leaves");

        assert_eq!(actual.len(), 2, "each /Kids slot is a page occurrence");
        assert_eq!(actual[0], first_ref);
        assert_eq!(actual, vec![first_ref, ObjectRef::new(4, 0)]);
        assert_ne!(actual[0], actual[1], "qpdf shallow-copies duplicate leaves");
        for reference in &actual {
            assert_eq!(
                pdf.get_object_handle(*reference)
                    .try_as_integer()
                    .expect("resolve page leaf"),
                Some(42)
            );
        }
        assert_eq!(
            kids.try_get_array_item(1)
                .expect("updated duplicate slot")
                .object_ref(),
            Some(actual[1])
        );
        assert!(pdf.repair_diagnostics().entries().iter().any(|warning| {
            warning.get_message_detail()
                == b"kid 1 (from 0) appears more than once in the pages tree; creating a new page object as a copy"
        }));
    }

    #[test]
    fn page_refs_promotes_nested_direct_kids_in_depth_first_order() {
        let mut pdf = Pdf::open_mem_owned(pdf_with_objects(&[
            "<< /Type /Catalog /Pages 2 0 R >>",
            "<< /Type /Pages /Kids [<< /Type /Pages /Kids [42] /Count 1 >> 43] /Count 2 /MediaBox [0 0 10 10] >>",
        ]))
        .expect("open nested direct subtree fixture");

        let actual = page_refs(&mut pdf).expect("enumerate nested direct leaves");

        assert_eq!(actual, vec![ObjectRef::new(3, 0), ObjectRef::new(4, 0)]);
        let values = actual
            .iter()
            .map(|reference| {
                pdf.get_object_handle(*reference)
                    .try_as_integer()
                    .expect("resolve promoted scalar leaf")
            })
            .collect::<Vec<_>>();
        assert_eq!(values, vec![Some(42), Some(43)]);
    }

    #[test]
    fn page_refs_expands_a_direct_pages_kid() {
        let mut pdf = Pdf::empty().expect("empty PDF");
        let root_pages = pdf
            .root_handle()
            .expect("empty catalog")
            .try_get_key(b"/Pages")
            .expect("empty /Pages");
        let page = pdf
            .make_indirect_object_handle(ObjectHandle::dictionary(vec![(
                b"/Type".to_vec(),
                ObjectHandle::name(b"Page".to_vec()),
            )]))
            .expect("indirect page");
        let direct_subtree = ObjectHandle::dictionary(vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"Pages".to_vec())),
            (b"/Kids".to_vec(), ObjectHandle::array(vec![page.clone()])),
        ]);
        root_pages
            .replace_key(b"/Kids", ObjectHandle::array(vec![direct_subtree]))
            .expect("install direct subtree");
        root_pages
            .replace_key(b"/Count", ObjectHandle::integer(1))
            .expect("install count");

        let refs = page_refs(&mut pdf).expect("direct subtree is expanded");
        assert_eq!(
            refs,
            vec![page.object_ref().expect("indirect page identity")]
        );
    }

    /// qpdf allocates each promoted leaf during its forward depth-first walk
    /// (`libqpdf/QPDF_pages.cc:95-101`), so the first kid gets the lower fresh
    /// object number. Classifying right-to-left would swap them and change the
    /// object numbering a caller observes.
    #[test]
    fn direct_scalar_kids_are_promoted_in_forward_order() {
        let mut pdf = Pdf::open_mem_owned(pdf_with_objects(&[
            "<< /Type /Catalog /Pages 2 0 R >>",
            "<< /Type /Pages /Kids [42 43] /Count 2 /MediaBox [0 0 10 10] >>",
        ]))
        .expect("open direct scalar leaf fixture");

        let refs = page_refs(&mut pdf).expect("two scalar leaves are two pages");

        assert_eq!(refs, vec![ObjectRef::new(3, 0), ObjectRef::new(4, 0)]);
        let values: Vec<_> = refs
            .iter()
            .map(|r| {
                pdf.get_object_handle(*r)
                    .try_as_integer()
                    .expect("resolve promoted leaf")
            })
            .collect();
        assert_eq!(values, vec![Some(42), Some(43)]);
    }

    #[test]
    fn inherited_attribute_walk_propagates_an_unresolved_parent_child_error() {
        let node = ObjectHandle::dictionary(vec![
            (b"/MediaBox".to_vec(), ObjectHandle::null()),
            (
                b"/Parent".to_vec(),
                ObjectHandle::new_indirect_unresolved(ObjectRef::new(99, 0), -1),
            ),
        ]);
        let error =
            resolve_inherited_handle_from_node_with_max_depth(node, b"/MediaBox", 4).unwrap_err();
        assert!(error
            .to_string()
            .contains("object 99 0 belongs to a dropped PDF"));
    }
}
