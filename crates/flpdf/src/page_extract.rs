//! Page extraction into a fresh minimal document.
//!
//! [`extract_pages`] builds a brand-new minimal [`Pdf`] containing the
//! selected pages from `source` plus their transitive object closure, copied
//! across documents; [`extract_page`] is the single-page convenience form.
//! This mirrors qpdf's `emptyPDF()` + `QPDFPageDocumentHelper::addPage()`
//! pattern: the document object is constructed and populated here, then
//! written by a separate writer (`write_pdf` / `write_pdf_with_options`).
//!
//! `source` is left unmodified. Inherited page attributes (`/Resources`,
//! `/MediaBox`, `/CropBox`, `/Rotate`) are materialized onto each extracted
//! page exactly as [`crate::page_tree_rebuild`] does, so the pages render
//! identically in isolation.
//!
//! Composes [`page_object_closure`] and [`copy_objects`]. All selected pages
//! are copied in a single pass, so objects shared between them (fonts, images,
//! content streams) are copied exactly once.
//!
//! # Cross-page annotation destinations
//!
//! Destinations on an extracted page that target another, now-absent page are
//! neutralized by dropping the dead destination while retaining the annotation
//! and action structure. This covers an annotation's `/Dest`, and `/GoTo`
//! actions reached through its `/A`, `/AA`, or `/A` `/Next` action chains, as
//! well as the page's own `/AA` actions. The sibling-page stub these referenced
//! then becomes unreachable and is pruned. Named, string, and external
//! (`/URI`, `/GoToR`) destinations carry no in-document page reference and are
//! left untouched, as are destinations targeting any extracted page.
//!
//! Both kinds of page destination are neutralized when they target an absent
//! page: an explicit destination (`/D`) and a GoTo action's structure
//! destination (`/SD`, ISO 32000-2 §12.6.4.3), the latter resolved through its
//! structure element's `/Pg`.
//!
//! # Cross-page page references
//!
//! Two further page references are dropped when they point at an absent page: a
//! malformed annotation `/P` (the page an annotation belongs to) and an
//! article-thread bead `/P`, reached by walking each page's `/B` thread ring
//! through each bead's `/N` and `/V` links. The `/B` array and the bead ring
//! itself are otherwise retained, matching qpdf's page-subset output; a
//! retained bead whose dangling `/P` was dropped therefore lacks the `/P` key.
//! This is a deliberate parity tradeoff: qpdf likewise leaves the orphaned ring
//! in place rather than splicing it.

use crate::object_copy::{copy_objects, rewrite_refs};
use crate::outline_dest_remap::dest_page_ref_resolved;
use crate::page_closure::page_object_closure;
use crate::page_rotate::resolve_inherited_rotate_with_max_depth;
use crate::page_tree_rebuild::resolve_inherited_raw;
use crate::pages::{
    page_refs, resolve_inherited_resources_with_max_depth, DEFAULT_MAX_PAGE_TREE_DEPTH,
};
use crate::ref_chain::resolve_ref_chain;
use crate::subset_prune::sweep_unreachable_objects;
use crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result};
use std::collections::BTreeSet;
use std::io::{Cursor, Read, Seek};

/// Upper bound on inline `/Next` action-chain nesting traversed when
/// neutralizing cross-page destinations. Indirect cycles are stopped by a
/// visited-set; this caps pathological inline nesting (ISO 32000-1 §12.6.3
/// permits `/Next` chains of arbitrary length).
const MAX_ACTION_CHAIN_DEPTH: usize = 64;

/// Inherited page attributes resolved from the source page tree before the
/// copy severs the `/Parent` chain.
pub(crate) struct InheritedAttrs {
    pub(crate) resources: Option<Dictionary>,
    pub(crate) rotate: i32,
    pub(crate) mediabox: Option<Object>,
    pub(crate) cropbox: Option<Object>,
}

impl InheritedAttrs {
    /// Resolve the four inheritable page attributes (`/Resources`, `/Rotate`,
    /// `/MediaBox`, `/CropBox`) for `page_ref` from `source`'s page tree, before
    /// any copy severs the `/Parent` chain.
    pub(crate) fn resolve<R: Read + Seek>(
        source: &mut Pdf<R>,
        page_ref: ObjectRef,
        depth: usize,
    ) -> Result<Self> {
        Ok(InheritedAttrs {
            resources: resolve_inherited_resources_with_max_depth(source, page_ref, depth)?,
            rotate: resolve_inherited_rotate_with_max_depth(source, page_ref, depth)?,
            mediabox: resolve_inherited_raw(source, page_ref, "MediaBox", depth)?,
            cropbox: resolve_inherited_raw(source, page_ref, "CropBox", depth)?,
        })
    }
}

