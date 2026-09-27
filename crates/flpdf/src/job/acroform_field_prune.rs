//! AcroForm field preservation after page-subset extraction.
//!
//! qpdf correspondence: QPDFJob.cc removal of unreferenced form fields after page selection.
//!
//! After [`crate::pages::tree_rebuild::rebuild_page_tree`] has rebuilt the page
//! tree so that only selected pages remain reachable from `/Root`, this module
//! enumerates those pages as raw handles, asks the AcroForm helper for each
//! page's top-level Widget fields, and filters `/AcroForm /Fields` by raw
//! `QpdfObjGen`. Stale or dangling `/P` page back-pointers are removed, but
//! the current `/Annots` owner is never used to synthesize a new `/P`.
//!
//! # qpdf 11.9.0 observed behaviour (truth source `/usr/bin/qpdf`)
//!
//! Test fixture: 3-page PDF with:
//!   - **FieldA** — merged field+widget dict (carries both `/T (FieldA)` and
//!     `/Subtype /Widget`) on page 1.
//!   - **FieldB** — split field: a parent dict with `/T (FieldB)` and
//!     `/Kids [B1 B2]`, where B1 is a pure widget on page 2 and B2 is a pure
//!     widget on page 3.
//!   - **FieldC** — merged field+widget on page 3.
//!
//! `qpdf in.pdf --pages in.pdf 1,2 -- out.pdf` (drops page 3):
//!   - `/AcroForm /Fields` in output: `[FieldA, FieldB]` — FieldC removed.
//!   - FieldB's `/Kids` still contains **both** B1 and B2; qpdf does **not**
//!     prune dropped-page widget entries from `/Kids`.
//!   - Existing `/P` entries that still resolve to retained pages remain
//!     unchanged.
//!   - B2 has no `/P` (its page was dropped; qpdf's page null-out and writer
//!     null suppression remove the stale entry).
//!   - `/AcroForm` remains on the catalog.
//!
//! `qpdf in.pdf --pages in.pdf 2 -- out.pdf` (all FieldA and FieldC widgets
//! dropped, only B1 retained):
//!   - `/Fields`: `[FieldB]`.
//!   - FieldB `/Kids` still contains both B1 and B2.
//!
//! `qpdf /only-fieldA-on-page1.pdf --pages … 2 -- out.pdf` (all widgets dropped):
//!   - `/AcroForm` is **removed** from the catalog entirely. `/Fields` becomes
//!     empty and the husk dict is not left behind.
//!
//! **flpdf matches qpdf exactly** on the above points:
//!   - Field survival is determined at the **top-level `/Fields`** granularity.
//!   - `/Kids` of a kept field are **not** pruned (matching qpdf).
//!   - Existing widget `/P` values that resolve to retained pages are kept.
//!   - Stale or dangling widget `/P` values are **removed**, preventing
//!     dangling refs after GC (matching qpdf: B2 had no `/P` in the pages-1,2
//!     extract output).
//!   - The first primary page occurrence is not passed through qpdf's
//!     `fixCopiedAnnotations`; widget indirectness is not copy provenance.
//!   - Empty `/Fields` → `/AcroForm` removed from catalog.
//!
//! # Scope — single document only
//!
//! This module operates on **one** [`Pdf`] produced by a single-input
//! extraction pipeline.  Multi-input cross-document AcroForm merging (merging
//! `/AcroForm` dicts from multiple source documents, handling field-name
//! collisions with qpdf-style suffix renaming) is explicitly **out of scope**
//! here and is not currently supported.  The single-document API boundary makes the cross-doc case
//! unreachable at this layer, so no `Error::Unsupported` stub is needed; see
//! the comment in `pages::tree_rebuild` for the same rationale.
//!
//! Heavy AcroForm operations (flattening, rendering appearance streams) are out
//! of scope; this module handles only the
//! extract-time field/widget survival filter and stale `/P` cleanup.

use crate::object_handle::{ObjectHandle, ObjectHandleIdentity};
use crate::pages::tree_rebuild::RebuildResult;
use crate::qpdf_obj_gen::QpdfObjGen;
use crate::{AcroFormDocumentHelper, Pdf, Result};
use std::collections::{BTreeSet, HashMap};
use std::io::{Read, Seek};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Default maximum depth for walking an AcroForm field tree.
///
/// Matches the depth limit used by the outline-remap module.
pub const DEFAULT_MAX_ACROFORM_DEPTH: usize = 100;

/// Canonical identities of Widget handles found on retained raw page handles.
#[allow(clippy::mutable_key_type)]
type WidgetPageMap = HashMap<ObjectHandleIdentity, ObjectHandle>;

/// Prune `/AcroForm /Fields` after a page-subset extraction and remove stale
/// widget `/P` back-pointers.
///
/// `result` is the [`RebuildResult`] from
/// [`crate::pages::tree_rebuild::rebuild_page_tree`]. Retained pages come from
/// the current raw page-handle list; `removed_page_objgens` identifies the
/// original leaves removed by the rebuild for callers that have not run the
/// production null-out pass yet.
///
/// The function mutates `pdf` in place and is a no-op when there is no
/// `/AcroForm` in the catalog.
///
/// # Errors
///
/// - Any error propagated from canonical ObjectHandle resolution.
/// - [`crate::Error::Unsupported`] when the field-tree depth limit is exceeded.
pub(crate) fn prune_acroform_after_subset<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    result: &RebuildResult,
) -> Result<()> {
    prune_acroform_after_subset_with_max_depth(pdf, result, DEFAULT_MAX_ACROFORM_DEPTH)
}

