//! Page extraction into a fresh minimal document.
//!
//! [`extract_pages`] builds a brand-new minimal [`Pdf`] containing the
//! selected pages from `source` plus their transitive object closure, copied
//! across documents; [`extract_page`] is the single-page convenience form.
//! This mirrors qpdf's `emptyPDF()` + `QPDFPageDocumentHelper::addPage()`
//! pattern: the document object is constructed and populated here, then
//! written by a separate writer (`write_pdf` / `write_pdf_with_options`).
//!
//! `source` is left unmodified. Inherited page attributes (`/Resources`,
//! `/MediaBox`, `/CropBox`, `/Rotate`) are materialized onto each extracted
//! page exactly as [`crate::page_tree_rebuild`] does, so the pages render
//! identically in isolation.
//!
//! Composes [`page_object_closure`](crate::page_closure::page_object_closure)
//! and [`copy_objects`]. All selected pages
//! are copied in a single pass, so objects shared between them (fonts, images,
//! content streams) are copied exactly once.
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
//! If such a reference caused an unselected source page to enter the copied
//! closure, that copied page object is replaced with `null`, matching qpdf's
//! page-selection behavior without interpreting the carrier's semantics.

use crate::object_copy::{copy_objects, rewrite_refs};
use crate::page_closure::extend_page_object_closure;
use crate::page_label_document_helper::merge_adjacent_ranges;
use crate::page_rotate::resolve_inherited_rotate_with_max_depth;
use crate::page_tree_rebuild::resolve_inherited_raw;
use crate::pages::{
    page_refs, resolve_inherited_resources_with_max_depth, DEFAULT_MAX_PAGE_TREE_DEPTH,
};
use crate::subset_prune::sweep_unreachable_objects;
use crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read, Seek};

/// Inherited page attributes resolved from the source page tree before the
/// copy severs the `/Parent` chain.
pub(crate) struct InheritedAttrs {
    pub(crate) resources: Option<Dictionary>,
    pub(crate) rotate: i32,
    pub(crate) mediabox: Option<Object>,
    pub(crate) cropbox: Option<Object>,
}

impl InheritedAttrs {
    /// Resolve the four inheritable page attributes (`/Resources`, `/Rotate`,
    /// `/MediaBox`, `/CropBox`) for `page_ref` from `source`'s page tree, before
    /// any copy severs the `/Parent` chain.
    pub(crate) fn resolve<R: Read + Seek>(
        source: &mut Pdf<R>,
        page_ref: ObjectRef,
        depth: usize,
    ) -> Result<Self> {
        Ok(InheritedAttrs {
            resources: resolve_inherited_resources_with_max_depth(source, page_ref, depth)?,
            rotate: resolve_inherited_rotate_with_max_depth(source, page_ref, depth)?,
            mediabox: resolve_inherited_raw(source, page_ref, "MediaBox", depth)?,
            cropbox: resolve_inherited_raw(source, page_ref, "CropBox", depth)?,
        })
    }
}

