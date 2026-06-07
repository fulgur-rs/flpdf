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
//! # Known limitation
//!
//! Annotations on the extracted page whose explicit `/Dest` targets another
//! (now-absent) page currently leak a stub of that sibling page and its
//! ancestor `/Pages` node into the output; explicit cross-page destinations are
//! not yet pruned or neutralized. The extracted page's
//! own content and resources are unaffected.

use crate::object_copy::{copy_objects, rewrite_refs};
use crate::page_closure::page_object_closure;
use crate::page_rotate::resolve_inherited_rotate_with_max_depth;
use crate::page_tree_rebuild::resolve_inherited_raw;
use crate::pages::{
    page_refs, resolve_inherited_resources_with_max_depth, DEFAULT_MAX_PAGE_TREE_DEPTH,
};
use crate::subset_prune::sweep_unreachable_objects;
use crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result};
use std::io::{Cursor, Read, Seek};

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

    // Drop the copied ancestor /Pages node(s) and any objects only they
    // referenced: they are unreachable from the new catalog now that the leaf
    // /Parent points at the fresh root. full_rewrite does NOT garbage-collect
    // (it emits every non-deleted object), so prune here to satisfy
    // "no unrelated objects". Same mark-and-sweep used after page-subset
    // rebuild (subset_prune::sweep_unreachable_objects).
    sweep_unreachable_objects(&mut target)?;

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
