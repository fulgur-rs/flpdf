//! qpdf correspondence: QPDF::emptyPDF plus QPDFPageDocumentHelper.cc addPage, library level only.
//! Page extraction into a fresh minimal document.
//!
//! [`extract_pages`] builds a brand-new minimal [`Pdf`] (via [`Pdf::empty`])
//! containing the selected pages from `source` plus their reachable object
//! graph, copied across documents; [`extract_page`] is the single-page
//! convenience form. This mirrors the qpdf *library* pattern of
//! `QPDF::emptyPDF()` followed by `QPDFPageDocumentHelper::addPage()`: a
//! **new** document object is constructed and populated here, then written
//! by a separate [`crate::PdfWriter`]. It is
//! deliberately distinct from [`crate::PageDocumentHelper`], whose
//! `add_page` mutates an already-open document, and from [`crate::pages`],
//! which owns page-tree traversal.
//!
//! Neither `QPDF::emptyPDF()` nor `QPDFPageDocumentHelper::addPage()`
//! touches the document's PDF version, so the returned document keeps
//! `emptyPDF()`'s own header version (`"1.3"`) regardless of `source`'s
//! version. Propagating an input's version into the output, as the `qpdf`
//! CLI's `--pages` does, is `QPDFJob` orchestration — a distinct
//! responsibility this module does not implement.
//!
//! qpdf's foreign-page insertion first materializes inherited page attributes
//! on the source document. Extraction follows that responsibility boundary;
//! the source page membership and ordering remain unchanged, while its page
//! dictionaries may gain explicit `/Resources`, `/MediaBox`, `/CropBox`, and
//! `/Rotate` entries before the copy.
//!
//! Composes [`PageDocumentHelper::push_inherited_attributes_to_pages`] with the
//! canonical [`Pdf::copy_foreign_object`] route. The destination keeps qpdf's
//! per-source identity map alive across all selected pages, so objects shared
//! between them (fonts, images, content streams) are copied exactly once.
//!
//! # Page labels
//!
//! When `source` carries a `/PageLabels` number tree, the extracted document
//! gets its own reconstructed `/PageLabels` reflecting the selected pages'
//! renumbered positions (a page's label at its old position becomes its
//! label at its new, 0-based output position). No catalog-level navigation
//! structure is otherwise copied: named destinations (`/Names /Dests`, the
//! legacy `/Catalog /Dests` dictionary) and the outline tree are not part of
//! any page's object closure, so they are absent from the extracted
//! document — matching qpdf's `addPage`-based copy, which brings over only
//! each page's own reachable objects.
//!
//! # References to removed pages
//!
//! Carriers such as annotation destinations, action dictionaries, structure
//! destinations, and article-thread beads keep their copied page references.
//! A reference to an unselected source page reaches qpdf's `/Pages` boundary
//! during `copyForeignObject` and becomes a destination-owned `null`
//! placeholder, matching qpdf's page-selection behavior without interpreting
//! the carrier's semantics.

use crate::page_label_document_helper::merge_adjacent_ranges;
use crate::pages::page_refs;
use crate::subset_prune::sweep_unreachable_objects;
use crate::{Dictionary, Error, Object, ObjectHandle, ObjectRef, PageDocumentHelper, Pdf, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read, Seek};

