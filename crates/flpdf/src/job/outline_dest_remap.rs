//! qpdf correspondence: QPDFJob.cc page selection and QPDFWriter.cc null visibility specialized for surviving destinations.
//! Outline and named-destination remapping after page extraction.
//!
//! After [`crate::pages::tree_rebuild::rebuild_page_tree`] has rebuilt the page tree
//! for a subset extraction, this module updates the document's `/Outlines` tree,
//! `/Names /Dests` name-tree (and the legacy `/Catalog /Dests` dictionary), the
//! link annotations (`/Dest`, `/A /GoTo /D`) on every surviving page, and the
//! catalog `/OpenAction`, all to match qpdf's `--pages` behaviour:
//!
//! - Every outline item and named destination is **kept** — none are dropped.
//!   Sibling `/Prev`/`/Next` links, parent `/First`/`/Last`, `/Count`, and name-tree
//!   `/Limits` are all left unchanged, and `/Outlines`/`/Names` are never removed
//!   from the catalog.
//! - A destination whose target page **survived** is remapped to its new
//!   `ObjectRef` (the first element of `ref_map[old_ref]`, matching qpdf's rule
//!   that a destination resolves to the first occurrence of a duplicated page).
//! - Every **removed** original page leaf is replaced with `null` in place, up
//!   front and independent of how it is referenced — qpdf enumerates the
//!   original page tree and `replaceObject`s each unselected `/Page`. A
//!   destination targeting a removed page is left verbatim, now resolving to
//!   that `null`. The subsequent job subset sweep keeps
//!   the null object only while a surviving destination still references it; a
//!   removed page referenced by nothing is garbage-collected entirely. Nulling
//!   the page object — rather than whatever a destination's (possibly indirect,
//!   possibly non-page) first element points at — means a removed page reached
//!   only through a reference holder or a non-page wrapper dictionary is still
//!   severed, so excluded page contents cannot leak into the output, while a
//!   non-page object a malformed destination happens to reference is never
//!   touched.
//!
//! # qpdf 11.9.0 observed behaviour (truth source `/usr/bin/qpdf`)
//!
//! For `qpdf in.pdf --pages in.pdf 1,3 -- out.pdf` over a document with an
//! `/Outlines` tree and a `/Names /Dests` name-tree, qpdf does not drop any
//! outline item or named destination: it sets each removed page object to `null`,
//! leaving destinations pointing at the now-null page (e.g. `[ 10 0 R /XYZ 0 792 0 ]`
//! where `10 0 R` resolves to `null`), and leaves `/Count` and the name-tree
//! `/Limits` unchanged. A removed page referenced by no surviving destination is
//! absent from the output. This module reproduces that behaviour.
//!
//! The removed-page null-out is page-driven, so it also covers a removed page
//! reached only through a surviving page's link annotation (`/Dest`, or
//! `/A /GoTo /D`) or the catalog `/OpenAction`: qpdf keeps the destination
//! reference verbatim and the target page object is already `null`. An
//! annotation is structurally identical to an outline item for the *remap* of a
//! surviving-page destination, so that remap logic is reused.
//! (A removed page reached only through a structure element's `/Pg` belongs to a
//! different, drop-and-garbage-collect family handled by
//! [`crate::struct_tree_pg`]; a thread bead's `/P` is in the same drop family
//! and is not handled here.)
//!
//! # String-form `/Dest`
//!
//! `/Dest (name)` on an outline item is a named destination. Because no entry is
//! dropped, such items are kept regardless of whether their named destination's
//! page survived; only explicit page references are remapped or nulled.
//!
//! # Scope
//!
//! Single-document only. Multi-input cross-document merge is a separate path.

