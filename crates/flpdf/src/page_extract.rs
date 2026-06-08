//! Single-page extraction into a fresh minimal document.
//!
//! [`extract_page`] builds a brand-new minimal [`Pdf`] containing exactly one
//! page from `source` plus its transitive object closure, copied across
//! documents. This mirrors qpdf's `emptyPDF()` + `QPDFPageDocumentHelper::
//! addPage()` pattern: the document object is constructed and populated here,
//! then written by a separate writer (`write_pdf` / `write_pdf_with_options`).
//!
//! `source` is left unmodified. Inherited page attributes (`/Resources`,
//! `/MediaBox`, `/CropBox`, `/Rotate`) are materialized onto the extracted page
//! exactly as [`crate::page_tree_rebuild`] does, so the page renders
//! identically in isolation.
//!
//! Composes [`page_object_closure`] and [`copy_objects`].
//!
//! # Cross-page annotation destinations
//!
//! Destinations on the extracted page that target another, now-absent page are
//! neutralized by dropping the dead destination while retaining the annotation
//! and action structure. This covers an annotation's `/Dest`, and `/GoTo`
//! actions reached through its `/A`, `/AA`, or `/A` `/Next` action chains, as
//! well as the page's own `/AA` actions. The sibling-page stub these referenced
//! then becomes unreachable and is pruned. Named, string, and external
//! (`/URI`, `/GoToR`) destinations carry no in-document page reference and are
//! left untouched, as are destinations targeting the extracted page itself.
//!
//! Only explicit page destinations (`/D`) are neutralized. A GoTo action's
//! structure destination (`/SD`, ISO 32000-2 §12.6.4.3) is not inspected, so a
//! `/SD` pointing into another page's structure tree can keep that page
//! reachable in the output.

use crate::object_copy::{copy_objects, rewrite_refs};
use crate::outline_dest_remap::{dest_page_ref_resolved, resolve_ref_chain};
use crate::page_closure::page_object_closure;
use crate::page_rotate::resolve_inherited_rotate_with_max_depth;
use crate::page_tree_rebuild::resolve_inherited_raw;
use crate::pages::{
    page_refs, resolve_inherited_resources_with_max_depth, DEFAULT_MAX_PAGE_TREE_DEPTH,
};
use crate::subset_prune::sweep_unreachable_objects;
use crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result};
use std::collections::BTreeSet;
use std::io::{Cursor, Read, Seek};

/// Upper bound on inline `/Next` action-chain nesting traversed when
/// neutralizing cross-page destinations. Indirect cycles are stopped by a
/// visited-set; this caps pathological inline nesting (ISO 32000-1 §12.6.3
/// permits `/Next` chains of arbitrary length).
const MAX_ACTION_CHAIN_DEPTH: usize = 64;

