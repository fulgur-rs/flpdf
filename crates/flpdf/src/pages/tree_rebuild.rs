//! qpdf correspondence: QPDF_pages.cc page-tree repair plus QPDFJob.cc page-selection rebuilding.
//! Page-tree rebuild after extraction / merge / rotate.
//!
//! Given an open [`Pdf`] and an ordered list of selected leaf `/Page`
//! `ObjectRef`s (the output of [`crate::PagePlan`] /
//! [`crate::CombinedPlan`] for a **single** document), this
//! module rebuilds the document's `/Pages` tree so that:
//!
//! - The root `/Pages` node's `/Kids` enumerates exactly the selected pages,
//!   in selection order.
//! - `/Count` equals the selection length.
//! - Inheritable attributes (`/Resources`, `/MediaBox`, `/CropBox`,
//!   `/Rotate`) that were inherited from an ancestor `/Pages` node are pushed
//!   onto each leaf as live handles **before** the leaf is reparented, so the
//!   leaf no longer depends on the old ancestor chain. Direct non-scalar values
//!   are promoted once through the canonical object registry; scalar values and
//!   existing indirect values retain qpdf's copy/identity behavior.
//! - After that push, the retained root no longer carries those four inheritable
//!   keys, matching `QPDF_optimization.cc:159-228`; its page-tree mutation keeps
//!   `/Type`, `/Kids`, and `/Count` as the structural keys.
//! - Every leaf's `/Parent` is repointed at the stable root `/Pages` value.
//!
//! The result is a **flat** qpdf-style page tree: no intermediate `/Pages`
//! nodes are created. An indirect root preserves its `ObjectRef`; a direct
//! root remains embedded in the catalog, matching qpdf's object-handle
//! ownership.
//!
//! # qpdf 11.9.0 observed behaviour (truth source `/usr/bin/qpdf`)
//!
//! Built a 3-page fixture with an intermediate `/Pages` node carrying
//! `/Rotate 90`, `/MediaBox [0 0 200 300]`, `/Resources 9 0 R`; page 2 had
//! its own `/MediaBox [0 0 400 500]`.
//!
//! - `qpdf in.pdf --pages in.pdf 1,3 -- out.pdf`: output has a single
//!   `/Pages` node whose `/Kids` directly lists the two leaves; each leaf
//!   gained explicit `/Rotate 90` and `/MediaBox` (the inherited value), with
//!   direct non-scalar values promoted to indirect objects; the intermediate
//!   node is gone from the reachable output.
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
//!   object number and shallow-copy the *post-push* page dictionary, then
//!   reparent that clone. Referenced sub-objects (e.g. `/Contents`,
//!   `/Resources` indirect refs) are left shared, exactly as qpdf does.
//!
//! # Scope (single document only)
//!
//! This layer operates on **one** [`Pdf`]. Rebuilding across multiple input
//! documents ([`crate::CombinedPlan`] with >1 input) additionally
//! requires cross-document object copying (renumbering, encryption-boundary
//! handling, name-conflict resolution) and is a separate future layer. The
//! single-input CLI wiring, outline/dest remap, and AcroForm
//! handling all operate over a single document and can build on the
//! [`RebuildResult`] returned here.
//!
//! Obsolete intermediate `/Pages` nodes are intentionally left as orphan
//! objects (unreachable from the page tree) for the unreferenced-resource
//! pruning layer to remove, mirroring the precedent set by
//! [`crate::job::QPDFJob::split_pages`]. Their qpdf-inheritable keys are
//! removed before they become orphaned, so preserved orphan objects still
//! match qpdf's flattening-side cleanup.

use crate::object_handle::ObjectHandleIdentity;
use crate::pages::{
    repair::{prepare_for_optimization_with_max_depth, PageTreeRoot},
    resolve_inherited_handle_with_max_depth, DEFAULT_MAX_PAGE_TREE_DEPTH,
};
use crate::{Error, ObjectHandle, ObjectRef, Pdf, Result};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
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
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RebuildResult {
    /// The rebuilt root `/Pages` `/Kids`, in selection order.
    pub new_kids: Vec<ObjectRef>,
    /// Source page ref → every new leaf ref derived from it (selection order).
    pub ref_map: BTreeMap<ObjectRef, Vec<ObjectRef>>,
    /// Every original page-tree leaf that the rebuild dropped: the source page
    /// leaves (qpdf `getAllPages`) that are absent from `ref_map`.
    ///
    /// This is the exact set qpdf nulls during `--pages` (`QPDFJob` enumerates
    /// the original page tree and replaces each unselected `/Page` object with
    /// `null`). Downstream null-out (8.10 outline / named-destination remap)
    /// keys on membership here so a destination is only allowed to null a
    /// genuine removed page — never an arbitrary non-page object it happens to
    /// reference. Membership is captured from the **original** tree before the
    /// rebuild reparents leaves, so it cannot be reconstructed afterwards.
    pub removed_pages: BTreeSet<ObjectRef>,
}

// ---------------------------------------------------------------------------
// Inherited-attribute handle helpers
// ---------------------------------------------------------------------------

/// Promote one direct non-scalar inherited value through the canonical PDF
/// object registry, reusing an earlier promotion of the same live handle.
///
/// qpdf's `QPDF_optimization.cc:179-190` promotes direct arrays, dictionaries,
/// and streams before attaching them to leaves, while scalar values are copied
/// directly. The shared direct-handle cache keeps several selected leaves from
/// minting separate objects for one ancestor value.
#[allow(
    clippy::mutable_key_type,
    reason = "ObjectHandleIdentity intentionally keys the canonical live allocation"
)]
fn promote_inherited_value<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    value: ObjectHandle,
    promoted: &mut HashMap<ObjectHandleIdentity, ObjectHandle>,
) -> Result<ObjectHandle> {
    value.try_dereference()?;
    let non_scalar = value.as_array().is_some()
        || value.as_dictionary().is_some()
        || value.as_stream_dict().is_some();
    if !value.is_direct() || !non_scalar {
        return Ok(value);
    }
    let identity = value.identity_key();
    if let Some(indirect) = promoted.get(&identity) {
        return Ok(indirect.clone());
    }
    let indirect = pdf.make_indirect_object_handle(value.clone())?;
    promoted.insert(identity, indirect.clone());
    Ok(indirect)
}

