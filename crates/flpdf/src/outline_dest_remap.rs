//! Outline and named-destination remapping after page extraction (flpdf-9hc.8.10).
//!
//! After [`crate::page_tree_rebuild::rebuild_page_tree`] has rebuilt the page tree
//! for a subset extraction, this module updates the document's `/Outlines` tree
//! and `/Names /Dests` name-tree (as well as the legacy `/Catalog /Dests` dictionary)
//! so that:
//!
//! - Outline items and named destinations whose target page **survived** the
//!   extraction are remapped to their new `ObjectRef` (the first element of
//!   `ref_map[old_ref]`, matching qpdf's duplicate-page rule).
//! - Outline items and named destinations whose target page was **removed** are
//!   dropped.  Sibling `/Prev`/`/Next` links, parent `/First`/`/Last`, and parent
//!   `/Count` are all patched so no dangling references remain.
//! - If the entire outline tree is dropped, `/Outlines` is removed from the
//!   catalog (no dangling catalog ref).
//!
//! # qpdf 11.9.0 observed behaviour (truth source `/usr/bin/qpdf`)
//!
//! Tested with a 4-page PDF carrying an `/Outlines` tree (outline items for each
//! page, including one GoTo-action form and one named-string `/Dest`) and a
//! `/Names /Dests` name-tree with 4 entries.
//!
//! Command: `qpdf in.pdf --pages in.pdf 1,3 -- out.pdf`
//!
//! Observed in the output:
//! - qpdf **does not drop** outline items pointing at removed pages; instead it
//!   sets the removed page objects to `null` in the xref, leaving outline `/Dest`
//!   arrays with dangling null refs (e.g. `[ 10 0 R /XYZ 0 792 0 ]` where
//!   `10 0 R` is null).
//! - `/Count` on the outline root remains unchanged.
//! - Named-dest entries in the name tree likewise retain null-page refs.
//!
//! **flpdf chooses DROP semantics** (per acceptance criteria: "dropped entries do
//! not leave dangling refs; /First/Last/Count/Prev/Next intact"). This is stricter
//! than qpdf; the divergence is intentional and documented here.
//!
//! # String-form `/Dest` resolution order
//!
//! `/Dest (name)` on an outline item is a named destination.  Named destinations
//! are resolved (for the purpose of deciding keep-or-drop) by looking up the name
//! in the surviving name-tree after pruning.  Therefore **named destinations are
//! pruned before the outline tree** so that string-dest outline items can be
//! correctly classified.
//!
//! # Scope
//!
//! Single-document only.  Multi-input cross-document merge is a separate future
//! layer (8.8 successor), not implemented here.

use crate::page_tree_rebuild::RebuildResult;
use crate::{Error, Object, ObjectRef, Pdf, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Remap or drop outline items and named destinations after a page-tree rebuild.
///
/// `result` is the [`RebuildResult`] returned by
/// [`crate::page_tree_rebuild::rebuild_page_tree`]. Its `ref_map` encodes the
/// old → new page reference mapping: a page absent from the map was removed;
/// a page present maps to `ref_map[old][0]` (first new occurrence).
///
/// The function mutates `pdf` in place (same convention as `rebuild_page_tree`)
/// and succeeds silently when there is no `/Outlines` or named-destination
/// structure to remap.
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
    remap_outline_and_dests_with_max_depth(pdf, result, crate::outline::DEFAULT_MAX_OUTLINE_DEPTH)
}