use crate::object_handle::ObjectHandle;
use crate::pages::tree_rebuild::RebuildResult;
use crate::{Error, ObjectRef, Pdf, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

const DEFAULT_MAX_OUTLINE_REMAP_DEPTH: usize = 100;

// ---------------------------------------------------------------------------
// Surviving-page map
// ---------------------------------------------------------------------------

/// The surviving-page map for a page-tree rebuild, paired with the set of every
/// rebuilt output page ref.
///
/// `map` sends each surviving source page ref to its first new occurrence
/// (`ref_map[old][0]`); a source ref absent from `map` was removed. `new_refs`
/// holds every ref in the rebuilt `/Pages` tree, so a destination already
/// pointing at a remapped new ref is recognised as a surviving target — not a
/// removed page — and is never nulled by the null-pass.
#[derive(Default)]
struct Surviving {
    /// Surviving source page ref → its first new occurrence.
    map: BTreeMap<ObjectRef, ObjectRef>,
    /// Every page ref present in the rebuilt `/Pages` tree.
    new_refs: BTreeSet<ObjectRef>,
}

impl Surviving {
    /// Build from a [`RebuildResult`]: `map` from `ref_map`'s first occurrences,
    /// `new_refs` from every rebuilt page ref (`new_kids`).
    fn from_rebuild(result: &RebuildResult) -> Self {
        let map = result
            .ref_map
            .iter()
            .filter_map(|(&old, new_refs)| new_refs.first().map(|&new| (old, new)))
            .collect();
        let new_refs = result.new_kids.iter().copied().collect();
        Surviving { map, new_refs }
    }

    /// The new (first-occurrence) ref a surviving source page remaps to, or
    /// `None` when `old` is not a surviving source ref.
    fn remap(&self, old: ObjectRef) -> Option<ObjectRef> {
        self.map.get(&old).copied()
    }

    /// Whether `page_ref` denotes a surviving page: either a surviving source
    /// ref (a remap key) or a rebuilt output ref (an already-remapped target).
    fn is_surviving_target(&self, page_ref: ObjectRef) -> bool {
        self.map.contains_key(&page_ref) || self.new_refs.contains(&page_ref)
    }
}

fn child_if_present(parent: &ObjectHandle, key: &[u8]) -> Result<Option<ObjectHandle>> {
    if parent.try_has_key(key)? {
        Ok(Some(parent.try_get_key(key)?))
    } else {
        Ok(None)
    }
}

fn indirect_child(parent: &ObjectHandle, key: &[u8]) -> Result<Option<ObjectHandle>> {
    Ok(child_if_present(parent, key)?.filter(|child| child.object_ref().is_some()))
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Null removed pages and remap surviving-page destinations after a page-tree
/// rebuild (qpdf `--pages` parity).
///
/// `result` is the [`RebuildResult`] returned by
/// [`crate::pages::tree_rebuild::rebuild_page_tree`]. Its `ref_map` encodes the
/// old → new page reference mapping (a page absent from the map was removed; a
/// page present maps to `ref_map[old][0]`, the first new occurrence), and
/// `removed_pages` is the set of dropped original page leaves.
///
/// First, every removed page leaf in `removed_pages` is replaced with `null` in
/// place (qpdf enumerates the original page tree and nulls each unselected
/// `/Page`, regardless of how it is referenced). Then every outline item and
/// named destination is kept and a surviving-page target is remapped to its new
/// ref; a destination targeting a removed page is left verbatim, now resolving
/// to that `null`. The function mutates `pdf` in place (same convention as
/// `rebuild_page_tree`) and remaps no navigation when there is no `/Outlines` or
/// named-destination structure (it still nulls removed pages).
///
/// # Errors
///
/// - Any error propagated from [`Pdf::resolve`].
/// - [`Error::Unsupported`] when the outline depth limit (100) is exceeded or
///   an unexpected object type is encountered in the outline tree.
pub fn remap_outline_and_dests<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    result: &RebuildResult,
) -> Result<()> {
    remap_outline_and_dests_with_max_depth(pdf, result, DEFAULT_MAX_OUTLINE_REMAP_DEPTH)
}

/// Like [`remap_outline_and_dests`] but with a caller-supplied outline-depth limit.
///
/// # Errors
///
/// - Any error propagated from [`Pdf::resolve`].
/// - [`Error::Unsupported`] when the name-tree or outline-tree depth exceeds
///   `max_depth` while remapping.
pub fn remap_outline_and_dests_with_max_depth<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    result: &RebuildResult,
    max_depth: usize,
) -> Result<()> {
    // Step 0: null every removed original page leaf in place (qpdf null-out).
    // qpdf's `--pages` enumerates the original page tree and `replaceObject`s
    // each unselected `/Page` with `null`, independent of how the page is
    // referenced. Doing this up front — rather than nulling whatever a
    // destination's first element resolves to — severs a removed page reached
    // only through a reference holder or a non-page wrapper dictionary, so its
    // contents cannot leak, while never touching a non-page object a malformed
    // destination happens to reference.
    null_removed_pages(pdf, result)?;

    // Step 1: build the surviving-page map (first new ref per surviving source)
    // together with the set of all rebuilt output refs, so a destination already
    // remapped to a surviving page's new ref is never mistaken for a removed
    // target by the remap-pass.
    let surviving = Surviving::from_rebuild(result);

    // Locate the catalog.
    let catalog_ref = match pdf.root_ref() {
        Some(r) => r,
        None => return Ok(()), // No catalog, nothing to do.
    };
    let catalog = pdf.get_object_handle(catalog_ref);
    pdf.resolve(&catalog)?;
    if catalog.try_as_dictionary()?.is_none() {
        return Ok(());
    }

    let outlines = indirect_child(&catalog, b"/Outlines")?;

    // --- Step 2: Remap named destinations -------------------------------------
    // qpdf keeps every named destination: a surviving-page dest is remapped to
    // its new page ref; a removed-page dest is left verbatim (its target page
    // object was already nulled in Step 0, and an unreferenced removed page is
    // then garbage-collected by the later subset sweep). /Names and /Dests are
    // never removed from the catalog, and /Limits is never recomputed.

    // /Names may be an indirect reference OR a direct dictionary on the catalog;
    // /Dests inside it likewise.
    if let Some(names) = child_if_present(&catalog, b"/Names")? {
        if names.try_as_dictionary()?.is_some() {
            if let Some(dests) = child_if_present(&names, b"/Dests")? {
                let mut nt_visited = BTreeSet::new();
                remap_name_tree(pdf, &dests, &surviving, 0, max_depth, &mut nt_visited)?;
            }
        }
    }

    // 2b. Legacy /Catalog /Dests dictionary (PDF 1.1 style)
    if let Some(dests) = child_if_present(&catalog, b"/Dests")? {
        if dests.try_as_dictionary()?.is_some() {
            remap_dests_dict(pdf, &dests, &surviving)?;
        }
    }

    // --- Step 3: Remap the outline tree -----------------------------------
    // Every outline item is kept; only its destination page ref is remapped when
    // the target page survived (a removed target was already nulled in Step 0 and
    // is left referenced verbatim). Sibling links, /Count, and the /Outlines
    // catalog entry are all left unchanged.
    if let Some(outlines) = outlines {
        if let Some(first) = indirect_child(&outlines, b"/First")? {
            let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
            remap_outline_tree(pdf, first, 0, max_depth, &surviving, &mut visited)?;
        }
        // If there is no /First, the outline root has no items → nothing to do.
    }

    // --- Step 4: Link-annotation and /OpenAction destinations -------------
    // Remap a surviving-page destination reached via a surviving page's link
    // annotation (/Dest or /A /GoTo /D) or the catalog /OpenAction. A removed
    // page reached only this way was already nulled in Step 0 (the destination
    // reference is kept verbatim). (A removed page reached only via a thread-bead
    // /P or a struct element /Pg is a different, drop-and-GC family; struct elem
    // /Pg is handled by crate::struct_tree_pg, after this pass in the pipeline.)
    remap_annot_dests(pdf, result, &surviving)?;
    remap_open_action_dest(pdf, &catalog, &surviving)?;

    Ok(())
}

