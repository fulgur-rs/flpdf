//! Outline and named-destination remapping after page extraction.
//!
//! After [`crate::page_tree_rebuild::rebuild_page_tree`] has rebuilt the page tree
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
//!   that `null`. The subsequent subset sweep ([`crate::subset_prune`]) keeps
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

use crate::page_tree_rebuild::RebuildResult;
use crate::ref_chain::resolve_ref_chain;
use crate::{Error, Object, ObjectRef, Pdf, Result};
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

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Null removed pages and remap surviving-page destinations after a page-tree
/// rebuild (qpdf `--pages` parity).
///
/// `result` is the [`RebuildResult`] returned by
/// [`crate::page_tree_rebuild::rebuild_page_tree`]. Its `ref_map` encodes the
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
    null_removed_pages(pdf, result);

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
    let catalog_obj = pdf.resolve_borrowed(catalog_ref)?;
    let Some(catalog) = catalog_obj.as_dict() else {
        return Ok(());
    };

    let outlines_ref = catalog.get_ref("Outlines");

    // --- Step 2: Remap named destinations -------------------------------------
    // qpdf keeps every named destination: a surviving-page dest is remapped to
    // its new page ref; a removed-page dest is left verbatim (its target page
    // object was already nulled in Step 0, and an unreferenced removed page is
    // then garbage-collected by the later subset sweep). /Names and /Dests are
    // never removed from the catalog, and /Limits is never recomputed.

    // /Names may be an indirect reference OR a direct dictionary on the catalog;
    // /Dests inside it likewise.
    enum NamesLoc {
        Indirect(ObjectRef),
        DirectInCatalog,
    }
    let (names_loc, mut names_dict) = match catalog.get("Names").cloned() {
        Some(Object::Reference(r)) => match pdf.resolve_borrowed(r)? {
            Object::Dictionary(d) => (Some(NamesLoc::Indirect(r)), d.clone()),
            _ => (None, crate::Dictionary::default()),
        },
        Some(Object::Dictionary(d)) => (Some(NamesLoc::DirectInCatalog), d),
        _ => (None, crate::Dictionary::default()),
    };
    if let Some(names_loc) = names_loc {
        match names_dict.get("Dests").cloned() {
            Some(Object::Reference(dr)) => {
                // /Dests is an indirect name-tree root: nodes are remapped in
                // place, so the /Names holder needs no rewrite.
                let mut nt_visited: BTreeSet<ObjectRef> = BTreeSet::new();
                remap_name_tree(pdf, dr, &surviving, 0, max_depth, &mut nt_visited)?;
            }
            Some(Object::Dictionary(node)) => {
                // /Dests held as a direct dict: rebuild it and write the holder
                // (the indirect /Names object, or the catalog) back.
                let new_node = remap_name_tree_node_dict(pdf, node, &surviving, max_depth)?;
                names_dict.insert("Dests", Object::Dictionary(new_node));
                match names_loc {
                    NamesLoc::Indirect(r) => pdf.set_object(r, Object::Dictionary(names_dict)),
                    NamesLoc::DirectInCatalog => {
                        let cat_obj = pdf.resolve_borrowed(catalog_ref)?;
                        if let Some(mut cat) = cat_obj.as_dict().cloned() {
                            cat.insert("Names", Object::Dictionary(names_dict));
                            pdf.set_object(catalog_ref, Object::Dictionary(cat));
                        }
                    }
                }
            }
            // No /Dests (other name-tree keys only) — nothing to remap here.
            _ => {}
        }
    }

    // 2b. Legacy /Catalog /Dests dictionary (PDF 1.1 style)
    let catalog_obj2 = pdf.resolve_borrowed(catalog_ref)?;
    let Some(catalog2) = catalog_obj2.as_dict() else {
        return Ok(());
    };
    match catalog2.get("Dests").cloned() {
        Some(Object::Reference(dests_obj_ref)) => {
            remap_legacy_dests(pdf, dests_obj_ref, &surviving)?;
        }
        Some(Object::Dictionary(dests)) => {
            // Legacy /Dests held as a direct dictionary on the catalog.
            let new_dests = remap_dests_dict(pdf, dests, &surviving)?;
            let catalog_obj3 = pdf.resolve_borrowed(catalog_ref)?;
            if let Some(mut cat) = catalog_obj3.as_dict().cloned() {
                cat.insert("Dests", Object::Dictionary(new_dests));
                pdf.set_object(catalog_ref, Object::Dictionary(cat));
            }
        }
        _ => {}
    }

    // --- Step 3: Remap the outline tree -----------------------------------
    // Every outline item is kept; only its destination page ref is remapped when
    // the target page survived (a removed target was already nulled in Step 0 and
    // is left referenced verbatim). Sibling links, /Count, and the /Outlines
    // catalog entry are all left unchanged.
    if let Some(outlines_obj_ref) = outlines_ref {
        let first_ref = {
            let outline_root_obj = pdf.resolve_borrowed(outlines_obj_ref)?;
            outline_root_obj.as_dict().and_then(|d| d.get_ref("First"))
        };
        if let Some(first_ref) = first_ref {
            let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
            remap_outline_tree(pdf, first_ref, 0, max_depth, &surviving, &mut visited)?;
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
    remap_open_action_dest(pdf, catalog_ref, &surviving)?;

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
/// already-remapped `/Dest` is a no-op (`remap_dest_value` returns `None`) rather
/// than being remapped a second time.
/// An *inline* `/Annots` array lives in a single page dict and cannot be shared
/// by reference, so it needs no dedup.
fn remap_annot_dests<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    result: &RebuildResult,
    surviving: &Surviving,
) -> Result<()> {
    let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
    for &page_ref in &result.new_kids {
        // /Annots may be an inline array (stored in the page dict) or an
        // indirect reference to an array object.
        let annots_val = {
            let page_obj = pdf.resolve_borrowed(page_ref)?;
            let Some(page) = page_obj.as_dict() else {
                continue;
            };
            page.get("Annots").cloned()
        };
        match annots_val {
            // Inline /Annots array: process elements, write the array back into
            // the page dict only if an inline annotation changed.
            Some(Object::Array(arr)) => {
                if let Some(new_arr) = remap_annot_array(pdf, arr, surviving, &mut visited)? {
                    let page_obj = pdf.resolve_borrowed(page_ref)?;
                    if let Some(mut page) = page_obj.as_dict().cloned() {
                        page.insert("Annots", Object::Array(new_arr));
                        pdf.set_object(page_ref, Object::Dictionary(page));
                    }
                }
            }
            // Indirect /Annots array: process elements, write the array back to
            // the array object only if an inline annotation changed. A shared
            // indirect array (duplicate-page selection) is processed once: a
            // second pass would re-remap an inline annot's already-remapped /Dest
            // (redundant work). Nulling a surviving page on that second pass is
            // independently prevented by the surviving-target guard (a rebuilt
            // output ref is never treated as a removed target).
            Some(Object::Reference(r)) => {
                // Follow the full reference chain (ref -> … -> array) so a
                // double-indirect /Annots is not dropped, then dedup on the
                // terminal array object so a shared array is processed once.
                let (resolved, terminal) = resolve_ref_chain(pdf, &Object::Reference(r))?;
                let array_ref = terminal.unwrap_or(r);
                if !visited.insert(array_ref) {
                    continue;
                }
                let Object::Array(arr) = resolved else {
                    continue;
                };
                if let Some(new_arr) = remap_annot_array(pdf, arr, surviving, &mut visited)? {
                    pdf.set_object(array_ref, Object::Array(new_arr));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Process every element of an `/Annots` array for destination remap.
///
/// An indirect annotation (`Object::Reference`) is rewritten in place by the
/// shared item helpers (deduplicated across duplicated pages via `visited`); an
/// inline (direct-dict) annotation is remapped on a copy. Returns
/// `Some(updated_array)` when an inline annotation changed — the caller stores
/// it back into the page dict or the indirect array object — or `None` when
/// only indirect annotations were touched (already rewritten in place) or
/// nothing changed.
fn remap_annot_array<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    arr: Vec<Object>,
    surviving: &Surviving,
    visited: &mut BTreeSet<ObjectRef>,
) -> Result<Option<Vec<Object>>> {
    let mut out: Vec<Object> = Vec::with_capacity(arr.len());
    let mut changed = false;
    for elem in arr {
        match elem {
            Object::Reference(r) => {
                // A shared indirect annot reachable from several duplicated
                // pages must be processed once (see remap_annot_dests).
                if visited.insert(r) {
                    remap_item_dest(pdf, r, surviving)?;
                }
                out.push(Object::Reference(r));
            }
            Object::Dictionary(annot) => {
                let (new_annot, ch) = remap_inline_annot(pdf, annot, surviving)?;
                changed |= ch;
                out.push(Object::Dictionary(new_annot));
            }
            other => out.push(other),
        }
    }
    Ok(if changed { Some(out) } else { None })
}

/// Remap an inline (direct-dict) annotation's destinations.
///
/// An inline annotation has no object identity, so its `/Dest` and `/A` are
/// handled directly: a surviving target is remapped, a removed target is left
/// verbatim (its page object was already nulled by [`null_removed_pages`]).
/// `/Dest` is a destination value handled by [`remap_dest`]; `/A` is an action,
/// processed by [`remap_action_dest`] so that only a `/S /GoTo` action's `/D` is
/// treated as a local page destination. Returns the (possibly updated) dict and
/// whether a destination key was present and processed.
fn remap_inline_annot<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    mut annot: crate::Dictionary,
    surviving: &Surviving,
) -> Result<(crate::Dictionary, bool)> {
    let mut changed = false;
    // `annot` is owned: take each value by `remove` and re-insert the processed
    // result (no inner clone of the destination).
    if let Some(dest) = annot.remove("Dest") {
        annot.insert("Dest", remap_dest(pdf, dest, surviving)?);
        changed = true;
    }
    if let Some(action) = annot.remove("A") {
        annot.insert("A", remap_action_dest(pdf, action, surviving)?);
        changed = true;
    }
    Ok((annot, changed))
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
    catalog_ref: ObjectRef,
    surviving: &Surviving,
) -> Result<()> {
    let oa = {
        let catalog_obj = pdf.resolve_borrowed(catalog_ref)?;
        let Some(catalog) = catalog_obj.as_dict() else {
            return Ok(());
        };
        catalog.get("OpenAction").cloned()
    };
    let Some(oa) = oa else {
        return Ok(());
    };
    // For an indirect /OpenAction the referenced object is rewritten in place
    // and the same value returned; for a direct value a remapped destination
    // comes back changed. Only rewrite the catalog when the value actually
    // changed, so an unchanged (or in-place-updated indirect) /OpenAction does
    // not needlessly mark the catalog dirty.
    let updated = remap_action_dest(pdf, oa.clone(), surviving)?;
    if updated != oa {
        let catalog_obj = pdf.resolve_borrowed(catalog_ref)?;
        if let Some(mut catalog) = catalog_obj.as_dict().cloned() {
            catalog.insert("OpenAction", updated);
            pdf.set_object(catalog_ref, Object::Dictionary(catalog));
        }
    }
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
    value: Object,
    surviving: &Surviving,
) -> Result<Object> {
    // Resolve to inspect /S without losing the original value form for the
    // write-back (remap_dest handles an indirect value in place).
    let (concrete, _) = resolve_ref_chain(pdf, &value)?;
    if let Some(dict) = concrete.as_dict() {
        // A non-GoTo action: keep verbatim (its /D is not a local destination).
        if matches!(dict.get("S"), Some(Object::Name(n)) if n != b"GoTo") {
            return Ok(value);
        }
    }
    remap_dest(pdf, value, surviving)
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
fn null_removed_pages<R: Read + Seek>(pdf: &mut Pdf<R>, result: &RebuildResult) {
    for &removed in &result.removed_pages {
        pdf.set_object(removed, Object::Null);
    }
}

/// Remap a `/Names`-leaf name tree (or descend its `/Kids`) in place, keeping
/// every entry. A surviving-page dest is remapped; a removed-page dest is left
/// verbatim (its target page object was already nulled by [`null_removed_pages`]).
/// `/Limits` is never recomputed.
fn remap_name_tree<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    node_ref: ObjectRef,
    surviving: &Surviving,
    depth: usize,
    max_depth: usize,
    visited: &mut BTreeSet<ObjectRef>,
) -> Result<()> {
    if depth >= max_depth {
        return Err(Error::Unsupported(format!(
            "outline_dest_remap: name-tree depth limit {max_depth} exceeded at {node_ref}"
        )));
    }
    if !visited.insert(node_ref) {
        return Ok(()); // Cycle: already processed.
    }
    let node_obj = pdf.resolve_borrowed(node_ref)?;
    let Some(node) = node_obj.as_dict() else {
        return Ok(()); // Malformed node.
    };

    if node.get("Names").is_some() {
        if let Some(pairs) = node.get("Names").cloned().and_then(Object::into_array) {
            let new_pairs = remap_name_pairs(pdf, pairs, surviving)?;
            let node_obj2 = pdf.resolve_borrowed(node_ref)?;
            if let Some(mut d) = node_obj2.as_dict().cloned() {
                d.insert("Names", Object::Array(new_pairs));
                pdf.set_object(node_ref, Object::Dictionary(d));
            }
        }
        return Ok(());
    }

    if let Some(kids) = node.get("Kids").and_then(Object::as_array) {
        let child_refs: Vec<ObjectRef> = kids.iter().filter_map(Object::as_ref_id).collect();
        for child_ref in child_refs {
            remap_name_tree(pdf, child_ref, surviving, depth + 1, max_depth, visited)?;
        }
    }
    Ok(())
}

/// Like [`remap_name_tree`] but for a name-tree root held as a *direct*
/// dictionary on the catalog's `/Names`. Child `/Kids` (always indirect)
/// delegate to [`remap_name_tree`]. Returns the rebuilt node dict.
fn remap_name_tree_node_dict<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    node: crate::Dictionary,
    surviving: &Surviving,
    max_depth: usize,
) -> Result<crate::Dictionary> {
    if let Some(Object::Array(pairs)) = node.get("Names").cloned() {
        let new_pairs = remap_name_pairs(pdf, pairs, surviving)?;
        let mut d = node;
        d.insert("Names", Object::Array(new_pairs));
        return Ok(d);
    }
    if let Some(kids) = node.get("Kids").and_then(Object::as_array) {
        let child_refs: Vec<ObjectRef> = kids.iter().filter_map(Object::as_ref_id).collect();
        let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
        for child_ref in child_refs {
            remap_name_tree(pdf, child_ref, surviving, 1, max_depth, &mut visited)?;
        }
    }
    Ok(node)
}

/// Keep every `(name, dest)` pair of a flat name-pairs array, remapping a
/// surviving-page dest (a removed-page dest is left verbatim, its page already
/// nulled). Returns the rebuilt array (same order as the input; a trailing odd
/// orphan key is dropped).
fn remap_name_pairs<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    pairs: Vec<Object>,
    surviving: &Surviving,
) -> Result<Vec<Object>> {
    let mut result: Vec<Object> = Vec::with_capacity(pairs.len());
    let mut i = 0;
    while i + 1 < pairs.len() {
        let name_obj = pairs[i].clone();
        let dest_obj = pairs[i + 1].clone();
        i += 2;
        result.push(name_obj);
        result.push(remap_dest(pdf, dest_obj, surviving)?);
    }
    Ok(result)
}

/// Remap a single dest value to its surviving target's new ref, or keep it
/// verbatim. Returns the dest value to store back. Indirect dests are rewritten
/// in place by [`remap_dest_value`], so the original value is returned unchanged.
///
/// A removed-page target needs no action here: the page object was already
/// replaced with `null` by [`null_removed_pages`], so the destination simply
/// resolves to that `null`. A non-page or named/external target likewise stays
/// verbatim.
fn remap_dest<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dest_obj: Object,
    surviving: &Surviving,
) -> Result<Object> {
    match dest_page_ref_resolved(pdf, &dest_obj)? {
        // A surviving source ref is remapped to its new ref; a ref that is
        // already a rebuilt output ref stays verbatim (remap is a no-op, so
        // `remap_dest_value` returns `None`).
        Some(page_ref) if surviving.is_surviving_target(page_ref) => {
            Ok(remap_dest_value(pdf, &dest_obj, surviving)?.unwrap_or(dest_obj))
        }
        // Removed page (already nulled), non-page object, or named/external
        // dest — keep the destination value verbatim.
        _ => Ok(dest_obj),
    }
}

/// Remap a legacy (PDF 1.1) `/Dests` dictionary held as an indirect object.
fn remap_legacy_dests<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dests_ref: ObjectRef,
    surviving: &Surviving,
) -> Result<()> {
    let dests_obj = pdf.resolve_borrowed(dests_ref)?;
    let Some(dests) = dests_obj.as_dict().cloned() else {
        return Ok(());
    };
    let new_dests = remap_dests_dict(pdf, dests, surviving)?;
    pdf.set_object(dests_ref, Object::Dictionary(new_dests));
    Ok(())
}

