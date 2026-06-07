//! Outline (`/Outlines`) traversal helpers.
//!
//! Walks the document outline in `First`/`Next`/`First` order (ISO 32000-1 §12.3.3) and
//! produces a flat list of [`OutlineItem`]s. Cycles introduced by hand-edited or damaged
//! PDFs are detected and ignored, and the recursion depth is bounded by a configurable
//! limit.

use crate::{Error, Object, ObjectRef, Pdf, Result};
use std::collections::BTreeSet;
use std::io::{Read, Seek};

/// Default recursion limit for [`outline_items`].
pub const DEFAULT_MAX_OUTLINE_DEPTH: usize = 100;

/// One entry in a document outline (sometimes called a bookmark).
///
/// `depth` is zero for top-level entries and increases for each nested level.
/// `title` is decoded as a PDF text string (UTF-16BE/LE with a byte-order mark,
/// otherwise PDFDocEncoding), falling back to [`String::from_utf8_lossy`] for bytes
/// that decode as neither. An indirect `/Title` reference is resolved one level
/// before decoding; an absent `/Title` yields `"<untitled>"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineItem {
    pub object_ref: ObjectRef,
    pub depth: usize,
    pub title: String,
}

/// Walk the document's outline tree using [`DEFAULT_MAX_OUTLINE_DEPTH`].
///
/// Returns an empty `Vec` if the catalog has no `/Outlines` entry or the outline root
/// has no `First` child.
///
/// # Errors
///
/// Returns [`Error::Unsupported`] if the outline nesting depth exceeds
/// [`DEFAULT_MAX_OUTLINE_DEPTH`]. Propagates any error from resolving outline objects
/// (for example I/O or parse failures surfaced by [`Pdf::resolve`]).
pub fn outline_items<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<Vec<OutlineItem>> {
    outline_items_with_max_depth(pdf, DEFAULT_MAX_OUTLINE_DEPTH)
}

/// Like [`outline_items`] but with a caller-supplied recursion limit.
///
/// # Errors
///
/// Returns [`Error::Unsupported`] if the outline nesting depth exceeds `max_depth`.
/// Propagates any error from resolving outline objects (for example I/O or parse
/// failures surfaced by [`Pdf::resolve`]).
pub fn outline_items_with_max_depth<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    max_depth: usize,
) -> Result<Vec<OutlineItem>> {
    let Some(catalog_ref) = pdf.root_ref() else {
        return Ok(Vec::new());
    };
    let catalog = pdf.resolve_borrowed(catalog_ref)?;
    let Object::Dictionary(catalog) = catalog else {
        return Ok(Vec::new());
    };
    let Some(outlines_ref) = catalog.get_ref("Outlines") else {
        return Ok(Vec::new());
    };
    let outline_root = pdf.resolve_borrowed(outlines_ref)?;
    let Object::Dictionary(outline_root) = outline_root else {
        return Ok(Vec::new());
    };
    let Some(first) = outline_root.get_ref("First") else {
        return Ok(Vec::new());
    };

    let mut visited = BTreeSet::new();
    let mut items = Vec::new();
    walk_outline(pdf, first, 0, &mut visited, &mut items, max_depth)?;
    Ok(items)
}

fn walk_outline<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    start: ObjectRef,
    depth: usize,
    visited: &mut BTreeSet<ObjectRef>,
    items: &mut Vec<OutlineItem>,
    max_depth: usize,
) -> Result<()> {
    if depth >= max_depth {
        return Err(Error::Unsupported(format!(
            "outline depth exceeds maximum of {max_depth} at {start}"
        )));
    }

    let mut current = Some(start);
    while let Some(current_ref) = current {
        if !visited.insert(current_ref) {
            break;
        }

        let current_obj = pdf.resolve_borrowed(current_ref)?;
        let Object::Dictionary(dict) = current_obj else {
            break;
        };

        let first = dict.get_ref("First");
        let next = dict.get_ref("Next");

        // Decode the title without cloning the `Object` (review rule 1). Direct
        // titles are decoded in place while `dict` still borrows `pdf`; an indirect
        // `/Title` only carries out the Copy `ObjectRef` (as `Err`), ending the
        // shared borrow of `pdf` so it can then be resolved with `&mut pdf`
        // (review rule 2).
        let title = match dict.get("Title") {
            Some(Object::Reference(r)) => Err(*r),
            Some(other) => Ok(decode_title_object(other)),
            None => Ok(String::from("<untitled>")),
        };
        let title = match title {
            Ok(title) => title,
            Err(title_ref) => decode_title_object(&pdf.resolve(title_ref)?),
        };

        items.push(OutlineItem {
            object_ref: current_ref,
            depth,
            title,
        });

        if let Some(first) = first {
            walk_outline(pdf, first, depth + 1, visited, items, max_depth)?;
        }

        current = next;
    }

    Ok(())
}

/// Decode an outline entry's `/Title` value (already resolved by the caller, which
/// handles one level of indirection — review rule 2). `Object::String` titles are
/// decoded as PDF text strings (UTF-16BE/LE BOM or PDFDocEncoding) with a
/// `from_utf8_lossy` fallback, so a BOM-prefixed UTF-16 title is no longer rendered
/// as mojibake. Non-string values fall back to their serialized form.
fn decode_title_object(obj: &Object) -> String {
    match obj {
        Object::String(bytes) => crate::json_inspect::decode_pdf_text_string(bytes)
            .unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned()),
        other => {
            let mut bytes = Vec::new();
            other.write_pdf(&mut bytes);
            String::from_utf8_lossy(&bytes).into_owned()
        }
    }
}
