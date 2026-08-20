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

use crate::filters::{decode_stream_data_from_handle, DecodeLimits};
use crate::pipeline::buffer::Buffer;
#[cfg(test)]
use crate::pipeline::test_support::ascii85_fixture_bytes;
use crate::ref_chain::resolve_ref_chain;
use crate::{Dictionary, Error, Object, ObjectHandle, ObjectRef, Pdf, Result, Stream};
use std::collections::BTreeSet;
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

    fn handle(&self) -> ObjectHandle {
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
pub(crate) fn page_parent_entries<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    cursor: &PageParentCursor,
    key: &[u8],
) -> Result<Option<(ObjectHandle, ObjectHandle)>> {
    let dict = cursor.handle();
    pdf.resolve_object_handle(&dict)?;
    if dict.as_dictionary().is_none() {
        return Ok(None);
    }
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
        // Parsed PDF references already have canonical indirect identity. The
        // terminal chase below only preserves the existing Pdf::set_object
        // redirect compatibility for callers that have not migrated yet; the
        // returned handle remains the original live child handle. qpdf's
        // QPDF::replaceObject rejects an indirect replacement
        // (libqpdf/QPDF.cc:1986-1991), so a bare-reference value cycle created
        // through that legacy mutation bridge is not a parsed qpdf object
        // shape. Do not classify its synthetic-null fallback as qpdf's
        // null-as-absent inheritance rule.
        let terminal = pdf.resolve_object_handle_to_terminal(&value)?;
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
/// - Any [`Error`] that [`Pdf::resolve_object_handle`] or the canonical content
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
    pdf.resolve_object_handle(&page)?;
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

    let contents = page.try_get_key(b"/Contents")?;
    let (contents, _) = pdf.resolve_object_handle_to_terminal_ref(&contents)?;
    if contents.is_null() {
        return Ok(Vec::new());
    }

    // `ObjectHandle::array_or_stream_to_stream_array` intentionally mirrors
    // qpdf's one-hop handle view. This public compatibility helper also has a
    // long-standing contract to follow holder chains in `/Contents`, so first
    // reduce every legal holder to its terminal handle and then hand the
    // normalized stream array to the canonical pipe path. The stream handles
    // themselves remain resolver-backed; no legacy `Object` materialization is
    // introduced for filter metadata or stream bytes.
    let mut streams = Vec::new();
    if let Some(items) = contents.as_array() {
        for item in items {
            let (item, _) = pdf.resolve_object_handle_to_terminal_ref(&item)?;
            if item.as_stream_dict().is_some() {
                streams.push(item);
            } else {
                return Err(Error::Unsupported(format!(
                    "/Contents array element on page {page_ref} does not resolve to a stream"
                )));
            }
        }
    } else if contents.as_stream_dict().is_some() {
        streams.push(contents);
    } else {
        return Err(Error::Unsupported(format!(
            "/Contents on page {page_ref} is not a stream or array"
        )));
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

/// Resolve a `Page`'s `/Contents` into its content streams, each paired with the
/// terminal [`ObjectRef`] of the indirect chain that produced it (`None` for a
/// direct inline stream). Holder chains (`ref -> ref -> stream`) are followed via
/// `resolve_ref_chain`, so a doubly-indirect `/Contents` is not dropped.
///
/// Every legal `/Contents` shape is accepted: a direct stream, an (indirect)
/// reference to a stream, an array of stream references, or a reference to such
/// an array.
///
/// A page with no `/Contents` entry yields an empty list (it is a blank page,
/// not an error).
///
/// # Errors
///
/// - [`Error::Unsupported`] when `page_ref` is not a `/Type /Page` dictionary, or
///   when a `/Contents` element does not resolve to a stream.
/// - Any [`Error`] propagated from [`Pdf::resolve`].
pub fn page_content_stream_entries<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<Vec<(Option<ObjectRef>, Stream)>> {
    page_content_stream_entries_with_policy(pdf, page_ref, false)
}

/// Resolve a `Page`'s `/Contents` streams while ignoring entries that do not
/// resolve to streams.
///
/// This mirrors qpdf's writer-side content normalization: valid stream entries
/// are normalized even when a damaged `/Contents` array also contains `null` or
/// another non-stream value. Reference-resolution and page-structure errors
/// remain fatal.
pub fn page_content_stream_entries_tolerant<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<Vec<(Option<ObjectRef>, Stream)>> {
    page_content_stream_entries_with_policy(pdf, page_ref, true)
}

fn page_content_stream_entries_with_policy<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    ignore_non_streams: bool,
) -> Result<Vec<(Option<ObjectRef>, Stream)>> {
    let page_obj = pdf.resolve_borrowed(page_ref)?;
    let Some(page_dict) = page_obj.as_dict() else {
        return Err(Error::Unsupported(format!(
            "object {page_ref} is not a dictionary, cannot extract /Contents"
        )));
    };
    match page_dict.get("Type") {
        Some(Object::Name(name)) if name.as_slice() == b"Page" => {}
        _ => {
            return Err(Error::Unsupported(format!(
                "object {page_ref} is not a /Type /Page dictionary"
            )));
        }
    }
    let contents = match page_dict.get("Contents").cloned() {
        None => return Ok(Vec::new()),
        Some(Object::Null) => return Ok(Vec::new()),
        Some(c) => c,
    };
    collect_content_stream_entries(pdf, &contents, page_ref, ignore_non_streams)
}

/// Resolve a `/Contents` value into a flat list of `(terminal ref, Stream)`
/// entries, handling all four legal forms: a direct `Stream` (no ref), a
/// `Reference` to a stream, an `Array` of `Reference`s (or direct streams), or
/// a `Reference` to such an array.
///
/// This is the shared holder-chain implementation behind both public entry
/// points (one retaining terminal refs and one returning only streams).
fn collect_content_stream_entries<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    contents: &Object,
    page_ref: ObjectRef,
    ignore_non_streams: bool,
) -> Result<Vec<(Option<ObjectRef>, Stream)>> {
    match contents {
        // Direct inline stream — valid per spec (§7.8.2 note) and used in test PDFs.
        Object::Stream(s) => Ok(vec![(None, s.clone())]),

        // Indirect reference — may point at a stream OR at an array of stream
        // refs (legal per ISO 32000-2 §7.7.3.3; qpdf accepts it). Resolve the
        // full holder chain (`ref → ref → …`) once, then dispatch on the
        // terminal type so a doubly-indirect /Contents is not dropped.
        Object::Reference(_) => {
            let (resolved, last) = resolve_ref_chain(pdf, contents)?;
            match resolved {
                Object::Stream(s) => Ok(vec![(last, s)]),
                Object::Array(elems) => {
                    collect_content_array_entries(pdf, &elems, page_ref, ignore_non_streams)
                }
                Object::Null => Ok(Vec::new()),
                _ if ignore_non_streams => Ok(Vec::new()),
                other => Err(Error::Unsupported(format!(
                    "/Contents reference on page {page_ref} resolves to {}, not a stream or array",
                    object_type_name(&other)
                ))),
            }
        }

        // Array — each element must be a Reference to a stream (or a direct stream).
        Object::Array(elems) => {
            collect_content_array_entries(pdf, elems, page_ref, ignore_non_streams)
        }

        _ if ignore_non_streams => Ok(Vec::new()),
        other => Err(Error::Unsupported(format!(
            "/Contents entry on page {page_ref} has unexpected type {}",
            object_type_name(other)
        ))),
    }
}

/// Flatten a `/Contents` array's elements into `(terminal ref, Stream)` entries.
///
/// Each element must be a `Reference` resolving (through its full holder chain)
/// to a stream, or a direct inline `Stream` (no ref). This is shared by both the
/// direct-`Array` arm and the resolved-from-reference array path of
/// [`collect_content_stream_entries`].
fn collect_content_array_entries<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    elems: &[Object],
    page_ref: ObjectRef,
    ignore_non_streams: bool,
) -> Result<Vec<(Option<ObjectRef>, Stream)>> {
    let mut out = Vec::with_capacity(elems.len());
    for elem in elems {
        match elem {
            // Follow the full holder chain per element so a doubly-indirect
            // array entry (`ref → ref → stream`) is not dropped.
            Object::Reference(r) => {
                let (obj, last) = resolve_ref_chain(pdf, elem)?;
                match obj.into_stream() {
                    Some(s) => out.push((last, s)),
                    None if !ignore_non_streams => {
                        return Err(Error::Unsupported(format!(
                            "/Contents array element {r} on page {page_ref} does not resolve to a stream"
                        )));
                    }
                    None => {}
                }
            }
            Object::Stream(s) => out.push((None, s.clone())),
            other if !ignore_non_streams => {
                let type_name = object_type_name(other);
                return Err(Error::Unsupported(format!(
                    "/Contents array element of type {type_name} on page {page_ref} is not a stream or reference"
                )));
            }
            _ => {}
        }
    }
    Ok(out)
}

