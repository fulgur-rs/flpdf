//! Push inherited page attributes down to `/Page` leaves and strip them from
//! interior `/Pages` nodes, mirroring qpdf's `pushInheritedAttributesToPage`
//! (`QPDF_optimization.cc:127-156`) together with the page-tree repairs its
//! `getAllPages` call performs first (`QPDF_pages.cc:39-138`). Linearization
//! runs this unconditionally before computing the linearization plan — qpdf
//! calls `optimize(..., allow_changes=true)` for linearized output
//! (`QPDFWriter.cc:2553`, in `QPDFWriter::writeLinearized`). The normal
//! (non-linearized) write path never performs this step and must keep emitting
//! `/Pages` nodes verbatim.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

use crate::object::{Dictionary, Object, ObjectRef};
use crate::ref_chain::terminal_ref_of_chain;
use crate::{Error, Pdf, Result};

/// The four page attributes a `/Pages` node may pass down to its descendants
/// (ISO 32000-2 §7.7.3.4 Table 30, "Inheritable").
// Alphabetical order, matching qpdf's own iteration order: `cur_pages.getKeys()`
// (QPDF_Dictionary.cc) returns keys via a sorted `std::set<std::string>`, so
// `QPDF_pages.cc`'s push loop visits inheritable keys as CropBox, MediaBox,
// Resources, Rotate. When a single node needs to mint more than one of these
// in the same visit (direct, non-indirect values), the mint order — and thus
// which new object number each gets — must match qpdf's, so this array is
// kept in that same order rather than declaration-convenient order.
const INHERITABLE_KEYS: [&[u8]; 4] = [b"CropBox", b"MediaBox", b"Resources", b"Rotate"];

/// Defensive cycle/depth bound. qpdf relies on an earlier `cache()` pass (which
/// repairs duplicate page objects and detects loops) before its own recursive
/// push runs unguarded; flpdf has no equivalent repair pass, so this function
/// guards itself. Matches the bound already used for page-tree walks elsewhere
/// in this crate ([`crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH`]).
const MAX_DEPTH: usize = crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH;

/// Push inherited attributes to every `/Page` leaf and strip them from interior
/// `/Pages` nodes, mutating `pdf` in place.
///
/// # Errors
///
/// Propagates any [`Error`] from resolving an object while walking the tree, and
/// returns [`Error::Unsupported`] if the tree exceeds [`MAX_DEPTH`].
pub(crate) fn push_inherited_attributes_to_pages<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<()> {
    let Some(root_ref) = pdf.root_ref() else {
        return Ok(());
    };
    let Some(mut pages_ref) = (match pdf.resolve_borrowed(root_ref)? {
        Object::Dictionary(d) => d.get_ref("Pages"),
        _ => None,
    }) else {
        return Ok(());
    };

    // qpdf's `pushInheritedAttributesToPage` calls `getAllPages` first
    // (QPDF_optimization.cc:138-140). `getAllPages` performs two families of
    // page-tree repair before the push sees the tree. First (QPDF_pages.cc:50-67)
    // it corrects a catalog whose `/Pages` points INTO the tree (e.g. at the
    // first page) instead of at the true root, by walking `/Parent` up to the
    // real root and rewriting the root `/Pages` — repair (6) below. Then
    // `getAllPagesInternal` (QPDF_pages.cc:77-138) repairs the tree itself:
    // cloning any `/Page` leaf reachable more than once in the `/Kids` tree
    // (:119-130), overriding mistyped interior/leaf `/Type` keys (:89-92,
    // :131-134), and defaulting a leaf's missing/invalid `/MediaBox` to
    // letter / ANSI A `[0 0 612 792]` when no ancestor supplies a rectangle
    // (:93-96, :104-112) — so the push below sees a well-formed tree.
    // qpdf 11.9.0's `getAllPagesInternal` performs these repairs unconditionally
    // (there is no xref-reconstruction gate anywhere in QPDF_pages.cc:77-138), so
    // flpdf runs them for every input, whether or not the xref was reconstructed.

    // (6) Correct a catalog whose `/Pages` points into the tree instead of at
    // the true root, by walking `/Parent` up (QPDF_pages.cc:50-67). This runs
    // before `repair_page_tree` so the subsequent walk (and `push_internal`
    // below) start from the corrected root, matching qpdf's getAllPages order.
    let mut seen_parent: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut corrected = pages_ref;
    let mut changed_pages = false;
    loop {
        let parent = match pdf.resolve_borrowed(corrected)? {
            Object::Dictionary(d) => match d.get("Parent") {
                Some(Object::Reference(r)) => Some(*r),
                // No `/Parent` (the true root) or a non-reference `/Parent`:
                // stop. qpdf would follow a direct-dict parent as a handle, but
                // flpdf needs a ref to rewrite/continue; such inputs are
                // unrealistic and have no golden.
                _ => None,
            },
            _ => None, // Not a dictionary: stop.
        };
        let Some(parent_ref) = parent else {
            break;
        };
        if !seen_parent.insert(corrected) {
            break; // Loop guard (qpdf's `seen.add`): a `/Parent` cycle.
        }
        corrected = parent_ref;
        changed_pages = true;
    }
    if changed_pages {
        if let Object::Dictionary(mut root_dict) = pdf.resolve(root_ref)? {
            root_dict.insert("Pages", Object::Reference(corrected));
            pdf.set_object(root_ref, Object::Dictionary(root_dict));
        }
        pages_ref = corrected;
    }

    let mut seen: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut clone_visited: BTreeSet<ObjectRef> = BTreeSet::new();
    // Compute the first free object number once and carry it through the
    // walk, incrementing per clone. Re-deriving it with `next_object_ref`
    // for every clone would rescan all object refs each time, making a
    // `/Kids` array with many duplicate leaves quadratic.
    let mut next_clone = next_object_ref(pdf)?;
    repair_page_tree(
        pdf,
        pages_ref,
        &mut seen,
        &mut clone_visited,
        &mut next_clone,
        0,
        false,
    )?;

    let mut key_ancestors: BTreeMap<&'static [u8], Vec<Object>> = BTreeMap::new();
    let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
    push_internal(pdf, pages_ref, &mut key_ancestors, &mut visited, 0)?;
    debug_assert!(
        key_ancestors.values().all(Vec::is_empty),
        "key_ancestors not empty after pushing inherited attributes to pages"
    );
    Ok(())
}

fn push_internal<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    node_ref: ObjectRef,
    key_ancestors: &mut BTreeMap<&'static [u8], Vec<Object>>,
    visited: &mut BTreeSet<ObjectRef>,
    depth: usize,
) -> Result<()> {
    if depth >= MAX_DEPTH {
        return Err(Error::Unsupported(format!(
            "page tree depth exceeds maximum of {MAX_DEPTH} at {node_ref}"
        )));
    }
    if !visited.insert(node_ref) {
        // Cycle guard: a node already on the path back to /Root. qpdf relies on
        // an earlier repair pass to make this unreachable; flpdf has none, so
        // this function defends itself. A well-formed tree never hits this.
        return Ok(());
    }

    let Object::Dictionary(mut dict) = pdf.resolve(node_ref)? else {
        return Ok(()); // Non-dictionary node: leave untouched (matches PageWalk's silent skip).
    };

    // Only an interior /Pages node has attributes to push down; a /Page leaf
    // reached here directly (e.g. a malformed /Root/Pages pointing straight at
    // a /Page, which flpdf's PageWalk tolerates leniently) has no /Kids to
    // push its own attributes to, so stripping them would drop real content.
    // qpdf never reaches an equivalent state: `Pages::cache()` (called before
    // its own push) throws on a /Kids-less root, or force-retypes a /Kids-
    // bearing one to /Pages first (QPDF_pages.cc) — flpdf has no matching
    // repair step, so guard here instead.
    let is_pages_node =
        matches!(dict.get("Type"), Some(Object::Name(n)) if n.as_slice() == b"Pages");
    if !is_pages_node {
        return Ok(());
    }

    let mut own_keys: Vec<&'static [u8]> = Vec::new();
    for &key in &INHERITABLE_KEYS {
        let Some(value) = dict.remove(key) else {
            continue;
        };
        // qpdf's collection loop iterates `cur_pages.getKeys()`
        // (QPDF_Dictionary.cc), which filters out any key whose value
        // resolves — following the FULL indirect-reference chain, not just
        // one hop — to null. Such a key is invisible to qpdf's loop, so it is
        // neither erased from this node nor pushed to descendants. Put a null
        // value back and skip it here to match: otherwise a null-valued key
        // on an interior /Pages node would shadow a real ancestor value
        // further up the stack instead of being transparent to it.
        let is_null = match &value {
            Object::Null => true,
            Object::Reference(r) => {
                let terminal = terminal_ref_of_chain(pdf, *r)?;
                matches!(pdf.resolve_borrowed(terminal)?, Object::Null)
            }
            _ => false,
        };
        if is_null {
            dict.insert(key, value);
            continue;
        }
        let value = match value {
            Object::Reference(_) => value, // already indirect: descendants share this ref
            Object::Array(_) | Object::Dictionary(_) => {
                // Direct (non-indirect) non-scalar value: mint a new indirect
                // object so descendants share ONE object instead of each
                // duplicating the structure inline (mirrors qpdf's
                // makeIndirectObject call in QPDF_optimization.cc:186-196).
                let new_ref = next_object_ref(pdf)?;
                pdf.set_object(new_ref, value);
                Object::Reference(new_ref)
            }
            // Integer/Real/Boolean/Name/String/Null: copy by value, no minting.
            scalar => scalar,
        };
        key_ancestors.entry(key).or_default().push(value);
        own_keys.push(key);
    }

    let kids = dict
        .get("Kids")
        .and_then(Object::as_array)
        .map(<[Object]>::to_vec);
    pdf.set_object(node_ref, Object::Dictionary(dict));

    if let Some(kids) = kids {
        for kid in &kids {
            let Object::Reference(kid_ref) = kid else {
                continue;
            };
            let is_pages_node = matches!(
                pdf.resolve_borrowed(*kid_ref)?,
                Object::Dictionary(d)
                    if matches!(d.get("Type"), Some(Object::Name(n)) if n.as_slice() == b"Pages")
            );
            if is_pages_node {
                push_internal(pdf, *kid_ref, key_ancestors, visited, depth + 1)?;
            } else {
                let Object::Dictionary(mut leaf) = pdf.resolve(*kid_ref)? else {
                    continue;
                };
                for (&key, values) in key_ancestors.iter() {
                    // A direct or indirect `null` counts as absent, matching
                    // qpdf's `contains()` (`!(*this)[key].null()` — resolves
                    // the FULL reference chain, not just one hop). Otherwise
                    // an explicit `/Resources null` leaf would keep the null
                    // instead of inheriting the ancestor's real value.
                    let leaf_value_is_present = match leaf.get(key) {
                        None | Some(Object::Null) => false,
                        Some(Object::Reference(r)) => {
                            let terminal = terminal_ref_of_chain(pdf, *r)?;
                            !matches!(pdf.resolve_borrowed(terminal)?, Object::Null)
                        }
                        Some(_) => true,
                    };
                    if !leaf_value_is_present {
                        if let Some(v) = values.last() {
                            leaf.insert(key, v.clone());
                        }
                    }
                }
                pdf.set_object(*kid_ref, Object::Dictionary(leaf));
            }
        }
    }

    for key in own_keys {
        if let Some(stack) = key_ancestors.get_mut(key) {
            stack.pop();
            if stack.is_empty() {
                key_ancestors.remove(key);
            }
        } // cov:ignore: unreachable — `own_keys` holds exactly the keys this
          // frame pushed onto `key_ancestors` above; every nested `push_internal`
          // call pops what it pushes before returning (balanced push/pop) and
          // any early `?` return skips this cleanup loop entirely, so the
          // stack for a key in `own_keys` is always still present here.
    }
    Ok(())
}

