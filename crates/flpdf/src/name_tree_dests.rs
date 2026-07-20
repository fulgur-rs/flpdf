//! Read/write access to the `/Names /Dests` name-tree (PDF 1.2+ named
//! destinations; ISO 32000-2 §7.9.6 + §12.3.2.3).
//!
//! This is the *modern* named-destination structure, added in PDF 1.2 to
//! supersede (but not replace — both may coexist) the legacy `/Catalog
//! /Dests` dictionary handled by
//! [`crate::OutlineDocumentHelper::legacy_dests`].
//!
//! # Structure
//!
//! Same shape as `/Names /EmbeddedFiles` (see [`crate::embedded_files`]):
//! `/Kids`/`/Names` nodes carrying `/Limits`, depth-first key-ascending
//! order. The two trees differ only in where they hang off `/Names` and in
//! the shape of a leaf value: an `/EmbeddedFiles` value must be an indirect
//! reference to a `/Filespec` dictionary, whereas a `/Dests` value is
//! commonly an inline destination array (`[page /Fit ...]`) or a `<< /D
//! array >>` dictionary, though an indirect reference to either is also
//! valid (ISO 32000-2 §12.3.2.3). Consequently the writer here accepts an
//! arbitrary [`Object`] value rather than requiring an [`ObjectRef`].
//!
//! # Reader
//!
//! This module exposes only the *raw* (verbatim-value) collector used by the
//! writer. For a reader that resolves each value to an explicit destination,
//! see [`crate::OutlineDocumentHelper::name_tree_dests`].
//!
//! # Writer
//!
//! [`insert_name_tree_dest`] and [`delete_name_tree_dest`] mutate the tree
//! using the same collect → modify → rebuild strategy as
//! [`crate::embedded_files::insert_embedded_file`] /
//! [`crate::embedded_files::delete_embedded_file`]: all entries are gathered,
//! the entry list is changed and re-sorted, and the entire tree is
//! reconstructed via [`crate::name_number_tree::build_name_tree`] (at most
//! two levels: a single leaf when the entry count is within
//! [`crate::name_number_tree::LEAF_MAX`], otherwise a `/Kids` root over leaf
//! chunks). A duplicate key on insert replaces the existing value rather
//! than being rejected, matching the `/EmbeddedFiles` writer's convention.
//!
//! Other keys in the `/Names` dictionary (e.g. `/EmbeddedFiles`,
//! `/JavaScript`) are preserved unchanged. When deletion empties the entry
//! list, `/Dests` is removed from `/Names`; if that leaves `/Names` empty,
//! `/Names` is removed from the catalog too.

use crate::ref_chain::resolve_ref_chain;
use crate::{Dictionary, Object, Pdf, Result};
use std::io::{Read, Seek};

/// Default maximum depth when descending `/Kids` chains. Mirrors
/// [`crate::embedded_files::DEFAULT_MAX_EMBEDDED_FILES_DEPTH`].
pub const DEFAULT_MAX_NAME_TREE_DESTS_DEPTH: usize = 100;

/// Enumerate `(name_key, value)` entries in the catalog's `/Names /Dests`
/// name tree, preserving each value **verbatim** as an [`Object`] (inline
/// destination array, inline `<< /D array >>` dict, or indirect reference to
/// either).
///
/// This is the writer's source of truth: rebuilding from a resolved-only
/// view would silently drop entries whose value cannot be resolved to an
/// explicit destination.
///
/// # Errors
///
/// Propagates any error from [`Pdf::resolve_borrowed`], and returns
/// [`crate::Error::Unsupported`] if a `/Kids` chain exceeds `max_depth`
/// (cyclic or maliciously deep tree).
pub(crate) fn collect_name_tree_dests_raw<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    max_depth: usize,
) -> Result<Vec<(Vec<u8>, Object)>> {
    let catalog_ref = match pdf.root_ref() {
        Some(r) => r,
        None => return Ok(vec![]),
    };
    let Some(catalog) = pdf.resolve_borrowed(catalog_ref)?.as_dict() else {
        return Ok(vec![]);
    };

    let names_dict = match catalog.get("Names").cloned() {
        // /Names may be reached through more than one indirect hop
        // (ref -> ref -> dict); follow the chain to its terminal.
        Some(Object::Reference(r)) => match resolve_ref_chain(pdf, &Object::Reference(r))?.0 {
            Object::Dictionary(d) => d,
            _ => return Ok(vec![]),
        },
        Some(Object::Dictionary(d)) => d,
        _ => return Ok(vec![]),
    };

    let dests_value = match names_dict.get("Dests").cloned() {
        Some(v) => v,
        None => return Ok(vec![]),
    };
    crate::name_number_tree::read_name_tree(pdf, dests_value, |_, v| Ok(Some(v)), max_depth)
}