/// Replace a missing/null leaf key with the live inherited handle.
fn install_inherited_value<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page: &ObjectHandle,
    key: &[u8],
    value: Option<&ObjectHandle>,
) -> Result<()> {
    if page.try_has_key(key)? {
        return Ok(());
    }
    let Some(value) = value else {
        return Ok(());
    };
    page.replace_key(key, value.clone())?;
    pdf.mark_object_handle_dirty(page)
}

/// Resolve and prepare an inherited value only when the page has no visible
/// own value. A leaf-owned non-scalar must not be promoted merely because the
/// canonical walk returns that same leaf handle.
#[allow(
    clippy::mutable_key_type,
    reason = "ObjectHandleIdentity intentionally keys the canonical live allocation"
)]
fn resolve_inherited_for_page<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    page: &ObjectHandle,
    key: &[u8],
    max_depth: usize,
    promoted: &mut HashMap<ObjectHandleIdentity, ObjectHandle>,
) -> Result<Option<ObjectHandle>> {
    let value = resolve_inherited_handle_with_max_depth(pdf, page_ref, key, max_depth)?;
    if page.try_has_key(key)? {
        return Ok(None);
    }
    value
        .map(|value| promote_inherited_value(pdf, value, promoted))
        .transpose()
}

/// Collect the canonical `/Pages` handles that qpdf's inherited-attribute
/// push visits before flattening the tree. The handles are captured before
/// leaf reparenting so the now-orphaned intermediate nodes can receive the
/// same inheritable-key cleanup as the retained root.
#[allow(
    clippy::mutable_key_type,
    reason = "ObjectHandleIdentity intentionally keys the canonical live allocation"
)]
fn collect_page_tree_nodes(
    node: ObjectHandle,
    nodes: &mut Vec<ObjectHandle>,
    seen: &mut HashSet<ObjectHandleIdentity>,
    depth: usize,
    max_depth: usize,
) -> Result<()> {
    if depth >= max_depth {
        let location = node
            .object_ref()
            .map_or_else(|| "direct /Pages node".to_owned(), |r| r.to_string());
        return Err(Error::Unsupported(format!(
            "page tree depth exceeds maximum of {max_depth} at {location}"
        )));
    }
    if !seen.insert(node.identity_key()) {
        return Ok(());
    }

    node.try_dereference()?;
    if !node.try_is_dictionary_of_type(b"Pages", b"")? {
        return Ok(());
    }
    nodes.push(node.clone());

    let kids = node.try_get_key(b"/Kids")?;
    let Some(kid_count) = kids.try_array_len()? else {
        return Ok(());
    };
    for index in 0..kid_count {
        let Some(kid) = kids.try_array_item(index)? else {
            continue; // cov:ignore: canonical array handles return Some for every in-range item
        };
        if kid.try_has_key(b"/Kids")? {
            collect_page_tree_nodes(kid, nodes, seen, depth + 1, max_depth)?;
        }
    }
    Ok(())
}

/// Materialize direct non-scalar inheritable values on every `/Pages` node
/// before the page-selection rebuild discards any branch.
///
/// qpdf's `pushInheritedAttributesToPageInternal` walks the complete tree
/// before flattening (`QPDF_optimization.cc:159-239`). Its direct-array,
/// dictionary, and stream promotion therefore also runs for a branch whose
/// pages will not be selected; the later page-selection operation can discard
/// that branch while `--preserve-unreferenced` still serializes the promoted
/// object. Keep the existing promotion cache so a value later inherited by a
/// selected leaf retains one canonical identity.
#[allow(
    clippy::mutable_key_type,
    reason = "ObjectHandleIdentity intentionally keys the canonical live allocation"
)]
fn promote_page_tree_inheritable_values<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    nodes: &[ObjectHandle],
    promoted: &mut HashMap<ObjectHandleIdentity, ObjectHandle>,
) -> Result<()> {
    let inheritable_keys = [
        b"/CropBox".as_slice(),
        b"/MediaBox".as_slice(),
        b"/Resources".as_slice(),
        b"/Rotate".as_slice(),
    ];

    for node in nodes {
        for key in inheritable_keys {
            if !node.try_has_key(key)? {
                continue;
            }
            let value = node.try_get_key(key)?;
            let promoted_value = promote_inherited_value(pdf, value.clone(), promoted)?;
            if !promoted_value.is_same_object_as(&value) {
                node.replace_key(key, promoted_value)?;
                pdf.mark_object_handle_dirty(node)?;
            }
        }
    }
    Ok(())
}

fn page_tree_root_handle<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_root: PageTreeRoot,
) -> Result<ObjectHandle> {
    match page_root {
        PageTreeRoot::Indirect(root_ref) => Ok(pdf.get_object_handle(root_ref)),
        PageTreeRoot::Direct { catalog } => pdf.get_object_handle(catalog).try_get_key(b"/Pages"),
    }
}