/// Extract page `page_index` (0-based) from `source` into a brand-new minimal
/// document.
///
/// Returns an owned in-memory [`Pdf`] whose catalog has a single-level
/// `/Pages` tree with a single entry in `/Kids`. The returned document is
/// already minimal: copied ancestor `/Pages` nodes left over from the closure
/// are pruned (mark-and-sweep from the new catalog) before returning. Write it
/// with [`write_pdf`](crate::write_pdf) or
/// [`write_pdf_with_options`](crate::write_pdf_with_options); enabling
/// [`WriteOptions::full_rewrite`](crate::WriteOptions::full_rewrite) is
/// recommended for compaction but is not required for correctness.
///
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
    let all_pages = page_refs(source)?;
    let page_ref = *all_pages.get(page_index).ok_or_else(|| {
        Error::Unsupported(format!(
            "page index {page_index} out of range (document has {} pages)",
            all_pages.len()
        ))
    })?;

    // Resolve inherited attributes from the SOURCE before copying severs the
    // /Parent chain. Same four attributes / helpers as page_tree_rebuild.
    let depth = DEFAULT_MAX_PAGE_TREE_DEPTH;
    let inherited_resources = resolve_inherited_resources_with_max_depth(source, page_ref, depth)?;
    let inherited_rotate = resolve_inherited_rotate_with_max_depth(source, page_ref, depth)?;
    let inherited_mediabox = resolve_inherited_raw(source, page_ref, "MediaBox", depth)?;
    let inherited_cropbox = resolve_inherited_raw(source, page_ref, "CropBox", depth)?;

    // Transitive closure of the page, then deep-copy into a fresh minimal doc.
    let closure = page_object_closure(source, page_ref)?;
    let mut target = Pdf::open_mem_owned(minimal_target_bytes())?;
    let map = copy_objects(source, &mut target, &closure)?;

    let copied_page_ref = *map
        .get(&page_ref)
        .ok_or(Error::Missing("extracted page missing from copy map"))?;
    let pages_root_ref = target_pages_root(&mut target)?;

    // Materialize inherited attrs onto the copied leaf (remapping refs), then
    // repoint /Parent at the fresh root.
    let mut leaf = resolve_dict(
        &mut target,
        copied_page_ref,
        "copied page is not a dictionary",
    )?;

    if !has_own(&leaf, "Resources") {
        if let Some(res) = inherited_resources {
            let mut value = Object::Dictionary(res);
            rewrite_refs(&mut value, &map);
            leaf.insert("Resources", value);
        }
    }
    if !has_own(&leaf, "MediaBox") {
        if let Some(mut mb) = inherited_mediabox {
            rewrite_refs(&mut mb, &map);
            leaf.insert("MediaBox", mb);
        }
    }
    if !has_own(&leaf, "CropBox") {
        if let Some(mut cb) = inherited_cropbox {
            rewrite_refs(&mut cb, &map);
            leaf.insert("CropBox", cb);
        }
    }
    if !has_own(&leaf, "Rotate") {
        leaf.insert("Rotate", Object::Integer(inherited_rotate as i64));
    }
    leaf.insert("Parent", Object::Reference(pages_root_ref));
    target.set_object(copied_page_ref, Object::Dictionary(leaf));

    // Build the fresh single-level /Pages root.
    let mut root = resolve_dict(
        &mut target,
        pages_root_ref,
        "target /Pages is not a dictionary",
    )?;
    root.insert(
        "Kids",
        Object::Array(vec![Object::Reference(copied_page_ref)]),
    );
    root.insert("Count", Object::Integer(1));
    target.set_object(pages_root_ref, Object::Dictionary(root));

    // Neutralize annotations on the extracted leaf whose destination targets a
    // page absent from this single-page output. Without this, an explicit
    // cross-page /Dest keeps the copied sibling-page stub (and its ancestor
    // /Pages) reachable, so the sweep below cannot prune them. qpdf-aligned:
    // the annotation is retained, only the dead destination is removed.
    neutralize_absent_dests(&mut target, copied_page_ref)?;

    // Drop the copied ancestor /Pages node(s) and any objects only they
    // referenced: they are unreachable from the new catalog now that the leaf
    // /Parent points at the fresh root. full_rewrite does NOT garbage-collect
    // (it emits every non-deleted object), so prune here to satisfy
    // "no unrelated objects". Same mark-and-sweep used after page-subset
    // rebuild (subset_prune::sweep_unreachable_objects).
    sweep_unreachable_objects(&mut target)?;

    Ok(target)
}

/// Drop cross-page `/GoTo` destinations from any annotation on `page_ref`, and
/// from the page's own `/AA`. A destination targeting a page other than
/// `page_ref` (i.e. a page absent from the single-page output) has its `/D`
/// dropped (annotation `/Dest`: the whole `/Dest` key); the action and chain
/// structure are otherwise retained. Named / string / `/URI` / `/GoToR`
/// destinations carry no in-document page reference and are left untouched.
fn neutralize_absent_dests(target: &mut Pdf<Cursor<Vec<u8>>>, page_ref: ObjectRef) -> Result<()> {
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
        neutralize_annot_if_absent(target, annot_ref, page_ref)?;
    }

    // Page-level /AA (open/close etc.). An inline /AA dict is rewritten back
    // onto the page; an indirect /AA is mutated in place via its own ref, so
    // the page needs no change in that case.
    if let Some(aa_val) = page_aa {
        if let Some(new_aa) = neutralize_aa_if_absent(target, &aa_val, page_ref)? {
            let mut page = resolve_dict(target, page_ref, "extracted page is not a dictionary")?;
            page.insert("AA", new_aa);
            target.set_object(page_ref, Object::Dictionary(page));
        }
    }
    Ok(())
}

