//! Surgical in-place splice of the `/Pages` tree.
//!
//! Unlike [`crate::page_tree_rebuild`], which always produces a flat single-level
//! tree, [`splice_pages`] preserves the existing multi-level `/Pages` structure
//! and performs a targeted depth-first walk to insert/remove pages at a specific
//! position, updating `/Count` at every ancestor node and repointing `/Parent`
//! on inserted pages.

use crate::pages::{page_refs_with_max_depth, DEFAULT_MAX_PAGE_TREE_DEPTH};
use crate::{Error, Object, ObjectRef, Pdf, Result};
use std::io::{Read, Seek};

/// Remove `remove.len()` pages starting at 0-based document-order position
/// `remove.start`, then insert `insert` at that position.
///
/// This is a **surgical** operation: the existing multi-level `/Pages` tree
/// structure is preserved. `/Count` is updated at every ancestor of the
/// affected nodes, and `/Parent` is repointed on every inserted page.
///
/// A no-op call (`remove.is_empty() && insert.is_empty()`) returns immediately
/// without touching the document.
///
/// # Errors
///
/// - [`Error::Unsupported`] if `remove.end > page_count`.
/// - [`Error::Missing`] if the result would be an empty document.
pub fn splice_pages<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    remove: std::ops::Range<usize>,
    insert: &[ObjectRef],
) -> Result<()> {
    splice_pages_with_max_depth(pdf, remove, insert, DEFAULT_MAX_PAGE_TREE_DEPTH)
}

/// Like [`splice_pages`] but with an explicit page-tree depth limit.
pub fn splice_pages_with_max_depth<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    remove: std::ops::Range<usize>,
    insert: &[ObjectRef],
    max_depth: usize,
) -> Result<()> {
    // No-op guard.
    if remove.is_empty() && insert.is_empty() {
        return Ok(());
    }

    let page_count = page_refs_with_max_depth(pdf, max_depth)?.len();

    if remove.end > page_count {
        return Err(Error::Unsupported(format!(
            "splice: remove.end {} exceeds page count {}",
            remove.end, page_count
        )));
    }

    let remaining = page_count - remove.len() + insert.len();
    if remaining == 0 {
        return Err(Error::Missing(
            "splice would result in an empty document",
        ));
    }

    let catalog_ref = pdf.root_ref().ok_or(Error::Missing("/Root"))?;
    let pages_ref = {
        let catalog = pdf.resolve(catalog_ref)?;
        catalog
            .as_dict()
            .ok_or(Error::Missing("/Catalog dict"))?
            .get_ref("Pages")
            .ok_or(Error::Missing("/Pages"))?
    };

    let mut insert_done = false;
    splice_subtree(
        pdf,
        pages_ref,
        0,
        &remove,
        insert,
        &mut insert_done,
        0,
        max_depth,
    )?;

    if !insert_done && !insert.is_empty() {
        return Err(Error::Unsupported(format!(
            "splice: insert position {} not found in page tree",
            remove.start
        )));
    }

    Ok(())
}

/// Returns the leaf-page count contributed by `node_ref`.
/// - `/Pages` → its `/Count` value
/// - `/Page` (or anything else) → 1
fn leaf_count_of<R: Read + Seek>(pdf: &mut Pdf<R>, node_ref: ObjectRef) -> Result<usize> {
    let obj = pdf.resolve_borrowed(node_ref)?;
    let dict = obj
        .as_dict()
        .ok_or_else(|| Error::Unsupported(format!("node {node_ref} is not a dictionary")))?;
    match dict.get("Type").and_then(Object::as_name) {
        Some(b"Pages") => match dict.get("Count").and_then(Object::as_integer) {
            Some(n) => Ok(n as usize),
            None => Err(Error::Unsupported(format!(
                "/Pages node {node_ref} has no /Count"
            ))),
        },
        _ => Ok(1),
    }
}

/// Sets `/Parent` on `page_ref` to point at `parent_ref`.
fn set_page_parent<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    parent_ref: ObjectRef,
) -> Result<()> {
    let mut dict = pdf
        .resolve(page_ref)?
        .into_dict()
        .ok_or_else(|| Error::Unsupported(format!("page {page_ref} is not a dictionary")))?;
    dict.insert("Parent", Object::Reference(parent_ref));
    pdf.set_object(page_ref, Object::Dictionary(dict));
    Ok(())
}

