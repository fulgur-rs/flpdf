//! Page-tree traversal helpers.
//!
//! qpdf correspondence: QPDF_pages.cc traversal responsibilities shared with page-tree rebuild and linearization repair.
//!
//! Iterates the document's `/Pages` tree in the order described by ISO 32000-1 §7.7.3.2
//! and yields the `ObjectRef` of every leaf `Page` node. The walker tolerates broken
//! cycles (each node is visited at most once). An explicit depth limit remains
//! available for callers that request a bounded walk; the default qpdf-shaped
//! walk has no arbitrary depth cap.

#[cfg(not(feature = "qtest-driver"))]
pub(crate) mod repair;
#[cfg(feature = "qtest-driver")]
#[doc(hidden)]
pub mod repair;
pub mod tree_rebuild;

use crate::object_handle::ObjectHandleIdentity;
use crate::pipeline::buffer::Buffer;
use crate::{Error, ObjectHandle, ObjectRef, Pdf, Result};
use std::collections::{BTreeSet, HashSet};
use std::fmt;
use std::io::{Read, Seek};

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
thread_local! {
    static PAGE_WALK_VISITS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn page_walk_visits_for_test() -> usize {
    PAGE_WALK_VISITS.with(Cell::get)
}

/// Default recursion limit for [`page_refs`].
///
/// Real-world PDFs almost always fit within a couple of dozen levels; the limit is
/// generous enough for legitimate documents while still preventing pathological inputs
/// from causing unbounded recursion.
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
/// - [`Error::Missing`] when the catalog (`/Root`) or its `/Pages` entry is absent.
/// - [`Error::Unsupported`] when the catalog is not a dictionary.
/// - Any [`Error`] propagated from canonical ObjectHandle resolution while walking the tree.
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
    pdf.mark_get_all_pages_called();
    PageWalk::new(pdf)?.collect()
}

/// Like [`page_refs`] but with a caller-supplied recursion limit.
///
/// # Errors
///
/// - [`Error::Missing`] when the catalog (`/Root`) or its `/Pages` entry is absent.
/// - [`Error::Unsupported`] when the catalog is not a dictionary, or when the page
///   tree exceeds `max_depth`.
/// - Any [`Error`] propagated from canonical ObjectHandle resolution while walking the tree.
pub fn page_refs_with_max_depth<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    max_depth: usize,
) -> Result<Vec<ObjectRef>> {
    pdf.mark_get_all_pages_called();
    PageWalk::with_max_depth(pdf, max_depth)?.collect()
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

/// An iterator over every leaf `Page` object-reference in the document's `/Pages`
/// tree, yielding refs in document order (ISO 32000-1 §7.7.3.2).
///
/// Leaf classification for `/Kids` entries follows qpdf's
/// `getAllPagesInternal` (`libqpdf/QPDF_pages.cc:91-131`): an entry with
/// `/Kids` is a subtree and anything else is a page, dictionary or not. A
/// direct entry is promoted to an indirect page object (the `/Kids` entry is
/// rewritten) before it is yielded, which is what keeps a scalar leaf in the
/// page list. Subtree expansion here still keys on `/Type /Pages`, and the
/// remaining page-dictionary repair (`/Type` override, media-box default,
/// duplicate copying, repair warnings) stays with the canonical
/// `pages::repair` walk that [`crate::pages::page_refs`] uses when a prepared
/// page list exists.
///
/// Each node is visited at most once (tracked via a `BTreeSet`) so cycles in
/// malformed documents are silently skipped. On the first resolve failure or
/// depth-limit breach the iterator emits `Some(Err(...))` and is then fused
/// — all subsequent calls return `None`.
///
/// # Construction
///
/// Use [`PageWalk::new`] or [`PageWalk::with_max_depth`].
///
/// # Example
///
/// ```no_run
/// use std::fs::File;
/// use std::io::BufReader;
/// use flpdf::{pages::PageWalk, Pdf};
///
/// let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
/// for page_ref in PageWalk::new(&mut pdf)? {
///     let page_ref = page_ref?;
///     println!("page: {page_ref}");
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone)]
enum PageNode {
    Indirect(ObjectRef),
    Direct(ObjectHandle),
    /// A `/Kids` entry without `/Kids` of its own: qpdf's
    /// `getAllPagesInternal` treats every such entry as a page leaf,
    /// dictionary or not (`libqpdf/QPDF_pages.cc:91-131`).
    Leaf(ObjectRef),
}

