//! Multi-document page merge (qpdf `--pages` parity).
//!
//! [`merge_documents`] copies selected pages from N source documents into one
//! fresh target. `inputs[0]` is the primary: its document-level information
//! (outlines, named destinations, AcroForm `/DR` `/DA`) is inherited; later
//! inputs contribute pages and form fields only. Shared resources within one
//! input are de-duplicated; form-field name collisions are resolved by qpdf's
//! `<name>+<N>` renaming rule.

use crate::object_copy::copy_objects;
use crate::outline_dest_remap::{dest_page_ref_resolved, resolve_ref_chain};
use crate::page_closure::page_object_closure;
use crate::page_extract::{
    append_selection_kids, materialize_leaf, minimal_target_bytes, p_target_page_ref, resolve_dict,
    sd_target_page_ref, target_pages_root, InheritedAttrs,
};
use crate::pages::{page_refs, DEFAULT_MAX_PAGE_TREE_DEPTH};
use crate::subset_prune::sweep_unreachable_objects;
use crate::{Error, Object, ObjectRef, Pdf, Result};
use std::collections::BTreeSet;
use std::io::{Cursor, Read, Seek};

/// One merge input: an opened source document and the 0-based page indices to
/// take from it (arbitrary order, duplicates allowed).
pub struct MergeInput<'a, R: Read + Seek> {
    /// The opened source document.
    pub source: &'a mut Pdf<R>,
    /// 0-based page indices to copy, in output order.
    pub pages: Vec<usize>,
}

/// Inline-`/Next`-nesting bound for action-chain traversal, mirroring extract's
/// `MAX_ACTION_CHAIN_DEPTH` so a cyclic or pathologically deep inline `/Next`
/// chain terminates. Indirect `/Next` cycles are additionally bounded by a
/// per-walk `visited` set.
const MAX_ACTION_CHAIN_DEPTH: usize = 64;

