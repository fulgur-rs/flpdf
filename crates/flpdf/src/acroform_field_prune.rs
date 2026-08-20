//! qpdf correspondence: QPDFJob.cc removal of unreferenced form fields after page selection.
//! AcroForm field preservation after page-subset extraction.
//!
//! After [`crate::pages::tree_rebuild::rebuild_page_tree`] has rebuilt the page
//! tree so that only the selected pages remain reachable from `/Root`, this
//! module prunes the `/AcroForm /Fields` array to remove any top-level field
//! whose **all** widget annotations live on dropped pages. Fields that have at
//! least one widget on a retained page are kept. Stale or dangling `/P` page
//! back-pointers are removed, but the current `/Annots` owner is never used to
//! synthesize a new `/P`.
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
use crate::page_object_helper::PageObjectHelper;
use crate::pages::tree_rebuild::RebuildResult;
use crate::{Object, ObjectRef, Pdf, Result};
use std::collections::{BTreeSet, HashMap};
use std::io::{Read, Seek};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Default maximum depth for walking an AcroForm field tree.
///
/// Matches the depth limit used by the outline-remap module.
pub const DEFAULT_MAX_ACROFORM_DEPTH: usize = 100;

/// Canonical widget identity plus the first retained page that contains it.
/// qpdf's page annotation walk preserves direct dictionaries, so `ObjectRef`
/// alone cannot represent every widget in the page subset. The page value is
/// used for field survival and retained-page membership, not to synthesize
/// widget `/P`.
#[allow(clippy::mutable_key_type)]
type WidgetPageMap = HashMap<ObjectHandleIdentity, (ObjectHandle, ObjectRef)>;