impl PageNode {
    fn from_handle(handle: ObjectHandle) -> Self {
        match handle.object_ref() {
            Some(object_ref) => Self::Indirect(object_ref),
            None => Self::Direct(handle),
        }
    }

    fn handle<R: Read + Seek>(&self, pdf: &mut Pdf<R>) -> ObjectHandle {
        match self {
            Self::Indirect(object_ref) | Self::Leaf(object_ref) => {
                pdf.get_object_handle(*object_ref)
            }
            Self::Direct(handle) => handle.clone(),
        }
    }

    fn object_ref(&self) -> Option<ObjectRef> {
        match self {
            Self::Indirect(object_ref) | Self::Leaf(object_ref) => Some(*object_ref),
            Self::Direct(_) => None,
        }
    }

    fn label(&self) -> String {
        self.object_ref().map_or_else(
            || "direct page-tree object".to_owned(),
            |reference| reference.to_string(),
        )
    }
}

pub struct PageWalk<'a, R: Read + Seek + 'static> {
    pdf: &'a mut Pdf<R>,
    /// Stack of page-tree handles and depths yet to be visited. Direct
    /// `/Pages` dictionaries are valid qpdf children and therefore cannot be
    /// represented by `ObjectRef` alone.
    stack: Vec<(PageNode, usize)>,
    seen: BTreeSet<ObjectRef>,
    #[allow(
        clippy::mutable_key_type,
        reason = "direct page-tree cycle detection keys on canonical handle identity"
    )]
    seen_direct: HashSet<ObjectHandleIdentity>,
    max_depth: Option<usize>,
    /// Set to `true` after yielding `Err`; causes all subsequent calls to return `None`.
    done: bool,
}

/// Probe qpdf's `/Kids` containment boundary without turning a contextless
/// programmatic null into a hard error. Parsed document handles have a warning
/// sink and propagate sink failures; direct nulls created without a document
/// retain the pre-existing page-classification behavior.
fn probe_page_kids(handle: &ObjectHandle) -> Result<()> {
    let contextless = handle.context().is_none();
    match handle.try_has_key(b"/Kids") {
        Ok(_) => Ok(()),
        Err(Error::QpdfExc(_)) if contextless => Ok(()),
        Err(error) => Err(error),
    }
}

/// qpdf's `kid.hasKey("/Kids")` page-tree classification
/// (`libqpdf/QPDF_pages.cc:91-131`). A contextless programmatic handle has no
/// warning sink, so the type warning `try_has_key` raises there is mapped to
/// `false` the same way [`probe_page_kids`] maps it for the sibling probes.
fn page_kid_has_kids(handle: &ObjectHandle) -> Result<bool> {
    let contextless = handle.context().is_none();
    match handle.try_has_key(b"/Kids") {
        Ok(has_kids) => Ok(has_kids),
        Err(Error::QpdfExc(_)) if contextless => Ok(false),
        Err(error) => Err(error),
    }
}

