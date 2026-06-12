//! Multi-document page merge (qpdf `--pages` parity).
//!
//! [`merge_documents`] copies selected pages from N source documents into one
//! fresh target. `inputs[0]` is the primary: its document-level information
//! (outlines, named destinations, AcroForm `/DR` `/DA`) is inherited; later
//! inputs contribute pages and form fields only. Shared resources within one
//! input are de-duplicated; form-field name collisions are resolved by qpdf's
//! `<name>+<N>` renaming rule.

use crate::object_copy::copy_objects;
use crate::page_closure::page_object_closure;
use crate::page_extract::{
    append_selection_kids, materialize_leaf, minimal_target_bytes, resolve_dict, target_pages_root,
    InheritedAttrs,
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
        let map = copy_objects(input.source, &mut target, &closure)?;

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
