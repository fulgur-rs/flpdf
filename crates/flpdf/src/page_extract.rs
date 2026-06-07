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
//! Part of the page extraction & merge primitives epic (flpdf-5h5). Composes
//! [`page_object_closure`](crate::page_closure::page_object_closure) and
//! [`copy_objects`](crate::object_copy::copy_objects).

use crate::object_copy::copy_objects;
use crate::page_closure::page_object_closure;
use crate::page_rotate::resolve_inherited_rotate_with_max_depth;
use crate::page_tree_rebuild::resolve_inherited_raw;
use crate::pages::{
    page_refs, resolve_inherited_resources_with_max_depth, DEFAULT_MAX_PAGE_TREE_DEPTH,
};
use crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result};
use std::collections::BTreeMap;
use std::io::{Cursor, Read, Seek};

/// Extract page `page_index` (0-based) from `source` into a brand-new minimal
/// document.
///
/// Returns an owned in-memory [`Pdf`] whose catalog has a single-level
/// `/Pages` tree with one `/Kid`. Write it with
/// [`write_pdf_with_options`](crate::write_pdf_with_options) and
/// `WriteOptions { full_rewrite: true, .. }` so the (unreferenced) copied
/// ancestor `/Pages` nodes are dropped.
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
    let mut leaf = target
        .resolve_borrowed(copied_page_ref)?
        .as_dict()
        .cloned()
        .ok_or(Error::Missing("copied page is not a dictionary"))?;

    if !has_own(&leaf, "Resources") {
        if let Some(res) = inherited_resources {
            let mut value = Object::Dictionary(res);
            remap_refs(&mut value, &map);
            leaf.insert("Resources", value);
        }
    }
    if !has_own(&leaf, "MediaBox") {
        if let Some(mut mb) = inherited_mediabox {
            remap_refs(&mut mb, &map);
            leaf.insert("MediaBox", mb);
        }
    }
    if !has_own(&leaf, "CropBox") {
        if let Some(mut cb) = inherited_cropbox {
            remap_refs(&mut cb, &map);
            leaf.insert("CropBox", cb);
        }
    }
    if !has_own(&leaf, "Rotate") {
        leaf.insert("Rotate", Object::Integer(inherited_rotate as i64));
    }
    leaf.insert("Parent", Object::Reference(pages_root_ref));
    target.set_object(copied_page_ref, Object::Dictionary(leaf));

    // Build the fresh single-level /Pages root.
    let mut root = target
        .resolve_borrowed(pages_root_ref)?
        .as_dict()
        .cloned()
        .ok_or(Error::Missing("target /Pages is not a dictionary"))?;
    root.insert("Kids", Object::Array(vec![Object::Reference(copied_page_ref)]));
    root.insert("Count", Object::Integer(1));
    target.set_object(pages_root_ref, Object::Dictionary(root));

    Ok(target)
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
    let catalog = target
        .resolve_borrowed(catalog_ref)?
        .as_dict()
        .cloned()
        .ok_or(Error::Missing("/Root is not a dictionary"))?;
    catalog
        .get("Pages")
        .and_then(|o| match o {
            Object::Reference(r) => Some(*r),
            _ => None,
        })
        .ok_or(Error::Missing("/Pages"))
}

/// `true` when `dict` carries `key` as something other than `null`
/// (ISO 32000-1 §7.3.9: explicit `null` == absent). Mirrors
/// `page_tree_rebuild::leaf_has_own`.
fn has_own(dict: &Dictionary, key: &str) -> bool {
    !matches!(dict.get(key), None | Some(Object::Null))
}

/// Rewrite every indirect reference inside `obj` through `map`. Refs not present
/// in `map` (out-of-closure) become `Object::Null`, matching `copy_objects`'
/// out-of-set policy. Used to fix up materialized inherited attribute values,
/// whose refs point into the SOURCE document.
fn remap_refs(obj: &mut Object, map: &BTreeMap<ObjectRef, ObjectRef>) {
    match obj {
        Object::Reference(r) => {
            *obj = match map.get(r) {
                Some(target) => Object::Reference(*target),
                None => Object::Null,
            };
        }
        Object::Array(items) => {
            for item in items.iter_mut() {
                remap_refs(item, map);
            }
        }
        Object::Dictionary(dict) => {
            for value in dict.values_mut() {
                remap_refs(value, map);
            }
        }
        Object::Stream(stream) => {
            for value in stream.dict.values_mut() {
                remap_refs(value, map);
            }
        }
        Object::Null
        | Object::Boolean(_)
        | Object::Integer(_)
        | Object::Real(_)
        | Object::Name(_)
        | Object::String(_) => {}
    }
}
