//! Font-resource collection helpers.
//!
//! Aggregates the fonts referenced from every `Page` node's `/Resources /Font`
//! dictionary into a single `BTreeMap` keyed by resource name. Uses the same recursion
//! limit and cycle protection as [`crate::pages`].

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
/// Returns [`Error::Missing`] if `/Root` or `/Pages` is absent. Returns
/// [`Error::Unsupported`] if the page tree exceeds the depth limit.
pub fn font_entries<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<BTreeMap<Vec<u8>, Object>> {
    font_entries_with_max_depth(pdf, DEFAULT_MAX_PAGE_TREE_DEPTH)
}

/// Like [`font_entries`] but with a caller-supplied recursion limit.
pub fn font_entries_with_max_depth<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    max_depth: usize,
) -> Result<BTreeMap<Vec<u8>, Object>> {
    let catalog_ref = pdf.root_ref().ok_or(Error::Missing("/Root"))?;
    let catalog = pdf.resolve(catalog_ref)?;
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

    let node_obj = pdf.resolve(node)?;
    let Object::Dictionary(dict) = node_obj else {
        return Ok(());
    };

    let node_type = dict
        .get("Type")
        .and_then(Object::as_name)
        .map(ToOwned::to_owned)
        .unwrap_or_default();

    if node_type.as_slice() == b"Pages" {
        if let Some(kids) = dict.get("Kids").and_then(Object::as_array) {
            for kid in kids {
                if let Some(reference) = kid.as_ref_id() {
                    walk_font_resources(pdf, reference, seen, fonts, depth + 1, max_depth)?;
                }
            }
        }
        return Ok(());
    }

    if node_type.as_slice() == b"Page" {
        collect_page_fonts(pdf, &dict, fonts)?;
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
                pdf.resolve(reference)?.into_dict()
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
                pdf.resolve(reference)?.into_dict()
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
        match value {
            Object::Reference(font_ref) => {
                if let Ok(font_obj) = pdf.resolve(*font_ref) {
                    fonts.insert(font_name.to_vec(), font_obj);
                }
            }
            Object::Dictionary(font_dict) => {
                fonts.insert(font_name.to_vec(), Object::Dictionary(font_dict.clone()));
            }
            Object::Stream(stream) => {
                fonts.insert(font_name.to_vec(), Object::Dictionary(stream.dict.clone()));
            }
            _ => {}
        }
    }

    Ok(())
}