/// Extract the pages at `page_indices` (0-based) from `source` into a
/// brand-new minimal document.
///
/// Returns an owned in-memory [`Pdf`] whose catalog has a single-level
/// `/Pages` tree with one `/Kids` entry per selected index, in **selection
/// order** (any order is accepted, matching qpdf's `--pages` selection
/// semantics). Selected pages are copied in a single pass with one shared
/// renumbering map, so objects referenced by several selected pages (fonts,
/// images, content streams) appear exactly once in the output.
///
/// An index may appear more than once. The second and later occurrences of a
/// page become shallow clones of its first copy: each duplicate gets its own
/// page object, while indirectly referenced sub-objects (`/Contents`,
/// `/Resources`, `/Annots`, `/B`) stay shared between the duplicates,
/// matching qpdf 11.9.0's observed duplicate-page output.
///
/// The returned document is already minimal: copied ancestor `/Pages` nodes
/// left over from the closure are pruned (mark-and-sweep from the new
/// catalog) before returning. Write it with [`write_pdf`](crate::write_pdf)
/// or [`write_pdf_with_options`](crate::write_pdf_with_options); enabling
/// [`WriteOptions::full_rewrite`](crate::WriteOptions::full_rewrite) is
/// recommended for compaction but is not required for correctness.
///
/// `source` is not modified. See also [`extract_page`] for the single-page
/// form, and the [module documentation](self) for how references to removed
/// pages are handled and how `/PageLabels` is reconstructed for the selection.
///
/// # Examples
///
/// ```no_run
/// use std::fs::File;
/// use std::io::BufReader;
/// use flpdf::{extract_pages, write_pdf_with_options, Pdf, WriteOptions};
///
/// let file = BufReader::new(File::open("input.pdf")?);
/// let mut pdf = Pdf::open(file)?;
///
/// // First and third page (0-based), in selection order.
/// let mut extracted = extract_pages(&mut pdf, &[0, 2])?;
///
/// let mut options = WriteOptions::default();
/// options.full_rewrite = true;
/// let mut out = File::create("extracted.pdf")?;
/// write_pdf_with_options(&mut extracted, &mut out, &options)?;
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

    // Unique source pages in first-occurrence order. Duplicates re-use the
    // same copied object and are shallow-cloned when building /Kids below.
    let mut seen: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut unique: Vec<ObjectRef> = Vec::with_capacity(selected.len());
    for &page_ref in &selected {
        if seen.insert(page_ref) {
            unique.push(page_ref);
        }
    }

    // Resolve inherited attributes from the SOURCE before copying severs the
    // /Parent chain. Same four attributes / helpers as page_tree_rebuild.
    let depth = DEFAULT_MAX_PAGE_TREE_DEPTH;
    let mut inherited: Vec<InheritedAttrs> = Vec::with_capacity(unique.len());
    for &page_ref in &unique {
        inherited.push(InheritedAttrs::resolve(source, page_ref, depth)?);
    }

    // UNION of the per-page transitive closures, then ONE deep-copy pass into
    // a fresh minimal doc: a single renumbering map means an object shared by
    // several selected pages is copied exactly once. The closures share one
    // `visited` set so a subtree reachable from several selected pages is
    // walked once for the whole union, not once per referencing page.
    let mut closure: BTreeSet<ObjectRef> = BTreeSet::new();
    for &page_ref in &unique {
        extend_page_object_closure(source, page_ref, &mut closure)?;
    }
    let mut target = Pdf::open_mem_owned(minimal_target_bytes())?;
    let map = copy_objects(source, &mut target, &closure)?;
    let selected_set: BTreeSet<ObjectRef> = unique.iter().copied().collect();
    null_copied_removed_pages(&mut target, &all_pages, &selected_set, &closure, &map);
    let pages_root_ref = target_pages_root(&mut target)?;

    // Materialize inherited attrs onto each copied leaf (remapping refs), then
    // repoint /Parent at the fresh root.
    let mut copied_unique: Vec<ObjectRef> = Vec::with_capacity(unique.len());
    for (&src_ref, attrs) in unique.iter().zip(inherited) {
        let copied_page_ref = *map
            .get(&src_ref)
            .ok_or(Error::Missing("extracted page missing from copy map"))?;
        materialize_leaf(&mut target, copied_page_ref, attrs, &map, pages_root_ref)?;
        copied_unique.push(copied_page_ref);
    }

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

    // Build /Kids in SELECTION order. The first occurrence of a source page
    // uses its mapped copy; later occurrences get a shallow clone of the
    // materialized first copy: a fresh page object whose
    // indirectly referenced sub-objects (/Contents, /Resources, /Annots, /B)
    // stay shared, matching qpdf's observed duplicate-page output and
    // page_tree_rebuild's duplicate-selection scheme.
    let mut kids: Vec<ObjectRef> = Vec::with_capacity(selected.len());
    let mut used: BTreeSet<ObjectRef> = BTreeSet::new();
    append_selection_kids(&mut target, &selected, &map, &mut used, &mut kids)?;

    // Build the fresh single-level /Pages root.
    let mut root = resolve_dict(
        &mut target,
        pages_root_ref,
        "target /Pages is not a dictionary",
    )?;
    root.insert(
        "Kids",
        Object::Array(kids.iter().map(|&r| Object::Reference(r)).collect()),
    );
    root.insert("Count", Object::Integer(kids.len() as i64));
    target.set_object(pages_root_ref, Object::Dictionary(root));

    // Drop the copied ancestor /Pages node(s) and any objects only they
    // referenced: they are unreachable from the new catalog now that each leaf
    // /Parent points at the fresh root. full_rewrite does NOT garbage-collect
    // (it emits every non-deleted object), so prune here to satisfy
    // "no unrelated objects". Same mark-and-sweep used after page-subset
    // rebuild (subset_prune::sweep_unreachable_objects).
    sweep_unreachable_objects(&mut target)?;

    Ok(target)
}

