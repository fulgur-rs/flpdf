//! qpdf correspondence: `QPDFPageDocumentHelper.cc:37-52` delegates page insertion/removal to the page-tree owner.
//! `QPDF_pages.cc:203-304` maintains `/Kids`, `/Count`, and `/Parent` during those mutations.
//! Surgical in-place splice of the `/Pages` tree.
//!
//! Unlike [`crate::page_tree_rebuild`], which always produces a flat single-level
//! tree, [`splice_pages`] preserves the existing multi-level `/Pages` structure
//! and performs a targeted depth-first walk to insert/remove pages at a specific
//! position, updating `/Count` at every ancestor node and repointing `/Parent`
//! on inserted pages.

use crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH;
#[cfg(test)]
use crate::Object;
use crate::ObjectHandle;
use crate::{Error, ObjectRef, Pdf, Result};
use std::collections::HashSet;
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
/// This function is part of the document page extraction and merge primitives.
/// The `insert` refs it splices in are typically pages copied
/// from another document with
/// [`copy_objects`](crate::object_copy::copy_objects), whose object set is first
/// computed per page by
/// [`page_object_closure`](crate::page_closure::page_object_closure). See the
/// runnable `examples/splice_pages.rs` and `examples/merge_pdfs.rs`.
///
/// # Errors
///
/// - [`Error::Unsupported`] if `remove.end` exceeds the document's page count.
/// - [`Error::Missing`] if the result would be an empty document.
pub fn splice_pages<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    remove: std::ops::Range<usize>,
    insert: &[ObjectRef],
) -> Result<()> {
    splice_pages_with_max_depth(pdf, remove, insert, DEFAULT_MAX_PAGE_TREE_DEPTH)
}

/// Like [`splice_pages`] but with an explicit page-tree depth limit.
///
/// # Errors
///
/// - [`Error::Unsupported`] if `remove.start > remove.end`, if
///   `remove.end` exceeds the document's page count, if the insert position is not found in the
///   page tree, or if the `/Pages` tree is malformed (a node deeper than
///   `max_depth`, a node that is not a dictionary, or a `/Pages` node with a
///   missing or negative `/Count`).
/// - [`Error::Missing`] if the result would be an empty document, or if a
///   required structural entry is absent (`/Root`, the `/Catalog` dictionary,
///   or `/Pages`).
/// - Propagates any error from resolving objects (for example a malformed
///   cross-reference table).
pub fn splice_pages_with_max_depth<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    remove: std::ops::Range<usize>,
    insert: &[ObjectRef],
    max_depth: usize,
) -> Result<()> {
    if remove.start > remove.end {
        return Err(Error::Unsupported(format!(
            "splice: invalid range {}..{} (start > end)",
            remove.start, remove.end
        )));
    }

    // No-op guard.
    if remove.is_empty() && insert.is_empty() {
        return Ok(());
    }

    let pages_ref = pages_ref(pdf)?;
    let existing_pages = page_refs_for_splice(pdf, pages_ref, max_depth)?;
    let page_count = existing_pages.len();

    if remove.end > page_count {
        return Err(Error::Unsupported(format!(
            "splice: remove.end {} exceeds page count {}",
            remove.end, page_count
        )));
    }

    let normalized_insert = normalize_insert_pages(pdf, &existing_pages, &remove, insert)?;
    let remaining = page_count - remove.len() + normalized_insert.len();
    if remaining == 0 {
        return Err(Error::Missing("splice would result in an empty document"));
    }

    let mut insert_done = false;
    splice_subtree(
        pdf,
        pages_ref,
        0,
        &remove,
        &normalized_insert,
        &mut insert_done,
        0,
        max_depth,
    )?;

    if !insert_done && !normalized_insert.is_empty() {
        return Err(Error::Unsupported(format!(
            "splice: insert position {} not found in page tree",
            remove.start
        )));
    }

    Ok(())
}

/// Resolve the catalog's `/Pages` entry to its canonical indirect identity.
///
/// qpdf's page-document helper hands mutation to the page-tree owner; keeping
/// this lookup on handles means an indirect `/Pages` entry is not flattened
/// into a legacy `Object` before the owner starts its walk.
fn pages_ref<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<ObjectRef> {
    let catalog_ref = pdf.root_ref().ok_or(Error::Missing("/Root"))?;
    let catalog = pdf.get_object_handle(catalog_ref);
    pdf.resolve_object_handle(&catalog)?;
    if catalog.as_dictionary().is_none() {
        return Err(Error::Missing("/Catalog dict"));
    }
    catalog
        .try_get_key(b"/Pages")?
        .object_ref()
        .ok_or(Error::Missing("/Pages"))
}