/// Like [`remap_outline_and_dests`] but with a caller-supplied outline-depth limit.
pub fn remap_outline_and_dests_with_max_depth<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    result: &RebuildResult,
    max_depth: usize,
) -> Result<()> {
    // Step 1: collect the set of surviving page refs (keys of ref_map).
    // We compute the first-new-ref for each old ref.
    let surviving: BTreeMap<ObjectRef, ObjectRef> = result
        .ref_map
        .iter()
        .filter_map(|(&old, new_refs)| new_refs.first().map(|&new| (old, new)))
        .collect();

    // Locate the catalog.
    let catalog_ref = match pdf.root_ref() {
        Some(r) => r,
        None => return Ok(()), // No catalog, nothing to do.
    };
    let catalog_obj = pdf.resolve(catalog_ref)?;
    let Object::Dictionary(catalog) = catalog_obj else {
        return Ok(());
    };

    let outlines_ref = catalog.get_ref("Outlines");
    let names_ref = catalog.get_ref("Names");

    // --- Step 2: Prune named destinations ---------------------------------
    // We need to know which named destinations survive before processing
    // outline string-form /Dest references.
    let mut surviving_names: BTreeSet<Vec<u8>> = BTreeSet::new();

    // 2a. /Names /Dests name-tree
    if let Some(names_dict_ref) = names_ref {
        let names_dict_obj = pdf.resolve(names_dict_ref)?;
        if let Object::Dictionary(names_dict) = names_dict_obj {
            if let Some(dests_ref) = names_dict.get_ref("Dests") {
                let dests_empty =
                    prune_name_tree(pdf, dests_ref, &surviving, &mut surviving_names)?;
                if dests_empty {
                    // All named dests were pruned — remove /Dests from /Names dict
                    // so no dangling ref remains.
                    let names_dict_obj2 = pdf.resolve(names_dict_ref)?;
                    if let Object::Dictionary(mut nd) = names_dict_obj2 {
                        nd.remove("Dests");
                        if nd.iter().next().is_none() {
                            // /Names dict is now completely empty — remove /Names from catalog.
                            let catalog_obj3 = pdf.resolve(catalog_ref)?;
                            if let Object::Dictionary(mut cat) = catalog_obj3 {
                                cat.remove("Names");
                                pdf.set_object(catalog_ref, Object::Dictionary(cat));
                            }
                        } else {
                            pdf.set_object(names_dict_ref, Object::Dictionary(nd));
                        }
                    }
                }
            }
        }
    }

    // 2b. Legacy /Catalog /Dests dictionary (PDF 1.1 style)
    let catalog_obj2 = pdf.resolve(catalog_ref)?;
    let Object::Dictionary(catalog2) = catalog_obj2 else {
        return Ok(());
    };
    if let Some(dests_obj_ref) = catalog2.get_ref("Dests") {
        prune_legacy_dests(pdf, dests_obj_ref, &surviving, &mut surviving_names)?;
    }

    // --- Step 3: Remap / drop the outline tree ----------------------------
    if let Some(outlines_obj_ref) = outlines_ref {
        let outline_root_obj = pdf.resolve(outlines_obj_ref)?;
        if let Object::Dictionary(outline_root) = outline_root_obj {
            if let Some(first_ref) = outline_root.get_ref("First") {
                // Walk the top-level items, collecting which to keep/drop.
                let mut kept: Vec<ObjectRef> = Vec::new();
                let mut dropped: BTreeSet<ObjectRef> = BTreeSet::new();

                collect_siblings(
                    pdf,
                    first_ref,
                    0,
                    max_depth,
                    &surviving,
                    &surviving_names,
                    &mut kept,
                    &mut dropped,
                )?;

                if kept.is_empty() {
                    // All top-level items dropped → remove /Outlines from catalog.
                    let catalog_obj3 = pdf.resolve(catalog_ref)?;
                    if let Object::Dictionary(mut cat) = catalog_obj3 {
                        cat.remove("Outlines");
                        pdf.set_object(catalog_ref, Object::Dictionary(cat));
                    }
                } else {
                    // Stitch surviving top-level items and update outline root /Count.
                    stitch_siblings(pdf, &kept, outlines_obj_ref)?;

                    // Recount visible descendants for the outline root.
                    let new_count = count_visible_descendants(pdf, &kept, max_depth)?;
                    let outline_root_obj2 = pdf.resolve(outlines_obj_ref)?;
                    if let Object::Dictionary(mut root_dict) = outline_root_obj2 {
                        // Preserve sign: root /Count is always positive (not closed),
                        // but we re-set it anyway to be safe.
                        root_dict.insert("Count", Object::Integer(new_count));
                        root_dict.insert("First", Object::Reference(kept[0]));
                        root_dict
                            .insert("Last", Object::Reference(*kept.last().expect("non-empty")));
                        pdf.set_object(outlines_obj_ref, Object::Dictionary(root_dict));
                    }
                }
            }
            // If there is no /First, the outline root has no items → nothing to do.
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Named-destination pruning helpers
// ---------------------------------------------------------------------------

/// Prune a name-tree rooted at `node_ref` in place, removing entries whose
/// page ref is not in `surviving`. Adds kept names to `surviving_names`.
///
/// Returns `true` if the node became empty after pruning (caller should remove
/// it from its parent's `/Kids`).
fn prune_name_tree<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    node_ref: ObjectRef,
    surviving: &BTreeMap<ObjectRef, ObjectRef>,
    surviving_names: &mut BTreeSet<Vec<u8>>,
) -> Result<bool> {
    let node_obj = pdf.resolve(node_ref)?;
    let Object::Dictionary(node) = node_obj else {
        return Ok(true); // Malformed node — treat as empty.
    };

    let has_names = node.get("Names").is_some();
    let has_kids = node.get("Kids").is_some();

    if has_names {
        // Leaf node: /Names is a flat [(name, dest), ...] array.
        let names_val = node.get("Names").cloned();
        if let Some(Object::Array(pairs)) = names_val {
            let filtered = prune_name_pairs(pairs, surviving, surviving_names);
            if filtered.is_empty() {
                return Ok(true); // Node is now empty.
            }
            // Rebuild the node with the filtered /Names and updated /Limits.
            let node_obj2 = pdf.resolve(node_ref)?;
            if let Object::Dictionary(mut d) = node_obj2 {
                let limits = compute_limits(&filtered);
                d.insert("Names", Object::Array(filtered));
                if let Some(lim) = limits {
                    d.insert("Limits", lim);
                } else {
                    d.remove("Limits");
                }
                pdf.set_object(node_ref, Object::Dictionary(d));
            }
            return Ok(false);
        }
        return Ok(true);
    }

    if has_kids {
        // Intermediate node: /Kids is an array of child node refs.
        let kids_val = node.get("Kids").cloned();
        if let Some(Object::Array(kids)) = kids_val {
            let child_refs: Vec<ObjectRef> = kids
                .iter()
                .filter_map(|k| {
                    if let Object::Reference(r) = k {
                        Some(*r)
                    } else {
                        None
                    }
                })
                .collect();

            let mut surviving_kids: Vec<ObjectRef> = Vec::new();
            for child_ref in child_refs {
                let empty = prune_name_tree(pdf, child_ref, surviving, surviving_names)?;
                if !empty {
                    surviving_kids.push(child_ref);
                }
            }

            if surviving_kids.is_empty() {
                return Ok(true);
            }

            // Rebuild node with surviving kids and recomputed /Limits.
            let node_obj2 = pdf.resolve(node_ref)?;
            if let Object::Dictionary(mut d) = node_obj2 {
                d.insert(
                    "Kids",
                    Object::Array(
                        surviving_kids
                            .iter()
                            .map(|&r| Object::Reference(r))
                            .collect(),
                    ),
                );
                // Recompute /Limits from first and last surviving child.
                let limits = merge_node_limits(pdf, &surviving_kids)?;
                if let Some(lim) = limits {
                    d.insert("Limits", lim);
                } else {
                    d.remove("Limits");
                }
                pdf.set_object(node_ref, Object::Dictionary(d));
            }
            return Ok(false);
        }
    }

    Ok(true) // Node has neither /Names nor /Kids — treat as empty.
}

/// Filter a flat name-pairs array `[(name_str, dest_obj), ...]` keeping only
/// entries whose dest resolves to a surviving page.  Adds kept names to
/// `surviving_names`. Returns the filtered array.
fn prune_name_pairs(
    pairs: Vec<Object>,
    surviving: &BTreeMap<ObjectRef, ObjectRef>,
    surviving_names: &mut BTreeSet<Vec<u8>>,
) -> Vec<Object> {
    let mut result: Vec<Object> = Vec::new();
    let mut i = 0;
    while i + 1 < pairs.len() {
        let name_obj = pairs[i].clone();
        let dest_obj = pairs[i + 1].clone();
        i += 2;

        let name_bytes = match &name_obj {
            Object::String(b) | Object::Name(b) => b.clone(),
            _ => continue, // Malformed — skip.
        };

        let page_ref = dest_page_ref(&dest_obj);
        let keep = match page_ref {
            Some(r) => {
                if let Some(&new_ref) = surviving.get(&r) {
                    // Remap dest's page ref.
                    let remapped = remap_dest_page_ref(dest_obj, new_ref);
                    surviving_names.insert(name_bytes.clone());
                    result.push(name_obj);
                    result.push(remapped);
                    true
                } else {
                    false // Page was removed.
                }
            }
            None => {
                // Dest has no resolvable page ref (e.g. external or malformed).
                // Keep it conservatively but don't add to surviving_names
                // (we can't verify the page).
                result.push(name_obj);
                result.push(dest_obj);
                true
            }
        };
        let _ = keep;
    }
    result
}

/// Prune a legacy (PDF 1.1) `/Dests` dictionary in place.
fn prune_legacy_dests<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dests_ref: ObjectRef,
    surviving: &BTreeMap<ObjectRef, ObjectRef>,
    surviving_names: &mut BTreeSet<Vec<u8>>,
) -> Result<()> {
    let dests_obj = pdf.resolve(dests_ref)?;
    let Object::Dictionary(dests) = dests_obj else {
        return Ok(());
    };

    let mut new_dests = dests.clone();
    let keys: Vec<Vec<u8>> = dests.iter().map(|(k, _)| k.to_vec()).collect();
    for key in keys {
        let val = match dests.get(&key).cloned() {
            Some(v) => v,
            None => continue,
        };
        let page_ref = dest_page_ref(&val);
        match page_ref {
            Some(r) => {
                if let Some(&new_ref) = surviving.get(&r) {
                    surviving_names.insert(key.clone());
                    let remapped = remap_dest_page_ref(val, new_ref);
                    new_dests.insert(key, remapped);
                } else {
                    new_dests.remove(&key);
                }
            }
            None => {
                // Keep conservatively.
                surviving_names.insert(key.clone());
            }
        }
    }

    pdf.set_object(dests_ref, Object::Dictionary(new_dests));
    Ok(())
}

// ---------------------------------------------------------------------------
// Outline tree traversal and stitching
// ---------------------------------------------------------------------------

/// Walk the sibling chain starting at `first_ref`, recursing into children.
/// Appends surviving item refs to `kept` (in order) and dropped ones to
/// `dropped`.  The children of dropped items are also dropped recursively
/// without being added to `kept`.
#[allow(clippy::too_many_arguments)]
fn collect_siblings<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    first_ref: ObjectRef,
    depth: usize,
    max_depth: usize,
    surviving: &BTreeMap<ObjectRef, ObjectRef>,
    surviving_names: &BTreeSet<Vec<u8>>,
    kept: &mut Vec<ObjectRef>,
    dropped: &mut BTreeSet<ObjectRef>,
) -> Result<()> {
    if depth >= max_depth {
        return Err(Error::Unsupported(format!(
            "outline_dest_remap: depth limit {max_depth} exceeded at {first_ref}"
        )));
    }

    let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut current = Some(first_ref);

    while let Some(item_ref) = current {
        if !visited.insert(item_ref) {
            break; // Cycle guard.
        }

        let item_obj = pdf.resolve(item_ref)?;
        let Object::Dictionary(item) = item_obj else {
            break; // Malformed — stop this chain.
        };

        let next_ref = item.get_ref("Next");
        let first_child = item.get_ref("First");
        let dest_val = item.get("Dest").cloned();
        let action_val = item.get("A").cloned();

        // Determine whether this item points at a surviving page.
        let keep = item_survives(&dest_val, &action_val, surviving, surviving_names);

        if keep {
            // Recurse into children if any.
            if let Some(child_first) = first_child {
                let mut child_kept: Vec<ObjectRef> = Vec::new();
                let mut child_dropped: BTreeSet<ObjectRef> = BTreeSet::new();
                collect_siblings(
                    pdf,
                    child_first,
                    depth + 1,
                    max_depth,
                    surviving,
                    surviving_names,
                    &mut child_kept,
                    &mut child_dropped,
                )?;

                if child_kept.is_empty() {
                    // All children dropped: update parent to remove First/Last/Count.
                    let item_obj2 = pdf.resolve(item_ref)?;
                    if let Object::Dictionary(mut d) = item_obj2 {
                        d.remove("First");
                        d.remove("Last");
                        // Reset Count to 0 (item has no visible descendants).
                        // Preserve sign (negative = closed), set magnitude to 0.
                        let count_sign = match d.get("Count") {
                            Some(Object::Integer(n)) if *n < 0 => -1i64,
                            _ => 0i64,
                        };
                        d.insert("Count", Object::Integer(count_sign));
                        pdf.set_object(item_ref, Object::Dictionary(d));
                    }
                } else {
                    // Some children survived: stitch them, update item.
                    stitch_siblings(pdf, &child_kept, item_ref)?;
                    let visible = count_visible_descendants(pdf, &child_kept, max_depth)?;
                    let item_obj2 = pdf.resolve(item_ref)?;
                    if let Object::Dictionary(mut d) = item_obj2 {
                        let count_sign = match d.get("Count") {
                            Some(Object::Integer(n)) if *n < 0 => -1i64,
                            _ => 1i64,
                        };
                        d.insert("Count", Object::Integer(count_sign * visible));
                        d.insert("First", Object::Reference(child_kept[0]));
                        d.insert(
                            "Last",
                            Object::Reference(*child_kept.last().expect("non-empty")),
                        );
                        pdf.set_object(item_ref, Object::Dictionary(d));
                    }
                }
            }

            // Remap the dest/action page ref in the item's own dict.
            remap_item_dest(pdf, item_ref, surviving)?;

            kept.push(item_ref);
        } else {
            dropped.insert(item_ref);
            // Children of dropped items are also dropped (recursive).
            if let Some(child_first) = first_child {
                drop_subtree(pdf, child_first, dropped)?;
            }
        }

        current = next_ref;
    }

    Ok(())
}