/// Replace every copied but unselected source page with `null`.
///
/// Page identity comes from the source page tree (`all_pages`), not from the
/// semantics or `/Type` of the object carrying the reference. Pages outside
/// `closure` were never copied and therefore require no placeholder object.
pub(crate) fn null_copied_removed_pages<R: Read + Seek>(
    target: &mut Pdf<R>,
    all_pages: &[ObjectRef],
    selected: &BTreeSet<ObjectRef>,
    closure: &BTreeSet<ObjectRef>,
    map: &BTreeMap<ObjectRef, ObjectRef>,
) {
    for source_page in all_pages {
        if !selected.contains(source_page) && closure.contains(source_page) {
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
    extract_pages(source, &[page_index])
}

/// Materialize the four inheritable attributes onto a copied leaf page and
/// repoint its `/Parent` at `pages_root_ref`.
///
/// `attrs` were resolved from the source page tree before the copy severed the
/// `/Parent` chain; each is inserted only when the leaf does not already carry
/// it directly, with any indirect references inside the attribute value
/// remapped through `map` into the target's numbering. Shared by
/// [`extract_pages`] and [`crate::page_merge::merge_documents`].
pub(crate) fn materialize_leaf(
    target: &mut Pdf<Cursor<Vec<u8>>>,
    copied_page_ref: ObjectRef,
    attrs: InheritedAttrs,
    map: &std::collections::BTreeMap<ObjectRef, ObjectRef>,
    pages_root_ref: ObjectRef,
) -> Result<()> {
    let mut leaf = resolve_dict(target, copied_page_ref, "copied page is not a dictionary")?; // cov:ignore: Err arm unreachable — page_refs yields only /Type /Page dicts and copy_objects preserves the source page dict

    if !has_own(&leaf, "Resources") {
        if let Some(res) = attrs.resources {
            let mut value = Object::Dictionary(res);
            rewrite_refs(&mut value, 0, map)?;
            leaf.insert("Resources", value);
        }
    }
    if !has_own(&leaf, "MediaBox") {
        if let Some(mut mb) = attrs.mediabox {
            rewrite_refs(&mut mb, 0, map)?;
            leaf.insert("MediaBox", mb);
        } // cov:ignore: rewrite_refs ? Err arm (MAX_INLINE_DEPTH) unreachable for shallow inherited /MediaBox
    }
    if !has_own(&leaf, "CropBox") {
        if let Some(mut cb) = attrs.cropbox {
            rewrite_refs(&mut cb, 0, map)?;
            leaf.insert("CropBox", cb);
        }
    }
    if !has_own(&leaf, "Rotate") {
        leaf.insert("Rotate", Object::Integer(attrs.rotate as i64));
    }
    leaf.insert("Parent", Object::Reference(pages_root_ref));
    target.set_object(copied_page_ref, Object::Dictionary(leaf));
    Ok(())
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
/// [`crate::page_merge::merge_documents`], or once by [`extract_pages`].
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
            let dict = resolve_dict(target, copied_page_ref, "copied page is not a dictionary")?; // cov:ignore: Err arm unreachable — the first copy of this page resolved to a dictionary in the materialize loop above
            target.set_object(clone_ref, Object::Dictionary(dict));
            clone_ref
        };
        kids.push(kid);
    }
    Ok(())
}

/// Minimal valid target: Catalog(1) + empty Pages(2). No placeholder page (so
/// there is no orphan to delete after copying).
pub(crate) fn minimal_target_bytes() -> Vec<u8> {
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
/// Shared by [`extract_pages`]'s leaf/root materialization and
/// [`target_pages_root`]; the error arm guards against a ref resolving to a
/// non-dictionary (or a missing object, which resolves to [`Object::Null`]).
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