/// Insert or replace a `(key, value)` entry in the catalog's `/Names /Dests`
/// name tree.
///
/// `value` is stored verbatim: pass an inline destination array (`[page
/// /Fit ...]`), an inline `<< /D array >>` dictionary, or an
/// [`Object::Reference`] to either — the tree does not require values to be
/// indirect (unlike `/Names /EmbeddedFiles`, whose values must reference a
/// `/Filespec`).
///
/// If `key` already exists its value is replaced. If the `/Names /Dests`
/// path does not yet exist it is created. The entire tree is rebuilt from
/// scratch after the insertion (qpdf-style aggressive rebuild): all existing
/// entries are read, the new entry is merged in sorted order, and a fresh
/// tree is written back via [`Pdf::set_object`].
///
/// # Errors
///
/// Propagates any error from [`Pdf::resolve`].
pub fn insert_name_tree_dest<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    key: &[u8],
    value: Object,
) -> Result<()> {
    let mut entries = collect_name_tree_dests_raw(pdf, DEFAULT_MAX_NAME_TREE_DESTS_DEPTH)?;

    if let Some(existing) = entries.iter_mut().find(|(k, _)| k.as_slice() == key) {
        existing.1 = value;
    } else {
        entries.push((key.to_vec(), value));
        entries.sort_by(|(a, _), (b, _)| a.cmp(b));
    }

    rebuild_name_tree_dests(pdf, entries)
}

/// Remove the entry with `key` from the catalog's `/Names /Dests` name tree.
///
/// Returns `true` if the key was found and removed, `false` if it was
/// absent.
///
/// When the last entry is removed the `/Dests` key is deleted from the
/// `/Names` dictionary. If that leaves the `/Names` dictionary empty, the
/// `/Names` key is removed from the catalog as well — no dangling
/// references remain.
///
/// # Errors
///
/// Propagates any error from [`Pdf::resolve`].
pub fn delete_name_tree_dest<R: Read + Seek>(pdf: &mut Pdf<R>, key: &[u8]) -> Result<bool> {
    let mut entries = collect_name_tree_dests_raw(pdf, DEFAULT_MAX_NAME_TREE_DESTS_DEPTH)?;
    let before = entries.len();
    entries.retain(|(k, _)| k != key);
    if entries.len() == before {
        return Ok(false); // Key was not present.
    }

    rebuild_name_tree_dests(pdf, entries)?;
    Ok(true)
}