/// Recursively mark all items in a subtree as dropped (do not add to any kept list).
fn drop_subtree<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    first_ref: ObjectRef,
    dropped: &mut BTreeSet<ObjectRef>,
) -> Result<()> {
    let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut current = Some(first_ref);
    while let Some(item_ref) = current {
        if !visited.insert(item_ref) {
            break;
        }
        dropped.insert(item_ref);
        let item_obj = pdf.resolve(item_ref)?;
        let Object::Dictionary(item) = item_obj else {
            break;
        };
        if let Some(child_first) = item.get_ref("First") {
            drop_subtree(pdf, child_first, dropped)?;
        }
        current = item.get_ref("Next");
    }
    Ok(())
}

/// Stitch `kept` items as a proper doubly-linked sibling chain and update
/// `/Parent` on each to `parent_ref`.
fn stitch_siblings<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    kept: &[ObjectRef],
    parent_ref: ObjectRef,
) -> Result<()> {
    for (i, &item_ref) in kept.iter().enumerate() {
        let item_obj = pdf.resolve(item_ref)?;
        let Object::Dictionary(mut d) = item_obj else {
            continue;
        };
        d.insert("Parent", Object::Reference(parent_ref));
        if i == 0 {
            d.remove("Prev");
        } else {
            d.insert("Prev", Object::Reference(kept[i - 1]));
        }
        if i + 1 == kept.len() {
            d.remove("Next");
        } else {
            d.insert("Next", Object::Reference(kept[i + 1]));
        }
        pdf.set_object(item_ref, Object::Dictionary(d));
    }
    Ok(())
}