/// DFS splice for a single `/Pages` node.
///
/// Returns the **net change** in leaf count within this subtree
/// (positive = pages added, negative = pages removed).
///
/// `base` is the document-order index of the first leaf page in this subtree.
/// `insert_done` is shared across all recursive calls; it flips to `true` when
/// the inserted pages have been placed exactly once.
#[allow(clippy::too_many_arguments)]
fn splice_subtree<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    node_ref: ObjectRef,
    base: usize,
    remove: &std::ops::Range<usize>,
    insert: &[ObjectRef],
    insert_done: &mut bool,
    depth: usize,
    max_depth: usize,
) -> Result<i64> {
    if depth >= max_depth {
        return Err(Error::Unsupported(format!(
            "page-tree depth exceeds {max_depth} at {node_ref}"
        )));
    }

    // Snapshot the node's kids and count *before* any mutation so that
    // the borrow on `pdf` is released before we recurse.
    let (kids, old_count) = {
        let obj = pdf.resolve_borrowed(node_ref)?;
        let dict = obj
            .as_dict()
            .ok_or_else(|| Error::Unsupported(format!("{node_ref} is not a /Pages dictionary")))?;

        let kids: Vec<ObjectRef> = dict
            .get("Kids")
            .and_then(Object::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Object::as_ref_id)
                    .collect()
            })
            .unwrap_or_default();

        let old_count = dict
            .get("Count")
            .and_then(Object::as_integer)
            .ok_or_else(|| Error::Unsupported(format!("/Pages node {node_ref} has no /Count")))?
            as usize;

        (kids, old_count)
    };

    let mut new_kids: Vec<ObjectRef> = Vec::with_capacity(kids.len() + insert.len());
    let mut net_delta: i64 = 0;
    let mut offset = base;

    for kid_ref in kids {
        let kid_leaf_count = leaf_count_of(pdf, kid_ref)?;
        let kid_start = offset;
        let kid_end = offset + kid_leaf_count;

        // Insertion point: insert BEFORE this kid.
        if !*insert_done && remove.start == kid_start {
            for &page_ref in insert {
                new_kids.push(page_ref);
                set_page_parent(pdf, page_ref, node_ref)?;
            }
            net_delta += insert.len() as i64;
            *insert_done = true;
        }

        let overlaps_remove = kid_end > remove.start && kid_start < remove.end;
        if overlaps_remove {
            // Determine kid type (Page vs Pages) without holding a borrow.
            let kid_is_pages = {
                let kid_obj = pdf.resolve_borrowed(kid_ref)?;
                kid_obj
                    .as_dict()
                    .and_then(|d| d.get("Type"))
                    .and_then(Object::as_name)
                    == Some(b"Pages")
            };

            if kid_is_pages {
                let sub_delta = splice_subtree(
                    pdf,
                    kid_ref,
                    kid_start,
                    remove,
                    insert,
                    insert_done,
                    depth + 1,
                    max_depth,
                )?;
                net_delta += sub_delta;

                // Drop now-empty intermediate nodes.
                let new_sub_count = kid_leaf_count as i64 + sub_delta;
                if new_sub_count > 0 {
                    new_kids.push(kid_ref);
                }
            } else {
                // /Page leaf inside remove range: drop it.
                net_delta -= 1;
            }
        } else {
            new_kids.push(kid_ref);
        }

        offset = kid_end;
    }

    // Append case: insertion point is at the end of this node's kids.
    if !*insert_done && remove.start == offset {
        for &page_ref in insert {
            new_kids.push(page_ref);
            set_page_parent(pdf, page_ref, node_ref)?;
        }
        net_delta += insert.len() as i64;
        *insert_done = true;
    }

    // Write back the modified node.
    let new_count = (old_count as i64 + net_delta) as usize;
    let mut dict = pdf
        .resolve(node_ref)?
        .into_dict()
        .ok_or_else(|| Error::Unsupported(format!("{node_ref} is not a dictionary (re-resolve)")))?;
    dict.insert("Count", Object::Integer(new_count as i64));
    dict.insert(
        "Kids",
        Object::Array(new_kids.iter().map(|&r| Object::Reference(r)).collect()),
    );
    pdf.set_object(node_ref, Object::Dictionary(dict));

    Ok(net_delta)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::page_refs;
    use crate::Pdf;
    use std::io::Cursor;

    // Placeholder — tests added in later tasks.
    #[test]
    fn placeholder() {}
}
