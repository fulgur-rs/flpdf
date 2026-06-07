//! Page-tree rebuild after extraction / merge / rotate (flpdf-9hc.8.8).
//!
//! Given an open [`Pdf`] and an ordered list of selected leaf `/Page`
//! `ObjectRef`s (the output of [`crate::page_plan::PagePlan`] /
//! [`crate::page_combine::CombinedPlan`] for a **single** document), this
//! module rebuilds the document's `/Pages` tree so that:
//!
//! - The root `/Pages` node's `/Kids` enumerates exactly the selected pages,
//!   in selection order.
//! - `/Count` equals the selection length.
//! - Inheritable attributes (`/Resources`, `/MediaBox`, `/CropBox`,
//!   `/Rotate`) that were inherited from an ancestor `/Pages` node are
//!   *materialized* (written explicitly) onto each leaf **before** the leaf is
//!   reparented, so the leaf no longer depends on the old ancestor chain.
//! - Every leaf's `/Parent` is repointed at the (stable) root `/Pages` object.
//!
//! The result is a **flat** qpdf-style page tree: no intermediate `/Pages`
//! nodes are created; the root `/Pages` object's `ObjectRef` is preserved so
//! the catalog's `/Pages` reference does not need patching.
//!
//! # qpdf 11.9.0 observed behaviour (truth source `/usr/bin/qpdf`)
//!
//! Built a 3-page fixture with an intermediate `/Pages` node carrying
//! `/Rotate 90`, `/MediaBox [0 0 200 300]`, `/Resources 9 0 R`; page 2 had
//! its own `/MediaBox [0 0 400 500]`.
//!
//! - `qpdf in.pdf --pages in.pdf 1,3 -- out.pdf`: output has a single
//!   `/Pages` node whose `/Kids` directly lists the two leaves; each leaf
//!   gained explicit `/Rotate 90` and `/MediaBox` (the inherited value);
//!   the intermediate node is gone.
//! - `qpdf in.pdf --pages in.pdf 2 -- out.pdf`: page 2 **kept** its own
//!   `/MediaBox [0 0 400 500]` (own attribute wins) while still gaining the
//!   inherited `/Rotate 90` and `/Resources` it lacked.
//! - `qpdf in.pdf --pages in.pdf 1,1 -- out.pdf`: `/Count` = 2 and `/Kids`
//!   holds **two distinct page-dictionary objects**, each a copy of page 1,
//!   while the shared `/Contents` stream object is referenced by both. So a
//!   duplicate selection slot yields a *separate page dictionary* but shared
//!   sub-objects (content streams, resources).
//!
//! This module reproduces that observable result while *mutating in place*
//! rather than writing a fresh renumbered document:
//!
//! - The **first** occurrence of a source page mutates the existing leaf
//!   (materialize inherited attrs + reparent); its `ObjectRef` is unchanged.
//! - **Subsequent** occurrences of the same source page allocate a fresh
//!   object number and deep-clone the *post-materialization* page dictionary,
//!   then reparent that clone. Referenced sub-objects (e.g. `/Contents`,
//!   `/Resources` indirect refs) are left shared, exactly as qpdf does.
//!
//! # Scope (single document only)
//!
//! This layer operates on **one** [`Pdf`]. Rebuilding across multiple input
//! documents ([`crate::page_combine::CombinedPlan`] with >1 input) additionally
//! requires cross-document object copying (renumbering, encryption-boundary
//! handling, name-conflict resolution) and is a separate future layer. The
//! single-input CLI wiring (8.12), outline/dest remap (8.10), and AcroForm
//! handling (8.11) all operate over a single document and can build on the
//! [`RebuildResult`] returned here.
//!
//! Obsolete intermediate `/Pages` nodes are intentionally left as orphan
//! objects (unreachable from the page tree) for the unreferenced-resource
//! pruning layer (8.9) to remove, mirroring the precedent set by
//! [`crate::page_split`]. They do not affect output validity.

use crate::page_rotate::resolve_inherited_rotate_with_max_depth;
use crate::pages::{resolve_inherited_resources_with_max_depth, DEFAULT_MAX_PAGE_TREE_DEPTH};
use crate::{Error, Object, ObjectRef, Pdf, Result};
use std::collections::BTreeMap;
use std::io::{Read, Seek};

// ---------------------------------------------------------------------------
// Public result type
// ---------------------------------------------------------------------------