/// Extract the pages at `page_indices` (0-based) from `source` into a
/// brand-new minimal document.
///
/// Returns an owned in-memory [`Pdf`] whose catalog has a single-level
/// `/Pages` tree with one `/Kids` entry per selected index, in **selection
/// order** (any order is accepted, matching qpdf's `--pages` selection
/// semantics). Selected pages are copied in a single pass with one shared
/// renumbering map, so objects referenced by several selected pages (fonts,
/// images, content streams) appear exactly once in the output.
///
/// An index may appear more than once. The second and later occurrences of a
/// page become shallow clones of its first copy: each duplicate gets its own
/// page object, while indirectly referenced sub-objects (`/Contents`,
/// `/Resources`, `/Annots`, `/B`) stay shared between the duplicates,
/// matching qpdf 11.9.0's observed duplicate-page output.
///
/// The returned document is already minimal: copied ancestor `/Pages` nodes
/// left over from the closure are pruned (mark-and-sweep from the new
/// catalog) before returning. Write it with [`write_pdf`](crate::write_pdf)
/// or [`write_pdf_with_options`](crate::write_pdf_with_options); enabling
/// [`WriteOptions::full_rewrite`](crate::WriteOptions::full_rewrite) is
/// recommended for compaction but is not required for correctness.
///
/// `source` is not modified. See also [`extract_page`] for the single-page
/// form, and the [module documentation](self) for how cross-page
/// destinations on the extracted pages are neutralized.
///
/// # Examples
///
/// ```no_run
/// use std::fs::File;
/// use std::io::BufReader;
/// use flpdf::{extract_pages, write_pdf_with_options, Pdf, WriteOptions};
///
/// let file = BufReader::new(File::open("input.pdf")?);
/// let mut pdf = Pdf::open(file)?;
///
/// // First and third page (0-based), in selection order.
/// let mut extracted = extract_pages(&mut pdf, &[0, 2])?;
///
/// let mut options = WriteOptions::default();
/// options.full_rewrite = true;
/// let mut out = File::create("extracted.pdf")?;
/// write_pdf_with_options(&mut extracted, &mut out, &options)?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// # Errors
///
/// - [`Error::Unsupported`] if `page_indices` is empty or any index is out of
///   range.
/// - Propagates resolve/copy errors from the underlying primitives.
pub fn extract_pages<R: Read + Seek>(
    source: &mut Pdf<R>,
    page_indices: &[usize],
) -> Result<Pdf<Cursor<Vec<u8>>>> {
    if page_indices.is_empty() {
        return Err(Error::Unsupported("empty page selection".to_string()));
    }
    let all_pages = page_refs(source)?;
    let mut selected: Vec<ObjectRef> = Vec::with_capacity(page_indices.len());
    for &idx in page_indices {
        let page_ref = *all_pages.get(idx).ok_or_else(|| {
            Error::Unsupported(format!(
                "page index {idx} out of range (document has {} pages)",
                all_pages.len()
            ))
        })?;
        selected.push(page_ref);
    }

    // Unique source pages in first-occurrence order. Duplicates re-use the
    // same copied object and are shallow-cloned when building /Kids below.
    let mut seen: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut unique: Vec<ObjectRef> = Vec::with_capacity(selected.len());
    for &page_ref in &selected {
        if seen.insert(page_ref) {
            unique.push(page_ref);
        }
    }

    // Resolve inherited attributes from the SOURCE before copying severs the
    // /Parent chain. Same four attributes / helpers as page_tree_rebuild.
    let depth = DEFAULT_MAX_PAGE_TREE_DEPTH;
    let mut inherited: Vec<InheritedAttrs> = Vec::with_capacity(unique.len());
    for &page_ref in &unique {
        inherited.push(InheritedAttrs::resolve(source, page_ref, depth)?);
    }

    // UNION of the per-page transitive closures, then ONE deep-copy pass into
    // a fresh minimal doc: a single renumbering map means an object shared by
    // several selected pages is copied exactly once.
    let mut closure: BTreeSet<ObjectRef> = BTreeSet::new();
    for &page_ref in &unique {
        closure.extend(page_object_closure(source, page_ref)?);
    }
    let mut target = Pdf::open_mem_owned(minimal_target_bytes())?;
    let map = copy_objects(source, &mut target, &closure)?;
    let pages_root_ref = target_pages_root(&mut target)?;

    // Materialize inherited attrs onto each copied leaf (remapping refs), then
    // repoint /Parent at the fresh root.
    let mut copied_unique: Vec<ObjectRef> = Vec::with_capacity(unique.len());
    for (&src_ref, attrs) in unique.iter().zip(inherited) {
        let copied_page_ref = *map
            .get(&src_ref)
            .ok_or(Error::Missing("extracted page missing from copy map"))?;
        materialize_leaf(&mut target, copied_page_ref, attrs, &map, pages_root_ref)?;
        copied_unique.push(copied_page_ref);
    }

    // Neutralize annotations on each extracted leaf whose destination targets
    // a page absent from this output. Without this, an explicit cross-page
    // /Dest keeps the copied sibling-page stub (and its ancestor /Pages)
    // reachable, so the sweep below cannot prune them. qpdf-aligned: the
    // annotation is retained, only the dead destination is removed. Duplicate
    // clones made below are never destination targets (copy_objects maps each
    // source page to its FIRST copy), so the unique set is the full keep-set.
    let keep: BTreeSet<ObjectRef> = copied_unique.iter().copied().collect();
    for &copied_page_ref in &copied_unique {
        neutralize_absent_dests(&mut target, copied_page_ref, &keep)?;
    }

    // Build /Kids in SELECTION order. The first occurrence of a source page
    // uses its mapped copy; later occurrences get a shallow clone of the
    // (materialized, neutralized) first copy: a fresh page object whose
    // indirectly referenced sub-objects (/Contents, /Resources, /Annots, /B)
    // stay shared, matching qpdf's observed duplicate-page output and
    // page_tree_rebuild's duplicate-selection scheme.
    let mut kids: Vec<ObjectRef> = Vec::with_capacity(selected.len());
    let mut used: BTreeSet<ObjectRef> = BTreeSet::new();
    append_selection_kids(&mut target, &selected, &map, &mut used, &mut kids)?;

    // Build the fresh single-level /Pages root.
    let mut root = resolve_dict(
        &mut target,
        pages_root_ref,
        "target /Pages is not a dictionary",
    )?;
    root.insert(
        "Kids",
        Object::Array(kids.iter().map(|&r| Object::Reference(r)).collect()),
    );
    root.insert("Count", Object::Integer(kids.len() as i64));
    target.set_object(pages_root_ref, Object::Dictionary(root));

    // Drop the copied ancestor /Pages node(s) and any objects only they
    // referenced: they are unreachable from the new catalog now that each leaf
    // /Parent points at the fresh root. full_rewrite does NOT garbage-collect
    // (it emits every non-deleted object), so prune here to satisfy
    // "no unrelated objects". Same mark-and-sweep used after page-subset
    // rebuild (subset_prune::sweep_unreachable_objects).
    sweep_unreachable_objects(&mut target)?;

    Ok(target)
}