impl<'a, R: Read + Seek> PageWalk<'a, R> {
    /// Create an unbounded qpdf-shaped `PageWalk`.
    ///
    /// # Errors
    ///
    /// - [`Error::Missing`] when the catalog (`/Root`) or its `/Pages` entry is absent.
    /// - [`Error::Unsupported`] when the catalog is not a dictionary.
    /// - Any [`Error`] propagated from canonical ObjectHandle resolution while resolving the catalog.
    pub fn new(pdf: &'a mut Pdf<R>) -> Result<Self> {
        // Only a *directly* absent `/Root` is `Missing` here. A present
        // `/Root` that resolves to a non-dictionary — including an indirect
        // reference to a free or missing object — is `QPDF::getRoot`'s
        // responsibility, which throws `unable to find /Root dictionary`
        // (`libqpdf/QPDF.cc:2354-2360`); resolving it here would preempt that
        // qpdf-shaped error with a `Missing`. `root_handle` implements
        // `getRoot`.
        let root = pdf.trailer_key_handle(b"Root");
        if !root.is_indirect() && root.try_is_null()? {
            return Err(Error::Missing("/Root"));
        }
        let catalog = pdf.root_handle()?;
        // Likewise for `/Pages`: qpdf's `getAllPages` reads it without a null
        // check and simply enumerates nothing when it carries no `/Kids`
        // (`libqpdf/QPDF_pages.cc:46,68-72`), so an indirect `/Pages`
        // resolving to null yields zero pages rather than an error.
        let pages = catalog.try_get_key(b"/Pages")?;
        if !pages.is_indirect() && pages.try_is_null()? {
            probe_page_kids(&pages)?;
            return Err(Error::Missing("/Pages"));
        }
        let pages = PageNode::from_handle(pages);
        Ok(PageWalk {
            pdf,
            stack: vec![(pages, 0)],
            seen: BTreeSet::new(),
            seen_direct: HashSet::new(),
            max_depth: None,
            done: false,
        })
    }

    /// Create a `PageWalk` with a caller-supplied recursion limit.
    ///
    /// # Errors
    ///
    /// - [`Error::Missing`] when the catalog (`/Root`) or its `/Pages` entry is absent.
    /// - [`Error::Unsupported`] when the catalog is not a dictionary.
    /// - Any [`Error`] propagated from canonical ObjectHandle resolution while resolving the catalog.
    pub fn with_max_depth(pdf: &'a mut Pdf<R>, max_depth: usize) -> Result<Self> {
        // Only a *directly* absent `/Root` is `Missing` here. A present
        // `/Root` that resolves to a non-dictionary — including an indirect
        // reference to a free or missing object — is `QPDF::getRoot`'s
        // responsibility, which throws `unable to find /Root dictionary`
        // (`libqpdf/QPDF.cc:2354-2360`); resolving it here would preempt that
        // qpdf-shaped error with a `Missing`. `root_handle` implements
        // `getRoot`.
        let root = pdf.trailer_key_handle(b"Root");
        if !root.is_indirect() && root.try_is_null()? {
            return Err(Error::Missing("/Root"));
        }
        let catalog = pdf.root_handle()?;
        // Likewise for `/Pages`: qpdf's `getAllPages` reads it without a null
        // check and simply enumerates nothing when it carries no `/Kids`
        // (`libqpdf/QPDF_pages.cc:46,68-72`), so an indirect `/Pages`
        // resolving to null yields zero pages rather than an error.
        let pages = catalog.try_get_key(b"/Pages")?;
        if !pages.is_indirect() && pages.try_is_null()? {
            probe_page_kids(&pages)?;
            return Err(Error::Missing("/Pages"));
        }
        let pages = PageNode::from_handle(pages);
        Ok(PageWalk {
            pdf,
            stack: vec![(pages, 0)],
            seen: BTreeSet::new(),
            seen_direct: HashSet::new(),
            max_depth: Some(max_depth),
            done: false,
        })
    }