/// Remap link-annotation destinations on every surviving page (qpdf `--pages`
/// parity). An annotation is structurally identical to an outline item for
/// destination purposes (`/Dest` and `/A /GoTo /D`): a surviving target is
/// remapped to its new ref, while a removed target needs no action — the page
/// object was already replaced with `null` by [`null_removed_pages`] and the
/// `/Dest`/`/D` reference is kept verbatim. qpdf applies this to both indirect
/// annotations and inline (direct-dict) annotations stored in `/Annots`, so both
/// forms are handled here.
///
/// An *indirect* annotation is remapped in place via [`remap_item_dest`]. An
/// *inline* (direct-dict) annotation has no object identity, so it is remapped on
/// the array element and the updated `/Annots` array written back (to the page
/// dict for an inline array, or to the array object for an indirect array).
///
/// A duplicate-page selection (e.g. `--pages . 1,1`) produces several surviving
/// pages that share the same indirect annotation object, so the same annotation
/// reference can appear under more than one page. A `visited` set (bounded-
/// traversal guard, as in [`remap_outline_tree`] / [`remap_name_tree`]) processes
/// each shared annot reference — and each shared *indirect* `/Annots` array
/// object — exactly once, so a shared destination is not re-remapped on a later
/// pass (avoiding redundant rewrites). Correctness does not rest on the dedup
/// alone: a destination already pointing at a rebuilt output ref is recognised as
/// a surviving target by [`Surviving::is_surviving_target`], so a re-resolved
/// already-remapped `/Dest` is a no-op because its new page reference is not a
/// source key in the surviving-page map.
/// An *inline* `/Annots` array lives in a single page dict and cannot be shared
/// by reference, so it needs no dedup.
fn remap_annot_dests<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    result: &RebuildResult,
    surviving: &Surviving,
) -> Result<()> {
    let mut visited_arrays: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut visited_items: BTreeSet<ObjectRef> = BTreeSet::new();
    for &page_ref in &result.new_kids {
        // /Annots may be an inline array (stored in the page dict) or an
        // indirect reference to an array object.
        let page = pdf.get_object_handle(page_ref);
        pdf.resolve(&page)?;
        let Some(annots) = child_if_present(&page, b"/Annots")? else {
            continue;
        };
        if annots.try_as_array()?.is_none() {
            continue;
        }
        if let Some(array_ref) = annots.object_ref() {
            if !visited_arrays.insert(array_ref) {
                continue;
            }
        }
        remap_annot_array(pdf, &annots, surviving, &mut visited_items)?;
    }
    Ok(())
}

