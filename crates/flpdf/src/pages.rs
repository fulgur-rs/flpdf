//! Page-tree traversal helpers.
//!
//! Iterates the document's `/Pages` tree in the order described by ISO 32000-1 §7.7.3.2
//! and yields the `ObjectRef` of every leaf `Page` node. The walker tolerates broken
//! cycles (each node is visited at most once) and bounds its recursion via a configurable
//! depth limit, since malformed PDFs occasionally embed self-referential page trees.

use crate::filters::decode_stream_data;
use crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result, Stream};
use std::collections::BTreeSet;
use std::io::{Read, Seek};

/// Default recursion limit for [`page_refs`].
///
/// Real-world PDFs almost always fit within a couple of dozen levels; the limit is
/// generous enough for legitimate documents while still preventing pathological inputs
/// from causing unbounded recursion.
pub const DEFAULT_MAX_PAGE_TREE_DEPTH: usize = 100;

/// Return every `Page` object in document order using [`DEFAULT_MAX_PAGE_TREE_DEPTH`].
///
/// Returns [`Error::Missing`] if the catalog or `/Pages` entry is absent. Returns
/// [`Error::Unsupported`] if the page tree exceeds the depth limit or the catalog is
/// not a dictionary.
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
pub fn page_refs_with_max_depth<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    max_depth: usize,
) -> Result<Vec<ObjectRef>> {
    let catalog_ref = pdf.root_ref().ok_or(Error::Missing("/Root"))?;
    let catalog = pdf.resolve(catalog_ref)?;
    let Object::Dictionary(catalog) = catalog else {
        return Err(Error::Unsupported(format!(
            "document catalog {catalog_ref} is not a dictionary"
        )));
    };
    let pages_ref = catalog.get_ref("Pages").ok_or(Error::Missing("/Pages"))?;

    let mut seen = BTreeSet::new();
    let mut pages = Vec::new();
    walk_page_tree(pdf, pages_ref, &mut seen, &mut pages, 0, max_depth)?;
    Ok(pages)
}

/// Return the decoded content-stream bytes for a single `Page` object.
///
/// The page's `/Contents` entry may be absent (returns `Ok(Vec::new())`), a single
/// `Stream` or `Reference → Stream`, or an `Array` of such references.  Every stream
/// is decoded through its filter pipeline via [`crate::filters::decode_stream_data`]
/// and the resulting byte slices are concatenated with a single ASCII space (`b' '`)
/// between each part, which is safe as a token boundary for PDF content-stream
/// tokenisers (matching the convention used by lopdf and qpdf).
///
/// # Errors
///
/// - [`Error::Unsupported`] when `page_ref` does not resolve to a dictionary with
///   `/Type /Page`, or when a `/Contents` element is not a stream.
/// - Any [`Error`] that [`Pdf::resolve`] or [`crate::filters::decode_stream_data`]
///   may return.
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
    // Resolve the page object itself.
    let page_obj = pdf.resolve(page_ref)?;
    let Object::Dictionary(page_dict) = page_obj else {
        return Err(Error::Unsupported(format!(
            "object {page_ref} is not a dictionary, cannot extract /Contents"
        )));
    };
    // Verify the /Type is /Page.
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

    // Extract the /Contents entry.  Absence is not an error (empty page).
    let contents = match page_dict.get("Contents").cloned() {
        None => return Ok(Vec::new()),
        Some(c) => c,
    };

    // Collect every stream that /Contents references.
    let streams: Vec<Stream> = collect_content_streams(pdf, &contents, page_ref)?;

    if streams.is_empty() {
        return Ok(Vec::new());
    }

    // Decode each stream and join with a single space separator.
    let mut result: Vec<u8> = Vec::new();
    for (i, stream) in streams.into_iter().enumerate() {
        let decoded = decode_stream_data(&stream.dict, &stream.data)?;
        if i > 0 {
            result.push(b' ');
        }
        result.extend_from_slice(&decoded);
    }
    Ok(result)
}

