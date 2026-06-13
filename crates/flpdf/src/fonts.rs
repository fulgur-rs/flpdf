//! Font-resource collection helpers.
//!
//! Aggregates the fonts referenced from every `Page` node's `/Resources /Font`
//! dictionary into a single `BTreeMap` keyed by resource name. Uses the same recursion
//! limit and cycle protection as [`crate::pages`].

use crate::ref_chain::resolve_ref_chain;
use crate::{Error, Object, ObjectRef, Pdf, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

/// Default recursion limit when walking the page tree to collect fonts.
pub const DEFAULT_MAX_PAGE_TREE_DEPTH: usize = 100;

/// Collect every font referenced by any page's `/Resources /Font` dictionary.
///
/// Fonts may be referenced indirectly (`/F1 7 0 R`) or inlined as a dictionary or
/// stream; in every case the returned [`Object`] is normalized to either the resolved
/// font dictionary or, for streams, the stream's font dictionary. Names that appear in
/// multiple pages are deduplicated and the latest definition wins, matching qpdf's
/// `--show-fonts` semantics.
///
/// # Errors
///
/// - [`Error::Missing`] when `/Root` or `/Pages` is absent.
/// - [`Error::Unsupported`] when the document catalog is not a dictionary, or when the
///   page tree exceeds the depth limit.
/// - Any [`Error`] propagated from [`Pdf::resolve_borrowed`] while resolving the catalog
///   or page-tree nodes.
pub fn font_entries<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<BTreeMap<Vec<u8>, Object>> {
    font_entries_with_max_depth(pdf, DEFAULT_MAX_PAGE_TREE_DEPTH)
}

/// Like [`font_entries`] but with a caller-supplied recursion limit.
///
/// # Errors
///
/// - [`Error::Missing`] when `/Root` or `/Pages` is absent.
/// - [`Error::Unsupported`] when the document catalog is not a dictionary, or when the
///   page tree exceeds `max_depth`.
/// - Any [`Error`] propagated from [`Pdf::resolve_borrowed`] while resolving the catalog
///   or page-tree nodes.
pub fn font_entries_with_max_depth<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    max_depth: usize,
) -> Result<BTreeMap<Vec<u8>, Object>> {
    let catalog_ref = pdf.root_ref().ok_or(Error::Missing("/Root"))?;
    let catalog = pdf.resolve_borrowed(catalog_ref)?;
    let Object::Dictionary(catalog) = catalog else {
        return Err(Error::Unsupported(format!(
            "document catalog {catalog_ref} is not a dictionary"
        )));
    };
    let pages_ref = catalog.get_ref("Pages").ok_or(Error::Missing("/Pages"))?;

    let mut seen = BTreeSet::new();
    let mut fonts = BTreeMap::new();
    walk_font_resources(pdf, pages_ref, &mut seen, &mut fonts, 0, max_depth)?;
    Ok(fonts)
}

fn walk_font_resources<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    node: ObjectRef,
    seen: &mut BTreeSet<ObjectRef>,
    fonts: &mut BTreeMap<Vec<u8>, Object>,
    depth: usize,
    max_depth: usize,
) -> Result<()> {
    if depth >= max_depth {
        return Err(Error::Unsupported(format!(
            "page tree depth exceeds maximum of {max_depth} at {node}"
        )));
    }

    if !seen.insert(node) {
        return Ok(());
    }

    let node_obj = pdf.resolve_borrowed(node)?;
    let Object::Dictionary(dict) = node_obj else {
        return Ok(());
    };

    let node_type = dict
        .get("Type")
        .and_then(Object::as_name)
        .map(ToOwned::to_owned)
        .unwrap_or_default();

    if node_type.as_slice() == b"Pages" {
        let kid_refs: Vec<ObjectRef> = dict
            .get("Kids")
            .and_then(Object::as_array)
            .map(|kids| kids.iter().filter_map(Object::as_ref_id).collect())
            .unwrap_or_default();
        for reference in kid_refs {
            walk_font_resources(pdf, reference, seen, fonts, depth + 1, max_depth)?;
        }
        return Ok(());
    }

    if node_type.as_slice() == b"Page" {
        let page_dict = dict.clone();
        collect_page_fonts(pdf, &page_dict, fonts)?;
    }

    Ok(())
}

fn collect_page_fonts<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page: &crate::Dictionary,
    fonts: &mut BTreeMap<Vec<u8>, Object>,
) -> Result<()> {
    let resources = match page.get("Resources") {
        Some(resources) => {
            if let Some(resources) = resources.as_dict() {
                Some(resources.clone())
            } else if let Some(reference) = resources.as_ref_id() {
                // /Resources may be reached through more than one indirect hop
                // (ref -> ref -> dict); follow the chain to its terminal.
                let (terminal, _) = resolve_ref_chain(pdf, &Object::Reference(reference))?;
                terminal.into_dict()
            } else {
                None
            }
        }
        _ => None,
    };

    let Some(resources) = resources else {
        return Ok(());
    };

    let fonts_dict = match resources.get("Font") {
        Some(fonts_dict) => {
            if let Some(fonts_dict) = fonts_dict.as_dict() {
                Some(fonts_dict.clone())
            } else if let Some(reference) = fonts_dict.as_ref_id() {
                // The /Font value may be reached through more than one indirect
                // hop (ref -> ref -> dict); follow the chain to its terminal.
                let (terminal, _) = resolve_ref_chain(pdf, &Object::Reference(reference))?;
                terminal.into_dict()
            } else {
                None
            }
        }
        _ => None,
    };

    let Some(fonts_dict) = fonts_dict else {
        return Ok(());
    };

    for (font_name, value) in fonts_dict.iter() {
        // A font value may be inlined as a dictionary, embedded as a stream, or
        // (most commonly) stored indirectly — possibly through more than one
        // indirect hop (ref -> ref -> dict), so follow the chain to its
        // terminal. Then normalize to the font dictionary: dictionaries are kept
        // as-is and streams contribute their dictionary. PDF streams are always
        // indirect objects, so a stream-valued font is only ever seen through
        // the reference arm; the direct Object::Stream arm below mirrors it for
        // completeness. Anything that is not a font dictionary is skipped.
        //
        // The reference arm yields an owned terminal, so the Dictionary/Stream
        // dictionary is moved out below rather than cloned again. Inline dict
        // and stream values are cloned once from `fonts_dict` (it is borrowed
        // here); the skip path borrows and never clones.
        //
        // Resolution errors propagate via `?`, matching how /Resources and
        // /Font are resolved above; a missing or deleted reference is not an
        // error (it resolves to `Object::Null`) and is skipped by the match
        // below.
        let resolved: Object = match value {
            Object::Reference(font_ref) => resolve_ref_chain(pdf, &Object::Reference(*font_ref))?.0,
            Object::Dictionary(_) | Object::Stream(_) => value.clone(),
            _ => continue,
        };
        match resolved {
            Object::Dictionary(font_dict) => {
                fonts.insert(font_name.to_vec(), Object::Dictionary(font_dict));
            }
            Object::Stream(stream) => {
                fonts.insert(font_name.to_vec(), Object::Dictionary(stream.dict));
            }
            _ => {}
        }
    }

    Ok(())
}