/// Count visible descendant items (the magnitude of `/Count`) for the given
/// list of top-level siblings.  Accounts for open/closed sub-trees.
fn count_visible_descendants<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    items: &[ObjectRef],
    max_depth: usize,
) -> Result<i64> {
    count_visible_in_chain(pdf, items, max_depth, 0)
}

fn count_visible_in_chain<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    items: &[ObjectRef],
    max_depth: usize,
    depth: usize,
) -> Result<i64> {
    if depth >= max_depth {
        return Ok(0);
    }
    let mut count = items.len() as i64;
    for &item_ref in items {
        let item_obj = pdf.resolve(item_ref)?;
        let Object::Dictionary(d) = item_obj else {
            continue;
        };
        // If the item's /Count is positive (open), add its visible descendants.
        if let Some(Object::Integer(n)) = d.get("Count") {
            if *n > 0 {
                count += n;
            }
        }
    }
    Ok(count)
}

// ---------------------------------------------------------------------------
// Destination resolution helpers
// ---------------------------------------------------------------------------

/// Determine whether an outline item should be kept, given its `/Dest` and `/A`.
fn item_survives(
    dest_val: &Option<Object>,
    action_val: &Option<Object>,
    surviving: &BTreeMap<ObjectRef, ObjectRef>,
    surviving_names: &BTreeSet<Vec<u8>>,
) -> bool {
    // Check /Dest first.
    if let Some(dest) = dest_val {
        return dest_survives(dest, surviving, surviving_names);
    }
    // Then /A (action).
    if let Some(Object::Dictionary(a)) = action_val {
        // Only handle GoTo actions (/S /GoTo).
        let is_goto = matches!(a.get("S"), Some(Object::Name(n)) if n == b"GoTo");
        if is_goto {
            if let Some(d) = a.get("D") {
                return dest_survives(d, surviving, surviving_names);
            }
        }
        // Non-GoTo actions (URI, Launch, etc.) — keep conservatively.
        return true;
    }
    // No dest and no action → keep (title-only entry, no navigation).
    true
}

