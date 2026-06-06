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
/// This function is part of the document page extraction and merge primitives
/// epic (flpdf-5h5). The `insert` refs it splices in are typically pages copied
/// from another document with
/// [`copy_objects`](crate::object_copy::copy_objects), whose object set is first
/// computed per page by
/// [`page_object_closure`](crate::page_closure::page_object_closure). See the
/// runnable `examples/splice_pages.rs` and `examples/merge_pdfs.rs`.
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

    let page_count = page_refs_with_max_depth(pdf, max_depth)?.len();

    if remove.end > page_count {
        return Err(Error::Unsupported(format!(
            "splice: remove.end {} exceeds page count {}",
            remove.end, page_count
        )));
    }

    let remaining = page_count - remove.len() + insert.len();
    if remaining == 0 {
        return Err(Error::Missing("splice would result in an empty document"));
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
            Some(n) if n >= 0 => Ok(n as usize),
            Some(n) => Err(Error::Unsupported(format!(
                "/Pages node {node_ref} has negative /Count {n}"
            ))),
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
            .map(|arr| arr.iter().filter_map(Object::as_ref_id).collect())
            .unwrap_or_default();

        let old_count_raw = dict
            .get("Count")
            .and_then(Object::as_integer)
            .ok_or_else(|| Error::Unsupported(format!("/Pages node {node_ref} has no /Count")))?;
        if old_count_raw < 0 {
            return Err(Error::Unsupported(format!(
                "/Pages node {node_ref} has negative /Count {old_count_raw}"
            )));
        }
        let old_count = old_count_raw as usize;

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
    let new_count = old_count as i64 + net_delta;
    if new_count < 0 {
        return Err(Error::Unsupported(format!(
            "splice: negative page count {new_count} for node {node_ref}"
        )));
    }
    let mut dict = pdf.resolve(node_ref)?.into_dict().ok_or_else(|| {
        Error::Unsupported(format!("{node_ref} is not a dictionary (re-resolve)"))
    })?;
    dict.insert("Count", Object::Integer(new_count));
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