/// Coalesce a page's `/Contents` array into a single stream.
///
/// When a PDF page's `/Contents` entry is an array of two or more stream
/// references, this function:
///
/// 1. Decodes each stream through its filter pipeline.
/// 2. Joins the decoded bytes with a single `b'\n'` separator between segments
///    (matching qpdf's `--coalesce-contents` semantics — ISO 32000-1 §7.8.2
///    allows any whitespace between concatenated content streams, and newline
///    is qpdf's choice).
/// 3. Stores the result as a new indirect `Stream` object (no filter applied —
///    re-encoding is the responsibility of the write path).
/// 4. Updates the page dictionary's `/Contents` entry to reference the new
///    single stream object.
///
/// **No `q`/`Q` framing is inserted.** ISO 32000-1 §7.8.2 specifies that
/// adjacent content streams are concatenated as if they formed a single stream;
/// the standard does not require wrapping. The acceptance criterion
/// "nested q/Q balance preserved" means that the `\n` separator guarantees
/// tokens from adjacent segments are never lexically merged (e.g. a trailing
/// number `12` and a leading digit `0` would form `120` without the newline).
///
/// # No-op cases
///
/// - `/Contents` absent → returns `Ok(())` (empty page, unchanged).
/// - `/Contents` is a single reference or a direct `Stream` → returns `Ok(())`
///   (already a single stream, no mutation needed).
/// - `/Contents` is an array with exactly **one** element → treated as a
///   single stream (no mutation needed).
///
/// Only when `/Contents` is an array with **two or more** elements does the
/// function perform a write.
///
/// # Errors
///
/// Propagates any error from [`Pdf::resolve`] or
/// [`crate::filters::decode_stream_data`], or returns [`Error::Unsupported`]
/// when the page object or its `/Contents` elements have unexpected types.
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
/// for page_ref in page_refs {
///     pages::coalesce_page_contents(&mut pdf, page_ref)?;
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn coalesce_page_contents<R: Read + Seek>(pdf: &mut Pdf<R>, page_ref: ObjectRef) -> Result<()> {
    // ── 1. Resolve the page dictionary ────────────────────────────────────────
    let page_obj = pdf.resolve_borrowed(page_ref)?;
    let Some(page_dict) = page_obj.as_dict() else {
        return Err(Error::Unsupported(format!(
            "object {page_ref} is not a dictionary, cannot coalesce /Contents"
        )));
    };

    // Verify /Type is /Page (same check as page_content_bytes).
    match page_dict.get("Type") {
        Some(Object::Name(name)) if name.as_slice() == b"Page" => {}
        Some(Object::Name(name)) => {
            return Err(Error::Unsupported(format!(
                "object {page_ref} has /Type /{}, expected /Page",
                String::from_utf8_lossy(name)
            )));
        }
        Some(_) => {
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

    // ── 2. Extract /Contents; skip if absent or already a single stream ───────
    let contents = match page_dict.get("Contents").cloned() {
        None => return Ok(()), // empty page
        Some(c) => c,
    };

    // Collect references: only an Array with ≥2 elements triggers coalesce.
    let refs: Vec<Object> = match &contents {
        Object::Array(elems) if elems.len() >= 2 => elems.clone(),
        // Single-element array, direct Stream, or Reference: no-op.
        _ => return Ok(()),
    };

    // qpdf's page helper delegates coalescing to the ObjectHandle route
    // (`QPDFPageObjectHelper.cc:474-476`, `QPDFObjectHandle.cc:1550-1572`).
    // Keep the legacy `Object` values above only for the existing
    // dictionary/write-back contract; filter metadata and raw bytes must come
    // from the resolver-backed handles.
    let page_handle = pdf.get_object_handle(page_ref);
    pdf.resolve_object_handle(&page_handle)?;
    let contents_handle = page_handle.try_get_key(b"/Contents")?;
    let content_handles = canonical_content_handles(contents_handle, refs.len(), page_ref)?;

    // ── 3. Decode each stream and concatenate with '\n' separators ─────────────
    let mut coalesced: Vec<u8> = Vec::new();
    let mut need_newline = false;
    for content_handle in &content_handles {
        let stream_handle = pdf.resolve_object_handle_to_terminal(content_handle)?;
        let stream_dict_handle = content_stream_dict(&stream_handle, page_ref)?;
        let stream_data = stream_handle.get_raw_stream_data()?;
        let decoded = decode_stream_data_from_handle(
            &stream_dict_handle,
            stream_data.as_ref(),
            DecodeLimits::default(),
        )?;
        if need_newline {
            coalesced.push(b'\n');
        }
        coalesced.extend_from_slice(&decoded);
        // qpdf's LastChar pipeline only inserts a separator before the next
        // stream when this decoded stream did not end in LF
        // (`QPDFObjectHandle.cc:1710-1737`).
        need_newline = decoded.last().copied() != Some(b'\n');
    }

    // ── 4. Allocate a fresh object number for the coalesced stream ─────────────
    let new_num: u32 = pdf
        .object_refs()
        .iter()
        .map(|r| r.number)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| {
            Error::Unsupported(
                "coalesce_page_contents: object number overflow allocating new stream".to_string(),
            )
        })?;
    let new_stream_ref = ObjectRef::new(new_num, 0);

    // ── 5. Build the new Stream (no filter: raw decoded bytes; writer handles re-encode) ─
    // qpdf newStream() creates a fresh stream with an empty dictionary
    // (QPDF.cc:1912-1917); metadata from the input streams is not copied.
    let new_stream = Stream::new(Dictionary::new(), coalesced);
    pdf.set_object(new_stream_ref, Object::Stream(new_stream));

    // ── 6. Re-resolve the page dictionary (it may have been evicted) and patch /Contents ─
    let page_obj2 = pdf.resolve_borrowed(page_ref)?;
    let Some(mut new_page_dict) = page_obj2.as_dict().cloned() else {
        return Err(Error::Unsupported(format!(
            "object {page_ref} unexpectedly not a dictionary after coalesce"
        )));
    };
    new_page_dict.insert("Contents", Object::Reference(new_stream_ref));
    pdf.set_object(page_ref, Object::Dictionary(new_page_dict));

    Ok(())
}

fn canonical_content_handles(
    contents: ObjectHandle,
    expected_len: usize,
    page_ref: ObjectRef,
) -> Result<Vec<ObjectHandle>> {
    let Some(content_handles) = contents.as_array() else {
        return Err(Error::Unsupported(format!(
            "/Contents on page {page_ref} changed from an array during coalesce"
        )));
    };
    if content_handles.len() != expected_len {
        return Err(Error::Unsupported(format!(
            "/Contents array on page {page_ref} changed during coalesce"
        )));
    }
    Ok(content_handles)
}

fn content_stream_dict(stream: &ObjectHandle, page_ref: ObjectRef) -> Result<ObjectHandle> {
    stream.as_stream_dict().ok_or_else(|| {
        Error::Unsupported(format!(
            "/Contents array element on page {page_ref} does not resolve to a stream"
        ))
    })
}

fn object_type_name(obj: &Object) -> &'static str {
    match obj {
        Object::Null => "null",
        Object::Boolean(_) => "boolean",
        Object::Integer(_) => "integer",
        Object::Real(_) | Object::RealLiteral { .. } => "real",
        Object::Name(_) => "name",
        Object::String(_) => "string",
        Object::Operator(_) => "operator",
        Object::InlineImage(_) => "inline-image",
        Object::Array(_) => "array",
        Object::Dictionary(_) => "dictionary",
        Object::Stream(_) => "stream",
        Object::Reference(_) => "reference",
    }
}

/// Return the `/Resources` dictionary for a page, walking up the `/Parent` chain
/// until one is found. Uses [`DEFAULT_MAX_PAGE_TREE_DEPTH`] as the depth limit.
///
/// Inheritable attributes in PDF are defined in ISO 32000-1 §7.7.3.4. `/Resources`
/// is one of them: if a `Page` node does not carry its own `/Resources`, the nearest
/// ancestor `Pages` node that has one should be used.
///
/// Returns `Ok(None)` when no node in the chain carries a `/Resources` entry.
///
/// # Errors
///
/// - [`Error::Unsupported`] if the depth limit is exceeded (indicates a malformed or
///   extremely deeply nested document), or when a `/Resources` entry (or a
///   reference it points to) is neither a dictionary nor null.
/// - Any [`Error`] propagated from [`Pdf::resolve`].
pub fn resolve_inherited_resources<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<Option<Dictionary>> {
    resolve_inherited_resources_with_max_depth(pdf, page_ref, DEFAULT_MAX_PAGE_TREE_DEPTH)
}