/// Outcome of a [`rebuild_page_tree`] call.
///
/// `new_kids` is the rebuilt `/Pages` `/Kids` array in selection order; its
/// length always equals the selection length (duplicates included).
///
/// `ref_map` maps each *source* page `ObjectRef` to **all** new leaf
/// `ObjectRef`s produced from it, in selection order. Duplicate selections
/// therefore appear as a multi-element `Vec`. Downstream layers use this:
///
/// - **8.10** (outline / named-destination remap): given an old page target,
///   look up `ref_map[old]` and remap to the first element (qpdf-equivalent:
///   destinations resolve to the first occurrence of a duplicated page).
/// - **8.11** (AcroForm): widget `/P` back-pointers follow the same rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebuildResult {
    /// The rebuilt root `/Pages` `/Kids`, in selection order.
    pub new_kids: Vec<ObjectRef>,
    /// Source page ref → every new leaf ref derived from it (selection order).
    pub ref_map: BTreeMap<ObjectRef, Vec<ObjectRef>>,
}

// ---------------------------------------------------------------------------
// Inherited-attribute resolution helpers
// ---------------------------------------------------------------------------

/// Resolve an inheritable attribute `key` for `page_ref` by walking the
/// `/Parent` chain, returning the **raw** [`Object`] of the first node that
/// carries the key (so malformed values round-trip unchanged).
///
/// Per ISO 32000-1 §7.3.9 a `null` value is equivalent to the key being
/// absent, so it falls through to the parent. A `null` (or absent) `/Parent`
/// terminates the walk. Cycles and over-deep trees are bounded the same way
/// as [`crate::page_rotate::resolve_inherited_rotate`] /
/// [`crate::pages::resolve_inherited_resources`].
///
/// Returns `Ok(None)` when no node in the chain carries the key.
pub(crate) fn resolve_inherited_raw<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    key: &str,
    max_depth: usize,
) -> Result<Option<Object>> {
    use std::collections::BTreeSet;

    let mut seen: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut current = page_ref;
    let mut depth: usize = 0;

    loop {
        if depth >= max_depth {
            return Err(Error::Unsupported(format!(
                "page tree depth exceeds maximum of {max_depth} at {current}"
            )));
        }
        if !seen.insert(current) {
            // Cycle — treat the attribute as absent.
            return Ok(None);
        }

        let Object::Dictionary(dict) = pdf.resolve_borrowed(current)? else {
            return Ok(None);
        };

        if let Some(val) = dict.get(key).cloned() {
            match val {
                // null == absent (§7.3.9): keep walking.
                Object::Null => {}
                // An indirect reference to the value is kept as-is: the leaf
                // can legitimately share the same indirect object (qpdf does
                // this for /MediaBox in the observed 1,3 case).
                other => return Ok(Some(other)),
            }
        }

        let parent_val = match dict.get("Parent").cloned() {
            Some(Object::Null) | None => return Ok(None),
            Some(v) => v,
        };
        match parent_val {
            Object::Reference(r) => {
                current = r;
                depth += 1;
            }
            // Non-reference /Parent is non-standard; treat as chain end.
            _ => return Ok(None),
        }
    }
}

/// `true` when `dict` carries `key` as something other than `null`
/// (ISO 32000-1 §7.3.9: an explicit `null` is equivalent to absent).
fn leaf_has_own(dict: &crate::Dictionary, key: &str) -> bool {
    !matches!(dict.get(key), None | Some(Object::Null))
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

/// Rebuild the document's `/Pages` tree from `selected` leaf page refs.
///
/// `selected` is the ordered list of source `/Page` `ObjectRef`s (from
/// [`crate::page_plan::PagePlan::pages`] / a single-input
/// [`crate::page_combine::CombinedPlan`]). Duplicate refs are permitted and
/// produce duplicate output pages, matching qpdf.
///
/// On success the in-memory document is mutated so that the (unchanged) root
/// `/Pages` object lists exactly the selected pages, each with inheritable
/// attributes materialized and `/Parent` repointed at the root. Serialize the
/// result with [`crate::write_pdf`].
///
/// This function is part of the document page extraction and merge primitives
/// epic (flpdf-5h5). The `selected` refs it consumes are produced by
/// [`PagePlan`](crate::page_plan::PagePlan) (single document) or a single-input
/// [`CombinedPlan`](crate::page_combine::CombinedPlan). For an end-to-end
/// extraction walkthrough see the runnable `examples/extract_pages.rs`.
///
/// # Errors
///
/// - [`Error::Missing`] when `/Root` or the catalog `/Pages` reference is
///   absent, or `selected` is empty.
/// - [`Error::Unsupported`] when the catalog / a selected ref is not a
///   dictionary, the page-tree depth limit is exceeded, or an object-number
///   overflow occurs while allocating clones for duplicate selections.
/// - Any error propagated from [`Pdf::resolve`].
pub fn rebuild_page_tree<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    selected: &[ObjectRef],
) -> Result<RebuildResult> {
    rebuild_page_tree_with_max_depth(pdf, selected, DEFAULT_MAX_PAGE_TREE_DEPTH)
}

