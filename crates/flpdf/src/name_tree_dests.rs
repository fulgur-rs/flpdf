//! qpdf correspondence: QPDFNameTreeObjectHelper.cc destination-tree access split from the generic tree module.
//! Read/write access to the `/Names /Dests` name-tree (PDF 1.2+ named
//! destinations; ISO 32000-2 §7.9.6 + §12.3.2.3).
//!
//! This is the *modern* named-destination structure, added in PDF 1.2 to
//! supersede (but not replace — both may coexist) the legacy `/Catalog
//! /Dests` dictionary. Both stores remain raw catalog data during ordinary
//! reading and rewriting.
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
//! arbitrary [`Object`] value rather than requiring an [`crate::ObjectRef`].
//!
//! # Reader
//!
//! This module exposes only the *raw* (verbatim-value) collector used by the
//! writer. Callers that inspect the tree read its catalog objects through
//! [`crate::Pdf::resolve`]; flpdf does not normalize the store into a typed
//! destination enumeration.
//!
//! # Writer
//!
//! [`insert_name_tree_dest`] and [`delete_name_tree_dest`] mutate the existing
//! tree through [`crate::NameTree`]. Unaffected nodes and the existing root
//! reference are retained; splits and `/Limits` repairs follow qpdf's NNTree
//! behavior. A duplicate key on insert replaces the existing value, matching
//! the `/EmbeddedFiles` writer's convention.
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
/// path does not yet exist it is created. The existing tree is mutated in
/// place; an existing root reference is retained.
///
/// # Errors
///
/// Propagates any error from [`Pdf::resolve`].
pub fn insert_name_tree_dest<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    key: &[u8],
    value: Object,
) -> Result<()> {
    let Some(catalog_ref) = pdf.root_ref() else {
        return Ok(());
    };
    let Some(mut catalog) = pdf.resolve_borrowed(catalog_ref)?.as_dict().cloned() else {
        return Ok(());
    };

    enum NamesLocation {
        Direct,
        Indirect(crate::ObjectRef),
        Missing,
    }

    let (location, mut names) = match catalog.get("Names").cloned() {
        Some(source @ Object::Reference(source_ref)) => {
            let (terminal, terminal_ref) = resolve_ref_chain(pdf, &source)?;
            match terminal.into_dict() {
                Some(dictionary) => (
                    NamesLocation::Indirect(terminal_ref.unwrap_or(source_ref)),
                    dictionary,
                ),
                None => (NamesLocation::Missing, Dictionary::new()),
            }
        }
        Some(Object::Dictionary(dictionary)) => (NamesLocation::Direct, dictionary),
        _ => (NamesLocation::Missing, Dictionary::new()),
    };

    let mut tree = match names.get("Dests").cloned() {
        Some(root) => crate::NameTree::new(root, true),
        None => crate::NameTree::new_empty(pdf, true)?,
    };
    tree.set_max_depth(DEFAULT_MAX_NAME_TREE_DESTS_DEPTH);
    tree.insert(pdf, key, value)?;
    tree.make_root_indirect(pdf)?;
    names.insert("Dests", tree.into_root());

    match location {
        NamesLocation::Indirect(names_ref) => {
            pdf.set_object(names_ref, Object::Dictionary(names));
            catalog.insert("Names", Object::Reference(names_ref));
        }
        NamesLocation::Direct | NamesLocation::Missing => {
            let names_ref = pdf.next_available_object_ref()?;
            pdf.set_object(names_ref, Object::Dictionary(names));
            catalog.insert("Names", Object::Reference(names_ref));
        }
    }
    pdf.set_object(catalog_ref, Object::Dictionary(catalog));
    Ok(())
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
    let Some(catalog_ref) = pdf.root_ref() else {
        return Ok(false);
    };
    let Some(mut catalog) = pdf.resolve_borrowed(catalog_ref)?.as_dict().cloned() else {
        return Ok(false);
    };

    enum NamesLocation {
        Direct,
        Indirect(crate::ObjectRef),
    }

    let (location, mut names) = match catalog.get("Names").cloned() {
        Some(source @ Object::Reference(source_ref)) => {
            let (terminal, terminal_ref) = resolve_ref_chain(pdf, &source)?;
            let Some(dictionary) = terminal.into_dict() else {
                return Ok(false);
            };
            (
                NamesLocation::Indirect(terminal_ref.unwrap_or(source_ref)),
                dictionary,
            )
        }
        Some(Object::Dictionary(dictionary)) => (NamesLocation::Direct, dictionary),
        _ => return Ok(false),
    };
    let Some(root) = names.get("Dests").cloned() else {
        return Ok(false);
    };

    let mut tree = crate::NameTree::new(root, true);
    tree.set_max_depth(DEFAULT_MAX_NAME_TREE_DESTS_DEPTH);
    if tree.remove(pdf, key)?.is_none() {
        return Ok(false);
    }

    if tree.begin(pdf)?.valid() {
        tree.make_root_indirect(pdf)?;
        names.insert("Dests", tree.into_root());
    } else {
        names.remove("Dests");
    }

    if names.iter().next().is_none() {
        catalog.remove("Names");
        if let NamesLocation::Indirect(names_ref) = location {
            pdf.delete_object(names_ref);
        }
    } else {
        match location {
            NamesLocation::Direct => {
                catalog.insert("Names", Object::Dictionary(names));
            }
            NamesLocation::Indirect(names_ref) => {
                pdf.set_object(names_ref, Object::Dictionary(names));
                catalog.insert("Names", Object::Reference(names_ref));
            }
        }
    }
    pdf.set_object(catalog_ref, Object::Dictionary(catalog));
    Ok(true)
}