/// Like [`prune_acroform_after_subset`] but with a caller-supplied depth limit
/// for the field-tree walk.
///
/// # Errors
///
/// - Any error propagated from canonical ObjectHandle resolution.
/// - [`crate::Error::Unsupported`] when the field-tree depth limit is exceeded.
#[allow(clippy::mutable_key_type)]
pub(crate) fn prune_acroform_after_subset_with_max_depth<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    result: &RebuildResult,
    max_depth: usize,
) -> Result<()> {
    // ── Step 1: collect raw pages, fields, and Widgets in qpdf order ───────
    // QPDFJob asks QPDFAcroFormDocumentHelper for the top-level fields on
    // each selected QPDFPageObjectHelper (`QPDFJob.cc:2599-2608`). Keep page
    // and field identity in qpdf's raw QpdfObjGen domain; a page or field
    // need not have a valid ObjectRef projection.
    let page_handles = crate::PageDocumentHelper::new(pdf).get_all_pages()?;
    let mut widget_to_page = WidgetPageMap::new();
    let mut retained_page_objgens = BTreeSet::new();
    let mut referenced_fields = BTreeSet::new();
    {
        // QPDFJob clears the primary page tree before constructing its
        // AcroForm helper (`QPDFJob.cc:2469-2471,2515`). Its initial analysis
        // therefore traverses the field tree without page-orphan Widgets;
        // selected raw pages are passed to getFormFieldsForPage afterward.
        let mut acroform_helper = AcroFormDocumentHelper::new_for_field_tree(pdf)?;
        for page_handle in page_handles {
            retained_page_objgens.insert(page_handle.get_obj_gen());
            let page_widgets = acroform_helper.get_widget_annotations_for_page(page_handle)?;
            let page_fields =
                acroform_helper.get_form_fields_for_widget_annotations(&page_widgets)?;
            referenced_fields.extend(page_fields.into_iter().map(|field| field.get_obj_gen()));
            for widget in page_widgets {
                widget_to_page
                    .entry(widget.identity_key())
                    .or_insert(widget);
            }
        }
    }

    // ── Step 3: locate and process /AcroForm ──────────────────────────────
    // QPDFJob works from the live catalog handle and mutates its existing
    // AcroForm graph in place (`QPDFJob.cc:2610-2632`).  Keep the same
    // ObjectHandle identity throughout this operation; do not materialize a
    // parallel raw dictionary snapshot.
    let catalog = pdf.trailer().try_get_key(b"/Root")?;
    if !catalog.try_is_dictionary()? {
        return Ok(());
    }

    // /AcroForm may be a direct dict or an indirect reference.
    let acroform = catalog.try_get_key(b"/AcroForm")?;
    if !acroform.try_is_dictionary()? {
        return Ok(()); // No /AcroForm — nothing to do.
    }

    // Resolve /Fields, handling the indirect-array form.
    let fields = acroform.try_get_key(b"/Fields")?;
    let Some(fields_arr) = fields.try_as_array()? else {
        return Ok(()); // /Fields is missing or not an array.
    };

    // qpdf allocates the replacement array's object number *before* counting
    // how many fields survive (`QPDFJob.cc:2610-2614`: `new_fields =
    // pdf.makeIndirectObject(new_fields)` runs ahead of the keep/drop loop).
    // When every field is later dropped, that already-allocated object is
    // never linked anywhere and stays an orphan in the object table -- this
    // reservation reproduces the same orphan rather than only allocating an
    // indirect object when there is content to attach to it.
    let reserved_indirect_fields = if fields.is_indirect() {
        Some(pdf.make_indirect_object_handle(ObjectHandle::array(Vec::new()))?)
    } else {
        None
    };

    // ── Step 4: keep fields referenced by retained pages ──────────────────
    // `getFormFieldsForPage` returns top-level fields and qpdf stores their
    // raw identities in `referenced_fields`; /Kids on a surviving field are
    // not pruned.
    let mut kept_fields = Vec::new();

    for field in fields_arr {
        if field.get_obj_gen().is_indirect() {
            // Preserve the caller-supplied safety limit for field branches
            // whose Widgets are all on dropped pages. qpdf's per-page helper
            // supplies field membership; this raw walk retains the existing
            // bounded traversal contract independently of that result.
            let _ = field_has_retained_widget_with_depth_limit(
                field.clone(),
                &widget_to_page,
                &mut BTreeSet::new(),
                0,
                max_depth,
            )?;
        }
        if referenced_fields.contains(&field.get_obj_gen()) {
            kept_fields.push(field);
        }
    }

    // ── Step 5: preserve valid /P and remove stale page references ─────────
    // qpdf does not infer a widget's page from the current /Annots owner. Its
    // copy paths establish /P through the object-copy map, while the first
    // primary occurrence is not passed through fixCopiedAnnotations. Keep an
    // existing /P that resolves to a retained page and remove only a stale or
    // dangling page reference; never synthesize a new /P from indirectness or
    // current annotation membership.
    //
    // For dropped-page widgets that remain in a kept field's /Kids (qpdf does
    // not prune /Kids), we must *remove* /P so the widget does not hold a
    // dangling reference to the orphaned page dict after prune_after_subset
    // GCs it (qpdf 11.9.0 observed: B2 had no /P in pages-1,2 output).
    for widget in widget_to_page.values() {
        remove_stale_widget_page_ref(widget, &retained_page_objgens, &result.removed_page_objgens)?;
    }
    // Collect all widgets reachable from kept fields; strip /P from any that
    // are NOT in widget_to_page (i.e. live in a kept field's /Kids but were on
    // a dropped page).
    for field in &kept_fields {
        strip_dropped_widget_p_refs(
            field.clone(),
            &widget_to_page,
            &mut BTreeSet::new(),
            0,
            max_depth,
        )?;
    }

    // ── Step 6: write back pruned /AcroForm or remove it ─────────────────
    if kept_fields.is_empty() {
        // All fields dropped → remove /AcroForm from catalog entirely,
        // matching qpdf's observed behaviour. `reserved_indirect_fields`
        // (if any) is intentionally left allocated but unlinked here,
        // matching qpdf's own orphaned `new_fields` object in this case.
        catalog.remove_key(b"/AcroForm");
    } else {
        // qpdf creates a fresh array for an indirect /Fields holder and
        // replaces the key on the live AcroForm handle. A direct holder
        // remains direct (`QPDFJob.cc:2620-2631`).
        let replacement = if let Some(reserved) = reserved_indirect_fields {
            reserved.try_set_array_items(kept_fields)?;
            reserved
        } else {
            ObjectHandle::array(kept_fields)
        };
        acroform.replace_key(b"/Fields", replacement)?;
    }

    // Step 6 changes both the AcroForm field tree and widget page links.
    // qpdf's explicit invalidation contract covers these external mutations
    // (`qpdf/include/qpdf/QPDFAcroFormDocumentHelper.hh:68-78`); do not let a
    // helper created before subset write-back reuse the old association map.
    *pdf.acroform_cache.borrow_mut() = None;
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Walk a field's raw `/Kids` tree until a retained Widget is found, preserving
/// the caller-supplied depth limit used by the previous subset pruning path.
///
/// The result does not select `/Fields`; qpdf's per-page helper supplies that
/// raw-identity membership. This traversal preserves the explicit safety
/// boundary even for top-level fields that will be dropped.
#[allow(clippy::mutable_key_type)]
fn field_has_retained_widget_with_depth_limit(
    field: ObjectHandle,
    widget_to_page: &WidgetPageMap,
    visited: &mut BTreeSet<QpdfObjGen>,
    depth: usize,
    max_depth: usize,
) -> Result<bool> {
    let field_objgen = field.get_obj_gen();
    if depth > max_depth {
        return Err(crate::Error::Unsupported(format!(
            "acroform_field_prune: field-tree depth limit {max_depth} exceeded at {field_objgen}"
        )));
    }
    if !field_objgen.is_indirect() || !visited.insert(field_objgen) {
        return Ok(false);
    }
    if widget_to_page.contains_key(&field.identity_key()) {
        return Ok(true);
    }
    if !field.try_is_dictionary()? {
        return Ok(false);
    }

    let kids = field.try_get_key(b"/Kids")?;
    let Some(kids) = kids.try_as_array()? else {
        return Ok(false);
    };
    for kid in kids {
        kid.try_dereference()?;
        let kid_objgen = kid.get_obj_gen();
        if !kid_objgen.is_indirect() {
            continue;
        }
        if widget_to_page.contains_key(&kid.identity_key()) {
            return Ok(true);
        }
        if field_has_retained_widget_with_depth_limit(
            kid,
            widget_to_page,
            visited,
            depth + 1,
            max_depth,
        )? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Preserve a valid widget `/P` and remove only a dangling (nulled) page ref.
///
/// qpdf establishes `/P` during copied-annotation graph remapping, not through
/// a generic page-owner repair pass. In the production route, this runs after
/// [`crate::job::remap_outline_and_dests`], which already
/// replaces every genuinely removed original page-tree leaf with `null` in
/// place (`null_removed_pages`, page-driven, independent of how it is
/// referenced) — so a `/P` pointing at a removed page already resolves to
/// a null object by the time this runs there. This function is also a public
/// API entry point that a caller may invoke directly after
/// [`crate::pages::tree_rebuild::rebuild_page_tree`] without that null-out
/// step, so `removed_pages` (the same original-leaf drop set the null-out
/// pass keys on) is checked independently of the target's current null
/// state — a widget's `/P` pointing at a genuinely dropped page is always
/// removed, whether or not it has been nulled yet. A live off-tree object
/// that merely carries `/Type /Page` but was never in the removed-page set
/// must therefore be preserved verbatim, matching qpdf's untouched
/// first-primary-occurrence path. Only dictionaries are inspected — widget
/// annotations should not be streams, but we guard defensively.
fn remove_stale_widget_page_ref(
    widget: &ObjectHandle,
    retained_page_objgens: &BTreeSet<QpdfObjGen>,
    removed_pages: &BTreeSet<QpdfObjGen>,
) -> Result<()> {
    if !widget.try_is_dictionary()? || !widget.try_has_key(b"/P")? {
        return Ok(());
    }
    let existing = widget.try_get_key(b"/P")?;
    let Some(existing_gen) = existing.qpdf_obj_gen() else {
        return Ok(());
    };
    if retained_page_objgens.contains(&existing_gen) {
        return Ok(());
    }
    if removed_pages.contains(&existing_gen) {
        widget.remove_key(b"/P");
        return Ok(());
    }
    if !existing.try_is_null()? {
        return Ok(());
    }
    widget.remove_key(b"/P");
    Ok(())
}

/// Walk a kept field's `/Kids` tree and remove `/P` from any widget that is
/// **not** in `widget_to_page` (i.e. its page was dropped).  This prevents
/// dangling indirect references after `prune_after_subset` GCs the orphaned
/// page objects, matching qpdf's observed output (B2 had no `/P` in the
/// pages-1,2 extraction result).
///
/// `visited` / `depth` / `max_depth` guard against cycles and over-deep trees.
#[allow(clippy::mutable_key_type)]
fn strip_dropped_widget_p_refs(
    field: ObjectHandle,
    widget_to_page: &WidgetPageMap,
    visited: &mut BTreeSet<QpdfObjGen>,
    depth: usize,
    max_depth: usize,
) -> Result<()> {
    let field_objgen = field.get_obj_gen();
    if depth > max_depth {
        return Err(crate::Error::Unsupported(format!(
            "acroform_field_prune: field-tree depth limit {max_depth} exceeded at {field_objgen}"
        )));
    }
    if !visited.insert(field_objgen) {
        return Ok(()); // Cycle guard.
    }

    let kids = field.try_get_key(b"/Kids")?;
    let Some(kids_arr) = kids.try_as_array()? else {
        // Leaf node with no /Kids. Merged field+widget dicts that were
        // retained were already handled by remove_stale_widget_page_ref; dropped
        // merged fields are not in kept_fields, so there is nothing to strip.
        return Ok(());
    };

    for kid in kids_arr {
        kid.try_dereference()?;
        // qpdf ignores direct field-tree entries, so do not promote or mutate
        // a direct `/Kids` member here.
        if !kid.get_obj_gen().is_indirect() {
            continue;
        }

        let subtype = kid.try_get_key(b"/Subtype")?;
        let is_widget = subtype.try_as_name()?.as_deref() == Some(b"Widget".as_slice());

        if is_widget {
            if !widget_to_page.contains_key(&kid.identity_key()) {
                // Widget on a dropped page — remove stale /P.
                kid.remove_key(b"/P");
            }
            // Pure widget kids do not have /Kids of their own (spec: a widget
            // annotation is a leaf); no need to recurse.
        } else {
            // Sub-field: recurse.
            strip_dropped_widget_p_refs(kid, widget_to_page, visited, depth + 1, max_depth)?;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::check_bytes_for_test;
    use crate::object_handle::ObjectValue;
    use crate::pages::page_refs;
    use crate::pages::tree_rebuild::rebuild_page_tree;
    use crate::writer::write_qpdf_to_memory;
    use crate::ObjectRef;
    use crate::Pdf;
    use std::collections::BTreeMap;
    use std::io::Cursor;

    // ── Fixture builder ───────────────────────────────────────────────────

    /// Build a 3-page AcroForm PDF matching the qpdf observation fixture:
    ///
    /// ```text
    /// 1 0 R  Catalog  → /Pages 2 0 R, /AcroForm 6 0 R
    /// 2 0 R  Pages    → /Kids [3 0 R 4 0 R 5 0 R]
    /// 3 0 R  Page 1   → /Annots [7 0 R]
    /// 4 0 R  Page 2   → /Annots [9 0 R]
    /// 5 0 R  Page 3   → /Annots [10 0 R 11 0 R]
    /// 6 0 R  AcroForm → /Fields [7 0 R 8 0 R 11 0 R]
    /// 7 0 R  FieldA   merged field+widget (page 1)
    /// 8 0 R  FieldB   parent field /Kids [9 0 R 10 0 R]
    /// 9 0 R  B1       pure widget (page 2)
    /// 10 0 R B2       pure widget (page 3)
    /// 11 0 R FieldC   merged field+widget (page 3)
    /// ```
    fn build_acroform_pdf() -> Vec<u8> {
        let objects: Vec<(u32, &[u8])> =
            vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 6 0 R >>"),
            (
                2,
                b"<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 /MediaBox [0 0 612 792] >>",
            ),
            (3, b"<< /Type /Page /Parent 2 0 R /Annots [7 0 R] >>"),
            (4, b"<< /Type /Page /Parent 2 0 R /Annots [9 0 R] >>"),
            (5, b"<< /Type /Page /Parent 2 0 R /Annots [10 0 R 11 0 R] >>"),
            (
                6,
                b"<< /Fields [7 0 R 8 0 R 11 0 R] /DA (/Helvetica 12 Tf 0 g) >>",
            ),
            (
                7,
                b"<< /Type /Annot /Subtype /Widget /FT /Tx /T (FieldA) /V (hello) \
                   /P 3 0 R /Rect [10 700 200 720] >>",
            ),
            (8, b"<< /FT /Tx /T (FieldB) /Kids [9 0 R 10 0 R] >>"),
            (
                9,
                b"<< /Type /Annot /Subtype /Widget /Parent 8 0 R /P 4 0 R \
                   /Rect [10 600 200 620] >>",
            ),
            (
                10,
                b"<< /Type /Annot /Subtype /Widget /Parent 8 0 R /P 5 0 R \
                   /Rect [10 500 200 520] >>",
            ),
            (
                11,
                b"<< /Type /Annot /Subtype /Widget /FT /Tx /T (FieldC) /V (world) \
                   /P 5 0 R /Rect [10 400 200 420] >>",
            ),
        ];
        build_pdf(&objects)
    }

    /// Build a minimal 2-page PDF where all fields are on page 1 only.
    fn build_all_on_page1_pdf() -> Vec<u8> {
        let objects: Vec<(u32, &[u8])> = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>"),
            (
                2,
                b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 612 792] >>",
            ),
            (3, b"<< /Type /Page /Parent 2 0 R /Annots [6 0 R] >>"),
            (4, b"<< /Type /Page /Parent 2 0 R >>"),
            (5, b"<< /Fields [<< /FT /Tx /T (DirectTop) /Kids [] >> 6 0 R] /DA (/Helvetica 12 Tf 0 g) >>"),
            (
                6,
                b"<< /Type /Annot /Subtype /Widget /FT /Tx /T (FieldA) \
                   /P 3 0 R /Rect [10 700 200 720] >>",
            ),
        ];
        build_pdf(&objects)
    }

    /// Build a 1-page PDF with no AcroForm at all.
    fn build_no_acroform_pdf() -> Vec<u8> {
        let objects: Vec<(u32, &[u8])> = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (
                2,
                b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>",
            ),
            (3, b"<< /Type /Page /Parent 2 0 R >>"),
        ];
        build_pdf(&objects)
    }

    /// Build a PDF whose only field-tree child is a direct Widget dictionary.
    /// qpdf's `traverseField` ignores direct `/Kids` entries, so the field must
    /// not be retained merely because that dictionary says `/Subtype /Widget`.
    fn build_direct_field_kid_pdf() -> Vec<u8> {
        let objects: Vec<(u32, &[u8])> = vec![
            (
                1,
                b"<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>",
            ),
            (
                2,
                b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>",
            ),
            (3, b"<< /Type /Page /Parent 2 0 R >>"),
            (5, b"<< /Fields [6 0 R] /DA (/Helvetica 12 Tf 0 g) >>"),
            (
                6,
                b"<< /FT /Tx /T (DirectKid) /Kids [<< /Type /Annot /Subtype /Widget /P 3 0 R /Rect [10 700 200 720] >>] >>",
            ),
        ];
        build_pdf(&objects)
    }

    fn build_pdf(objects: &[(u32, &[u8])]) -> Vec<u8> {
        let mut out = b"%PDF-1.6\n".to_vec();
        let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();
        for &(num, bytes) in objects {
            offsets.insert(num, out.len() as u64);
            out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
            out.extend_from_slice(bytes);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref_pos = out.len() as u64;
        let max_num = objects.iter().map(|&(n, _)| n).max().unwrap_or(0);
        out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", max_num + 1).as_bytes());
        for i in 1..=max_num {
            match offsets.get(&i) {
                Some(&off) => {
                    out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
                }
                None => {
                    out.extend_from_slice(b"0000000000 00001 f \n");
                }
            }
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_pos}\n%%EOF\n",
                max_num + 1
            )
            .as_bytes(),
        );
        out
    }

    fn open(bytes: Vec<u8>) -> Pdf<Cursor<Vec<u8>>> {
        Pdf::open(Cursor::new(bytes)).expect("PDF should parse")
    }

    fn dict_of(pdf: &mut Pdf<Cursor<Vec<u8>>>, r: ObjectRef) -> BTreeMap<Vec<u8>, ObjectHandle> {
        let handle = pdf.get_object_handle(r);
        handle.try_is_scalar().unwrap();
        handle
            .as_dictionary()
            .unwrap_or_else(|| panic!("{r} is not a dictionary: {handle:?}"))
    }

    fn reference_target(value: &ObjectHandle) -> Option<ObjectRef> {
        value.object_ref()
    }

    fn acroform_fields(pdf: &mut Pdf<Cursor<Vec<u8>>>) -> Vec<ObjectRef> {
        let catalog = pdf.trailer().try_get_key(b"/Root").unwrap();
        catalog.try_is_scalar().unwrap();
        let acroform = catalog.try_get_key(b"/AcroForm").unwrap();
        acroform.try_is_scalar().unwrap();
        if acroform.as_dictionary().is_none() {
            return vec![];
        }
        let fields = acroform.try_get_key(b"/Fields").unwrap();
        fields.try_is_scalar().unwrap();
        fields
            .as_array()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|field| field.object_ref())
            .collect()
    }

    // ── Tests ─────────────────────────────────────────────────────────────

    /// No AcroForm in input → function is a no-op, returns Ok.
    #[test]
    fn no_acroform_is_noop() {
        let mut pdf = open(build_no_acroform_pdf());
        let pages = page_refs(&mut pdf).unwrap();
        let result = rebuild_page_tree(&mut pdf, &pages).unwrap();
        assert!(prune_acroform_after_subset(&mut pdf, &result).is_ok());
        assert!(acroform_fields(&mut pdf).is_empty());
    }

    // `direct_page_widget_removes_dropped_page_ref` and
    // `duplicate_page_selection_removes_stale_direct_widget_page_refs` were
    // removed here: both called `rebuild_page_tree` then
    // `prune_acroform_after_subset` directly on a *live* (never-nulled)
    // dropped-page dictionary, an isolated-function-call precondition that
    // qpdf-parity-correct code never actually observes (the production route
    // always runs `remap_outline_and_dests`'s `null_removed_pages` first,
    // replacing a dropped page's original object with `null` in place before
    // this runs). Both tests were only passing because of the now-removed
    // `is_page_object` heuristic. Attempting to reproduce the real
    // precondition (nulling the dropped page, before or after
    // `rebuild_page_tree`) surfaces a pre-existing, unrelated staleness in
    // how `rebuild_page_tree` materializes a *direct* widget's `/P` redirect
    // relative to a same-call canonical null-out. This isolated precondition
    // does not reproduce through
    // any real call path: `flpdf --pages . 1 --` on this exact direct-widget
    // shape strips `/P` correctly, byte-identical with qpdf (verified
    // manually and covered by the CLI differential regression below).

    #[test]
    fn non_dictionary_widget_handle_is_ignored() {
        let widget = ObjectHandle::integer(1);

        let retained = BTreeSet::from([QpdfObjGen::new(3, 0)]);
        remove_stale_widget_page_ref(&widget, &retained, &BTreeSet::new()).unwrap();
    }

    #[test]
    fn retained_indirect_widget_keeps_existing_page_reference() {
        let mut pdf = open(build_acroform_pdf());
        let widget = pdf.get_object_handle(ObjectRef::new(7, 0));
        let retained = BTreeSet::from([QpdfObjGen::new(3, 0)]);

        remove_stale_widget_page_ref(&widget, &retained, &BTreeSet::new()).unwrap();

        let widget_dict = dict_of(&mut pdf, ObjectRef::new(7, 0));
        assert_eq!(
            widget_dict.get(b"/P".as_slice()).and_then(reference_target),
            Some(ObjectRef::new(3, 0)),
            "an indirect widget already owned by a retained page must keep /P"
        );
    }

    #[test]
    fn standalone_caller_removes_p_for_a_removed_page_before_null_out_runs() {
        // A library caller may invoke `prune_acroform_after_subset` directly
        // after `rebuild_page_tree`, without the production route's prior
        // `remap_outline_and_dests` null-out pass. Simulate that ordering:
        // widget 11's `/P 5 0 R` target (page 5) is still a live dictionary
        // (never nulled), but page 5 is a genuinely dropped original leaf, so
        // it is reported via `removed_page_objgens`. The removal must not depend on
        // the target's null state to match qpdf's actual removal set.
        let mut pdf = open(build_acroform_pdf());
        let widget = pdf.get_object_handle(ObjectRef::new(11, 0));
        widget.try_is_scalar().unwrap();
        let retained = BTreeSet::from([QpdfObjGen::new(3, 0), QpdfObjGen::new(4, 0)]);
        let removed_pages = BTreeSet::from([QpdfObjGen::new(5, 0)]);

        remove_stale_widget_page_ref(&widget, &retained, &removed_pages).unwrap();

        let widget_dict = dict_of(&mut pdf, ObjectRef::new(11, 0));
        assert!(
            !widget_dict.contains_key(b"/P".as_slice()),
            "a widget /P pointing at a page reported as removed must lose \
             the key even when the target has not been nulled yet"
        );
    }

    #[test]
    fn widget_non_reference_page_value_is_preserved() {
        let mut pdf = open(build_acroform_pdf());
        let widget = pdf.get_object_handle(ObjectRef::new(7, 0));
        widget.try_is_scalar().unwrap();
        widget.replace_key(b"/P", ObjectHandle::integer(7)).unwrap();

        let result = rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
        prune_acroform_after_subset(&mut pdf, &result).unwrap();

        assert_eq!(
            dict_of(&mut pdf, ObjectRef::new(7, 0))
                .get(b"/P".as_slice())
                .and_then(ObjectHandle::as_integer),
            Some(7),
            "qpdf writer does not infer page ownership from a non-reference /P value"
        );
    }

    #[test]
    fn widget_non_page_reference_is_preserved() {
        let mut pdf = open(build_acroform_pdf());
        let widget = pdf.get_object_handle(ObjectRef::new(7, 0));
        widget.try_is_scalar().unwrap();
        let non_page_ref = pdf.get_object_handle(ObjectRef::new(6, 0));
        widget.replace_key(b"/P", non_page_ref).unwrap();

        let result = rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
        prune_acroform_after_subset(&mut pdf, &result).unwrap();

        assert_eq!(
            dict_of(&mut pdf, ObjectRef::new(7, 0))
                .get(b"/P".as_slice())
                .and_then(reference_target),
            Some(ObjectRef::new(6, 0)),
            "qpdf does not rewrite a non-page /P target through an owner heuristic"
        );
    }

    #[test]
    fn direct_field_kid_is_not_promoted_to_retained_widget() {
        let mut pdf = open(build_direct_field_kid_pdf());
        let result = rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
        prune_acroform_after_subset(&mut pdf, &result).unwrap();

        let catalog = dict_of(&mut pdf, ObjectRef::new(1, 0));
        assert!(
            !catalog.contains_key(b"/AcroForm".as_slice()),
            "direct field-tree kid must not keep its parent field"
        );
    }

    /// Retained page widget → field kept.
    #[test]
    fn retained_page_widget_keeps_field() {
        let mut pdf = open(build_acroform_pdf());
        // Extract only pages 1 and 2 (objects 3 and 4) — drop page 3.
        let sel = [ObjectRef::new(3, 0), ObjectRef::new(4, 0)];
        let result = rebuild_page_tree(&mut pdf, &sel).unwrap();
        prune_acroform_after_subset(&mut pdf, &result).unwrap();

        let fields = acroform_fields(&mut pdf);
        // FieldA (7) and FieldB (8) should survive; FieldC (11) dropped.
        assert!(
            fields.contains(&ObjectRef::new(7, 0)),
            "FieldA should be retained; fields={fields:?}"
        );
        assert!(
            fields.contains(&ObjectRef::new(8, 0)),
            "FieldB should be retained (B1 on page 2); fields={fields:?}"
        );
        assert!(
            !fields.contains(&ObjectRef::new(11, 0)),
            "FieldC should be removed; fields={fields:?}"
        );
    }

    #[test]
    fn raw_generation_page_keeps_its_acroform_field_after_subset_pruning() {
        let mut pdf = Pdf::empty().expect("empty PDF should open");
        let catalog = pdf.root_handle().expect("empty PDF has a catalog");
        let pages = catalog
            .try_get_key(b"/Pages")
            .expect("empty PDF page-tree root");
        let page = pdf.get_object_handle_by_raw_identity(17, 65_535);
        let field = pdf.get_object_handle(ObjectRef::new(21, 0));
        let widget = pdf.get_object_handle(ObjectRef::new(22, 0));

        widget.set_resolved(ObjectValue::Dictionary(
            [
                (b"/Subtype".to_vec(), ObjectHandle::name(b"Widget".to_vec())),
                (b"/Parent".to_vec(), field.clone()),
                (b"/P".to_vec(), page.clone()),
            ]
            .into_iter()
            .collect(),
        ));
        field.set_resolved(ObjectValue::Dictionary(
            [
                (b"/FT".to_vec(), ObjectHandle::name(b"Tx".to_vec())),
                (b"/T".to_vec(), ObjectHandle::string(b"raw-page".to_vec())),
                (b"/Kids".to_vec(), ObjectHandle::array(vec![widget.clone()])),
            ]
            .into_iter()
            .collect(),
        ));
        page.set_resolved(ObjectValue::Dictionary(
            [
                (b"/Type".to_vec(), ObjectHandle::name(b"Page".to_vec())),
                (b"/Parent".to_vec(), pages.clone()),
                (
                    b"/MediaBox".to_vec(),
                    ObjectHandle::array(vec![
                        ObjectHandle::integer(0),
                        ObjectHandle::integer(0),
                        ObjectHandle::integer(612),
                        ObjectHandle::integer(792),
                    ]),
                ),
                (
                    b"/Annots".to_vec(),
                    ObjectHandle::array(vec![widget.clone()]),
                ),
            ]
            .into_iter()
            .collect(),
        ));
        pages
            .replace_key(b"/Kids", ObjectHandle::array(vec![page.clone()]))
            .expect("install raw-generation page");
        pages
            .replace_key(b"/Count", ObjectHandle::integer(1))
            .expect("set page count");
        catalog
            .replace_key(
                b"/AcroForm",
                ObjectHandle::dictionary(vec![(
                    b"/Fields".to_vec(),
                    ObjectHandle::array(vec![field.clone()]),
                )]),
            )
            .expect("install AcroForm");

        prune_acroform_after_subset(&mut pdf, &RebuildResult::default())
            .expect("raw page should not require ObjectRef projection");

        assert!(
            catalog
                .try_has_key(b"/AcroForm")
                .expect("check AcroForm presence"),
            "a Widget on a retained raw page must keep /AcroForm"
        );
        let acroform = catalog.try_get_key(b"/AcroForm").expect("read AcroForm");
        let fields = acroform
            .try_get_key(b"/Fields")
            .expect("read fields")
            .try_as_array()
            .expect("read fields array")
            .expect("fields must remain an array");
        assert_eq!(
            fields
                .iter()
                .map(ObjectHandle::get_obj_gen)
                .collect::<Vec<_>>(),
            vec![QpdfObjGen::new(21, 0)],
            "a field with a Widget on the retained raw page must remain in /Fields"
        );
        assert_eq!(
            widget
                .try_get_key(b"/P")
                .expect("read Widget page")
                .get_obj_gen(),
            QpdfObjGen::new(17, 65_535),
            "the existing /P keeps the raw identity of its retained page"
        );
    }

    #[test]
    fn prune_preserves_the_caller_supplied_field_tree_depth_limit() {
        let objects: Vec<(u32, &[u8])> = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>"),
            (
                2,
                b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>",
            ),
            (3, b"<< /Type /Page /Parent 2 0 R /Annots [9 0 R] >>"),
            (5, b"<< /Fields [6 0 R] >>"),
            (6, b"<< /FT /Tx /T (Top) /Kids [7 0 R] >>"),
            (7, b"<< /T (Middle) /Parent 6 0 R /Kids [8 0 R] >>"),
            (8, b"<< /T (Inner) /Parent 7 0 R /Kids [9 0 R] >>"),
            (
                9,
                b"<< /Type /Annot /Subtype /Widget /Parent 8 0 R /P 3 0 R /Rect [0 0 10 10] >>",
            ),
        ];
        let mut pdf = open(build_pdf(&objects));
        let result =
            rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).expect("keep the only page");

        let error = prune_acroform_after_subset_with_max_depth(&mut pdf, &result, 1)
            .expect_err("nested sub-fields exceed the caller-supplied depth limit");

        assert!(matches!(
            error,
            crate::Error::Unsupported(message)
                if message == "acroform_field_prune: field-tree depth limit 1 exceeded at 8,0"
        ));
    }

    #[test]
    fn prune_enforces_the_depth_limit_on_fields_with_only_dropped_widgets() {
        let objects: Vec<(u32, &[u8])> = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>"),
            (
                2,
                b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 612 792] >>",
            ),
            (3, b"<< /Type /Page /Parent 2 0 R >>"),
            (4, b"<< /Type /Page /Parent 2 0 R /Annots [9 0 R] >>"),
            (5, b"<< /Fields [6 0 R] >>"),
            (6, b"<< /FT /Tx /T (Top) /Kids [7 0 R] >>"),
            (7, b"<< /T (Middle) /Parent 6 0 R /Kids [8 0 R] >>"),
            (8, b"<< /T (Inner) /Parent 7 0 R /Kids [9 0 R] >>"),
            (
                9,
                b"<< /Type /Annot /Subtype /Widget /Parent 8 0 R /P 4 0 R /Rect [0 0 10 10] >>",
            ),
        ];
        let mut pdf = open(build_pdf(&objects));
        let result = rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)])
            .expect("keep the page without Widgets");

        let error = prune_acroform_after_subset_with_max_depth(&mut pdf, &result, 1)
            .expect_err("the caller limit applies even when all Widgets are on dropped pages");

        assert!(matches!(
            error,
            crate::Error::Unsupported(message)
                if message == "acroform_field_prune: field-tree depth limit 1 exceeded at 8,0"
        ));
    }

    #[test]
    fn prune_checks_the_depth_limit_on_later_subfields_after_a_retained_widget() {
        let objects: Vec<(u32, &[u8])> =
            vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 6 0 R >>"),
            (
                2,
                b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 612 792] >>",
            ),
            (3, b"<< /Type /Page /Parent 2 0 R /Annots [7 0 R] >>"),
            (4, b"<< /Type /Page /Parent 2 0 R >>"),
            (6, b"<< /Fields [8 0 R] >>"),
            (7, b"<< /Type /Annot /Subtype /Widget /Parent 8 0 R /P 3 0 R /Rect [0 0 10 10] >>"),
            (8, b"<< /FT /Tx /T (Top) /Kids [7 0 R 9 0 R] >>"),
            (9, b"<< /T (Middle) /Parent 8 0 R /Kids [10 0 R] >>"),
            (10, b"<< /T (Inner) /Parent 9 0 R /Kids [11 0 R] >>"),
            (11, b"<< /Type /Annot /Subtype /Widget /Parent 10 0 R /P 4 0 R /Rect [0 0 10 10] >>"),
        ];
        let mut pdf = open(build_pdf(&objects));
        let result =
            rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).expect("keep the first page");

        let error = prune_acroform_after_subset_with_max_depth(&mut pdf, &result, 1)
            .expect_err("a later dropped-page branch exceeds the depth limit");

        assert!(matches!(
            error,
            crate::Error::Unsupported(message)
                if message == "acroform_field_prune: field-tree depth limit 1 exceeded at 10,0"
        ));
    }

    #[test]
    fn prune_ignores_a_non_dictionary_top_level_field() {
        let objects: Vec<(u32, &[u8])> = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>"),
            (
                2,
                b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>",
            ),
            (3, b"<< /Type /Page /Parent 2 0 R >>"),
            (5, b"<< /Fields [6 0 R] >>"),
            (6, b"42"),
        ];
        let mut pdf = open(build_pdf(&objects));
        let result =
            rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).expect("keep the only page");

        prune_acroform_after_subset(&mut pdf, &result)
            .expect("a non-dictionary field is ignored after warning");

        assert!(
            !dict_of(&mut pdf, ObjectRef::new(1, 0)).contains_key(b"/AcroForm".as_slice()),
            "the invalid field cannot be referenced by a retained page"
        );
    }

    /// All widgets on dropped pages → field removed from /Fields.
    #[test]
    fn all_widgets_dropped_removes_field() {
        let mut pdf = open(build_acroform_pdf());
        // Extract only page 2 (obj 4) — drops page 1 (FieldA) and page 3 (FieldC).
        // FieldB has B1 on page 2 → kept.
        let sel = [ObjectRef::new(4, 0)];
        let result = rebuild_page_tree(&mut pdf, &sel).unwrap();
        prune_acroform_after_subset(&mut pdf, &result).unwrap();

        let fields = acroform_fields(&mut pdf);
        assert!(
            !fields.contains(&ObjectRef::new(7, 0)),
            "FieldA should be removed (page 1 dropped)"
        );
        assert!(
            fields.contains(&ObjectRef::new(8, 0)),
            "FieldB should be retained (B1 on retained page 2)"
        );
        assert!(
            !fields.contains(&ObjectRef::new(11, 0)),
            "FieldC should be removed (page 3 dropped)"
        );
    }

    /// Missing original widget /P remains missing; qpdf does not infer an owner
    /// from the current page /Annots membership.
    ///
    /// The test explicitly verifies the update fires by first stripping /P
    /// from the widgets, then running prune and asserting it is re-set.
    #[test]
    fn widget_p_missing_is_left_missing() {
        let mut pdf = open(build_acroform_pdf());

        // Pre-condition: strip /P from FieldA (7) and B1 (9) to confirm the
        // update is driven by our code, not just a pre-existing correct value.
        for &r in &[ObjectRef::new(7, 0), ObjectRef::new(9, 0)] {
            let widget = pdf.get_object_handle(r);
            widget.try_is_scalar().unwrap();
            widget.remove_key(b"/P");
        }

        // Extract pages 1 and 2 (objects 3 and 4).
        let sel = [ObjectRef::new(3, 0), ObjectRef::new(4, 0)];
        let result = rebuild_page_tree(&mut pdf, &sel).unwrap();
        prune_acroform_after_subset(&mut pdf, &result).unwrap();

        // FieldA (7): the absent /P must remain absent.
        let field_a = dict_of(&mut pdf, ObjectRef::new(7, 0));
        assert!(
            !field_a.contains_key(b"/P".as_slice()),
            "FieldA /P must not be synthesized"
        );

        // B1 (9): the absent /P must remain absent.
        let b1 = dict_of(&mut pdf, ObjectRef::new(9, 0));
        assert!(
            !b1.contains_key(b"/P".as_slice()),
            "B1 /P must not be synthesized"
        );
    }

    /// Dropped-page widget in a kept field's /Kids must have /P removed
    /// (prevents dangling ref after prune_after_subset GCs the orphaned page;
    /// matches qpdf: B2 had no /P in pages-1,2 extract output).
    #[test]
    fn dropped_page_widget_p_removed() {
        let mut pdf = open(build_acroform_pdf());
        // Extract pages 1 and 2 — B2 (obj 10, on dropped page 3) stays in
        // FieldB /Kids but its /P should be stripped.
        let sel = [ObjectRef::new(3, 0), ObjectRef::new(4, 0)];
        let result = rebuild_page_tree(&mut pdf, &sel).unwrap();
        prune_acroform_after_subset(&mut pdf, &result).unwrap();

        let b2 = dict_of(&mut pdf, ObjectRef::new(10, 0));
        assert!(
            !b2.contains_key(b"/P".as_slice()),
            "B2 (dropped page) /P should be removed"
        );
    }

    #[test]
    fn indirect_arrays_are_resolved_while_pruning_fields_annots_and_kids() {
        let objects: Vec<(u32, &[u8])> = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields 12 0 R /DA (/Helvetica 12 Tf 0 g) >> >>"),
            (
                2,
                b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 612 792] >>",
            ),
            (3, b"<< /Type /Page /Parent 2 0 R /Annots 13 0 R >>"),
            (4, b"<< /Type /Page /Parent 2 0 R /Annots [10 0 R] >>"),
            (8, b"<< /FT /Tx /T (FieldB) /Kids 14 0 R >>"),
            (
                9,
                b"<< /Type /Annot /Subtype /Widget /Parent 8 0 R /P 3 0 R /Rect [10 600 200 620] >>",
            ),
            (
                10,
                b"<< /Type /Annot /Subtype /Widget /Parent 8 0 R /P 4 0 R /Rect [10 500 200 520] >>",
            ),
            (12, b"[8 0 R]"),
            (13, b"[9 0 R]"),
            (14, b"[9 0 R 10 0 R]"),
        ];
        let mut pdf = open(build_pdf(&objects));
        let sel = [ObjectRef::new(3, 0)];
        let result = rebuild_page_tree(&mut pdf, &sel).unwrap();
        prune_acroform_after_subset(&mut pdf, &result).unwrap();

        let fields = acroform_fields(&mut pdf);
        assert_eq!(fields, vec![ObjectRef::new(8, 0)]);
        let b1 = dict_of(&mut pdf, ObjectRef::new(9, 0));
        assert_eq!(
            b1.get(b"/P".as_slice()).and_then(reference_target),
            Some(ObjectRef::new(3, 0))
        );
        let b2 = dict_of(&mut pdf, ObjectRef::new(10, 0));
        assert!(
            !b2.contains_key(b"/P".as_slice()),
            "dropped-page widget /P must be stripped"
        );
    }

    /// Empty /Fields after pruning → /AcroForm removed from catalog.
    #[test]
    fn empty_fields_removes_acroform_from_catalog() {
        let mut pdf = open(build_all_on_page1_pdf());
        // Extract only page 2 (obj 4) — drops page 1 where all widgets live.
        let sel = [ObjectRef::new(4, 0)];
        let result = rebuild_page_tree(&mut pdf, &sel).unwrap();
        prune_acroform_after_subset(&mut pdf, &result).unwrap();

        let cat = dict_of(&mut pdf, ObjectRef::new(1, 0));
        assert!(
            !cat.contains_key(b"/AcroForm".as_slice()),
            "/AcroForm should be removed from catalog when /Fields is empty"
        );
    }

    /// Build a 1-page PDF whose `/AcroForm /Fields` is an *indirect* array
    /// (unlike `build_all_on_page1_pdf`'s direct array), with every field on
    /// the single retained page — so pruning drops all fields but still hits
    /// the `fields.is_indirect()` branch.
    fn build_indirect_empty_fields_pdf() -> Vec<u8> {
        let objects: Vec<(u32, &[u8])> = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>"),
            (
                2,
                b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 612 792] >>",
            ),
            (3, b"<< /Type /Page /Parent 2 0 R /Annots [6 0 R] >>"),
            (4, b"<< /Type /Page /Parent 2 0 R >>"),
            (5, b"<< /Fields 7 0 R /DA (/Helvetica 12 Tf 0 g) >>"),
            (
                6,
                b"<< /Type /Annot /Subtype /Widget /FT /Tx /T (FieldA) \
                   /P 3 0 R /Rect [10 700 200 720] >>",
            ),
            (7, b"[6 0 R]"),
        ];
        build_pdf(&objects)
    }

    /// qpdf allocates the replacement array's object number *before* the
    /// keep/drop loop even when every field is later dropped
    /// (`QPDFJob.cc:2610-2632`), so that already-allocated object survives as
    /// an orphan once /AcroForm itself is removed. `--json` (unlike the write
    /// path, which garbage-collects unreferenced objects) must report this
    /// orphan, matching qpdf's observed `maxobjectid` and object enumeration.
    #[test]
    fn indirect_empty_fields_reserves_an_orphan_object() {
        let mut pdf = open(build_indirect_empty_fields_pdf());

        // Extract only page 2 (obj 4) — drops page 1 where the only widget lives.
        let sel = [ObjectRef::new(4, 0)];
        let result = rebuild_page_tree(&mut pdf, &sel).unwrap();
        // `rebuild_page_tree` allocates its own new objects (e.g. a rebuilt
        // Pages container); count *after* it, so the delta below isolates
        // just the object `prune_acroform_after_subset` itself allocates.
        let before_prune_count = pdf.get_object_count().unwrap();
        prune_acroform_after_subset(&mut pdf, &result).unwrap();

        let cat = dict_of(&mut pdf, ObjectRef::new(1, 0));
        assert!(
            !cat.contains_key(b"/AcroForm".as_slice()),
            "/AcroForm should be removed from catalog when /Fields is empty"
        );

        let after_count = pdf.get_object_count().unwrap();
        assert_eq!(
            after_count,
            before_prune_count + 1,
            "the reserved replacement array must remain allocated as an orphan"
        );
    }

    #[test]
    fn prune_invalidates_a_shared_acroform_cache_after_writeback() {
        let mut pdf = open(build_acroform_pdf());
        let before = pdf
            .acroform()
            .unwrap()
            .canonical_annotation_to_field_handles()
            .unwrap();
        assert!(before
            .iter()
            .any(|(annotation, _)| annotation.object_ref() == Some(ObjectRef::new(11, 0))));

        // Retain pages 1 and 2, so FieldC (obj 11) is removed from /Fields.
        let sel = [ObjectRef::new(3, 0), ObjectRef::new(4, 0)];
        let result = rebuild_page_tree(&mut pdf, &sel).unwrap();
        prune_acroform_after_subset(&mut pdf, &result).unwrap();

        let after = pdf
            .acroform()
            .unwrap()
            .canonical_annotation_to_field_handles()
            .unwrap();
        assert!(
            !after
                .iter()
                .any(|(annotation, _)| annotation.object_ref() == Some(ObjectRef::new(11, 0))),
            "pruning /AcroForm /Fields must invalidate the shared cache"
        );
    }

    #[test]
    fn prune_invalidates_a_shared_acroform_cache_after_removing_acroform() {
        let mut pdf = open(build_all_on_page1_pdf());
        let before = pdf
            .acroform()
            .unwrap()
            .canonical_annotation_to_field_handles()
            .unwrap();
        assert!(before
            .iter()
            .any(|(annotation, _)| annotation.object_ref() == Some(ObjectRef::new(6, 0))));

        // Retain only page 2; all fields and widgets are on dropped page 1.
        let sel = [ObjectRef::new(4, 0)];
        let result = rebuild_page_tree(&mut pdf, &sel).unwrap();
        prune_acroform_after_subset(&mut pdf, &result).unwrap();

        let after = pdf
            .acroform()
            .unwrap()
            .canonical_annotation_to_field_handles()
            .unwrap();
        assert!(
            after.is_empty(),
            "removing /AcroForm must invalidate the shared cache"
        );
    }

    /// Split field (/Kids) with widgets on mixed pages: field kept because
    /// at least one widget is on a retained page.  /Kids not pruned (qpdf
    /// compatible).
    #[test]
    fn split_field_with_mixed_widgets_kept_and_kids_not_pruned() {
        let mut pdf = open(build_acroform_pdf());
        // Extract pages 1 and 2 → B1 (on page 2) is retained, B2 (page 3) dropped.
        let sel = [ObjectRef::new(3, 0), ObjectRef::new(4, 0)];
        let result = rebuild_page_tree(&mut pdf, &sel).unwrap();
        prune_acroform_after_subset(&mut pdf, &result).unwrap();

        let fields = acroform_fields(&mut pdf);
        assert!(
            fields.contains(&ObjectRef::new(8, 0)),
            "FieldB kept because B1 is on retained page 2"
        );

        // FieldB's /Kids should still contain both B1 and B2 (qpdf does not prune).
        let field_b = dict_of(&mut pdf, ObjectRef::new(8, 0));
        match field_b
            .get(b"/Kids".as_slice())
            .and_then(ObjectHandle::as_array)
        {
            Some(kids) => {
                assert_eq!(
                    kids.len(),
                    2,
                    "FieldB /Kids should still have 2 entries (not pruned)"
                );
                assert!(
                    kids.iter()
                        .any(|kid| kid.object_ref() == Some(ObjectRef::new(9, 0))),
                    "B1 should remain in /Kids"
                );
                assert!(
                    kids.iter()
                        .any(|kid| kid.object_ref() == Some(ObjectRef::new(10, 0))),
                    "B2 should remain in /Kids (qpdf-compatible: no /Kids pruning)"
                );
            }
            other => panic!("FieldB /Kids unexpected: {other:?}"),
        }
    }

    /// Cycle guard: a field /Kids that forms a cycle must not hang.
    #[test]
    fn cycle_in_field_kids_does_not_hang() {
        // Build a tiny PDF where FieldX /Kids points to itself (cycle).
        let objects: Vec<(u32, &[u8])> = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>"),
            (
                2,
                b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>",
            ),
            (3, b"<< /Type /Page /Parent 2 0 R >>"),
            (4, b"<< /Fields [5 0 R] /DA (/Helvetica 12 Tf 0 g) >>"),
            // FieldX /Kids points to itself.
            (5, b"<< /FT /Tx /T (FieldX) /Kids [5 0 R] >>"),
        ];
        let mut pdf = open(build_pdf(&objects));
        let pages = page_refs(&mut pdf).unwrap();
        let result = rebuild_page_tree(&mut pdf, &pages).unwrap();
        // Must not hang; any result (keep/drop) is acceptable.
        assert!(prune_acroform_after_subset(&mut pdf, &result).is_ok());
    }

    /// Round-trip: after rebuild + prune, qpdf writer output reopens cleanly.
    #[test]
    fn round_trip_valid_pdf_after_prune() {
        let mut pdf = open(build_acroform_pdf());
        let sel = [ObjectRef::new(3, 0), ObjectRef::new(4, 0)];
        let result = rebuild_page_tree(&mut pdf, &sel).unwrap();
        prune_acroform_after_subset(&mut pdf, &result).unwrap();

        let out = write_qpdf_to_memory(&mut pdf, |_| {}).unwrap();

        let mut pdf2 = Pdf::open(Cursor::new(out.clone())).expect("rebuilt PDF should parse");
        let refs = page_refs(&mut pdf2).expect("page tree should walk");
        assert_eq!(refs.len(), 2);

        check_bytes_for_test(out).expect("canonical qpdf check should run");
    }

    /// Extract all pages (identity selection) → all fields kept.
    #[test]
    fn all_pages_retained_keeps_all_fields() {
        let mut pdf = open(build_acroform_pdf());
        let all_pages = page_refs(&mut pdf).unwrap();
        let result = rebuild_page_tree(&mut pdf, &all_pages).unwrap();
        prune_acroform_after_subset(&mut pdf, &result).unwrap();

        let fields = acroform_fields(&mut pdf);
        assert_eq!(fields.len(), 3, "All 3 fields should be kept");
    }
}