/// Partial mirror of qpdf 11.9.0 `getAllPagesInternal` (QPDF_pages.cc:77-138):
/// walk the `/Kids` tree depth-first, repairing page-tree nodes in place so the
/// subsequent inherited-attribute push sees a well-formed tree. Five repairs
/// from `getAllPagesInternal` are applied:
///
/// - **Interior `/Type`** (:89-92): a node reached as an interior node (one with
///   `/Kids`) whose `/Type` is not `/Pages` has it overridden to `/Pages`.
/// - **`/MediaBox` default** (:93-96, :104-112): `media_box` tracks whether this
///   node or any ancestor already supplies a `/MediaBox` rectangle; it is set
///   from this node's own `/MediaBox` (:93-96) and threaded into the recursion.
///   A leaf that lacks a valid `/MediaBox` rectangle while `media_box` is false
///   has its `/MediaBox` set to a direct letter / ANSI A array `[0 0 612 792]`.
///   Applied to the original leaf *before* the direct-leaf and duplicate-clone
///   decisions (qpdf order :104-112 before :113-130), so a minted object
///   inherits the defaulted box.
/// - **Direct leaf → indirect** (:113-118): a `/Kids` entry that is a direct
///   (inline) `/Page` dict with no `/Kids` of its own is minted into a fresh
///   indirect object (via the same running allocator as the clone below) and the
///   entry is rewritten to that reference (qpdf's `makeIndirectObject`). The
///   minted object carries NO synthesized `/Parent` (`makeIndirectObject` adds
///   none). A direct *interior* node (a direct dict WITH `/Kids`) is out of
///   scope: qpdf recurses into it in place (:101-102), which flpdf's
///   reference-keyed walk cannot do, so it is left direct and untouched (no
///   golden exists for this exotic shape).
/// - **Duplicate leaf** (:119-130): the first occurrence of a `/Page` leaf is
///   recorded; each later occurrence is replaced, in the parent's `/Kids` array,
///   by a fresh shallow copy of the leaf dict (indirect sub-objects such as
///   `/Contents` stay shared, and the original leaf's `/Parent` is kept — the
///   clone arm never flattens).
/// - **Leaf `/Type`** (:131-134): a leaf (no `/Kids`) whose `/Type` is not
///   `/Page` has it overridden to `/Page`, for both a first-occurrence leaf and
///   a freshly minted clone.
///
/// A well-formed tree (correct `/Type` keys, every leaf with a `/MediaBox` or an
/// ancestor supplying one, no shared leaf) is a complete no-op — no node is
/// rewritten and no object is minted. The walk order matches
/// `getAllPagesInternal` (depth-first, recursing into any kid that has a `/Kids`
/// key) so the minted clone object numbers match qpdf's. `next_clone` is the
/// running next free object number, threaded through the walk and incremented
/// per clone (qpdf allocates from a running maximum rather than rescanning),
/// which keeps cloning many duplicates linear rather than quadratic.
///
/// Direct `/Kids` entries: a direct *leaf* is mirrored (minted to indirect, the
/// **Direct leaf → indirect** repair above); a direct *interior* node remains
/// out of scope (qpdf's recurse-in-place at :101-102 is not mirrored). A direct
/// non-dictionary entry is skipped, as the inherited-attribute push does.
fn repair_page_tree<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    node_ref: ObjectRef,
    seen: &mut BTreeSet<ObjectRef>,
    visited: &mut BTreeSet<ObjectRef>,
    next_clone: &mut ObjectRef,
    depth: usize,
    media_box: bool,
) -> Result<()> {
    if depth >= MAX_DEPTH {
        return Err(Error::Unsupported(format!(
            "page tree depth exceeds maximum of {MAX_DEPTH} at {node_ref}"
        )));
    }
    if !visited.insert(node_ref) {
        // Cycle guard: qpdf throws here; flpdf tolerates (matching PageWalk) and
        // stops descending. A well-formed tree never hits this.
        return Ok(());
    }
    let Object::Dictionary(mut dict) = pdf.resolve(node_ref)? else {
        return Ok(()); // Non-dictionary node: nothing to walk.
    };
    let Some(mut kids) = dict
        .get("Kids")
        .and_then(Object::as_array)
        .map(<[Object]>::to_vec)
    else {
        return Ok(()); // No /Kids: a leaf reached directly, or a malformed node.
    };

    // `kids` is a local copy that does not borrow `pdf`, so we can rewrite an
    // entry in place while separately resolving/minting through `pdf`.
    let mut changed = false;

    // (2i) Override this interior node's /Type to /Pages if it is not already
    // (QPDF_pages.cc:89-92). qpdf runs getAllPagesInternal only on /Kids-bearing
    // nodes (getAllPages gates the root on `pages.hasKey("/Kids")`, recursion on
    // the kid's /Kids), so reaching here — past the /Kids guard — is exactly that
    // condition. An already-`/Pages` node is left untouched (no rewrite).
    if !type_name_is(&dict, b"Pages") {
        dict.insert("Type", Object::Name(b"Pages".to_vec()));
        changed = true;
    }

    // (2m) Track whether this node or any ancestor supplies a /MediaBox rectangle
    // (QPDF_pages.cc:93-96). Once true it stays true down the subtree, so a leaf
    // that inherits a rectangle is not defaulted. Threaded into the recursion.
    let media_box = media_box || is_rectangle(pdf, dict.get("MediaBox"))?;

    for kid in kids.iter_mut() {
        let kid_ref = if let Object::Reference(r) = &*kid {
            *r
        } else if let Object::Dictionary(d) = &*kid {
            // A DIRECT (inline) /Kids entry. qpdf 11.9.0's getAllPagesInternal
            // classifies interior-vs-leaf by `kid.hasKey("/Kids")` before any
            // direct→indirect conversion (QPDF_pages.cc:100-118), so branch the
            // same way here.
            if d.get("Kids").is_some() {
                // (1i) Direct *interior* node (a direct dict WITH /Kids). qpdf
                // recurses into it in place (:101-102); flpdf's reference-keyed
                // walk has no non-reference recursion path, so this is out of
                // scope — leave the entry direct and untouched. No golden exists
                // for this exotic shape.
                continue;
            }
            // (1l) Direct *leaf* (no /Kids): mint it into a fresh indirect object
            // and rewrite the /Kids entry to the new reference (makeIndirectObject,
            // :113-118). Draw from the SAME running allocator the duplicate-clone
            // arm uses so the object numbering matches qpdf. qpdf applies the
            // /MediaBox default (:104-112) to the direct dict BEFORE
            // makeIndirectObject (:113-118); minting first here and letting the
            // existing leaf branch below apply the default to the now-indirect
            // object yields the SAME object content and — since defaulting never
            // mints — the SAME object numbers, so this reordering is byte-faithful.
            let new_ref = *next_clone;
            let next_num = new_ref
                .number
                .checked_add(1)
                .ok_or_else(|| Error::Unsupported("object-number space exhausted".to_string()))?;
            *next_clone = ObjectRef::new(next_num, 0);
            // Move the inline dict out of the /Kids entry WITHOUT cloning; the
            // moved dict carries no /Parent and makeIndirectObject adds none.
            let owned = std::mem::replace(kid, Object::Reference(new_ref));
            pdf.set_object(new_ref, owned);
            changed = true;
            new_ref
        } else {
            continue; // Direct (non-dictionary) /Kids entry: skip, as push does.
        };
        // Classify the kid (interior /Pages node vs leaf) and, for a leaf,
        // snapshot its own /MediaBox value — all in a scope that ends the
        // immutable borrow of `pdf` before we mutate it. `leaf_media_box` is
        // unused for an interior node (the snapshot is a small scalar/array/ref).
        let (has_kids, leaf_media_box) = match pdf.resolve_borrowed(kid_ref)? {
            Object::Dictionary(d) => (d.get("Kids").is_some(), d.get("MediaBox").cloned()),
            _ => continue, // Reference to a non-dictionary: skip, as push does.
        };
        if has_kids {
            // Interior /Pages node: descend, threading the /MediaBox flag.
            repair_page_tree(
                pdf,
                kid_ref,
                seen,
                visited,
                next_clone,
                depth + 1,
                media_box,
            )?;
            continue;
        }
        // Leaf branch.
        // (2b) Default a missing/invalid /MediaBox on the ORIGINAL leaf FIRST —
        // before the duplicate-clone decision below — so a clone inherits the
        // defaulted box (QPDF_pages.cc:104-112 precedes :119-130). An ancestor
        // rectangle (tracked by `media_box`) suppresses the default.
        if !media_box && !is_rectangle(pdf, leaf_media_box.as_ref())? {
            if let Object::Dictionary(mut leaf) = pdf.resolve(kid_ref)? {
                leaf.insert(
                    "MediaBox",
                    Object::Array(vec![
                        Object::Integer(0),
                        Object::Integer(0),
                        Object::Integer(612),
                        Object::Integer(792),
                    ]),
                );
                pdf.set_object(kid_ref, Object::Dictionary(leaf));
            }
        }
        // (2c) Duplicate-leaf clone (QPDF_pages.cc:119-130). `leaf_ref` is the
        // original leaf for a first occurrence, or the freshly minted clone for a
        // duplicate; qpdf overrides the leaf /Type AFTER this clone decision
        // (:131-134), so both flow through the (2l) override below. The clone is
        // taken from the (possibly defaulted) original via `resolve`, so it
        // inherits any /MediaBox default applied just above.
        let leaf_ref = if seen.insert(kid_ref) {
            kid_ref // First occurrence of this leaf.
        } else {
            let clone = pdf.resolve(kid_ref)?; // Owned copy of the leaf dict.
            let new_ref = *next_clone;
            let next_num = new_ref
                .number
                .checked_add(1)
                .ok_or_else(|| Error::Unsupported("object-number space exhausted".to_string()))?;
            *next_clone = ObjectRef::new(next_num, 0);
            pdf.set_object(new_ref, clone);
            seen.insert(new_ref);
            *kid = Object::Reference(new_ref);
            changed = true;
            new_ref
        };
        // (2l) Override the leaf /Type to /Page if it is not already
        // (QPDF_pages.cc:131-134). Check via a borrow so a correctly typed leaf
        // (the common case) is never cloned or rewritten.
        let wrong_type = matches!(
            pdf.resolve_borrowed(leaf_ref)?,
            Object::Dictionary(d) if !type_name_is(d, b"Page")
        );
        if wrong_type {
            if let Object::Dictionary(mut leaf) = pdf.resolve(leaf_ref)? {
                leaf.insert("Type", Object::Name(b"Page".to_vec()));
                pdf.set_object(leaf_ref, Object::Dictionary(leaf));
            }
        }
    }
    if changed {
        dict.insert("Kids", Object::Array(kids));
        pdf.set_object(node_ref, Object::Dictionary(dict));
    }
    Ok(())
}

/// Allocate a fresh indirect-object reference (the new-object idiom used across
/// the crate): one past the current highest object number.
fn next_object_ref<R: Read + Seek>(pdf: &Pdf<R>) -> Result<ObjectRef> {
    let n = pdf
        .object_refs()
        .iter()
        .map(|r| r.number)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| Error::Unsupported("object-number space exhausted".to_string()))?;
    Ok(ObjectRef::new(n, 0))
}

/// True iff `dict`'s `/Type` is a `Name` equal to `want`. Mirrors the dictionary
/// half of qpdf's `isDictionaryOfType` (QPDFObjectHandle.cc:462-466); the caller
/// has already established that the object is a dictionary. `/Type` is matched
/// directly — missing, or a non-matching name, yields `false` — like the other
/// page-tree `/Type` checks in this module.
fn type_name_is(dict: &Dictionary, want: &[u8]) -> bool {
    matches!(dict.get("Type"), Some(Object::Name(n)) if n.as_slice() == want)
}