/// Extract the pages at `page_indices` (0-based) from `source` into a
/// brand-new minimal document.
///
/// Returns an owned in-memory [`Pdf`] whose catalog has a single-level
/// `/Pages` tree with one `/Kids` entry per selected index, in **selection
/// order** (any order is accepted, matching qpdf's `--pages` selection
/// semantics). Selected pages are copied through one destination-side
/// per-source identity map, so objects referenced by several selected pages
/// (fonts, images, content streams) appear exactly once in the output.
///
/// An index may appear more than once. The second and later occurrences of a
/// page become shallow clones of its first copy: each duplicate gets its own
/// page object, while indirectly referenced sub-objects (`/Contents`,
/// `/Resources`, `/Annots`, `/B`) stay shared between the duplicates,
/// matching qpdf 11.9.0's observed duplicate-page output.
///
/// The returned document is already minimal: the canonical foreign copier's
/// `/Pages` boundary is attached to the fresh single-level root, and a
/// mark-and-sweep from the new catalog removes any other unreachable
/// construction objects before returning. Write it with [`crate::PdfWriter`],
/// which always emits a fresh qpdf-style document rewrite.
///
/// See also [`extract_page`] for the single-page form, and the [module
/// documentation](self) for qpdf's source-side inherited-attribute
/// materialization, how references to removed pages are handled, and how
/// `/PageLabels` is reconstructed for the selection.
///
/// # Examples
///
/// ```no_run
/// use std::fs::File;
/// use std::io::BufReader;
/// use flpdf::{extract_pages, Pdf, PdfWriter};
///
/// let file = BufReader::new(File::open("input.pdf")?);
/// let mut pdf = Pdf::open(file)?;
///
/// // First and third page (0-based), in selection order.
/// let mut extracted = extract_pages(&mut pdf, &[0, 2])?;
///
/// let mut writer = PdfWriter::new(&mut extracted);
/// writer.set_output_file("extracted.pdf")?;
/// writer.write()?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// # Errors
///
/// - [`Error::Unsupported`] if `page_indices` is empty or any index is out of
///   range.
/// - Propagates resolve/copy errors from the underlying primitives.
pub fn extract_pages<R: Read + Seek>(
    source: &mut Pdf<R>,
    page_indices: &[usize],
) -> Result<Pdf<Cursor<Vec<u8>>>> {
    if page_indices.is_empty() {
        return Err(Error::Unsupported("empty page selection".to_string()));
    }
    let all_pages = page_refs(source)?;
    let mut selected: Vec<ObjectRef> = Vec::with_capacity(page_indices.len());
    for &idx in page_indices {
        let page_ref = *all_pages.get(idx).ok_or_else(|| {
            Error::Unsupported(format!(
                "page index {idx} out of range (document has {} pages)",
                all_pages.len()
            ))
        })?;
        selected.push(page_ref);
    }

    // qpdf first flattens the destination page tree, then prepares inherited
    // attributes on the foreign source before calling copyForeignObject. Keep
    // those two responsibility boundaries separate: the source helper performs
    // only the qpdf-compatible inherited-attribute push, while the extraction
    // route below performs the canonical foreign copy and page-tree mutations.
    let mut target = Pdf::empty()?;
    let pages_root_ref = target_pages_root(&mut target)?;
    PageDocumentHelper::new(source).push_inherited_attributes_to_pages()?;

    // Copy each unique selected page through the destination's persistent
    // qpdf ObjCopier map. If a page was encountered earlier as a nested
    // `/Page` boundary, a later top-level copy fills that same reservation.
    let mut page_map = BTreeMap::new();
    for &source_page_ref in &selected {
        if page_map.contains_key(&source_page_ref) {
            continue;
        }
        let source_page = source.get_object_handle(source_page_ref);
        let copied_page = target.copy_foreign_object(&source_page)?;
        let copied_page_ref = copied_page
            .object_ref()
            .ok_or(Error::Missing("extracted page missing from copy map"))?;
        page_map.insert(source_page_ref, copied_page_ref);
    }

    // qpdf::insertPage replaces the copied page's `/Parent` through the live
    // destination handle, before it inserts that page into `/Kids`.
    let pages_handle = target.get_object_handle(pages_root_ref);
    for &copied_page_ref in page_map.values() {
        let page = target.get_object_handle(copied_page_ref);
        target.resolve_object_handle(&page)?;
        page.replace_key(b"/Parent", pages_handle.clone())?;
        target.mark_object_handle_dirty(&page)?;
    }

    // Build `/Kids` in selection order. Repeated selections reuse the copied
    // page identity as the first occurrence and then shallow-copy its page
    // dictionary into a fresh indirect object, preserving shared child
    // identities exactly as QPDF_pages.cc:233-237 does.
    let mut kids = Vec::with_capacity(selected.len());
    let mut used = BTreeSet::new();
    for &source_page_ref in &selected {
        let copied_page_ref = *page_map
            .get(&source_page_ref)
            .ok_or(Error::Missing("extracted page missing from copy map"))?;
        let kid = if used.insert(copied_page_ref) {
            copied_page_ref
        } else {
            let page = target.get_object_handle(copied_page_ref);
            let clone = target.make_indirect_object_handle(page.shallow_copy()?)?;
            clone.object_ref().ok_or(Error::Missing(
                "duplicate extracted page missing from target",
            ))?
        };
        kids.push(kid);
    }

    let root = target.get_object_handle(pages_root_ref);
    root.replace_key(
        b"/Kids",
        ObjectHandle::array(
            kids.iter()
                .map(|&kid| target.get_object_handle(kid))
                .collect(),
        ),
    )?; // cov:ignore: Pdf::empty creates a dictionary /Pages root, so this defensive replace_key error is unreachable
    root.replace_key(b"/Count", ObjectHandle::integer(kids.len() as i64))?;
    target.mark_object_handle_dirty(&root)?;

    // /PageLabels (qpdf `addPage`-based reconstruction parity — the same
    // per-page accumulation `QPDFJob::handlePageSpecs` performs while adding
    // pages, generalized here to arbitrary/duplicate selection order). A
    // source with no `/PageLabels` at all leaves the fresh target untouched
    // (it never gains one), matching qpdf's `emptyPDF()`-based output.
    {
        let mut source_labels = source.page_labels();
        if source_labels.has_page_labels()? {
            let src_indices: Vec<i64> = page_indices.iter().map(|&i| i as i64).collect();
            let entries = source_labels.labels_for_selection(&src_indices, 0)?;
            let folded = merge_adjacent_ranges(entries);
            target.page_labels().write_reconstructed_labels(&folded)?;
        }
    }

    // The canonical handle mutations above built the fresh single-level
    // /Pages root. Drop any objects that were made unreachable by that rebuild
    // before handing the graph to the canonical writer.
    sweep_unreachable_objects(&mut target)?;

    Ok(target)
}