/// Extract page `page_index` (0-based) from `source` into a brand-new minimal
/// document.
///
/// Single-page convenience form of [`extract_pages`]: the returned document's
/// catalog has a single-level `/Pages` tree with a single entry in `/Kids`.
/// `source` is not modified.
///
/// # Errors
///
/// - [`Error::Unsupported`] if `page_index` is out of range.
/// - Propagates resolve/copy errors from the underlying primitives.
pub fn extract_page<R: Read + Seek>(
    source: &mut Pdf<R>,
    page_index: usize,
) -> Result<Pdf<Cursor<Vec<u8>>>> {
    extract_pages(source, &[page_index])
}

/// Materialize the four inheritable attributes onto a copied leaf page and
/// repoint its `/Parent` at `pages_root_ref`.
///
/// `attrs` were resolved from the source page tree before the copy severed the
/// `/Parent` chain; each is inserted only when the leaf does not already carry
/// it directly, with any indirect references inside the attribute value
/// remapped through `map` into the target's numbering. Shared by
/// [`extract_pages`] and [`crate::page_merge::merge_documents`].
pub(crate) fn materialize_leaf(
    target: &mut Pdf<Cursor<Vec<u8>>>,
    copied_page_ref: ObjectRef,
    attrs: InheritedAttrs,
    map: &std::collections::BTreeMap<ObjectRef, ObjectRef>,
    pages_root_ref: ObjectRef,
) -> Result<()> {
    let mut leaf = resolve_dict(target, copied_page_ref, "copied page is not a dictionary")?; // cov:ignore: Err arm unreachable — page_refs yields only /Type /Page dicts and copy_objects preserves the source page dict

    if !has_own(&leaf, "Resources") {
        if let Some(res) = attrs.resources {
            let mut value = Object::Dictionary(res);
            rewrite_refs(&mut value, 0, map)?;
            leaf.insert("Resources", value);
        }
    }
    if !has_own(&leaf, "MediaBox") {
        if let Some(mut mb) = attrs.mediabox {
            rewrite_refs(&mut mb, 0, map)?;
            leaf.insert("MediaBox", mb);
        } // cov:ignore: rewrite_refs ? Err arm (MAX_INLINE_DEPTH) unreachable for shallow inherited /MediaBox
    }
    if !has_own(&leaf, "CropBox") {
        if let Some(mut cb) = attrs.cropbox {
            rewrite_refs(&mut cb, 0, map)?;
            leaf.insert("CropBox", cb);
        }
    }
    if !has_own(&leaf, "Rotate") {
        leaf.insert("Rotate", Object::Integer(attrs.rotate as i64));
    }
    leaf.insert("Parent", Object::Reference(pages_root_ref));
    target.set_object(copied_page_ref, Object::Dictionary(leaf));
    Ok(())
}