/// Keep every entry of a legacy `/Dests` dictionary, remapping surviving-page
/// dests (a removed-page dest is left verbatim, its page already nulled).
/// Returns the rebuilt dictionary.
fn remap_dests_dict<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dests: crate::Dictionary,
    surviving: &Surviving,
) -> Result<crate::Dictionary> {
    let mut new_dests = dests.clone();
    let keys: Vec<Vec<u8>> = dests.iter().map(|(k, _)| k.to_vec()).collect();
    for key in keys {
        let Some(val) = dests.get(&key).cloned() else {
            continue;
        };
        let updated = remap_dest(pdf, val, surviving)?;
        new_dests.insert(key, updated);
    }
    Ok(new_dests)
}

/// Walk the outline sibling chain from `first_ref`, recursing into children,
/// keeping every item: remap each item's `/Dest` and `/A /GoTo /D` to its
/// surviving target's new ref (a removed target is left verbatim, its page
/// already nulled). Sibling links and `/Count` are left unchanged. Bounded by
/// `depth`/`max_depth` and a shared `visited` set (hostile-PDF guards).
fn remap_outline_tree<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    first_ref: ObjectRef,
    depth: usize,
    max_depth: usize,
    surviving: &Surviving,
    visited: &mut BTreeSet<ObjectRef>,
) -> Result<()> {
    if depth >= max_depth {
        return Err(Error::Unsupported(format!(
            "outline_dest_remap: depth limit {max_depth} exceeded at {first_ref}"
        )));
    }
    let mut current = Some(first_ref);
    while let Some(item_ref) = current {
        if !visited.insert(item_ref) {
            break; // Cycle guard (/Next or /First back-edge).
        }
        let (next_ref, first_child) = {
            let item_obj = pdf.resolve_borrowed(item_ref)?;
            let Some(item) = item_obj.as_dict() else {
                break; // Malformed — stop this chain.
            };
            (item.get_ref("Next"), item.get_ref("First"))
        };

        // Remap surviving-page refs in place. Removed target pages need no
        // action here — they were already replaced with `null` by
        // [`null_removed_pages`] (the destination reference is kept verbatim).
        remap_item_dest(pdf, item_ref, surviving)?;

        if let Some(child_first) = first_child {
            remap_outline_tree(pdf, child_first, depth + 1, max_depth, surviving, visited)?;
        }
        current = next_ref;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Destination resolution helpers
// ---------------------------------------------------------------------------

/// Remap the page reference in an outline item's `/Dest` or `/A /D` field.
fn remap_item_dest<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    item_ref: ObjectRef,
    surviving: &Surviving,
) -> Result<()> {
    let item_obj = pdf.resolve_borrowed(item_ref)?;
    let Some(mut d) = item_obj.as_dict().cloned() else {
        return Ok(());
    };

    let mut changed = false;

    // /Dest — array, dict, or an indirect reference to either.
    if let Some(dest) = d.get("Dest").cloned() {
        if let Some(remapped) = remap_dest_value(pdf, &dest, surviving)? {
            d.insert("Dest", remapped);
            changed = true;
        }
        // String/name-form dest: no page ref to remap here; the name tree was
        // already updated.
    }

    // /A /GoTo /D (action form). /A may be an indirect reference to the
    // action dict; resolve one level so an indirect GoTo action's /D is
    // still pruned/remapped.
    if let Some(a_val) = d.get("A").cloned() {
        // /A may be a multi-level indirect chain; follow it to the terminal
        // action object. action_target is the LAST ref in the chain so an
        // in-place rewrite updates the object /A ultimately points at.
        let (action_obj, action_target) = resolve_ref_chain(pdf, &a_val)?;
        if let Some(mut action) = action_obj.into_dict() {
            let is_goto = matches!(action.get("S"), Some(Object::Name(n)) if n == b"GoTo");
            if is_goto {
                if let Some(d_val) = action.get("D").cloned() {
                    if let Some(remapped) = remap_dest_value(pdf, &d_val, surviving)? {
                        action.insert("D", remapped);
                        match action_target {
                            Some(ar) => {
                                // Rewrite the referenced action object in place;
                                // /A keeps pointing at the same object number.
                                pdf.set_object(ar, Object::Dictionary(action));
                            }
                            None => {
                                d.insert("A", Object::Dictionary(action));
                                changed = true;
                            }
                        }
                    }
                }
            }
        }
    }

    if changed {
        pdf.set_object(item_ref, Object::Dictionary(d));
    }
    Ok(())
}

/// Remap a `/Dest` or `/A /D` value to its surviving page ref.
///
/// Returns `Some(new_value)` to store back in the owning dict when a change is
/// needed, or `None` when nothing should change (page absent from `surviving`,
/// or no resolvable page ref). For an indirect destination the referenced
/// object is rewritten in place and `None` is returned (the owning dict keeps
/// pointing at the same object number).
fn remap_dest_value<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dest: &Object,
    surviving: &Surviving,
) -> Result<Option<Object>> {
    remap_dest_value_depth(pdf, dest, surviving, MAX_DEST_RESOLVE_DEPTH)
}

/// Bound on indirection/`/D` nesting followed when resolving a destination.
/// Real dests nest 1–2 levels; this only exists to make a malformed or
/// hostile cyclic structure (e.g. `40 0 obj << /D 40 0 R >>`) terminate
/// instead of overflowing the stack.
const MAX_DEST_RESOLVE_DEPTH: usize = 64;