/// Snapshot the complete page order through canonical handles.
///
/// qpdf's page owner maintains a page-object index while flattening the tree
/// (`QPDF_pages.cc:154-188`). The index is also the duplicate-page boundary
/// used by `QPDF::insertPage` (`QPDF_pages.cc:233-237`), while the recursive
/// count check below mirrors the same flattening pass's `/Count` validation.
fn page_refs_for_splice<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    pages_ref: ObjectRef,
    max_depth: usize,
) -> Result<Vec<ObjectRef>> {
    let mut pages = Vec::new();
    collect_page_refs(pdf, pages_ref, 0, max_depth, &mut pages)?;
    Ok(pages)
}

fn collect_page_refs<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    node_ref: ObjectRef,
    depth: usize,
    max_depth: usize,
    pages: &mut Vec<ObjectRef>,
) -> Result<usize> {
    if depth >= max_depth {
        return Err(Error::Unsupported(format!(
            "page-tree depth exceeds {max_depth} at {node_ref}"
        )));
    }

    let node = pdf.get_object_handle(node_ref);
    pdf.resolve_object_handle(&node)?;
    if node.as_dictionary().is_none() {
        return Err(Error::Unsupported(format!(
            "node {node_ref} is not a dictionary"
        )));
    }

    let node_type = node.try_get_key(b"/Type")?;
    pdf.resolve_object_handle(&node_type)?;
    if node_type.as_name().as_deref() != Some(b"Pages") {
        pages.push(node_ref);
        return Ok(1);
    }

    let kids_value = node.try_get_key(b"/Kids")?;
    pdf.resolve_object_handle(&kids_value)?;
    let kids: Vec<ObjectRef> = kids_value
        .as_array()
        .unwrap_or_default()
        .into_iter()
        .map(|child| {
            child.object_ref().ok_or_else(|| {
                Error::Unsupported(format!(
                    "child of /Pages node {node_ref} is not an indirect object"
                ))
            })
        })
        .collect::<Result<_>>()?;

    let count_value = node.try_get_key(b"/Count")?;
    pdf.resolve_object_handle(&count_value)?;
    let declared_count = match count_value.as_integer() {
        Some(n) if n >= 0 => n as usize,
        Some(n) => {
            return Err(Error::Unsupported(format!(
                "/Pages node {node_ref} has negative /Count {n}"
            )))
        }
        None => {
            return Err(Error::Unsupported(format!(
                "/Pages node {node_ref} has no /Count"
            )))
        }
    };

    let mut actual_count = 0usize;
    for child_ref in kids {
        let child_count = collect_page_refs(pdf, child_ref, depth + 1, max_depth, pages)?;
        // cov:ignore-start: usize page-count overflow cannot be constructed by a finite PDF object tree
        actual_count = actual_count.checked_add(child_count).ok_or_else(|| {
            Error::Unsupported(format!("page count overflow at /Pages node {node_ref}"))
        })?;
        // cov:ignore-end
    }
    if declared_count != actual_count {
        return Err(Error::Unsupported(format!(
            "/Pages node {node_ref} has /Count {declared_count}, but /Kids contain {actual_count} pages"
        )));
    }

    Ok(actual_count)
}

/// Apply qpdf's duplicate-page boundary before the splice mutates `/Parent`.
///
/// A page removed by this operation is no longer an existing page at the
/// insertion boundary, so it may be reinserted with its original identity.
/// Every other duplicate is promoted from a shallow dictionary copy to a new
/// indirect object, matching `QPDF::insertPage`'s `shallowCopy` branch.
fn normalize_insert_pages<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    existing_pages: &[ObjectRef],
    remove: &std::ops::Range<usize>,
    insert: &[ObjectRef],
) -> Result<Vec<ObjectRef>> {
    let mut occupied = HashSet::with_capacity(existing_pages.len() + insert.len());
    for (index, &page_ref) in existing_pages.iter().enumerate() {
        if !remove.contains(&index) {
            occupied.insert(page_ref);
        }
    }

    let mut normalized = Vec::with_capacity(insert.len());
    for &page_ref in insert {
        if occupied.insert(page_ref) {
            normalized.push(page_ref);
            continue;
        }

        let page = pdf.get_object_handle(page_ref);
        pdf.resolve_object_handle(&page)?;
        let copy = page.shallow_copy()?;
        let indirect = pdf.make_indirect_object_handle(copy)?;
        // cov:ignore-start: make_indirect_object_handle guarantees a fresh indirect identity
        let copy_ref = indirect.object_ref().ok_or_else(|| {
            Error::Internal("shallow page copy did not receive an indirect identity".to_owned())
        })?;
        // cov:ignore-end
        occupied.insert(copy_ref);
        normalized.push(copy_ref);
    }
    Ok(normalized)
}