/// Process every element of an `/Annots` array for destination remap.
///
/// Each indirect annotation is rewritten through its live handle exactly once
/// across duplicated pages. Direct dictionaries are mutated in place through
/// the containing array handle, so their owner observes the same identity.
fn remap_annot_array<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    arr: &ObjectHandle,
    surviving: &Surviving,
    visited: &mut BTreeSet<ObjectRef>,
) -> Result<()> {
    let Some(items) = arr.try_as_array()? else {
        return Ok(());
    };
    for item in items {
        if let Some(item_ref) = item.object_ref() {
            if !visited.insert(item_ref) {
                continue;
            }
        }
        remap_item_dest(pdf, &item, surviving)?;
    }
    Ok(())
}

/// Remap the catalog `/OpenAction` destination (qpdf `--pages` parity).
/// `/OpenAction` is either a destination array `[page /Fit ...]` or an action
/// dict (possibly indirect). [`remap_action_dest`] handles both: a
/// `/S /GoTo` action's `/D` — or a bare destination array/dict — targeting a
/// surviving page is remapped (a removed target is left verbatim, its page
/// already nulled), while a non-GoTo action is kept verbatim (its `/D` is not a
/// local page destination).
fn remap_open_action_dest<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    catalog: &ObjectHandle,
    surviving: &Surviving,
) -> Result<()> {
    let Some(oa) = child_if_present(catalog, b"/OpenAction")? else {
        return Ok(());
    };
    remap_action_dest(pdf, &oa, surviving)?;
    Ok(())
}

/// Remap a GoTo destination carried by an action value (`/A` or `/OpenAction`).
///
/// Only a `/S /GoTo` action's `/D` is a local page destination, so a non-GoTo
/// action (e.g. `/GoToR`, `/URI`, `/Launch`) is kept verbatim — its `/D`, when
/// present, targets a remote or named destination and must never be mistaken
/// for a local page ref. A bare destination value (an array `[page /Fit]` or a
/// `<< /D … >>` dict with no `/S`) is passed through to [`remap_dest`].
/// This mirrors the `/S /GoTo` check the indirect-annotation path performs in
/// [`remap_item_dest`].
fn remap_action_dest<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    value: &ObjectHandle,
    surviving: &Surviving,
) -> Result<bool> {
    // Resolve to inspect /S without losing the original value form for the
    // write-back (remap_dest handles an indirect value in place).
    if value.try_as_dictionary()?.is_none() {
        return remap_dest(pdf, value, surviving);
    }
    if let Some(action_type) = child_if_present(value, b"/S")? {
        // A non-GoTo action: keep verbatim (its /D is not a local destination).
        if let Some(name) = action_type.try_as_name()? {
            if name != b"GoTo" {
                return Ok(false);
            }
        }
    }
    let Some(dest) = child_if_present(value, b"/D")? else {
        return Ok(false);
    };
    remap_dest(pdf, &dest, surviving)
}