/// Append `/Kids` entries to `kids` for `selected` (in selection order),
/// shallow-cloning any source page selected more than once.
///
/// The first occurrence of a source page uses its mapped copy from `map`;
/// later occurrences become a fresh page object whose indirectly referenced
/// sub-objects (`/Contents`, `/Resources`, `/Annots`, `/B`) stay shared with
/// the first copy, matching qpdf's observed duplicate-page output. `used`
/// tracks which copied page objects already appear in `kids`, so this may be
/// called once per input (with `used`/`kids` accumulating across calls) by
/// [`crate::page_merge::merge_documents`], or once by [`extract_pages`].
///
/// New object numbers for clones are allocated above the current maximum in
/// `target`, recomputed on entry so repeated calls into a growing target do
/// not collide.
pub(crate) fn append_selection_kids(
    target: &mut Pdf<Cursor<Vec<u8>>>,
    selected: &[ObjectRef],
    map: &std::collections::BTreeMap<ObjectRef, ObjectRef>,
    used: &mut BTreeSet<ObjectRef>,
    kids: &mut Vec<ObjectRef>,
) -> Result<()> {
    let mut next_num: u32 = target
        .object_refs()
        .iter()
        .map(|r| r.number)
        .max()
        .unwrap_or(0);
    for &src_ref in selected {
        let copied_page_ref = *map
            .get(&src_ref)
            .ok_or(Error::Missing("extracted page missing from copy map"))?;
        let kid = if used.insert(copied_page_ref) {
            copied_page_ref
        } else {
            next_num = next_num.checked_add(1).ok_or_else(|| {
                // cov:ignore-start: unreachable in practice — copy_objects
                // renumbers the freshly built target sequentially from a small
                // base, so hitting u32::MAX would need ~2^32 copied objects.
                // The `})?;` terminator carries the Err-propagation region of
                // this same arm, so the block extends through it.
                Error::Unsupported(
                    "page extract: object-number overflow allocating duplicate page".to_string(),
                )
            })?;
            // cov:ignore-end
            let clone_ref = ObjectRef::new(next_num, 0);
            // The one intentional copy: the duplicate kid's own dictionary.
            let dict = resolve_dict(target, copied_page_ref, "copied page is not a dictionary")?; // cov:ignore: Err arm unreachable — the first copy of this page resolved to a dictionary in the materialize loop above
            target.set_object(clone_ref, Object::Dictionary(dict));
            clone_ref
        };
        kids.push(kid);
    }
    Ok(())
}

