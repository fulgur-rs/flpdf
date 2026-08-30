//! qpdf correspondence: QPDF_pages.cc traversal responsibilities shared with page-tree rebuild and linearization repair.
//! Page-tree traversal helpers.
//!
//! Iterates the document's `/Pages` tree in the order described by ISO 32000-1 §7.7.3.2
//! and yields the `ObjectRef` of every leaf `Page` node. The walker tolerates broken
//! cycles (each node is visited at most once) and bounds its recursion via a configurable
//! depth limit, since malformed PDFs occasionally embed self-referential page trees.

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
pub(crate) fn page_parent_entries<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    cursor: &PageParentCursor,
    key: &[u8],
) -> Result<Option<(ObjectHandle, ObjectHandle)>> {
    let dict = cursor.handle();
    pdf.resolve(&dict)?;
    Ok(Some((
        dict.try_get_key(key)?,
        dict.try_get_key(b"/Parent")?,
    )))
}

/// Advance a page-tree parent cursor when `/Parent` is a dictionary handle.
pub(crate) fn next_page_parent(parent: ObjectHandle) -> Result<Option<PageParentCursor>> {
    if parent.is_null() {
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
    if parent.as_dictionary().is_none() {
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
pub(crate) fn resolve_inherited_handle_from_node_with_max_depth<R: Read + Seek>(
    pdf: &mut Pdf<R>,
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

        let Some((value, parent)) = page_parent_entries(pdf, &current, key)? else {
            return Ok(None);
        };
        // Resolve the live value before classifying it under qpdf's
        // null-as-absent inheritance rule.
        let terminal = pdf.resolve_handle(&value)?;
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
    resolve_inherited_handle_from_node_with_max_depth(pdf, page, key, max_depth)
}

/// Return every `Page` object in document order using [`DEFAULT_MAX_PAGE_TREE_DEPTH`].
///
/// # Errors
///
/// - [`Error::Missing`] when the catalog (`/Root`) or its `/Pages` entry is absent.
/// - [`Error::Unsupported`] when the catalog is not a dictionary, or when the page
///   tree exceeds [`DEFAULT_MAX_PAGE_TREE_DEPTH`].
/// - Any [`Error`] propagated from [`Pdf::resolve`] while walking the tree.
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
    page_refs_with_max_depth(pdf, DEFAULT_MAX_PAGE_TREE_DEPTH)
}

/// Like [`page_refs`] but with a caller-supplied recursion limit.
///
/// # Errors
///
/// - [`Error::Missing`] when the catalog (`/Root`) or its `/Pages` entry is absent.
/// - [`Error::Unsupported`] when the catalog is not a dictionary, or when the page
///   tree exceeds `max_depth`.
/// - Any [`Error`] propagated from [`Pdf::resolve`] while walking the tree.
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
/// - Any [`Error`] that [`Pdf::resolve`] or the canonical content
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
    pdf.resolve(&page)?;
    if page.as_dictionary().is_none() {
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
        None if page.has_key(b"/Type") => {
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
            Self::Indirect(object_ref) => pdf.get_object_handle(*object_ref),
            Self::Direct(handle) => handle.clone(),
        }
    }

    fn object_ref(&self) -> Option<ObjectRef> {
        match self {
            Self::Indirect(object_ref) => Some(*object_ref),
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
    max_depth: usize,
    /// Set to `true` after yielding `Err`; causes all subsequent calls to return `None`.
    done: bool,
}

impl<'a, R: Read + Seek> PageWalk<'a, R> {
    /// Create a `PageWalk` using [`DEFAULT_MAX_PAGE_TREE_DEPTH`].
    ///
    /// # Errors
    ///
    /// - [`Error::Missing`] when the catalog (`/Root`) or its `/Pages` entry is absent.
    /// - [`Error::Unsupported`] when the catalog is not a dictionary.
    /// - Any [`Error`] propagated from [`Pdf::resolve`] while resolving the catalog.
    pub fn new(pdf: &'a mut Pdf<R>) -> Result<Self> {
        Self::with_max_depth(pdf, DEFAULT_MAX_PAGE_TREE_DEPTH)
    }

    /// Create a `PageWalk` with a caller-supplied recursion limit.
    ///
    /// # Errors
    ///
    /// - [`Error::Missing`] when the catalog (`/Root`) or its `/Pages` entry is absent.
    /// - [`Error::Unsupported`] when the catalog is not a dictionary.
    /// - Any [`Error`] propagated from [`Pdf::resolve`] while resolving the catalog.
    pub fn with_max_depth(pdf: &'a mut Pdf<R>, max_depth: usize) -> Result<Self> {
        let root = pdf.trailer_key_handle(b"Root");
        if root.is_null() {
            return Err(Error::Missing("/Root"));
        }
        let catalog = pdf.root_handle()?;
        let pages = catalog.try_get_key(b"/Pages")?;
        if pages.is_null() {
            return Err(Error::Missing("/Pages"));
        }
        let pages = PageNode::from_handle(pages);
        Ok(PageWalk {
            pdf,
            stack: vec![(pages, 0)],
            seen: BTreeSet::new(),
            seen_direct: HashSet::new(),
            max_depth,
            done: false,
        })
    }

    fn visit_node(&mut self, node: &PageNode, depth: usize) -> Result<Option<ObjectRef>> {
        let node_obj = node.handle(self.pdf);
        self.pdf.resolve(&node_obj)?;

        if node_obj.as_dictionary().is_none() {
            return Ok(None); // non-dictionary: skip silently
        }

        let node_type = node_obj.try_get_key(b"/Type")?;
        self.pdf.resolve(&node_type)?;

        if node_type.as_name().as_deref() == Some(b"Pages") {
            let kids = node_obj.try_get_key(b"/Kids")?;
            self.pdf.resolve(&kids)?;
            if let Some(kids) = kids.as_array() {
                // Push in reverse order so that the first kid is popped first.
                for kid in kids.iter().rev() {
                    if let Some(r) = kid.object_ref() {
                        self.stack.push((PageNode::Indirect(r), depth + 1));
                    } else if kid.as_dictionary().is_some() {
                        self.stack.push((PageNode::Direct(kid.clone()), depth + 1));
                    }
                }
            }
            return Ok(None);
        }

        if node_type.as_name().as_deref() == Some(b"Page") {
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

            if depth >= self.max_depth {
                self.done = true;
                return Some(Err(Error::Unsupported(format!(
                    "page tree depth exceeds maximum of {} at {}",
                    self.max_depth,
                    node.label()
                ))));
            }

            let first_visit = match &node {
                PageNode::Indirect(reference) => self.seen.insert(*reference),
                PageNode::Direct(handle) => self.seen_direct.insert(handle.identity_key()),
            };
            if !first_visit {
                continue; // cycle guard: already visited
            }

            match self.visit_node(&node, depth) {
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