/// Warn about discarded unknown keys and remove qpdf's four inheritable keys
/// from every original `/Pages` node.
///
/// The selected leaves have already received their effective values when this
/// runs. Keeping this cleanup on live handles matters when the writer is asked
/// to preserve otherwise-unreferenced objects: the old intermediate nodes are
/// then still serialized, but no longer retain stale inherited attributes.
///
/// `QPDF::flattenPagesTree` asks `pushInheritedAttributesToPage` to warn while
/// walking an intermediate `/Pages` node. The retained root is excluded by its
/// missing `/Parent`; structural keys are also excluded because they are part
/// of the page-tree representation rather than inheritable page attributes.
/// qpdf prefixes each warning with the current `Pages object` description; the
/// parenthesized prefix below lets the resolver add the input filename in the
/// same position.
fn remove_inheritable_keys_from_page_tree<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    nodes: &[ObjectHandle],
) -> Result<()> {
    let inheritable_keys = [
        b"/CropBox".as_slice(),
        b"/MediaBox".as_slice(),
        b"/Resources".as_slice(),
        b"/Rotate".as_slice(),
    ];
    let structural_keys = [
        b"/Type".as_slice(),
        b"/Parent".as_slice(),
        b"/Kids".as_slice(),
        b"/Count".as_slice(),
    ];

    for node in nodes {
        let pages_object = node.object_ref().map_or_else(
            || "Pages object".to_owned(),
            |object_ref| {
                format!(
                    "Pages object: object {} {}",
                    object_ref.number, object_ref.generation
                )
            },
        );

        if node.try_has_key(b"/Parent")? {
            for key in node.try_get_keys()? {
                if !inheritable_keys.contains(&key.as_slice())
                    && !structural_keys.contains(&key.as_slice())
                {
                    pdf.push_warning(format!(
                        "({pages_object}): Unknown key {} in /Pages object is being discarded as a result of flattening the /Pages tree",
                        String::from_utf8_lossy(&key)
                    ))?;
                }
            }
        }

        for key in inheritable_keys {
            node.remove_key(key);
        }
        pdf.mark_object_handle_dirty(node)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

/// Rebuild the document's `/Pages` tree from `selected` leaf page refs.
///
/// `selected` is the ordered list of source `/Page` `ObjectRef`s (from
/// [`crate::PagePlan::pages`] / a single-input
/// [`crate::CombinedPlan`]). Duplicate refs are permitted and
/// produce duplicate output pages, matching qpdf.
///
/// On success the in-memory document is mutated so that its root `/Pages`
/// value lists exactly the selected pages, each with inheritable attributes
/// pushed and `/Parent` repointed at that root. An indirect root keeps
/// its object reference; a direct catalog root remains direct. Serialize the
/// result with [`crate::PdfWriter`].
///
/// The `selected` refs it consumes are produced by
/// [`PagePlan`](crate::PagePlan) (single document) or a single-input
/// [`CombinedPlan`](crate::CombinedPlan). For an end-to-end
/// extraction walkthrough see the runnable `examples/extract_pages.rs`.
///
/// # Errors
///
/// - [`Error::Missing`] when `/Root` or the catalog `/Pages` value is absent,
///   or `selected` is empty.
/// - [`Error::Unsupported`] when the catalog / a selected ref is not a
///   dictionary, the page-tree depth limit is exceeded, or canonical indirect
///   allocation fails while duplicating a selection.
/// - Any error propagated from the canonical `ObjectHandle` resolver or
///   mutation surface.
pub fn rebuild_page_tree<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    selected: &[ObjectRef],
) -> Result<RebuildResult> {
    rebuild_page_tree_with_max_depth(pdf, selected, DEFAULT_MAX_PAGE_TREE_DEPTH)
}

/// Like [`rebuild_page_tree`] but with a caller-supplied inheritance-walk
/// recursion limit.
///
/// # Errors
///
/// - [`Error::Missing`] when `selected` is empty, or when `/Root` or the
///   catalog `/Pages` value is absent.
/// - [`Error::Unsupported`] when the catalog, a selected ref, or the `/Pages`
///   root is not a dictionary, a selected object is not a `/Page` dictionary,
///   the page-tree depth limit (`max_depth`) is exceeded, or canonical indirect
///   allocation fails while duplicating a selection.
/// - Any error propagated from the canonical `ObjectHandle` resolver or
///   mutation surface while resolving the catalog, leaves, or inherited
///   attributes.
#[allow(
    clippy::mutable_key_type,
    reason = "ObjectHandleIdentity intentionally keys the canonical live allocation"
)]
pub fn rebuild_page_tree_with_max_depth<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    selected: &[ObjectRef],
    max_depth: usize,
) -> Result<RebuildResult> {
    if selected.is_empty() {
        return Err(Error::Missing("page-tree rebuild: empty selection"));
    }

    // qpdf obtains the effective /Pages handle through getAllPages before
    // flattening it. That handle can be either an indirect root or a direct
    // dictionary embedded in the catalog. Keep the ownership boundary through
    // this rebuild instead of requiring catalog.get_ref("Pages").
    let _catalog_ref = pdf.root_ref().ok_or(Error::Missing("/Root"))?;
    let prepared =
        prepare_for_optimization_with_max_depth(pdf, max_depth)?.ok_or(Error::Missing("/Pages"))?;
    let page_root = prepared.root;
    let root = page_tree_root_handle(pdf, page_root)?;
    let mut page_tree_nodes = Vec::new();
    collect_page_tree_nodes(
        root.clone(),
        &mut page_tree_nodes,
        &mut HashSet::new(),
        0,
        max_depth,
    )?; // cov:ignore: LLVM attributes this multiline canonical traversal terminator separately

    // qpdf promotes direct non-scalar values on every original /Pages node
    // before page selection discards an unselected branch. Keep these objects
    // in the canonical registry so preserve-unreferenced can serialize them.
    let mut promoted_inherited: HashMap<ObjectHandleIdentity, ObjectHandle> = HashMap::new();
    promote_page_tree_inheritable_values(pdf, &page_tree_nodes, &mut promoted_inherited)?;

    // Capture qpdf's repaired leaf order before changing /Kids or /Parent.
    // Any original leaf absent from ref_map is a removed page.
    let original_pages = prepared.pages;

    let mut new_kids: Vec<ObjectRef> = Vec::with_capacity(selected.len());
    let mut ref_map: BTreeMap<ObjectRef, Vec<ObjectRef>> = BTreeMap::new();
    let mut pending_leaves: Vec<ObjectHandle> = Vec::with_capacity(selected.len());
    // Tracks whether a given source ref has already consumed its in-place slot.
    let mut materialized: BTreeSet<ObjectRef> = BTreeSet::new();

    for &src in selected {
        let page = pdf.get_object_handle(src);
        page.try_dereference()?;
        if !page.try_is_dictionary_of_type(b"Page", b"")? {
            return Err(Error::Unsupported(format!(
                "selected object {src} is not a /Page dictionary"
            )));
        }

        let target = if materialized.insert(src) {
            // Resolve every inheritable attribute through the original live
            // parent chain before the final /Parent replacement. Resolve in
            // qpdf's key order so direct non-scalar promotions have the same
            // deterministic allocation order as QPDF_optimization.cc.
            let inherited_cropbox = resolve_inherited_for_page(
                pdf,
                src,
                &page,
                b"/CropBox",
                max_depth,
                &mut promoted_inherited,
            )?; // cov:ignore: LLVM maps this covered multiline call terminator to the call setup
            let inherited_mediabox = resolve_inherited_for_page(
                pdf,
                src,
                &page,
                b"/MediaBox",
                max_depth,
                &mut promoted_inherited,
            )?; // cov:ignore: LLVM maps this covered multiline call terminator to the call setup
            let inherited_resources = resolve_inherited_for_page(
                pdf,
                src,
                &page,
                b"/Resources",
                max_depth,
                &mut promoted_inherited,
            )?; // cov:ignore: LLVM maps this covered multiline call terminator to the call setup
            let inherited_rotate =
                resolve_inherited_handle_with_max_depth(pdf, src, b"/Rotate", max_depth)?;

            // qpdf's inherited attribute push leaves an absent key absent;
            // explicit null is treated as absent only when a real ancestor
            // value is available. Inherited handles retain their identity.
            install_inherited_value(pdf, &page, b"/CropBox", inherited_cropbox.as_ref())?;
            install_inherited_value(pdf, &page, b"/MediaBox", inherited_mediabox.as_ref())?;
            install_inherited_value(pdf, &page, b"/Resources", inherited_resources.as_ref())?;
            install_inherited_value(pdf, &page, b"/Rotate", inherited_rotate.as_ref())?;
            page.clone()
        } else {
            // Duplicate occurrence: shallow-copy the post-materialization
            // live page and allocate only the page dictionary. Indirect child
            // handles (`/Contents`, `/Resources`, ...) remain shared.
            pdf.make_indirect_object_handle(page.shallow_copy()?)?
        };
        let target_ref = target
            .object_ref()
            .expect("prepared pages and duplicate promotion are indirect");

        new_kids.push(target_ref);
        ref_map.entry(src).or_default().push(target_ref);
        pending_leaves.push(target);
    }

    // Rewrite the root /Pages handle in place: flat /Kids in selection order,
    // /Count equal to the selection length, and no stale /Parent. A direct
    // root remains the live dictionary embedded in the catalog.
    // cov:ignore-start: prepare_for_optimization guarantees that the retained /Pages root is a dictionary
    if root.try_as_dictionary()?.is_none() {
        return Err(Error::Unsupported(match page_root {
            PageTreeRoot::Indirect(root_ref) => {
                format!("document /Pages root {root_ref} is not a dictionary")
            }
            PageTreeRoot::Direct { .. } => {
                "document catalog /Pages root is not a dictionary".into()
            }
        }));
    } // cov:ignore-end
    root.replace_key(b"/Type", ObjectHandle::name(b"Pages".to_vec()))?;
    let kid_handles: Vec<_> = new_kids
        .iter()
        .map(|object_ref| pdf.get_object_handle(*object_ref))
        .collect();
    root.replace_key(b"/Kids", ObjectHandle::array(kid_handles))?;
    let count = i64::try_from(new_kids.len()).unwrap_or(i64::MAX);
    root.replace_key(b"/Count", ObjectHandle::integer(count))?;
    // qpdf's QPDF_optimization.cc:159-228 removes each inheritable key from
    // every /Pages node after pushing its effective value to the leaves. The
    // retained rebuilt root and the now-orphaned intermediate nodes are all
    // subject to that same cleanup.
    remove_inheritable_keys_from_page_tree(pdf, &page_tree_nodes)?;
    root.remove_key(b"/Parent");
    pdf.mark_object_handle_dirty(&root)?;

    // Reparent every retained page through the same live root handle. This is
    // qpdf's `flattenPagesTree` `replaceKey("/Parent", pages)` operation, and
    // direct roots therefore retain one shared direct-dictionary identity.
    for leaf in pending_leaves {
        leaf.replace_key(b"/Parent", root.clone())?;
        pdf.mark_object_handle_dirty(&leaf)?;
    }

    // A removed page is an original leaf that no selection kept (absent from
    // `ref_map`). New refs minted for duplicate selections are fresh object
    // numbers, never original leaves, so they are correctly excluded.
    let removed_pages: BTreeSet<ObjectRef> = original_pages
        .into_iter()
        .filter(|p| !ref_map.contains_key(p))
        .collect();

    Ok(RebuildResult {
        new_kids,
        ref_map,
        removed_pages,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::check_bytes_for_test;
    use crate::pages::page_refs;
    use crate::pipeline::test_support::NthWriteFailure;
    use crate::pipeline::PipelineHandle;
    use crate::writer::write_qpdf_to_memory;
    use crate::Object;
    use crate::Pdf;
    use std::io::Cursor;
    use std::process::Command;
    use std::rc::Rc;

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

    /// Build a two-branch page tree where the selected page is under branch B
    /// and branch A carries a direct non-scalar inherited value. qpdf promotes
    /// branch A's direct `/MediaBox` before page selection discards that branch,
    /// so preserve-unreferenced output retains the promoted orphan object.
    fn build_deselected_branch_pdf() -> Vec<u8> {
        let parts: Vec<(u32, String)> = vec![
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".into()),
            (
                3,
                "<< /Type /Pages /Parent 2 0 R /Kids [5 0 R] /Count 1 \
                 /MediaBox [0 0 200 300] >>"
                    .into(),
            ),
            (
                4,
                "<< /Type /Pages /Parent 2 0 R /Kids [6 0 R] /Count 1 /Rotate 180 >>".into(),
            ),
            (5, "<< /Type /Page /Parent 3 0 R /Contents 7 0 R >>".into()),
            (
                6,
                "<< /Type /Page /Parent 4 0 R /Contents 8 0 R /MediaBox [0 0 612 792] >>".into(),
            ),
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

        let xref_start = out.len() as u64;
        let total = 9u32;
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

    /// A root `/Pages` node whose only `/Kids` entry is a direct `/Page`
    /// dictionary. qpdf's page enumeration promotes that live handle to the
    /// first available indirect object before a later rebuild allocates a
    /// duplicate selection slot.
    fn build_direct_leaf_pdf() -> Vec<u8> {
        let mut out = b"%PDF-1.4\n".to_vec();
        let off1 = out.len() as u64;
        out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = out.len() as u64;
        out.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [<< /Type /Page /MediaBox [0 0 612 792] >>] /Count 1 >>\nendobj\n",
        );

        let xref_start = out.len() as u64;
        out.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
        out.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        out.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        out.extend_from_slice(
            format!("trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        out
    }

    fn open(bytes: Vec<u8>) -> Pdf<Cursor<Vec<u8>>> {
        Pdf::open(Cursor::new(bytes)).expect("PDF should parse")
    }

    #[test]
    fn rebuild_marks_qpdf_get_all_pages_observation() {
        let mut pdf = open(build_nested_pdf());
        assert!(!pdf.ever_called_get_all_pages());

        rebuild_page_tree(&mut pdf, &[ObjectRef::new(4, 0)]).unwrap();

        assert!(
            pdf.ever_called_get_all_pages(),
            "qpdf rebuild preparation enumerates pages through getAllPages"
        );
    }

    #[allow(
        clippy::mutable_key_type,
        reason = "the test exercises the canonical live-allocation identity set"
    )]
    #[test]
    fn collect_page_tree_nodes_handles_duplicate_and_malformed_handles() {
        let pages = ObjectHandle::dictionary(vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"Pages".to_vec())),
            (b"/Kids".to_vec(), ObjectHandle::integer(7)),
        ]);
        let mut nodes = Vec::new();
        let mut seen = HashSet::new();

        collect_page_tree_nodes(pages.clone(), &mut nodes, &mut seen, 0, 8)
            .expect("a malformed /Kids value is treated as an empty array");
        assert_eq!(nodes.len(), 1);
        collect_page_tree_nodes(pages, &mut nodes, &mut seen, 0, 8)
            .expect("a repeated canonical handle is skipped");
        assert_eq!(nodes.len(), 1);

        let leaf = ObjectHandle::dictionary(vec![(
            b"/Type".to_vec(),
            ObjectHandle::name(b"Page".to_vec()),
        )]);
        collect_page_tree_nodes(leaf, &mut nodes, &mut seen, 0, 8)
            .expect("a non-/Pages dictionary is ignored");
        assert_eq!(nodes.len(), 1);

        let direct_pages = ObjectHandle::dictionary(vec![(
            b"/Type".to_vec(),
            ObjectHandle::name(b"Pages".to_vec()),
        )]);
        let error = collect_page_tree_nodes(direct_pages, &mut nodes, &mut seen, 0, 0)
            .expect_err("the depth bound must apply before dereferencing the node");
        assert!(
            matches!(error, Error::Unsupported(message) if message.contains("direct /Pages node"))
        );
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
    fn direct_catalog_root_stays_direct_after_rebuild_and_round_trips() {
        let mut pdf = open(build_nested_pdf());
        let root = dict_of(&mut pdf, ObjectRef::new(2, 0));
        let Object::Dictionary(mut catalog) = pdf.resolve_object(ObjectRef::new(1, 0)).unwrap()
        else {
            panic!("catalog must be a dictionary"); // cov:ignore: build_nested_pdf fixes object 1 as the catalog dictionary
        };
        catalog.insert("Pages", Object::Dictionary(root));
        pdf.set_object(ObjectRef::new(1, 0), Object::Dictionary(catalog));

        let result =
            rebuild_page_tree(&mut pdf, &[ObjectRef::new(4, 0), ObjectRef::new(4, 0)]).unwrap();
        assert_eq!(result.new_kids.len(), 2);

        let Object::Dictionary(catalog) = pdf.resolve_object(ObjectRef::new(1, 0)).unwrap() else {
            panic!("catalog must remain a dictionary"); // cov:ignore: only this test writes object 1 as a catalog dictionary
        };
        let Some(Object::Dictionary(root)) = catalog.get("Pages") else {
            panic!("catalog /Pages must remain direct"); // cov:ignore: direct-root preservation is asserted by the successful rebuild above
        };
        assert_eq!(root.get("Count"), Some(&Object::Integer(2)));
        for page_ref in result.new_kids {
            let page = dict_of(&mut pdf, page_ref);
            assert_eq!(page.get("Parent"), Some(&Object::Dictionary(root.clone())));
        }

        let out = write_qpdf_to_memory(&mut pdf, |_| {}).unwrap();
        let mut reopened = Pdf::open(Cursor::new(out)).expect("direct-root output must parse");
        assert_eq!(
            crate::PageDocumentHelper::new(&mut reopened)
                .get_all_pages()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn rebuild_does_not_synthesize_absent_rotate() {
        let mut pdf = open(build_direct_leaf_pdf());
        let prepared = crate::pages::repair::prepare_for_optimization(&mut pdf)
            .expect("page preparation must succeed")
            .expect("fixture has a page tree");
        assert_eq!(prepared.pages, vec![ObjectRef::new(3, 0)]);

        rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)])
            .expect("flat page rebuild must succeed");

        let page = dict_of(&mut pdf, ObjectRef::new(3, 0));
        assert!(
            page.get("Rotate").is_none(),
            "qpdf leaves an absent inherited /Rotate absent"
        );
    }

    #[test]
    fn rebuild_preserves_inherited_indirect_handle_identity() {
        let mut pdf = open(build_nested_pdf());
        let page = pdf.get_object_handle(ObjectRef::new(4, 0));
        let resources = pdf.get_object_handle(ObjectRef::new(9, 0));

        rebuild_page_tree(&mut pdf, &[ObjectRef::new(4, 0)])
            .expect("flat page rebuild must succeed");

        let page_resources = page
            .try_get_key(b"/Resources")
            .expect("page /Resources lookup must succeed");
        assert!(
            page_resources.is_same_object_as(&resources),
            "inherited /Resources must retain its live indirect handle"
        );
    }

    #[test]
    fn rebuild_mutates_retained_page_handles_in_place() {
        let mut pdf = open(build_nested_pdf());
        let root = pdf.get_object_handle(ObjectRef::new(2, 0));
        let page = pdf.get_object_handle(ObjectRef::new(4, 0));

        rebuild_page_tree(&mut pdf, &[ObjectRef::new(4, 0)])
            .expect("flat page rebuild must succeed");

        assert_eq!(
            root.try_get_key(b"/Count")
                .expect("live root /Count lookup must succeed")
                .as_integer(),
            Some(1)
        );
        let kids = root
            .try_get_key(b"/Kids")
            .expect("live root /Kids lookup must succeed");
        assert_eq!(
            kids.try_array_item(0)
                .expect("live root /Kids item lookup must succeed")
                .expect("the rebuilt root must have one child")
                .object_ref(),
            Some(ObjectRef::new(4, 0))
        );
        assert!(
            page.try_get_key(b"/Parent")
                .expect("live page /Parent lookup must succeed")
                .is_same_object_as(&root),
            "reparenting must be visible through the retained page handle"
        );
    }

    #[test]
    fn rebuilt_root_drops_qpdf_inheritable_attributes_after_materializing_pages() {
        let mut pdf = open(build_nested_pdf());
        let mut root = dict_of(&mut pdf, ObjectRef::new(2, 0));
        root.insert(
            "MediaBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(612),
                Object::Integer(792),
            ]),
        );
        root.insert(
            "CropBox",
            Object::Array(vec![
                Object::Integer(10),
                Object::Integer(20),
                Object::Integer(500),
                Object::Integer(700),
            ]),
        );
        root.insert("Resources", Object::Dictionary(crate::Dictionary::new()));
        root.insert("Rotate", Object::Integer(180));
        pdf.set_object(ObjectRef::new(2, 0), Object::Dictionary(root));

        rebuild_page_tree(&mut pdf, &[ObjectRef::new(6, 0)])
            .expect("flat page rebuild must succeed");

        let root = dict_of(&mut pdf, ObjectRef::new(2, 0));
        for key in ["MediaBox", "CropBox", "Resources", "Rotate"] {
            assert!(
                root.get(key).is_none(),
                "rebuilt /Pages root must not retain inheritable /{key}: {root:?}"
            );
        }

        let intermediate = dict_of(&mut pdf, ObjectRef::new(3, 0));
        for key in ["MediaBox", "CropBox", "Resources", "Rotate"] {
            assert!(
                intermediate.get(key).is_none(),
                "orphaned intermediate /Pages must not retain inheritable /{key}: {intermediate:?}"
            );
        }

        let page = dict_of(&mut pdf, ObjectRef::new(6, 0));
        assert_eq!(
            page.get("CropBox"),
            Some(&Object::Reference(ObjectRef::new(10, 0))),
            "the selected page must retain the root-inherited /CropBox handle"
        );
    }

    #[test]
    fn promotes_direct_inheritable_values_on_deselected_branches() {
        let mut pdf = open(build_deselected_branch_pdf());

        rebuild_page_tree(&mut pdf, &[ObjectRef::new(6, 0)])
            .expect("page-tree rebuild must succeed");

        let promoted = pdf
            .get_all_objects()
            .expect("canonical object enumeration must succeed")
            .into_iter()
            .filter_map(|handle| handle.as_array())
            .any(|items| {
                items.len() == 4
                    && items
                        .iter()
                        .map(ObjectHandle::as_integer)
                        .collect::<Vec<_>>()
                        == vec![Some(0), Some(0), Some(200), Some(300)]
            });
        assert!(
            promoted,
            "qpdf promotes the deselected branch's direct /MediaBox before cleanup"
        );
    }

    #[test]
    fn rebuild_warns_for_unknown_keys_only_on_flattened_pages_nodes() {
        let mut pdf = open(build_nested_pdf());

        let mut root = dict_of(&mut pdf, ObjectRef::new(2, 0));
        root.insert("UserUnit", Object::Integer(1));
        pdf.set_object(ObjectRef::new(2, 0), Object::Dictionary(root));

        let mut intermediate = dict_of(&mut pdf, ObjectRef::new(3, 0));
        intermediate.insert("UserUnit", Object::Integer(2));
        pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(intermediate));

        rebuild_page_tree(&mut pdf, &[ObjectRef::new(4, 0)])
            .expect("page-tree rebuild must preserve qpdf warning behavior");

        let diagnostics = pdf.repair_diagnostics();
        let warnings: Vec<_> = diagnostics
            .entries()
            .iter()
            .map(|entry| entry.message.as_str())
            .filter(|message| message.contains("Unknown key /UserUnit"))
            .collect();
        assert_eq!(
            warnings,
            ["(Pages object: object 3 0): Unknown key /UserUnit in /Pages object is being discarded as a result of flattening the /Pages tree"],
            "only the flattened intermediate /Pages node should warn"
        );

        let root = dict_of(&mut pdf, ObjectRef::new(2, 0));
        assert_eq!(root.get("UserUnit"), Some(&Object::Integer(1)));
    }

    #[test]
    fn rebuild_propagates_unknown_pages_warning_sink_failure() {
        let mut pdf = open(build_nested_pdf());

        let mut intermediate = dict_of(&mut pdf, ObjectRef::new(3, 0));
        intermediate.insert("UserUnit", Object::Integer(2));
        pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(intermediate));

        let logger = crate::QPDFLogger::create();
        logger.set_warn(Some(PipelineHandle::new(NthWriteFailure::new(1))));
        pdf.set_logger(logger);

        assert!(matches!(
            rebuild_page_tree(&mut pdf, &[ObjectRef::new(4, 0)]),
            Err(Error::System(ref message)) if message == "sink write failure 1"
        ));
        assert!(pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.message.contains("Unknown key /UserUnit")));
    }

    #[test]
    fn inherited_attrs_materialized_on_leaf() {
        // Page 1 (obj 4) inherits everything from intermediate node 3.
        let mut pdf = open(build_nested_pdf());
        rebuild_page_tree(&mut pdf, &[ObjectRef::new(4, 0)]).unwrap();

        let leaf = dict_of(&mut pdf, ObjectRef::new(4, 0));
        assert_eq!(leaf.get("Rotate"), Some(&Object::Integer(90)));
        let Some(Object::Reference(media_box_ref)) = leaf.get("MediaBox") else {
            // cov:ignore-start: fixture-shape guard
            panic!("expected promoted inherited /MediaBox reference, got {leaf:?}");
            // cov:ignore-end
        };
        assert_eq!(
            pdf.resolve_object(*media_box_ref).unwrap(),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(200),
                Object::Integer(300),
            ])
        );
        // /Resources inherited via indirect ref 9 0 R retains that live
        // indirect handle, rather than materializing a dictionary clone.
        match leaf.get("Resources") {
            Some(Object::Reference(resources_ref)) => {
                assert_eq!(*resources_ref, ObjectRef::new(9, 0));
            }
            other => panic!("expected inherited /Resources reference, got {other:?}"), // cov:ignore: fixture-shape guard
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
    fn duplicate_selection_allocates_after_canonical_direct_promotion() {
        let mut pdf = open(build_direct_leaf_pdf());

        // This is the lower-layer sequence from the review: page enumeration
        // first promotes the direct leaf, then rebuild receives its canonical
        // reference twice. The allocator must see that promoted reference.
        let prepared = crate::pages::repair::prepare_for_optimization(&mut pdf)
            .expect("page preparation must succeed")
            .expect("fixture has a page tree");
        assert_eq!(prepared.pages, vec![ObjectRef::new(3, 0)]);

        let result = rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0), ObjectRef::new(3, 0)])
            .expect("duplicate rebuild must succeed");
        assert_eq!(
            result.new_kids,
            vec![ObjectRef::new(3, 0), ObjectRef::new(4, 0)]
        );

        let root = dict_of(&mut pdf, ObjectRef::new(2, 0));
        assert_eq!(
            root.get("Kids"),
            Some(&Object::Array(vec![
                Object::Reference(ObjectRef::new(3, 0)),
                Object::Reference(ObjectRef::new(4, 0)),
            ]))
        );
    }

    #[test]
    fn subset_round_trips_to_valid_pdf() {
        // Extract pages 1 and 3; write, reopen, and verify structure + check.
        let mut pdf = open(build_nested_pdf());
        rebuild_page_tree(&mut pdf, &[ObjectRef::new(4, 0), ObjectRef::new(6, 0)]).unwrap();

        let out = write_qpdf_to_memory(&mut pdf, |_| {}).unwrap();

        let mut pdf2 = Pdf::open(Cursor::new(out.clone())).expect("rebuilt PDF should parse");
        let refs = page_refs(&mut pdf2).expect("page tree should walk");
        assert_eq!(refs.len(), 2, "/Pages should enumerate exactly 2 leaves");

        // Each leaf must carry the materialized inherited attrs after reopen.
        for page_ref in refs {
            let leaf = dict_of(&mut pdf2, page_ref);
            assert_eq!(leaf.get("Rotate"), Some(&Object::Integer(90)));
            assert!(leaf.get("MediaBox").is_some());
        }

        // Belt-and-suspenders: the crate's own validity check is clean.
        check_bytes_for_test(out).expect("canonical qpdf check should run");
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

        let out = write_qpdf_to_memory(&mut pdf, |_| {}).unwrap();

        let mut pdf2 = Pdf::open(Cursor::new(out)).expect("should parse");
        let refs = page_refs(&mut pdf2).expect("walk");
        assert_eq!(refs.len(), 3, "duplicate selection → 3 enumerated pages");
    }

    #[test]
    fn rebuild_honors_repair_depth_limit() {
        let mut pdf = open(build_nested_pdf());

        let mut root = dict_of(&mut pdf, ObjectRef::new(2, 0));
        root.insert(
            "Kids",
            Object::Array(vec![Object::Reference(ObjectRef::new(10, 0))]),
        );
        root.insert("Count", Object::Integer(1));
        pdf.set_object(ObjectRef::new(2, 0), Object::Dictionary(root));

        let mut first = crate::Dictionary::new();
        first.insert("Type", Object::Name(b"Pages".to_vec()));
        first.insert("Parent", Object::Reference(ObjectRef::new(2, 0)));
        first.insert(
            "Kids",
            Object::Array(vec![Object::Reference(ObjectRef::new(11, 0))]),
        );
        first.insert("Count", Object::Integer(1));
        pdf.set_object(ObjectRef::new(10, 0), Object::Dictionary(first));

        // The missing /Type is deliberately below the supplied bound. The old
        // default-bound preparation silently repairs it before rebuilding.
        let mut second = crate::Dictionary::new();
        second.insert("Parent", Object::Reference(ObjectRef::new(10, 0)));
        second.insert(
            "Kids",
            Object::Array(vec![Object::Reference(ObjectRef::new(4, 0))]),
        );
        second.insert("Count", Object::Integer(1));
        pdf.set_object(ObjectRef::new(11, 0), Object::Dictionary(second));

        let Object::Dictionary(mut leaf) = pdf.resolve_object(ObjectRef::new(4, 0)).unwrap() else {
            panic!("selected object must be a page"); // cov:ignore: build_nested_pdf fixes object 4 as a page dictionary
        };
        leaf.insert("Parent", Object::Reference(ObjectRef::new(10, 0)));
        leaf.insert("Rotate", Object::Integer(0));
        leaf.insert("Resources", Object::Dictionary(crate::Dictionary::new()));
        leaf.insert(
            "CropBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(200),
                Object::Integer(300),
            ]),
        );
        pdf.set_object(ObjectRef::new(4, 0), Object::Dictionary(leaf));

        let before_root = dict_of(&mut pdf, ObjectRef::new(2, 0));
        let before_second = dict_of(&mut pdf, ObjectRef::new(11, 0));
        let error = rebuild_page_tree_with_max_depth(&mut pdf, &[ObjectRef::new(4, 0)], 2)
            .expect_err("repair must use the caller-supplied depth limit");
        assert!(matches!(error, Error::Unsupported(_)), "got {error:?}");
        assert_eq!(dict_of(&mut pdf, ObjectRef::new(2, 0)), before_root);
        assert_eq!(dict_of(&mut pdf, ObjectRef::new(11, 0)), before_second);
    }

    #[test]
    fn rebuild_cases_are_pinned_to_qpdf_11_9_page_probe() {
        let version = match Command::new("qpdf").arg("--version").output() {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).into_owned()
            }
            // cov:ignore-start: qpdf is an optional runtime oracle for this test.
            Ok(_) | Err(_) => {
                eprintln!("qpdf 11.9.0 unavailable; skipping page rebuild oracle probe");
                return;
            } // cov:ignore-end
        };
        // cov:ignore-start: the probe is only authoritative for the pinned qpdf release.
        if !version.starts_with("qpdf version 11.9.") {
            eprintln!("qpdf 11.9.0 unavailable; skipping page rebuild oracle probe");
            return;
        }
        // cov:ignore-end

        let directory = tempfile::tempdir().expect("create qpdf page probe directory");
        let input = directory.path().join("input.pdf");
        let output = directory.path().join("output.pdf");
        std::fs::write(&input, build_nested_pdf()).expect("write qpdf page probe input");

        let flattened = Command::new("qpdf")
            .arg(&input)
            .arg("--pages")
            .arg(&input)
            .arg("1,1")
            .arg("--")
            .arg(&output)
            .output()
            .expect("run qpdf duplicate page probe");
        assert!(
            flattened.status.success(),
            "qpdf duplicate page probe failed: {}",
            String::from_utf8_lossy(&flattened.stderr) // cov:ignore: assertion failure arm
        );

        let pages = Command::new("qpdf")
            .arg("--show-pages")
            .arg(&output)
            .output()
            .expect("run qpdf page listing probe");
        assert!(
            pages.status.success(),
            "qpdf page listing probe failed: {}",
            String::from_utf8_lossy(&pages.stderr) // cov:ignore: assertion failure arm
        );
        let pages = String::from_utf8_lossy(&pages.stdout);
        assert_eq!(
            pages.matches("page ").count(),
            2,
            "qpdf duplicate selection must produce two page entries: {pages}"
        );

        let page = Command::new("qpdf")
            .arg("--show-object=4")
            .arg(&output)
            .output()
            .expect("show qpdf first page probe");
        assert!(
            page.status.success(),
            "qpdf first-page probe failed: {}",
            String::from_utf8_lossy(&page.stderr) // cov:ignore: assertion failure arm
        );
        let page = String::from_utf8_lossy(&page.stdout);
        assert!(page.contains("/Rotate 90"), "qpdf first page: {page}");
        assert!(page.contains("/MediaBox"), "qpdf first page: {page}");
        assert!(page.contains("/Resources"), "qpdf first page: {page}");

        let mut direct_leaf = open(build_direct_leaf_pdf());
        rebuild_page_tree(&mut direct_leaf, &[ObjectRef::new(3, 0)])
            .expect("direct leaf rebuild must succeed");
        let direct_page = dict_of(&mut direct_leaf, ObjectRef::new(3, 0));
        assert!(
            direct_page.get("Rotate").is_none(),
            "qpdf's direct-leaf probe has no inherited /Rotate to push"
        );
    }

    #[test]
    fn promote_inherited_value_promotes_direct_streams() {
        let mut pdf = Pdf::empty().expect("empty PDF");
        let stream = ObjectHandle::stream(
            ObjectHandle::dictionary(Vec::new()),
            Rc::new(b"stream".to_vec()),
        );
        let promoted = promote_inherited_value(&mut pdf, stream, &mut HashMap::new())
            .expect("direct stream promotion must succeed");

        assert!(promoted.is_indirect());
        assert!(promoted.as_stream_dict().is_some());
    }

    #[test]
    #[allow(
        clippy::mutable_key_type,
        reason = "ObjectHandleIdentity intentionally keys the canonical live allocation"
    )]
    fn promote_inherited_value_reuses_a_same_identity_promotion_from_a_map() {
        let mut pdf = Pdf::empty().expect("empty PDF");
        let value = ObjectHandle::dictionary(vec![(b"Marker".to_vec(), ObjectHandle::integer(7))]);
        let mut promoted = std::collections::HashMap::new();

        let first = promote_inherited_value(&mut pdf, value.clone(), &mut promoted)
            .expect("first promotion must succeed");
        let second = promote_inherited_value(&mut pdf, value, &mut promoted)
            .expect("same-identity promotion must reuse the cached object");

        assert!(first.is_indirect());
        assert!(first.is_same_object_as(&second));
        assert_eq!(promoted.len(), 1);
    }
}