/// Collect the source page references reachable from `page_ref` through any of
/// the same carriers extract's neutralize-drop path covers (see
/// [`crate::page_extract`] `neutralize_absent_dests` and its helpers
/// `neutralize_action_chain`, `neutralize_action_array`,
/// `neutralize_aa_if_absent`, `dest_targets_absent_page`,
/// `sd_target_page_ref`, `p_target_page_ref`, and the bead ring), keeping only
/// targets that are *not* in `selected` (the input's chosen pages). Those are
/// the removed pages a destination still points at; for qpdf `--pages` null-out
/// parity they are copied as placeholders and then replaced with `null`, so the
/// reference survives but resolves to a null page object.
///
/// The carriers, per page, are:
/// - each `/Annots` annotation's `/Dest`, `/A` action chain, `/AA` actions,
///   and `/P` (the annotation's owning page);
/// - the page's own `/AA` additional actions;
/// - the article-thread bead ring reachable from the page's `/B` (each bead's
///   `/P`).
///
/// Within each action chain, both `/GoTo /D` page destinations and `/GoTo /SD`
/// structure destinations are followed, as are `/Next` continuations (single
/// action or array form). This set MUST stay equal to neutralize's carrier set:
/// merge drops no carrier, so every reference [`page_object_closure`] copies and
/// `copy_objects` remaps to a removed page must be named here, else a removed
/// page survives un-nulled as a live orphan. Keep the two paths in sync.
///
/// Indirect references are resolved throughout via [`resolve_ref_chain`] /
/// [`dest_page_ref_resolved`] / [`sd_target_page_ref`] / [`p_target_page_ref`],
/// which bound the indirection. Named/string/external destinations carry no
/// in-document page reference and contribute nothing.
fn collect_removed_dest_targets<R: Read + Seek>(
    source: &mut Pdf<R>,
    page_ref: ObjectRef,
    selected: &BTreeSet<ObjectRef>,
    removed: &mut BTreeSet<ObjectRef>,
) -> Result<()> {
    // Detach the values that need a `&mut source` resolve before mutating the
    // borrow: the raw /Annots value and the page's own /AA value.
    let (annots_val, page_aa): (Option<Object>, Option<Object>) = {
        let page_obj = source.resolve_borrowed(page_ref)?;
        let Some(page_dict) = page_obj.as_dict() else {
            return Ok(()); // cov:ignore: malformed input — a page ref always resolves to a dictionary
        };
        (
            page_dict.get("Annots").cloned(),
            page_dict.get("AA").cloned(),
        )
    };

    // /Annots may be an inline array or an indirect reference to one (mirrors
    // neutralize_absent_dests).
    let annot_refs: Vec<ObjectRef> = match annots_val {
        Some(Object::Array(arr)) => arr.iter().filter_map(Object::as_ref_id).collect(),
        Some(Object::Reference(r)) => match source.resolve_borrowed(r)? {
            Object::Array(arr) => arr.iter().filter_map(Object::as_ref_id).collect(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    for annot_ref in annot_refs {
        let Some(annot) = source.resolve_borrowed(annot_ref)?.as_dict().cloned() else {
            continue;
        };
        // /Dest — explicit array/dict destination (mirrors
        // neutralize_annot_if_absent's /Dest arm).
        if let Some(dest) = annot.get("Dest") {
            collect_dest_target(source, dest, selected, removed)?;
        }
        // /A — an action chain (mirrors the /A arm via neutralize_action_chain).
        if let Some(a_val) = annot.get("A") {
            let mut visited = BTreeSet::new();
            collect_action_chain_targets(
                source,
                a_val,
                selected,
                removed,
                &mut visited,
                MAX_ACTION_CHAIN_DEPTH,
            )?; // cov:ignore: `?` Err arm — resolve cannot fail on already-opened action objects
        }
        // Annotation-level /AA (e.g. a widget's /E enter, /X exit actions);
        // each entry is an action chain (mirrors neutralize_aa_if_absent).
        if let Some(aa_val) = annot.get("AA") {
            collect_aa_dest_targets(source, aa_val, selected, removed)?;
        }
        // /P — the annotation's owning page. A malformed /P pointing at a
        // removed (sibling) page keeps that page reachable (mirrors the /P arm
        // in neutralize_annot_if_absent, via p_target_page_ref).
        if let Some(p_val) = annot.get("P") {
            collect_p_target(source, p_val, selected, removed)?;
        }
    }

    // Page-level /AA: each entry (/O, /C, …) is an action chain (mirrors the
    // page-level /AA arm in neutralize_absent_dests).
    if let Some(aa_val) = page_aa {
        collect_aa_dest_targets(source, &aa_val, selected, removed)?;
    }

    // Article-thread bead ring (mirrors neutralize_bead_ring).
    collect_bead_ring_targets(source, page_ref, selected, removed)?;

    Ok(())
}

/// Scan every entry of an additional-actions (`/AA`) value for destinations
/// targeting a removed page. The value may be an inline dict or an indirect
/// reference to one ([`resolve_ref_chain`] bounds the indirection). Mirrors
/// extract's `neutralize_aa_if_absent` (collect-only).
fn collect_aa_dest_targets<R: Read + Seek>(
    source: &mut Pdf<R>,
    aa_value: &Object,
    selected: &BTreeSet<ObjectRef>,
    removed: &mut BTreeSet<ObjectRef>,
) -> Result<()> {
    let (concrete, _) = resolve_ref_chain(source, aa_value)?;
    // A non-dictionary /AA is malformed; nothing to scan.
    if let Some(aa) = concrete.into_dict() {
        for (_key, sub) in aa.iter() {
            // Each subaction is an independent chain; reset `visited` per entry,
            // matching neutralize_aa_if_absent.
            let mut visited = BTreeSet::new();
            collect_action_chain_targets(
                source,
                sub,
                selected,
                removed,
                &mut visited,
                MAX_ACTION_CHAIN_DEPTH,
            )?; // cov:ignore: `?` Err arm — resolve cannot fail on already-opened action objects
        }
    }
    Ok(())
}

/// Resolve `dest` to its target page reference and record it in `removed` when
/// it is not in `selected`.
fn collect_dest_target<R: Read + Seek>(
    source: &mut Pdf<R>,
    dest: &Object,
    selected: &BTreeSet<ObjectRef>,
    removed: &mut BTreeSet<ObjectRef>,
) -> Result<()> {
    if let Some(target) = dest_page_ref_resolved(source, dest)? {
        if !selected.contains(&target) {
            removed.insert(target);
        }
    }
    Ok(())
}

/// Resolve a `/P` (annotation or bead owning-page) to its Page reference and
/// record it in `removed` when not in `selected`. Non-Page `/P` values resolve
/// to `None` and contribute nothing.
fn collect_p_target<R: Read + Seek>(
    source: &mut Pdf<R>,
    p: &Object,
    selected: &BTreeSet<ObjectRef>,
    removed: &mut BTreeSet<ObjectRef>,
) -> Result<()> {
    if let Some(target) = p_target_page_ref(source, p)? {
        if !selected.contains(&target) {
            removed.insert(target);
        }
    }
    Ok(())
}

/// Walk an action value (`/A`, an `/AA` subaction, or a `/Next` element),
/// recording the target page of every `/GoTo /D` and `/GoTo /SD` whose
/// destination is a removed page, and following `/Next` continuations (single
/// action or array). Mirrors extract's `neutralize_action_chain` /
/// `neutralize_action_array` (collect-only): indirect cycles are bounded by
/// `visited`, inline `/Next` nesting by `depth`.
fn collect_action_chain_targets<R: Read + Seek>(
    source: &mut Pdf<R>,
    action_value: &Object,
    selected: &BTreeSet<ObjectRef>,
    removed: &mut BTreeSet<ObjectRef>,
    visited: &mut BTreeSet<ObjectRef>,
    depth: usize,
) -> Result<()> {
    if depth == 0 {
        return Ok(()); // cov:ignore: inline /Next nesting deeper than MAX_ACTION_CHAIN_DEPTH (DoS bound)
    }

    let (concrete, terminal_ref) = resolve_ref_chain(source, action_value)?;
    // Indirect-cycle guard: stop if we have already entered this action object.
    if let Some(r) = terminal_ref {
        if !visited.insert(r) {
            return Ok(());
        }
    }

    // An action value may be an ARRAY of actions (ISO 32000-1 §12.6.3: `/Next`
    // may be a single action or an array). Handle it before the dict path so an
    // array stored as a separate object is not silently skipped.
    if let Object::Array(elems) = concrete {
        for elem in &elems {
            collect_action_chain_targets(source, elem, selected, removed, visited, depth - 1)?;
        }
        return Ok(());
    }

    let Some(action) = concrete.into_dict() else {
        return Ok(());
    };
    let is_goto = matches!(action.get("S"), Some(Object::Name(n)) if n == b"GoTo");
    if is_goto {
        if let Some(d_val) = action.get("D") {
            collect_dest_target(source, d_val, selected, removed)?;
        }
        if let Some(sd_val) = action.get("SD") {
            collect_sd_target(source, sd_val, selected, removed)?;
        }
    }

    // /Next — a single action or an array of actions. Recurse into each.
    if let Some(next_val) = action.get("Next") {
        match next_val {
            Object::Array(elems) => {
                // Each array element is an independent action chain, recursed at
                // `depth - 1` (matching extract's neutralize_action_array so the
                // inline-nesting bound stays identical across the two paths).
                for elem in elems {
                    collect_action_chain_targets(
                        source,
                        elem,
                        selected,
                        removed,
                        visited,
                        depth - 1,
                    )?; // cov:ignore: `?` Err arm — resolve cannot fail on already-opened action objects
                }
            }
            single => {
                collect_action_chain_targets(
                    source,
                    single,
                    selected,
                    removed,
                    visited,
                    depth - 1,
                )?; // cov:ignore: `?` Err arm — resolve cannot fail on already-opened action objects
            }
        }
    }
    Ok(())
}

/// Resolve a `/GoTo /SD` structure destination to its target page (via the
/// StructElem `/Pg` hop) and record it in `removed` when not in `selected`.
/// Mirrors extract's `sd_targets_absent_page` (shares [`sd_target_page_ref`]).
fn collect_sd_target<R: Read + Seek>(
    source: &mut Pdf<R>,
    sd: &Object,
    selected: &BTreeSet<ObjectRef>,
    removed: &mut BTreeSet<ObjectRef>,
) -> Result<()> {
    if let Some(target) = sd_target_page_ref(source, sd)? {
        if !selected.contains(&target) {
            removed.insert(target);
        }
    }
    Ok(())
}

/// Walk the article-thread bead ring reachable from `page_ref`'s `/B`, recording
/// every bead's `/P` that targets a removed page. Mirrors extract's
/// `neutralize_bead_ring` (collect-only): `/B`, `/N`, `/V` may each be an
/// indirect-reference chain, and the ring is bounded by `visited` (each bead
/// handled once).
fn collect_bead_ring_targets<R: Read + Seek>(
    source: &mut Pdf<R>,
    page_ref: ObjectRef,
    selected: &BTreeSet<ObjectRef>,
    removed: &mut BTreeSet<ObjectRef>,
) -> Result<()> {
    let b_val = {
        let page_obj = source.resolve_borrowed(page_ref)?;
        let Some(page_dict) = page_obj.as_dict() else {
            return Ok(()); // cov:ignore: malformed input — a page ref always resolves to a dictionary
        };
        page_dict.get("B").cloned()
    };
    let Some(b_val) = b_val else {
        return Ok(());
    };
    // /B may itself be an indirect reference to the bead array; normalize it.
    let (b_concrete, _) = resolve_ref_chain(source, &b_val)?;
    let Object::Array(beads) = b_concrete else {
        return Ok(());
    };
    let mut queue: Vec<ObjectRef> = beads.iter().filter_map(Object::as_ref_id).collect();
    let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
    while let Some(start_ref) = queue.pop() {
        let (concrete, terminal) = resolve_ref_chain(source, &Object::Reference(start_ref))?;
        let bead_ref = terminal.unwrap_or(start_ref);
        if !visited.insert(bead_ref) {
            continue;
        }
        let Some(bead) = concrete.into_dict() else {
            continue;
        };
        // Enqueue ring neighbours.
        for key in ["N", "V"] {
            if let Some(Object::Reference(r)) = bead.get(key) {
                queue.push(*r);
            }
        }
        if let Some(p_val) = bead.get("P") {
            collect_p_target(source, p_val, selected, removed)?;
        }
    }
    Ok(())
}

/// Merge selected pages from N sources into one fresh document.
///
/// Returns an owned in-memory [`Pdf`] whose catalog has a single-level
/// `/Pages` tree containing the selected pages from every input, concatenated
/// in input order and, within each input, in the order given by that input's
/// `pages`. Each input is copied in a single pass with one renumbering map, so
/// objects shared between selected pages of the same input (fonts, images,
/// content streams) appear once per input in the output.
///
/// Inherited page attributes (`/Resources`, `/MediaBox`, `/CropBox`,
/// `/Rotate`) are materialized onto each copied page from its source page
/// tree, and a page selected more than once within an input becomes a shallow
/// clone of its first copy, matching [`extract_pages`](crate::extract_pages).
///
/// Each source is left unmodified. Write the result with
/// [`write_pdf`](crate::write_pdf) or
/// [`write_pdf_with_options`](crate::write_pdf_with_options).
///
/// # Errors
///
/// - [`Error::Unsupported`] if `inputs` is empty, or if a requested page index
///   is out of range for its input.
/// - Propagates resolve/copy errors from the underlying primitives.
pub fn merge_documents<R: Read + Seek>(
    inputs: &mut [MergeInput<'_, R>],
) -> Result<Pdf<Cursor<Vec<u8>>>> {
    if inputs.is_empty() {
        return Err(Error::Unsupported(
            "merge requires at least one input".to_string(),
        ));
    }

    let mut target = Pdf::open_mem_owned(minimal_target_bytes())?;
    let pages_root_ref = target_pages_root(&mut target)?;

    // Output `/Kids`, accumulated across inputs in input/selection order.
    let mut kids: Vec<ObjectRef> = Vec::new();
    // Copied page objects already placed in `kids`, so a page selected more
    // than once becomes a shallow clone rather than a duplicated reference.
    let mut used: BTreeSet<ObjectRef> = BTreeSet::new();
    // Every page object copied into the target (the keep set). Unused in this
    // single-pass copy, but accumulated for the cross-input disjointness check
    // and absent-destination handling added by later merge stages.
    let mut all_new_pages: BTreeSet<ObjectRef> = BTreeSet::new();

    let depth = DEFAULT_MAX_PAGE_TREE_DEPTH;
    for input in inputs.iter_mut() {
        let all = page_refs(input.source)?;
        // Resolve the selected source page refs (range-checked, duplicates
        // allowed), in selection order.
        let mut selected: Vec<ObjectRef> = Vec::with_capacity(input.pages.len());
        for &idx in &input.pages {
            let page_ref = *all.get(idx).ok_or_else(|| {
                Error::Unsupported(format!(
                    "page index {idx} out of range (input document has {} pages)",
                    all.len()
                ))
            })?;
            selected.push(page_ref);
        }

        // Unique source pages in first-occurrence order; duplicates re-use the
        // same copied object and are shallow-cloned when building /Kids.
        let mut seen: BTreeSet<ObjectRef> = BTreeSet::new();
        let mut unique: Vec<ObjectRef> = Vec::with_capacity(selected.len());
        for &page_ref in &selected {
            if seen.insert(page_ref) {
                unique.push(page_ref);
            }
        }

        // Resolve inherited attributes from the SOURCE before copying severs
        // the /Parent chain.
        let mut inherited: Vec<InheritedAttrs> = Vec::with_capacity(unique.len());
        for &page_ref in &unique {
            inherited.push(InheritedAttrs::resolve(input.source, page_ref, depth)?);
        }

        // UNION of the per-page transitive closures, then ONE deep-copy pass
        // into the growing target: a single renumbering map means an object
        // shared by several selected pages of this input is copied once.
        let mut closure: BTreeSet<ObjectRef> = BTreeSet::new();
        for &page_ref in &unique {
            closure.extend(page_object_closure(input.source, page_ref)?);
        }

        // qpdf `--pages` null-out parity: a destination (annotation `/Dest`,
        // `/A /GoTo /D`, or page `/AA`) targeting a page NOT selected from this
        // input keeps its reference but resolves to `null`. Collect those
        // removed targets, fold them into the closure so `copy_objects`'s single
        // rewrite pass remaps each destination to the removed page's NEW ref
        // (instead of inline-`null`-ing the array element), then null the copied
        // placeholder body below. Folding the remap into the copy pass — rather
        // than a separate post-copy remap — keeps a destination shared by
        // several carriers from being rewritten twice. (page_object_closure
        // already pulls these pages in via the same `/Annots`/`/AA` traversal;
        // adding them explicitly keeps the remap correct independent of that
        // traversal's reach and names the removed set for the null-out.)
        let unique_set: BTreeSet<ObjectRef> = unique.iter().copied().collect();
        let mut removed_targets: BTreeSet<ObjectRef> = BTreeSet::new();
        for &page_ref in &unique {
            // cov:ignore-start: the `?` error path propagates a resolve failure not reachable from a well-formed source page
            collect_removed_dest_targets(
                input.source,
                page_ref,
                &unique_set,
                &mut removed_targets,
            )?;
            // cov:ignore-end
        }
        closure.extend(removed_targets.iter().copied());
        // Renumbering-disjointness invariant: copy_objects allocates fresh
        // target object numbers starting one past the current maximum, so the
        // refs it returns never collide with objects already surviving in the
        // target (prior inputs' copied pages, or the seed catalog/pages root).
        // This is the structural guard that makes a shared-destination
        // double-remap unreachable; capture the surviving set BEFORE copying.
        let surviving_before: BTreeSet<ObjectRef> = target.object_refs().into_iter().collect();
        let map = copy_objects(input.source, &mut target, &closure)?;
        debug_assert!(
            map.values().all(|new| !surviving_before.contains(new)),
            "copy_objects must allocate refs disjoint from surviving target refs \
             (renumbering-disjointness invariant; guards against shared-destination double-remap)"
        );

        // Null the copied placeholder body of each removed destination target.
        // These pages are referenced only by a surviving destination, never
        // appear in /Kids, and are not materialized as leaves; replacing the
        // body with `null` makes the kept destination resolve to a null page
        // (qpdf `--pages` parity). sweep_unreachable_objects later GCs any
        // placeholder no surviving destination still references.
        for src_ref in &removed_targets {
            if let Some(&new_ref) = map.get(src_ref) {
                target.set_object(new_ref, Object::Null);
            }
        }

        // Materialize inherited attrs onto each copied leaf and reparent it to
        // the fresh /Pages root.
        for (&src_ref, attrs) in unique.iter().zip(inherited) {
            let copied_page_ref = *map
                .get(&src_ref)
                .ok_or(Error::Missing("merged page missing from copy map"))?;
            materialize_leaf(&mut target, copied_page_ref, attrs, &map, pages_root_ref)?;
            all_new_pages.insert(copied_page_ref);
        }

        // Append this input's pages to /Kids in selection order, with each
        // input resolved through its own copy map.
        append_selection_kids(&mut target, &selected, &map, &mut used, &mut kids)?;
    }

    // Build the fresh single-level /Pages root over the accumulated kids.
    let mut root = resolve_dict(
        &mut target,
        pages_root_ref,
        "target /Pages is not a dictionary",
    )?; // cov:ignore: Err arm unreachable — minimal_target_bytes creates /Pages as a dict, and nothing since overwrites it (copy_objects renumbers into fresh numbers; materialize_leaf/append_selection_kids touch only copied leaves)
    root.insert(
        "Kids",
        Object::Array(kids.iter().map(|&r| Object::Reference(r)).collect()),
    );
    root.insert("Count", Object::Integer(kids.len() as i64));
    target.set_object(pages_root_ref, Object::Dictionary(root));

    // Drop the copied ancestor /Pages node(s) and any objects only they
    // referenced: they are unreachable now that each leaf /Parent points at the
    // fresh root. full_rewrite does NOT garbage-collect, so prune here.
    sweep_unreachable_objects(&mut target)?;

    Ok(target)
}