/// Rebuild the `/Names /Dests` name tree from a sorted entry list and patch
/// it back into the document via [`Pdf::set_object`].
///
/// Mirrors [`crate::embedded_files`]'s internal rebuild exactly (same
/// catalog-wiring, holder-chain-collapse, and empty-cleanup rules), keyed at
/// `/Dests` instead of `/EmbeddedFiles`.
fn rebuild_name_tree_dests<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    entries: Vec<(Vec<u8>, Object)>,
) -> Result<()> {
    // Resolve the catalog.
    let catalog_ref = match pdf.root_ref() {
        Some(r) => r,
        None => return Ok(()),
    };
    let Some(mut catalog) = pdf.resolve_borrowed(catalog_ref)?.as_dict().cloned() else {
        return Ok(());
    };

    // ── Allocate a block of fresh object numbers ──────────────────────────────
    let mut next_num: u32 = pdf
        .object_refs()
        .iter()
        .map(|r| r.number)
        .max()
        .unwrap_or(0);
    let mut alloc = move || -> crate::ObjectRef {
        next_num += 1;
        crate::ObjectRef::new(next_num, 0)
    };

    // ── Empty case: remove /Dests ─────────────────────────────────────────────
    if entries.is_empty() {
        let names_dict_opt = match catalog.get("Names") {
            // /Names may be reached through more than one indirect hop
            // (ref -> ref -> dict); follow the chain so the terminal dict — the
            // object actually rewritten below — is the one updated, not an
            // intermediate carrier.
            Some(value @ Object::Reference(r)) => {
                let (terminal, terminal_ref) = resolve_ref_chain(pdf, value)?;
                terminal
                    .into_dict()
                    .map(|d| (Some(terminal_ref.unwrap_or(*r)), d))
            }
            Some(Object::Dictionary(d)) => Some((None, d.clone())),
            // `entries` can only be empty here when `delete_name_tree_dest`
            // actually found and removed an entry, which requires
            // `collect_name_tree_dests_raw` to have matched this same
            // (unmutated) `/Names` value as either `Reference` or
            // `Dictionary` moments earlier in the same call — this wildcard
            // can only fire for some other type or absence.
            _ => None, // cov:ignore: unreachable via the public writer API (see comment above)
        };
        // `names_dict_opt` is always `Some` in practice for the same reason
        // the wildcard above is unreachable; the `if let` is retained (rather
        // than an `unwrap`) purely for defensive symmetry with the read path.
        if let Some((names_ref_opt, mut names_dict)) = names_dict_opt {
            names_dict.remove("Dests");
            if names_dict.iter().next().is_none() {
                // /Names dict is now empty — remove from catalog.
                catalog.remove("Names");
                pdf.set_object(catalog_ref, Object::Dictionary(catalog));
                if let Some(r) = names_ref_opt {
                    pdf.delete_object(r);
                }
            } else {
                match names_ref_opt {
                    Some(r) => {
                        pdf.set_object(r, Object::Dictionary(names_dict));
                        // Collapse any holder chain: re-point catalog /Names
                        // straight at the terminal dict, mirroring the
                        // non-empty rebuild path.
                        catalog.insert("Names", Object::Reference(r));
                        pdf.set_object(catalog_ref, Object::Dictionary(catalog));
                    }
                    None => {
                        catalog.insert("Names", Object::Dictionary(names_dict));
                        pdf.set_object(catalog_ref, Object::Dictionary(catalog));
                    }
                }
            }
        } // cov:ignore: the `None` (skip) arm of this `if let` is unreachable; see above
        return Ok(());
    }

    // ── Build the name-tree nodes (shared builder) ────────────────────────────
    let (tree_root_ref, nodes) = crate::name_number_tree::build_name_tree(&entries, &mut alloc);
    for (node_ref, node) in nodes {
        pdf.set_object(node_ref, node);
    }

    // ── Patch the catalog /Names dictionary ───────────────────────────────────
    let (names_ref, mut names_dict) = match catalog.get("Names") {
        // /Names may be reached through more than one indirect hop
        // (ref -> ref -> dict); follow the chain so /Dests is written into
        // the terminal dict and the catalog /Names rewrite below collapses
        // the chain to point straight at it.
        Some(value @ Object::Reference(r)) => {
            let (terminal, terminal_ref) = resolve_ref_chain(pdf, value)?;
            match terminal.into_dict() {
                Some(d) => (terminal_ref.unwrap_or(*r), d),
                None => {
                    let r2 = alloc();
                    (r2, Dictionary::new())
                }
            }
        }
        Some(Object::Dictionary(d)) => {
            let r = alloc();
            (r, d.clone())
        }
        _ => {
            let r = alloc();
            (r, Dictionary::new())
        }
    };

    names_dict.insert("Dests", Object::Reference(tree_root_ref));
    pdf.set_object(names_ref, Object::Dictionary(names_dict));

    // Point catalog /Names to the (possibly new) indirect names dict.
    catalog.insert("Names", Object::Reference(names_ref));
    pdf.set_object(catalog_ref, Object::Dictionary(catalog));

    Ok(())
}