/// Resolve a `/Contents` value into a flat list of `Stream`s, handling all three
/// legal forms: a direct `Stream`, a `Reference` to a stream, or an `Array` of
/// `Reference`s (or direct streams).
fn collect_content_streams<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    contents: &Object,
    page_ref: ObjectRef,
) -> Result<Vec<Stream>> {
    match contents {
        // Direct inline stream — valid per spec (§7.8.2 note) and used in test PDFs.
        Object::Stream(s) => Ok(vec![s.clone()]),

        // Indirect reference — must resolve to a stream.
        Object::Reference(r) => {
            let resolved = pdf.resolve(*r)?;
            match resolved {
                Object::Stream(s) => Ok(vec![s]),
                _ => Err(Error::Unsupported(format!(
                    "/Contents reference {r} on page {page_ref} does not resolve to a stream"
                ))),
            }
        }

        // Array — each element must be a Reference to a stream (or a direct stream).
        Object::Array(elems) => {
            let mut streams = Vec::with_capacity(elems.len());
            for elem in elems {
                match elem {
                    Object::Reference(r) => {
                        let resolved = pdf.resolve(*r)?;
                        match resolved {
                            Object::Stream(s) => streams.push(s),
                            _ => {
                                return Err(Error::Unsupported(format!(
                                    "/Contents array element {r} on page {page_ref} does not resolve to a stream"
                                )));
                            }
                        }
                    }
                    Object::Stream(s) => streams.push(s.clone()),
                    other => {
                        let type_name = object_type_name(other);
                        return Err(Error::Unsupported(format!(
                            "/Contents array element of type {type_name} on page {page_ref} is not a stream or reference"
                        )));
                    }
                }
            }
            Ok(streams)
        }

        other => Err(Error::Unsupported(format!(
            "/Contents entry on page {page_ref} has unexpected type {}",
            object_type_name(other)
        ))),
    }
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
///    re-encoding is the responsibility of the write path / flpdf-9hc.12.5).
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
pub fn coalesce_page_contents<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<()> {
    // ── 1. Resolve the page dictionary ────────────────────────────────────────
    let page_obj = pdf.resolve(page_ref)?;
    let Object::Dictionary(page_dict) = page_obj else {
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

    // ── 3. Decode each stream and concatenate with '\n' separators ─────────────
    let mut coalesced: Vec<u8> = Vec::new();
    for (i, elem) in refs.iter().enumerate() {
        let stream: Stream = match elem {
            Object::Reference(r) => {
                let resolved = pdf.resolve(*r)?;
                match resolved {
                    Object::Stream(s) => s,
                    _ => {
                        return Err(Error::Unsupported(format!(
                            "/Contents array element {r} on page {page_ref} does not resolve to a stream"
                        )));
                    }
                }
            }
            Object::Stream(s) => s.clone(),
            other => {
                let type_name = object_type_name(other);
                return Err(Error::Unsupported(format!(
                    "/Contents array element of type {type_name} on page {page_ref} is not a stream or reference"
                )));
            }
        };

        let decoded = decode_stream_data(&stream.dict, &stream.data)?;
        if i > 0 {
            coalesced.push(b'\n');
        }
        coalesced.extend_from_slice(&decoded);
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
    let new_stream = Stream::new(Dictionary::new(), coalesced);
    pdf.set_object(new_stream_ref, Object::Stream(new_stream));

    // ── 6. Re-resolve the page dictionary (it may have been evicted) and patch /Contents ─
    let page_obj2 = pdf.resolve(page_ref)?;
    let Object::Dictionary(mut new_page_dict) = page_obj2 else {
        return Err(Error::Unsupported(format!(
            "object {page_ref} unexpectedly not a dictionary after coalesce"
        )));
    };
    new_page_dict.insert("Contents", Object::Reference(new_stream_ref));
    pdf.set_object(page_ref, Object::Dictionary(new_page_dict));

    Ok(())
}

fn object_type_name(obj: &Object) -> &'static str {
    match obj {
        Object::Null => "null",
        Object::Boolean(_) => "boolean",
        Object::Integer(_) => "integer",
        Object::Real(_) => "real",
        Object::Name(_) => "name",
        Object::String(_) => "string",
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
///   extremely deeply nested document).
/// - Any [`Error`] propagated from [`Pdf::resolve`].
pub fn resolve_inherited_resources<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<Option<Dictionary>> {
    resolve_inherited_resources_with_max_depth(pdf, page_ref, DEFAULT_MAX_PAGE_TREE_DEPTH)
}

/// Like [`resolve_inherited_resources`] but with a caller-supplied recursion limit.
pub fn resolve_inherited_resources_with_max_depth<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    max_depth: usize,
) -> Result<Option<Dictionary>> {
    let mut seen: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut current = page_ref;
    let mut depth: usize = 0;

    loop {
        if depth >= max_depth {
            return Err(Error::Unsupported(format!(
                "page tree depth exceeds maximum of {max_depth} at {current}"
            )));
        }

        // Cycle guard: if we have already visited this node, stop.
        if !seen.insert(current) {
            return Ok(None);
        }

        let node_obj = pdf.resolve(current)?;
        let Object::Dictionary(dict) = node_obj else {
            // Not a dictionary — cannot walk further.
            return Ok(None);
        };

        // Check for /Resources on this node. Per PDF §7.3.9, a null value is
        // equivalent to the key being absent — so Object::Null (and references
        // that resolve to null) fall through to the /Parent chain.
        if let Some(resources_val) = dict.get("Resources").cloned() {
            match resources_val {
                Object::Null => {}
                Object::Dictionary(d) => return Ok(Some(d)),
                Object::Reference(r) => {
                    let resolved = pdf.resolve(r)?;
                    match resolved {
                        Object::Null => {}
                        Object::Dictionary(d) => return Ok(Some(d)),
                        _ => {
                            return Err(Error::Unsupported(format!(
                                "/Resources reference {r} on node {current} does not resolve to a dictionary"
                            )));
                        }
                    }
                }
                _ => {
                    return Err(Error::Unsupported(format!(
                        "/Resources entry on node {current} has unexpected type"
                    )));
                }
            }
        }

        // No /Resources here — try the /Parent. A null /Parent is equivalent
        // to no /Parent at all (PDF §7.3.9), so stop walking in either case.
        let parent_val = match dict.get("Parent").cloned() {
            Some(Object::Null) | None => return Ok(None),
            Some(v) => v,
        };

        match parent_val {
            Object::Reference(r) => {
                current = r;
                depth += 1;
            }
            // Direct dictionary as /Parent is non-standard; treat as absent.
            _ => return Ok(None),
        }
    }
}

fn walk_page_tree<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    node: ObjectRef,
    seen: &mut BTreeSet<ObjectRef>,
    pages: &mut Vec<ObjectRef>,
    depth: usize,
    max_depth: usize,
) -> Result<()> {
    if depth >= max_depth {
        return Err(Error::Unsupported(format!(
            "page tree depth exceeds maximum of {max_depth} at {node}"
        )));
    }

    if !seen.insert(node) {
        return Ok(());
    }

    let node_obj = pdf.resolve(node)?;
    let Object::Dictionary(dict) = node_obj else {
        return Ok(());
    };

    let node_type = dict
        .get("Type")
        .and_then(|value| match value {
            Object::Name(value) => Some(value.clone()),
            _ => None,
        })
        .unwrap_or_default();

    if node_type.as_slice() == b"Pages" {
        if let Some(Object::Array(kids)) = dict.get("Kids") {
            for kid in kids {
                if let Object::Reference(reference) = kid {
                    walk_page_tree(pdf, *reference, seen, pages, depth + 1, max_depth)?;
                }
            }
        }
        return Ok(());
    }

    if node_type.as_slice() == b"Page" {
        pages.push(node);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filters::encode_stream_data;
    use crate::Dictionary;
    use std::io::Cursor;

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
    /// then ASCII85Decode-decodes.  To produce bytes that round-trip correctly we therefore
    /// *encode* in the reverse order: first ASCII85Decode-encode, then FlateDecode-compress.
    /// We encode each step manually (single-name dicts) rather than using encode_stream_data
    /// with an Array dict, because encode_stream_data applies Array filters in the same
    /// left-to-right order as decode (which is the inverse of what we need here).
    ///
    /// This exercises the Array branch in decode_stream_data_with_filters_and_crypt
    /// (filters.rs L78-96).
    fn chained_filter_stream_object_bytes(num: u32, body: &[u8]) -> Vec<u8> {
        // Step 1: ASCII85Decode-encode (encode is the inverse of ASCII85Decode decode)
        let mut a85_dict = Dictionary::new();
        a85_dict.insert("Filter", Object::Name(b"ASCII85Decode".to_vec()));
        let after_a85 = encode_stream_data(&a85_dict, body).unwrap();

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
    // Test: /Contents = Array of References → decoded and joined with space
    // -----------------------------------------------------------------------

    #[test]
    fn page_content_bytes_concatenates_array_of_refs_with_space_separator() {
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
        expected.push(b' ');
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
}