    fn visit_node(&mut self, node: &PageNode, depth: usize) -> Result<Option<ObjectRef>> {
        let node_obj = node.handle(self.pdf);
        node_obj.try_dereference()?;

        if !node_obj.try_is_dictionary()? {
            probe_page_kids(&node_obj)?;
            return Ok(None); // non-dictionary: skip silently
        }

        let node_type = node_obj.try_get_key(b"/Type")?;

        if node_type.try_as_name()?.as_deref() == Some(b"Pages") {
            let kids = node_obj.try_get_key(b"/Kids")?;
            if let Some(kids_items) = kids.try_as_array()? {
                // qpdf classifies an entry by `/Kids` containment, not
                // `/Type`: an entry with `/Kids` is a subtree and everything
                // else is a page leaf even when it is not a dictionary
                // (`libqpdf/QPDF_pages.cc:91-131`). Subtree expansion itself
                // still keys on `/Type /Pages` in `visit_node`.
                //
                // Classify -- and in particular promote -- in forward order.
                // qpdf allocates each promoted leaf during its forward
                // depth-first walk (`libqpdf/QPDF_pages.cc:95-101`), so
                // `/Kids [42 43]` must mint the object for `42` before the one
                // for `43`; `pages/repair.rs` does the same. Only the stack
                // insertion is reversed afterwards, so the first kid is still
                // popped first.
                let mut classified = Vec::with_capacity(kids_items.len());
                for (index, kid) in kids_items.iter().enumerate() {
                    if page_kid_has_kids(kid)? {
                        if let Some(r) = kid.object_ref() {
                            classified.push(PageNode::Indirect(r));
                        } else {
                            classified.push(PageNode::Direct(kid.clone()));
                        }
                    } else if let Some(r) = kid.object_ref() {
                        classified.push(PageNode::Leaf(r));
                    } else {
                        // qpdf promotes every direct kid to an indirect page
                        // object before pushing it (`libqpdf/QPDF_pages.cc:95-101`),
                        // which is also what keeps a non-dictionary leaf in
                        // the page list.
                        let promoted = self.pdf.make_indirect_object_handle(kid.clone())?;
                        kids.set_array_item(index, promoted.clone())?;
                        let promoted_ref = promoted
                            .object_ref()
                            .expect("make_indirect_object_handle returns an indirect handle");
                        classified.push(PageNode::Leaf(promoted_ref));
                    }
                }
                for node in classified.into_iter().rev() {
                    self.stack.push((node, depth + 1));
                }
            }
            return Ok(None);
        }

        if node_type.try_as_name()?.as_deref() == Some(b"Page") {
            return Ok(node.object_ref());
        }

        Ok(None)
    }
}