/// Remap only an outline or annotation action explicitly typed as `/GoTo`.
fn remap_goto_action<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    action: &ObjectHandle,
    surviving: &Surviving,
) -> Result<bool> {
    if action.try_as_dictionary()?.is_none() {
        return Ok(false);
    }
    let Some(action_type) = child_if_present(action, b"/S")? else {
        return Ok(false);
    };
    if !matches!(action_type.try_as_name()?, Some(name) if name == b"GoTo") {
        return Ok(false);
    }
    let Some(dest) = child_if_present(action, b"/D")? else {
        return Ok(false);
    };
    remap_dest(pdf, &dest, surviving)
}

// ---------------------------------------------------------------------------
// qpdf null-out: replace every removed original page leaf with `null` in place,
// independent of how it is referenced. Destination remap (surviving pages) is
// handled separately below.
// ---------------------------------------------------------------------------

/// Replace every removed original page leaf with `null` in place (qpdf null-out).
///
/// `result.removed_pages` is the set of original page-tree leaves the rebuild
/// dropped — exactly the objects qpdf's `--pages` nulls (`QPDFJob` enumerates
/// the original page tree and `replaceObject`s each unselected `/Page`). This is
/// page-driven, never destination-driven: a removed page reached only through a
/// reference holder (`[40 0 R]` with `40 0 obj` = `4 0 R`) or a non-page wrapper
/// dictionary (`40 0 obj` = `<< /X 4 0 R >>`) is still severed, so its contents
/// cannot leak; a non-page object a malformed destination happens to reference
/// is never in this set and is left untouched. The subsequent subset sweep
/// drops any nulled page that no surviving destination still references.
fn null_removed_pages<R: Read + Seek>(pdf: &mut Pdf<R>, result: &RebuildResult) -> Result<()> {
    for &removed in &result.removed_pages {
        pdf.replace_object(removed, ObjectHandle::null())?;
    }
    Ok(())
}

/// Remap a `/Names`-leaf name tree (or descend its `/Kids`) in place, keeping
/// every entry. A surviving-page dest is remapped; a removed-page dest is left
/// verbatim (its target page object was already nulled by [`null_removed_pages`]).
/// `/Limits` is never recomputed.
fn remap_name_tree<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    node: &ObjectHandle,
    surviving: &Surviving,
    depth: usize,
    max_depth: usize,
    visited: &mut BTreeSet<ObjectRef>,
) -> Result<()> {
    if depth >= max_depth {
        let node_ref = node
            .object_ref()
            .map_or_else(|| "direct".to_owned(), |object_ref| object_ref.to_string());
        return Err(Error::Unsupported(format!(
            "outline_dest_remap: name-tree depth limit {max_depth} exceeded at {node_ref}"
        )));
    }
    if let Some(node_ref) = node.object_ref() {
        if !visited.insert(node_ref) {
            return Ok(()); // Cycle: already processed.
        }
    }
    if node.try_as_dictionary()?.is_none() {
        return Ok(()); // Malformed node.
    }

    if let Some(pairs) = child_if_present(node, b"/Names")? {
        remap_name_pairs(pdf, &pairs, surviving)?;
        return Ok(());
    }

    if let Some(kids) = child_if_present(node, b"/Kids")? {
        if let Some(items) = kids.try_as_array()? {
            for child in items {
                if child.object_ref().is_some() {
                    remap_name_tree(pdf, &child, surviving, depth + 1, max_depth, visited)?;
                } // cov:ignore: llvm-cov attributes this syntactic closing line to the recursive call span
            }
        }
    }
    Ok(())
}

/// Keep every `(name, dest)` pair of a flat name-pairs array, remapping a
/// surviving-page dest (a removed-page dest is left verbatim, its page already
/// nulled). A trailing odd orphan key is dropped in place.
fn remap_name_pairs<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    pairs: &ObjectHandle,
    surviving: &Surviving,
) -> Result<()> {
    let Some(mut items) = pairs.try_as_array()? else {
        return Ok(());
    };
    if items.len() % 2 != 0 {
        items.pop();
        pairs.set_array_items(items.clone())?;
        pdf.mark_object_handle_dirty(pairs)?;
    }
    for dest in items.iter().skip(1).step_by(2) {
        remap_dest(pdf, dest, surviving)?;
    }
    Ok(())
}

/// Remap a single destination through its live handle. A removed-page target
/// needs no action because [`null_removed_pages`] already replaced that page
/// object. Non-page and named destinations stay unchanged.
fn remap_dest<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dest: &ObjectHandle,
    surviving: &Surviving,
) -> Result<bool> {
    remap_dest_depth(pdf, dest, surviving, MAX_DEST_RESOLVE_DEPTH)
}

