//! Page-tree traversal helpers.
//!
//! Iterates the document's `/Pages` tree in the order described by ISO 32000-1 §7.7.3.2
//! and yields the `ObjectRef` of every leaf `Page` node. The walker tolerates broken
//! cycles (each node is visited at most once) and bounds its recursion via a configurable
//! depth limit, since malformed PDFs occasionally embed self-referential page trees.

use crate::{Error, Object, ObjectRef, Pdf, Result};
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