/// True iff `value` resolves to a `/MediaBox` rectangle: an `Array` of exactly
/// four numbers (`Integer` or `Real`). Mirrors qpdf's
/// `QPDFObjectHandle::isRectangle` (QPDFObjectHandle.cc:789-800 — an array whose
/// size is 4 and whose first four items are numbers). Both the array value and
/// each of its four elements are resolved through any indirect-reference chain
/// first, matching qpdf: `getKey("/MediaBox").isRectangle()` resolves the array,
/// and its per-item `isNumber()` dereferences an element that is itself an
/// indirect reference to a number (e.g. `[0 0 612 5 0 R]`, kept — not defaulted —
/// on qpdf 11.9.0). A missing key (`None`), a wrong-length array, or an element
/// that does not resolve to a number yields `false`.
fn is_rectangle<R: Read + Seek>(pdf: &mut Pdf<R>, value: Option<&Object>) -> Result<bool> {
    // Resolve the /MediaBox value itself through any indirect chain (as qpdf's
    // `getKey("/MediaBox")` does) and, if it is a four-element array, snapshot its
    // elements. The snapshot (four small scalars/refs — never a stream) is needed
    // to release the borrow on `pdf` before resolving each element below.
    let items: Vec<Object> = match value {
        None => return Ok(false),
        Some(Object::Reference(r)) => {
            let terminal = terminal_ref_of_chain(pdf, *r)?;
            match pdf.resolve_borrowed(terminal)? {
                Object::Array(items) if items.len() == 4 => items.clone(),
                _ => return Ok(false),
            }
        }
        Some(Object::Array(items)) if items.len() == 4 => items.clone(),
        Some(_) => return Ok(false),
    };
    // Every element must resolve to a number. qpdf's `isRectangle()` tests each
    // item with `isNumber()`, which dereferences an indirect reference before the
    // type check — verified on qpdf 11.9.0: a `/MediaBox [0 0 612 5 0 R]` whose
    // last element is an indirect number is kept, NOT overwritten with the default
    // — so each element's indirect chain is resolved here as well.
    for e in &items {
        let is_num = match e {
            Object::Integer(_) | Object::Real(_) | Object::RealLiteral { .. } => true,
            Object::Reference(r) => {
                let terminal = terminal_ref_of_chain(pdf, *r)?;
                let resolved = pdf.resolve_borrowed(terminal)?;
                // cov:ignore-start: rustfmt reflow of the `matches!` macro
                // parks the opening `matches!(` on its own line, and llvm-cov
                // instruments the invocation head separately from its arms;
                // the arms record hits (DA:555+, exercised by
                // `indirect_real_literal_mediabox_elem_is_recognized_...`)
                // but the head line always shows zero. Same behavior in
                // similar multi-line `matches!` sites across the crate.
                matches!(
                    resolved,
                    Object::Integer(_) | Object::Real(_) | Object::RealLiteral { .. }
                )
                // cov:ignore-end
            }
            _ => false,
        };
        if !is_num {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Pdf;
    use std::io::Cursor;

    /// One `/Pages` node, one `/Page` leaf, no inheritable keys anywhere.
    /// Object layout: 1 Catalog, 2 Pages, 3 Page.
    fn pdf_with_no_inheritable_keys() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn no_inheritable_keys_is_a_no_op() {
        let bytes = pdf_with_no_inheritable_keys();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");
        let before_count = pdf.object_refs().len();

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        assert_eq!(
            pdf.object_refs().len(),
            before_count,
            "no inheritable keys present anywhere: no object should be minted"
        );
        let page = pdf.resolve(ObjectRef::new(3, 0)).expect("page resolves");
        let Object::Dictionary(page_dict) = page else {
            panic!("page is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert!(
            page_dict.get("MediaBox").is_some(),
            "the page's own /MediaBox must be untouched"
        );
    }

    /// `/Pages` (2) has a direct, scalar `/Rotate 90`. `/Page` (3) has none.
    /// Object layout: 1 Catalog, 2 Pages, 3 Page.
    fn pdf_with_inherited_scalar_rotate() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 /Rotate 90 >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn scalar_rotate_is_copied_by_value_not_minted() {
        let bytes = pdf_with_inherited_scalar_rotate();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");
        let before_count = pdf.object_refs().len();

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        assert_eq!(
            pdf.object_refs().len(),
            before_count,
            "a scalar inherited value must never mint a new object"
        );

        let pages = pdf.resolve(ObjectRef::new(2, 0)).expect("pages resolves");
        let Object::Dictionary(pages_dict) = pages else {
            panic!("pages is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert!(
            pages_dict.get("Rotate").is_none(),
            "/Rotate must be stripped from the interior /Pages node"
        );

        let page = pdf.resolve(ObjectRef::new(3, 0)).expect("page resolves");
        let Object::Dictionary(page_dict) = page else {
            panic!("page is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            page_dict.get("Rotate"),
            Some(&Object::Integer(90)),
            "/Rotate must be pushed to the leaf as a direct (literal) value"
        );
    }

    /// `/Pages` (2) has a direct `/Resources` dict (non-scalar). `/Page` (3) has
    /// none. Object layout: 1 Catalog, 2 Pages, 3 Page.
    fn pdf_with_inherited_direct_resources() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 \
              /Resources << /Font << /F1 4 0 R >> >> >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn direct_non_scalar_resources_is_minted_as_new_object() {
        let bytes = pdf_with_inherited_direct_resources();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");
        let before_count = pdf.object_refs().len();

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        assert_eq!(
            pdf.object_refs().len(),
            before_count + 1,
            "a direct non-scalar inherited value must mint exactly one new object"
        );

        let pages = pdf.resolve(ObjectRef::new(2, 0)).expect("pages resolves");
        let Object::Dictionary(pages_dict) = pages else {
            panic!("pages is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert!(
            pages_dict.get("Resources").is_none(),
            "/Resources must be stripped from the interior /Pages node"
        );

        let page = pdf.resolve(ObjectRef::new(3, 0)).expect("page resolves");
        let Object::Dictionary(page_dict) = page else {
            panic!("page is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        let Some(Object::Reference(resources_ref)) = page_dict.get("Resources") else {
            // cov:ignore-start: unreachable — fixture always resolves to the expected type
            panic!("/Resources must be pushed to the leaf as an indirect reference, not inline");
            // cov:ignore-end
        };
        assert_eq!(
            resources_ref.number, 5,
            "the minted object must be the next free object number (4 was already in use)"
        );
        let minted = pdf.resolve(*resources_ref).expect("minted object resolves");
        let Object::Dictionary(minted_dict) = minted else {
            panic!("minted object is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert!(
            minted_dict.get("Font").is_some(),
            "the minted object must carry the original /Resources content"
        );
    }

    /// `/Pages` (2) has BOTH a direct `/CropBox` array and a direct `/MediaBox`
    /// array (both non-scalar, both need minting). `/Page` (3) has neither.
    /// qpdf mints in alphabetical key order (CropBox before MediaBox), so the
    /// CropBox object must get the lower object number.
    fn pdf_with_two_direct_non_scalar_keys_on_one_node() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 \
              /CropBox [0 0 100 100] /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(b"3 0 obj\n<< /Type /Page /Parent 2 0 R >>\nendobj\n");

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn multiple_direct_non_scalar_keys_mint_in_qpdf_alphabetical_order() {
        let bytes = pdf_with_two_direct_non_scalar_keys_on_one_node();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let leaf = pdf.resolve(ObjectRef::new(3, 0)).expect("leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("leaf is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        let Some(Object::Reference(crop_ref)) = leaf_dict.get("CropBox") else {
            panic!("/CropBox must be pushed as an indirect reference"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        let Some(Object::Reference(media_ref)) = leaf_dict.get("MediaBox") else {
            panic!("/MediaBox must be pushed as an indirect reference"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert!(
            crop_ref.number < media_ref.number,
            "/CropBox must mint before /MediaBox (qpdf's alphabetical getKeys() \
             order), got CropBox={crop_ref} MediaBox={media_ref}"
        );
    }

    /// `/Pages` (2) has `/Resources` as an *existing* indirect reference (4 0 R)
    /// rather than a direct dict. Two leaves (3, 5) both lack their own
    /// /Resources, so both must end up pointing at the SAME object 4 — no
    /// minting.
    fn pdf_with_already_indirect_resources_shared_by_two_pages() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 /Resources 4 0 R >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(b"4 0 obj\n<< /Font << /F1 6 0 R >> >>\nendobj\n");

        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"5 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let off6 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"6 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off5:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off6:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn already_indirect_value_is_shared_not_reminted() {
        let bytes = pdf_with_already_indirect_resources_shared_by_two_pages();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");
        let before_count = pdf.object_refs().len();

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        assert_eq!(
            pdf.object_refs().len(),
            before_count,
            "an already-indirect inherited value must never be re-minted"
        );

        for page_num in [3u32, 5] {
            let page = pdf
                .resolve(ObjectRef::new(page_num, 0))
                .unwrap_or_else(|e| panic!("page {page_num} resolves: {e}"));
            let Object::Dictionary(page_dict) = page else {
                panic!("page {page_num} is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
            };
            assert_eq!(
                page_dict.get("Resources"),
                Some(&Object::Reference(ObjectRef::new(4, 0))),
                "page {page_num} must share the original object 4, not a copy"
            );
        }
    }

    /// `/Pages` (2) has `/Resources` (4 0 R). The leaf `/Page` (3) has its OWN
    /// `/Resources` (5 0 R). The leaf's own value must win.
    fn pdf_with_leaf_local_resources_override() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources 4 0 R >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Resources 5 0 R >>\nendobj\n",
        );

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(b"4 0 obj\n<< /Font << /F1 6 0 R >> >>\nendobj\n");

        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(b"5 0 obj\n<< /Font << /F2 6 0 R >> >>\nendobj\n");

        let off6 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"6 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off5:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off6:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn leaf_local_value_is_never_overwritten() {
        let bytes = pdf_with_leaf_local_resources_override();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let page = pdf.resolve(ObjectRef::new(3, 0)).expect("page resolves");
        let Object::Dictionary(page_dict) = page else {
            panic!("page is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            page_dict.get("Resources"),
            Some(&Object::Reference(ObjectRef::new(5, 0))),
            "the leaf's own /Resources (5 0 R) must NOT be replaced by the \
             ancestor's (4 0 R)"
        );
    }

    /// 3-level tree: grandparent /Pages (2) supplies /Resources (4 0 R).
    /// Parent /Pages (3) supplies its OWN /Resources (5 0 R), shadowing the
    /// grandparent's for everything under it. Leaf /Page (6) has neither, so
    /// it must inherit the NEAREST ancestor's value (5 0 R from the parent),
    /// not the grandparent's (4 0 R).
    fn pdf_with_three_level_nearest_ancestor_wins() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources 4 0 R >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [6 0 R] /Count 1 \
              /Resources 5 0 R >>\nendobj\n",
        );

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(b"4 0 obj\n<< /Font << /F1 7 0 R >> >>\nendobj\n");

        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(b"5 0 obj\n<< /Font << /F2 7 0 R >> >>\nendobj\n");

        let off6 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"6 0 obj\n<< /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let off7 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"7 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 8\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off5:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off6:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off7:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 8 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn nearest_ancestor_value_wins_in_three_level_tree() {
        let bytes = pdf_with_three_level_nearest_ancestor_wins();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let leaf = pdf.resolve(ObjectRef::new(6, 0)).expect("leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("leaf is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            leaf_dict.get("Resources"),
            Some(&Object::Reference(ObjectRef::new(5, 0))),
            "the leaf must inherit the NEAREST ancestor's /Resources (5 0 R, \
             from the parent /Pages), not the grandparent's (4 0 R)"
        );

        // Both interior nodes must have /Resources stripped.
        let grandparent = pdf
            .resolve(ObjectRef::new(2, 0))
            .expect("grandparent resolves");
        let Object::Dictionary(gp_dict) = grandparent else {
            panic!("grandparent is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert!(gp_dict.get("Resources").is_none());

        let parent = pdf.resolve(ObjectRef::new(3, 0)).expect("parent resolves");
        let Object::Dictionary(parent_dict) = parent else {
            panic!("parent is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert!(parent_dict.get("Resources").is_none());
    }

    /// /Pages (2)'s /Kids includes itself (2 0 R) alongside a real leaf (3 0 R).
    /// The walk must not loop forever.
    fn pdf_with_self_referential_pages_node() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [2 0 R 3 0 R] /Count 1 >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn self_referential_pages_node_terminates() {
        let bytes = pdf_with_self_referential_pages_node();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        // Must return (Ok or Err), not hang. The test harness's own timeout
        // is the real backstop; this assertion documents the expected outcome.
        let result = push_inherited_attributes_to_pages(&mut pdf);
        assert!(
            result.is_ok(),
            "a self-referential /Kids entry must be skipped, not error"
        );
    }

    /// `/Root /Pages` (2) itself resolves to a non-dictionary object (a bare
    /// integer). The walk's very first `push_internal` call must bail via the
    /// "resolved node is not a dictionary" branch, not panic.
    fn pdf_with_non_dictionary_pages_root() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n42\nendobj\n");

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn non_dictionary_pages_root_is_a_no_op() {
        let bytes = pdf_with_non_dictionary_pages_root();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        let result = push_inherited_attributes_to_pages(&mut pdf);
        assert!(
            result.is_ok(),
            "a non-dictionary /Pages root must be a no-op, not an error: {result:?}"
        );
    }

    /// Trailer has no `/Root` entry at all. `push_inherited_attributes_to_pages`
    /// must bail out via its very first guard (`pdf.root_ref()` returns `None`).
    fn pdf_without_root() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 2 >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
        );
        pdf
    }

    #[test]
    fn missing_root_is_a_no_op() {
        let bytes = pdf_without_root();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF even without /Root");
        assert!(
            pdf.root_ref().is_none(),
            "fixture must have no /Root for this test to be meaningful"
        );

        let result = push_inherited_attributes_to_pages(&mut pdf);
        assert!(
            result.is_ok(),
            "a document with no /Root must be a no-op, not an error: {result:?}"
        );
    }

    /// `/Root` (1) itself resolves to a non-dictionary object (a bare integer),
    /// rather than a Catalog. `push_inherited_attributes_to_pages` must bail out
    /// via its "root did not resolve to a dictionary" match arm.
    fn pdf_with_non_dictionary_root() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n42\nendobj\n");

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn non_dictionary_root_is_a_no_op() {
        let bytes = pdf_with_non_dictionary_root();
        let mut pdf =
            Pdf::open(Cursor::new(bytes)).expect("valid PDF even with a non-dictionary /Root");

        let result = push_inherited_attributes_to_pages(&mut pdf);
        assert!(
            result.is_ok(),
            "a non-dictionary /Root must be a no-op, not an error: {result:?}"
        );
    }

    /// `/Root` (1) is a dictionary (a Catalog) but has no `/Pages` key at all.
    /// `push_inherited_attributes_to_pages` must bail out via the "no /Pages
    /// entry" branch, distinct from the non-dictionary-root case above.
    fn pdf_with_root_missing_pages_key() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn root_dict_without_pages_key_is_a_no_op() {
        let bytes = pdf_with_root_missing_pages_key();
        let mut pdf =
            Pdf::open(Cursor::new(bytes)).expect("valid PDF even with /Root lacking /Pages");

        let result = push_inherited_attributes_to_pages(&mut pdf);
        assert!(
            result.is_ok(),
            "a /Root dictionary with no /Pages key must be a no-op, not an error: {result:?}"
        );
    }

    /// The root `/Pages` node (2) carries an inheritable key (`/Rotate`) but has
    /// no `/Kids` entry at all (malformed — every `/Pages` node must have one).
    /// The walk must still strip `/Rotate` from the node and return `Ok`, not
    /// panic, even though there are no children to push it to.
    fn pdf_with_pages_node_missing_kids() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Rotate 90 >>\nendobj\n");

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn pages_node_without_kids_is_a_no_op_not_a_panic() {
        let bytes = pdf_with_pages_node_missing_kids();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        let result = push_inherited_attributes_to_pages(&mut pdf);
        assert!(
            result.is_ok(),
            "a /Pages node without /Kids must be a no-op, not an error: {result:?}"
        );

        let pages = pdf.resolve(ObjectRef::new(2, 0)).expect("pages resolves");
        assert!(
            matches!(&pages, Object::Dictionary(d) if d.get("Rotate").is_none()),
            "/Rotate must still be stripped from the /Pages node even with no \
             /Kids to push it to: {pages:?}"
        );
    }

    /// `/Pages` (2)'s `/Kids` mixes a direct (non-reference) entry (`42`), a
    /// reference to a non-dictionary object (3, a literal string), and one
    /// real `/Page` leaf (4). Both malformed entries must be skipped; the
    /// real leaf must still receive the inherited `/Rotate`.
    fn pdf_with_malformed_kids_entries() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [42 3 0 R 4 0 R] /Count 1 /Rotate 90 >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(b"3 0 obj\n(not a dictionary)\nendobj\n");

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn malformed_kids_entries_are_skipped_valid_leaf_still_pushed() {
        let bytes = pdf_with_malformed_kids_entries();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let leaf = pdf.resolve(ObjectRef::new(4, 0)).expect("leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("leaf is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            leaf_dict.get("Rotate"),
            Some(&Object::Integer(90)),
            "the one valid leaf must still receive the inherited /Rotate despite \
             the malformed sibling entries in /Kids"
        );
    }

    /// A /Pages chain `MAX_DEPTH + 1` nodes deep, terminating in one /Page leaf.
    fn pdf_with_excessive_pages_depth() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let depth = MAX_DEPTH + 1;
        // Object numbers: 1 = Catalog, 2..=(1+depth) = Pages chain,
        // (2+depth) = the leaf Page.
        let leaf_num = 2 + depth as u32;
        let mut offsets: Vec<u64> = Vec::with_capacity(1 + depth + 1);

        offsets.push(pdf.len() as u64);
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        for level in 0..depth {
            let this_num = 2 + level as u32;
            let next_ref = if level + 1 == depth {
                leaf_num
            } else {
                this_num + 1
            };
            offsets.push(pdf.len() as u64);
            pdf.extend_from_slice(
                format!(
                    "{this_num} 0 obj\n<< /Type /Pages /Kids [{next_ref} 0 R] /Count 1 >>\nendobj\n"
                )
                .as_bytes(),
            );
        }

        offsets.push(pdf.len() as u64);
        pdf.extend_from_slice(
            format!(
                "{leaf_num} 0 obj\n<< /Type /Page /Parent {} 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
                leaf_num - 1
            )
            .as_bytes(),
        );

        let total = offsets.len() + 1; // +1 for the free-list head at object 0
        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes());
        for off in &offsets {
            pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn excessive_depth_returns_unsupported_error() {
        let bytes = pdf_with_excessive_pages_depth();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        let result = push_inherited_attributes_to_pages(&mut pdf);
        assert!(
            matches!(result, Err(Error::Unsupported(_))),
            "a /Pages tree deeper than MAX_DEPTH must error, not stack-overflow: {result:?}"
        );
    }

    /// `/Pages` (2) has a scalar `/Rotate 90`. Leaf `/Page` (3) has its OWN
    /// direct, non-null, non-reference `/Rotate 270` — the `Some(_) => true`
    /// branch of the leaf-presence check (distinct from
    /// [`leaf_local_value_is_never_overwritten`], whose leaf override is an
    /// indirect reference and so exercises the `Object::Reference` arm
    /// instead).
    fn pdf_with_leaf_direct_scalar_override() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 /Rotate 90 >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Rotate 270 >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn leaf_direct_scalar_override_is_never_overwritten() {
        let bytes = pdf_with_leaf_direct_scalar_override();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let leaf = pdf.resolve(ObjectRef::new(3, 0)).expect("leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("leaf is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            leaf_dict.get("Rotate"),
            Some(&Object::Integer(270)),
            "the leaf's own direct (non-reference, non-null) /Rotate must win \
             over the ancestor's /Rotate 90"
        );
    }

    /// `/Pages` (2) has `/Resources` (4 0 R). Leaf `/Page` (3) has its own
    /// `/Resources` set to a DIRECT `null` (not absent). qpdf's `contains()`
    /// (`!(*this)[key].null()`) treats a null value the same as absent, so
    /// the ancestor's real value must still be inherited.
    fn pdf_with_leaf_direct_null_resources() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources 4 0 R >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Resources null >>\nendobj\n",
        );

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(b"4 0 obj\n<< /Font << /F1 5 0 R >> >>\nendobj\n");

        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off5:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn explicit_null_leaf_value_is_treated_as_absent() {
        let bytes = pdf_with_leaf_direct_null_resources();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let leaf = pdf.resolve(ObjectRef::new(3, 0)).expect("leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("leaf is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            leaf_dict.get("Resources"),
            Some(&Object::Reference(ObjectRef::new(4, 0))),
            "a direct null /Resources on the leaf must be treated as absent \
             and replaced by the inherited value, not left as null"
        );
    }

    /// Same shape as [`pdf_with_leaf_direct_null_resources`], but the leaf's
    /// `/Resources` is an INDIRECT reference (6 0 R) to an object that itself
    /// resolves to `null`, rather than a direct `null`. qpdf's `contains()`
    /// resolves references transparently before the null check.
    fn pdf_with_leaf_indirect_null_resources() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources 4 0 R >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Resources 6 0 R >>\nendobj\n",
        );

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(b"4 0 obj\n<< /Font << /F1 5 0 R >> >>\nendobj\n");

        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
        );

        let off6 = pdf.len() as u64;
        pdf.extend_from_slice(b"6 0 obj\nnull\nendobj\n");

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off5:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off6:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn indirect_reference_resolving_to_null_is_treated_as_absent() {
        let bytes = pdf_with_leaf_indirect_null_resources();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let leaf = pdf.resolve(ObjectRef::new(3, 0)).expect("leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("leaf is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            leaf_dict.get("Resources"),
            Some(&Object::Reference(ObjectRef::new(4, 0))),
            "an indirect /Resources reference resolving to null must be treated \
             as absent and replaced by the inherited value"
        );
    }

    /// Same shape as [`pdf_with_leaf_indirect_null_resources`], but the null
    /// is reached through a TWO-hop holder chain (6 0 R -> 7 0 R -> null),
    /// not a single hop. qpdf's `contains()` (`.null()` / `type_code()`)
    /// resolves the full reference chain, not just one hop.
    fn pdf_with_leaf_multi_hop_null_resources() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources 4 0 R >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Resources 6 0 R >>\nendobj\n",
        );

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(b"4 0 obj\n<< /Font << /F1 5 0 R >> >>\nendobj\n");

        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
        );

        let off6 = pdf.len() as u64;
        pdf.extend_from_slice(b"6 0 obj\n7 0 R\nendobj\n");

        let off7 = pdf.len() as u64;
        pdf.extend_from_slice(b"7 0 obj\nnull\nendobj\n");

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 8\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off5:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off6:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off7:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 8 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn leaf_multi_hop_null_reference_chain_is_treated_as_absent() {
        let bytes = pdf_with_leaf_multi_hop_null_resources();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");
        pdf.set_object(
            ObjectRef::new(6, 0),
            Object::Reference(ObjectRef::new(7, 0)),
        );

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let leaf = pdf.resolve(ObjectRef::new(3, 0)).expect("leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("leaf is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            leaf_dict.get("Resources"),
            Some(&Object::Reference(ObjectRef::new(4, 0))),
            "a /Resources reaching null through a two-hop reference chain \
             (6 0 R -> 7 0 R -> null) must be treated as absent and replaced \
             by the inherited value, not just a single-hop null"
        );
    }

    /// `/Root/Pages` (2) points DIRECTLY at a `/Type /Page` object with its own
    /// direct `/MediaBox` and no `/Kids` -- a malformed shape flpdf's `PageWalk`
    /// tolerates leniently. qpdf never reaches an equivalent state (`cache()`
    /// throws on a /Kids-less root, or force-retypes a /Kids-bearing one to
    /// /Pages first); flpdf must not strip this node's own attributes with
    /// nowhere to push them.
    fn pdf_with_page_typed_root_pages() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Page /MediaBox [0 0 612 792] >>\nendobj\n");

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn page_typed_root_pages_keeps_its_own_attributes() {
        let bytes = pdf_with_page_typed_root_pages();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");
        let before_count = pdf.object_refs().len();

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed, not error");

        assert_eq!(
            pdf.object_refs().len(),
            before_count,
            "no object should be minted when the root itself is not a /Pages node"
        );
        let root_page = pdf
            .resolve(ObjectRef::new(2, 0))
            .expect("root page resolves");
        let Object::Dictionary(root_page_dict) = root_page else {
            panic!("root page is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            root_page_dict.get("MediaBox"),
            Some(&Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(612),
                Object::Integer(792),
            ])),
            "/MediaBox must NOT be stripped from a /Page reached as the (malformed) \
             /Root/Pages entry, since it has no /Kids to push the value to"
        );
    }

    /// 3-level tree: grandparent `/Pages` (2) has a real, indirect
    /// `/Resources` (5 0 R). Child `/Pages` (3) has `/Resources` set to a
    /// DIRECT `null`, shadowing nothing per qpdf semantics (`getKeys()`
    /// filters null-valued keys entirely). Leaf `/Page` (4) has no local
    /// `/Resources`, so it must inherit the GRANDPARENT's real value, not the
    /// child's null.
    fn pdf_with_ancestor_direct_null_resources() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources 5 0 R >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [4 0 R] /Count 1 \
              /Resources null >>\nendobj\n",
        );

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<< /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(b"5 0 obj\n<< /Font << /F1 6 0 R >> >>\nendobj\n");

        let off6 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"6 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off5:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off6:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn ancestor_direct_null_value_does_not_shadow_grandparent() {
        let bytes = pdf_with_ancestor_direct_null_resources();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let child = pdf.resolve(ObjectRef::new(3, 0)).expect("child resolves");
        let Object::Dictionary(child_dict) = child else {
            panic!("child is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            child_dict.get("Resources"),
            Some(&Object::Null),
            "a null-valued /Resources on the child /Pages node must be left \
             in place, not erased — it is invisible to qpdf's getKeys(), so \
             qpdf's loop never touches it"
        );

        let leaf = pdf.resolve(ObjectRef::new(4, 0)).expect("leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("leaf is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            leaf_dict.get("Resources"),
            Some(&Object::Reference(ObjectRef::new(5, 0))),
            "the leaf must inherit the GRANDPARENT's real /Resources, not be \
             shadowed by the child's null"
        );
    }

    /// Same shape as [`pdf_with_ancestor_direct_null_resources`], but the
    /// child `/Pages`' `/Resources` is an INDIRECT reference (7 0 R) to an
    /// object that itself resolves to `null`, rather than a direct `null`.
    fn pdf_with_ancestor_indirect_null_resources() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources 5 0 R >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [4 0 R] /Count 1 \
              /Resources 7 0 R >>\nendobj\n",
        );

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<< /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(b"5 0 obj\n<< /Font << /F1 6 0 R >> >>\nendobj\n");

        let off6 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"6 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
        );

        let off7 = pdf.len() as u64;
        pdf.extend_from_slice(b"7 0 obj\nnull\nendobj\n");

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 8\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off5:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off6:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off7:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 8 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn ancestor_indirect_null_value_does_not_shadow_grandparent() {
        let bytes = pdf_with_ancestor_indirect_null_resources();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let child = pdf.resolve(ObjectRef::new(3, 0)).expect("child resolves");
        let Object::Dictionary(child_dict) = child else {
            panic!("child is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            child_dict.get("Resources"),
            Some(&Object::Reference(ObjectRef::new(7, 0))),
            "an indirect /Resources reference resolving to null on the child \
             /Pages node must be left in place, not erased"
        );

        let leaf = pdf.resolve(ObjectRef::new(4, 0)).expect("leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("leaf is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            leaf_dict.get("Resources"),
            Some(&Object::Reference(ObjectRef::new(5, 0))),
            "the leaf must inherit the GRANDPARENT's real /Resources, not be \
             shadowed by the child's indirect-null value"
        );
    }

    /// Same shape as [`pdf_with_ancestor_indirect_null_resources`], but the
    /// child `/Pages`' null is reached through a TWO-hop holder chain
    /// (7 0 R -> 8 0 R -> null), not a single hop.
    fn pdf_with_ancestor_multi_hop_null_resources() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources 5 0 R >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [4 0 R] /Count 1 \
              /Resources 7 0 R >>\nendobj\n",
        );

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<< /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(b"5 0 obj\n<< /Font << /F1 6 0 R >> >>\nendobj\n");

        let off6 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"6 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
        );

        let off7 = pdf.len() as u64;
        pdf.extend_from_slice(b"7 0 obj\n8 0 R\nendobj\n");

        let off8 = pdf.len() as u64;
        pdf.extend_from_slice(b"8 0 obj\nnull\nendobj\n");

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 9\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off5:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off6:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off7:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off8:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 9 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn ancestor_multi_hop_null_reference_chain_does_not_shadow_grandparent() {
        let bytes = pdf_with_ancestor_multi_hop_null_resources();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");
        pdf.set_object(
            ObjectRef::new(7, 0),
            Object::Reference(ObjectRef::new(8, 0)),
        );

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let child = pdf.resolve(ObjectRef::new(3, 0)).expect("child resolves");
        let Object::Dictionary(child_dict) = child else {
            panic!("child is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            child_dict.get("Resources"),
            Some(&Object::Reference(ObjectRef::new(7, 0))),
            "a /Resources reaching null through a two-hop reference chain on \
             the child /Pages node must be left in place, not erased"
        );

        let leaf = pdf.resolve(ObjectRef::new(4, 0)).expect("leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("leaf is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            leaf_dict.get("Resources"),
            Some(&Object::Reference(ObjectRef::new(5, 0))),
            "the leaf must inherit the GRANDPARENT's real /Resources, not be \
             shadowed by the child's two-hop null reference chain"
        );
    }

    /// The SAME `/Page` leaf (5) is a kid of TWO different `/Pages` parents:
    /// A (3, `/Rotate 90`) and B (4, `/Rotate 180`). Object layout:
    /// 1 Catalog, 2 root /Pages [3 4], 3 /Pages A [5], 4 /Pages B [5],
    /// 5 /Page (shared) /Contents 6, 6 (shared /Contents target).
    fn pdf_with_leaf_shared_by_two_parents() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [5 0 R] /Count 1 /Rotate 90 >>\nendobj\n",
        );

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [5 0 R] /Count 1 /Rotate 180 >>\nendobj\n",
        );

        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"5 0 obj\n<< /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] \
              /Contents 6 0 R >>\nendobj\n",
        );

        let off6 = pdf.len() as u64;
        pdf.extend_from_slice(b"6 0 obj\n<< >>\nendobj\n");

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off5:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off6:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn duplicate_leaf_across_two_parents_is_cloned() {
        let bytes = pdf_with_leaf_shared_by_two_parents();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");
        let before_count = pdf.object_refs().len();

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        // Exactly one clone minted (object 7 = next after the highest, 6).
        assert_eq!(
            pdf.object_refs().len(),
            before_count + 1,
            "a leaf shared by two parents must mint exactly one clone"
        );

        // Parent A keeps the original leaf; parent B now points at the clone.
        let a = pdf.resolve(ObjectRef::new(3, 0)).expect("A resolves");
        let Object::Dictionary(a_dict) = a else {
            panic!("A not a dict") // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            a_dict.get("Kids"),
            Some(&Object::Array(vec![Object::Reference(ObjectRef::new(
                5, 0
            ))])),
            "parent A must keep the original leaf 5 in its /Kids"
        );
        let b = pdf.resolve(ObjectRef::new(4, 0)).expect("B resolves");
        let Object::Dictionary(b_dict) = b else {
            panic!("B not a dict") // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            b_dict.get("Kids"),
            Some(&Object::Array(vec![Object::Reference(ObjectRef::new(
                7, 0
            ))])),
            "parent B's /Kids entry must be rewritten to the clone (7)"
        );

        // Original leaf inherits A's /Rotate 90; clone inherits B's /Rotate 180.
        let leaf = pdf.resolve(ObjectRef::new(5, 0)).expect("leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("leaf not a dict") // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(leaf_dict.get("Rotate"), Some(&Object::Integer(90)));
        let clone = pdf.resolve(ObjectRef::new(7, 0)).expect("clone resolves");
        let Object::Dictionary(clone_dict) = clone else {
            panic!("clone not a dict") // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            clone_dict.get("Rotate"),
            Some(&Object::Integer(180)),
            "the clone must inherit parent B's /Rotate 180 independently — a naive \
             skip would drop it"
        );

        // The clone keeps the ORIGINAL leaf's /Parent (3 = A), not B (4): the
        // clone arm never flattens, so qpdf leaves /Parent unfixed.
        assert_eq!(
            clone_dict.get("Parent"),
            Some(&Object::Reference(ObjectRef::new(3, 0))),
            "the clone must keep the original leaf's /Parent (A), not be re-pointed to B"
        );

        // Shallow copy: leaf and clone share the same indirect /Contents (6).
        assert_eq!(
            leaf_dict.get("Contents"),
            Some(&Object::Reference(ObjectRef::new(6, 0)))
        );
        assert_eq!(
            clone_dict.get("Contents"),
            Some(&Object::Reference(ObjectRef::new(6, 0))),
            "shallowCopy keeps indirect sub-objects (/Contents) shared, not duplicated"
        );

        // The root /Count is untouched (clone arm never flattens).
        let root_pages = pdf
            .resolve(ObjectRef::new(2, 0))
            .expect("root pages resolves");
        let Object::Dictionary(rp) = root_pages else {
            panic!("root pages not a dict") // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            rp.get("Count"),
            Some(&Object::Integer(2)),
            "/Count must be unchanged"
        );
    }

    /// One `/Pages` node lists the SAME leaf (3) twice in its `/Kids`.
    fn pdf_with_leaf_listed_twice() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 3 0 R] /Count 2 >>\nendobj\n",
        );
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn leaf_listed_twice_in_one_parent_is_cloned() {
        let bytes = pdf_with_leaf_listed_twice();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");
        let before_count = pdf.object_refs().len();

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        assert_eq!(
            pdf.object_refs().len(),
            before_count + 1,
            "one clone minted"
        );
        let pages = pdf.resolve(ObjectRef::new(2, 0)).expect("pages resolves");
        let Object::Dictionary(pages_dict) = pages else {
            panic!("not a dict") // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            pages_dict.get("Kids"),
            Some(&Object::Array(vec![
                Object::Reference(ObjectRef::new(3, 0)),
                Object::Reference(ObjectRef::new(4, 0)),
            ])),
            "the second occurrence must be rewritten to the clone (4), first kept as 3"
        );
    }

    /// One `/Pages` node lists the same leaf (3) THREE times: two clones minted.
    fn pdf_with_leaf_listed_thrice() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 3 0 R 3 0 R] /Count 3 >>\nendobj\n",
        );
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn leaf_appearing_three_times_mints_two_clones() {
        let bytes = pdf_with_leaf_listed_thrice();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");
        let before_count = pdf.object_refs().len();

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        assert_eq!(
            pdf.object_refs().len(),
            before_count + 2,
            "two clones minted"
        );
        let pages = pdf.resolve(ObjectRef::new(2, 0)).expect("pages resolves");
        let Object::Dictionary(pages_dict) = pages else {
            panic!("not a dict") // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            pages_dict.get("Kids"),
            Some(&Object::Array(vec![
                Object::Reference(ObjectRef::new(3, 0)),
                Object::Reference(ObjectRef::new(4, 0)),
                Object::Reference(ObjectRef::new(5, 0)),
            ])),
            "3x occurrence must become three distinct refs [3, clone 4, clone 5]"
        );
    }

    /// An ordinary two-page tree with NO shared leaf: the clone pass is a no-op.
    fn pdf_with_two_distinct_pages() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>\nendobj\n",
        );
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn no_duplicate_leaf_mints_nothing() {
        let bytes = pdf_with_two_distinct_pages();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");
        let before_count = pdf.object_refs().len();

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        assert_eq!(
            pdf.object_refs().len(),
            before_count,
            "a tree with no shared leaf must mint nothing (the clone pass is a no-op)"
        );
    }

    #[test]
    fn clone_pass_is_idempotent() {
        let bytes = pdf_with_leaf_shared_by_two_parents();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        push_inherited_attributes_to_pages(&mut pdf).expect("first push");
        let after_first = pdf.object_refs().len();
        push_inherited_attributes_to_pages(&mut pdf).expect("second push");

        assert_eq!(
            pdf.object_refs().len(),
            after_first,
            "re-running the pass must not clone again (no leaf appears twice after \
             the first run)"
        );
    }

    /// Repoint a fixture's `startxref` value to a bogus offset (9) so the strict
    /// parse fails and `open_with_repair` reconstructs the cross-reference table.
    fn damage_startxref(mut bytes: Vec<u8>) -> Vec<u8> {
        let needle = b"startxref\n";
        let pos = bytes
            .windows(needle.len())
            .position(|w| w == needle)
            .expect("fixture has startxref");
        let num_start = pos + needle.len();
        let num_end = bytes[num_start..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|i| num_start + i)
            .expect("startxref value ends in newline");
        bytes.splice(num_start..num_end, b"9".to_vec());
        bytes
    }

    /// The shared-leaf fixture, but with a bogus `startxref` offset so opening
    /// forces qpdf-style xref reconstruction. qpdf 11.9.0's `getAllPagesInternal`
    /// clones duplicate leaves unconditionally (QPDF_pages.cc:119-130, no
    /// reconstruction gate), and flpdf matches: the clone pass runs for
    /// reconstructed inputs too.
    fn pdf_shared_leaf_damaged_xref() -> Vec<u8> {
        damage_startxref(pdf_with_leaf_shared_by_two_parents())
    }

    #[test]
    fn reconstructed_xref_input_clones_shared_leaf() {
        let bytes = pdf_shared_leaf_damaged_xref();
        let mut pdf = Pdf::open_with_repair(Cursor::new(bytes)).expect("recovers");
        assert!(
            pdf.repair_diagnostics()
                .entries()
                .iter()
                .any(|d| d.message.contains("reconstruct cross-reference")),
            "fixture must actually reconstruct its xref for this test to be meaningful"
        );
        let before_count = pdf.object_refs().len();

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        // qpdf 11.9.0's getAllPagesInternal has no reconstruction gate, so the
        // duplicate leaf is cloned even for a reconstructed input (mirroring the
        // clean-parse `duplicate_leaf_across_two_parents_is_cloned`). Exactly one
        // clone minted (object 7 = next after the highest, 6).
        assert_eq!(
            pdf.object_refs().len(),
            before_count + 1,
            "a reconstructed-xref shared leaf must mint exactly one clone"
        );

        // Parent A keeps the original leaf 5; parent B is rewritten to the clone 7.
        let a = pdf.resolve(ObjectRef::new(3, 0)).expect("A resolves");
        let Object::Dictionary(a_dict) = a else {
            panic!("A not a dict") // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            a_dict.get("Kids"),
            Some(&Object::Array(vec![Object::Reference(ObjectRef::new(
                5, 0
            ))])),
            "parent A must keep the original leaf 5 in its /Kids"
        );
        let b = pdf.resolve(ObjectRef::new(4, 0)).expect("B resolves");
        let Object::Dictionary(b_dict) = b else {
            panic!("B not a dict") // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            b_dict.get("Kids"),
            Some(&Object::Array(vec![Object::Reference(ObjectRef::new(
                7, 0
            ))])),
            "parent B's /Kids entry must be rewritten to the clone (7)"
        );

        // Original leaf inherits A's /Rotate 90; the clone inherits B's /Rotate
        // 180 independently — a naive skip would drop the 180 entirely.
        let leaf = pdf.resolve(ObjectRef::new(5, 0)).expect("leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("leaf not a dict") // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(leaf_dict.get("Rotate"), Some(&Object::Integer(90)));
        let clone = pdf.resolve(ObjectRef::new(7, 0)).expect("clone resolves");
        let Object::Dictionary(clone_dict) = clone else {
            panic!("clone not a dict") // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            clone_dict.get("Rotate"),
            Some(&Object::Integer(180)),
            "the clone must inherit parent B's /Rotate 180 independently"
        );
    }

    #[test]
    fn reconstructed_interior_type_not_pages_is_overridden() {
        let bytes = damage_startxref(pdf_with_interior_type_not_pages());
        let mut pdf = Pdf::open_with_repair(Cursor::new(bytes)).expect("recovers");
        assert!(
            pdf.repair_diagnostics()
                .entries()
                .iter()
                .any(|d| d.message.contains("reconstruct cross-reference")),
            "fixture must actually reconstruct its xref for this test to be meaningful"
        );

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        // qpdf 11.9.0's getAllPagesInternal overrides a mistyped interior /Type
        // unconditionally (QPDF_pages.cc:89-92, no reconstruction gate), so the
        // override runs for a reconstructed input too.
        let interior = pdf
            .resolve(ObjectRef::new(3, 0))
            .expect("interior resolves");
        let Object::Dictionary(interior_dict) = interior else {
            panic!("interior is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            interior_dict.get("Type"),
            Some(&Object::Name(b"Pages".to_vec())),
            "the interior node's mistyped /Type /Foo must be overridden to /Pages \
             even when the xref was reconstructed"
        );
    }

    #[test]
    fn reconstructed_clean_input_is_a_no_op_repair() {
        // A well-formed single-page tree (no duplicate leaf, correct /Type,
        // correct root /Pages) with a damaged startxref. Removing the
        // `!reconstructed` gate must NOT regress such an input: repair (6) and
        // repair_page_tree are both no-ops, so no object is minted and the
        // inherited /Rotate is still pushed to the leaf as before.
        let bytes = damage_startxref(pdf_with_inherited_scalar_rotate());
        let mut pdf = Pdf::open_with_repair(Cursor::new(bytes)).expect("recovers");
        assert!(
            pdf.repair_diagnostics()
                .entries()
                .iter()
                .any(|d| d.message.contains("reconstruct cross-reference")),
            "fixture must actually reconstruct its xref for this test to be meaningful"
        );
        let before_count = pdf.object_refs().len();

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        assert_eq!(
            pdf.object_refs().len(),
            before_count,
            "a clean reconstructed input must not mint any object (repairs are no-ops)"
        );
        let pages = pdf.resolve(ObjectRef::new(2, 0)).expect("pages resolves");
        let Object::Dictionary(pages_dict) = pages else {
            panic!("pages is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert!(
            pages_dict.get("Rotate").is_none(),
            "/Rotate must still be stripped from the interior /Pages node"
        );
        let page = pdf.resolve(ObjectRef::new(3, 0)).expect("page resolves");
        let Object::Dictionary(page_dict) = page else {
            panic!("page is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            page_dict.get("Rotate"),
            Some(&Object::Integer(90)),
            "/Rotate must still be pushed to the leaf"
        );
    }

    /// An interior node (has `/Kids`) whose `/Type` is `/Foo`, not `/Pages`, and
    /// which carries an inheritable `/Rotate 90`. qpdf 11.9.0 getAllPagesInternal
    /// overrides the interior `/Type` to `/Pages` (QPDF_pages.cc:89-92); with the
    /// node correctly typed, the inherited-attribute push then strips its `/Rotate`
    /// and pushes it down to the leaf.
    fn pdf_with_interior_type_not_pages() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Foo /Parent 2 0 R /Kids [4 0 R] /Count 1 /Rotate 90 >>\nendobj\n",
        );

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<< /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn interior_type_not_pages_is_overridden() {
        let bytes = pdf_with_interior_type_not_pages();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");
        assert!(
            pdf.repair_diagnostics().entries().is_empty(),
            "fixture must parse without xref reconstruction (authored as a \
             clean-parse repair case): {:?}",
            pdf.repair_diagnostics().entries(), // cov:ignore: message arg formatted only on assertion failure (fixture never reconstructs)
        );

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let interior = pdf
            .resolve(ObjectRef::new(3, 0))
            .expect("interior resolves");
        let Object::Dictionary(interior_dict) = interior else {
            panic!("interior is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            interior_dict.get("Type"),
            Some(&Object::Name(b"Pages".to_vec())),
            "the interior node's mistyped /Type /Foo must be overridden to /Pages"
        );
        assert!(
            interior_dict.get("Rotate").is_none(),
            "/Rotate must be stripped from the now-correctly-typed interior /Pages node"
        );

        let leaf = pdf.resolve(ObjectRef::new(4, 0)).expect("leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("leaf is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            leaf_dict.get("Rotate"),
            Some(&Object::Integer(90)),
            "/Rotate must be pushed to the leaf — proving the interior override \
             let the push recognise the node as a /Pages node"
        );
    }

    /// A leaf (no `/Kids`) whose `/Type` is `/Bar`, not `/Page`. qpdf 11.9.0
    /// getAllPagesInternal overrides the leaf `/Type` to `/Page`
    /// (QPDF_pages.cc:131-134).
    fn pdf_with_leaf_type_not_page() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Bar /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn leaf_type_not_page_is_overridden() {
        let bytes = pdf_with_leaf_type_not_page();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let leaf = pdf.resolve(ObjectRef::new(3, 0)).expect("leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("leaf is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            leaf_dict.get("Type"),
            Some(&Object::Name(b"Page".to_vec())),
            "the leaf's mistyped /Type /Bar must be overridden to /Page"
        );
    }

    #[test]
    fn correct_types_unchanged_no_mint() {
        // A well-formed tree (root /Pages, leaf /Page): the /Type-override pass
        // is a complete no-op — no /Type is mutated and no object is minted.
        let bytes = pdf_with_no_inheritable_keys();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");
        let before_count = pdf.object_refs().len();

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        assert_eq!(
            pdf.object_refs().len(),
            before_count,
            "a correctly-typed tree must not mint any object"
        );
        let pages = pdf.resolve(ObjectRef::new(2, 0)).expect("pages resolves");
        let Object::Dictionary(pages_dict) = pages else {
            panic!("pages is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            pages_dict.get("Type"),
            Some(&Object::Name(b"Pages".to_vec())),
            "an already-correct interior /Type must be left untouched"
        );
        let page = pdf.resolve(ObjectRef::new(3, 0)).expect("page resolves");
        let Object::Dictionary(page_dict) = page else {
            panic!("page is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            page_dict.get("Type"),
            Some(&Object::Name(b"Page".to_vec())),
            "an already-correct leaf /Type must be left untouched"
        );
    }

    /// An interior node and a leaf that BOTH omit `/Type` entirely. qpdf 11.9.0
    /// getAllPagesInternal treats a missing `/Type` the same as a wrong one:
    /// `isDictionaryOfType` is false, so it sets `/Type` to `/Pages` (interior)
    /// and `/Page` (leaf).
    fn pdf_with_missing_types() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(b"3 0 obj\n<< /Parent 2 0 R /Kids [4 0 R] /Count 1 >>\nendobj\n");

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(b"4 0 obj\n<< /Parent 3 0 R /MediaBox [0 0 612 792] >>\nendobj\n");

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn type_missing_is_set() {
        let bytes = pdf_with_missing_types();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let interior = pdf
            .resolve(ObjectRef::new(3, 0))
            .expect("interior resolves");
        let Object::Dictionary(interior_dict) = interior else {
            panic!("interior is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            interior_dict.get("Type"),
            Some(&Object::Name(b"Pages".to_vec())),
            "an interior node with no /Type must have /Type set to /Pages"
        );

        let leaf = pdf.resolve(ObjectRef::new(4, 0)).expect("leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("leaf is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            leaf_dict.get("Type"),
            Some(&Object::Name(b"Page".to_vec())),
            "a leaf with no /Type must have /Type set to /Page"
        );
    }

    /// A `/Page` leaf with no `/MediaBox` and no ancestor supplying one. qpdf
    /// 11.9.0 getAllPagesInternal defaults such a leaf to letter/ANSI A
    /// `[0 0 612 792]` (QPDF_pages.cc:104-112).
    fn pdf_with_missing_mediabox_leaf() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(b"3 0 obj\n<< /Type /Page /Parent 2 0 R >>\nendobj\n");

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn missing_mediabox_leaf_gets_default() {
        let bytes = pdf_with_missing_mediabox_leaf();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");
        assert!(
            pdf.repair_diagnostics().entries().is_empty(),
            "fixture must parse without xref reconstruction (authored as a \
             clean-parse repair case): {:?}",
            pdf.repair_diagnostics().entries(), // cov:ignore: message arg formatted only on assertion failure (fixture never reconstructs)
        );

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let leaf = pdf.resolve(ObjectRef::new(3, 0)).expect("leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("leaf is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            leaf_dict.get("MediaBox"),
            Some(&Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(612),
                Object::Integer(792),
            ])),
            "a leaf with no /MediaBox and no ancestor /MediaBox must get the \
             letter/ANSI A default [0 0 612 792] as a direct 4-integer array"
        );
    }

    /// A grandparent `/Pages` node carries a valid `/MediaBox [0 0 200 300]`, the
    /// intermediate `/Pages` node and the leaf have none. qpdf 11.9.0 sets its
    /// `media_box` flag once any ancestor supplies a rectangle (QPDF_pages.cc:93-96)
    /// and threads that flag into the recursion, which suppresses the leaf default
    /// (`:104` `if (!media_box && ...)`); the leaf inherits the ancestor box instead.
    /// The three levels ensure the flag must propagate across a recursive call — a
    /// two-level tree would pass even if the flag were not threaded into recursion.
    fn pdf_with_ancestor_mediabox_no_leaf_mediabox() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 300] >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [4 0 R] /Count 1 >>\nendobj\n",
        );

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(b"4 0 obj\n<< /Type /Page /Parent 3 0 R >>\nendobj\n");

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn ancestor_mediabox_suppresses_default() {
        let bytes = pdf_with_ancestor_mediabox_no_leaf_mediabox();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let leaf = pdf.resolve(ObjectRef::new(4, 0)).expect("leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("leaf is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        // The ancestor's direct /MediaBox is minted into an indirect object by
        // the inherited-attribute push and shared by reference; resolve it.
        let mb_ref = leaf_dict
            .get_ref("MediaBox")
            .expect("leaf must inherit the ancestor /MediaBox as an indirect reference");
        let media_box = pdf.resolve(mb_ref).expect("/MediaBox ref resolves");
        assert_eq!(
            media_box,
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(200),
                Object::Integer(300),
            ]),
            "the leaf must inherit the ancestor /MediaBox [0 0 200 300], NOT the \
             [0 0 612 792] default — inheritance wins, the default is suppressed"
        );
    }

    /// A `/Page` leaf whose `/MediaBox` is present but is NOT a 4-number array
    /// (here `[0 0 612]`), with no ancestor `/MediaBox`. qpdf's `isRectangle()`
    /// is false for a non-4-number array, so getAllPagesInternal replaces it
    /// with the `[0 0 612 792]` default (QPDF_pages.cc:104-112).
    fn pdf_with_invalid_mediabox_leaf() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612] >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn invalid_mediabox_leaf_gets_default() {
        let bytes = pdf_with_invalid_mediabox_leaf();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let leaf = pdf.resolve(ObjectRef::new(3, 0)).expect("leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("leaf is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            leaf_dict.get("MediaBox"),
            Some(&Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(612),
                Object::Integer(792),
            ])),
            "a leaf whose /MediaBox is not a 4-number array must be replaced with \
             the [0 0 612 792] default"
        );
    }

    /// A `/Page` leaf that already has a valid `/MediaBox [0 0 100 100]` and no
    /// ancestor `/MediaBox`. qpdf's `isRectangle()` is true, so the default guard
    /// (`if (!media_box && !kid.getKey("/MediaBox").isRectangle())`) is false and
    /// the leaf is left untouched.
    fn pdf_with_valid_mediabox_leaf() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn valid_mediabox_leaf_unchanged() {
        let bytes = pdf_with_valid_mediabox_leaf();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let leaf = pdf.resolve(ObjectRef::new(3, 0)).expect("leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("leaf is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            leaf_dict.get("MediaBox"),
            Some(&Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(100),
            ])),
            "a leaf with an already-valid /MediaBox must be left unchanged (no default)"
        );
    }

    /// A `/Page` leaf whose `/MediaBox` is an *indirect reference* (4 0 R) to a
    /// valid 4-number array, with no ancestor `/MediaBox`. qpdf's
    /// `getKey("/MediaBox").isRectangle()` dereferences the reference before
    /// testing the shape, so it is a rectangle and the default is suppressed; the
    /// leaf keeps its reference untouched. Exercises `is_rectangle`'s indirect-
    /// reference arm (and `terminal_ref_of_chain`).
    fn pdf_with_indirect_reference_mediabox_leaf() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox 4 0 R >>\nendobj\n",
        );

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(b"4 0 obj\n[0 0 300 400]\nendobj\n");

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn indirect_reference_mediabox_leaf_is_recognized_and_not_defaulted() {
        let bytes = pdf_with_indirect_reference_mediabox_leaf();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let leaf = pdf.resolve(ObjectRef::new(3, 0)).expect("leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("leaf is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            leaf_dict.get("MediaBox"),
            Some(&Object::Reference(ObjectRef::new(4, 0))),
            "a leaf whose /MediaBox is an indirect reference to a valid rectangle \
             must be recognized as a rectangle and left as the reference (no default)"
        );
    }

    /// A `/Page` leaf whose `/MediaBox` is a DIRECT array with an ELEMENT that is
    /// an indirect reference to a number (`[0 0 612 4 0 R]`), with no ancestor
    /// `/MediaBox`. qpdf's `isRectangle()` tests each item with `isNumber()`, which
    /// dereferences the indirect element (verified on qpdf 11.9.0: the box is kept,
    /// not defaulted), so `is_rectangle` must resolve each element too and leave the
    /// leaf's `/MediaBox` untouched. Exercises `is_rectangle`'s per-element indirect
    /// arm (codex review r3522482671 on PR #453).
    fn pdf_with_indirect_element_mediabox_leaf() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 4 0 R] >>\nendobj\n",
        );

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(b"4 0 obj\n792\nendobj\n");

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn indirect_element_mediabox_leaf_is_recognized_and_not_defaulted() {
        let bytes = pdf_with_indirect_element_mediabox_leaf();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let leaf = pdf.resolve(ObjectRef::new(3, 0)).expect("leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("leaf is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            leaf_dict.get("MediaBox"),
            Some(&Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(612),
                Object::Reference(ObjectRef::new(4, 0)),
            ])),
            "a /MediaBox array whose element is an indirect reference to a number \
             is a valid rectangle (qpdf dereferences it) and must be left unchanged"
        );
    }

    /// A `/Page` leaf whose `/MediaBox` is an indirect reference (4 0 R) to a
    /// NON-rectangle (a three-element array `[0 0 300]`), no ancestor `/MediaBox`.
    /// After resolving the reference the value is not a four-number array, so it is
    /// not a rectangle and the default fires. Exercises `is_rectangle`'s
    /// reference-resolves-to-a-non-rectangle arm.
    fn pdf_with_reference_non_rectangle_mediabox_leaf() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox 4 0 R >>\nendobj\n",
        );

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(b"4 0 obj\n[0 0 300]\nendobj\n");

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn reference_non_rectangle_mediabox_leaf_gets_default() {
        let bytes = pdf_with_reference_non_rectangle_mediabox_leaf();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let leaf = pdf.resolve(ObjectRef::new(3, 0)).expect("leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("leaf is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            leaf_dict.get("MediaBox"),
            Some(&Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(612),
                Object::Integer(792),
            ])),
            "a /MediaBox that is an indirect reference to a non-rectangle must be \
             replaced with the [0 0 612 792] default"
        );
    }

    /// A `/Page` leaf shared by two `/Pages` parents where ONLY parent A (3)
    /// carries a `/MediaBox [0 0 200 300]`; parent B (4) has none and the shared
    /// leaf (5) has none. This makes qpdf 11.9.0's MediaBox-default-BEFORE-clone
    /// order observable: the leaf is first visited via A (media_box=true ⇒ default
    /// suppressed), then via B (media_box=false), where the default `[0 0 612 792]`
    /// is applied to the shared ORIGINAL (QPDF_pages.cc:104-112) and the clone is
    /// copied from it (`:119-130`). A same-parent duplicate cannot observe this —
    /// the first occurrence always defaults the original first.
    fn pdf_with_shared_leaf_mediabox_default() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>\nendobj\n",
        );

        // Parent A carries a /MediaBox ⇒ the leaf's first visit sees media_box=true.
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [5 0 R] /Count 1 \
              /MediaBox [0 0 200 300] >>\nendobj\n",
        );

        // Parent B has NO /MediaBox ⇒ the leaf's second visit sees media_box=false.
        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [5 0 R] /Count 1 >>\nendobj\n",
        );

        // Shared leaf: correctly typed /Page, NO local /MediaBox.
        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"5 0 obj\n<< /Type /Page /Parent 3 0 R /Contents 6 0 R >>\nendobj\n",
        );

        let off6 = pdf.len() as u64;
        pdf.extend_from_slice(b"6 0 obj\n<< >>\nendobj\n");

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off5:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off6:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn shared_leaf_mediabox_default_before_clone_ordering() {
        let bytes = pdf_with_shared_leaf_mediabox_default();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        // The duplicate leaf is cloned during the repair pass (which runs before the
        // push), so the clone takes the first free object number, 7. Parent B's
        // /Kids entry is rewritten to it. (The push separately mints A's direct
        // /MediaBox into an indirect object, object 8, so the total object count
        // grows by two — the clone is specifically 7.)
        let b = pdf.resolve(ObjectRef::new(4, 0)).expect("B resolves");
        let Object::Dictionary(b_dict) = b else {
            panic!("B not a dict") // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            b_dict.get("Kids"),
            Some(&Object::Array(vec![Object::Reference(ObjectRef::new(
                7, 0
            ))])),
            "parent B's /Kids entry must be rewritten to the clone (7)"
        );

        // The default runs BEFORE the clone and mutates the shared ORIGINAL on its
        // 2nd (B) visit, so the A page (the original leaf, 5) ends up [0 0 612 792]
        // — NOT A's inherited [0 0 200 300]. A reversed order (clone before default)
        // would leave the original at [0 0 200 300] and only default the clone.
        let default_box = Some(&Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(612),
            Object::Integer(792),
        ]));
        let leaf = pdf.resolve(ObjectRef::new(5, 0)).expect("leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("leaf not a dict") // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            leaf_dict.get("MediaBox"),
            default_box,
            "the shared original (A's page) must be defaulted to [0 0 612 792] on \
             the B visit, not keep A's inherited [0 0 200 300] (default-before-clone)"
        );
        let clone = pdf.resolve(ObjectRef::new(7, 0)).expect("clone resolves");
        let Object::Dictionary(clone_dict) = clone else {
            panic!("clone not a dict") // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            clone_dict.get("MediaBox"),
            default_box,
            "the clone (B's page) must inherit the defaulted [0 0 612 792]"
        );
        // The clone keeps the original leaf's /Parent (3 = A); the clone arm never
        // flattens.
        assert_eq!(
            clone_dict.get("Parent"),
            Some(&Object::Reference(ObjectRef::new(3, 0))),
            "the clone must keep the original leaf's /Parent (A)"
        );
    }

    /// A `/Page` leaf whose `/MediaBox` is a 4-element array with a NON-numeric
    /// element (`[0 0 612 /Foo]`), no ancestor `/MediaBox`. qpdf's `isRectangle()`
    /// requires every element to be a number, so this is not a rectangle and the
    /// default fires. Exercises `is_rectangle`'s non-numeric-element arm.
    fn pdf_with_non_numeric_mediabox_leaf() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 /Foo] >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn non_numeric_mediabox_element_gets_default() {
        let bytes = pdf_with_non_numeric_mediabox_leaf();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let leaf = pdf.resolve(ObjectRef::new(3, 0)).expect("leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("leaf is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            leaf_dict.get("MediaBox"),
            Some(&Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(612),
                Object::Integer(792),
            ])),
            "a /MediaBox array with a non-numeric element is not a rectangle and \
             must be replaced with the [0 0 612 792] default"
        );
    }

    /// A `/Page` leaf whose `/MediaBox` is four `Real` values
    /// (`[0.0 0.0 612.0 792.0]`), no ancestor `/MediaBox`. `Real` elements are
    /// numbers, so `isRectangle()` is true and the default is suppressed — the leaf
    /// keeps its real-valued box. Exercises `is_rectangle`'s `Real` arm.
    fn pdf_with_real_mediabox_leaf() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0.0 0.0 612.0 792.0] >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn real_mediabox_leaf_is_recognized_and_not_defaulted() {
        let bytes = pdf_with_real_mediabox_leaf();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let leaf = pdf.resolve(ObjectRef::new(3, 0)).expect("leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("leaf is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        // Fixture bytes are "0.0"/"612.0"/"792.0" — non-canonical for f64
        // (Rust's shortest-roundtrip yields "0", "612", "792") — so the parser
        // preserves the source literal via [`Object::RealLiteral`].
        let real_lit = |v: f64, s: &[u8]| Object::RealLiteral {
            value: v,
            literal: s.to_vec(),
        };
        assert_eq!(
            leaf_dict.get("MediaBox"),
            Some(&Object::Array(vec![
                real_lit(0.0, b"0.0"),
                real_lit(0.0, b"0.0"),
                real_lit(612.0, b"612.0"),
                real_lit(792.0, b"792.0"),
            ])),
            "a /MediaBox of four Real values is a valid rectangle and must be left \
             unchanged (no default)"
        );
    }

    /// A `/Page` leaf whose `/MediaBox` mixes direct integers with one
    /// **indirect** element that resolves to an [`Object::RealLiteral`]
    /// (source literal `792.0`, which the parser preserves because Rust's
    /// shortest form of `792.0f64` is `"792"`). Exercises `is_rectangle`'s
    /// `Reference → RealLiteral` arm at line 550-560.
    fn pdf_with_indirect_real_literal_mediabox_elem() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 4 0 R] >>\nendobj\n",
        );
        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(b"4 0 obj\n792.0\nendobj\n");
        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    /// A `/MediaBox` whose last element is an indirect ref to a
    /// [`Object::RealLiteral`] terminal is a valid rectangle and must NOT be
    /// overwritten with the `[0 0 612 792]` default. Guards the
    /// `matches!(resolved, ... | Object::RealLiteral { .. })` arm added
    /// alongside the RealLiteral variant.
    #[test]
    fn indirect_real_literal_mediabox_elem_is_recognized_and_not_defaulted() {
        let bytes = pdf_with_indirect_real_literal_mediabox_elem();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");
        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");
        let leaf = pdf.resolve(ObjectRef::new(3, 0)).expect("leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("leaf is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        let mb = leaf_dict.get("MediaBox").expect("MediaBox must be present");
        let Object::Array(items) = mb else {
            panic!("MediaBox must be an array, got {mb:?}"); // cov:ignore: unreachable — fixture always uses an array
        };
        // Fourth element must still be the indirect reference; the default
        // rectangle would have replaced it with an Integer(792).
        assert!(
            matches!(items.get(3), Some(Object::Reference(_))),
            "expected /MediaBox[3] to remain an indirect reference, got {:?}",
            items.get(3) // cov:ignore: format arg only evaluated on assert failure
        );
    }

    /// The root `/Pages` node's single `/Kids` entry is a DIRECT (inline) `/Page`
    /// dictionary, NOT an indirect reference. The inline leaf carries its own
    /// valid `/MediaBox` and correct `/Type /Page`, so ONLY the direct → indirect
    /// repair applies. Object layout: 1 Catalog, 2 Pages (holding the inline leaf
    /// in `/Kids`), 3 the leaf's `/Contents` target.
    fn pdf_with_direct_leaf_kid() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [<< /Type /Page \
              /MediaBox [0 0 612 792] /Contents 3 0 R >>] /Count 1 >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(b"3 0 obj\n<< >>\nendobj\n");

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    /// A DIRECT (inline) `/Page` leaf in `/Kids` must be minted into a fresh
    /// indirect object and the `/Kids` entry rewritten to the new reference,
    /// mirroring qpdf 11.9.0 `getAllPagesInternal`'s `makeIndirectObject`
    /// (QPDF_pages.cc:113-118). Observed from the qpdf 11.9.0 golden of the
    /// `direct-leaf-kid` fixture: the now-indirect leaf keeps its own `/MediaBox`,
    /// its `/Contents` reference, and `/Type /Page`, and gains NO `/Parent`
    /// (`makeIndirectObject` does not add one).
    #[test]
    fn direct_leaf_kid_is_made_indirect() {
        let bytes = pdf_with_direct_leaf_kid();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");
        assert!(
            pdf.repair_diagnostics().entries().is_empty(),
            "fixture must parse without xref reconstruction (authored as a \
             clean-parse repair case): {:?}",
            pdf.repair_diagnostics().entries(), // cov:ignore: message arg formatted only on assertion failure (fixture never reconstructs)
        );
        let before_count = pdf.object_refs().len();

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        assert_eq!(
            pdf.object_refs().len(),
            before_count + 1,
            "the direct leaf must be minted into exactly one new indirect object"
        );

        // The /Pages node's /Kids[0] must be rewritten from the inline dict to a
        // reference to the minted leaf (the next free number, 4).
        let pages = pdf.resolve(ObjectRef::new(2, 0)).expect("pages resolves");
        let Object::Dictionary(pages_dict) = pages else {
            panic!("pages is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            pages_dict.get("Kids"),
            Some(&Object::Array(vec![Object::Reference(ObjectRef::new(
                4, 0
            ))])),
            "/Kids[0] must be rewritten to an indirect reference to the minted leaf (4)"
        );

        // Resolving the minted reference yields the leaf dict unchanged.
        let leaf = pdf
            .resolve(ObjectRef::new(4, 0))
            .expect("minted leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("minted leaf is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            leaf_dict.get("Type"),
            Some(&Object::Name(b"Page".to_vec())),
            "the minted leaf must keep /Type /Page"
        );
        assert_eq!(
            leaf_dict.get("MediaBox"),
            Some(&Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(612),
                Object::Integer(792),
            ])),
            "the minted leaf must keep its own /MediaBox"
        );
        assert_eq!(
            leaf_dict.get("Contents"),
            Some(&Object::Reference(ObjectRef::new(3, 0))),
            "the minted leaf must preserve its /Contents reference"
        );
        // qpdf's makeIndirectObject does NOT synthesize a /Parent (the golden's
        // minted leaf has none); flpdf moves the inline dict verbatim and must not
        // add one either.
        assert_eq!(
            leaf_dict.get("Parent"),
            None,
            "the minted leaf must not gain a synthesized /Parent (makeIndirectObject adds none)"
        );
    }

    /// A DIRECT (inline) `/Page` leaf with NO `/MediaBox` and no ancestor
    /// `/MediaBox`. Object layout: 1 Catalog, 2 Pages (holding the inline leaf in
    /// `/Kids`), 3 the leaf's `/Contents` target.
    fn pdf_with_direct_leaf_kid_no_mediabox() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [<< /Type /Page \
              /Contents 3 0 R >>] /Count 1 >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(b"3 0 obj\n<< >>\nendobj\n");

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    /// A direct leaf with no `/MediaBox` must, after the pass, be minted into an
    /// indirect object that carries the `[0 0 612 792]` default. This pins qpdf
    /// 11.9.0's per-kid ORDER: the `/MediaBox` default (QPDF_pages.cc:104-112)
    /// runs BEFORE `makeIndirectObject` (:113-118), so the minted object inherits
    /// the defaulted box rather than being minted from the box-less original.
    #[test]
    fn direct_leaf_without_mediabox_minted_object_gets_default() {
        let bytes = pdf_with_direct_leaf_kid_no_mediabox();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");
        assert!(
            pdf.repair_diagnostics().entries().is_empty(),
            "fixture must parse without xref reconstruction (authored as a \
             clean-parse repair case): {:?}",
            pdf.repair_diagnostics().entries(), // cov:ignore: message arg formatted only on assertion failure (fixture never reconstructs)
        );

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        // The minted leaf takes the next free object number (4).
        let leaf = pdf
            .resolve(ObjectRef::new(4, 0))
            .expect("minted leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("minted leaf is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            leaf_dict.get("MediaBox"),
            Some(&Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(612),
                Object::Integer(792),
            ])),
            "the minted indirect leaf must carry the [0 0 612 792] default \
             (default-before-makeIndirectObject ordering)"
        );
    }

    /// A `/Pages` node whose single `/Kids` entry is a DIRECT dict that itself has
    /// `/Kids` — a direct *interior* node. Object layout: 1 Catalog, 2 Pages
    /// (holding the inline interior node in `/Kids`), 3 the real `/Page` leaf
    /// under the interior node, 4 the leaf's `/Contents` target.
    fn pdf_with_direct_interior_node() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [<< /Type /Pages \
              /Kids [3 0 R] /Count 1 >>] /Count 1 >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Contents 4 0 R >>\nendobj\n",
        );

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(b"4 0 obj\n<< >>\nendobj\n");

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    /// A direct *interior* node (a direct dict WITH `/Kids`) is out of scope for
    /// the direct → indirect repair: 11.9.0's `makeIndirectObject` conversion is
    /// LEAF-ONLY (QPDF_pages.cc:113-118 is in the `else` branch, i.e. a kid with
    /// no `/Kids`), while a direct interior node is recursed in place (:101-102).
    /// flpdf's reference-keyed repair pass has no non-reference recursion path, so
    /// it leaves such a node direct and untouched. This test pins the no-panic
    /// behavior and that the direct entry stays direct — NOT byte parity (there is
    /// no qpdf golden for this exotic shape).
    #[test]
    fn direct_interior_node_is_skipped() {
        let bytes = pdf_with_direct_interior_node();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");
        assert!(
            pdf.repair_diagnostics().entries().is_empty(),
            "fixture must NOT trip xref reconstruction: {:?}",
            pdf.repair_diagnostics().entries(), // cov:ignore: message arg formatted only on assertion failure (fixture never reconstructs)
        );

        let result = push_inherited_attributes_to_pages(&mut pdf);
        assert!(
            result.is_ok(),
            "a direct interior node must be skipped, not panic or error: {result:?}"
        );

        // The direct interior /Kids entry is left direct (not converted to a ref).
        let pages = pdf.resolve(ObjectRef::new(2, 0)).expect("pages resolves");
        let Object::Dictionary(pages_dict) = pages else {
            panic!("pages is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        let Some(Object::Array(kids)) = pages_dict.get("Kids") else {
            panic!("/Kids must still be an array"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert!(
            matches!(kids.first(), Some(Object::Dictionary(_))),
            "the direct interior node must stay a direct dict (out-of-scope, not minted)"
        );
    }

    /// A `/Pages` node whose single `/Kids` entry is a DIRECT non-dictionary
    /// value (a bare integer). Object layout: 1 Catalog, 2 Pages (holding the
    /// scalar in `/Kids`).
    fn pdf_with_direct_scalar_kid() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [42] /Count 1 >>\nendobj\n");

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    /// A DIRECT non-dictionary `/Kids` entry (a bare integer) is skipped: qpdf's
    /// leaf branch would still call `makeIndirectObject` on it (its `isIndirect`
    /// check does not require a dictionary), but flpdf's reference-keyed repair
    /// pass mints only direct *dictionary* leaves and otherwise leaves the entry
    /// as the inherited-attribute push does. This pins the no-panic behavior and
    /// that the scalar entry stays direct — NOT byte parity (no golden for this
    /// malformed shape).
    #[test]
    fn direct_non_dictionary_kid_is_skipped() {
        let bytes = pdf_with_direct_scalar_kid();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");
        let before_count = pdf.object_refs().len();

        let result = push_inherited_attributes_to_pages(&mut pdf);
        assert!(
            result.is_ok(),
            "a direct non-dictionary /Kids entry must be skipped, not panic or error: {result:?}"
        );
        assert_eq!(
            pdf.object_refs().len(),
            before_count,
            "a direct non-dictionary /Kids entry must never mint an object"
        );

        let pages = pdf.resolve(ObjectRef::new(2, 0)).expect("pages resolves");
        let Object::Dictionary(pages_dict) = pages else {
            panic!("pages is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            pages_dict.get("Kids"),
            Some(&Object::Array(vec![Object::Integer(42)])),
            "the scalar /Kids entry must stay a direct integer (not minted to a ref)"
        );
    }

    /// The catalog's `/Pages` points INTO the page tree — at the first-page LEAF
    /// (obj 3), whose `/Parent` is the true root `/Pages` node (obj 2). Object
    /// layout: 1 Catalog (`/Pages 3 0 R`, WRONG), 2 the true root `/Pages` (no
    /// `/Parent`), 3 the `/Page` leaf (`/Parent 2 0 R`), 4 the leaf's `/Contents`
    /// target.
    fn pdf_with_root_pages_pointing_into_tree() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 3 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Contents 4 0 R >>\nendobj\n",
        );

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(b"4 0 obj\n<< >>\nendobj\n");

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    /// A catalog whose mis-aimed `/Pages` points at the leaf must have it walked
    /// up its `/Parent` chain to the true root and rewritten, mirroring qpdf
    /// 11.9.0 getAllPages's root->/Pages correction (QPDF_pages.cc:50-67).
    /// Observed from the qpdf 11.9.0 golden of the `root-pages-points-into-tree`
    /// fixture: the output catalog `/Pages` points at the true root `/Pages` node
    /// (the one with `/Kids` and no `/Parent`), NOT the leaf.
    #[test]
    fn root_pages_pointing_into_tree_is_corrected() {
        let bytes = pdf_with_root_pages_pointing_into_tree();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");
        assert!(
            pdf.repair_diagnostics().entries().is_empty(),
            "fixture must parse without xref reconstruction (authored as a \
             clean-parse repair case): {:?}",
            pdf.repair_diagnostics().entries(), // cov:ignore: message arg formatted only on assertion failure (fixture never reconstructs)
        );

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let catalog = pdf.resolve(ObjectRef::new(1, 0)).expect("catalog resolves");
        let Object::Dictionary(catalog_dict) = catalog else {
            panic!("catalog is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            catalog_dict.get("Pages"),
            Some(&Object::Reference(ObjectRef::new(2, 0))),
            "the catalog's /Pages must be corrected from the leaf (3 0 R) to the \
             true root /Pages node (2 0 R)"
        );
    }

    /// The catalog's `/Pages` points at the LEAF two `/Parent` hops below the true
    /// root. Object layout: 1 Catalog (`/Pages 4 0 R`, WRONG), 2 the true root
    /// `/Pages` (no `/Parent`), 3 a mid `/Pages` (`/Parent 2 0 R`, `/Kids [4 0 R]`),
    /// 4 the `/Page` leaf (`/Parent 3 0 R`). The walk must follow 4 -> 3 -> 2 all
    /// the way to the true root, not stop after the first hop.
    fn pdf_with_root_pages_two_hop_chain() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 4 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [4 0 R] /Count 1 >>\nendobj\n",
        );

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<< /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] \
              /Contents 5 0 R >>\nendobj\n",
        );

        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(b"5 0 obj\n<< >>\nendobj\n");

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off5:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn root_pages_two_hop_chain_corrected_to_true_root() {
        let bytes = pdf_with_root_pages_two_hop_chain();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");
        assert!(
            pdf.repair_diagnostics().entries().is_empty(),
            "fixture must parse without xref reconstruction (authored as a \
             clean-parse repair case): {:?}",
            pdf.repair_diagnostics().entries(), // cov:ignore: message arg formatted only on assertion failure (fixture never reconstructs)
        );

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        // The walk must climb BOTH hops (4 -> 3 -> 2) to the true root, not stop at
        // the first parent (3). A "stop after one hop" bug would leave /Pages at 3.
        let catalog = pdf.resolve(ObjectRef::new(1, 0)).expect("catalog resolves");
        let Object::Dictionary(catalog_dict) = catalog else {
            panic!("catalog is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            catalog_dict.get("Pages"),
            Some(&Object::Reference(ObjectRef::new(2, 0))),
            "the catalog's /Pages must be corrected two hops up to the true root \
             (2 0 R), not left at the mid node (3 0 R)"
        );
    }

    #[test]
    fn correct_root_pages_is_unchanged() {
        // A well-formed tree: the catalog's /Pages already points at the true root
        // /Pages node (obj 2, no /Parent). The root->/Pages correction is a
        // complete no-op — /Pages is left pointing at the same object.
        let bytes = pdf_with_no_inheritable_keys();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let catalog = pdf.resolve(ObjectRef::new(1, 0)).expect("catalog resolves");
        let Object::Dictionary(catalog_dict) = catalog else {
            panic!("catalog is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            catalog_dict.get("Pages"),
            Some(&Object::Reference(ObjectRef::new(2, 0))),
            "a catalog /Pages already at the true root must be left unchanged"
        );
    }

    /// The catalog's `/Pages` (node A, obj 2) has a `/Parent` cycle: A's `/Parent`
    /// is B (obj 3) and B's `/Parent` is A. Object layout: 1 Catalog
    /// (`/Pages 2 0 R`), 2 node A (`/Parent 3 0 R`, `/Kids [4 0 R]`), 3 node B
    /// (`/Parent 2 0 R`), 4 the `/Page` leaf under A. Walking the `/Parent` chain
    /// to find the true root must terminate on the loop guard, not spin forever.
    fn pdf_with_root_pages_parent_cycle() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Parent 3 0 R /Kids [4 0 R] /Count 1 >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(b"3 0 obj\n<< /Type /Pages /Parent 2 0 R >>\nendobj\n");

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn root_pages_parent_cycle_terminates() {
        let bytes = pdf_with_root_pages_parent_cycle();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");
        assert!(
            pdf.repair_diagnostics().entries().is_empty(),
            "fixture must parse without xref reconstruction (authored as a \
             clean-parse repair case; reconstruction would mask the loop guard): {:?}",
            pdf.repair_diagnostics().entries(), // cov:ignore: message arg formatted only on assertion failure (fixture never reconstructs)
        );

        // The /Parent chain A(2) -> B(3) -> A(2) is a cycle; the root->/Pages
        // correction's walk up the chain must terminate on the loop guard, not
        // hang. The harness timeout is the real backstop; this assertion documents
        // the expected outcome (pins the loop guard).
        let result = push_inherited_attributes_to_pages(&mut pdf);
        assert!(
            result.is_ok(),
            "a /Parent cycle above the page tree root must terminate, not error: {result:?}"
        );

        // qpdf's walk breaks when it re-visits A: pages = A(2) -> B(3) -> A(2)
        // (already seen) -> break, leaving pages = A(2), which it then writes back.
        // flpdf lands on the same node, so /Pages stays 2 0 R.
        let catalog = pdf.resolve(ObjectRef::new(1, 0)).expect("catalog resolves");
        let Object::Dictionary(catalog_dict) = catalog else {
            panic!("catalog is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            catalog_dict.get("Pages"),
            Some(&Object::Reference(ObjectRef::new(2, 0))),
            "a /Parent cycle must leave /Pages at the node where the guard broke (2 0 R)"
        );
    }

    /// The catalog's `/Pages` (obj 2, a valid `/Kids`-bearing root) carries a
    /// DIRECT-dictionary `/Parent` rather than an indirect reference. Object
    /// layout: 1 Catalog (`/Pages 2 0 R`), 2 the root `/Pages` (`/Parent << >>`,
    /// `/Kids [3 0 R]`), 3 the `/Page` leaf (`/Parent 2 0 R`). The root->/Pages
    /// correction stops at the non-reference `/Parent` (flpdf needs a ref to
    /// continue; qpdf would follow a direct-dict parent as a handle), leaving
    /// `/Pages` unchanged.
    fn pdf_with_root_pages_non_reference_parent() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Parent << >> /Kids [3 0 R] /Count 1 >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn root_pages_non_reference_parent_stops_correction() {
        let bytes = pdf_with_root_pages_non_reference_parent();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");
        assert!(
            pdf.repair_diagnostics().entries().is_empty(),
            "fixture must parse without xref reconstruction (authored as a \
             clean-parse repair case): {:?}",
            pdf.repair_diagnostics().entries(), // cov:ignore: message arg formatted only on assertion failure (fixture never reconstructs)
        );

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        // A non-reference /Parent is a stop condition for the /Parent walk: the
        // catalog's /Pages must be left pointing at the same object (2 0 R), not
        // rewritten. This pins the non-reference-/Parent break arm.
        let catalog = pdf.resolve(ObjectRef::new(1, 0)).expect("catalog resolves");
        let Object::Dictionary(catalog_dict) = catalog else {
            panic!("catalog is not a dictionary"); // cov:ignore: unreachable — fixture always resolves to the expected type
        };
        assert_eq!(
            catalog_dict.get("Pages"),
            Some(&Object::Reference(ObjectRef::new(2, 0))),
            "a non-reference /Parent must stop the walk, leaving /Pages unchanged"
        );
    }
}