/// Like [`rebuild_page_tree`] but with a caller-supplied inheritance-walk
/// recursion limit.
pub fn rebuild_page_tree_with_max_depth<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    selected: &[ObjectRef],
    max_depth: usize,
) -> Result<RebuildResult> {
    if selected.is_empty() {
        return Err(Error::Missing("page-tree rebuild: empty selection"));
    }

    // Locate the root /Pages object; keep its ObjectRef stable so the
    // catalog's /Pages reference never needs patching.
    let catalog_ref = pdf.root_ref().ok_or(Error::Missing("/Root"))?;
    let Object::Dictionary(catalog) = pdf.resolve_borrowed(catalog_ref)? else {
        return Err(Error::Unsupported(format!(
            "document catalog {catalog_ref} is not a dictionary"
        )));
    };
    let pages_root_ref = catalog.get_ref("Pages").ok_or(Error::Missing("/Pages"))?;

    // Next free object number, for cloning duplicate-selection leaves.
    let mut next_num: u32 = pdf
        .object_refs()
        .iter()
        .map(|r| r.number)
        .max()
        .unwrap_or(0);

    let mut new_kids: Vec<ObjectRef> = Vec::with_capacity(selected.len());
    let mut ref_map: BTreeMap<ObjectRef, Vec<ObjectRef>> = BTreeMap::new();
    // Tracks whether a given source ref has already consumed its in-place slot.
    let mut materialized: BTreeMap<ObjectRef, ()> = BTreeMap::new();

    for &src in selected {
        // ── Resolve every inheritable attribute via the ORIGINAL parent
        //    chain, BEFORE any reparent severs it. /Rotate and /Resources
        //    reuse the existing dedicated resolvers; the rest use the raw
        //    walk so malformed values round-trip unchanged.
        let inherited_resources = resolve_inherited_resources_with_max_depth(pdf, src, max_depth)?;
        let inherited_rotate = resolve_inherited_rotate_with_max_depth(pdf, src, max_depth)?;
        let inherited_mediabox = resolve_inherited_raw(pdf, src, "MediaBox", max_depth)?;
        let inherited_cropbox = resolve_inherited_raw(pdf, src, "CropBox", max_depth)?;

        // Fetch the leaf dictionary.
        let Object::Dictionary(mut leaf) = pdf.resolve(src)? else {
            return Err(Error::Unsupported(format!(
                "selected object {src} is not a dictionary (expected /Page)"
            )));
        };

        // Only leaf /Page dictionaries are valid inputs here. A /Pages tree
        // node, a typeless dict, or any non-/Page dict would produce a broken
        // page tree (e.g. a self-referential /Kids), so fail closed: require
        // an explicit /Type /Page. Legitimate selections always carry it —
        // `pages::page_refs` only enumerates nodes whose /Type is /Page.
        if !matches!(leaf.get("Type"), Some(Object::Name(name)) if name == b"Page") {
            return Err(Error::Unsupported(format!(
                "selected object {src} is not a /Page dictionary"
            )));
        }

        // Materialize each inherited attribute ONLY when the leaf lacks its
        // own (own attribute wins — verified against qpdf for /MediaBox).
        if !leaf_has_own(&leaf, "Resources") {
            if let Some(res) = &inherited_resources {
                leaf.insert("Resources", Object::Dictionary(res.clone()));
            }
        }
        if !leaf_has_own(&leaf, "MediaBox") {
            if let Some(mb) = &inherited_mediabox {
                leaf.insert("MediaBox", mb.clone());
            }
        }
        if !leaf_has_own(&leaf, "CropBox") {
            // ISO 32000-1 §14.11.2: /CropBox defaults to /MediaBox. We do NOT
            // synthesize that default — only materialize a /CropBox that was
            // explicitly present somewhere in the ancestor chain.
            if let Some(cb) = &inherited_cropbox {
                leaf.insert("CropBox", cb.clone());
            }
        }
        // /Rotate is always materialized explicitly (matching page_rotate's
        // policy and qpdf's observed output: every leaf carries /Rotate),
        // unless the leaf already has its own non-null value.
        if !leaf_has_own(&leaf, "Rotate") {
            leaf.insert("Rotate", Object::Integer(inherited_rotate as i64));
        }

        // Reparent to the (stable) root /Pages object.
        leaf.insert("Parent", Object::Reference(pages_root_ref));

        let target_ref = if materialized.insert(src, ()).is_none() {
            // First occurrence: mutate the existing object in place.
            pdf.set_object(src, Object::Dictionary(leaf));
            src
        } else {
            // Duplicate occurrence: allocate a fresh object holding a clone of
            // the post-materialization dictionary. Sub-objects referenced
            // indirectly (/Contents, /Resources, …) stay shared, matching
            // qpdf's observed 1,1 behaviour.
            next_num = next_num.checked_add(1).ok_or_else(|| {
                Error::Unsupported(
                    "page-tree rebuild: object-number overflow allocating duplicate page"
                        .to_string(),
                )
            })?;
            let clone_ref = ObjectRef::new(next_num, 0);
            pdf.set_object(clone_ref, Object::Dictionary(leaf));
            clone_ref
        };

        new_kids.push(target_ref);
        ref_map.entry(src).or_default().push(target_ref);
    }

    // Rewrite the root /Pages node: flat /Kids in selection order, /Count =
    // selection length, /Type /Pages. Other root entries are preserved.
    let Object::Dictionary(mut root_dict) = pdf.resolve_borrowed(pages_root_ref)?.clone() else {
        return Err(Error::Unsupported(format!(
            "document /Pages root {pages_root_ref} is not a dictionary"
        )));
    };
    root_dict.insert("Type", Object::Name(b"Pages".to_vec()));
    root_dict.insert(
        "Kids",
        Object::Array(new_kids.iter().map(|&r| Object::Reference(r)).collect()),
    );
    root_dict.insert("Count", Object::Integer(new_kids.len() as i64));
    // A root /Pages must not carry /Parent; drop any stale one so the rebuilt
    // tree is well-formed even if the original root was itself an interior
    // node of a degenerate tree.
    root_dict.remove("Parent");
    pdf.set_object(pages_root_ref, Object::Dictionary(root_dict));

    Ok(RebuildResult { new_kids, ref_map })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::check_reader;
    use crate::pages::page_refs;
    use crate::writer::write_pdf;
    use crate::Pdf;
    use std::io::Cursor;

    /// Build a PDF with a root `/Pages` (2 0 R) → intermediate `/Pages`
    /// (3 0 R, carrying `/Rotate 90`, `/MediaBox [0 0 200 300]`,
    /// `/Resources 9 0 R`) → three leaves:
    ///   4 0 R  Page (no own attrs, /Contents 7 0 R)
    ///   5 0 R  Page (own /MediaBox [0 0 400 500], /Contents 8 0 R)
    ///   6 0 R  Page (no own attrs, no contents)
    /// plus 7,8 content streams and 9 the shared Resources dict.
    fn build_nested_pdf() -> Vec<u8> {
        let parts: Vec<(u32, String)> = vec![
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 3 >>".into()),
            (
                3,
                "<< /Type /Pages /Parent 2 0 R /Kids [4 0 R 5 0 R 6 0 R] /Count 3 \
                 /Rotate 90 /MediaBox [0 0 200 300] /Resources 9 0 R >>"
                    .into(),
            ),
            (4, "<< /Type /Page /Parent 3 0 R /Contents 7 0 R >>".into()),
            (
                5,
                "<< /Type /Page /Parent 3 0 R /Contents 8 0 R /MediaBox [0 0 400 500] >>".into(),
            ),
            (6, "<< /Type /Page /Parent 3 0 R >>".into()),
        ];
        let c1 = b"BT /F1 12 Tf 10 10 Td (Page1) Tj ET";
        let c2 = b"BT /F1 12 Tf 10 10 Td (Page2) Tj ET";

        let mut out: Vec<u8> = b"%PDF-1.5\n".to_vec();
        let mut offs: BTreeMap<u32, u64> = BTreeMap::new();
        for (n, s) in &parts {
            offs.insert(*n, out.len() as u64);
            out.extend_from_slice(format!("{n} 0 obj\n{s}\nendobj\n").as_bytes());
        }
        offs.insert(7, out.len() as u64);
        out.extend_from_slice(format!("7 0 obj\n<< /Length {} >>\nstream\n", c1.len()).as_bytes());
        out.extend_from_slice(c1);
        out.extend_from_slice(b"\nendstream\nendobj\n");
        offs.insert(8, out.len() as u64);
        out.extend_from_slice(format!("8 0 obj\n<< /Length {} >>\nstream\n", c2.len()).as_bytes());
        out.extend_from_slice(c2);
        out.extend_from_slice(b"\nendstream\nendobj\n");
        offs.insert(9, out.len() as u64);
        out.extend_from_slice(
            b"9 0 obj\n<< /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >>\nendobj\n",
        );

        let xref_start = out.len() as u64;
        let total = 10u32;
        out.extend_from_slice(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes());
        for i in 1..total {
            out.extend_from_slice(format!("{:010} 00000 n \n", offs[&i]).as_bytes());
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

    fn dict_of(pdf: &mut Pdf<Cursor<Vec<u8>>>, r: ObjectRef) -> crate::Dictionary {
        match pdf.resolve_borrowed(r).unwrap() {
            Object::Dictionary(d) => d.clone(),
            other => panic!("{r} is not a dictionary: {other:?}"),
        }
    }

    #[test]
    fn empty_selection_is_error() {
        let mut pdf = open(build_nested_pdf());
        let err = rebuild_page_tree(&mut pdf, &[]).unwrap_err();
        assert!(matches!(err, Error::Missing(_)), "got {err:?}");
    }

    #[test]
    fn selecting_a_pages_node_is_rejected() {
        // 2 0 R is the root /Pages node, not a leaf /Page. Passing it must
        // error rather than build a self-referential page tree.
        let mut pdf = open(build_nested_pdf());
        let err = rebuild_page_tree(&mut pdf, &[ObjectRef::new(2, 0)]).unwrap_err();
        assert!(
            matches!(err, Error::Unsupported(_)),
            "expected Unsupported for /Pages node, got {err:?}"
        );
    }

    #[test]
    fn count_and_kids_match_selection_order() {
        // Select pages 3,1 (objects 6 then 4) — descending / out of order.
        let mut pdf = open(build_nested_pdf());
        let sel = [ObjectRef::new(6, 0), ObjectRef::new(4, 0)];
        let res = rebuild_page_tree(&mut pdf, &sel).unwrap();

        assert_eq!(
            res.new_kids,
            vec![ObjectRef::new(6, 0), ObjectRef::new(4, 0)]
        );

        let root = dict_of(&mut pdf, ObjectRef::new(2, 0));
        assert_eq!(root.get("Count"), Some(&Object::Integer(2)));
        let Some(Object::Array(kids)) = root.get("Kids") else {
            panic!("root /Kids missing or wrong type");
        };
        assert_eq!(
            kids,
            &vec![
                Object::Reference(ObjectRef::new(6, 0)),
                Object::Reference(ObjectRef::new(4, 0)),
            ]
        );
    }

    #[test]
    fn inherited_attrs_materialized_on_leaf() {
        // Page 1 (obj 4) inherits everything from intermediate node 3.
        let mut pdf = open(build_nested_pdf());
        rebuild_page_tree(&mut pdf, &[ObjectRef::new(4, 0)]).unwrap();

        let leaf = dict_of(&mut pdf, ObjectRef::new(4, 0));
        assert_eq!(leaf.get("Rotate"), Some(&Object::Integer(90)));
        assert_eq!(
            leaf.get("MediaBox"),
            Some(&Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(200),
                Object::Integer(300),
            ]))
        );
        // /Resources inherited via indirect ref 9 0 R → materialized as the
        // resolved dictionary (qpdf-equivalent).
        match leaf.get("Resources") {
            Some(Object::Dictionary(d)) => assert!(d.get("Font").is_some()),
            other => panic!("expected materialized /Resources dict, got {other:?}"),
        }
        // Reparented to root.
        assert_eq!(
            leaf.get("Parent"),
            Some(&Object::Reference(ObjectRef::new(2, 0)))
        );
    }

    #[test]
    fn own_attribute_wins_over_inherited() {
        // Page 2 (obj 5) has its own /MediaBox; it must be preserved while the
        // inherited /Rotate is still materialized.
        let mut pdf = open(build_nested_pdf());
        rebuild_page_tree(&mut pdf, &[ObjectRef::new(5, 0)]).unwrap();

        let leaf = dict_of(&mut pdf, ObjectRef::new(5, 0));
        assert_eq!(
            leaf.get("MediaBox"),
            Some(&Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(400),
                Object::Integer(500),
            ])),
            "own /MediaBox must win over inherited [0 0 200 300]"
        );
        assert_eq!(leaf.get("Rotate"), Some(&Object::Integer(90)));
    }

    #[test]
    fn duplicate_selection_yields_distinct_page_dicts_sharing_contents() {
        // Select page 1 twice (obj 4, 4). qpdf: /Count 2, two distinct page
        // dicts, shared /Contents stream object.
        let mut pdf = open(build_nested_pdf());
        let res =
            rebuild_page_tree(&mut pdf, &[ObjectRef::new(4, 0), ObjectRef::new(4, 0)]).unwrap();

        assert_eq!(res.new_kids.len(), 2);
        assert_eq!(res.new_kids[0], ObjectRef::new(4, 0)); // first keeps original
        let clone_ref = res.new_kids[1];
        assert_ne!(clone_ref, ObjectRef::new(4, 0), "second slot is a clone");

        // ref_map records both new refs under the source.
        assert_eq!(
            res.ref_map.get(&ObjectRef::new(4, 0)),
            Some(&vec![ObjectRef::new(4, 0), clone_ref])
        );

        let original = dict_of(&mut pdf, ObjectRef::new(4, 0));
        let clone = dict_of(&mut pdf, clone_ref);
        // Distinct objects but identical materialized content; /Contents
        // stream object is shared (same indirect ref), not duplicated.
        assert_eq!(original.get("Contents"), clone.get("Contents"));
        assert_eq!(
            original.get("Contents"),
            Some(&Object::Reference(ObjectRef::new(7, 0)))
        );
        assert_eq!(clone.get("Rotate"), Some(&Object::Integer(90)));

        let root = dict_of(&mut pdf, ObjectRef::new(2, 0));
        assert_eq!(root.get("Count"), Some(&Object::Integer(2)));
    }

    #[test]
    fn subset_round_trips_to_valid_pdf() {
        // Extract pages 1 and 3; write, reopen, and verify structure + check.
        let mut pdf = open(build_nested_pdf());
        rebuild_page_tree(&mut pdf, &[ObjectRef::new(4, 0), ObjectRef::new(6, 0)]).unwrap();

        let mut out: Vec<u8> = Vec::new();
        write_pdf(&mut pdf, &mut out).unwrap();

        let mut pdf2 = Pdf::open(Cursor::new(out.clone())).expect("rebuilt PDF should parse");
        let refs = page_refs(&mut pdf2).expect("page tree should walk");
        assert_eq!(refs.len(), 2, "/Pages should enumerate exactly 2 leaves");
        assert_eq!(refs, vec![ObjectRef::new(4, 0), ObjectRef::new(6, 0)]);

        // Each leaf must carry the materialized inherited attrs after reopen.
        let leaf = dict_of(&mut pdf2, ObjectRef::new(6, 0));
        assert_eq!(leaf.get("Rotate"), Some(&Object::Integer(90)));
        assert!(leaf.get("MediaBox").is_some());

        // Belt-and-suspenders: the crate's own validity check is clean.
        let report = check_reader(Cursor::new(out)).expect("check should run");
        assert!(
            report.valid,
            "rebuilt subset PDF should pass check_reader: {:?}",
            report.diagnostics
        );
    }

    #[test]
    fn duplicate_round_trips_with_correct_page_count() {
        let mut pdf = open(build_nested_pdf());
        rebuild_page_tree(
            &mut pdf,
            &[
                ObjectRef::new(4, 0),
                ObjectRef::new(4, 0),
                ObjectRef::new(5, 0),
            ],
        )
        .unwrap();

        let mut out: Vec<u8> = Vec::new();
        write_pdf(&mut pdf, &mut out).unwrap();

        let mut pdf2 = Pdf::open(Cursor::new(out)).expect("should parse");
        let refs = page_refs(&mut pdf2).expect("walk");
        assert_eq!(refs.len(), 3, "duplicate selection → 3 enumerated pages");
    }
}