/// Returns the leaf-page count contributed by `node_ref`.
/// - `/Pages` → its `/Count` value
/// - another dictionary type → 1
fn leaf_count_of<R: Read + Seek>(pdf: &mut Pdf<R>, node_ref: ObjectRef) -> Result<usize> {
    let node = pdf.get_object_handle(node_ref);
    pdf.resolve_object_handle(&node)?;
    if node.as_dictionary().is_none() {
        return Err(Error::Unsupported(format!(
            "node {node_ref} is not a dictionary"
        )));
    }

    let node_type = node.try_get_key(b"/Type")?;
    pdf.resolve_object_handle(&node_type)?;
    if node_type.as_name().as_deref() != Some(b"Pages") {
        return Ok(1);
    }

    let count = node.try_get_key(b"/Count")?;
    pdf.resolve_object_handle(&count)?;
    match count.as_integer() {
        Some(n) if n >= 0 => Ok(n as usize),
        Some(n) => Err(Error::Unsupported(format!(
            "/Pages node {node_ref} has negative /Count {n}"
        ))),
        None => Err(Error::Unsupported(format!(
            "/Pages node {node_ref} has no /Count"
        ))),
    }
}

/// Sets `/Parent` on `page_ref` to point at `parent_ref`.
fn set_page_parent<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    parent_ref: ObjectRef,
) -> Result<()> {
    let page = pdf.get_object_handle(page_ref);
    pdf.resolve_object_handle(&page)?;
    if page.as_dictionary().is_none() {
        return Err(Error::Unsupported(format!(
            "page {page_ref} is not a dictionary"
        )));
    }
    page.replace_key(b"/Parent", pdf.get_object_handle(parent_ref))?;
    pdf.mark_object_dirty(page_ref);
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

    // Snapshot the node's kids and count *before* any mutation so that the
    // canonical node handle remains stable while we recurse.
    let (kids, old_count, kids_handle) = {
        let node = pdf.get_object_handle(node_ref);
        pdf.resolve_object_handle(&node)?;
        if node.as_dictionary().is_none() {
            return Err(Error::Unsupported(format!(
                "{node_ref} is not a /Pages dictionary"
            )));
        }

        let kids_value = node.try_get_key(b"/Kids")?;
        pdf.resolve_object_handle(&kids_value)?;
        let kids: Vec<ObjectRef> = kids_value
            .as_array()
            .unwrap_or_default()
            .into_iter()
            .map(|child| {
                child.object_ref().ok_or_else(|| {
                    Error::Unsupported(format!(
                        "child of /Pages node {node_ref} is not an indirect object"
                    ))
                })
            })
            .collect::<Result<_>>()?;
        let kids_handle = if kids_value.as_array().is_some() {
            Some(kids_value.clone())
        } else {
            None
        };

        let count_value = node.try_get_key(b"/Count")?;
        pdf.resolve_object_handle(&count_value)?;
        let old_count_raw = count_value
            .as_integer()
            .ok_or_else(|| Error::Unsupported(format!("/Pages node {node_ref} has no /Count")))?;
        if old_count_raw < 0 {
            return Err(Error::Unsupported(format!(
                "/Pages node {node_ref} has negative /Count {old_count_raw}"
            )));
        }
        let old_count = old_count_raw as usize;

        (kids, old_count, kids_handle)
    };

    let actual_count = kids.iter().try_fold(0usize, |total, &child_ref| {
        let child_count = leaf_count_of(pdf, child_ref)?;
        // cov:ignore-start: usize page-count overflow cannot be constructed by a finite PDF object tree
        total.checked_add(child_count).ok_or_else(|| {
            Error::Unsupported(format!("page count overflow at /Pages node {node_ref}"))
        })
        // cov:ignore-end
    })?;
    if old_count != actual_count {
        return Err(Error::Unsupported(format!(
            "/Pages node {node_ref} has /Count {old_count}, but /Kids contain {actual_count} pages"
        )));
    }

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
            // Determine kid type (Page vs Pages) through the live child handle.
            let kid_is_pages = {
                let kid = pdf.get_object_handle(kid_ref);
                pdf.resolve_object_handle(&kid)?;
                let kid_type = kid.try_get_key(b"/Type")?;
                pdf.resolve_object_handle(&kid_type)?;
                kid_type.as_name().as_deref() == Some(b"Pages")
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
    let new_count = old_count as i64 + net_delta;
    if new_count < 0 {
        return Err(Error::Unsupported(format!(
            "splice: negative page count {new_count} for node {node_ref}"
        )));
    }
    let node = pdf.get_object_handle(node_ref);
    pdf.resolve_object_handle(&node)?;
    node.replace_key(b"/Count", ObjectHandle::integer(new_count))?;
    let new_kid_handles = new_kids
        .iter()
        .map(|&child_ref| pdf.get_object_handle(child_ref))
        .collect();
    if let Some(kids_handle) = kids_handle {
        kids_handle.set_array_items(new_kid_handles)?;
        pdf.mark_object_handle_dirty(&kids_handle)?;
    } else {
        node.replace_key(b"/Kids", ObjectHandle::array(new_kid_handles))?;
    } // cov:ignore: all array children come from this Pdf, so ownership failure is invariant-impossible
    pdf.mark_object_dirty(node_ref);

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
    use std::collections::BTreeMap;
    use std::io::Cursor;

    /// Build a flat 3-page PDF:
    ///   1 0 R  Catalog → 2 0 R
    ///   2 0 R  Pages   /Kids [3 4 5] /Count 3
    ///   3 0 R  Page A  /Parent 2 0 R
    ///   4 0 R  Page B  /Parent 2 0 R
    ///   5 0 R  Page C  /Parent 2 0 R
    fn build_flat_pdf() -> Vec<u8> {
        let parts: &[(u32, &str)] = &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (5, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        ];
        build_pdf(parts)
    }

    /// Build a 2-level PDF with 4 pages:
    ///   1 0 R  Catalog
    ///   2 0 R  Pages root  /Kids [3 6] /Count 4
    ///   3 0 R  Pages left  /Kids [4 5] /Count 2  /Parent 2 0 R
    ///   4 0 R  Page A      /Parent 3 0 R
    ///   5 0 R  Page B      /Parent 3 0 R
    ///   6 0 R  Pages right /Kids [7 8] /Count 2  /Parent 2 0 R
    ///   7 0 R  Page C      /Parent 6 0 R
    ///   8 0 R  Page D      /Parent 6 0 R
    fn build_nested_pdf() -> Vec<u8> {
        let parts: &[(u32, &str)] = &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 4 >>"),
            (
                3,
                "<< /Type /Pages /Parent 2 0 R /Kids [4 0 R 5 0 R] /Count 2 >>",
            ),
            (4, "<< /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] >>"),
            (5, "<< /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] >>"),
            (
                6,
                "<< /Type /Pages /Parent 2 0 R /Kids [7 0 R 8 0 R] /Count 2 >>",
            ),
            (7, "<< /Type /Page /Parent 6 0 R /MediaBox [0 0 612 792] >>"),
            (8, "<< /Type /Page /Parent 6 0 R /MediaBox [0 0 612 792] >>"),
        ];
        build_pdf(parts)
    }

    /// Build the same flat tree as [`build_flat_pdf`] with the root's `/Kids`
    /// array and `/Count` stored in indirect objects. qpdf page-tree mutation
    /// resolves these values through the live page-node handle rather than
    /// assuming that either entry is an inline value.
    fn build_indirect_flat_pdf() -> Vec<u8> {
        let parts: &[(u32, &str)] = &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids 9 0 R /Count 10 0 R >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (5, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (9, "[3 0 R 4 0 R 5 0 R]"),
            (10, "3"),
        ];
        build_pdf(parts)
    }

    fn build_catalog_not_dictionary_pdf() -> Vec<u8> {
        build_pdf(&[(1, "[]")])
    }

    fn build_pages_not_dictionary_pdf() -> Vec<u8> {
        build_pdf(&[(1, "<< /Type /Catalog /Pages 2 0 R >>"), (2, "[]")])
    }

    fn build_pages_with_count_pdf(count: &str) -> Vec<u8> {
        build_pdf(&[(1, "<< /Type /Catalog /Pages 2 0 R >>"), (2, count)])
    }

    fn build_pages_with_mismatched_count_pdf() -> Vec<u8> {
        build_pdf(&[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        ])
    }

    fn build_pages_with_direct_kid_pdf() -> Vec<u8> {
        build_pdf(&[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (
                2,
                "<< /Type /Pages /Kids [<< /Type /Page /MediaBox [0 0 612 792] >>] /Count 1 >>",
            ),
        ])
    }

    fn build_empty_pages_without_kids_pdf() -> Vec<u8> {
        build_pdf(&[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Count 0 >>"),
            (3, "<< /Type /Page /MediaBox [0 0 612 792] >>"),
        ])
    }

    fn build_pdf(parts: &[(u32, &str)]) -> Vec<u8> {
        let mut out: Vec<u8> = b"%PDF-1.5\n".to_vec();
        let mut offs: BTreeMap<u32, u64> = BTreeMap::new();
        for (n, s) in parts {
            offs.insert(*n, out.len() as u64);
            out.extend_from_slice(format!("{n} 0 obj\n{s}\nendobj\n").as_bytes());
        }
        let max_obj = parts.iter().map(|(n, _)| n).max().copied().unwrap_or(0);
        let total = max_obj + 1;
        let xref_start = out.len() as u64;
        out.extend_from_slice(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes());
        for i in 1..total {
            out.extend_from_slice(
                format!("{:010} 00000 n \n", offs.get(&i).copied().unwrap_or(0)).as_bytes(),
            );
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        out
    }

    fn open(bytes: Vec<u8>) -> Pdf<Cursor<Vec<u8>>> {
        Pdf::open(Cursor::new(bytes)).expect("PDF should parse")
    }

    fn page_list(pdf: &mut Pdf<Cursor<Vec<u8>>>) -> Vec<ObjectRef> {
        page_refs(pdf).expect("page_refs failed")
    }

    fn dict_of(pdf: &mut Pdf<Cursor<Vec<u8>>>, r: ObjectRef) -> crate::Dictionary {
        pdf.resolve(r)
            .unwrap()
            .into_dict()
            .expect("not a dictionary")
    }

    #[test]
    fn noop_returns_ok_and_does_not_mutate() {
        let mut pdf = open(build_flat_pdf());
        let before = page_list(&mut pdf);
        splice_pages(&mut pdf, 0..0, &[]).unwrap();
        let after = page_list(&mut pdf);
        assert_eq!(before, after);
    }

    #[test]
    fn remove_first_page_flat_tree() {
        let mut pdf = open(build_flat_pdf());
        splice_pages(&mut pdf, 0..1, &[]).unwrap();
        let pages = page_list(&mut pdf);
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0], ObjectRef::new(4, 0)); // B
        assert_eq!(pages[1], ObjectRef::new(5, 0)); // C
                                                    // Root /Pages /Count should be 2.
        let root = dict_of(&mut pdf, ObjectRef::new(2, 0));
        assert_eq!(root.get("Count"), Some(&Object::Integer(2)));
    }

    #[test]
    fn remove_last_page_flat_tree() {
        let mut pdf = open(build_flat_pdf());
        splice_pages(&mut pdf, 2..3, &[]).unwrap();
        let pages = page_list(&mut pdf);
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0], ObjectRef::new(3, 0)); // A
        assert_eq!(pages[1], ObjectRef::new(4, 0)); // B
        let root = dict_of(&mut pdf, ObjectRef::new(2, 0));
        assert_eq!(root.get("Count"), Some(&Object::Integer(2)));
    }

    #[test]
    fn insert_at_start_flat_tree() {
        let mut pdf = open(build_flat_pdf());
        let new_page = ObjectRef::new(6, 0);
        pdf.set_object(
            new_page,
            Object::Dictionary({
                let mut d = crate::Dictionary::new();
                d.insert("Type", Object::Name(b"Page".to_vec()));
                d.insert(
                    "MediaBox",
                    Object::Array(vec![
                        Object::Integer(0),
                        Object::Integer(0),
                        Object::Integer(612),
                        Object::Integer(792),
                    ]),
                );
                d
            }),
        );
        splice_pages(&mut pdf, 0..0, &[new_page]).unwrap();
        let pages = page_list(&mut pdf);
        assert_eq!(pages.len(), 4);
        assert_eq!(pages[0], new_page);
        assert_eq!(pages[1], ObjectRef::new(3, 0));
        // /Parent of new_page must point at root /Pages (2 0 R).
        let d = dict_of(&mut pdf, new_page);
        assert_eq!(d.get_ref("Parent"), Some(ObjectRef::new(2, 0)));
        // /Count = 4
        let root = dict_of(&mut pdf, ObjectRef::new(2, 0));
        assert_eq!(root.get("Count"), Some(&Object::Integer(4)));
    }

    #[test]
    fn insert_at_end_flat_tree() {
        let mut pdf = open(build_flat_pdf());
        let new_page = ObjectRef::new(6, 0);
        pdf.set_object(
            new_page,
            Object::Dictionary({
                let mut d = crate::Dictionary::new();
                d.insert("Type", Object::Name(b"Page".to_vec()));
                d.insert(
                    "MediaBox",
                    Object::Array(vec![
                        Object::Integer(0),
                        Object::Integer(0),
                        Object::Integer(612),
                        Object::Integer(792),
                    ]),
                );
                d
            }),
        );
        splice_pages(&mut pdf, 3..3, &[new_page]).unwrap();
        let pages = page_list(&mut pdf);
        assert_eq!(pages.len(), 4);
        assert_eq!(pages[3], new_page);
        let d = dict_of(&mut pdf, new_page);
        assert_eq!(d.get_ref("Parent"), Some(ObjectRef::new(2, 0)));
    }

    #[test]
    fn insert_in_middle_flat_tree() {
        let mut pdf = open(build_flat_pdf());
        let new_page = ObjectRef::new(6, 0);
        pdf.set_object(
            new_page,
            Object::Dictionary({
                let mut d = crate::Dictionary::new();
                d.insert("Type", Object::Name(b"Page".to_vec()));
                d.insert(
                    "MediaBox",
                    Object::Array(vec![
                        Object::Integer(0),
                        Object::Integer(0),
                        Object::Integer(612),
                        Object::Integer(792),
                    ]),
                );
                d
            }),
        );
        // Insert after page B (between index 1 and 2)
        splice_pages(&mut pdf, 2..2, &[new_page]).unwrap();
        let pages = page_list(&mut pdf);
        assert_eq!(pages.len(), 4);
        assert_eq!(pages[0], ObjectRef::new(3, 0)); // A
        assert_eq!(pages[1], ObjectRef::new(4, 0)); // B
        assert_eq!(pages[2], new_page); // X
        assert_eq!(pages[3], ObjectRef::new(5, 0)); // C
    }

    #[test]
    fn remove_range_flat_tree() {
        let mut pdf = open(build_flat_pdf());
        splice_pages(&mut pdf, 0..2, &[]).unwrap(); // remove A, B
        let pages = page_list(&mut pdf);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0], ObjectRef::new(5, 0)); // C only
        let root = dict_of(&mut pdf, ObjectRef::new(2, 0));
        assert_eq!(root.get("Count"), Some(&Object::Integer(1)));
    }

    #[test]
    fn indirect_kids_and_count_are_mutated_canonically() {
        let mut pdf = open(build_indirect_flat_pdf());
        splice_pages(&mut pdf, 1..2, &[]).unwrap();

        let root = pdf.get_object_handle(ObjectRef::new(2, 0));
        pdf.resolve_object_handle(&root).unwrap();
        let kids_ref = root.try_get_key(b"/Kids").unwrap().object_ref();
        assert_eq!(kids_ref, Some(ObjectRef::new(9, 0)));
        let kids = pdf.get_object_handle(ObjectRef::new(9, 0));
        pdf.resolve_object_handle(&kids).unwrap();
        let kids = kids
            .as_array()
            .expect("indirect /Kids array remains canonical");
        let kids_refs: Vec<_> = kids
            .iter()
            .map(|child| child.object_ref().expect("/Kids entry reference"))
            .collect();
        assert_eq!(kids_refs, vec![ObjectRef::new(3, 0), ObjectRef::new(5, 0)]);
        let count = root.try_get_key(b"/Count").unwrap();
        pdf.resolve_object_handle(&count).unwrap();
        assert_eq!(count.as_integer(), Some(2));
    }

    #[test]
    fn count_mismatch_between_pages_kids_is_rejected() {
        let mut pdf = open(build_pages_with_mismatched_count_pdf());
        let err = splice_pages(&mut pdf, 0..1, &[]).unwrap_err();
        assert!(
            matches!(err, Error::Unsupported(ref message) if message.contains("/Count 2") && message.contains("1")),
            "got {err:?}"
        );
    }

    #[test]
    fn direct_page_kid_is_rejected_by_the_public_walk() {
        let mut pdf = open(build_pages_with_direct_kid_pdf());
        let err = splice_pages(&mut pdf, 0..1, &[]).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)), "got {err:?}");
    }

    #[test]
    fn missing_kids_is_replaced_when_inserting_into_an_empty_tree() {
        let mut pdf = open(build_empty_pages_without_kids_pdf());
        splice_pages(&mut pdf, 0..0, &[ObjectRef::new(3, 0)]).unwrap();
        assert_eq!(page_list(&mut pdf), vec![ObjectRef::new(3, 0)]);
        let root = dict_of(&mut pdf, ObjectRef::new(2, 0));
        assert_eq!(root.get("Count"), Some(&Object::Integer(1)));
        assert_eq!(
            root.get("Kids")
                .and_then(Object::as_array)
                .map(|items| items.len()),
            Some(1)
        );
    }

    #[test]
    fn inserting_an_existing_page_shallow_copies_it() {
        let mut pdf = open(build_flat_pdf());
        let original = ObjectRef::new(3, 0);
        splice_pages(&mut pdf, 0..0, &[original]).unwrap();

        let pages = page_list(&mut pdf);
        assert_eq!(pages.len(), 4);
        assert_ne!(pages[0], original);
        assert_eq!(pages[1], original);
        let copy = dict_of(&mut pdf, pages[0]);
        assert_eq!(copy.get_ref("Parent"), Some(ObjectRef::new(2, 0)));
    }

    #[test]
    fn malformed_catalog_is_rejected_before_page_count() {
        let mut pdf = open(build_catalog_not_dictionary_pdf());
        let err = splice_pages(&mut pdf, 0..1, &[]).unwrap_err();
        assert!(
            matches!(err, Error::Missing("/Catalog dict")),
            "got {err:?}"
        );
    }

    #[test]
    fn non_dictionary_pages_root_is_rejected() {
        let mut pdf = open(build_pages_not_dictionary_pdf());
        let err = splice_pages(&mut pdf, 0..1, &[]).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)), "got {err:?}");
    }

    #[test]
    fn negative_pages_count_is_rejected() {
        let mut pdf = open(build_pages_with_count_pdf(
            "<< /Type /Pages /Kids [] /Count -1 >>",
        ));
        let err = splice_pages(&mut pdf, 0..1, &[]).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)), "got {err:?}");
    }

    #[test]
    fn missing_pages_count_is_rejected() {
        let mut pdf = open(build_pages_with_count_pdf("<< /Type /Pages /Kids [] >>"));
        let err = splice_pages(&mut pdf, 0..1, &[]).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)), "got {err:?}");
    }

    #[test]
    fn splice_subtree_rejects_a_non_dictionary_node() {
        let mut pdf = open(build_pages_not_dictionary_pdf());
        let mut insert_done = false;
        let err = splice_subtree(
            &mut pdf,
            ObjectRef::new(2, 0),
            0,
            &(0..0),
            &[],
            &mut insert_done,
            0,
            DEFAULT_MAX_PAGE_TREE_DEPTH,
        )
        .unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)), "got {err:?}");
    }

    #[test]
    fn splice_subtree_rejects_a_count_mismatch() {
        let mut pdf = open(build_pages_with_mismatched_count_pdf());
        let mut insert_done = false;
        let err = splice_subtree(
            &mut pdf,
            ObjectRef::new(2, 0),
            0,
            &(0..1),
            &[],
            &mut insert_done,
            0,
            DEFAULT_MAX_PAGE_TREE_DEPTH,
        )
        .unwrap_err();
        assert!(
            matches!(err, Error::Unsupported(ref message) if message.contains("/Count 2") && message.contains("1")),
            "got {err:?}"
        );
    }

    #[test]
    fn splice_subtree_rejects_a_direct_page_kid() {
        let mut pdf = open(build_pages_with_direct_kid_pdf());
        let mut insert_done = false;
        let err = splice_subtree(
            &mut pdf,
            ObjectRef::new(2, 0),
            0,
            &(0..1),
            &[],
            &mut insert_done,
            0,
            DEFAULT_MAX_PAGE_TREE_DEPTH,
        )
        .unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)), "got {err:?}");
    }

    #[test]
    fn leaf_count_of_rejects_a_non_dictionary_node() {
        let mut pdf = open(build_pages_not_dictionary_pdf());
        let err = leaf_count_of(&mut pdf, ObjectRef::new(2, 0)).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)), "got {err:?}");
    }

    #[test]
    fn leaf_count_of_rejects_negative_and_missing_counts() {
        let mut negative = open(build_pages_with_count_pdf(
            "<< /Type /Pages /Kids [] /Count -1 >>",
        ));
        let negative_err = leaf_count_of(&mut negative, ObjectRef::new(2, 0)).unwrap_err();
        assert!(matches!(negative_err, Error::Unsupported(_)));

        let mut missing = open(build_pages_with_count_pdf("<< /Type /Pages /Kids [] >>"));
        let missing_err = leaf_count_of(&mut missing, ObjectRef::new(2, 0)).unwrap_err();
        assert!(matches!(missing_err, Error::Unsupported(_)));
    }

    #[test]
    fn non_dictionary_insert_page_is_rejected() {
        let mut pdf = open(build_flat_pdf());
        let bad_page = ObjectRef::new(6, 0);
        pdf.set_object(bad_page, Object::Array(Vec::new()));
        let err = splice_pages(&mut pdf, 0..0, &[bad_page]).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)), "got {err:?}");
    }

    #[test]
    fn replace_middle_page_flat_tree() {
        let mut pdf = open(build_flat_pdf());
        let new_page = ObjectRef::new(6, 0);
        pdf.set_object(
            new_page,
            Object::Dictionary({
                let mut d = crate::Dictionary::new();
                d.insert("Type", Object::Name(b"Page".to_vec()));
                d.insert(
                    "MediaBox",
                    Object::Array(vec![
                        Object::Integer(0),
                        Object::Integer(0),
                        Object::Integer(612),
                        Object::Integer(792),
                    ]),
                );
                d
            }),
        );
        // Replace page B (index 1) with new_page.
        splice_pages(&mut pdf, 1..2, &[new_page]).unwrap();
        let pages = page_list(&mut pdf);
        assert_eq!(pages.len(), 3);
        assert_eq!(pages[0], ObjectRef::new(3, 0)); // A
        assert_eq!(pages[1], new_page); // X
        assert_eq!(pages[2], ObjectRef::new(5, 0)); // C
                                                    // Count stays 3.
        let root = dict_of(&mut pdf, ObjectRef::new(2, 0));
        assert_eq!(root.get("Count"), Some(&Object::Integer(3)));
    }

    /// Remove page B (index 1, in left subtree) from the nested tree.
    /// CRITICAL: intermediate nodes (3 0 R left, 6 0 R right) must STILL EXIST
    /// with their /Count updated. This is the key difference from rebuild_page_tree.
    #[test]
    fn nested_remove_updates_intermediate_count() {
        let mut pdf = open(build_nested_pdf());
        splice_pages(&mut pdf, 1..2, &[]).unwrap(); // remove B
                                                    // Page order: A C D
        let pages = page_list(&mut pdf);
        assert_eq!(pages.len(), 3);
        assert_eq!(pages[0], ObjectRef::new(4, 0)); // A
        assert_eq!(pages[1], ObjectRef::new(7, 0)); // C
        assert_eq!(pages[2], ObjectRef::new(8, 0)); // D
                                                    // Root /Count = 3
        let root = dict_of(&mut pdf, ObjectRef::new(2, 0));
        assert_eq!(root.get("Count"), Some(&Object::Integer(3)));
        // Left intermediate node /Count = 1 (only A remains)
        let left = dict_of(&mut pdf, ObjectRef::new(3, 0));
        assert_eq!(left.get("Count"), Some(&Object::Integer(1)));
        // Right intermediate node /Count = 2 (unchanged)
        let right = dict_of(&mut pdf, ObjectRef::new(6, 0));
        assert_eq!(right.get("Count"), Some(&Object::Integer(2)));
    }

    /// Remove pages B and C (indices 1 and 2), which span both left and right subtrees.
    #[test]
    fn nested_remove_spanning_subtrees() {
        let mut pdf = open(build_nested_pdf());
        splice_pages(&mut pdf, 1..3, &[]).unwrap(); // remove B (left) and C (right)
        let pages = page_list(&mut pdf);
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0], ObjectRef::new(4, 0)); // A
        assert_eq!(pages[1], ObjectRef::new(8, 0)); // D
        let root = dict_of(&mut pdf, ObjectRef::new(2, 0));
        assert_eq!(root.get("Count"), Some(&Object::Integer(2)));
        // Left subtree: only A remains → /Count = 1
        let left = dict_of(&mut pdf, ObjectRef::new(3, 0));
        assert_eq!(left.get("Count"), Some(&Object::Integer(1)));
        // Right subtree: only D remains → /Count = 1
        let right = dict_of(&mut pdf, ObjectRef::new(6, 0));
        assert_eq!(right.get("Count"), Some(&Object::Integer(1)));
    }

    #[test]
    fn depth_limit_is_preserved_for_nested_page_trees() {
        let mut pdf = open(build_nested_pdf());
        let err = splice_pages_with_max_depth(&mut pdf, 0..1, &[], 1).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)), "got {err:?}");
    }

    #[test]
    fn error_remove_end_out_of_bounds() {
        let mut pdf = open(build_flat_pdf()); // 3 pages
        let err = splice_pages(&mut pdf, 0..4, &[]).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)), "got {err:?}");
    }

    #[test]
    fn error_empty_result_document() {
        let mut pdf = open(build_flat_pdf()); // 3 pages
        let err = splice_pages(&mut pdf, 0..3, &[]).unwrap_err();
        assert!(matches!(err, Error::Missing(_)), "got {err:?}");
    }

    /// Remove all pages in the left subtree (A and B, indices 0..2).
    /// The now-empty left intermediate node must be dropped from root /Kids.
    #[test]
    fn empty_intermediate_node_is_dropped() {
        let mut pdf = open(build_nested_pdf()); // A B C D
        splice_pages(&mut pdf, 0..2, &[]).unwrap(); // remove A, B
        let pages = page_list(&mut pdf);
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0], ObjectRef::new(7, 0)); // C
        assert_eq!(pages[1], ObjectRef::new(8, 0)); // D
        let root = dict_of(&mut pdf, ObjectRef::new(2, 0));
        // Root /Kids should only contain right subtree (6 0 R).
        let kids = root.get("Kids").and_then(Object::as_array).unwrap();
        assert_eq!(kids.len(), 1);
        assert_eq!(kids[0].as_ref_id(), Some(ObjectRef::new(6, 0)));
        assert_eq!(root.get("Count"), Some(&Object::Integer(2)));
    }

    /// Insert a new page at index 2 (between B and C, at the boundary of left and right subtrees).
    /// The new page should be inserted into the right subtree (as its first kid).
    #[test]
    fn nested_insert_at_subtree_boundary() {
        let mut pdf = open(build_nested_pdf());
        let new_page = ObjectRef::new(9, 0);
        pdf.set_object(
            new_page,
            Object::Dictionary({
                let mut d = crate::Dictionary::new();
                d.insert("Type", Object::Name(b"Page".to_vec()));
                d.insert(
                    "MediaBox",
                    Object::Array(vec![
                        Object::Integer(0),
                        Object::Integer(0),
                        Object::Integer(612),
                        Object::Integer(792),
                    ]),
                );
                d
            }),
        );
        splice_pages(&mut pdf, 2..2, &[new_page]).unwrap();
        let pages = page_list(&mut pdf);
        assert_eq!(pages.len(), 5);
        assert_eq!(pages[0], ObjectRef::new(4, 0)); // A
        assert_eq!(pages[1], ObjectRef::new(5, 0)); // B
        assert_eq!(pages[2], new_page); // X
        assert_eq!(pages[3], ObjectRef::new(7, 0)); // C
        assert_eq!(pages[4], ObjectRef::new(8, 0)); // D
                                                    // Root /Count = 5
        let root = dict_of(&mut pdf, ObjectRef::new(2, 0));
        assert_eq!(root.get("Count"), Some(&Object::Integer(5)));
        // new_page's /Parent should point at an ancestor /Pages node.
        let d = dict_of(&mut pdf, new_page);
        let parent = d.get_ref("Parent").expect("/Parent must be set");
        // Parent must be a /Pages node in the tree
        let parent_dict = dict_of(&mut pdf, parent);
        assert_eq!(
            parent_dict.get("Type").and_then(Object::as_name),
            Some(b"Pages".as_ref())
        );
    }
}