/// Drop cross-page `/GoTo` destinations from any annotation on `page_ref`, and
/// from the page's own `/AA`. A destination targeting a page not in `keep`
/// (i.e. a page absent from the output) has its `/D` dropped (annotation
/// `/Dest`: the whole `/Dest` key); the action and chain structure are
/// otherwise retained. Named / string / `/URI` / `/GoToR` destinations carry
/// no in-document page reference and are left untouched.
fn neutralize_absent_dests(
    target: &mut Pdf<Cursor<Vec<u8>>>,
    page_ref: ObjectRef,
    keep: &BTreeSet<ObjectRef>,
) -> Result<()> {
    // Detach everything we need from the immutable page borrow before any
    // `&mut target` call: the raw /Annots value and the page's own /AA value.
    let (annots_val, page_aa): (Option<Object>, Option<Object>) = {
        let page_obj = target.resolve_borrowed(page_ref)?;
        let Some(page_dict) = page_obj.as_dict() else {
            return Ok(());
        };
        (
            page_dict.get("Annots").cloned(),
            page_dict.get("AA").cloned(),
        )
    };

    // /Annots may be an inline array or an indirect reference to one.
    // Inline-dict annotations (no indirect ref) are skipped: there is no
    // object to set_object back; in practice /Annots entries are indirect.
    let annot_refs: Vec<ObjectRef> = match annots_val {
        Some(Object::Array(arr)) => arr.iter().filter_map(Object::as_ref_id).collect(),
        Some(Object::Reference(r)) => match target.resolve_borrowed(r)? {
            Object::Array(arr) => arr.iter().filter_map(Object::as_ref_id).collect(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };

    for annot_ref in annot_refs {
        neutralize_annot_if_absent(target, annot_ref, keep)?;
    }

    // Page-level /AA (open/close etc.). An inline /AA dict is rewritten back
    // onto the page; an indirect /AA is mutated in place via its own ref, so
    // the page needs no change in that case.
    if let Some(aa_val) = page_aa {
        if let Some(new_aa) = neutralize_aa_if_absent(target, &aa_val, keep)? {
            let mut page = resolve_dict(target, page_ref, "extracted page is not a dictionary")?;
            page.insert("AA", new_aa);
            target.set_object(page_ref, Object::Dictionary(page));
        }
    }

    neutralize_bead_ring(target, page_ref, keep)?;
    Ok(())
}

/// Walk the article-thread bead ring reachable from this page's `/B` and drop
/// each bead's `/P` that targets an absent page. `/N`/`/V` link beads (not
/// pages), so they never leak; only the page-valued `/P` is neutralized. The
/// ring is bounded by `visited` (each bead handled once). The `/B` array and
/// the beads themselves are retained — only dangling `/P` keys are dropped,
/// matching qpdf's single-page output.
///
/// `/B`, `/N`, and `/V` may each be an indirect-reference chain, so every link
/// is normalized through [`resolve_ref_chain`] to the terminal bead object:
/// this both reaches the bead body through a chain and ensures the write-back
/// targets the real bead, not an intermediate reference holder.
fn neutralize_bead_ring(
    target: &mut Pdf<Cursor<Vec<u8>>>,
    page_ref: ObjectRef,
    keep: &BTreeSet<ObjectRef>,
) -> Result<()> {
    let b_val = {
        let page_obj = target.resolve_borrowed(page_ref)?;
        let Some(page_dict) = page_obj.as_dict() else {
            return Ok(());
        };
        page_dict.get("B").cloned()
    };
    let Some(b_val) = b_val else {
        return Ok(());
    };
    // /B may itself be an indirect reference to the bead array; normalize it.
    let (b_concrete, _) = resolve_ref_chain(target, &b_val)?;
    let Object::Array(beads) = b_concrete else {
        return Ok(());
    };
    let mut queue: Vec<ObjectRef> = beads.iter().filter_map(Object::as_ref_id).collect();
    let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
    while let Some(start_ref) = queue.pop() {
        // Resolve the link to the terminal bead object so an indirect chain
        // still reaches the bead, and so `set_object` below rewrites the real
        // bead rather than an intermediate reference holder.
        let (concrete, terminal) = resolve_ref_chain(target, &Object::Reference(start_ref))?;
        let bead_ref = terminal.unwrap_or(start_ref);
        if !visited.insert(bead_ref) {
            continue;
        }
        let Some(mut bead) = concrete.into_dict() else {
            continue;
        };
        // Enqueue ring neighbours before mutating.
        for key in ["N", "V"] {
            if let Some(Object::Reference(r)) = bead.get(key) {
                queue.push(*r);
            }
        }
        // Inspect `/P` first; only `remove` it and write the bead back when it
        // is actually dropped, so a kept bead's stored `/P` is never rewritten.
        if let Some(p_val) = bead.get("P") {
            if p_targets_absent_page(target, p_val, keep)? {
                bead.remove("P");
                target.set_object(bead_ref, Object::Dictionary(bead));
            }
        }
    }
    Ok(())
}

/// Inspect one annotation; drop the cross-page destination from `/Dest`, `/A`,
/// and `/AA` when it resolves to a page not in `keep`.
fn neutralize_annot_if_absent(
    target: &mut Pdf<Cursor<Vec<u8>>>,
    annot_ref: ObjectRef,
    keep: &BTreeSet<ObjectRef>,
) -> Result<()> {
    let Some(mut annot) = target.resolve_borrowed(annot_ref)?.as_dict().cloned() else {
        return Ok(());
    };
    let mut changed = false;

    // /Dest — explicit array, dict, or an indirect reference to either. The
    // whole key is dropped (a destination-less annotation is the neutralized
    // form for an explicit destination). `annot` is already owned, so take the
    // value by `remove` and re-insert it when it stays (no inner clone).
    if let Some(dest) = annot.remove("Dest") {
        if dest_targets_absent_page(target, &dest, keep)? {
            changed = true;
        } else {
            annot.insert("Dest", dest);
        }
    }

    // /A — an action (or chain via /Next). Drop the /D of every cross-page
    // GoTo, keeping the action(s) and chain structure intact.
    if let Some(a_val) = annot.remove("A") {
        let mut visited = BTreeSet::new();
        if let Some(new) =
            neutralize_action_chain(target, &a_val, keep, &mut visited, MAX_ACTION_CHAIN_DEPTH)?
        {
            annot.insert("A", new);
            changed = true;
        } else {
            annot.insert("A", a_val);
        }
    }

    // /AA — additional-actions dict; each entry is an action (or chain).
    if let Some(aa_val) = annot.remove("AA") {
        if let Some(new_aa) = neutralize_aa_if_absent(target, &aa_val, keep)? {
            annot.insert("AA", new_aa);
            changed = true;
        } else {
            annot.insert("AA", aa_val);
        }
    }

    // /P — the page this annotation belongs to. A malformed /P pointing at an
    // absent (sibling) page keeps that page's stub reachable; drop it.
    if let Some(p_val) = annot.remove("P") {
        if p_targets_absent_page(target, &p_val, keep)? {
            changed = true;
        } else {
            annot.insert("P", p_val);
        }
    }

    if changed {
        target.set_object(annot_ref, Object::Dictionary(annot));
    }
    Ok(())
}

/// Neutralize every subaction in an `/AA` additional-actions value.
///
/// `aa_value` may be an inline dict or an indirect reference to one. Each entry
/// (`/O`, `/C`, `/U`, `/E`, …) is an action value handled by
/// [`neutralize_action_chain`]. Returns `Some(updated_aa_dict)` when an INLINE
/// `/AA` must be stored back by the caller; returns `None` when nothing changed
/// OR the change was applied in place on an indirect `/AA` dict (the caller
/// keeps pointing at the same ref).
fn neutralize_aa_if_absent(
    target: &mut Pdf<Cursor<Vec<u8>>>,
    aa_value: &Object,
    keep: &BTreeSet<ObjectRef>,
) -> Result<Option<Object>> {
    let (concrete, terminal_ref) = resolve_ref_chain(target, aa_value)?;
    let Some(mut aa) = concrete.into_dict() else {
        return Ok(None);
    };
    let keys: Vec<Vec<u8>> = aa.iter().map(|(k, _)| k.to_vec()).collect();
    let mut changed = false;
    for key in keys {
        // `aa` is owned: take the subaction by `remove` and re-insert it when it
        // stays (no inner clone of potentially deep action chains).
        let Some(sub_val) = aa.remove(&key) else {
            continue;
        };
        // Reset visited per subaction: each subaction is an independent chain,
        // and re-running over an already-neutralized GoTo is a no-op.
        let mut visited = BTreeSet::new();
        if let Some(new) =
            neutralize_action_chain(target, &sub_val, keep, &mut visited, MAX_ACTION_CHAIN_DEPTH)?
        {
            aa.insert(&key, new);
            changed = true;
        } else {
            aa.insert(&key, sub_val);
        }
    }
    if !changed {
        return Ok(None);
    }
    match terminal_ref {
        // Indirect /AA: rewrite the referenced dict; the carrier's /AA ref is
        // unchanged.
        Some(r) => {
            target.set_object(r, Object::Dictionary(aa));
            Ok(None)
        }
        // Inline /AA: the caller stores the updated dict back.
        None => Ok(Some(Object::Dictionary(aa))),
    }
}

/// Walk an action value (`/A`, an `/AA` subaction, or a `/Next` element),
/// dropping the `/D` of every GoTo whose destination targets a page not in
/// `keep`. Follows the `/A`->action indirection chain and `/Next` chains
/// (single action or array). Indirect cycles are bounded by `visited`; inline
/// `/Next` nesting by `depth`. Returns `Some(updated)` when an INLINE action
/// value must be stored back by the caller; returns `None` when there was no
/// change OR the change was applied in place via `set_object` on an indirect
/// action (the caller keeps pointing at the same ref).
fn neutralize_action_chain(
    target: &mut Pdf<Cursor<Vec<u8>>>,
    action_value: &Object,
    keep: &BTreeSet<ObjectRef>,
    visited: &mut BTreeSet<ObjectRef>,
    depth: usize,
) -> Result<Option<Object>> {
    if depth == 0 {
        return Ok(None);
    }

    let (concrete, terminal_ref) = resolve_ref_chain(target, action_value)?;
    // Indirect-cycle guard: stop if we have already entered this action object.
    if let Some(r) = terminal_ref {
        if !visited.insert(r) {
            return Ok(None);
        }
    }

    // An action value may also be an ARRAY of actions (ISO 32000-1 §12.6.3:
    // `/Next` may be a single action or an array). When `/Next NN 0 R` resolves
    // to a separately stored array object, handle it here so its cross-page
    // GoTos are not silently skipped by the dict-only path below.
    if let Object::Array(mut elems) = concrete {
        let any = neutralize_action_array(target, &mut elems, keep, visited, depth)?;
        if !any {
            return Ok(None);
        }
        return match terminal_ref {
            // Indirect array: rewrite the referenced object in place.
            Some(r) => {
                target.set_object(r, Object::Array(elems));
                Ok(None)
            }
            // Inline array: the caller stores the updated value back. (Not
            // reachable via the `/Next` single-value arm, which only recurses
            // here on an indirect reference; kept for completeness.)
            None => Ok(Some(Object::Array(elems))),
        };
    }

    let Some(mut act) = concrete.into_dict() else {
        return Ok(None);
    };
    let mut changed = false;

    // Drop a cross-page GoTo destination (the action itself is retained).
    // `act` is owned: take `/D` by `remove` and re-insert it when it stays.
    let is_goto = matches!(act.get("S"), Some(Object::Name(n)) if n == b"GoTo");
    if is_goto {
        if let Some(d_val) = act.remove("D") {
            if dest_targets_absent_page(target, &d_val, keep)? {
                changed = true;
            } else {
                act.insert("D", d_val);
            }
        }
        if let Some(sd_val) = act.remove("SD") {
            if sd_targets_absent_page(target, &sd_val, keep)? {
                changed = true;
            } else {
                act.insert("SD", sd_val);
            }
        }
    }

    // /Next — a single action or an array of actions. Recurse into each. The
    // value is taken by `remove` (no clone) and re-inserted after walking.
    if let Some(next_val) = act.remove("Next") {
        match next_val {
            Object::Array(mut elems) => {
                if neutralize_action_array(target, &mut elems, keep, visited, depth)? {
                    changed = true;
                }
                act.insert("Next", Object::Array(elems));
            }
            single => match neutralize_action_chain(target, &single, keep, visited, depth - 1)? {
                Some(new) => {
                    act.insert("Next", new);
                    changed = true;
                }
                None => {
                    act.insert("Next", single);
                }
            },
        }
    }

    if !changed {
        return Ok(None);
    }
    match terminal_ref {
        // Indirect action: rewrite the referenced object in place; the caller's
        // ref is unchanged.
        Some(r) => {
            target.set_object(r, Object::Dictionary(act));
            Ok(None)
        }
        // Inline action: the caller stores the updated value back.
        None => Ok(Some(Object::Dictionary(act))),
    }
}

/// Neutralize each element of an action array in place, returning `true` if any
/// element changed. Each element is an independent action (chain) recursed at
/// `depth - 1`; the shared `visited` set and length are preserved (no splicing).
/// Used both for an inline `/Next` array and for an indirect `/Next` resolving
/// to a separately stored array object.
fn neutralize_action_array(
    target: &mut Pdf<Cursor<Vec<u8>>>,
    elems: &mut [Object],
    keep: &BTreeSet<ObjectRef>,
    visited: &mut BTreeSet<ObjectRef>,
    depth: usize,
) -> Result<bool> {
    let mut any = false;
    for elem in elems.iter_mut() {
        if let Some(new) = neutralize_action_chain(target, elem, keep, visited, depth - 1)? {
            *elem = new;
            any = true;
        }
    }
    Ok(any)
}

/// `true` when `dest` resolves to an explicit page reference not in `keep`.
/// Named / string / external destinations (no resolvable in-doc page ref) and
/// links to kept pages return `false` — they are not neutralized.
fn dest_targets_absent_page(
    target: &mut Pdf<Cursor<Vec<u8>>>,
    dest: &Object,
    keep: &BTreeSet<ObjectRef>,
) -> Result<bool> {
    Ok(match dest_page_ref_resolved(target, dest)? {
        Some(page_ref) => !keep.contains(&page_ref),
        None => false,
    })
}

/// `true` when a GoTo `/SD` structure destination resolves to a page not in
/// `keep`. Thin predicate over [`sd_target_page_ref`].
fn sd_targets_absent_page(
    target: &mut Pdf<Cursor<Vec<u8>>>,
    sd: &Object,
    keep: &BTreeSet<ObjectRef>,
) -> Result<bool> {
    Ok(match sd_target_page_ref(target, sd)? {
        Some(r) => !keep.contains(&r),
        None => false,
    })
}

/// Resolve a GoTo `/SD` structure destination to its target page reference, or
/// `None` when it carries no in-document page ref.
///
/// An `/SD` value is `[structElemRef /Fit ...]` (or an indirect ref to one);
/// the first element is a *structure element*, whose `/Pg` is the target page
/// (ISO 32000-2 §12.6.4.3). Named structure destinations (a name/string,
/// resolved via the structure tree) carry no in-document page ref and return
/// `None`. A missing / unresolvable / non-Page `/Pg` also returns `None`. Each
/// level may be indirect; [`resolve_ref_chain`] bounds the indirection.
///
/// Shared by extract's neutralize-drop path and merge's collect path so both
/// reach the `/SD` target page through the identical StructElem -> `/Pg` hop.
pub(crate) fn sd_target_page_ref<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    sd: &Object,
) -> Result<Option<ObjectRef>> {
    let (concrete, _) = resolve_ref_chain(pdf, sd)?;
    let Object::Array(arr) = concrete else {
        return Ok(None); // named structure destination or malformed
    };
    let Some(struct_elem) = arr.into_iter().next() else {
        return Ok(None);
    };
    let (se, _) = resolve_ref_chain(pdf, &struct_elem)?;
    let Some(se_dict) = se.into_dict() else {
        return Ok(None);
    };
    let Some(pg) = se_dict.get("Pg").cloned() else {
        return Ok(None);
    };
    let (pg_concrete, pg_ref) = resolve_ref_chain(pdf, &pg)?;
    // Unlike `dest_page_ref_resolved`, where the `/D` array's first element IS
    // the page ref, `/SD` reaches the page through an extra StructElem -> `/Pg`
    // hop, so confirm the resolved `/Pg` target is actually a `/Type /Page`
    // before treating it as a page destination.
    Ok(match pg_ref {
        Some(r) if is_page_dict(&pg_concrete) => Some(r),
        _ => None,
    })
}

/// `true` when `p` (an annotation's or bead's `/P`) resolves to a Page object
/// not in `keep`. On an annotation (ISO 32000-2 §12.5.2, Table 166) or an
/// article bead (§12.4.3), `/P` denotes the page the object belongs to, so a
/// `/P` pointing at an absent page is dangling and dropped. Non-Page /
/// unresolvable / kept-page targets return `false` (kept) — the `is_page_dict`
/// gate also keeps any non-page `/P` (e.g. a StructElem's parent `/P`).
fn p_targets_absent_page(
    target: &mut Pdf<Cursor<Vec<u8>>>,
    p: &Object,
    keep: &BTreeSet<ObjectRef>,
) -> Result<bool> {
    let (concrete, p_ref) = resolve_ref_chain(target, p)?;
    Ok(match p_ref {
        Some(r) => !keep.contains(&r) && is_page_dict(&concrete),
        None => false,
    })
}

/// `true` when `obj` is a `<< /Type /Page ... >>` dictionary.
fn is_page_dict(obj: &Object) -> bool {
    obj.as_dict()
        .and_then(|d| d.get("Type"))
        .is_some_and(|t| matches!(t, Object::Name(n) if n == b"Page"))
}

/// Minimal valid target: Catalog(1) + empty Pages(2). No placeholder page (so
/// there is no orphan to delete after copying).
pub(crate) fn minimal_target_bytes() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let off1 = out.len() as u64;
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let off2 = out.len() as u64;
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");
    let xref_start = out.len() as u64;
    out.extend_from_slice(
        format!("xref\n0 3\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n")
            .as_bytes(),
    );
    out.extend_from_slice(
        format!("trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    out
}

/// Resolve the target catalog's `/Pages` root ref.
pub(crate) fn target_pages_root(target: &mut Pdf<Cursor<Vec<u8>>>) -> Result<ObjectRef> {
    let catalog_ref = target.root_ref().ok_or(Error::Missing("/Root"))?;
    let catalog = resolve_dict(target, catalog_ref, "/Root is not a dictionary")?;
    catalog
        .get("Pages")
        .and_then(|o| match o {
            Object::Reference(r) => Some(*r),
            _ => None,
        })
        .ok_or(Error::Missing("/Pages"))
}

/// Resolve `r` in `target` and move out its [`Dictionary`], or fail with `ctx`.
///
/// Shared by [`extract_pages`]'s leaf/root materialization and
/// [`target_pages_root`]; the error arm guards against a ref resolving to a
/// non-dictionary (or a missing object, which resolves to [`Object::Null`]).
pub(crate) fn resolve_dict(
    target: &mut Pdf<Cursor<Vec<u8>>>,
    r: ObjectRef,
    ctx: &'static str,
) -> Result<Dictionary> {
    match target.resolve(r)? {
        Object::Dictionary(d) => Ok(d),
        _ => Err(Error::Missing(ctx)),
    }
}

/// `true` when `dict` carries `key` as something other than `null`
/// (ISO 32000-1 §7.3.9: explicit `null` == absent). Mirrors
/// `page_tree_rebuild::leaf_has_own`.
fn has_own(dict: &Dictionary, key: &str) -> bool {
    !matches!(dict.get(key), None | Some(Object::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Build a PDF from `(number, body)` object definitions plus a `/Root`
    /// number, computing xref offsets so the bytes are always valid.
    fn build_pdf(objects: &[(u32, &str)], root: u32) -> Vec<u8> {
        let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
        let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();
        let max = objects.iter().map(|(n, _)| *n).max().unwrap_or(0);
        for (n, body) in objects {
            offsets.insert(*n, out.len() as u64);
            out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
        }
        let xref_start = out.len() as u64;
        let size = max + 1;
        out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for n in 1..=max {
            match offsets.get(&n) {
                Some(off) => out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes()),
                None => out.extend_from_slice(b"0000000000 65535 f \n"),
            }
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {size} /Root {root} 0 R >>\nstartxref\n{xref_start}\n%%EOF\n"
            )
            .as_bytes(),
        );
        out
    }

    #[test]
    fn resolve_dict_errors_on_non_dictionary() {
        // Object 3 is an integer, not a dictionary.
        let bytes = build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [] /Count 0 >>"),
                (3, "42"),
            ],
            1,
        );
        let mut pdf = Pdf::open_mem_owned(bytes).unwrap();
        let err = resolve_dict(&mut pdf, ObjectRef::new(3, 0), "not a dict")
            .expect_err("resolving an integer as a dict must error");
        assert!(matches!(err, Error::Missing("not a dict")), "got {err:?}");
    }

    #[test]
    fn target_pages_root_errors_when_pages_is_not_a_reference() {
        // /Pages is an inline dictionary (a direct object), not an indirect
        // reference, so target_pages_root cannot extract a root ref.
        let bytes = build_pdf(
            &[(
                1,
                "<< /Type /Catalog /Pages << /Type /Pages /Kids [] /Count 0 >> >>",
            )],
            1,
        );
        let mut pdf = Pdf::open_mem_owned(bytes).unwrap();
        let err = target_pages_root(&mut pdf).expect_err("inline /Pages must error");
        assert!(matches!(err, Error::Missing("/Pages")), "got {err:?}");
    }
}