fn remap_dest_value_depth<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dest: &Object,
    surviving: &Surviving,
    depth: usize,
) -> Result<Option<Object>> {
    if depth == 0 {
        // Cycle / pathological nesting — stop conservatively (no remap).
        return Ok(None);
    }
    match dest {
        // Indirect: resolve, recurse, and if the referenced object changed,
        // rewrite it in place. The caller keeps pointing at the same ref.
        Object::Reference(dr) => {
            let concrete = pdf.resolve(*dr)?;
            if let Some(updated) = remap_dest_value_depth(pdf, &concrete, surviving, depth - 1)? {
                pdf.set_object(*dr, updated);
            }
            Ok(None)
        }
        // Array form `[pageRef /Fit ...]`.
        Object::Array(arr) => {
            if let Some(old) = arr.first().and_then(Object::as_ref_id) {
                if let Some(new_ref) = surviving.remap(old) {
                    let mut a = arr.clone();
                    a[0] = Object::Reference(new_ref);
                    return Ok(Some(Object::Array(a)));
                }
            }
            Ok(None)
        }
        // Dictionary form `<< /D <dest> >>` — /D may itself be inline or an
        // indirect reference; recurse so either is remapped.
        Object::Dictionary(d) => {
            if let Some(d_val) = d.get("D").cloned() {
                if let Some(updated) = remap_dest_value_depth(pdf, &d_val, surviving, depth - 1)? {
                    let mut nd = d.clone();
                    nd.insert("D", updated);
                    return Ok(Some(Object::Dictionary(nd)));
                }
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

/// pdf-aware page-ref extraction. Resolves one level at each indirection
/// (the dest value itself, or a dictionary's `/D`) so every indirection form
/// — inline array, dict `/D`, indirect dest, dict whose `/D` is indirect — is
/// classified uniformly. Returns `None` for named/string/external dests, or
/// when a cyclic/over-deep structure is hit (handled conservatively).
pub(crate) fn dest_page_ref_resolved<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dest: &Object,
) -> Result<Option<ObjectRef>> {
    dest_page_ref_resolved_depth(pdf, dest, MAX_DEST_RESOLVE_DEPTH)
}

fn dest_page_ref_resolved_depth<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dest: &Object,
    depth: usize,
) -> Result<Option<ObjectRef>> {
    if depth == 0 {
        // Cycle / pathological nesting — treat as no resolvable page ref so
        // the entry is kept conservatively rather than overflowing the stack.
        return Ok(None);
    }
    match dest {
        Object::Reference(r) => {
            let c = pdf.resolve_borrowed(*r)?.clone();
            dest_page_ref_resolved_depth(pdf, &c, depth - 1)
        }
        Object::Array(arr) => Ok(match arr.first() {
            Some(Object::Reference(r)) => Some(*r),
            _ => None,
        }),
        Object::Dictionary(d) => match d.get("D").cloned() {
            Some(v) => dest_page_ref_resolved_depth(pdf, &v, depth - 1),
            None => Ok(None),
        },
        _ => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::check_reader;
    use crate::page_tree_rebuild::rebuild_page_tree;
    use crate::writer::write_pdf;
    use crate::{Object, ObjectRef, Pdf};
    use std::collections::BTreeMap;
    use std::io::Cursor;

    // -----------------------------------------------------------------------
    // Test PDF builders
    // -----------------------------------------------------------------------

    /// Build a 4-page PDF with:
    ///
    /// - Pages: 3 0 R (p1), 4 0 R (p2), 5 0 R (p3), 6 0 R (p4)
    /// - Outline items:
    ///   - 20 0 R "Page 1" → /Dest [3 0 R /XYZ 0 792 0]   (array dest, page 1)
    ///   - 21 0 R "Page 2" → /A /GoTo /D [4 0 R /XYZ 0 792 0]  (action dest, page 2)
    ///   - 22 0 R "Page 3" → /Dest [5 0 R /Fit], has child 24 0 R  (parent item)
    ///   - 23 0 R "Page 4" → /Dest (dest_named_p4)         (string dest, page 4)
    ///   - 24 0 R "Page 3 sub" → /Dest [5 0 R /XYZ 0 400 0]  (child of 22)
    /// - Named dests (/Names/Dests name tree at 30 0 R):
    ///   dest_p1 → [3 0 R /XYZ 0 792 0]
    ///   dest_p2 → [4 0 R /XYZ 0 792 0]
    ///   dest_p3 → [5 0 R /XYZ 0 792 0]
    ///   dest_named_p4 → [6 0 R /XYZ 0 792 0]
    fn build_outline_pdf() -> Vec<u8> {
        let mut raw: Vec<u8> = b"%PDF-1.5\n".to_vec();
        let mut offs: BTreeMap<u32, usize> = BTreeMap::new();

        let add = |raw: &mut Vec<u8>, offs: &mut BTreeMap<u32, usize>, num: u32, content: &str| {
            offs.insert(num, raw.len());
            raw.extend_from_slice(format!("{num} 0 obj\n{content}\nendobj\n").as_bytes());
        };

        // Catalog: /Outlines 10 0 R, /Names 11 0 R
        add(
            &mut raw,
            &mut offs,
            1,
            "<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R /Names 11 0 R >>",
        );
        // Pages root
        add(
            &mut raw,
            &mut offs,
            2,
            "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R 6 0 R] /Count 4 >>",
        );
        // Pages
        add(
            &mut raw,
            &mut offs,
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
        );
        add(
            &mut raw,
            &mut offs,
            4,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
        );
        add(
            &mut raw,
            &mut offs,
            5,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
        );
        add(
            &mut raw,
            &mut offs,
            6,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
        );
        // Outline root
        add(
            &mut raw,
            &mut offs,
            10,
            "<< /Type /Outlines /First 20 0 R /Last 23 0 R /Count 5 >>",
        );
        // Names dict
        add(&mut raw, &mut offs, 11, "<< /Dests 30 0 R >>");
        // Name tree (leaf): 4 named dests
        add(
            &mut raw,
            &mut offs,
            30,
            "<< /Limits [(dest_named_p4) (dest_p3)] \
             /Names [(dest_named_p4) [6 0 R /XYZ 0 792 0] \
                     (dest_p1) [3 0 R /XYZ 0 792 0] \
                     (dest_p2) [4 0 R /XYZ 0 792 0] \
                     (dest_p3) [5 0 R /XYZ 0 792 0]] >>",
        );
        // Outline items
        add(
            &mut raw,
            &mut offs,
            20,
            "<< /Title (Page 1) /Parent 10 0 R /Next 21 0 R /Dest [3 0 R /XYZ 0 792 0] >>",
        );
        add(
            &mut raw,
            &mut offs,
            21,
            "<< /Title (Page 2) /Parent 10 0 R /Prev 20 0 R /Next 22 0 R \
             /A << /S /GoTo /D [4 0 R /XYZ 0 792 0] >> >>",
        );
        add(
            &mut raw,
            &mut offs,
            22,
            "<< /Title (Page 3) /Parent 10 0 R /Prev 21 0 R /Next 23 0 R \
             /Dest [5 0 R /Fit] /First 24 0 R /Last 24 0 R /Count 1 >>",
        );
        add(
            &mut raw,
            &mut offs,
            23,
            "<< /Title (Page 4) /Parent 10 0 R /Prev 22 0 R /Dest (dest_named_p4) >>",
        );
        add(
            &mut raw,
            &mut offs,
            24,
            "<< /Title (Page 3 sub) /Parent 22 0 R /Dest [5 0 R /XYZ 0 400 0] >>",
        );

        // xref
        let all_nums: Vec<u32> = offs.keys().cloned().collect();
        let max_num = *all_nums.iter().max().unwrap_or(&0);
        let xref_pos = raw.len();
        raw.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", max_num + 1).as_bytes());
        for i in 1..=max_num {
            if let Some(&off) = offs.get(&i) {
                raw.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
            } else {
                raw.extend_from_slice(b"0000000000 65535 f \n");
            }
        }
        raw.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
                max_num + 1,
                xref_pos
            )
            .as_bytes(),
        );
        raw
    }

    fn open(bytes: Vec<u8>) -> Pdf<Cursor<Vec<u8>>> {
        Pdf::open(Cursor::new(bytes)).expect("PDF should parse")
    }

    fn dict_of(pdf: &mut Pdf<Cursor<Vec<u8>>>, r: ObjectRef) -> crate::Dictionary {
        match pdf.resolve(r).unwrap() {
            Object::Dictionary(d) => d,
            other => panic!("{r} is not a dictionary: {other:?}"),
        }
    }

    fn get_ref(d: &crate::Dictionary, key: &str) -> Option<ObjectRef> {
        d.get_ref(key)
    }

    /// Build a synthetic [`RebuildResult`] for a test PDF whose page tree is
    /// still intact (no real rebuild ran). `removed_pages` is every original
    /// page-tree leaf (`pages::page_refs`) that is not a surviving target —
    /// i.e. neither a `ref_map` key nor a `new_kids` member, mirroring
    /// [`Surviving::is_surviving_target`]. (In a real rebuild every `new_kids`
    /// member is also a `ref_map` key, so this equals `page_refs − ref_map.keys()`;
    /// the extra `new_kids` exclusion only matters for hand-built results whose
    /// surviving output ref is not itself a `ref_map` key.)
    fn synthetic_result(
        pdf: &mut Pdf<Cursor<Vec<u8>>>,
        new_kids: Vec<ObjectRef>,
        ref_map: BTreeMap<ObjectRef, Vec<ObjectRef>>,
    ) -> RebuildResult {
        let new_set: BTreeSet<ObjectRef> = new_kids.iter().copied().collect();
        let removed_pages = crate::pages::page_refs(pdf)
            .unwrap()
            .into_iter()
            .filter(|p| !ref_map.contains_key(p) && !new_set.contains(p))
            .collect();
        RebuildResult {
            new_kids,
            ref_map,
            removed_pages,
        }
    }

    // -----------------------------------------------------------------------
    // Test: all pages survive → pure remap
    // -----------------------------------------------------------------------

    #[test]
    fn all_pages_survive_remap_only() {
        // Keep all 4 pages. rebuild_page_tree ref_map: old → same (flat tree).
        let mut pdf = open(build_outline_pdf());
        let pages = vec![
            ObjectRef::new(3, 0),
            ObjectRef::new(4, 0),
            ObjectRef::new(5, 0),
            ObjectRef::new(6, 0),
        ];
        let result = rebuild_page_tree(&mut pdf, &pages).unwrap();
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        // Catalog still has /Outlines.
        let cat = dict_of(&mut pdf, ObjectRef::new(1, 0));
        assert!(
            cat.get_ref("Outlines").is_some(),
            "catalog should still have /Outlines"
        );

        // Item 20 dest remapped to the new ref for page 3.
        let new_p1 = result.ref_map[&ObjectRef::new(3, 0)][0];
        let item20 = dict_of(&mut pdf, ObjectRef::new(20, 0));
        assert_eq!(
            item20.get("Dest"),
            Some(&Object::Array(vec![
                Object::Reference(new_p1),
                Object::Name(b"XYZ".to_vec()),
                Object::Integer(0),
                Object::Integer(792),
                Object::Integer(0),
            ])),
            "item 20 dest should use new page ref"
        );
    }

    // -----------------------------------------------------------------------
    // Test: some pages removed — outline items KEPT, links unchanged (null-out)
    // -----------------------------------------------------------------------

    #[test]
    fn removed_pages_keep_items_and_links() {
        // Keep pages 1 and 3 (objects 3 and 5). Remove pages 2 and 4.
        // qpdf null-out: no item is dropped or stitched; the full sibling chain
        // 20 -> 21 -> 22 -> 23 stays intact and removed targets (obj4, obj6) are
        // nulled in place.
        let mut pdf = open(build_outline_pdf());
        let pages = vec![ObjectRef::new(3, 0), ObjectRef::new(5, 0)];
        let result = rebuild_page_tree(&mut pdf, &pages).unwrap();
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        // Catalog still has /Outlines.
        let cat = dict_of(&mut pdf, ObjectRef::new(1, 0));
        let outlines_ref = cat
            .get_ref("Outlines")
            .expect("catalog should have /Outlines");

        // Outline root: /First and /Last unchanged (20 and 23).
        let root = dict_of(&mut pdf, outlines_ref);
        assert_eq!(
            root.get_ref("First"),
            Some(ObjectRef::new(20, 0)),
            "outline root /First should be item 20 (unchanged)"
        );
        assert_eq!(
            root.get_ref("Last"),
            Some(ObjectRef::new(23, 0)),
            "outline root /Last should be item 23 (unchanged)"
        );

        // Item 20 (Page 1): /Next stays 21 0 R (no stitching).
        let item20 = dict_of(&mut pdf, ObjectRef::new(20, 0));
        assert_eq!(
            get_ref(&item20, "Next"),
            Some(ObjectRef::new(21, 0)),
            "item 20 /Next stays item 21 (chain not stitched)"
        );
        assert!(
            get_ref(&item20, "Prev").is_none(),
            "item 20 keeps no /Prev (it was already first)"
        );

        // Item 21 (Page 2, removed): KEPT, links intact, target page nulled.
        let item21 = dict_of(&mut pdf, ObjectRef::new(21, 0));
        assert_eq!(get_ref(&item21, "Prev"), Some(ObjectRef::new(20, 0)));
        assert_eq!(get_ref(&item21, "Next"), Some(ObjectRef::new(22, 0)));

        // Item 22 (Page 3): /Prev stays 21 0 R, /Next stays 23 0 R.
        let item22 = dict_of(&mut pdf, ObjectRef::new(22, 0));
        assert_eq!(get_ref(&item22, "Prev"), Some(ObjectRef::new(21, 0)));
        assert_eq!(get_ref(&item22, "Next"), Some(ObjectRef::new(23, 0)));

        // Item 22's child (24 0 R) still present (page 5 survived).
        assert_eq!(
            get_ref(&item22, "First"),
            Some(ObjectRef::new(24, 0)),
            "item 22 should still have child 24"
        );

        // Removed-page targets nulled in place.
        assert!(
            matches!(pdf.resolve(ObjectRef::new(4, 0)).unwrap(), Object::Null),
            "removed page 2 (obj4) nulled"
        );
        assert!(
            matches!(pdf.resolve(ObjectRef::new(6, 0)).unwrap(), Object::Null),
            "removed page 4 (obj6) nulled"
        );
    }

    // -----------------------------------------------------------------------
    // Test: /Count left unchanged (qpdf null-out never recomputes counts)
    // -----------------------------------------------------------------------

    #[test]
    fn count_left_unchanged() {
        // Keep pages 1 and 3.
        let mut pdf = open(build_outline_pdf());
        let pages = vec![ObjectRef::new(3, 0), ObjectRef::new(5, 0)];
        let result = rebuild_page_tree(&mut pdf, &pages).unwrap();
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        let cat = dict_of(&mut pdf, ObjectRef::new(1, 0));
        let outlines_ref = cat.get_ref("Outlines").unwrap();
        let root = dict_of(&mut pdf, outlines_ref);

        // Root /Count stays at its original 5 (not recomputed).
        assert_eq!(
            root.get("Count"),
            Some(&Object::Integer(5)),
            "outline root /Count should be unchanged (5)"
        );

        // Item 22 /Count stays at its original 1.
        let item22 = dict_of(&mut pdf, ObjectRef::new(22, 0));
        assert_eq!(
            item22.get("Count"),
            Some(&Object::Integer(1)),
            "item 22 /Count should be unchanged (1)"
        );
    }

    // -----------------------------------------------------------------------
    // Test: all outline items dropped → /Outlines removed from catalog
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // Test: all pages removed -> /Outlines RETAINED, every item kept, nulled
    // -----------------------------------------------------------------------

    #[test]
    fn all_pages_removed_keeps_outlines_and_nulls_targets() {
        // Empty ref_map means every old page is considered removed. qpdf
        // null-out keeps /Outlines and every outline item; all referenced page
        // objects (obj3,4,5,6) are nulled in place.
        let mut pdf = open(build_outline_pdf());
        // No page survives, so the rebuilt /Pages tree is empty too (a page
        // present in `new_kids` would, by definition, have survived); every
        // original leaf is therefore a removed page.
        let result = synthetic_result(&mut pdf, vec![], BTreeMap::new());
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        // /Outlines is RETAINED on the catalog.
        let cat = dict_of(&mut pdf, ObjectRef::new(1, 0));
        let outlines_ref = cat
            .get_ref("Outlines")
            .expect("catalog should retain /Outlines");

        // Outline root links unchanged; every item still present.
        let root = dict_of(&mut pdf, outlines_ref);
        assert_eq!(root.get_ref("First"), Some(ObjectRef::new(20, 0)));
        assert_eq!(root.get_ref("Last"), Some(ObjectRef::new(23, 0)));
        for n in [20u32, 21, 22, 23, 24] {
            let item = dict_of(&mut pdf, ObjectRef::new(n, 0));
            assert!(item.get("Title").is_some(), "item {n} should still exist");
        }

        // Every target page object nulled in place.
        for n in [3u32, 4, 5, 6] {
            assert!(
                matches!(pdf.resolve(ObjectRef::new(n, 0)).unwrap(), Object::Null),
                "removed page obj{n} should be nulled"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Test: named destinations pruned correctly
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // Test: named destinations kept; surviving remapped, removed targets nulled
    // -----------------------------------------------------------------------

    #[test]
    fn named_dests_pruned_and_remapped() {
        // Keep pages 1 and 3 (objs 3 and 5).
        let mut pdf = open(build_outline_pdf());
        let pages = vec![ObjectRef::new(3, 0), ObjectRef::new(5, 0)];
        let result = rebuild_page_tree(&mut pdf, &pages).unwrap();
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        // Check name tree (30 0 R): all 4 entries kept (qpdf null-out).
        let name_tree = dict_of(&mut pdf, ObjectRef::new(30, 0));
        let Some(Object::Array(names)) = name_tree.get("Names") else {
            panic!("name tree /Names should still be an array");
        };
        let name_strs: Vec<String> = names
            .iter()
            .step_by(2)
            .map(|o| match o {
                Object::String(b) => String::from_utf8_lossy(b).into_owned(),
                Object::Name(b) => String::from_utf8_lossy(b).into_owned(),
                _ => "<other>".into(),
            })
            .collect();
        for k in ["dest_p1", "dest_p2", "dest_p3", "dest_named_p4"] {
            assert!(name_strs.contains(&k.to_string()), "{k} should be kept");
        }

        let new_p1 = result.ref_map[&ObjectRef::new(3, 0)][0];
        let new_p3 = result.ref_map[&ObjectRef::new(5, 0)][0];

        let dest_first = |key: &[u8]| -> ObjectRef {
            let idx = names
                .iter()
                .step_by(2)
                .position(|o| matches!(o, Object::String(b) if b == key))
                .expect("named dest present");
            names[idx * 2 + 1]
                .as_array()
                .expect("array dest")
                .first()
                .unwrap()
                .as_ref_id()
                .unwrap()
        };

        // dest_p1 / dest_p3 -> surviving pages: remapped to new refs.
        assert_eq!(dest_first(b"dest_p1"), new_p1, "dest_p1 remapped");
        assert_eq!(dest_first(b"dest_p3"), new_p3, "dest_p3 remapped");
        // dest_p2 / dest_named_p4 -> removed pages: ref kept, page obj nulled.
        assert_eq!(
            dest_first(b"dest_p2"),
            ObjectRef::new(4, 0),
            "dest_p2 keeps its removed-page ref"
        );
        assert_eq!(
            dest_first(b"dest_named_p4"),
            ObjectRef::new(6, 0),
            "dest_named_p4 keeps its removed-page ref"
        );
        assert!(
            matches!(pdf.resolve(ObjectRef::new(4, 0)).unwrap(), Object::Null),
            "page 2 (obj4) nulled"
        );
        assert!(
            matches!(pdf.resolve(ObjectRef::new(6, 0)).unwrap(), Object::Null),
            "page 4 (obj6) nulled"
        );
    }

    // -----------------------------------------------------------------------
    // Test: string-form /Dest outline item dropped when named dest pruned
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // Test: string-form /Dest outline item KEPT when its named dest is nulled
    // -----------------------------------------------------------------------

    #[test]
    fn string_dest_item_kept_when_named_dest_target_nulled() {
        // Keep only pages 1 and 3. Item 23 has /Dest (dest_named_p4) -> page 4
        // (removed). qpdf null-out keeps item 23 verbatim and nulls obj6.
        let mut pdf = open(build_outline_pdf());
        let pages = vec![ObjectRef::new(3, 0), ObjectRef::new(5, 0)];
        let result = rebuild_page_tree(&mut pdf, &pages).unwrap();
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        let cat = dict_of(&mut pdf, ObjectRef::new(1, 0));
        let outlines_ref = cat.get_ref("Outlines").unwrap();
        let root = dict_of(&mut pdf, outlines_ref);
        // Root /Last stays item 23 (not dropped/stitched).
        assert_eq!(
            root.get_ref("Last"),
            Some(ObjectRef::new(23, 0)),
            "item 23 should be kept (string-dest item not dropped)"
        );

        // Item 23 still present with its string /Dest verbatim.
        let item23 = dict_of(&mut pdf, ObjectRef::new(23, 0));
        assert_eq!(item23.get_ref("Prev"), Some(ObjectRef::new(22, 0)));
        assert!(
            matches!(item23.get("Dest"), Some(Object::String(b)) if b == b"dest_named_p4"),
            "item 23 keeps its string /Dest"
        );
        // The named dest's target page (obj6) is nulled.
        assert!(
            matches!(pdf.resolve(ObjectRef::new(6, 0)).unwrap(), Object::Null),
            "page 4 (obj6) nulled"
        );
    }

    // -----------------------------------------------------------------------
    // Test: duplicate page selection → first new ref used
    // -----------------------------------------------------------------------

    #[test]
    fn duplicate_selection_uses_first_new_ref() {
        // Select page 1 twice → ref_map[obj3] = [obj3, obj10].
        // Outline item 20 points at page 1 → should be remapped to obj3 (first).
        let mut pdf = open(build_outline_pdf());
        let pages = vec![ObjectRef::new(3, 0), ObjectRef::new(3, 0)];
        let result = rebuild_page_tree(&mut pdf, &pages).unwrap();
        assert_eq!(result.ref_map[&ObjectRef::new(3, 0)].len(), 2);
        let first_new = result.ref_map[&ObjectRef::new(3, 0)][0];

        remap_outline_and_dests(&mut pdf, &result).unwrap();

        let item20 = dict_of(&mut pdf, ObjectRef::new(20, 0));
        if let Some(arr) = item20.get("Dest").and_then(Object::as_array) {
            assert_eq!(
                arr.first(),
                Some(&Object::Reference(first_new)),
                "item 20 should be remapped to first new ref of duplicated page"
            );
        } else {
            panic!("item 20 /Dest should be array");
        }
    }

    // -----------------------------------------------------------------------
    // Test: round-trip produces valid PDF
    // -----------------------------------------------------------------------

    #[test]
    fn round_trip_produces_valid_pdf() {
        let mut pdf = open(build_outline_pdf());
        let pages = vec![ObjectRef::new(3, 0), ObjectRef::new(5, 0)];
        let result = rebuild_page_tree(&mut pdf, &pages).unwrap();
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        let mut out: Vec<u8> = Vec::new();
        write_pdf(&mut pdf, &mut out).unwrap();

        let report = check_reader(Cursor::new(out)).expect("check should run");
        assert!(
            report.valid,
            "rebuilt PDF should pass check_reader: {:?}",
            report.diagnostics
        );
    }

    // -----------------------------------------------------------------------
    // Test: all named dests pruned → /Names /Dests removed from catalog (no dangling ref)
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // Test: all pages removed -> /Names + /Dests RETAINED, all entries kept
    // -----------------------------------------------------------------------

    #[test]
    fn all_pages_removed_keeps_named_dests_nulls_targets() {
        // Empty ref_map: every page removed. The name tree (30 0 R) keeps all 4
        // entries; /Names and /Dests stay on the catalog; every target page obj
        // is nulled.
        let mut pdf = open(build_outline_pdf());
        // No page survives, so the rebuilt /Pages tree is empty too; all pages
        // removed.
        let result = synthetic_result(&mut pdf, vec![], BTreeMap::new());
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        let cat = dict_of(&mut pdf, ObjectRef::new(1, 0));
        // /Names retained on catalog.
        let names_ref = cat
            .get_ref("Names")
            .expect("catalog /Names should be retained");
        let names_dict = dict_of(&mut pdf, names_ref);
        assert!(
            names_dict.get_ref("Dests").is_some(),
            "/Dests should be retained in the /Names dict"
        );

        // All 4 named-dest entries still present.
        let leaf = dict_of(&mut pdf, ObjectRef::new(30, 0));
        let Some(Object::Array(names)) = leaf.get("Names") else {
            panic!("/Names array expected");
        };
        let keys: Vec<&[u8]> = names
            .iter()
            .step_by(2)
            .filter_map(|o| match o {
                Object::String(b) | Object::Name(b) => Some(b.as_slice()),
                _ => None,
            })
            .collect();
        assert_eq!(keys.len(), 4, "all 4 named dests kept");

        // Every target page object nulled.
        for n in [3u32, 4, 5, 6] {
            assert!(
                matches!(pdf.resolve(ObjectRef::new(n, 0)).unwrap(), Object::Null),
                "page obj{n} nulled"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Test: parent item with all children dropped has Count=0 and no First/Last
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // Test: parent item whose subtree targets a removed page is KEPT and nulled
    // -----------------------------------------------------------------------

    #[test]
    fn parent_with_removed_target_kept_and_nulled() {
        // Keep only page 1 (obj 3). Item 22 (page 3 = obj5) and its child 24
        // (also obj5) target a removed page. qpdf null-out keeps item 22 and
        // child 24, keeps item 22's /First 24 and /Count, and nulls obj5.
        let mut pdf = open(build_outline_pdf());
        let pages = vec![ObjectRef::new(3, 0)]; // only page 1
        let result = rebuild_page_tree(&mut pdf, &pages).unwrap();
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        // Outline root unchanged.
        let cat = dict_of(&mut pdf, ObjectRef::new(1, 0));
        let outlines_ref = cat
            .get_ref("Outlines")
            .expect("catalog should still have /Outlines");
        let root = dict_of(&mut pdf, outlines_ref);
        assert_eq!(root.get_ref("First"), Some(ObjectRef::new(20, 0)));
        assert_eq!(root.get_ref("Last"), Some(ObjectRef::new(23, 0)));

        // Item 22 kept with its /First child 24 and /Count unchanged.
        let item22 = dict_of(&mut pdf, ObjectRef::new(22, 0));
        assert_eq!(item22.get_ref("First"), Some(ObjectRef::new(24, 0)));
        assert_eq!(item22.get("Count"), Some(&Object::Integer(1)));

        // Child 24 still present.
        let item24 = dict_of(&mut pdf, ObjectRef::new(24, 0));
        assert!(item24.get("Title").is_some(), "child 24 should be kept");

        // The removed target page (obj5) is nulled.
        assert!(
            matches!(pdf.resolve(ObjectRef::new(5, 0)).unwrap(), Object::Null),
            "page 3 (obj5) nulled"
        );
    }

    /// Build a minimal raw PDF from a list of `(objnum, body)` pairs plus a
    /// trailer dict body. Shared by the regression tests below.
    fn build_min_pdf(objs: &[(u32, &str)], trailer_extra: &str) -> Vec<u8> {
        let mut raw: Vec<u8> = b"%PDF-1.5\n".to_vec();
        let mut offs: BTreeMap<u32, usize> = BTreeMap::new();
        for (num, body) in objs {
            offs.insert(*num, raw.len());
            raw.extend_from_slice(format!("{num} 0 obj\n{body}\nendobj\n").as_bytes());
        }
        let max_num = objs.iter().map(|(n, _)| *n).max().unwrap_or(0);
        let xref_pos = raw.len();
        raw.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", max_num + 1).as_bytes());
        for i in 1..=max_num {
            if let Some(&off) = offs.get(&i) {
                raw.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
            } else {
                raw.extend_from_slice(b"0000000000 65535 f \n");
            }
        }
        raw.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R {trailer_extra} >>\nstartxref\n{}\n%%EOF\n",
                max_num + 1,
                xref_pos
            )
            .as_bytes(),
        );
        raw
    }

    #[test]
    fn indirect_named_dest_remapped_and_nulled() {
        // Named dest values are *indirect references* to the dest arrays
        // (obj 40 -> page 1 kept, obj 41 -> page 2 removed). qpdf null-out keeps
        // both entries: obj40's page ref is remapped in place; obj41 stays
        // verbatim and the removed page obj4 is nulled (the indirect dest holder
        // obj41 is NOT nulled).
        let bytes = build_min_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R /Names 11 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (11, "<< /Dests 30 0 R >>"),
                (
                    30,
                    "<< /Limits [(d1) (d2)] /Names [(d1) 40 0 R (d2) 41 0 R] >>",
                ),
                (40, "[3 0 R /XYZ 0 792 0]"),
                (41, "[4 0 R /XYZ 0 792 0]"),
            ],
            "",
        );
        let mut pdf = open(bytes);
        let result = rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
        let new_p1 = result.ref_map[&ObjectRef::new(3, 0)][0];
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        // Both named dests kept, in original order.
        let leaf = dict_of(&mut pdf, ObjectRef::new(30, 0));
        let Some(Object::Array(names)) = leaf.get("Names").cloned() else {
            panic!("/Names array expected");
        };
        let kept_names: Vec<&[u8]> = names
            .iter()
            .filter_map(|o| match o {
                Object::String(b) | Object::Name(b) => Some(b.as_slice()),
                _ => None,
            })
            .collect();
        assert_eq!(kept_names, vec![b"d1".as_slice(), b"d2".as_slice()]);

        // obj40 (d1) page ref remapped in place.
        let dest40 = pdf.resolve(ObjectRef::new(40, 0)).unwrap();
        let Some(arr40) = dest40.into_array() else {
            panic!("obj 40 should remain a dest array");
        };
        assert_eq!(arr40.first(), Some(&Object::Reference(new_p1)));

        // obj41 (d2) dest array intact; only the page obj4 is nulled.
        let dest41 = pdf.resolve(ObjectRef::new(41, 0)).unwrap();
        let Some(arr41) = dest41.into_array() else {
            panic!("obj 41 should remain a dest array (holder not nulled)");
        };
        assert_eq!(
            arr41.first(),
            Some(&Object::Reference(ObjectRef::new(4, 0)))
        );
        assert!(
            matches!(pdf.resolve(ObjectRef::new(4, 0)).unwrap(), Object::Null),
            "page 2 (obj4) nulled, holder obj41 untouched"
        );
    }

    #[test]
    fn malformed_named_dest_to_non_page_dict_is_not_nulled() {
        // An attacker-controlled named destination points its first array
        // element at a signature field (obj7, a non-page object); a second entry
        // points at the genuinely removed page (obj4). qpdf nulls only removed
        // PAGE objects, so obj7 must survive intact while obj4 is still nulled.
        let bytes = build_min_pdf(
            &[
                (
                    1,
                    "<< /Type /Catalog /Pages 2 0 R /Names 11 0 R /AcroForm 6 0 R >>",
                ),
                (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (6, "<< /Fields [7 0 R] /SigFlags 3 >>"),
                (7, "<< /FT /Sig /T (sig) /V 8 0 R >>"),
                (8, "<< /Type /Sig /Filter /Adobe.PPKLite >>"),
                (11, "<< /Dests 30 0 R >>"),
                (
                    30,
                    "<< /Names [(evil) [7 0 R /Fit] (removed) [4 0 R /Fit]] >>",
                ),
            ],
            "",
        );
        let mut pdf = open(bytes);
        let result = rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        // The signature field object survives intact — it is not a page.
        let sig_field = dict_of(&mut pdf, ObjectRef::new(7, 0));
        assert_eq!(sig_field.get("FT"), Some(&Object::Name(b"Sig".to_vec())));
        // The genuinely removed page (obj4) is still nulled.
        assert!(
            matches!(pdf.resolve(ObjectRef::new(4, 0)).unwrap(), Object::Null),
            "removed page obj4 is still nulled"
        );
    }

    #[test]
    fn malformed_outline_dest_to_non_page_dict_is_not_nulled() {
        // Same boundary via an outline item's /Dest: item 20 targets a non-page
        // object (obj7), item 21 targets the removed page (obj4). obj7 survives;
        // obj4 is nulled.
        let bytes = build_min_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (7, "<< /FT /Sig /T (sig) /V 8 0 R >>"),
                (8, "<< /Type /Sig /Filter /Adobe.PPKLite >>"),
                (
                    10,
                    "<< /Type /Outlines /First 20 0 R /Last 21 0 R /Count 2 >>",
                ),
                (
                    20,
                    "<< /Title (evil) /Parent 10 0 R /Next 21 0 R /Dest [7 0 R /Fit] >>",
                ),
                (
                    21,
                    "<< /Title (removed) /Parent 10 0 R /Prev 20 0 R /Dest [4 0 R /Fit] >>",
                ),
            ],
            "",
        );
        let mut pdf = open(bytes);
        let result = rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        let sig_field = dict_of(&mut pdf, ObjectRef::new(7, 0));
        assert_eq!(sig_field.get("FT"), Some(&Object::Name(b"Sig".to_vec())));
        assert!(
            matches!(pdf.resolve(ObjectRef::new(4, 0)).unwrap(), Object::Null),
            "removed page obj4 is still nulled"
        );
    }

    #[test]
    fn malformed_dest_to_fake_type_page_outside_tree_is_not_nulled() {
        // The hostile object (obj7) forges /Type /Page but was never a leaf of
        // the source page tree (absent from /Pages /Kids). qpdf nulls only
        // original page-tree members, so obj7 must survive — a /Type-only check
        // would wrongly null it, leaving the signature-evidence bypass open.
        let bytes = build_min_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R /Names 11 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (7, "<< /FT /Sig /Type /Page /T (sig) /V 8 0 R >>"),
                (8, "<< /Type /Sig /Filter /Adobe.PPKLite >>"),
                (11, "<< /Dests 30 0 R >>"),
                (
                    30,
                    "<< /Names [(evil) [7 0 R /Fit] (removed) [4 0 R /Fit]] >>",
                ),
            ],
            "",
        );
        let mut pdf = open(bytes);
        let result = rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        // The forged-/Type/Page object survives — it was never a page-tree leaf.
        let forged = dict_of(&mut pdf, ObjectRef::new(7, 0));
        assert_eq!(forged.get("FT"), Some(&Object::Name(b"Sig".to_vec())));
        // The genuine removed page (obj4, an actual tree leaf) is still nulled.
        assert!(
            matches!(pdf.resolve(ObjectRef::new(4, 0)).unwrap(), Object::Null),
            "removed page obj4 is still nulled"
        );
    }

    #[test]
    fn removed_page_behind_reference_holder_is_nulled() {
        // A named dest reaches the removed page through a reference *holder*
        // (`40 0 obj` = `4 0 R`), not by pointing at obj4 directly. qpdf nulls
        // the page leaf (obj4) regardless of how it is referenced, so its
        // contents cannot leak; the holder (obj40, not a page) is untouched.
        // Regression for the Codex finding that the destination-following guard
        // left obj4 live behind the holder.
        let bytes = build_min_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R /Names 11 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (
                    4,
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Secret (S4) >>",
                ),
                (11, "<< /Dests 30 0 R >>"),
                (30, "<< /Names [(evil) [40 0 R /Fit]] >>"),
                (40, "4 0 R"),
            ],
            "",
        );
        let mut pdf = open(bytes);
        let result = rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
        remap_outline_and_dests(&mut pdf, &result).unwrap();
        assert!(
            matches!(pdf.resolve(ObjectRef::new(4, 0)).unwrap(), Object::Null),
            "removed page obj4 reached only through a reference holder must be nulled"
        );
    }

    #[test]
    fn removed_page_behind_non_page_wrapper_is_nulled() {
        // A named dest reaches the removed page through a NON-PAGE wrapper dict
        // (`40 0 obj` = `<< /X 4 0 R >>`). qpdf nulls the page leaf (obj4)
        // directly; the wrapper (not a page) is left untouched and now points at
        // the null. Regression for the Codex finding that the wrapper kept the
        // removed page reachable through the subset sweep.
        let bytes = build_min_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R /Names 11 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (
                    4,
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Secret (S4) >>",
                ),
                (11, "<< /Dests 30 0 R >>"),
                (30, "<< /Names [(evil) [40 0 R /Fit]] >>"),
                (40, "<< /X 4 0 R >>"),
            ],
            "",
        );
        let mut pdf = open(bytes);
        let result = rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
        remap_outline_and_dests(&mut pdf, &result).unwrap();
        assert!(
            matches!(pdf.resolve(ObjectRef::new(4, 0)).unwrap(), Object::Null),
            "removed page obj4 reached only through a non-page wrapper must be nulled"
        );
        // The wrapper itself is a non-page object and is left untouched.
        let wrapper = dict_of(&mut pdf, ObjectRef::new(40, 0));
        assert_eq!(
            wrapper.get("X"),
            Some(&Object::Reference(ObjectRef::new(4, 0)))
        );
    }

    #[test]
    fn malformed_dest_to_non_dict_object_is_not_nulled() {
        // The first dest-array element references a non-dictionary object (an
        // integer). It is neither a page nor null, so null-out leaves it intact
        // (exercises the non-dict arm of `is_nullable_removed_page`).
        let bytes = build_min_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R /Names 11 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (9, "42"),
                (11, "<< /Dests 30 0 R >>"),
                (30, "<< /Names [(evil) [9 0 R /Fit]] >>"),
            ],
            "",
        );
        let mut pdf = open(bytes);
        let result = rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        // obj9 (the integer) is untouched — not nulled.
        assert_eq!(
            pdf.resolve(ObjectRef::new(9, 0)).unwrap(),
            Object::Integer(42),
            "non-dict dest target obj9 is left untouched",
        );
    }

    #[test]
    fn surviving_parent_with_removed_child_kept_and_nulled() {
        // Item 20 points at kept page 1; its only child 21 points at removed
        // page 2 and the parent is closed (/Count -1). qpdf null-out keeps child
        // 21 and item 20's /First 21 and /Count -1 unchanged, and nulls obj4.
        let bytes = build_min_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (
                    10,
                    "<< /Type /Outlines /First 20 0 R /Last 20 0 R /Count 1 >>",
                ),
                (
                    20,
                    "<< /Title (P1) /Parent 10 0 R /Dest [3 0 R /Fit] \
                     /First 21 0 R /Last 21 0 R /Count -1 >>",
                ),
                (
                    21,
                    "<< /Title (P2 sub) /Parent 20 0 R /Dest [4 0 R /Fit] >>",
                ),
            ],
            "",
        );
        let mut pdf = open(bytes);
        let result = rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        let item20 = dict_of(&mut pdf, ObjectRef::new(20, 0));
        assert_eq!(item20.get_ref("First"), Some(ObjectRef::new(21, 0)));
        assert_eq!(item20.get_ref("Last"), Some(ObjectRef::new(21, 0)));
        assert_eq!(
            item20.get("Count"),
            Some(&Object::Integer(-1)),
            "item 20 keeps its /Count -1 unchanged"
        );

        // Child 21 still present; its removed target page obj4 nulled.
        let item21 = dict_of(&mut pdf, ObjectRef::new(21, 0));
        assert!(item21.get("Title").is_some(), "child 21 kept");
        assert!(
            matches!(pdf.resolve(ObjectRef::new(4, 0)).unwrap(), Object::Null),
            "page 2 (obj4) nulled"
        );
    }

    #[test]
    fn dict_form_named_dest_page_ref_is_remapped() {
        // Named dest value is the dictionary form << /D [pageRef /Fit] >>.
        // dest_page_ref accepts it, so remap must rewrite the page ref inside
        // /D; otherwise a kept dict-dest keeps a stale (soon-dangling) ref.
        let bytes = build_min_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R /Names 11 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (11, "<< /Dests 30 0 R >>"),
                (
                    30,
                    "<< /Limits [(d1) (d1)] /Names [(d1) << /D [3 0 R /Fit] >>] >>",
                ),
            ],
            "",
        );
        let mut pdf = open(bytes);
        let result = rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
        let new_p1 = result.ref_map[&ObjectRef::new(3, 0)][0];
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        let leaf = dict_of(&mut pdf, ObjectRef::new(30, 0));
        let Some(Object::Array(names)) = leaf.get("Names").cloned() else {
            panic!("/Names array expected");
        };
        // (d1) survives; its dict-form dest /D page ref is remapped.
        let dest = &names[1];
        let Some(dd) = dest.as_dict() else {
            panic!("dict-form dest expected, got {dest:?}");
        };
        let Some(arr) = dd.get("D").and_then(Object::as_array) else {
            panic!("/D array expected");
        };
        assert_eq!(arr.first(), Some(&Object::Reference(new_p1)));
    }

    #[test]
    fn indirect_outline_item_dest_remapped_and_nulled() {
        // Item 20 /Dest is an *indirect ref* (40 0 R) to [3 0 R /Fit] (page 1,
        // kept). Item 21 /Dest is 41 0 R -> [4 0 R /Fit] (page 2, removed).
        // qpdf null-out keeps both items; obj40 is remapped in place; obj41's
        // array stays intact and only page obj4 is nulled.
        let bytes = build_min_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (
                    10,
                    "<< /Type /Outlines /First 20 0 R /Last 21 0 R /Count 2 >>",
                ),
                (
                    20,
                    "<< /Title (P1) /Parent 10 0 R /Next 21 0 R /Dest 40 0 R >>",
                ),
                (
                    21,
                    "<< /Title (P2) /Parent 10 0 R /Prev 20 0 R /Dest 41 0 R >>",
                ),
                (40, "[3 0 R /Fit]"),
                (41, "[4 0 R /Fit]"),
            ],
            "",
        );
        let mut pdf = open(bytes);
        let result = rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
        let new_p1 = result.ref_map[&ObjectRef::new(3, 0)][0];
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        // Both items kept; sibling links unchanged.
        let root = dict_of(&mut pdf, ObjectRef::new(10, 0));
        assert_eq!(root.get_ref("First"), Some(ObjectRef::new(20, 0)));
        assert_eq!(root.get_ref("Last"), Some(ObjectRef::new(21, 0)));
        let item20 = dict_of(&mut pdf, ObjectRef::new(20, 0));
        assert_eq!(item20.get_ref("Next"), Some(ObjectRef::new(21, 0)));
        assert_eq!(item20.get_ref("Dest"), Some(ObjectRef::new(40, 0)));
        let item21 = dict_of(&mut pdf, ObjectRef::new(21, 0));
        assert_eq!(item21.get_ref("Dest"), Some(ObjectRef::new(41, 0)));

        // obj40 remapped in place.
        let dest40 = pdf.resolve(ObjectRef::new(40, 0)).unwrap();
        let Some(arr40) = dest40.into_array() else {
            panic!("obj 40 should stay a dest array");
        };
        assert_eq!(arr40.first(), Some(&Object::Reference(new_p1)));

        // obj41 array intact; page obj4 nulled.
        let dest41 = pdf.resolve(ObjectRef::new(41, 0)).unwrap();
        let Some(arr41) = dest41.into_array() else {
            panic!("obj 41 should stay a dest array (holder not nulled)");
        };
        assert_eq!(
            arr41.first(),
            Some(&Object::Reference(ObjectRef::new(4, 0)))
        );
        assert!(
            matches!(pdf.resolve(ObjectRef::new(4, 0)).unwrap(), Object::Null),
            "page 2 (obj4) nulled"
        );
    }

    #[test]
    fn indirect_goto_action_remapped_and_nulled() {
        // /A is an *indirect ref* to a GoTo action. Item 20's action -> page 1
        // (kept), item 21's -> page 2 (removed). qpdf null-out keeps both items;
        // action 50 is remapped in place; action 51 stays verbatim and page obj4
        // is nulled.
        let bytes = build_min_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (
                    10,
                    "<< /Type /Outlines /First 20 0 R /Last 21 0 R /Count 2 >>",
                ),
                (
                    20,
                    "<< /Title (P1) /Parent 10 0 R /Next 21 0 R /A 50 0 R >>",
                ),
                (
                    21,
                    "<< /Title (P2) /Parent 10 0 R /Prev 20 0 R /A 51 0 R >>",
                ),
                (50, "<< /S /GoTo /D [3 0 R /Fit] >>"),
                (51, "<< /S /GoTo /D [4 0 R /Fit] >>"),
            ],
            "",
        );
        let mut pdf = open(bytes);
        let result = rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
        let new_p1 = result.ref_map[&ObjectRef::new(3, 0)][0];
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        // Both items kept; links unchanged.
        let root = dict_of(&mut pdf, ObjectRef::new(10, 0));
        assert_eq!(root.get_ref("First"), Some(ObjectRef::new(20, 0)));
        assert_eq!(root.get_ref("Last"), Some(ObjectRef::new(21, 0)));
        let item20 = dict_of(&mut pdf, ObjectRef::new(20, 0));
        assert_eq!(item20.get_ref("A"), Some(ObjectRef::new(50, 0)));

        // action 50 remapped in place.
        let action50 = dict_of(&mut pdf, ObjectRef::new(50, 0));
        let Some(Object::Array(arr50)) = action50.get("D") else {
            panic!("/D array expected on action 50");
        };
        assert_eq!(arr50.first(), Some(&Object::Reference(new_p1)));

        // action 51 intact; page obj4 nulled.
        let action51 = dict_of(&mut pdf, ObjectRef::new(51, 0));
        let Some(Object::Array(arr51)) = action51.get("D") else {
            panic!("/D array expected on action 51 (holder not nulled)");
        };
        assert_eq!(
            arr51.first(),
            Some(&Object::Reference(ObjectRef::new(4, 0)))
        );
        assert!(
            matches!(pdf.resolve(ObjectRef::new(4, 0)).unwrap(), Object::Null),
            "page 2 (obj4) nulled"
        );
    }

    #[test]
    fn conserved_no_page_named_dest_keeps_string_dest_outline_item() {
        // Named dest (ext) has no resolvable page ref (action-form, no page).
        // It is kept conservatively; the outline item with /Dest (ext) must
        // therefore also survive (regression: name not added to
        // surviving_names previously dropped the item).
        let bytes = build_min_pdf(
            &[
                (
                    1,
                    "<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R /Names 11 0 R >>",
                ),
                (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (11, "<< /Dests 30 0 R >>"),
                (
                    30,
                    "<< /Limits [(ext) (ext)] \
                     /Names [(ext) << /S /GoTo /D (somewhere) >>] >>",
                ),
                (
                    10,
                    "<< /Type /Outlines /First 20 0 R /Last 20 0 R /Count 1 >>",
                ),
                (20, "<< /Title (E) /Parent 10 0 R /Dest (ext) >>"),
            ],
            "",
        );
        let mut pdf = open(bytes);
        // Keep page 1 only; the named dest has no page ref so it is conserved.
        let result = rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        let cat = dict_of(&mut pdf, ObjectRef::new(1, 0));
        let outlines = cat
            .get_ref("Outlines")
            .expect("/Outlines should survive (item kept)");
        let root = dict_of(&mut pdf, outlines);
        assert_eq!(
            root.get_ref("First"),
            Some(ObjectRef::new(20, 0)),
            "string-dest item to a conserved named dest must be kept"
        );
    }

    #[test]
    fn legacy_dests_direct_dictionary_on_catalog_is_remapped() {
        // Legacy /Dests is a *direct* dictionary on the catalog (not an indirect
        // ref). qpdf null-out keeps both entries: d1 (page 1 kept) is remapped;
        // d2 (page 2 removed) stays verbatim and page obj4 is nulled.
        let bytes = build_min_pdf(
            &[
                (
                    1,
                    "<< /Type /Catalog /Pages 2 0 R \
                     /Dests << /d1 [3 0 R /Fit] /d2 [4 0 R /Fit] >> >>",
                ),
                (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            ],
            "",
        );
        let mut pdf = open(bytes);
        let result = rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
        let new_p1 = result.ref_map[&ObjectRef::new(3, 0)][0];
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        let cat = dict_of(&mut pdf, ObjectRef::new(1, 0));
        let Some(Object::Dictionary(dests)) = cat.get("Dests") else {
            panic!("/Dests direct dict expected on catalog");
        };
        // d1 (page 1 kept) remapped.
        let Some(Object::Array(arr1)) = dests.get("d1") else {
            panic!("d1 should be an array dest");
        };
        assert_eq!(arr1.first(), Some(&Object::Reference(new_p1)));
        // d2 (page 2 removed) kept verbatim; page obj4 nulled.
        let Some(Object::Array(arr2)) = dests.get("d2") else {
            panic!("d2 should be kept as an array dest");
        };
        assert_eq!(arr2.first(), Some(&Object::Reference(ObjectRef::new(4, 0))));
        assert!(
            matches!(pdf.resolve(ObjectRef::new(4, 0)).unwrap(), Object::Null),
            "page 2 (obj4) nulled"
        );
    }

    #[test]
    fn dict_form_dest_with_indirect_d_is_resolved() {
        // Named dest is dict form whose /D is an *indirect* ref:
        //   (d1) << /D 40 0 R >>   40 -> [3 0 R /Fit]  (page 1 kept)
        //   (d2) << /D 41 0 R >>   41 -> [4 0 R /Fit]  (page 2 removed)
        // qpdf null-out keeps both entries: obj40 remapped in place; obj41 stays
        // intact and page obj4 is nulled.
        let bytes = build_min_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R /Names 11 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (11, "<< /Dests 30 0 R >>"),
                (
                    30,
                    "<< /Limits [(d1) (d2)] \
                     /Names [(d1) << /D 40 0 R >> (d2) << /D 41 0 R >>] >>",
                ),
                (40, "[3 0 R /Fit]"),
                (41, "[4 0 R /Fit]"),
            ],
            "",
        );
        let mut pdf = open(bytes);
        let result = rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
        let new_p1 = result.ref_map[&ObjectRef::new(3, 0)][0];
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        let leaf = dict_of(&mut pdf, ObjectRef::new(30, 0));
        let Some(Object::Array(names)) = leaf.get("Names").cloned() else {
            panic!("/Names array expected");
        };
        let kept: Vec<&[u8]> = names
            .iter()
            .filter_map(|o| match o {
                Object::String(b) | Object::Name(b) => Some(b.as_slice()),
                _ => None,
            })
            .collect();
        assert_eq!(kept, vec![b"d1".as_slice(), b"d2".as_slice()], "both kept");

        // obj40 (d1's /D) remapped in place.
        let dest40 = pdf.resolve(ObjectRef::new(40, 0)).unwrap();
        let Some(arr40) = dest40.into_array() else {
            panic!("obj 40 should remain a dest array");
        };
        assert_eq!(arr40.first(), Some(&Object::Reference(new_p1)));

        // obj41 (d2's /D) intact; page obj4 nulled.
        let dest41 = pdf.resolve(ObjectRef::new(41, 0)).unwrap();
        let Some(arr41) = dest41.into_array() else {
            panic!("obj 41 should remain a dest array (holder not nulled)");
        };
        assert_eq!(
            arr41.first(),
            Some(&Object::Reference(ObjectRef::new(4, 0)))
        );
        assert!(
            matches!(pdf.resolve(ObjectRef::new(4, 0)).unwrap(), Object::Null),
            "page 2 (obj4) nulled"
        );
    }

    #[test]
    fn names_direct_dictionary_on_catalog_is_remapped() {
        // /Names is a *direct* dictionary on the catalog (not an indirect ref).
        // qpdf null-out keeps both entries: d1 (page 1 kept) remapped; d2 (page 2
        // removed) kept verbatim and page obj4 nulled.
        let bytes = build_min_pdf(
            &[
                (
                    1,
                    "<< /Type /Catalog /Pages 2 0 R /Names << /Dests 30 0 R >> >>",
                ),
                (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (
                    30,
                    "<< /Limits [(d1) (d2)] \
                     /Names [(d1) [3 0 R /Fit] (d2) [4 0 R /Fit]] >>",
                ),
            ],
            "",
        );
        let mut pdf = open(bytes);
        let result = rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
        let new_p1 = result.ref_map[&ObjectRef::new(3, 0)][0];
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        // Catalog still carries a direct /Names dict.
        let cat = dict_of(&mut pdf, ObjectRef::new(1, 0));
        assert!(
            matches!(cat.get("Names"), Some(Object::Dictionary(_))),
            "/Names should remain a direct dict on the catalog"
        );
        let leaf = dict_of(&mut pdf, ObjectRef::new(30, 0));
        let Some(Object::Array(names)) = leaf.get("Names").cloned() else {
            panic!("/Names array expected in leaf");
        };
        let kept: Vec<&[u8]> = names
            .iter()
            .filter_map(|o| match o {
                Object::String(b) | Object::Name(b) => Some(b.as_slice()),
                _ => None,
            })
            .collect();
        assert_eq!(kept, vec![b"d1".as_slice(), b"d2".as_slice()], "both kept");

        // d1 remapped to new page-1 ref.
        let pos1 = names
            .iter()
            .position(|o| matches!(o, Object::String(b) | Object::Name(b) if b == b"d1"))
            .unwrap();
        let Object::Array(arr1) = &names[pos1 + 1] else {
            panic!("d1 dest array expected");
        };
        assert_eq!(arr1.first(), Some(&Object::Reference(new_p1)));

        // d2 kept verbatim; page obj4 nulled.
        let pos2 = names
            .iter()
            .position(|o| matches!(o, Object::String(b) | Object::Name(b) if b == b"d2"))
            .unwrap();
        let Object::Array(arr2) = &names[pos2 + 1] else {
            panic!("d2 dest array expected");
        };
        assert_eq!(arr2.first(), Some(&Object::Reference(ObjectRef::new(4, 0))));
        assert!(
            matches!(pdf.resolve(ObjectRef::new(4, 0)).unwrap(), Object::Null),
            "page 2 (obj4) nulled"
        );
    }

    #[test]
    fn cyclic_indirect_dest_terminates_without_overflow() {
        // Hostile self-referential dest: 40 0 obj << /D 40 0 R >>. Resolution
        // must terminate via the depth guard instead of overflowing the
        // stack; the entry is kept conservatively (no resolvable page ref).
        let bytes = build_min_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R /Names 11 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (11, "<< /Dests 30 0 R >>"),
                (30, "<< /Limits [(c) (c)] /Names [(c) 40 0 R] >>"),
                (40, "<< /D 40 0 R >>"),
            ],
            "",
        );
        let mut pdf = open(bytes);
        let result = rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
        // Must return (not hang / overflow).
        remap_outline_and_dests(&mut pdf, &result).unwrap();
        // Conserved: name (c) is still present (no resolvable page ref).
        let leaf = dict_of(&mut pdf, ObjectRef::new(30, 0));
        let Some(Object::Array(names)) = leaf.get("Names").cloned() else {
            panic!("/Names array expected");
        };
        assert!(names
            .iter()
            .any(|o| matches!(o, Object::String(b) | Object::Name(b) if b == b"c")));
    }

    // -----------------------------------------------------------------------
    // Hostile-PDF hardening: bounded recursion / cycle guards on the
    // outline-tree and name-tree walkers.
    //
    // Both walkers are guarded (depth limit + a shared `visited` set):
    //   - remap_outline_tree walks the outline sibling/child chains.
    //   - remap_name_tree walks the name-tree /Kids.
    // The tests below are load-bearing — each one hangs or errors for the
    // wrong reason if its guard is removed. Dest resolution
    // (dest_page_ref_resolved / remap_dest_value / resolve_ref_chain) is
    // covered by `cyclic_indirect_dest_terminates_without_overflow` above.
    // -----------------------------------------------------------------------

    #[test]
    fn remap_outline_tree_cycle_terminates() {
        // Sibling /Next back-edge cycle: 40 /Next 41, 41 /Next 40. The shared
        // `visited` set must break the loop; otherwise it spins forever.
        let bytes = build_min_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (40, "<< /Title (a) /Next 41 0 R >>"),
                (41, "<< /Title (b) /Next 40 0 R >>"),
            ],
            "",
        );
        let mut pdf = open(bytes);
        let surviving = Surviving::default();
        let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
        remap_outline_tree(
            &mut pdf,
            ObjectRef::new(40, 0),
            0,
            100,
            &surviving,
            &mut visited,
        )
        .expect("cyclic /Next chain must terminate gracefully");
        assert!(visited.contains(&ObjectRef::new(40, 0)));
        assert!(visited.contains(&ObjectRef::new(41, 0)));
    }

    #[test]
    fn remap_outline_tree_deep_first_chain_hits_depth_limit() {
        // A /First chain deeper than max_depth must error rather than overflow
        // the stack. depths entered: 40@0, 41@1, 42@2, 43@3 -> limit (3) fires.
        let bytes = build_min_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (40, "<< /Title (a) /First 41 0 R >>"),
                (41, "<< /Title (b) /First 42 0 R >>"),
                (42, "<< /Title (c) /First 43 0 R >>"),
                (43, "<< /Title (d) >>"),
            ],
            "",
        );
        let mut pdf = open(bytes);
        let surviving = Surviving::default();
        let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
        let err = remap_outline_tree(
            &mut pdf,
            ObjectRef::new(40, 0),
            0,
            3,
            &surviving,
            &mut visited,
        )
        .expect_err("depth limit must be enforced");
        assert!(matches!(err, Error::Unsupported(_)), "got {err:?}");
    }

    #[test]
    fn remap_name_tree_kids_cycle_terminates() {
        // /Kids back-edge cycle: node 50 /Kids [51], node 51 /Kids [50]. The
        // shared `visited` set stops at the revisited node instead of recursing
        // forever.
        let bytes = build_min_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (50, "<< /Kids [51 0 R] >>"),
                (51, "<< /Kids [50 0 R] >>"),
            ],
            "",
        );
        let mut pdf = open(bytes);
        let surviving = Surviving::default();
        let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
        remap_name_tree(
            &mut pdf,
            ObjectRef::new(50, 0),
            &surviving,
            0,
            100,
            &mut visited,
        )
        .expect("cyclic /Kids chain must terminate gracefully");
        assert!(visited.contains(&ObjectRef::new(50, 0)));
        assert!(visited.contains(&ObjectRef::new(51, 0)));
    }

    #[test]
    fn remap_name_tree_deep_kids_chain_hits_depth_limit() {
        // A /Kids chain deeper than max_depth must error. Depths entered:
        // 50@0, 51@1, 52@2, 53@3 -> limit (3) fires before node 53 is read.
        let bytes = build_min_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (50, "<< /Kids [51 0 R] >>"),
                (51, "<< /Kids [52 0 R] >>"),
                (52, "<< /Kids [53 0 R] >>"),
                (53, "<< /Names [(z) [3 0 R /Fit]] >>"),
            ],
            "",
        );
        let mut pdf = open(bytes);
        let surviving = Surviving::default();
        let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
        let err = remap_name_tree(
            &mut pdf,
            ObjectRef::new(50, 0),
            &surviving,
            0,
            3,
            &mut visited,
        )
        .expect_err("depth limit must be enforced");
        assert!(matches!(err, Error::Unsupported(_)), "got {err:?}");
    }

    // -----------------------------------------------------------------------
    // qpdf null-out parity: outline items / named dests are NEVER dropped;
    // surviving-page dests are remapped; a removed page still referenced by a
    // kept dest is emitted as `null`; /Count, /Limits, and sibling links are
    // left unchanged. Oracle: qpdf 11.9.0
    // `--static-id in.pdf --pages in.pdf 1,3 -- out.pdf`.
    // -----------------------------------------------------------------------

    #[test]
    fn nullout_named_dests_kept_removed_pages_nulled() {
        let mut pdf = open(build_outline_pdf());
        let pages = vec![ObjectRef::new(3, 0), ObjectRef::new(5, 0)];
        let result = rebuild_page_tree(&mut pdf, &pages).unwrap();
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        // ALL four named dests are still present (none dropped), in original
        // order; /Limits not removed.
        let leaf = dict_of(&mut pdf, ObjectRef::new(30, 0));
        let Some(Object::Array(names)) = leaf.get("Names") else {
            panic!("/Names array");
        };
        let keys: Vec<&[u8]> = names
            .iter()
            .step_by(2)
            .filter_map(|o| match o {
                Object::String(b) | Object::Name(b) => Some(b.as_slice()),
                _ => None,
            })
            .collect();
        assert_eq!(
            keys,
            vec![
                b"dest_named_p4".as_slice(),
                b"dest_p1",
                b"dest_p2",
                b"dest_p3"
            ],
            "all named dests kept in original order"
        );
        assert!(leaf.get("Limits").is_some(), "/Limits not removed");

        // Surviving dests remapped; removed-page dests point at a now-null page.
        let new_p1 = result.ref_map[&ObjectRef::new(3, 0)][0];
        let new_p3 = result.ref_map[&ObjectRef::new(5, 0)][0];
        let dest_of = |names: &[Object], k: &[u8]| -> Object {
            let i = names
                .iter()
                .step_by(2)
                .position(|o| matches!(o, Object::String(b) | Object::Name(b) if b == k))
                .unwrap();
            names[i * 2 + 1].clone()
        };
        let arr_first = |o: &Object| -> ObjectRef {
            o.as_array().unwrap().first().unwrap().as_ref_id().unwrap()
        };
        assert_eq!(arr_first(&dest_of(names, b"dest_p1")), new_p1);
        assert_eq!(arr_first(&dest_of(names, b"dest_p3")), new_p3);
        // dest_p2 -> obj4 (page 2 removed): kept, target nulled.
        assert_eq!(
            arr_first(&dest_of(names, b"dest_p2")),
            ObjectRef::new(4, 0),
            "removed-page dest keeps its original ref"
        );
        assert!(
            matches!(pdf.resolve(ObjectRef::new(4, 0)).unwrap(), Object::Null),
            "removed page 2 (obj4) nulled"
        );
        assert!(
            matches!(pdf.resolve(ObjectRef::new(6, 0)).unwrap(), Object::Null),
            "removed page 4 (obj6) nulled (referenced by dest_named_p4)"
        );
    }

    #[test]
    fn nullout_outline_items_all_kept_count_and_links_unchanged() {
        let mut pdf = open(build_outline_pdf());
        let result =
            rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0), ObjectRef::new(5, 0)]).unwrap();
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        // /Outlines retained; root /Count and First/Last unchanged.
        let cat = dict_of(&mut pdf, ObjectRef::new(1, 0));
        let root = dict_of(
            &mut pdf,
            cat.get_ref("Outlines").expect("/Outlines retained"),
        );
        assert_eq!(
            root.get("Count"),
            Some(&Object::Integer(5)),
            "root /Count unchanged"
        );
        assert_eq!(root.get_ref("First"), Some(ObjectRef::new(20, 0)));
        assert_eq!(root.get_ref("Last"), Some(ObjectRef::new(23, 0)));

        // Item 21 (GoTo action -> removed page 2/obj4): KEPT, links intact.
        let i21 = dict_of(&mut pdf, ObjectRef::new(21, 0));
        assert_eq!(i21.get_ref("Prev"), Some(ObjectRef::new(20, 0)));
        assert_eq!(i21.get_ref("Next"), Some(ObjectRef::new(22, 0)));
        assert!(matches!(
            pdf.resolve(ObjectRef::new(4, 0)).unwrap(),
            Object::Null
        ));

        // Item 23 (string /Dest to dest_named_p4 -> removed page): KEPT (qpdf
        // does not drop string-dest items even when the named dest is nulled).
        let i23 = dict_of(&mut pdf, ObjectRef::new(23, 0));
        assert_eq!(i23.get_ref("Prev"), Some(ObjectRef::new(22, 0)));

        // Item 22 (/Dest [5 0 R] surviving) keeps /Count 1 and its child 24.
        let i22 = dict_of(&mut pdf, ObjectRef::new(22, 0));
        assert_eq!(i22.get("Count"), Some(&Object::Integer(1)));
        assert_eq!(i22.get_ref("First"), Some(ObjectRef::new(24, 0)));
    }

    // -----------------------------------------------------------------------
    // Link-annotation and /OpenAction destination null-out (qpdf --pages)
    // -----------------------------------------------------------------------

    /// Build a 3-page PDF (objs 3,4,5 = page1,2,3) where page1 carries a single
    /// link annotation (obj 50) whose body is `annot_body`, plus optional extra
    /// objects and catalog entries. Used by the annotation/OpenAction tests.
    fn build_annot_pdf(
        annot_body: &str,
        catalog_extra: &str,
        extra_objs: &[(u32, &str)],
    ) -> Vec<u8> {
        let mut objs: Vec<(u32, String)> = vec![
            (
                1,
                format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>"),
            ),
            (
                2,
                "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>".to_string(),
            ),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [50 0 R] >>"
                    .to_string(),
            ),
            (
                4,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_string(),
            ),
            (
                5,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_string(),
            ),
            (50, annot_body.to_string()),
        ];
        for (n, body) in extra_objs {
            objs.push((*n, (*body).to_string()));
        }
        let refs: Vec<(u32, &str)> = objs.iter().map(|(n, b)| (*n, b.as_str())).collect();
        build_min_pdf(&refs, "")
    }

    /// Build a 3-page PDF whose page1 carries an INLINE (direct-dict) annotation
    /// in its `/Annots` array (no indirect annot object).
    fn build_inline_annot_pdf(annot_inline: &str, catalog_extra: &str) -> Vec<u8> {
        let catalog = format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>");
        let page1 = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [ {annot_inline} ] >>"
        );
        build_min_pdf(
            &[
                (1, catalog.as_str()),
                (2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>"),
                (3, page1.as_str()),
                (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (5, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            ],
            "",
        )
    }

    /// First page-ref element of an array-form dest on an object.
    fn dest_array_first(pdf: &mut Pdf<Cursor<Vec<u8>>>, r: ObjectRef, key: &str) -> ObjectRef {
        let d = dict_of(pdf, r);
        d.get(key)
            .and_then(Object::as_array)
            .and_then(|a| a.first())
            .and_then(Object::as_ref_id)
            .unwrap_or_else(|| panic!("{r} /{key} should be an array dest with a page ref"))
    }

    #[test]
    fn annot_dest_to_removed_page_is_nulled() {
        // page1 link annot /Dest [4 0 R /Fit] targets page2 (removed). No
        // outline, no named dests. qpdf null-out: obj4 -> null, /Dest verbatim.
        let mut pdf = open(build_annot_pdf(
            "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /Dest [4 0 R /Fit] >>",
            "",
            &[],
        ));
        let result =
            rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0), ObjectRef::new(5, 0)]).unwrap();
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        assert!(
            matches!(pdf.resolve(ObjectRef::new(4, 0)).unwrap(), Object::Null),
            "removed page2 (obj4) should be nulled"
        );
        assert_eq!(
            dest_array_first(&mut pdf, ObjectRef::new(50, 0), "Dest"),
            ObjectRef::new(4, 0),
            "annot /Dest keeps the now-null page ref verbatim"
        );
    }

    #[test]
    fn annot_goto_action_to_removed_page_is_nulled() {
        // page1 link annot /A << /S /GoTo /D [4 0 R /Fit] >> -> page2 (removed).
        let mut pdf = open(build_annot_pdf(
            "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] \
             /A << /S /GoTo /D [4 0 R /Fit] >> >>",
            "",
            &[],
        ));
        let result =
            rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0), ObjectRef::new(5, 0)]).unwrap();
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        assert!(
            matches!(pdf.resolve(ObjectRef::new(4, 0)).unwrap(), Object::Null),
            "removed page2 (obj4) should be nulled"
        );
        let annot = dict_of(&mut pdf, ObjectRef::new(50, 0));
        let action = annot
            .get("A")
            .and_then(Object::as_dict)
            .expect("annot /A should be an action dict");
        let d_first = action
            .get("D")
            .and_then(Object::as_array)
            .and_then(|a| a.first())
            .and_then(Object::as_ref_id)
            .expect("/A /D should be an array dest");
        assert_eq!(
            d_first,
            ObjectRef::new(4, 0),
            "annot /A /D keeps the now-null page ref verbatim"
        );
    }

    #[test]
    fn open_action_goto_to_removed_page_is_nulled() {
        // catalog /OpenAction << /S /GoTo /D [4 0 R /Fit] >> -> page2 (removed).
        let mut pdf = open(build_annot_pdf(
            "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] >>",
            "/OpenAction << /S /GoTo /D [4 0 R /Fit] >>",
            &[],
        ));
        let result =
            rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0), ObjectRef::new(5, 0)]).unwrap();
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        assert!(
            matches!(pdf.resolve(ObjectRef::new(4, 0)).unwrap(), Object::Null),
            "removed page2 (obj4) reached only via /OpenAction should be nulled"
        );
    }

    #[test]
    fn open_action_dest_array_to_removed_page_is_nulled() {
        // catalog /OpenAction [4 0 R /Fit] (destination-array form) -> page2.
        let mut pdf = open(build_annot_pdf(
            "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] >>",
            "/OpenAction [4 0 R /Fit]",
            &[],
        ));
        let result =
            rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0), ObjectRef::new(5, 0)]).unwrap();
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        assert!(
            matches!(pdf.resolve(ObjectRef::new(4, 0)).unwrap(), Object::Null),
            "removed page2 (obj4) reached only via /OpenAction array should be nulled"
        );
    }

    #[test]
    fn annot_dest_to_surviving_page_is_remapped() {
        // page1 link annot /Dest [5 0 R /Fit] -> page3, which SURVIVES. qpdf
        // remaps a surviving dest to the page's new ref. In single-input
        // --pages a first-materialized page keeps its original object number, so
        // rebuild_page_tree only ever produces identity mappings; to exercise a
        // genuine non-identity remap (regression guard) we hand-build a
        // RebuildResult mapping obj5 -> a fresh ref (obj99), mirroring the
        // synthetic-RebuildResult precedent in the all_pages_removed_* tests.
        let mut pdf = open(build_annot_pdf(
            "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /Dest [5 0 R /Fit] >>",
            "",
            &[],
        ));
        let mut ref_map: BTreeMap<ObjectRef, Vec<ObjectRef>> = BTreeMap::new();
        ref_map.insert(ObjectRef::new(5, 0), vec![ObjectRef::new(99, 0)]);
        let result = synthetic_result(&mut pdf, vec![ObjectRef::new(3, 0)], ref_map);
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        assert_eq!(
            dest_array_first(&mut pdf, ObjectRef::new(50, 0), "Dest"),
            ObjectRef::new(99, 0),
            "annot /Dest to a surviving page should be remapped to its new ref"
        );
    }

    #[test]
    fn inline_annot_dest_to_removed_page_is_nulled() {
        // page1 /Annots holds an INLINE (direct-dict) link annot whose
        // /Dest [4 0 R /Fit] targets page2 (removed). qpdf 11.9.0 nulls the page
        // and keeps the inline annot's /Dest verbatim (verified empirically).
        let mut pdf = open(build_inline_annot_pdf(
            "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /Dest [4 0 R /Fit] >>",
            "",
        ));
        let result =
            rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0), ObjectRef::new(5, 0)]).unwrap();
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        assert!(
            matches!(pdf.resolve(ObjectRef::new(4, 0)).unwrap(), Object::Null),
            "removed page2 (obj4) reached only via an inline annot should be nulled"
        );
        let page = dict_of(&mut pdf, ObjectRef::new(3, 0));
        let inline = page
            .get("Annots")
            .and_then(Object::as_array)
            .and_then(|a| a.first())
            .and_then(Object::as_dict)
            .expect("inline annot dict in /Annots");
        let d_first = inline
            .get("Dest")
            .and_then(Object::as_array)
            .and_then(|a| a.first())
            .and_then(Object::as_ref_id)
            .expect("inline annot /Dest array");
        assert_eq!(
            d_first,
            ObjectRef::new(4, 0),
            "inline annot /Dest keeps the now-null page ref verbatim"
        );
    }

    #[test]
    fn inline_annot_dest_to_surviving_page_is_remapped() {
        // Inline annot /Dest [5 0 R /Fit] -> page3, which SURVIVES. A hand-built
        // non-identity RebuildResult (obj5 -> obj99) exercises the remap and the
        // /Annots array write-back path for inline annotations.
        let mut pdf = open(build_inline_annot_pdf(
            "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /Dest [5 0 R /Fit] >>",
            "",
        ));
        let mut ref_map: BTreeMap<ObjectRef, Vec<ObjectRef>> = BTreeMap::new();
        ref_map.insert(ObjectRef::new(5, 0), vec![ObjectRef::new(99, 0)]);
        let result = synthetic_result(&mut pdf, vec![ObjectRef::new(3, 0)], ref_map);
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        let page = dict_of(&mut pdf, ObjectRef::new(3, 0));
        let inline = page
            .get("Annots")
            .and_then(Object::as_array)
            .and_then(|a| a.first())
            .and_then(Object::as_dict)
            .expect("inline annot dict in /Annots");
        let d_first = inline
            .get("Dest")
            .and_then(Object::as_array)
            .and_then(|a| a.first())
            .and_then(Object::as_ref_id)
            .expect("inline annot /Dest array");
        assert_eq!(
            d_first,
            ObjectRef::new(99, 0),
            "inline annot /Dest to a surviving page should be remapped to its new ref"
        );
    }

    #[test]
    fn shared_indirect_annot_array_processed_once() {
        // Regression guard for the indirect-/Annots-array dedup. An INDIRECT
        // /Annots array object (obj60) shared by a duplicated surviving page
        // (obj3 twice in new_kids) holds an inline annot whose /Dest [5 0 R /Fit]
        // targets obj5, which survives and remaps to obj3 (surviving = {obj5 ->
        // obj3}). Without deduping the shared array object, the first pass remaps
        // the inline /Dest to [3 0 R]; the second pass would resolve that to obj3
        // -- not a key of `surviving` -- and null obj3, a SURVIVING page. The
        // array-object dedup skips the second pass so obj3 stays a dictionary.
        let mut pdf = open(build_min_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>"),
                (
                    3,
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots 60 0 R >>",
                ),
                (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (5, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (
                    60,
                    "[ << /Type /Annot /Subtype /Link /Rect [0 0 10 10] /Dest [5 0 R /Fit] >> ]",
                ),
            ],
            "",
        ));
        let mut ref_map: BTreeMap<ObjectRef, Vec<ObjectRef>> = BTreeMap::new();
        ref_map.insert(ObjectRef::new(5, 0), vec![ObjectRef::new(3, 0)]);
        let result = synthetic_result(
            &mut pdf,
            vec![ObjectRef::new(3, 0), ObjectRef::new(3, 0)],
            ref_map,
        );
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        assert!(
            pdf.resolve(ObjectRef::new(3, 0))
                .unwrap()
                .as_dict()
                .is_some(),
            "surviving page obj3 must stay a dictionary (shared indirect /Annots array deduped)"
        );
    }

    #[test]
    fn open_action_goto_to_surviving_page_is_remapped() {
        // catalog /OpenAction << /S /GoTo /D [5 0 R /Fit] >> -> page3, which
        // SURVIVES. The only case where remap_open_action_dest's catalog
        // re-store actually changes the document: remap_dest returns a
        // modified action dict and catalog.insert("OpenAction", ..) applies it.
        // As in annot_dest_to_surviving_page_is_remapped, a hand-built
        // RebuildResult maps obj5 -> a fresh ref (obj99) to exercise a genuine
        // non-identity remap.
        let mut pdf = open(build_annot_pdf(
            "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] >>",
            "/OpenAction << /S /GoTo /D [5 0 R /Fit] >>",
            &[],
        ));
        let mut ref_map: BTreeMap<ObjectRef, Vec<ObjectRef>> = BTreeMap::new();
        ref_map.insert(ObjectRef::new(5, 0), vec![ObjectRef::new(99, 0)]);
        let result = synthetic_result(&mut pdf, vec![ObjectRef::new(3, 0)], ref_map);
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        // /OpenAction is a GoTo action dict; descend to /D and check its first
        // element is the new ref.
        let cat = dict_of(&mut pdf, ObjectRef::new(1, 0));
        let action = cat
            .get("OpenAction")
            .and_then(Object::as_dict)
            .expect("catalog /OpenAction should be an action dict");
        let d_first = action
            .get("D")
            .and_then(Object::as_array)
            .and_then(|a| a.first())
            .and_then(Object::as_ref_id)
            .expect("/OpenAction /D should be an array dest");
        assert_eq!(
            d_first,
            ObjectRef::new(99, 0),
            "/OpenAction /D to a surviving page should be remapped to its new ref"
        );
    }

    #[test]
    fn inline_annot_non_goto_action_d_is_not_treated_as_dest() {
        // An inline annot whose /A is a NON-GoTo action (/S /Launch) carrying a
        // page-ref-shaped /D must NOT be treated as a local GoTo destination:
        // only /S /GoTo carries a local /D. With a synthetic non-identity remap
        // obj5 -> obj99, the /A /D must stay [5 0 R] verbatim (a /S /GoTo /D
        // would be remapped to [99 0 R]). obj5 survives, so it is not nulled;
        // the null-out (Step 0) is page-driven, not reached through this /D.
        let mut pdf = open(build_inline_annot_pdf(
            "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] \
             /A << /S /Launch /D [5 0 R /Fit] >> >>",
            "",
        ));
        let mut ref_map: BTreeMap<ObjectRef, Vec<ObjectRef>> = BTreeMap::new();
        ref_map.insert(ObjectRef::new(5, 0), vec![ObjectRef::new(99, 0)]);
        let result = synthetic_result(&mut pdf, vec![ObjectRef::new(3, 0)], ref_map);
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        let page = dict_of(&mut pdf, ObjectRef::new(3, 0));
        let d_first = page
            .get("Annots")
            .and_then(Object::as_array)
            .and_then(|a| a.first())
            .and_then(Object::as_dict)
            .and_then(|annot| annot.get("A"))
            .and_then(Object::as_dict)
            .and_then(|a| a.get("D"))
            .and_then(Object::as_array)
            .and_then(|arr| arr.first())
            .and_then(Object::as_ref_id)
            .expect("inline annot /A /D array first ref");
        assert_eq!(
            d_first,
            ObjectRef::new(5, 0),
            "a non-GoTo inline-annot /A /D must be left verbatim (not remapped)"
        );
    }

    #[test]
    fn open_action_non_goto_d_is_not_treated_as_dest() {
        // catalog /OpenAction << /S /Launch /D [5 0 R /Fit] >>: a non-GoTo
        // action's /D is not a local destination, so it must NOT be remapped.
        // With a synthetic non-identity remap obj5 -> obj99, the /D stays
        // [5 0 R] verbatim (a /S /GoTo /D would be remapped to [99 0 R]).
        let mut pdf = open(build_annot_pdf(
            "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] >>",
            "/OpenAction << /S /Launch /D [5 0 R /Fit] >>",
            &[],
        ));
        let mut ref_map: BTreeMap<ObjectRef, Vec<ObjectRef>> = BTreeMap::new();
        ref_map.insert(ObjectRef::new(5, 0), vec![ObjectRef::new(99, 0)]);
        let result = synthetic_result(&mut pdf, vec![ObjectRef::new(3, 0)], ref_map);
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        let catalog = dict_of(&mut pdf, ObjectRef::new(1, 0));
        let d_first = catalog
            .get("OpenAction")
            .and_then(Object::as_dict)
            .and_then(|oa| oa.get("D"))
            .and_then(Object::as_array)
            .and_then(|a| a.first())
            .and_then(Object::as_ref_id)
            .expect("/OpenAction /D array first ref");
        assert_eq!(
            d_first,
            ObjectRef::new(5, 0),
            "a non-GoTo /OpenAction /D must be left verbatim (not remapped)"
        );
    }

    #[test]
    fn shared_annot_on_duplicated_page_processed_once() {
        // Regression guard for the function-scope `visited` dedup set in
        // remap_annot_dests (Step 4). Under a duplicate-page selection the same
        // surviving page (obj3) appears twice in new_kids, so its shared indirect
        // annotation (obj50) is reached twice. obj50 /Dest [5 0 R /Fit] targets
        // obj5, which SURVIVES and remaps to obj3 (surviving = {obj5 -> obj3}).
        //
        // Without the guard, the first pass remaps the annot /Dest in place to
        // [3 0 R]; the second pass then resolves that already-remapped dest to
        // obj3, which is NOT a key of `surviving` (keys are OLD refs), and
        // misclassifies it as a removed target -- nulling obj3, a SURVIVING page.
        // The guard skips the already-processed annot so obj3 stays a dictionary.
        let mut pdf = open(build_annot_pdf(
            "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /Dest [5 0 R /Fit] >>",
            "",
            &[],
        ));
        let mut ref_map: BTreeMap<ObjectRef, Vec<ObjectRef>> = BTreeMap::new();
        ref_map.insert(ObjectRef::new(5, 0), vec![ObjectRef::new(3, 0)]);
        // Same surviving page listed twice, so the shared annot obj50 is
        // reached from both clones (duplicate-page selection).
        let result = synthetic_result(
            &mut pdf,
            vec![ObjectRef::new(3, 0), ObjectRef::new(3, 0)],
            ref_map,
        );
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        assert!(
            pdf.resolve(ObjectRef::new(3, 0))
                .unwrap()
                .as_dict()
                .is_some(),
            "surviving page obj3 must stay a dictionary; a shared annot reached \
             from two duplicated pages must be processed once, never nulling it"
        );
    }

    #[test]
    fn shared_goto_action_across_outline_and_annot_does_not_null_remapped_page() {
        // Cross-pass hazard guard. An indirect GoTo action object (obj50) is
        // shared by an outline item (obj20, Step 3) and a link annotation (obj60
        // on the surviving page, Step 4). Its /D [3 0 R /Fit] targets obj3, which
        // SURVIVES and remaps to a fresh ref obj99 (a non-identity remap, built
        // by hand because single-input rebuild_page_tree only ever yields an
        // identity map).
        //
        // Step 3 rewrites the shared action's /D in place to [99 0 R]. Step 4
        // then re-resolves the SAME action and, keying "removed?" on `surviving`'s
        // OLD refs alone, would see obj99 (a NEW ref, absent from the keys) and
        // null it -- the surviving output page. The Step-3 and Step-4 `visited`
        // sets are independent and neither ever holds the shared action object,
        // so traversal dedup cannot prevent this; only treating a rebuilt output
        // ref as surviving does. obj99 must stay a dictionary.
        let mut pdf = open(build_min_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R >>"),
                (2, "<< /Type /Pages /Kids [99 0 R] /Count 1 >>"),
                (
                    99,
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [60 0 R] >>",
                ),
                (
                    10,
                    "<< /Type /Outlines /First 20 0 R /Last 20 0 R /Count 1 >>",
                ),
                (20, "<< /Title (Shared) /Parent 10 0 R /A 50 0 R >>"),
                (
                    60,
                    "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /A 50 0 R >>",
                ),
                (50, "<< /S /GoTo /D [3 0 R /Fit] >>"),
            ],
            "",
        ));
        let mut ref_map: BTreeMap<ObjectRef, Vec<ObjectRef>> = BTreeMap::new();
        ref_map.insert(ObjectRef::new(3, 0), vec![ObjectRef::new(99, 0)]);
        let result = synthetic_result(&mut pdf, vec![ObjectRef::new(99, 0)], ref_map);
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        assert!(
            pdf.resolve(ObjectRef::new(99, 0))
                .unwrap()
                .as_dict()
                .is_some(),
            "surviving remapped page obj99 must stay a dictionary; a GoTo action \
             shared by an outline item and an annotation must not let the Step-4 \
             null-pass null an already-remapped (new-ref) destination"
        );
    }

    #[test]
    fn genuinely_removed_page_is_still_nulled_under_surviving_target_guard() {
        // The surviving-target guard must not over-skip: a destination whose
        // target page is neither a surviving source ref (a remap key) nor a
        // rebuilt output ref (a `new_kids` member) is genuinely removed and must
        // still be nulled. obj99 survives (identity remap) and obj7 is a real
        // page in the original page tree (both objs are in /Kids) but absent
        // from ref_map → a removed page → must be nulled. obj99 is untouched.
        let mut pdf = open(build_min_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R >>"),
                (2, "<< /Type /Pages /Kids [99 0 R 7 0 R] /Count 2 >>"),
                (
                    99,
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
                ),
                (
                    10,
                    "<< /Type /Outlines /First 20 0 R /Last 20 0 R /Count 1 >>",
                ),
                (
                    20,
                    "<< /Title (Removed) /Parent 10 0 R /Dest [7 0 R /Fit] >>",
                ),
                (7, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            ],
            "",
        ));
        let mut ref_map: BTreeMap<ObjectRef, Vec<ObjectRef>> = BTreeMap::new();
        ref_map.insert(ObjectRef::new(99, 0), vec![ObjectRef::new(99, 0)]);
        let result = synthetic_result(&mut pdf, vec![ObjectRef::new(99, 0)], ref_map);
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        assert!(
            matches!(pdf.resolve(ObjectRef::new(7, 0)).unwrap(), Object::Null),
            "a removed page (neither a remap key nor a rebuilt output ref) must \
             still be nulled by the null-pass"
        );
        assert!(
            pdf.resolve(ObjectRef::new(99, 0))
                .unwrap()
                .as_dict()
                .is_some(),
            "the surviving page obj99 must be untouched"
        );
    }

    #[test]
    fn direct_dict_dests_node_under_names_remapped_and_nulled() {
        // /Names is indirect (obj11) but its /Dests is held as a DIRECT name-tree
        // leaf dict (not an indirect node root), exercising the
        // `remap_name_tree_node_dict` path. Keep page1 (obj3), remove page2
        // (obj4): d1 is remapped to the new page1 ref; d2 is kept verbatim and
        // obj4 is nulled.
        let mut pdf = open(build_min_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R /Names 11 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (
                    11,
                    "<< /Dests << /Names [(d1) [3 0 R /Fit] (d2) [4 0 R /Fit]] >> >>",
                ),
            ],
            "",
        ));
        let result = rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
        let new_p1 = result.ref_map[&ObjectRef::new(3, 0)][0];
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        // obj11 /Dests is rewritten in place; read its leaf /Names pairs.
        let names_dict = dict_of(&mut pdf, ObjectRef::new(11, 0));
        let dests = names_dict
            .get("Dests")
            .and_then(Object::as_dict)
            .expect("/Dests direct dict retained");
        let pairs = dests
            .get("Names")
            .and_then(Object::as_array)
            .expect("/Dests leaf /Names array expected");
        assert_eq!(
            pairs[1]
                .as_array()
                .and_then(|a| a.first())
                .and_then(Object::as_ref_id),
            Some(new_p1),
            "d1 dest should be remapped to the new page1 ref"
        );
        assert!(
            matches!(pdf.resolve(ObjectRef::new(4, 0)).unwrap(), Object::Null),
            "removed page2 (obj4) should be nulled"
        );
    }

    #[test]
    fn legacy_indirect_catalog_dests_remapped_and_nulled() {
        // Legacy (PDF 1.1) /Catalog /Dests held as an INDIRECT reference (obj30),
        // exercising the `remap_legacy_dests` path. Keep page1 (obj3), remove
        // page2 (obj4).
        let mut pdf = open(build_min_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R /Dests 30 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
                (30, "<< /d1 [3 0 R /Fit] /d2 [4 0 R /Fit] >>"),
            ],
            "",
        ));
        let result = rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
        let new_p1 = result.ref_map[&ObjectRef::new(3, 0)][0];
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        assert_eq!(
            dest_array_first(&mut pdf, ObjectRef::new(30, 0), "d1"),
            new_p1,
            "legacy /Dests d1 should be remapped to the new page1 ref"
        );
        assert!(
            matches!(pdf.resolve(ObjectRef::new(4, 0)).unwrap(), Object::Null),
            "removed page2 (obj4) should be nulled"
        );
    }
}