const MAX_DEST_RESOLVE_DEPTH: usize = 64;

fn remap_dest_depth<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dest: &ObjectHandle,
    surviving: &Surviving,
    depth: usize,
) -> Result<bool> {
    if depth == 0 {
        return Ok(false);
    }
    if let Some(items) = dest.try_as_array()? {
        let Some(first) = items.first() else {
            return Ok(false);
        };
        let Some(old_ref) = first.object_ref() else {
            return Ok(false);
        };
        let Some(new_ref) = surviving.remap(old_ref) else {
            return Ok(false);
        };
        if new_ref == old_ref || !surviving.is_surviving_target(old_ref) {
            return Ok(false);
        }
        dest.set_array_item(0, pdf.get_object_handle(new_ref))?;
        pdf.mark_object_handle_dirty(dest)?;
        return Ok(true);
    }
    if dest.try_as_dictionary()?.is_some() {
        let Some(value) = child_if_present(dest, b"/D")? else {
            return Ok(false);
        };
        return remap_dest_depth(pdf, &value, surviving, depth - 1);
    }
    Ok(false)
}

/// Keep every entry of a legacy `/Dests` dictionary, remapping surviving-page
/// dests (a removed-page dest is left verbatim, its page already nulled).
/// Returns the rebuilt dictionary.
fn remap_dests_dict<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dests: &ObjectHandle,
    surviving: &Surviving,
) -> Result<()> {
    for key in dests.try_get_keys()? {
        let value = dests.try_get_key(&key)?;
        remap_dest(pdf, &value, surviving)?;
    }
    Ok(())
}

/// Walk the outline sibling chain from `first_ref`, recursing into children,
/// keeping every item: remap each item's `/Dest` and `/A /GoTo /D` to its
/// surviving target's new ref (a removed target is left verbatim, its page
/// already nulled). Sibling links and `/Count` are left unchanged. Bounded by
/// `depth`/`max_depth` and a shared `visited` set (hostile-PDF guards).
fn remap_outline_tree<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    first: ObjectHandle,
    depth: usize,
    max_depth: usize,
    surviving: &Surviving,
    visited: &mut BTreeSet<ObjectRef>,
) -> Result<()> {
    if depth >= max_depth {
        let first_ref = first
            .object_ref()
            .map_or_else(|| "direct".to_owned(), |object_ref| object_ref.to_string());
        return Err(Error::Unsupported(format!(
            "outline_dest_remap: depth limit {max_depth} exceeded at {first_ref}"
        )));
    }
    let mut current = Some(first);
    while let Some(item) = current {
        if let Some(item_ref) = item.object_ref() {
            if !visited.insert(item_ref) {
                break; // Cycle guard (/Next or /First back-edge).
            }
        }
        if item.try_as_dictionary()?.is_none() {
            break; // Malformed — stop this chain.
        }
        let next = indirect_child(&item, b"/Next")?;
        let first_child = indirect_child(&item, b"/First")?;

        // Remap surviving-page refs in place. Removed target pages need no
        // action here — they were already replaced with `null` by
        // [`null_removed_pages`] (the destination reference is kept verbatim).
        remap_item_dest(pdf, &item, surviving)?;

        if let Some(child_first) = first_child {
            remap_outline_tree(pdf, child_first, depth + 1, max_depth, surviving, visited)?;
        }
        current = next;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Destination resolution helpers
// ---------------------------------------------------------------------------

/// Remap the page reference in an outline item's `/Dest` or `/A /D` field.
fn remap_item_dest<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    item: &ObjectHandle,
    surviving: &Surviving,
) -> Result<()> {
    if item.try_as_dictionary()?.is_none() {
        return Ok(());
    }

    // /Dest — array, dict, or an indirect reference to either.
    if let Some(dest) = child_if_present(item, b"/Dest")? {
        remap_dest(pdf, &dest, surviving)?;
        // String/name-form dest: no page ref to remap here; the name tree was
        // already updated.
    }

    // /A /GoTo /D (action form). /A may be an indirect reference to the
    // action dict; resolve one level so an indirect GoTo action's /D is
    // still pruned/remapped.
    if let Some(action) = child_if_present(item, b"/A")? {
        remap_goto_action(pdf, &action, surviving)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