impl<'a, R: Read + Seek> Iterator for PageWalk<'a, R> {
    type Item = Result<ObjectRef>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }

        loop {
            let (node, depth) = self.stack.pop()?;

            #[cfg(test)]
            PAGE_WALK_VISITS.with(|visits| visits.set(visits.get() + 1));

            if self.max_depth.is_some_and(|max_depth| depth >= max_depth) {
                self.done = true;
                return Some(Err(Error::Unsupported(format!(
                    "page tree depth exceeds maximum of {} at {}",
                    self.max_depth.expect("checked above"),
                    node.label()
                ))));
            }

            let first_visit = match &node {
                PageNode::Indirect(reference) | PageNode::Leaf(reference) => {
                    self.seen.insert(*reference)
                }
                PageNode::Direct(handle) => self.seen_direct.insert(handle.identity_key()),
            };
            if !first_visit {
                continue; // cycle guard: already visited
            }

            let visited = match &node {
                PageNode::Leaf(reference) => Ok(Some(*reference)),
                PageNode::Indirect(_) | PageNode::Direct(_) => self.visit_node(&node, depth),
            };
            match visited {
                Ok(Some(page)) => return Some(Ok(page)),
                Ok(None) => continue,
                Err(error) => {
                    self.done = true;
                    return Some(Err(error));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unbounded_page_walk_reports_a_missing_root() {
        let mut pdf = Pdf::<std::io::Cursor<Vec<u8>>>::uninitialized();
        assert!(matches!(
            PageWalk::new(&mut pdf),
            Err(Error::Missing("/Root"))
        ));
    }

    #[test]
    fn unbounded_page_walk_reports_a_missing_pages_entry() {
        let mut pdf = Pdf::empty().expect("empty PDF");
        let catalog = pdf.root_handle().expect("empty catalog");
        catalog.remove_key(b"/Pages");
        assert!(matches!(
            PageWalk::new(&mut pdf),
            Err(Error::Missing("/Pages"))
        ));
        let messages: Vec<_> = pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|entry| entry.message_string())
            .collect();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains(
            "operation for dictionary attempted on object of type null: returning false for a key containment request"
        ));
    }

    #[test]
    fn bounded_page_walk_reports_a_missing_pages_entry() {
        let mut pdf = Pdf::empty().expect("empty PDF");
        pdf.root_handle()
            .expect("empty catalog")
            .remove_key(b"/Pages");
        assert!(matches!(
            PageWalk::with_max_depth(&mut pdf, 4),
            Err(Error::Missing("/Pages"))
        ));
    }

    #[test]
    fn page_kids_probe_propagates_uninitialized_handle_errors() {
        let error = probe_page_kids(&ObjectHandle::uninitialized()).unwrap_err();
        assert!(matches!(error, Error::Internal(_)));
    }

    #[test]
    fn page_kid_classification_propagates_uninitialized_handle_errors() {
        let error = page_kid_has_kids(&ObjectHandle::uninitialized()).unwrap_err();
        assert!(matches!(error, Error::Internal(_)));
    }

    #[test]
    fn explicit_page_walk_limit_remains_available() {
        let mut pdf = Pdf::empty().expect("empty PDF");
        let mut walk = PageWalk::with_max_depth(&mut pdf, 0).expect("empty pages root");
        let error = walk
            .next()
            .expect("bounded walk must report an error")
            .unwrap_err();
        assert!(error.to_string().contains("depth exceeds maximum of 0"));
    }

    #[test]
    fn page_refs_reuses_the_nonempty_prepared_page_cache() {
        let mut pdf = Pdf::open_mem_owned(
            include_bytes!("../../../tests/fixtures/compat/one-page.pdf").to_vec(),
        )
        .expect("one-page fixture");
        let prepared = crate::pages::repair::prepare_for_optimization(&mut pdf)
            .expect("prepare pages")
            .expect("prepared page list");
        let before = page_walk_visits_for_test();

        let actual = page_refs(&mut pdf).expect("cached page refs");

        assert_eq!(
            actual,
            prepared
                .pages
                .iter()
                .map(|page| page.object_ref().expect("page identity"))
                .collect::<Vec<_>>()
        );
        assert_eq!(page_walk_visits_for_test(), before);
    }

    #[test]
    fn page_walk_expands_a_direct_pages_kid() {
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
        let mut pdf = Pdf::empty().expect("empty PDF");
        let pages = pdf
            .root_handle()
            .expect("empty catalog")
            .try_get_key(b"/Pages")
            .expect("empty /Pages");
        let kids = ObjectHandle::array(vec![ObjectHandle::integer(42), ObjectHandle::integer(43)]);
        pages
            .replace_key(b"/Kids", kids.clone())
            .expect("install scalar kids");
        pages
            .replace_key(b"/Count", ObjectHandle::integer(2))
            .expect("install count");

        let refs = page_refs(&mut pdf).expect("two scalar leaves are two pages");

        assert_eq!(refs.len(), 2);
        assert!(
            refs[0].number < refs[1].number,
            "the first kid must mint the lower object number, got {refs:?}"
        );
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
    fn page_walk_promotes_a_direct_scalar_kid_like_qpdf() {
        let mut pdf = Pdf::empty().expect("empty PDF");
        let pages = pdf
            .root_handle()
            .expect("empty catalog")
            .try_get_key(b"/Pages")
            .expect("empty /Pages");
        let kids = ObjectHandle::array(vec![ObjectHandle::integer(42)]);
        pages
            .replace_key(b"/Kids", kids.clone())
            .expect("install scalar kid");
        pages
            .replace_key(b"/Count", ObjectHandle::integer(1))
            .expect("install count");

        let refs = page_refs(&mut pdf).expect("a scalar leaf is a page");
        assert_eq!(refs.len(), 1, "qpdf counts the scalar leaf as one page");
        assert_eq!(
            pdf.get_object_handle(refs[0])
                .try_as_integer()
                .expect("resolve promoted leaf"),
            Some(42)
        );
        assert!(
            kids.try_get_array_item(0)
                .expect("rewritten kid")
                .is_indirect(),
            "qpdf promotes a direct kid to an indirect page object"
        );
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
