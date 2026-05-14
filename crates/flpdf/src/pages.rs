//! Page-tree traversal helpers.
//!
//! Iterates the document's `/Pages` tree in the order described by ISO 32000-1 §7.7.3.2
//! and yields the `ObjectRef` of every leaf `Page` node. The walker tolerates broken
//! cycles (each node is visited at most once) and bounds its recursion via a configurable
//! depth limit, since malformed PDFs occasionally embed self-referential page trees.

use crate::filters::decode_stream_data;
use crate::{Error, Object, ObjectRef, Pdf, Result, Stream};
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
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n"
                .to_string()
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

    /// Build raw stream object bytes with a FlateDecode filter.
    fn flate_stream_object_bytes(num: u32, body: &[u8]) -> Vec<u8> {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        let encoded = encode_stream_data(&dict, body).unwrap();
        let mut out = Vec::new();
        out.extend_from_slice(
            format!(
                "{num} 0 obj\n<< /Filter /FlateDecode /Length {} >>\nstream\n",
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
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n"
                .to_string()
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
    // Test: /Contents stream with chained FlateDecode filter
    // -----------------------------------------------------------------------

    #[test]
    fn page_content_bytes_applies_chained_filters() {
        let body = b"q 0.5 g 100 100 300 300 re f Q";
        let stream_bytes = flate_stream_object_bytes(4, body);
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
}