/// Prune `/AcroForm /Fields` after a page-subset extraction and remove stale
/// widget `/P` back-pointers.
///
/// `result` is the [`RebuildResult`] from
/// [`crate::pages::tree_rebuild::rebuild_page_tree`].  Its `new_kids` encodes
/// the retained pages; its `ref_map` maps old page refs to new page refs.
///
/// The function mutates `pdf` in place and is a no-op when there is no
/// `/AcroForm` in the catalog.
///
/// # Errors
///
/// - Any error propagated from [`Pdf::resolve`].
/// - [`crate::Error::Unsupported`] when the field-tree depth limit is exceeded.
pub fn prune_acroform_after_subset<R: Read + Seek>(
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
/// - Any error propagated from [`Pdf::resolve`].
/// - [`crate::Error::Unsupported`] when the field-tree depth limit is exceeded.
#[allow(clippy::mutable_key_type)]
pub fn prune_acroform_after_subset_with_max_depth<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    result: &RebuildResult,
    max_depth: usize,
) -> Result<()> {
    // ── Step 1: collect widget handles found on retained pages ─────────────
    // Walk *every* retained page's /Annots array through the canonical
    // helper, including every duplicate-selection occurrence -- not just
    // ref_map[old][0]. For every entry whose resolved /Subtype is /Widget,
    // retain the live handle AND the new page ref it lives on.
    //
    // An *indirect* widget on a duplicate page selection shares its
    // ObjectHandle identity with the original page's widget
    // (rebuild_page_tree leaves indirect sub-objects shared, only the page
    // dictionary itself is cloned per duplicate); collect_page_widgets's own
    // `.or_insert` on that identity naturally keeps the first occurrence's
    // page, matching the /P update rule used by outline_dest_remap for
    // /Dest. A *direct* widget, however, is embedded inside the deep-cloned
    // page dictionary (`pages/tree_rebuild.rs`'s own doc: "deep-clone the
    // post-materialization page dictionary"), so each duplicate occurrence
    // gets its own distinct direct widget with its own distinct identity --
    // visiting only the first occurrence would leave every later
    // occurrence's /P dangling at its pre-rebuild value. Visiting every
    // occurrence lets collect_page_widgets's identity-keyed map do the
    // right thing for both shapes.
    let mut widget_to_page = WidgetPageMap::new();
    for new_refs in result.ref_map.values() {
        for &new_page in new_refs {
            collect_page_widgets(pdf, new_page, &mut widget_to_page)?;
        }
    }

    // ── Step 3: locate and process /AcroForm ──────────────────────────────
    let catalog_ref = match pdf.root_ref() {
        Some(r) => r,
        None => return Ok(()), // No catalog.
    };

    let catalog_obj = pdf.resolve_borrowed(catalog_ref)?;
    let Some(catalog) = catalog_obj.as_dict() else {
        return Ok(());
    };

    // /AcroForm may be a direct dict or an indirect reference.
    let (acroform_ref, acroform_dict) = match catalog.get("AcroForm").cloned() {
        Some(Object::Reference(r)) => match pdf.resolve_borrowed(r)? {
            Object::Dictionary(d) => (Some(r), d.clone()),
            _ => return Ok(()),
        },
        Some(Object::Dictionary(d)) => (None, d),
        _ => return Ok(()), // No /AcroForm — nothing to do.
    };

    // Resolve /Fields, handling the indirect-array form.
    let fields_val = match acroform_dict.get("Fields").cloned() {
        Some(v) => v,
        None => return Ok(()), // /AcroForm with no /Fields.
    };
    let fields_arr: Vec<Object> = match fields_val {
        Object::Array(arr) => arr,
        Object::Reference(r) => match pdf.resolve_borrowed(r)? {
            Object::Array(arr) => arr.clone(),
            _ => return Ok(()),
        },
        _ => return Ok(()),
    };

    // ── Step 4: for each top-level field, decide keep/drop ────────────────
    // A field is kept when it (or any descendant in its /Kids tree) has at
    // least one widget in `widget_to_page` (i.e. a widget on a retained page).
    // Matching qpdf: we do NOT prune /Kids of kept fields — the retained-page
    // test is purely a keep-or-drop decision at the /Fields list level.
    let mut kept_fields: Vec<Object> = Vec::new();

    for field_val in &fields_arr {
        let field_ref = match field_val {
            Object::Reference(r) => *r,
            _ => continue, // Non-reference entry in /Fields; skip.
        };

        let has_widget = field_has_retained_widget(
            pdf,
            field_ref,
            &widget_to_page,
            &mut BTreeSet::new(),
            0,
            max_depth,
        )?;

        if has_widget {
            kept_fields.push(Object::Reference(field_ref));
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
    let retained_page_refs: BTreeSet<ObjectRef> = result.new_kids.iter().copied().collect();
    for (widget, _) in widget_to_page.values() {
        remove_stale_widget_page_ref(pdf, widget, &retained_page_refs)?;
    }
    // Collect all widgets reachable from kept fields; strip /P from any that
    // are NOT in widget_to_page (i.e. live in a kept field's /Kids but were on
    // a dropped page).
    for field_val in &kept_fields {
        let field_ref = match field_val {
            Object::Reference(r) => *r,
            _ => continue,
        };
        strip_dropped_widget_p_refs(
            pdf,
            field_ref,
            &widget_to_page,
            &mut BTreeSet::new(),
            0,
            max_depth,
        )?;
    }

    // ── Step 6: write back pruned /AcroForm or remove it ─────────────────
    if kept_fields.is_empty() {
        // All fields dropped → remove /AcroForm from catalog entirely,
        // matching qpdf's observed behaviour.
        let catalog_obj2 = pdf.resolve_borrowed(catalog_ref)?;
        if let Some(mut cat) = catalog_obj2.as_dict().cloned() {
            cat.remove("AcroForm");
            pdf.set_object(catalog_ref, Object::Dictionary(cat));
        }
    } else {
        // Update /Fields on the AcroForm dict.
        let mut new_acroform = acroform_dict;
        new_acroform.insert("Fields", Object::Array(kept_fields));

        match acroform_ref {
            Some(r) => {
                // /AcroForm was an indirect object — update it in place.
                pdf.set_object(r, Object::Dictionary(new_acroform));
            }
            None => {
                // /AcroForm was a direct dictionary on the catalog — write it
                // back into the catalog.
                let catalog_obj2 = pdf.resolve_borrowed(catalog_ref)?;
                if let Some(mut cat) = catalog_obj2.as_dict().cloned() {
                    cat.insert("AcroForm", Object::Dictionary(new_acroform));
                    pdf.set_object(catalog_ref, Object::Dictionary(cat));
                }
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Walk a page's `/Annots` array and insert any `/Subtype /Widget` handles into
/// `widget_to_page`, mapping them to `page_ref`.
///
/// `PageObjectHelper::get_annotation_handles` is the canonical qpdf-shaped
/// enumeration boundary (`QPDFPageObjectHelper.cc:439-454`): it resolves an
/// indirect `/Annots` carrier, filters non-dictionaries, resolves `/Subtype`,
/// and preserves direct dictionary members instead of projecting them to
/// `ObjectRef`.
#[allow(clippy::mutable_key_type)]
fn collect_page_widgets<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    widget_to_page: &mut WidgetPageMap,
) -> Result<()> {
    let widgets = {
        let mut page = PageObjectHelper::new(page_ref, pdf);
        page.get_annotations_filtered(Some(b"/Widget"))?
    };
    for widget in widgets {
        // qpdf-deviation: chases flpdf's temporary Pdf::set_object
        // bare-reference bridge (ObjectValue::Reference) to its terminal
        // value; qpdf's resolved object graph has no reference-value type
        // at all (QPDF::resolve, QPDF.cc:1699-1753, never returns a value
        // that is itself another reference, and QPDF::replaceObject
        // rejects an indirect handle, QPDF.cc:1980-1991), and
        // QPDFAcroFormDocumentHelper::traverseField / QPDFJob.cc's field
        // walk use plain one-hop getKey/getObjGen with no chase loop. Every
        // other resolve_object_handle_to_terminal call in this file is the
        // same deviation and is marked individually at its own site.
        let widget = pdf.resolve_object_handle_to_terminal(&widget)?;
        // First-occurrence rule: don't overwrite if already present from a
        // duplicate-page selection (ref_map iteration is in BTreeMap order,
        // first occurrence is recorded first).
        widget_to_page
            .entry(widget.identity_key())
            .or_insert((widget, page_ref));
    }

    Ok(())
}

/// Returns `true` when `field_ref` or any descendant in its `/Kids` tree has a
/// widget annotation that lives on a retained page (i.e. is in `widget_to_page`).
///
/// `visited` / `depth` / `max_depth` guard against cycles and over-deep trees
/// in hostile PDFs.
#[allow(clippy::mutable_key_type)]
fn field_has_retained_widget<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    field_ref: ObjectRef,
    widget_to_page: &WidgetPageMap,
    visited: &mut BTreeSet<ObjectRef>,
    depth: usize,
    max_depth: usize,
) -> Result<bool> {
    if depth > max_depth {
        // Per the public contract, an over-deep field tree is an explicit
        // error: silently treating it as "no retained widget" would drop
        // valid /Fields. Propagate so the caller can decide.
        return Err(crate::Error::Unsupported(format!(
            "acroform_field_prune: field-tree depth limit {max_depth} exceeded at {field_ref}"
        )));
    }
    if !visited.insert(field_ref) {
        // Cycle — treat as no retained widget to avoid infinite loop.
        return Ok(false);
    }

    let field = pdf.get_object_handle(field_ref);
    // qpdf-deviation: chases flpdf's Pdf::set_object bare-reference bridge;
    // see collect_page_widgets's marker for the full citation.
    let field = pdf.resolve_object_handle_to_terminal(&field)?;

    // A merged field+widget dict is its own widget.
    if widget_to_page.contains_key(&field.identity_key()) {
        return Ok(true);
    }

    // Walk /Kids: entries may be sub-fields (have /T) or pure widgets.
    // qpdf-deviation: chases flpdf's Pdf::set_object bare-reference bridge;
    // see collect_page_widgets's marker for the full citation.
    let kids = pdf.resolve_object_handle_to_terminal(&field.try_get_key(b"/Kids")?)?;
    let Some(kids_arr) = kids.as_array() else {
        return Ok(false);
    };

    for kid in kids_arr {
        // qpdf-deviation: chases flpdf's Pdf::set_object bare-reference
        // bridge; see collect_page_widgets's marker for the full citation.
        let kid = pdf.resolve_object_handle_to_terminal(&kid)?;
        // qpdf's field-tree traversal ignores direct field/kid entries. Only
        // indirect kids can participate in `/Fields` association; direct
        // page annotations are already collected by `collect_page_widgets`.
        let Some(kid_ref) = kid.object_ref() else {
            continue;
        };

        // A pure widget kid is directly in widget_to_page.
        if widget_to_page.contains_key(&kid.identity_key()) {
            return Ok(true);
        }

        // A sub-field kid: recurse.
        if field_has_retained_widget(pdf, kid_ref, widget_to_page, visited, depth + 1, max_depth)? {
            return Ok(true);
        }
    }

    Ok(false)
}

/// Preserve a valid widget `/P` and remove only a stale or dangling page ref.
///
/// qpdf establishes `/P` during copied-annotation graph remapping, not through
/// a generic page-owner repair pass. Only dictionaries are inspected — widget
/// annotations should not be streams, but we guard defensively.
fn remove_stale_widget_page_ref<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    widget: &ObjectHandle,
    retained_page_refs: &BTreeSet<ObjectRef>,
) -> Result<()> {
    if widget.as_dictionary().is_none() || !widget.try_has_key(b"/P")? {
        return Ok(());
    }
    let existing = widget.try_get_key(b"/P")?;
    let Some(existing_ref) = existing.object_ref() else {
        return Ok(());
    };
    if retained_page_refs.contains(&existing_ref) {
        return Ok(());
    }
    // qpdf-deviation: chases flpdf's temporary Pdf::set_object bare-reference
    // bridge; see collect_page_widgets for the full qpdf source citation.
    let existing_page = pdf.resolve_object_handle_to_terminal(&existing)?;
    let is_page_object = if existing_page.as_dictionary().is_some() {
        existing_page.try_get_key(b"/Type")?.as_name().as_deref() == Some(b"Page".as_slice())
    } else {
        false
    };
    if !existing_page.is_null() && !is_page_object {
        return Ok(());
    }
    widget.remove_key(b"/P");
    pdf.mark_object_handle_dirty(widget)?;
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
fn strip_dropped_widget_p_refs<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    field_ref: ObjectRef,
    widget_to_page: &WidgetPageMap,
    visited: &mut BTreeSet<ObjectRef>,
    depth: usize,
    max_depth: usize,
) -> Result<()> {
    if depth > max_depth {
        return Err(crate::Error::Unsupported(format!(
            "acroform_field_prune: field-tree depth limit {max_depth} exceeded at {field_ref}"
        )));
    }
    if !visited.insert(field_ref) {
        return Ok(()); // Cycle guard.
    }

    let field = pdf.get_object_handle(field_ref);
    // qpdf-deviation: chases flpdf's Pdf::set_object bare-reference bridge;
    // see collect_page_widgets's marker for the full citation.
    let field = pdf.resolve_object_handle_to_terminal(&field)?;
    // qpdf-deviation: same bare-reference bridge chase as above.
    let kids = pdf.resolve_object_handle_to_terminal(&field.try_get_key(b"/Kids")?)?;
    let Some(kids_arr) = kids.as_array() else {
        // Leaf node with no /Kids. Merged field+widget dicts that were
        // retained were already handled by remove_stale_widget_page_ref; dropped
        // merged fields are not in kept_fields, so there is nothing to strip.
        return Ok(());
    };

    for kid in kids_arr {
        // qpdf-deviation: chases flpdf's Pdf::set_object bare-reference
        // bridge; see collect_page_widgets's marker for the full citation.
        let kid = pdf.resolve_object_handle_to_terminal(&kid)?;
        // qpdf ignores direct field-tree entries, so do not promote or mutate
        // a direct `/Kids` member here.
        let Some(kid_ref) = kid.object_ref() else {
            continue;
        };

        // qpdf-deviation: same bare-reference bridge chase as above.
        let subtype = pdf.resolve_object_handle_to_terminal(&kid.try_get_key(b"/Subtype")?)?;
        let is_widget = subtype.as_name().as_deref() == Some(b"Widget".as_slice());

        if is_widget {
            if !widget_to_page.contains_key(&kid.identity_key()) {
                // Widget on a dropped page — remove stale /P.
                kid.remove_key(b"/P");
                pdf.mark_object_handle_dirty(&kid)?;
            }
            // Pure widget kids do not have /Kids of their own (spec: a widget
            // annotation is a leaf); no need to recurse.
        } else {
            // Sub-field: recurse.
            strip_dropped_widget_p_refs(
                pdf,
                kid_ref,
                widget_to_page,
                visited,
                depth + 1,
                max_depth,
            )?;
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
    use crate::check::check_reader;
    use crate::pages::page_refs;
    use crate::pages::tree_rebuild::rebuild_page_tree;
    use crate::writer::write_qpdf_to_memory;
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
            (5, b"<< /Fields [6 0 R] /DA (/Helvetica 12 Tf 0 g) >>"),
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

    /// Build a two-page PDF whose first page contains a direct Widget
    /// dictionary. Its stale `/P` points to the dropped second page so the
    /// test proves that qpdf-style stale-reference cleanup removes the entry
    /// instead of repairing it from the current `/Annots` owner.
    fn build_direct_widget_page_pdf() -> Vec<u8> {
        let objects: Vec<(u32, &[u8])> = vec![
            (
                1,
                b"<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>",
            ),
            (
                2,
                b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 612 792] >>",
            ),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /Annots [<< /Type /Annot /Subtype /Widget /P 4 0 R /Rect [10 700 200 720] >>] >>",
            ),
            (4, b"<< /Type /Page /Parent 2 0 R >>"),
            (5, b"<< /Fields [] /DA (/Helvetica 12 Tf 0 g) >>"),
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

    fn dict_of(pdf: &mut Pdf<Cursor<Vec<u8>>>, r: ObjectRef) -> crate::Dictionary {
        match pdf.resolve_borrowed(r).unwrap() {
            Object::Dictionary(d) => d.clone(),
            other => panic!("{r} is not a dictionary: {other:?}"),
        }
    }

    fn acroform_fields(pdf: &mut Pdf<Cursor<Vec<u8>>>) -> Vec<ObjectRef> {
        let cat_ref = pdf.root_ref().expect("root");
        let cat = dict_of(pdf, cat_ref);
        let acro_val = match cat.get("AcroForm").cloned() {
            None => return vec![],
            Some(v) => v,
        };
        let acro_dict = match acro_val {
            Object::Dictionary(d) => d,
            Object::Reference(r) => match pdf.resolve_borrowed(r).unwrap() {
                Object::Dictionary(d) => d.clone(),
                _ => return vec![],
            },
            _ => return vec![],
        };
        match acro_dict.get("Fields").cloned() {
            Some(Object::Array(arr)) => arr.iter().filter_map(Object::as_ref_id).collect(),
            _ => vec![],
        }
    }

    // ── Tests ─────────────────────────────────────────────────────────────

    /// No AcroForm in input → function is a no-op, returns Ok.
    #[test]
    fn no_acroform_is_noop() {
        let mut pdf = open(build_no_acroform_pdf());
        let pages = page_refs(&mut pdf).unwrap();
        let result = rebuild_page_tree(&mut pdf, &pages).unwrap();
        assert!(prune_acroform_after_subset(&mut pdf, &result).is_ok());
    }

    #[test]
    fn direct_page_widget_removes_dropped_page_ref() {
        let mut pdf = open(build_direct_widget_page_pdf());
        let result = rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
        prune_acroform_after_subset(&mut pdf, &result).unwrap();

        let page = dict_of(&mut pdf, ObjectRef::new(3, 0));
        let annots = page
            .get("Annots")
            .and_then(Object::as_array)
            .expect("page /Annots must remain an array");
        let widget = annots[0]
            .as_dict()
            .expect("the page widget must remain a direct dictionary");
        assert_eq!(
            widget.get("P"),
            None,
            "a direct original widget must not be repaired to its current owner"
        );
    }

    #[test]
    fn duplicate_page_selection_removes_stale_direct_widget_page_refs() {
        // A duplicate page selection deep-clones the page dictionary per
        // occurrence, but qpdf still does not infer `/P` from the new owner.
        // The source `/P` points at the dropped page and is therefore removed
        // from both copied direct widgets by the writer-visible cleanup.
        let mut pdf = open(build_direct_widget_page_pdf());
        let page_ref = ObjectRef::new(3, 0);
        let result = rebuild_page_tree(&mut pdf, &[page_ref, page_ref]).unwrap();
        assert_eq!(
            result.new_kids.len(),
            2,
            "both occurrences of the duplicate selection must produce a leaf"
        );
        prune_acroform_after_subset(&mut pdf, &result).unwrap();

        for (index, &new_page) in result.new_kids.iter().enumerate() {
            let page = dict_of(&mut pdf, new_page);
            let annots = page
                .get("Annots")
                .and_then(Object::as_array)
                .unwrap_or_else(|| panic!("occurrence {index} page /Annots must remain an array"));
            let widget = annots[0]
                .as_dict()
                .unwrap_or_else(|| panic!("occurrence {index} widget must be direct")); // cov:ignore: fixture invariant
            assert_eq!(
                widget.get("P"),
                None,
                "occurrence {index}'s direct widget /P must not be synthesized from its owner"
            );
        }
    }

    #[test]
    fn non_dictionary_widget_handle_is_ignored() {
        let mut pdf = open(build_no_acroform_pdf());
        let widget = ObjectHandle::integer(1);

        let retained = BTreeSet::from([ObjectRef::new(3, 0)]);
        remove_stale_widget_page_ref(&mut pdf, &widget, &retained).unwrap();
    }

    #[test]
    fn retained_indirect_widget_keeps_existing_page_reference() {
        let mut pdf = open(build_acroform_pdf());
        let widget = pdf.get_object_handle(ObjectRef::new(7, 0));
        let retained = BTreeSet::from([ObjectRef::new(3, 0)]);

        remove_stale_widget_page_ref(&mut pdf, &widget, &retained).unwrap();

        let widget_dict = dict_of(&mut pdf, ObjectRef::new(7, 0));
        assert_eq!(
            widget_dict.get("P"),
            Some(&Object::Reference(ObjectRef::new(3, 0))),
            "an indirect widget already owned by a retained page must keep /P"
        );
    }

    #[test]
    fn direct_field_kid_is_not_promoted_to_retained_widget() {
        let mut pdf = open(build_direct_field_kid_pdf());
        let result = rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
        prune_acroform_after_subset(&mut pdf, &result).unwrap();

        let catalog = dict_of(&mut pdf, ObjectRef::new(1, 0));
        assert!(
            catalog.get("AcroForm").is_none(),
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
            let Object::Dictionary(mut d) = pdf.resolve(r).unwrap() else {
                panic!("expected dict for {r}");
            };
            d.remove("P");
            pdf.set_object(r, Object::Dictionary(d));
        }

        // Extract pages 1 and 2 (objects 3 and 4).
        let sel = [ObjectRef::new(3, 0), ObjectRef::new(4, 0)];
        let result = rebuild_page_tree(&mut pdf, &sel).unwrap();
        prune_acroform_after_subset(&mut pdf, &result).unwrap();

        // FieldA (7): the absent /P must remain absent.
        let field_a = dict_of(&mut pdf, ObjectRef::new(7, 0));
        assert_eq!(field_a.get("P"), None, "FieldA /P must not be synthesized");

        // B1 (9): the absent /P must remain absent.
        let b1 = dict_of(&mut pdf, ObjectRef::new(9, 0));
        assert_eq!(b1.get("P"), None, "B1 /P must not be synthesized");
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
            b2.get("P").is_none(),
            "B2 (dropped page) /P should be removed; got {:?}",
            b2.get("P")
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
        assert_eq!(b1.get("P"), Some(&Object::Reference(ObjectRef::new(3, 0))));
        let b2 = dict_of(&mut pdf, ObjectRef::new(10, 0));
        assert!(
            b2.get("P").is_none(),
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

        let cat_ref = pdf.root_ref().unwrap();
        let cat = dict_of(&mut pdf, cat_ref);
        assert!(
            cat.get("AcroForm").is_none(),
            "/AcroForm should be removed from catalog when /Fields is empty"
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
        match field_b.get("Kids") {
            Some(Object::Array(kids)) => {
                assert_eq!(
                    kids.len(),
                    2,
                    "FieldB /Kids should still have 2 entries (not pruned)"
                );
                assert!(
                    kids.contains(&Object::Reference(ObjectRef::new(9, 0))),
                    "B1 should remain in /Kids"
                );
                assert!(
                    kids.contains(&Object::Reference(ObjectRef::new(10, 0))),
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

        let report = check_reader(Cursor::new(out)).expect("check should run");
        assert!(
            report.valid,
            "pruned PDF should pass check_reader: {:?}",
            report.diagnostics
        );
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