/// Replace every copied but unselected source page with `null`.
///
/// Page identity comes from the source page tree (`all_pages`), not from the
/// semantics or `/Type` of the object carrying the reference. Pages absent
/// from `map` were never copied and therefore require no placeholder object.
pub(crate) fn null_copied_removed_pages<R: Read + Seek>(
    target: &mut Pdf<R>,
    all_pages: &[ObjectRef],
    selected: &BTreeSet<ObjectRef>,
    map: &BTreeMap<ObjectRef, ObjectRef>,
) {
    for source_page in all_pages {
        if !selected.contains(source_page) {
            if let Some(&copied_page) = map.get(source_page) {
                target.set_object(copied_page, Object::Null);
            }
        }
    }
}

/// Extract page `page_index` (0-based) from `source` into a brand-new minimal
/// document.
///
/// Single-page convenience form of [`extract_pages`]: the returned document's
/// catalog has a single-level `/Pages` tree with a single entry in `/Kids`.
/// qpdf may materialize inherited page attributes on `source` while preparing
/// the foreign copy; page membership and ordering remain unchanged.
///
/// # Errors
///
/// - [`Error::Unsupported`] if `page_index` is out of range.
/// - Propagates resolve/copy errors from the underlying primitives.
pub fn extract_page<R: Read + Seek>(
    source: &mut Pdf<R>,
    page_index: usize,
) -> Result<Pdf<Cursor<Vec<u8>>>> {
    extract_pages(source, &[page_index])
}

/// Append `/Kids` entries to `kids` for `selected` (in selection order),
/// shallow-cloning any source page selected more than once.
///
/// The first occurrence of a source page uses its mapped copy from `map`;
/// later occurrences become a fresh page object whose indirectly referenced
/// sub-objects (`/Contents`, `/Resources`, `/Annots`, `/B`) stay shared with
/// the first copy, matching qpdf's observed duplicate-page output. `used`
/// tracks which copied page objects already appear in `kids`, so this may be
/// called once per input (with `used`/`kids` accumulating across calls) by
/// [`crate::page_merge::merge_documents`], its sole caller.
///
/// New object numbers for clones are allocated above the current maximum in
/// `target`, recomputed on entry so repeated calls into a growing target do
/// not collide.
pub(crate) fn append_selection_kids(
    target: &mut Pdf<Cursor<Vec<u8>>>,
    selected: &[ObjectRef],
    map: &std::collections::BTreeMap<ObjectRef, ObjectRef>,
    used: &mut BTreeSet<ObjectRef>,
    kids: &mut Vec<ObjectRef>,
) -> Result<()> {
    let mut next_num: u32 = target
        .object_refs()
        .iter()
        .map(|r| r.number)
        .max()
        .unwrap_or(0);
    for &src_ref in selected {
        let copied_page_ref = *map
            .get(&src_ref)
            .ok_or(Error::Missing("extracted page missing from copy map"))?;
        let kid = if used.insert(copied_page_ref) {
            copied_page_ref
        } else {
            next_num = next_num.checked_add(1).ok_or_else(|| {
                // cov:ignore-start: unreachable in practice — copy_objects
                // renumbers the freshly built target sequentially from a small
                // base, so hitting u32::MAX would need ~2^32 copied objects.
                // The `})?;` terminator carries the Err-propagation region of
                // this same arm, so the block extends through it.
                Error::Unsupported(
                    "page extract: object-number overflow allocating duplicate page".to_string(),
                )
            })?;
            // cov:ignore-end
            let clone_ref = ObjectRef::new(next_num, 0);
            // The one intentional copy: the duplicate kid's own dictionary.
            let dict = resolve_dict(target, copied_page_ref, "copied page is not a dictionary")?; // cov:ignore: Err arm unreachable — the first copy of this page resolved to a dictionary before it was reparented above
            target.set_object(clone_ref, Object::Dictionary(dict));
            clone_ref
        };
        kids.push(kid);
    }
    Ok(())
}

/// Resolve the target catalog's `/Pages` root ref.
pub(crate) fn target_pages_root(target: &mut Pdf<Cursor<Vec<u8>>>) -> Result<ObjectRef> {
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
/// Shared by [`target_pages_root`] and the merge route's page/root bookkeeping;
/// the error arm guards against a ref resolving to a non-dictionary (or a
/// missing object, which resolves to [`Object::Null`]).
pub(crate) fn resolve_dict(
    target: &mut Pdf<Cursor<Vec<u8>>>,
    r: ObjectRef,
    ctx: &'static str,
) -> Result<Dictionary> {
    match target.resolve(r)? {
        Object::Dictionary(d) => Ok(d),
        _ => Err(Error::Missing(ctx)),
    }
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