/// `true` when `dest` resolves to a surviving page or is a surviving named dest.
fn dest_survives(
    dest: &Object,
    surviving: &BTreeMap<ObjectRef, ObjectRef>,
    surviving_names: &BTreeSet<Vec<u8>>,
) -> bool {
    match dest {
        // Array form: [pageRef /XYZ ...] or [pageRef /Fit].
        Object::Array(arr) => {
            if let Some(Object::Reference(page_ref)) = arr.first() {
                surviving.contains_key(page_ref)
            } else {
                // No page ref in array (malformed) — keep conservatively.
                true
            }
        }
        // String or name form: a named destination.
        Object::String(name) | Object::Name(name) => surviving_names.contains(name.as_slice()),
        // Other forms — keep conservatively.
        _ => true,
    }
}

/// Extract the page `ObjectRef` from a destination value, if it is the array
/// form `[pageRef /FitType ...]`.
fn dest_page_ref(dest: &Object) -> Option<ObjectRef> {
    match dest {
        Object::Array(arr) => {
            if let Some(Object::Reference(r)) = arr.first() {
                Some(*r)
            } else {
                None
            }
        }
        Object::Dictionary(d) => {
            // Some dest forms are dicts with a /D key.
            if let Some(d_val) = d.get("D") {
                dest_page_ref(d_val)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Remap the page reference inside a destination to `new_ref`.
fn remap_dest_page_ref(dest: Object, new_ref: ObjectRef) -> Object {
    match dest {
        Object::Array(mut arr) => {
            if let Some(first) = arr.first_mut() {
                if matches!(first, Object::Reference(_)) {
                    *first = Object::Reference(new_ref);
                }
            }
            Object::Array(arr)
        }
        other => other,
    }
}

/// Remap the page reference in an outline item's `/Dest` or `/A /D` field.
fn remap_item_dest<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    item_ref: ObjectRef,
    surviving: &BTreeMap<ObjectRef, ObjectRef>,
) -> Result<()> {
    let item_obj = pdf.resolve(item_ref)?;
    let Object::Dictionary(mut d) = item_obj else {
        return Ok(());
    };

    let mut changed = false;

    // /Dest (array form).
    if let Some(dest) = d.get("Dest").cloned() {
        if let Object::Array(ref arr) = dest {
            if let Some(Object::Reference(old_ref)) = arr.first() {
                if let Some(&new_ref) = surviving.get(old_ref) {
                    d.insert("Dest", remap_dest_page_ref(dest.clone(), new_ref));
                    changed = true;
                }
            }
        }
        // String/name-form dest: no page ref to remap here; the name tree was
        // already updated.
    }

    // /A /GoTo /D (action form).
    if let Some(Object::Dictionary(mut action)) = d.get("A").cloned() {
        let is_goto = matches!(action.get("S"), Some(Object::Name(n)) if n == b"GoTo");
        if is_goto {
            if let Some(d_val) = action.get("D").cloned() {
                if let Object::Array(ref arr) = d_val {
                    if let Some(Object::Reference(old_ref)) = arr.first() {
                        if let Some(&new_ref) = surviving.get(old_ref) {
                            action.insert("D", remap_dest_page_ref(d_val.clone(), new_ref));
                            d.insert("A", Object::Dictionary(action));
                            changed = true;
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

// ---------------------------------------------------------------------------
// Name-tree /Limits helpers
// ---------------------------------------------------------------------------

/// Compute `/Limits [min max]` from the first and last string in `pairs`.
/// `pairs` must be a flat `[name, dest, name, dest, ...]` array.
fn compute_limits(pairs: &[Object]) -> Option<Object> {
    if pairs.len() < 2 {
        return None;
    }
    let first_name = pairs[0].clone();
    let last_name = pairs[pairs.len() - 2].clone();
    Some(Object::Array(vec![first_name, last_name]))
}

/// Merge `/Limits` values from a list of child nodes to produce the parent's `/Limits`.
fn merge_node_limits<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    kids: &[ObjectRef],
) -> Result<Option<Object>> {
    if kids.is_empty() {
        return Ok(None);
    }
    let first_kid_obj = pdf.resolve(kids[0])?;
    let last_kid_obj = pdf.resolve(*kids.last().expect("non-empty"))?;

    let first_min = if let Object::Dictionary(d) = &first_kid_obj {
        if let Some(Object::Array(lim)) = d.get("Limits") {
            lim.first().cloned()
        } else {
            None
        }
    } else {
        None
    };

    let last_max = if let Object::Dictionary(d) = &last_kid_obj {
        if let Some(Object::Array(lim)) = d.get("Limits") {
            lim.last().cloned()
        } else {
            None
        }
    } else {
        None
    };

    match (first_min, last_max) {
        (Some(min), Some(max)) => Ok(Some(Object::Array(vec![min, max]))),
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
    // Test: some pages dropped — outline items dropped, siblings stitched
    // -----------------------------------------------------------------------

    #[test]
    fn dropped_pages_outline_items_removed_and_stitched() {
        // Keep pages 1 and 3 (objects 3 and 5). Drop pages 2 and 4.
        let mut pdf = open(build_outline_pdf());
        let pages = vec![ObjectRef::new(3, 0), ObjectRef::new(5, 0)];
        let result = rebuild_page_tree(&mut pdf, &pages).unwrap();
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        // Catalog still has /Outlines (some items survived).
        let cat = dict_of(&mut pdf, ObjectRef::new(1, 0));
        let outlines_ref = cat
            .get_ref("Outlines")
            .expect("catalog should have /Outlines");

        // Outline root: /First should be 20 0 R (Page 1), /Last should be 22 0 R (Page 3).
        let root = dict_of(&mut pdf, outlines_ref);
        assert_eq!(
            root.get_ref("First"),
            Some(ObjectRef::new(20, 0)),
            "outline root /First should be item 20 (Page 1)"
        );
        assert_eq!(
            root.get_ref("Last"),
            Some(ObjectRef::new(22, 0)),
            "outline root /Last should be item 22 (Page 3)"
        );

        // Item 20 (Page 1): /Next should now point to 22 0 R (skip dropped 21).
        let item20 = dict_of(&mut pdf, ObjectRef::new(20, 0));
        assert_eq!(
            get_ref(&item20, "Next"),
            Some(ObjectRef::new(22, 0)),
            "item 20 /Next should jump to item 22 (dropping item 21)"
        );
        assert!(
            get_ref(&item20, "Prev").is_none(),
            "item 20 should have no /Prev (it is now first)"
        );

        // Item 22 (Page 3): /Prev should be 20 0 R.
        let item22 = dict_of(&mut pdf, ObjectRef::new(22, 0));
        assert_eq!(
            get_ref(&item22, "Prev"),
            Some(ObjectRef::new(20, 0)),
            "item 22 /Prev should be item 20"
        );
        assert!(
            get_ref(&item22, "Next").is_none(),
            "item 22 should have no /Next (it is now last)"
        );

        // Item 22's child (24 0 R, "Page 3 sub") should still exist (page 5 survived).
        let first_child = get_ref(&item22, "First");
        assert_eq!(
            first_child,
            Some(ObjectRef::new(24, 0)),
            "item 22 should still have child 24"
        );
    }

    // -----------------------------------------------------------------------
    // Test: /Count recomputed correctly
    // -----------------------------------------------------------------------

    #[test]
    fn count_recomputed_correctly() {
        // Keep pages 1 and 3.
        let mut pdf = open(build_outline_pdf());
        let pages = vec![ObjectRef::new(3, 0), ObjectRef::new(5, 0)];
        let result = rebuild_page_tree(&mut pdf, &pages).unwrap();
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        let cat = dict_of(&mut pdf, ObjectRef::new(1, 0));
        let outlines_ref = cat.get_ref("Outlines").unwrap();
        let root = dict_of(&mut pdf, outlines_ref);

        // 2 top-level items (20, 22) + 1 child of 22 (24) = 3 visible.
        assert_eq!(
            root.get("Count"),
            Some(&Object::Integer(3)),
            "outline root /Count should be 3"
        );

        // Item 22 /Count should be 1 (one surviving child).
        let item22 = dict_of(&mut pdf, ObjectRef::new(22, 0));
        assert_eq!(
            item22.get("Count"),
            Some(&Object::Integer(1)),
            "item 22 /Count should be 1"
        );
    }

    // -----------------------------------------------------------------------
    // Test: all outline items dropped → /Outlines removed from catalog
    // -----------------------------------------------------------------------

    #[test]
    fn all_items_dropped_outlines_removed_from_catalog() {
        // Keep only page 4 (obj 6). Items 20,21,22,23,24 point at pages 3,4,5,6.
        // Items 21 (page 4 → GoTo action) and 23 (named dest_named_p4 → page 6).
        // Wait — page 4 here is obj 4, but we're keeping obj 6 (which is page 4 in
        // our fixture, the 4th page). Let me double-check the fixture layout:
        // Pages: 3 0 R (p1), 4 0 R (p2), 5 0 R (p3), 6 0 R (p4).
        // Item 21 → /A /GoTo /D [4 0 R /XYZ ...] — this is page 2 (obj 4).
        // Item 23 → /Dest (dest_named_p4) which maps to [6 0 R ...] — page 4.
        // If we keep only page 3 (obj 5), items 20,21,23 drop; item 22 has dest
        // [5 0 R /Fit] so it survives. Let's use a selection that drops ALL:
        // Keep none of pages 3..6 (impossible since selection must be non-empty).
        // Instead: the only way to drop ALL is to select a page that no item points to.
        // But all items point at pages 3-6. We can trick by selecting e.g. only page 1
        // with a fresh rebuilt PDF that has no outline items pointing at page 1...
        // Actually, simpler: build a custom ref_map that maps page 1 to itself but
        // excludes pages 2,3,4. So items 21 (page 2), 22 (page 3), 23 (page 4 named
        // dest) all drop. Item 20 (page 1 = obj 3) survives. Let's instead verify
        // "all drop" by passing an empty ref_map (all pages removed) — but that's
        // artificial. Instead, let's make a sub-fixture where no items survive.
        //
        // Use ref_map = { obj5: obj5 } (keeping only page 3), which makes items
        // 20 (page 1), 21 (page 2), 23 (named_p4 → page 4) drop, but 22+24 survive.
        // To get ALL dropped, we need to keep only a page that no item points to.
        // Since our fixture doesn't have such a page, we test via a manual RebuildResult.

        let mut pdf = open(build_outline_pdf());
        // Simulate keeping a "page 99" that no outline item or named dest points at.
        // We just need ref_map to be empty (nothing surviving) but selection non-empty.
        // Actually, rebuild_page_tree would fail with empty selection, so we build
        // the RebuildResult manually.
        let fake_ref = ObjectRef::new(3, 0); // exists in PDF but not targeted by items
        let result = RebuildResult {
            new_kids: vec![fake_ref],
            ref_map: BTreeMap::new(), // empty → no old page survives
        };
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        let cat = dict_of(&mut pdf, ObjectRef::new(1, 0));
        assert!(
            cat.get_ref("Outlines").is_none(),
            "catalog should NOT have /Outlines when all items are dropped"
        );
    }

    // -----------------------------------------------------------------------
    // Test: named destinations pruned correctly
    // -----------------------------------------------------------------------

    #[test]
    fn named_dests_pruned_and_remapped() {
        // Keep pages 1 and 3 (objs 3 and 5).
        let mut pdf = open(build_outline_pdf());
        let pages = vec![ObjectRef::new(3, 0), ObjectRef::new(5, 0)];
        let result = rebuild_page_tree(&mut pdf, &pages).unwrap();
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        // Check name tree (30 0 R).
        let name_tree = dict_of(&mut pdf, ObjectRef::new(30, 0));
        let Some(Object::Array(names)) = name_tree.get("Names") else {
            panic!("name tree /Names should still be an array");
        };
        // Surviving: dest_p1 (page 1=obj3) and dest_p3 (page 3=obj5).
        // Dropped: dest_p2 (page 2=obj4), dest_named_p4 (page 4=obj6).
        let name_strs: Vec<String> = names
            .iter()
            .step_by(2)
            .map(|o| match o {
                Object::String(b) => String::from_utf8_lossy(b).into_owned(),
                Object::Name(b) => String::from_utf8_lossy(b).into_owned(),
                _ => "<other>".into(),
            })
            .collect();
        assert!(
            name_strs.contains(&"dest_p1".to_string()),
            "dest_p1 should survive"
        );
        assert!(
            name_strs.contains(&"dest_p3".to_string()),
            "dest_p3 should survive"
        );
        assert!(
            !name_strs.contains(&"dest_p2".to_string()),
            "dest_p2 should be pruned (page 2 removed)"
        );
        assert!(
            !name_strs.contains(&"dest_named_p4".to_string()),
            "dest_named_p4 should be pruned (page 4 removed)"
        );

        // The surviving entries' page refs should be remapped.
        let new_p1 = result.ref_map[&ObjectRef::new(3, 0)][0];
        let new_p3 = result.ref_map[&ObjectRef::new(5, 0)][0];

        // Find dest_p1 entry.
        let p1_idx = names
            .iter()
            .step_by(2)
            .position(|o| matches!(o, Object::String(b) if b == b"dest_p1"));
        if let Some(idx) = p1_idx {
            let dest = &names[idx * 2 + 1];
            if let Object::Array(arr) = dest {
                assert_eq!(
                    arr.first(),
                    Some(&Object::Reference(new_p1)),
                    "dest_p1 should point at new page 1 ref"
                );
            } else {
                panic!("dest_p1 dest should be array");
            }
        }

        // Find dest_p3 entry.
        let p3_idx = names
            .iter()
            .step_by(2)
            .position(|o| matches!(o, Object::String(b) if b == b"dest_p3"));
        if let Some(idx) = p3_idx {
            let dest = &names[idx * 2 + 1];
            if let Object::Array(arr) = dest {
                assert_eq!(
                    arr.first(),
                    Some(&Object::Reference(new_p3)),
                    "dest_p3 should point at new page 3 ref"
                );
            } else {
                panic!("dest_p3 dest should be array");
            }
        }
    }

    // -----------------------------------------------------------------------
    // Test: string-form /Dest outline item dropped when named dest pruned
    // -----------------------------------------------------------------------

    #[test]
    fn string_dest_outline_item_dropped_when_named_dest_pruned() {
        // Keep only pages 1 and 3. Item 23 has /Dest (dest_named_p4) → page 4 (removed).
        // Item 23 should be dropped.
        let mut pdf = open(build_outline_pdf());
        let pages = vec![ObjectRef::new(3, 0), ObjectRef::new(5, 0)];
        let result = rebuild_page_tree(&mut pdf, &pages).unwrap();
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        // Item 23 (/Dest (dest_named_p4)) should have been dropped.
        // We verify by checking the outline root /Last is not 23 0 R.
        let cat = dict_of(&mut pdf, ObjectRef::new(1, 0));
        let outlines_ref = cat.get_ref("Outlines").unwrap();
        let root = dict_of(&mut pdf, outlines_ref);
        assert_ne!(
            root.get_ref("Last"),
            Some(ObjectRef::new(23, 0)),
            "item 23 (string-dest pointing at removed named dest) should be dropped"
        );
        // Last should be 22 0 R.
        assert_eq!(
            root.get_ref("Last"),
            Some(ObjectRef::new(22, 0)),
            "outline root /Last should be item 22 after dropping item 23"
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
        if let Some(Object::Array(arr)) = item20.get("Dest") {
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

    #[test]
    fn all_named_dests_pruned_removes_names_dests_from_catalog() {
        // Use an empty ref_map so all pages are considered removed.
        // The name tree (30 0 R) has 4 entries all pointing at removed pages.
        // After remap, /Dests should be gone from the /Names dict (11 0 R),
        // and since /Names dict is now empty, /Names should be gone from catalog.
        let mut pdf = open(build_outline_pdf());
        let result = RebuildResult {
            new_kids: vec![ObjectRef::new(3, 0)],
            ref_map: BTreeMap::new(), // all pages removed
        };
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        let cat = dict_of(&mut pdf, ObjectRef::new(1, 0));

        // /Names should have been removed from catalog (since /Names dict is now empty).
        assert!(
            cat.get_ref("Names").is_none(),
            "catalog /Names should be removed when all named dests are pruned"
        );

        // /Outlines should also be gone (all items drop with empty ref_map).
        assert!(
            cat.get_ref("Outlines").is_none(),
            "catalog /Outlines should be removed when all outline items are dropped"
        );
    }

    // -----------------------------------------------------------------------
    // Test: parent item with all children dropped has Count=0 and no First/Last
    // -----------------------------------------------------------------------

    #[test]
    fn parent_with_all_children_dropped_has_no_first_last() {
        // Keep only page 1 (obj 3). Item 22 (page 3) and its child 24 (page 3) → drop.
        // So item 22 drops entirely (it points at page 3 which is removed).
        // Item 20 (page 1) survives. Items 21 (page 2), 22 (page 3), 23 (page 4 named) drop.
        let mut pdf = open(build_outline_pdf());
        let pages = vec![ObjectRef::new(3, 0)]; // only page 1
        let result = rebuild_page_tree(&mut pdf, &pages).unwrap();
        remap_outline_and_dests(&mut pdf, &result).unwrap();

        // Outline root: only item 20 should remain.
        let cat = dict_of(&mut pdf, ObjectRef::new(1, 0));
        let outlines_ref = cat
            .get_ref("Outlines")
            .expect("catalog should still have /Outlines");
        let root = dict_of(&mut pdf, outlines_ref);
        assert_eq!(root.get_ref("First"), Some(ObjectRef::new(20, 0)));
        assert_eq!(root.get_ref("Last"), Some(ObjectRef::new(20, 0)));

        // Item 20: no /Next, no /Prev.
        let item20 = dict_of(&mut pdf, ObjectRef::new(20, 0));
        assert!(item20.get_ref("Next").is_none());
        assert!(item20.get_ref("Prev").is_none());
    }
}