/// Like [`resolve_inherited_resources`] but with a caller-supplied recursion limit.
///
/// # Errors
///
/// - [`Error::Unsupported`] when the `/Parent` chain reaches `max_depth`, when a
///   `/Resources` entry (or a reference it points to) is neither a dictionary nor
///   null, or when a `/Resources` reference does not resolve to a dictionary.
/// - Any [`Error`] propagated from [`Pdf::resolve`].
pub fn resolve_inherited_resources_with_max_depth<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    max_depth: usize,
) -> Result<Option<Dictionary>> {
    let Some(resources) =
        resolve_inherited_handle_with_max_depth(pdf, page_ref, b"/Resources", max_depth)?
    else {
        return Ok(None);
    };

    let resources_ref = resources.object_ref();
    let resources = pdf.resolve_object_handle_to_terminal(&resources)?;
    match resources.materialize()? {
        Object::Dictionary(dictionary) => Ok(Some(dictionary)),
        _ => match resources_ref {
            Some(resources_ref) => Err(Error::Unsupported(format!(
                "/Resources reference {resources_ref} on page {page_ref} does not resolve to a dictionary"
            ))),
            None => Err(Error::Unsupported(format!(
                "/Resources entry on page {page_ref} has unexpected type"
            ))),
        },
    }
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
pub struct PageWalk<'a, R: Read + Seek + 'static> {
    pdf: &'a mut Pdf<R>,
    /// Stack of `(node_ref, depth)` yet to be visited.
    stack: Vec<(ObjectRef, usize)>,
    seen: BTreeSet<ObjectRef>,
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
        let catalog_ref = pdf.root_ref().ok_or(Error::Missing("/Root"))?;
        let catalog = pdf.resolve_borrowed(catalog_ref)?;
        let Some(catalog) = catalog.as_dict() else {
            return Err(Error::Unsupported(format!(
                "document catalog {catalog_ref} is not a dictionary"
            )));
        };
        let pages_ref = catalog.get_ref("Pages").ok_or(Error::Missing("/Pages"))?;
        Ok(PageWalk {
            pdf,
            stack: vec![(pages_ref, 0)],
            seen: BTreeSet::new(),
            max_depth,
            done: false,
        })
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
                    self.max_depth, node
                ))));
            }

            if !self.seen.insert(node) {
                continue; // cycle guard: already visited
            }

            let node_obj = match self.pdf.resolve_borrowed(node) {
                Ok(o) => o,
                Err(e) => {
                    self.done = true;
                    return Some(Err(e));
                }
            };

            let Some(dict) = node_obj.as_dict() else {
                continue; // non-dictionary: skip silently
            };

            let node_type = dict
                .get("Type")
                .and_then(|value| match value {
                    Object::Name(value) => Some(value.as_slice()),
                    _ => None,
                })
                .unwrap_or(&[]);

            if node_type == b"Pages" {
                if let Some(kids) = dict.get("Kids").and_then(Object::as_array) {
                    // Push in reverse order so that the first kid is popped first.
                    for kid in kids.iter().rev() {
                        if let Object::Reference(r) = kid {
                            self.stack.push((*r, depth + 1));
                        }
                    }
                }
                continue;
            }

            if node_type == b"Page" {
                return Some(Ok(node));
            }

            // Unknown or absent /Type: skip silently.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filters::encode_stream_data;
    use crate::Dictionary;
    use std::cell::Cell;
    use std::io::{Cursor, Read, Seek, SeekFrom};
    use std::process::Command;
    use std::rc::Rc;

    struct ToggleReadFailure {
        cursor: Cursor<Vec<u8>>,
        fail: Rc<Cell<bool>>,
    }

    impl Read for ToggleReadFailure {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.fail.get() {
                return Err(std::io::Error::other("boundary parent read unexpectedly"));
            }
            self.cursor.read(buffer)
        }
    }

    impl Seek for ToggleReadFailure {
        fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
            self.cursor.seek(position)
        }
    }

    /// `object_type_name` collapses both real variants to `"real"` so that
    /// diagnostics do not leak the implementation detail that one form
    /// preserves the source literal and the other does not.
    #[test]
    fn object_type_name_collapses_real_and_real_literal() {
        assert_eq!(object_type_name(&Object::Real(1.5)), "real");
        assert_eq!(
            object_type_name(&Object::RealLiteral {
                value: 1.5,
                literal: b"1.5".to_vec(),
            }),
            "real"
        );
    }

    #[test]
    fn object_type_name_labels_content_only_values() {
        assert_eq!(
            object_type_name(&Object::Operator(b"q".to_vec())),
            "operator"
        );
        assert_eq!(
            object_type_name(&Object::InlineImage(b"data".to_vec())),
            "inline-image"
        );
    }

    #[test]
    fn page_parent_cursor_handles_direct_and_non_dictionary_parents() {
        assert_eq!(
            PageParentCursor::from_handle(ObjectHandle::dictionary(Vec::new())).to_string(),
            "direct page-tree dictionary"
        );
        assert!(next_page_parent(ObjectHandle::integer(42))
            .expect("non-dictionary parent lookup should succeed")
            .is_none());
    }

    #[test]
    fn page_parent_handles_preserve_identity_and_absent_inheritance() {
        let direct_resources = ObjectHandle::dictionary(Vec::new());
        let direct_parent = ObjectHandle::dictionary(vec![
            (b"/Resources".to_vec(), direct_resources.clone()),
            (b"/Parent".to_vec(), ObjectHandle::null()),
        ]);
        let mut empty = Pdf::empty().expect("empty PDF should be constructible");
        let non_dictionary = PageParentCursor::from_handle(ObjectHandle::integer(42));
        assert!(
            page_parent_entries(&mut empty, &non_dictionary, b"/Resources")
                .expect("non-dictionary parent lookup should succeed")
                .is_none()
        );

        let cursor = PageParentCursor::from_handle(direct_parent.clone());
        let (resources, parent) = page_parent_entries(&mut empty, &cursor, b"/Resources")
            .expect("direct parent lookup should succeed")
            .expect("direct parent should be a dictionary");

        assert!(cursor.handle().is_same_object_as(&direct_parent));
        assert!(resources.is_same_object_as(&direct_resources));
        assert!(parent.is_null());
        assert!(next_page_parent(parent)
            .expect("null parent lookup should succeed")
            .is_none());

        let bytes = pdf_from_objects(
            1,
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (
                    2,
                    "<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources 7 0 R >>",
                ),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (7, "<< /Font << >> >>"),
            ],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        let depth_error =
            resolve_inherited_handle_with_max_depth(&mut pdf, ObjectRef::new(3, 0), b"/Rotate", 0)
                .expect_err("a zero page-tree depth limit should fail before walking");
        assert!(depth_error
            .to_string()
            .contains("page tree depth exceeds maximum of 0 at 3 0 R"));

        let inherited = resolve_inherited_handle_with_max_depth(
            &mut pdf,
            ObjectRef::new(3, 0),
            b"/Resources",
            10,
        )
        .expect("inherited resource lookup should succeed")
        .expect("the page should inherit /Resources");
        assert_eq!(inherited.object_ref(), Some(ObjectRef::new(7, 0)));
        assert!(inherited.is_same_object_as(&pdf.get_object_handle(ObjectRef::new(7, 0))));
        assert!(resolve_inherited_handle_with_max_depth(
            &mut pdf,
            ObjectRef::new(3, 0),
            b"/Rotate",
            10,
        )
        .expect("absent /Rotate lookup should succeed")
        .is_none());

        let mut non_dictionary_pdf = Pdf::empty().expect("empty PDF should be constructible");
        non_dictionary_pdf.set_object(ObjectRef::new(3, 0), Object::Integer(42));
        assert!(resolve_inherited_handle_with_max_depth(
            &mut non_dictionary_pdf,
            ObjectRef::new(3, 0),
            b"/Resources",
            10,
        )
        .expect("non-dictionary page lookup should succeed")
        .is_none());
    }

    #[test]
    fn inherited_walk_reports_depth_limit_before_dereferencing_boundary_parent() {
        let bytes = pdf_from_objects(
            1,
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            ],
        );
        let fail = Rc::new(Cell::new(false));
        let reader = ToggleReadFailure {
            cursor: Cursor::new(bytes),
            fail: Rc::clone(&fail),
        };
        let mut pdf = Pdf::open(reader).expect("PDF should parse");
        let page = pdf.get_object_handle(ObjectRef::new(3, 0));
        pdf.resolve_object_handle(&page)
            .expect("the current page should be readable before the boundary");
        fail.set(true);
        let error =
            resolve_inherited_handle_with_max_depth(&mut pdf, ObjectRef::new(3, 0), b"/Rotate", 1)
                .expect_err("a boundary parent must report the depth limit before resolution");

        assert!(
            error
                .to_string()
                .contains("page tree depth exceeds maximum of 1 at 2 0 R"),
            "expected depth-limit error, got {error}"
        );
    }

    #[test]
    fn inherited_walk_propagates_a_genuine_read_failure_past_the_depth_guard() {
        let bytes = pdf_from_objects(
            1,
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            ],
        );
        let fail = Rc::new(Cell::new(false));
        let reader = ToggleReadFailure {
            cursor: Cursor::new(bytes),
            fail: Rc::clone(&fail),
        };
        let mut pdf = Pdf::open(reader).expect("PDF should parse");
        let page = pdf.get_object_handle(ObjectRef::new(3, 0));
        pdf.resolve_object_handle(&page)
            .expect("the current page should be readable before the boundary");
        fail.set(true);
        // A depth limit well past the actual parent chain means the guard
        // never fires, so the read failure on the /Parent object itself
        // must surface as-is rather than being masked or misreported as a
        // depth-limit error.
        let error =
            resolve_inherited_handle_with_max_depth(&mut pdf, ObjectRef::new(3, 0), b"/Rotate", 10)
                .expect_err("a genuine read failure must propagate");

        assert!(
            error
                .to_string()
                .contains("boundary parent read unexpectedly"),
            "expected the underlying I/O error, got {error}"
        );
    }

    #[test]
    fn inherited_walk_rejects_an_already_resolved_non_dictionary_parent_without_depth_error() {
        let bytes = pdf_from_objects(
            1,
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "42"),
                (3, "<< /Type /Page /Parent 2 0 R >>"),
            ],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        // Resolve the malformed /Parent target ahead of the walk, mirroring
        // an earlier, unrelated read that already populated the object
        // cache. `is_indirect()` reflects identity, not resolution state,
        // so `next_page_parent` must not defer this already-known
        // non-dictionary value just because it is indirect.
        let parent = pdf.get_object_handle(ObjectRef::new(2, 0));
        pdf.resolve_object_handle(&parent)
            .expect("the malformed parent object should be readable");
        assert!(parent.is_resolved());
        assert!(parent.as_dictionary().is_none());

        // A depth limit of exactly 1 means the malformed parent sits right
        // at the boundary: if it were incorrectly deferred, the next loop
        // iteration's depth check would fire first and report a depth-limit
        // error instead of the correct clean termination.
        let result =
            resolve_inherited_handle_with_max_depth(&mut pdf, ObjectRef::new(3, 0), b"/Rotate", 1)
                .expect("a malformed non-dictionary parent must not error");
        assert!(
            result.is_none(),
            "expected no inherited value, got {result:?}"
        );
    }

    #[test]
    fn page_attribute_inheritance_is_limited_to_qpdf_inheritable_keys() {
        let bytes = pdf_from_objects(
            1,
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 /Custom 7 0 R >>"),
                (3, "<< /Type /Page /Parent 2 0 R >>"),
                (7, "<< /Value /Inherited >>"),
            ],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");

        assert!(resolve_inherited_handle_with_max_depth(
            &mut pdf,
            ObjectRef::new(3, 0),
            b"/Custom",
            10,
        )
        .expect("unknown page attributes should be readable")
        .is_none());
    }

    #[test]
    fn page_parent_handle_walk_matches_qpdf_11_9_flatten_probe() {
        let version = match Command::new("qpdf").arg("--version").output() {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).into_owned()
            }
            // cov:ignore-start: the optional qpdf oracle probe skips when qpdf is absent or exits unsuccessfully in a CI environment.
            Ok(_) | Err(_) => {
                eprintln!("qpdf 11.9.0 is unavailable; skipping page-tree oracle probe");
                return;
            } // cov:ignore-end
        };
        // cov:ignore-start: the optional qpdf oracle probe skips when a different qpdf version is installed.
        if !version.starts_with("qpdf version 11.9.") {
            eprintln!("qpdf 11.9.0 is unavailable; skipping page-tree oracle probe");
            return;
        }
        // cov:ignore-end

        let bytes = pdf_from_objects(
            1,
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (
                    2,
                    "<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources 7 0 R >>",
                ),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (7, "<< /Font << >> >>"),
            ],
        );
        let dir = tempfile::tempdir().expect("temporary qpdf probe directory");
        let input = dir.path().join("input.pdf");
        let flattened = dir.path().join("flattened.pdf");
        std::fs::write(&input, &bytes).expect("write qpdf probe input");

        let flatten = Command::new("qpdf")
            .arg(&input)
            .arg("--pages")
            .arg(&input)
            .arg("1")
            .arg("--")
            .arg(&flattened)
            .output()
            .expect("run qpdf page flatten probe");
        assert!(
            flatten.status.success(),
            "qpdf --pages probe failed: {}",
            String::from_utf8_lossy(&flatten.stderr) // cov:ignore: assertion failure arm, only evaluated when the qpdf probe fails
        );

        let shown = Command::new("qpdf")
            .arg("--show-object=3")
            .arg(&flattened)
            .output()
            .expect("show qpdf flattened page");
        assert!(
            shown.status.success(),
            "qpdf --show-object probe failed: {}",
            String::from_utf8_lossy(&shown.stderr) // cov:ignore: assertion failure arm, only evaluated when the qpdf probe fails
        );
        let shown = String::from_utf8_lossy(&shown.stdout);
        assert!(shown.contains("/Resources"), "qpdf output: {shown}");
        assert!(!shown.contains("/Rotate"), "qpdf output: {shown}");

        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        let inherited = resolve_inherited_handle_with_max_depth(
            &mut pdf,
            ObjectRef::new(3, 0),
            b"/Resources",
            10,
        )
        .expect("inherited resource lookup should succeed")
        .expect("the page should inherit /Resources");
        assert_eq!(inherited.object_ref(), Some(ObjectRef::new(7, 0)));
        assert!(resolve_inherited_handle_with_max_depth(
            &mut pdf,
            ObjectRef::new(3, 0),
            b"/Rotate",
            10,
        )
        .expect("absent /Rotate lookup should succeed")
        .is_none());
    }

    #[test]
    fn inherited_reference_cycle_probe_matches_qpdf_11_9() {
        let version = match Command::new("qpdf").arg("--version").output() {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).into_owned()
            }
            // cov:ignore-start: the optional qpdf oracle probe skips when qpdf is absent or exits unsuccessfully in a CI environment.
            Ok(_) | Err(_) => {
                eprintln!("qpdf 11.9.0 is unavailable; skipping inherited-cycle oracle probe");
                return;
            } // cov:ignore-end
        };
        // cov:ignore-start: the optional qpdf oracle probe skips when a different qpdf version is installed.
        if !version.starts_with("qpdf version 11.9.") {
            eprintln!("qpdf 11.9.0 is unavailable; skipping inherited-cycle oracle probe");
            return;
        }
        // cov:ignore-end

        let bytes = pdf_from_objects(
            1,
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (
                    2,
                    "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox 4 0 R >>",
                ),
                (3, "<< /Type /Page /Parent 2 0 R >>"),
                (4, "5 0 R"),
                (5, "4 0 R"),
            ],
        );
        let dir = tempfile::tempdir().expect("temporary qpdf probe directory");
        let input = dir.path().join("inherited-cycle.pdf");
        std::fs::write(&input, &bytes).expect("write inherited-cycle probe input");

        let checked = Command::new("qpdf")
            .arg("--check")
            .arg(&input)
            .output()
            .expect("run qpdf inherited-cycle check probe");
        let qpdf_stderr = String::from_utf8_lossy(&checked.stderr);
        eprintln!(
            "qpdf inherited-cycle check: status={}, stderr={qpdf_stderr:?}",
            checked.status
        );

        let pages = Command::new("qpdf")
            .arg("--show-pages")
            .arg(&input)
            .output()
            .expect("run qpdf inherited-cycle page probe");
        let pages_stderr = String::from_utf8_lossy(&pages.stderr);
        assert_eq!(pages.status.code(), Some(3));
        assert!(String::from_utf8_lossy(&pages.stdout).contains("page 1: 3 0 R"));
        assert!(pages_stderr.contains("MediaBox is undefined; setting to letter / ANSI A"));

        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("inherited-cycle PDF should parse");
        let flpdf_result = resolve_inherited_handle_with_max_depth(
            &mut pdf,
            ObjectRef::new(3, 0),
            b"/MediaBox",
            10,
        )
        .expect("cyclic inherited value should be inspected, not fatal");
        eprintln!("flpdf inherited-cycle result: {flpdf_result:?}");
        // QPDFParser::parse handles a top-level integer as the complete object
        // (`QPDFParser.cc:87-88`); it does not parse the following `0 R` as an
        // indirect-reference value. qpdf therefore diagnoses this shape as a
        // malformed object, not as a recursive resolve loop.
        assert_eq!(checked.status.code(), Some(3));
        assert!(qpdf_stderr.contains("expected endobj"));
        assert!(qpdf_stderr.contains("MediaBox is undefined; setting to letter / ANSI A"));
        assert!(!qpdf_stderr.contains("loop detected resolving object"));

        // The canonical flpdf parser likewise exposes object 4 as the
        // inherited non-null value; the legacy bare-reference chase is only
        // reachable after Pdf::set_object mutation, not from this parsed PDF.
        let inherited = flpdf_result.expect("qpdf did not treat the malformed value as absent");
        assert_eq!(inherited.object_ref(), Some(ObjectRef::new(4, 0)));
        let terminal = pdf
            .resolve_object_handle_to_terminal(&inherited)
            .expect("resolve parsed inherited value");
        assert_eq!(terminal.as_integer(), Some(5));
    }

    // -----------------------------------------------------------------------
    // Minimal PDF builder helpers
    // -----------------------------------------------------------------------

    /// Build a minimal PDF with one page whose /Contents is supplied by the
    /// caller. `extra_objects` is appended verbatim (for additional stream
    /// objects); `contents_entry` is placed in the Page dictionary.
    ///
    /// Object layout:
    ///   1 0 R  Catalog
    ///   2 0 R  Pages
    ///   3 0 R  Page (with /Contents = contents_entry)
    ///   4+ 0 R extra objects
    fn build_pdf_with_contents(contents_entry: &str, extra_objects: &[(u32, &str)]) -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        let off3 = pdf.len() as u64;
        let page_obj = if contents_entry.is_empty() {
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n".to_string()
        } else {
            format!(
                "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {contents_entry} >>\nendobj\n"
            )
        };
        pdf.extend_from_slice(page_obj.as_bytes());

        // Extra stream objects
        let mut extra_offsets: Vec<(u32, u64)> = Vec::new();
        for (num, body) in extra_objects.iter() {
            let off = pdf.len() as u64;
            extra_offsets.push((*num, off));
            pdf.extend_from_slice(body.as_bytes());
        }

        let xref_start = pdf.len() as u64;
        let max_num = extra_offsets.iter().map(|(n, _)| *n).max().unwrap_or(3);
        let total = max_num as usize + 1; // 0..=max_num
        let mut xref = format!("xref\n0 {total}\n0000000000 65535 f \n");
        xref.push_str(&format!("{:010} 00000 n \n", off1));
        xref.push_str(&format!("{:010} 00000 n \n", off2));
        xref.push_str(&format!("{:010} 00000 n \n", off3));
        for i in 4..=max_num {
            if let Some((_, off)) = extra_offsets.iter().find(|(n, _)| *n == i) {
                xref.push_str(&format!("{:010} 00000 n \n", off));
            } else {
                xref.push_str("0000000000 65535 f \n");
            }
        }
        pdf.extend_from_slice(xref.as_bytes());
        let trailer =
            format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    /// Build a minimal PDF with a non-Page object (a Pages node) at object 3.
    fn build_pdf_with_pages_node_as_obj3() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        // object 2 is actually the Pages intermediary (no /Kids for simplicity)
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");

        let off3 = pdf.len() as u64;
        // object 3 is deliberately another Pages node, not a Page
        pdf.extend_from_slice(b"3 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");

        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 4\n0000000000 65535 f \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \n",
            off1, off2, off3,
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer =
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    /// Build raw stream object bytes (binary-safe).
    fn stream_object_bytes(num: u32, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(
            format!("{num} 0 obj\n<< /Length {} >>\nstream\n", body.len()).as_bytes(),
        );
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendstream\nendobj\n");
        out
    }

    /// Build raw stream object bytes with a chained [/FlateDecode /ASCII85Decode] filter array.
    ///
    /// PDF semantics: Filter = [/FlateDecode /ASCII85Decode] means the *decode* pipeline
    /// applies filters left-to-right, so during decode the reader first FlateDecode-decompresses,
    /// then ASCII85Decode-decodes. To produce bytes that round-trip correctly we therefore
    /// *encode* in the reverse order: first ASCII85 fixture-encode, then FlateDecode-compress.
    ///
    /// This exercises the Array branch in decode_stream_data_with_filters_and_crypt
    /// (filters.rs L78-96).
    fn chained_filter_stream_object_bytes(num: u32, body: &[u8]) -> Vec<u8> {
        // Step 1: ASCII85 fixture-encode for the decoder-only test route.
        let after_a85 = ascii85_fixture_bytes(body);

        // Step 2: FlateDecode-compress
        let mut flate_dict = Dictionary::new();
        flate_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        let encoded = encode_stream_data(&flate_dict, &after_a85).unwrap();

        let mut out = Vec::new();
        out.extend_from_slice(
            format!(
                "{num} 0 obj\n<< /Filter [ /FlateDecode /ASCII85Decode ] /Length {} >>\nstream\n",
                encoded.len()
            )
            .as_bytes(),
        );
        out.extend_from_slice(&encoded);
        out.extend_from_slice(b"\nendstream\nendobj\n");
        out
    }

    // -----------------------------------------------------------------------
    // A flexible builder that takes pre-built extra object bytes.
    // -----------------------------------------------------------------------

    fn build_pdf_with_binary_extras(
        contents_entry: &str,
        extra_objects: &[(u32, Vec<u8>)],
    ) -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        let off3 = pdf.len() as u64;
        let page_obj = if contents_entry.is_empty() {
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n".to_string()
        } else {
            format!(
                "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {contents_entry} >>\nendobj\n"
            )
        };
        pdf.extend_from_slice(page_obj.as_bytes());

        let mut extra_offsets: Vec<(u32, u64)> = Vec::new();
        for (num, body) in extra_objects.iter() {
            let off = pdf.len() as u64;
            extra_offsets.push((*num, off));
            pdf.extend_from_slice(body);
        }

        let xref_start = pdf.len() as u64;
        let max_num = extra_offsets.iter().map(|(n, _)| *n).max().unwrap_or(3);
        let total = max_num as usize + 1;
        let mut xref = format!("xref\n0 {total}\n0000000000 65535 f \n");
        xref.push_str(&format!("{:010} 00000 n \n", off1));
        xref.push_str(&format!("{:010} 00000 n \n", off2));
        xref.push_str(&format!("{:010} 00000 n \n", off3));
        for i in 4..=max_num {
            if let Some((_, off)) = extra_offsets.iter().find(|(n, _)| *n == i) {
                xref.push_str(&format!("{:010} 00000 n \n", off));
            } else {
                xref.push_str("0000000000 65535 f \n");
            }
        }
        pdf.extend_from_slice(xref.as_bytes());
        let trailer =
            format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    // -----------------------------------------------------------------------
    // Test: /Contents absent → empty Vec
    // -----------------------------------------------------------------------

    #[test]
    fn page_content_bytes_returns_empty_for_page_without_contents() {
        let bytes = build_pdf_with_contents("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        let page_ref = ObjectRef::new(3, 0);
        let content = page_content_bytes(&mut pdf, page_ref).unwrap();
        assert!(
            content.is_empty(),
            "expected empty Vec for page without /Contents"
        );
    }

    #[test]
    fn page_content_bytes_returns_empty_for_null_contents() {
        let bytes = build_pdf_with_contents("null", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        let content = page_content_bytes(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        assert!(
            content.is_empty(),
            "qpdf treats explicit null /Contents as empty"
        );
    }

    // -----------------------------------------------------------------------
    // Test: /Contents = single Reference → decoded bytes
    // -----------------------------------------------------------------------

    #[test]
    fn page_content_bytes_resolves_single_stream_reference() {
        let body = b"BT /F1 12 Tf (Hello) Tj ET";
        let stream_bytes = stream_object_bytes(4, body);
        let bytes = build_pdf_with_binary_extras("4 0 R", &[(4, stream_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        let page_ref = ObjectRef::new(3, 0);
        let content = page_content_bytes(&mut pdf, page_ref).unwrap();
        assert_eq!(content, body);
    }

    // -----------------------------------------------------------------------
    // Test: /Contents = Array of References → joined the way qpdf's
    // pipeContentStreams does: a '\n' is inserted between streams only when the
    // previous decoded stream does not already end in a newline.
    // -----------------------------------------------------------------------

    #[test]
    fn page_content_bytes_concatenates_array_of_refs_with_newline_separator() {
        // Neither body ends in a newline, so qpdf inserts a single '\n' between them.
        let body1 = b"q 1 0 0 1 0 0 cm";
        let body2 = b"BT /F1 12 Tf (World) Tj ET";
        let stream_bytes1 = stream_object_bytes(4, body1);
        let stream_bytes2 = stream_object_bytes(5, body2);
        let bytes = build_pdf_with_binary_extras(
            "[4 0 R 5 0 R]",
            &[(4, stream_bytes1), (5, stream_bytes2)],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        let page_ref = ObjectRef::new(3, 0);
        let content = page_content_bytes(&mut pdf, page_ref).unwrap();
        let mut expected = body1.to_vec();
        expected.push(b'\n');
        expected.extend_from_slice(body2);
        assert_eq!(content, expected);
    }

    #[test]
    fn page_content_bytes_no_separator_when_prev_stream_ends_in_newline() {
        // body1 already ends in '\n' → qpdf adds NO separator before body2.
        let body1 = b"q 1 0 0 1 0 0 cm\n";
        let body2 = b"BT /F1 12 Tf (World) Tj ET";
        let stream_bytes1 = stream_object_bytes(4, body1);
        let stream_bytes2 = stream_object_bytes(5, body2);
        let bytes = build_pdf_with_binary_extras(
            "[4 0 R 5 0 R]",
            &[(4, stream_bytes1), (5, stream_bytes2)],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        let content = page_content_bytes(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        let mut expected = body1.to_vec();
        expected.extend_from_slice(body2);
        assert_eq!(content, expected);
    }

    #[test]
    fn page_content_bytes_empty_stream_forces_newline() {
        // An empty first stream produces no bytes; qpdf's per-stream LastChar is
        // initialised to 0 (not '\n'), so a '\n' is still inserted before body2.
        let body2 = b"BT /F1 12 Tf (World) Tj ET";
        let stream_bytes1 = stream_object_bytes(4, b"");
        let stream_bytes2 = stream_object_bytes(5, body2);
        let bytes = build_pdf_with_binary_extras(
            "[4 0 R 5 0 R]",
            &[(4, stream_bytes1), (5, stream_bytes2)],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        let content = page_content_bytes(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        let mut expected = vec![b'\n'];
        expected.extend_from_slice(body2);
        assert_eq!(content, expected);
    }

    // -----------------------------------------------------------------------
    // Test: /Contents stream with chained [/FlateDecode /ASCII85Decode] filter array
    // -----------------------------------------------------------------------

    #[test]
    fn page_content_bytes_applies_chained_filters() {
        // This stream uses Filter = [/FlateDecode /ASCII85Decode], exercising the Array branch
        // in decode_stream_data (not the single-Name branch).
        let body = b"q 0.5 g 100 100 300 300 re f Q";
        let stream_bytes = chained_filter_stream_object_bytes(4, body);
        let bytes = build_pdf_with_binary_extras("4 0 R", &[(4, stream_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        let page_ref = ObjectRef::new(3, 0);
        let content = page_content_bytes(&mut pdf, page_ref).unwrap();
        assert_eq!(content, body);
    }

    #[test]
    fn page_content_bytes_resolves_indirect_filter_and_decode_params() {
        let body = b"q 1 0 0 1 0 0 cm Q";
        let mut filter_dict = Dictionary::new();
        filter_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        let encoded = encode_stream_data(&filter_dict, body).unwrap();

        let stream_ref = ObjectRef::new(4, 0);
        let filter_ref = ObjectRef::new(6, 0);
        let decode_params_ref = ObjectRef::new(7, 0);
        let bytes = build_pdf_with_binary_extras("4 0 R", &[(4, stream_object_bytes(4, &encoded))]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");

        let mut stream_dict = Dictionary::new();
        stream_dict.insert("Filter", Object::Reference(filter_ref));
        stream_dict.insert("DecodeParms", Object::Reference(decode_params_ref));
        pdf.set_object(
            stream_ref,
            Object::Stream(Stream::new(stream_dict, encoded)),
        );
        pdf.set_object(filter_ref, Object::Name(b"FlateDecode".to_vec()));
        let mut decode_params = Dictionary::new();
        decode_params.insert("Predictor", Object::Integer(1));
        pdf.set_object(decode_params_ref, Object::Dictionary(decode_params));

        let content = page_content_bytes(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        assert_eq!(content, body);
    }

    #[test]
    fn coalesce_page_contents_resolves_indirect_filter_and_decode_params() {
        let body1 = b"q 1 0 0 1 0 0 cm";
        let body2 = b"Q";
        let mut filter_dict = Dictionary::new();
        filter_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        let encoded = encode_stream_data(&filter_dict, body1).unwrap();

        let bytes = build_pdf_with_binary_extras(
            "[4 0 R 5 0 R]",
            &[
                (4, stream_object_bytes(4, &encoded)),
                (5, stream_object_bytes(5, body2)),
            ],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");

        let mut stream_dict = Dictionary::new();
        stream_dict.insert("Filter", Object::Reference(ObjectRef::new(6, 0)));
        stream_dict.insert("DecodeParms", Object::Reference(ObjectRef::new(7, 0)));
        pdf.set_object(
            ObjectRef::new(4, 0),
            Object::Stream(Stream::new(stream_dict, encoded)),
        );
        pdf.set_object(ObjectRef::new(6, 0), Object::Name(b"FlateDecode".to_vec()));
        let mut decode_params = Dictionary::new();
        decode_params.insert("Predictor", Object::Integer(1));
        pdf.set_object(ObjectRef::new(7, 0), Object::Dictionary(decode_params));

        coalesce_page_contents(&mut pdf, ObjectRef::new(3, 0)).unwrap();

        let page = pdf.resolve_borrowed(ObjectRef::new(3, 0)).unwrap();
        let new_stream_ref = page
            .as_dict()
            .and_then(|dict| dict.get("Contents"))
            .and_then(Object::as_ref_id)
            .expect("coalesce should replace /Contents with a stream reference");
        let stream = pdf
            .resolve(new_stream_ref)
            .unwrap()
            .into_stream()
            .expect("coalesce target should be a stream");
        let mut expected = body1.to_vec();
        expected.push(b'\n');
        expected.extend_from_slice(body2);
        assert_eq!(stream.data, expected);
    }

    #[test]
    fn coalesce_page_contents_avoids_separator_after_trailing_newline() {
        let body1 = b"q 1 0 0 1 0 0 cm\n";
        let body2 = b"Q";
        let bytes = build_pdf_with_binary_extras(
            "[4 0 R 5 0 R]",
            &[
                (4, stream_object_bytes(4, body1)),
                (5, stream_object_bytes(5, body2)),
            ],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");

        coalesce_page_contents(&mut pdf, ObjectRef::new(3, 0)).unwrap();

        let page = pdf.resolve_borrowed(ObjectRef::new(3, 0)).unwrap();
        let new_stream_ref = page
            .as_dict()
            .and_then(|dict| dict.get("Contents"))
            .and_then(Object::as_ref_id)
            .expect("coalesce should replace /Contents with a stream reference");
        let stream = pdf
            .resolve(new_stream_ref)
            .unwrap()
            .into_stream()
            .expect("coalesce target should be a stream");
        let mut expected = body1.to_vec();
        expected.extend_from_slice(body2);
        assert_eq!(stream.data, expected);
    }

    #[test]
    fn coalesce_page_contents_propagates_canonical_filter_error() {
        let body1 = b"q 1 0 0 1 0 0 cm";
        let body2 = b"Q";
        let bytes = build_pdf_with_binary_extras(
            "[4 0 R 5 0 R]",
            &[
                (4, stream_object_bytes(4, body1)),
                (5, stream_object_bytes(5, body2)),
            ],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");

        let mut stream_dict = Dictionary::new();
        stream_dict.insert("Filter", Object::Reference(ObjectRef::new(6, 0)));
        pdf.set_object(
            ObjectRef::new(4, 0),
            Object::Stream(Stream::new(stream_dict, body1.to_vec())),
        );
        pdf.set_object(ObjectRef::new(6, 0), Object::Integer(1));

        let result = coalesce_page_contents(&mut pdf, ObjectRef::new(3, 0));
        assert!(
            matches!(result, Err(Error::Unsupported(message)) if message.contains("stream filter type is not name or array"))
        );
    }

    #[test]
    fn canonical_content_handles_rejects_non_array_and_wrong_length() {
        let page_ref = ObjectRef::new(3, 0);
        assert!(canonical_content_handles(ObjectHandle::integer(1), 0, page_ref).is_err());
        assert!(canonical_content_handles(
            ObjectHandle::array(vec![ObjectHandle::null()]),
            0,
            page_ref
        )
        .is_err());
    }

    #[test]
    fn content_stream_dict_rejects_non_stream() {
        assert!(content_stream_dict(&ObjectHandle::integer(1), ObjectRef::new(3, 0)).is_err());
    }

    // -----------------------------------------------------------------------
    // Test: non-Page object → Error::Unsupported
    // -----------------------------------------------------------------------

    #[test]
    fn page_content_bytes_rejects_non_page_ref() {
        let bytes = build_pdf_with_pages_node_as_obj3();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        // Object 3 is a Pages node, not a Page.
        let result = page_content_bytes(&mut pdf, ObjectRef::new(3, 0));
        assert!(result.is_err(), "expected error for non-Page ref, got Ok");
        match result.unwrap_err() {
            Error::Unsupported(_) => {}
            err => panic!("expected Error::Unsupported, got {err:?}"),
        }
    }

    #[test]
    fn page_content_bytes_rejects_non_name_page_type() {
        let bytes = build_pdf_with_contents("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        let mut page_dict = Dictionary::new();
        page_dict.insert("Type", Object::Integer(17));
        pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page_dict));

        let err = page_content_bytes(&mut pdf, ObjectRef::new(3, 0)).unwrap_err();
        assert!(
            matches!(&err, Error::Unsupported(message) if message.contains("non-name /Type")),
            "expected non-name /Type error, got {err:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test: /Contents = direct inline Stream (edge case, not typical in PDFs)
    // -----------------------------------------------------------------------

    #[test]
    fn page_content_bytes_handles_direct_stream_in_contents() {
        // We build a PDF where /Contents holds an inline Stream via set_object.
        let base_bytes = build_pdf_with_contents("", &[]);
        let mut pdf = Pdf::open(Cursor::new(base_bytes)).expect("PDF should parse");

        // Build a Page dict that has a direct Stream in /Contents.
        let content_body = b"BT /F1 12 Tf (Direct) Tj ET";
        let stream = Stream::new(Dictionary::new(), content_body.to_vec());
        let mut page_dict = Dictionary::new();
        page_dict.insert("Type", Object::Name(b"Page".to_vec()));
        page_dict.insert("Contents", Object::Stream(stream));
        pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page_dict));

        let content = page_content_bytes(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        assert_eq!(content, content_body);
    }

    // -----------------------------------------------------------------------
    // Tests: page_content_stream_entries (streams paired with terminal refs)
    // -----------------------------------------------------------------------

    /// Build a FlateDecode content-stream object `num 0 obj` whose payload is
    /// encoded via the in-crate `encode_stream_data` so it round-trips through
    /// `decode_stream_data`.
    fn flate_content_object_bytes(num: u32) -> Vec<u8> {
        let mut flate_dict = Dictionary::new();
        flate_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        let encoded = encode_stream_data(&flate_dict, b"BT /F1 12 Tf (hi) Tj ET").unwrap();

        let mut stream_bytes = Vec::new();
        stream_bytes.extend_from_slice(
            format!(
                "{num} 0 obj\n<< /Filter /FlateDecode /Length {} >>\nstream\n",
                encoded.len()
            )
            .as_bytes(),
        );
        stream_bytes.extend_from_slice(&encoded);
        stream_bytes.extend_from_slice(b"\nendstream\nendobj\n");
        stream_bytes
    }

    /// Build a single-page PDF whose `/Contents` is `4 0 R`, a FlateDecode
    /// content stream.
    fn single_page_with_flate_content() -> Vec<u8> {
        build_pdf_with_binary_extras("4 0 R", &[(4, flate_content_object_bytes(4))])
    }

    #[test]
    fn content_stream_entries_yield_terminal_ref() {
        // Single page whose /Contents is `4 0 R` (a FlateDecode stream).
        let bytes = single_page_with_flate_content();
        let mut pdf = Pdf::open(std::io::Cursor::new(bytes)).unwrap();
        let page = page_refs(&mut pdf).unwrap()[0];
        let entries = page_content_stream_entries(&mut pdf, page).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, Some(ObjectRef::new(4, 0)));
        assert!(crate::filters::decode_stream_data(&entries[0].1.dict, &entries[0].1.data).is_ok());
    }

    #[test]
    fn content_stream_entries_follows_two_hop_holder_chain() {
        // /Contents 5 0 R, where `5 0 obj` = `6 0 R` and `6 0 obj` is the stream.
        // The terminal ref must be 6 0 R (not the first hop 5 0 R), proving the
        // function returns the *terminal* ref of the holder chain.
        let bytes = build_pdf_with_binary_extras(
            "5 0 R",
            &[
                (5, b"5 0 obj\n6 0 R\nendobj\n".to_vec()),
                (6, flate_content_object_bytes(6)),
            ],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Reference(ObjectRef::new(6, 0)),
        );
        let entries = page_content_stream_entries(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        assert_eq!(entries.len(), 1);
        // Terminal ref is the stream (6 0 R), not the first hop (5 0 R).
        assert_eq!(entries[0].0, Some(ObjectRef::new(6, 0)));
        assert!(crate::filters::decode_stream_data(&entries[0].1.dict, &entries[0].1.data).is_ok());
    }

    #[test]
    fn content_stream_entries_accepts_reference_to_array() {
        // /Contents 5 0 R, where `5 0 obj` = `[6 0 R]` (a legal indirect array of
        // stream refs) and `6 0 obj` is the stream. qpdf accepts this shape.
        let bytes = build_pdf_with_binary_extras(
            "5 0 R",
            &[
                (5, b"5 0 obj\n[ 6 0 R ]\nendobj\n".to_vec()),
                (6, flate_content_object_bytes(6)),
            ],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        let entries = page_content_stream_entries(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, Some(ObjectRef::new(6, 0)));
        assert!(crate::filters::decode_stream_data(&entries[0].1.dict, &entries[0].1.data).is_ok());
    }

    #[test]
    fn content_stream_entries_rejects_reference_to_non_stream_non_array() {
        // /Contents 5 0 R, where `5 0 obj` is an Integer — neither a stream nor
        // an array → the Reference arm's catch-all error fires.
        let bytes =
            build_pdf_with_binary_extras("5 0 R", &[(5, b"5 0 obj\n42\nendobj\n".to_vec())]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        let err = page_content_stream_entries(&mut pdf, ObjectRef::new(3, 0)).unwrap_err();
        assert!(
            matches!(&err, Error::Unsupported(msg) if msg.contains("not a stream or array")),
            "expected Unsupported(\"…not a stream or array\"), got {err:?}"
        );
    }

    #[test]
    fn content_stream_entries_rejects_direct_non_stream_contents() {
        // /Contents 42 (a direct Integer, neither stream/reference/array) → the
        // top-level catch-all error fires.
        let bytes = build_pdf_with_contents("42", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        let err = page_content_stream_entries(&mut pdf, ObjectRef::new(3, 0)).unwrap_err();
        assert!(
            matches!(&err, Error::Unsupported(msg) if msg.contains("has unexpected type")),
            "expected Unsupported(\"…has unexpected type\"), got {err:?}"
        );
    }

    #[test]
    fn content_stream_entries_rejects_array_element_of_unexpected_type() {
        // /Contents [42] — an array whose element is an Integer (not a stream or
        // reference) → the array-element catch-all error fires.
        let bytes = build_pdf_with_contents("[ 42 ]", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        let err = page_content_stream_entries(&mut pdf, ObjectRef::new(3, 0)).unwrap_err();
        assert!(
            matches!(&err, Error::Unsupported(msg) if msg.contains("is not a stream or reference")),
            "expected Unsupported(\"…is not a stream or reference\"), got {err:?}"
        );
    }

    #[test]
    fn tolerant_content_stream_entries_skip_mixed_array_non_streams() {
        let bytes = build_pdf_with_binary_extras(
            "[ 4 0 R null 5 0 R 42 ]",
            &[
                (4, flate_content_object_bytes(4)),
                (5, b"5 0 obj\n42\nendobj\n".to_vec()),
            ],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        let entries = page_content_stream_entries_tolerant(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, Some(ObjectRef::new(4, 0)));
    }

    #[test]
    fn tolerant_content_stream_entries_skip_non_stream_contents() {
        for bytes in [
            build_pdf_with_contents("42", &[]),
            build_pdf_with_binary_extras("5 0 R", &[(5, b"5 0 obj\n42\nendobj\n".to_vec())]),
        ] {
            let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
            let entries =
                page_content_stream_entries_tolerant(&mut pdf, ObjectRef::new(3, 0)).unwrap();
            assert!(entries.is_empty());
        }
    }

    #[test]
    fn content_stream_entries_rejects_non_page_ref() {
        // Object 3 is a Pages node, not a Page → the /Type arm errors.
        let bytes = build_pdf_with_pages_node_as_obj3();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        let err = page_content_stream_entries(&mut pdf, ObjectRef::new(3, 0)).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)), "got {err:?}");
    }

    #[test]
    fn content_stream_entries_handles_direct_stream_in_array() {
        // /Contents = [<direct stream>]. Direct streams cannot be produced by
        // parsing (streams must be indirect), so inject one via set_object.
        let base_bytes = build_pdf_with_contents("", &[]);
        let mut pdf = Pdf::open(Cursor::new(base_bytes)).expect("PDF should parse");
        let stream = Stream::new(Dictionary::new(), b"BT /F1 12 Tf (hi) Tj ET".to_vec());
        let mut page_dict = Dictionary::new();
        page_dict.insert("Type", Object::Name(b"Page".to_vec()));
        page_dict.insert("Contents", Object::Array(vec![Object::Stream(stream)]));
        pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page_dict));

        let entries = page_content_stream_entries(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        assert_eq!(entries.len(), 1);
        // A direct stream has no terminal indirect ref.
        assert_eq!(entries[0].0, None);
    }

    #[test]
    fn content_stream_entries_empty_for_page_without_contents() {
        // Page 3 has no /Contents → returns an empty Vec, not an error.
        let bytes = build_pdf_with_contents("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        let entries = page_content_stream_entries(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn content_stream_entries_empty_for_null_contents_and_reference_to_null() {
        let cases = [
            build_pdf_with_contents("null", &[]),
            build_pdf_with_binary_extras("4 0 R", &[(4, b"4 0 obj\nnull\nendobj\n".to_vec())]),
        ];
        for bytes in cases {
            let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
            let entries = page_content_stream_entries(&mut pdf, ObjectRef::new(3, 0)).unwrap();
            assert!(entries.is_empty());
        }
    }

    #[test]
    fn content_stream_entries_rejects_non_dictionary_ref() {
        // Object 4 resolves to an Integer, so `as_dict()` yields None.
        let bytes = build_pdf_with_binary_extras("", &[(4, b"4 0 obj\n42\nendobj\n".to_vec())]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        let err = page_content_stream_entries(&mut pdf, ObjectRef::new(4, 0)).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)), "got {err:?}");
    }

    // -----------------------------------------------------------------------
    // Helpers for resolve_inherited_resources tests
    // -----------------------------------------------------------------------

    /// Build a PDF where the Page (3 0 R) has /Resources directly as a Dictionary.
    ///
    /// Object layout:
    ///   1 0 R  Catalog
    ///   2 0 R  Pages  (no /Resources)
    ///   3 0 R  Page   (/Resources << /Font << >> >>)
    fn build_pdf_page_has_direct_resources() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << >> >> >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 4\n0000000000 65535 f \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \n",
            off1, off2, off3,
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer =
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    /// Build a PDF where:
    ///   1 0 R  Catalog
    ///   2 0 R  Pages  (/Resources 4 0 R — indirect reference)
    ///   3 0 R  Page   (no /Resources, /Parent 2 0 R)
    ///   4 0 R  Resources dictionary (indirect object)
    fn build_pdf_resources_inherited_via_indirect_ref() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources 4 0 R >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let off4 = pdf.len() as u64;
        // The actual Resources dictionary as an indirect object
        pdf.extend_from_slice(b"4 0 obj\n<< /Font << /F1 << /Type /Font >> >> >>\nendobj\n");

        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 5\n0000000000 65535 f \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \n",
            off1, off2, off3, off4,
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer =
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    /// Build a PDF where no node has /Resources at all.
    ///
    /// Object layout:
    ///   1 0 R  Catalog
    ///   2 0 R  Pages  (no /Resources)
    ///   3 0 R  Page   (no /Resources, /Parent 2 0 R)
    fn build_pdf_no_resources() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 4\n0000000000 65535 f \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \n",
            off1, off2, off3,
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer =
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    // -----------------------------------------------------------------------
    // Test (a): Page itself has /Resources as a direct Dictionary
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_inherited_resources_page_has_direct_dict() {
        let bytes = build_pdf_page_has_direct_resources();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        let page_ref = ObjectRef::new(3, 0);
        let result = resolve_inherited_resources(&mut pdf, page_ref).expect("should succeed");
        let dict = result.expect("should find /Resources");
        // The page's /Resources has a /Font key
        assert!(
            dict.get("Font").is_some(),
            "expected /Font key in resolved Resources dict"
        );
    }

    // -----------------------------------------------------------------------
    // Test (b): Page inherits /Resources from parent via an indirect Reference
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_inherited_resources_from_parent_via_indirect_ref() {
        let bytes = build_pdf_resources_inherited_via_indirect_ref();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        // Page (3 0 R) has no /Resources — it must be inherited from Pages (2 0 R)
        // which references Resources dict (4 0 R) via an indirect reference.
        let page_ref = ObjectRef::new(3, 0);
        let result = resolve_inherited_resources(&mut pdf, page_ref).expect("should succeed");
        let dict = result.expect("should find inherited /Resources via indirect ref");
        // The inherited Resources (4 0 R) has a /Font key
        assert!(
            dict.get("Font").is_some(),
            "expected /Font key in inherited Resources dict"
        );
    }

    // -----------------------------------------------------------------------
    // Test (c): No /Resources anywhere → Ok(None)
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_inherited_resources_none_when_absent() {
        let bytes = build_pdf_no_resources();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        let page_ref = ObjectRef::new(3, 0);
        let result =
            resolve_inherited_resources(&mut pdf, page_ref).expect("should succeed with Ok(None)");
        assert!(
            result.is_none(),
            "expected Ok(None) when no /Resources anywhere in the parent chain"
        );
    }

    #[test]
    fn resolve_inherited_resources_non_dictionary_parent_terminates_chain() {
        let bytes = build_pdf_no_resources();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        let mut page = pdf
            .resolve(ObjectRef::new(3, 0))
            .unwrap()
            .into_dict()
            .expect("page must be a dictionary");
        page.insert("Parent", Object::Integer(42));
        pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));

        assert!(resolve_inherited_resources(&mut pdf, ObjectRef::new(3, 0))
            .unwrap()
            .is_none());
    }

    // -----------------------------------------------------------------------
    // Test (d): Circular /Parent reference → does not crash; returns Err or Ok(None)
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_inherited_resources_cycle_does_not_crash() {
        // We build a PDF via set_object to introduce a cycle: Page 3 → Pages 2 → Page 3.
        // Use the no-resources PDF as a base, then patch /Parent of Pages (2 0 R) to point
        // back to Page (3 0 R), creating a cycle.
        let bytes = build_pdf_no_resources();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");

        // Patch Pages node (2 0 R) to have /Parent pointing back to Page (3 0 R)
        let mut pages_dict = Dictionary::new();
        pages_dict.insert("Type", Object::Name(b"Pages".to_vec()));
        pages_dict.insert(
            "Kids",
            Object::Array(vec![Object::Reference(ObjectRef::new(3, 0))]),
        );
        pages_dict.insert("Count", Object::Integer(1));
        pages_dict.insert("Parent", Object::Reference(ObjectRef::new(3, 0))); // cycle!
        pdf.set_object(ObjectRef::new(2, 0), Object::Dictionary(pages_dict));

        let page_ref = ObjectRef::new(3, 0);
        // Must not panic or loop forever; either Ok(None) or Err(Unsupported) is acceptable.
        let result = resolve_inherited_resources(&mut pdf, page_ref);
        match result {
            Ok(None) => {} // cycle detected via BTreeSet — acceptable
            Ok(Some(_)) => panic!("should not find resources in a cycle-only graph"),
            Err(Error::Unsupported(_)) => {} // depth exceeded or explicit cycle error — acceptable
            Err(e) => panic!("unexpected error variant: {e:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Test: /Resources null and /Parent null are treated as absent (PDF §7.3.9)
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_inherited_resources_null_falls_through_to_parent() {
        // Page has /Resources null → must continue inheritance to the Pages
        // node, which has a real /Resources dict.
        let bytes = build_pdf_resources_inherited_via_indirect_ref();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");

        let mut page_dict = Dictionary::new();
        page_dict.insert("Type", Object::Name(b"Page".to_vec()));
        page_dict.insert("Parent", Object::Reference(ObjectRef::new(2, 0)));
        page_dict.insert(
            "MediaBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(612),
                Object::Integer(792),
            ]),
        );
        page_dict.insert("Resources", Object::Null);
        pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page_dict));

        let page_ref = ObjectRef::new(3, 0);
        let result =
            resolve_inherited_resources(&mut pdf, page_ref).expect("null should be like absent");
        let dict = result.expect("should inherit /Resources from parent despite null on page");
        assert!(
            dict.get("Font").is_some(),
            "expected inherited /Font key when page's /Resources is null"
        );
    }

    #[test]
    fn resolve_inherited_resources_null_parent_terminates_chain() {
        // Page has no /Resources and /Parent null → must terminate at this
        // node and return Ok(None), rather than dereferencing null as a ref.
        let bytes = build_pdf_no_resources();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");

        let mut page_dict = Dictionary::new();
        page_dict.insert("Type", Object::Name(b"Page".to_vec()));
        page_dict.insert("Parent", Object::Null);
        page_dict.insert(
            "MediaBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(612),
                Object::Integer(792),
            ]),
        );
        pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page_dict));

        let page_ref = ObjectRef::new(3, 0);
        let result = resolve_inherited_resources(&mut pdf, page_ref)
            .expect("null /Parent should be like absent");
        assert!(
            result.is_none(),
            "expected Ok(None) when /Parent is null and no /Resources"
        );
    }

    // -----------------------------------------------------------------------
    // PageWalk tests
    // -----------------------------------------------------------------------

    /// Build a minimal valid PDF from a list of (object_number, body_literal) pairs.
    /// `catalog_ref` is the object number of the /Catalog object.
    fn pdf_from_objects(catalog_ref: u32, objects: &[(u32, &str)]) -> Vec<u8> {
        let mut data: Vec<u8> = b"%PDF-1.4\n".to_vec();
        let mut offsets: Vec<(u32, u64)> = Vec::new();
        for (num, body) in objects {
            let off = data.len() as u64;
            offsets.push((*num, off));
            data.extend_from_slice(format!("{num} 0 obj\n{body}\nendobj\n").as_bytes());
        }
        let xref_start = data.len() as u64;
        let max_num = offsets.iter().map(|(n, _)| *n).max().unwrap_or(0);
        let total = max_num as usize + 1;
        let mut xref = format!("xref\n0 {total}\n0000000000 65535 f \n");
        for i in 1..=max_num {
            if let Some((_, off)) = offsets.iter().find(|(n, _)| *n == i) {
                xref.push_str(&format!("{off:010} 00000 n \n"));
            } else {
                xref.push_str("0000000000 65535 f \n");
            }
        }
        data.extend_from_slice(xref.as_bytes());
        let trailer = format!(
            "trailer\n<< /Size {total} /Root {catalog_ref} 0 R >>\nstartxref\n{xref_start}\n%%EOF\n"
        );
        data.extend_from_slice(trailer.as_bytes());
        data
    }

    #[test]
    fn page_walk_single_page_yields_one_ref() {
        let bytes = pdf_from_objects(
            1,
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            ],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let refs: Vec<ObjectRef> = PageWalk::new(&mut pdf)
            .unwrap()
            .collect::<Result<_>>()
            .unwrap();
        assert_eq!(refs, vec![ObjectRef::new(3, 0)]);
    }

    #[test]
    fn page_walk_sibling_order_is_document_order() {
        let bytes = pdf_from_objects(
            1,
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            ],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let refs: Vec<ObjectRef> = PageWalk::new(&mut pdf)
            .unwrap()
            .collect::<Result<_>>()
            .unwrap();
        assert_eq!(
            refs,
            vec![ObjectRef::new(3, 0), ObjectRef::new(4, 0)],
            "pages must be yielded in document order (3 before 4)"
        );
    }

    #[test]
    fn page_walk_nested_tree_document_order() {
        // Tree: Pages(2) -> [Pages(3), Page(6)]
        //       Pages(3) -> [Page(4), Page(5)]
        // Expected yield order: 4, 5, 6
        let bytes = pdf_from_objects(
            1,
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 3 >>"),
                (
                    3,
                    "<< /Type /Pages /Parent 2 0 R /Kids [4 0 R 5 0 R] /Count 2 >>",
                ),
                (4, "<< /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] >>"),
                (5, "<< /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] >>"),
                (6, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            ],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let refs: Vec<ObjectRef> = PageWalk::new(&mut pdf)
            .unwrap()
            .collect::<Result<_>>()
            .unwrap();
        assert_eq!(
            refs,
            vec![
                ObjectRef::new(4, 0),
                ObjectRef::new(5, 0),
                ObjectRef::new(6, 0),
            ],
            "pages must be yielded in document order across nested Pages nodes"
        );
    }

    #[test]
    fn page_walk_cycle_does_not_crash() {
        // Cycle: Pages(2) -> [Pages(3)], Pages(3) -> [Pages(2)].
        // seen-set breaks the cycle; no pages exist so result is Ok([]).
        let bytes = pdf_from_objects(
            1,
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 0 >>"),
                (3, "<< /Type /Pages /Kids [2 0 R] /Count 0 >>"),
            ],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let result: Result<Vec<ObjectRef>> = PageWalk::new(&mut pdf).unwrap().collect();
        assert_eq!(result.unwrap(), vec![]);
    }

    #[test]
    fn page_walk_depth_limit_returns_err() {
        // max_depth=1: the Pages root is at depth 0; its kid is pushed at depth 1
        // which equals max_depth, so popping it triggers a depth-limit error.
        let bytes = pdf_from_objects(
            1,
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            ],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let result: Result<Vec<ObjectRef>> =
            PageWalk::with_max_depth(&mut pdf, 1).unwrap().collect();
        match result {
            Err(Error::Unsupported(_)) => {}
            other => panic!("expected Err(Unsupported) for depth limit, got {other:?}"),
        }
    }

    #[test]
    fn page_walk_fused_after_depth_err() {
        // After yielding Err the iterator must return None (fused).
        let bytes = pdf_from_objects(
            1,
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            ],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let mut walk = PageWalk::with_max_depth(&mut pdf, 1).unwrap();
        let first = walk.next();
        assert!(
            matches!(first, Some(Err(Error::Unsupported(_)))),
            "first item should be Err(Unsupported)"
        );
        assert!(walk.next().is_none(), "iterator must be fused after Err");
    }

    #[test]
    fn page_walk_pages_node_without_kids_is_skipped() {
        // A Pages node with no /Kids entry: silently produces no pages.
        let bytes = pdf_from_objects(
            1,
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Count 0 >>"),
            ],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let refs: Vec<ObjectRef> = PageWalk::new(&mut pdf)
            .unwrap()
            .collect::<Result<_>>()
            .unwrap();
        assert!(refs.is_empty());
    }

    #[test]
    fn page_walk_non_ref_kid_is_ignored() {
        // Non-Reference element in /Kids must be silently ignored.
        let bytes = pdf_from_objects(
            1,
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            ],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // Patch the Pages node to include an Integer kid alongside the real Page ref.
        let mut pages_dict = Dictionary::new();
        pages_dict.insert("Type", Object::Name(b"Pages".to_vec()));
        pages_dict.insert(
            "Kids",
            Object::Array(vec![
                Object::Integer(999), // non-Reference: must be ignored
                Object::Reference(ObjectRef::new(3, 0)),
            ]),
        );
        pages_dict.insert("Count", Object::Integer(1));
        pdf.set_object(ObjectRef::new(2, 0), Object::Dictionary(pages_dict));

        let refs: Vec<ObjectRef> = PageWalk::new(&mut pdf)
            .unwrap()
            .collect::<Result<_>>()
            .unwrap();
        assert_eq!(refs, vec![ObjectRef::new(3, 0)]);
    }

    #[test]
    fn page_walk_unknown_type_node_is_skipped() {
        // A node with unknown /Type in the Kids list is silently skipped.
        let bytes = pdf_from_objects(
            1,
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 1 >>"),
                (3, "<< /Type /Widget /Parent 2 0 R >>"),
                (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            ],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let refs: Vec<ObjectRef> = PageWalk::new(&mut pdf)
            .unwrap()
            .collect::<Result<_>>()
            .unwrap();
        assert_eq!(
            refs,
            vec![ObjectRef::new(4, 0)],
            "Widget node must be skipped"
        );
    }

    #[test]
    fn page_walk_matches_page_refs() {
        // Regression: PageWalk and page_refs must produce identical results.
        let bytes = pdf_from_objects(
            1,
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 3 >>"),
                (
                    3,
                    "<< /Type /Pages /Parent 2 0 R /Kids [4 0 R 5 0 R] /Count 2 >>",
                ),
                (4, "<< /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] >>"),
                (5, "<< /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] >>"),
                (6, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            ],
        );
        let bytes2 = bytes.clone();
        let mut pdf1 = Pdf::open(Cursor::new(bytes)).unwrap();
        let mut pdf2 = Pdf::open(Cursor::new(bytes2)).unwrap();

        let from_page_refs = page_refs(&mut pdf1).unwrap();
        let from_walk: Vec<ObjectRef> = PageWalk::new(&mut pdf2)
            .unwrap()
            .collect::<Result<_>>()
            .unwrap();
        assert_eq!(from_page_refs, from_walk);
    }

    #[test]
    fn page_refs_records_that_all_pages_were_requested() {
        let bytes = pdf_from_objects(
            1,
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>"),
            ],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        assert!(!pdf.ever_called_get_all_pages());
        assert_eq!(page_refs_with_max_depth(&mut pdf, 8).unwrap().len(), 1);
        assert!(pdf.ever_called_get_all_pages());
    }

    /// Build a raw indirect object whose body is a bare reference (`N 0 R`),
    /// i.e. a holder-chain carrier object.
    fn ref_carrier_object_bytes(num: u32, target: u32) -> Vec<u8> {
        format!("{num} 0 obj\n{target} 0 R\nendobj\n").into_bytes()
    }

    // -----------------------------------------------------------------------
    // Holder-chain (flpdf-3x23) tests: /Contents reached via ref → ref → value
    // must be followed to its terminal, not dropped at the first hop.
    // -----------------------------------------------------------------------

    // Site 1: /Contents = single indirect Reference behind a 2-hop chain.
    #[test]
    fn page_content_bytes_follows_single_contents_holder_chain() {
        let body = b"BT /F1 12 Tf (Chained) Tj ET";
        // /Contents 4 0 R ; 4 0 R → 5 0 R (carrier) ; 5 0 R is the stream.
        let carrier = ref_carrier_object_bytes(4, 5);
        let stream_bytes = stream_object_bytes(5, body);
        let bytes = build_pdf_with_binary_extras("4 0 R", &[(4, carrier), (5, stream_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        pdf.set_object(
            ObjectRef::new(4, 0),
            Object::Reference(ObjectRef::new(5, 0)),
        );
        let content = page_content_bytes(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        assert_eq!(content, body);
    }

    // Site 1 error arm: a 2-hop chain terminating at a non-stream is rejected.
    #[test]
    fn page_content_bytes_single_contents_chain_to_non_stream_errors() {
        // /Contents 4 0 R ; 4 0 R → 5 0 R ; 5 0 R is a dictionary, not a stream.
        let carrier = ref_carrier_object_bytes(4, 5);
        let non_stream = b"5 0 obj\n<< /NotAStream true >>\nendobj\n".to_vec();
        let bytes = build_pdf_with_binary_extras("4 0 R", &[(4, carrier), (5, non_stream)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        pdf.set_object(
            ObjectRef::new(4, 0),
            Object::Reference(ObjectRef::new(5, 0)),
        );
        let err = page_content_bytes(&mut pdf, ObjectRef::new(3, 0)).unwrap_err();
        // A single /Contents reference dispatches on its terminal type: a value
        // that is neither a stream nor an array hits the catch-all error.
        assert!(
            matches!(&err, Error::Unsupported(msg) if msg.contains("not a stream or array")),
            "expected Unsupported(\"…not a stream or array\"), got {err:?}"
        );
    }

    // Site 2: /Contents array element reached via a 2-hop chain.
    #[test]
    fn page_content_bytes_follows_array_element_holder_chain() {
        let body1 = b"q 1 0 0 1 0 0 cm";
        let body2 = b"BT /F1 12 Tf (World) Tj ET";
        // First element is direct (4 0 R → stream); second is chained (5 0 R → 6 0 R → stream).
        let stream1 = stream_object_bytes(4, body1);
        let carrier = ref_carrier_object_bytes(5, 6);
        let stream2 = stream_object_bytes(6, body2);
        let bytes = build_pdf_with_binary_extras(
            "[4 0 R 5 0 R]",
            &[(4, stream1), (5, carrier), (6, stream2)],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Reference(ObjectRef::new(6, 0)),
        );
        let content = page_content_bytes(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        let mut expected = body1.to_vec();
        expected.push(b'\n');
        expected.extend_from_slice(body2);
        assert_eq!(content, expected);
    }

    // Site 2 error arm: a chained array element terminating at a non-stream errors.
    #[test]
    fn page_content_bytes_array_element_chain_to_non_stream_errors() {
        let stream1 = stream_object_bytes(4, b"q Q");
        let carrier = ref_carrier_object_bytes(5, 6);
        let non_stream = b"6 0 obj\n<< /NotAStream true >>\nendobj\n".to_vec();
        let bytes = build_pdf_with_binary_extras(
            "[4 0 R 5 0 R]",
            &[(4, stream1), (5, carrier), (6, non_stream)],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Reference(ObjectRef::new(6, 0)),
        );
        let err = page_content_bytes(&mut pdf, ObjectRef::new(3, 0)).unwrap_err();
        assert!(
            matches!(&err, Error::Unsupported(msg) if msg.contains("does not resolve to a stream")),
            "expected Unsupported(\"…does not resolve to a stream\"), got {err:?}"
        );
    }

    // Site 4: inherited /Resources held behind a 2-hop chain on the parent node.
    #[test]
    fn resolve_inherited_resources_follows_holder_chain() {
        // 2 0 R Pages has /Resources 4 0 R ; 4 0 R → 5 0 R (carrier) ; 5 0 R is the dict.
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources 4 0 R >>\nendobj\n",
        );
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(b"4 0 obj\n5 0 R\nendobj\n"); // carrier
        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(b"5 0 obj\n<< /Font << /F1 << /Type /Font >> >> >>\nendobj\n");
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 6\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n{off4:010} 00000 n \n{off5:010} 00000 n \n",
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer =
            format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());

        let mut pdf = Pdf::open(Cursor::new(pdf)).expect("PDF should parse");
        pdf.set_object(
            ObjectRef::new(4, 0),
            Object::Reference(ObjectRef::new(5, 0)),
        );
        let result = resolve_inherited_resources(&mut pdf, ObjectRef::new(3, 0))
            .expect("should succeed")
            .expect("should find inherited /Resources via holder chain");
        assert!(
            result.get("Font").is_some(),
            "expected /Font key in chained inherited Resources dict"
        );
    }

    // Site 4 error arm: a chained /Resources terminating at a non-dict, non-null
    // value is rejected.
    #[test]
    fn resolve_inherited_resources_chain_to_non_dict_errors() {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources 4 0 R >>\nendobj\n",
        );
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(b"4 0 obj\n5 0 R\nendobj\n"); // carrier
        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(b"5 0 obj\n42\nendobj\n"); // integer, not a dict
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 6\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n{off4:010} 00000 n \n{off5:010} 00000 n \n",
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer =
            format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());

        let mut pdf = Pdf::open(Cursor::new(pdf)).expect("PDF should parse");
        pdf.set_object(
            ObjectRef::new(4, 0),
            Object::Reference(ObjectRef::new(5, 0)),
        );
        let err = resolve_inherited_resources(&mut pdf, ObjectRef::new(3, 0)).unwrap_err();
        assert!(
            matches!(&err, Error::Unsupported(msg) if msg.contains("does not resolve to a dictionary")),
            "expected Unsupported(\"…does not resolve to a dictionary\"), got {err:?}"
        );
    }

    // Site 4 null arm: a chained /Resources that resolves to null must fall
    // through inheritance to the parent's real /Resources (PDF §7.3.9).
    #[test]
    fn resolve_inherited_resources_chain_to_null_falls_through_to_parent() {
        // 3 0 R Page has /Resources 5 0 R ; 5 0 R → 6 0 R → null, so the page's
        // own /Resources is "absent" and inheritance continues to 2 0 R Pages,
        // which carries the real /Resources dict (4 0 R).
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources 4 0 R >>\nendobj\n",
        );
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources 5 0 R >>\nendobj\n",
        );
        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(b"4 0 obj\n<< /Font << /F1 << /Type /Font >> >> >>\nendobj\n");
        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(b"5 0 obj\n6 0 R\nendobj\n"); // carrier
        let off6 = pdf.len() as u64;
        pdf.extend_from_slice(b"6 0 obj\nnull\nendobj\n"); // terminal null
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 7\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n{off4:010} 00000 n \n{off5:010} 00000 n \n{off6:010} 00000 n \n",
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer =
            format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());

        let mut pdf = Pdf::open(Cursor::new(pdf)).expect("PDF should parse");
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Reference(ObjectRef::new(6, 0)),
        );
        let result = resolve_inherited_resources(&mut pdf, ObjectRef::new(3, 0))
            .expect("should succeed")
            .expect("null /Resources chain should fall through to parent's /Resources");
        assert!(
            result.get("Font").is_some(),
            "expected /Font key inherited from parent after null fall-through"
        );
    }
}