/// Inspect one annotation; drop the cross-page destination from `/Dest`, `/A`,
/// and `/AA` when it resolves to a page other than `keep`.
fn neutralize_annot_if_absent(
    target: &mut Pdf<Cursor<Vec<u8>>>,
    annot_ref: ObjectRef,
    keep: ObjectRef,
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
    keep: ObjectRef,
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
/// dropping the `/D` of every GoTo whose destination targets a page other than
/// `keep`. Follows the `/A`->action indirection chain and `/Next` chains
/// (single action or array). Indirect cycles are bounded by `visited`; inline
/// `/Next` nesting by `depth`. Returns `Some(updated)` when an INLINE action
/// value must be stored back by the caller; returns `None` when there was no
/// change OR the change was applied in place via `set_object` on an indirect
/// action (the caller keeps pointing at the same ref).
fn neutralize_action_chain(
    target: &mut Pdf<Cursor<Vec<u8>>>,
    action_value: &Object,
    keep: ObjectRef,
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
    keep: ObjectRef,
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

/// `true` when `dest` resolves to an explicit page reference other than `keep`.
/// Named / string / external destinations (no resolvable in-doc page ref) and
/// self-links (`== keep`) return `false` — they are not neutralized.
fn dest_targets_absent_page(
    target: &mut Pdf<Cursor<Vec<u8>>>,
    dest: &Object,
    keep: ObjectRef,
) -> Result<bool> {
    Ok(match dest_page_ref_resolved(target, dest)? {
        Some(page_ref) => page_ref != keep,
        None => false,
    })
}

/// `true` when a GoTo `/SD` structure destination resolves to a page other than
/// `keep`. An `/SD` value is `[structElemRef /Fit ...]` (or an indirect ref to
/// one); the first element is a *structure element*, whose `/Pg` is the target
/// page (ISO 32000-2 §12.6.4.3). Named structure destinations (a name/string,
/// resolved via the structure tree) carry no in-document page ref and return
/// `false`. A missing / unresolvable / non-Page `/Pg`, or a `/Pg` pointing at
/// `keep`, returns `false` (kept conservatively). Each level may be indirect;
/// `resolve_ref_chain` bounds the indirection.
fn sd_targets_absent_page(
    target: &mut Pdf<Cursor<Vec<u8>>>,
    sd: &Object,
    keep: ObjectRef,
) -> Result<bool> {
    let (concrete, _) = resolve_ref_chain(target, sd)?;
    let Object::Array(arr) = concrete else {
        return Ok(false); // named structure destination or malformed
    };
    let Some(struct_elem) = arr.into_iter().next() else {
        return Ok(false);
    };
    let (se, _) = resolve_ref_chain(target, &struct_elem)?;
    let Some(se_dict) = se.into_dict() else {
        return Ok(false);
    };
    let Some(pg) = se_dict.get("Pg").cloned() else {
        return Ok(false);
    };
    let (pg_concrete, pg_ref) = resolve_ref_chain(target, &pg)?;
    Ok(match pg_ref {
        Some(r) => r != keep && is_page_dict(&pg_concrete),
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
fn minimal_target_bytes() -> Vec<u8> {
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
fn target_pages_root(target: &mut Pdf<Cursor<Vec<u8>>>) -> Result<ObjectRef> {
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
/// Shared by [`extract_page`]'s leaf/root materialization and
/// [`target_pages_root`]; the error arm guards against a ref resolving to a
/// non-dictionary (or a missing object, which resolves to [`Object::Null`]).
fn resolve_dict(
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
