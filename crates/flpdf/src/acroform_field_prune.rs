//! AcroForm field preservation after page-subset extraction.
//!
//! After [`crate::page_tree_rebuild::rebuild_page_tree`] has rebuilt the page
//! tree so that only the selected pages remain reachable from `/Root`, this
//! module prunes the `/AcroForm /Fields` array to remove any top-level field
//! whose **all** widget annotations live on dropped pages.  Fields that have at
//! least one widget on a retained page are kept, and the `/P` (page
//! back-pointer) on each retained widget is updated to the new page
//! `ObjectRef`.
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
//!   - FieldA's `/P` is updated to the new page-1 object ref.
//!   - B1's `/P` is updated to the new page-2 object ref.
//!   - B2 has no `/P` (it was on the dropped page; qpdf leaves it without one).
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
//!   - Widget `/P` is updated for widgets on retained pages.
//!   - Widget `/P` is **removed** for widgets in kept fields' `/Kids` whose
//!     page was dropped, preventing dangling refs after GC (matching qpdf:
//!     B2 had no `/P` in the pages-1,2 extract output).
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
//! the comment in `page_tree_rebuild` for the same rationale.
//!
//! Heavy AcroForm operations (flattening, rendering appearance streams) are out
//! of scope; this module handles only the
//! extract-time field/widget survival filter and `/P` back-pointer repair.

use crate::page_tree_rebuild::RebuildResult;
use crate::{Object, ObjectRef, Pdf, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Default maximum depth for walking an AcroForm field tree.
///
/// Matches the depth limit used by the outline-remap module.
pub const DEFAULT_MAX_ACROFORM_DEPTH: usize = 100;

/// Prune `/AcroForm /Fields` after a page-subset extraction and repair widget
/// `/P` back-pointers.
///
/// `result` is the [`RebuildResult`] from
/// [`crate::page_tree_rebuild::rebuild_page_tree`].  Its `new_kids` encodes
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
pub fn prune_acroform_after_subset_with_max_depth<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    result: &RebuildResult,
    max_depth: usize,
) -> Result<()> {
    // ── Step 1: collect widget ObjectRefs found on retained pages ─────────
    // Walk each retained page's /Annots array.  For every entry whose
    // resolved /Subtype is /Widget, record the widget ref AND the new page
    // ref it lives on (first-occurrence rule: ref_map[old][0], matching the
    // /P update rule used by outline_dest_remap for /Dest).
    //
    // Note: widgets added by *duplicate* page selections share their
    // ObjectRef with the original page's widget (rebuild_page_tree clones
    // only the *page dictionary*, not sub-objects like annotation dicts).
    let mut widget_to_page: BTreeMap<ObjectRef, ObjectRef> = BTreeMap::new();
    for (&_old_page, new_refs) in &result.ref_map {
        let Some(&new_page) = new_refs.first() else {
            continue;
        };
        collect_page_widgets(pdf, new_page, &mut widget_to_page)?;
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

    // ── Step 5: update /P on retained widgets; strip /P from dropped widgets ─
    // For each widget on a retained page, set /P to the (new) page ObjectRef,
    // matching observed qpdf behaviour for both merged field+widget objects
    // and pure-widget objects in /Kids.
    //
    // For dropped-page widgets that remain in a kept field's /Kids (qpdf does
    // not prune /Kids), we must *remove* /P so the widget does not hold a
    // dangling reference to the orphaned page dict after prune_after_subset
    // GCs it (qpdf 11.9.0 observed: B2 had no /P in pages-1,2 output).
    for (&widget_ref, &new_page_ref) in &widget_to_page {
        update_widget_page_ref(pdf, widget_ref, new_page_ref)?;
    }
    // Collect all widget refs reachable from kept fields; strip /P from any
    // that are NOT in widget_to_page (i.e. live in a kept field's /Kids but
    // were on a dropped page).
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

/// Walk a page's `/Annots` array and insert any `/Subtype /Widget` entries
/// into `widget_to_page`, mapping them to `page_ref`.
///
/// Handles the indirect-array form of `/Annots` (some PDFs store `/Annots` as
/// an indirect reference to an array object).
fn collect_page_widgets<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    widget_to_page: &mut BTreeMap<ObjectRef, ObjectRef>,
) -> Result<()> {
    let page_obj = pdf.resolve_borrowed(page_ref)?;
    let Some(page_dict) = page_obj.as_dict() else {
        return Ok(());
    };

    let annots_val = match page_dict.get("Annots").cloned() {
        Some(v) => v,
        None => return Ok(()),
    };

    let annots_arr: Vec<Object> = match annots_val {
        Object::Array(arr) => arr,
        Object::Reference(r) => match pdf.resolve_borrowed(r)? {
            Object::Array(arr) => arr.clone(),
            _ => return Ok(()),
        },
        _ => return Ok(()),
    };

    for annot_val in &annots_arr {
        let annot_ref = match annot_val {
            Object::Reference(r) => *r,
            _ => continue,
        };

        let annot_obj = pdf.resolve_borrowed(annot_ref)?;
        let Some(annot_dict) = annot_obj.as_dict() else {
            continue;
        };

        let is_widget = matches!(
            annot_dict.get("Subtype"),
            Some(Object::Name(n)) if n.as_slice() == b"Widget"
        );
        if is_widget {
            // First-occurrence rule: don't overwrite if already present from a
            // duplicate-page selection (ref_map iteration is in BTreeMap order,
            // first occurrence is recorded first).
            widget_to_page.entry(annot_ref).or_insert(page_ref);
        }
    }

    Ok(())
}

/// Returns `true` when `field_ref` or any descendant in its `/Kids` tree has a
/// widget annotation that lives on a retained page (i.e. is in `widget_to_page`).
///
/// `visited` / `depth` / `max_depth` guard against cycles and over-deep trees
/// in hostile PDFs.
fn field_has_retained_widget<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    field_ref: ObjectRef,
    widget_to_page: &BTreeMap<ObjectRef, ObjectRef>,
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

    // A merged field+widget dict is its own widget.
    if widget_to_page.contains_key(&field_ref) {
        return Ok(true);
    }

    let field_obj = pdf.resolve_borrowed(field_ref)?;
    let Some(field_dict) = field_obj.as_dict() else {
        return Ok(false);
    };

    // Walk /Kids: entries may be sub-fields (have /T) or pure widgets.
    let kids_val = match field_dict.get("Kids").cloned() {
        Some(v) => v,
        None => return Ok(false),
    };

    let kids_arr: Vec<Object> = match kids_val {
        Object::Array(arr) => arr,
        Object::Reference(r) => match pdf.resolve_borrowed(r)? {
            Object::Array(arr) => arr.clone(),
            _ => return Ok(false),
        },
        _ => return Ok(false),
    };

    for kid_val in &kids_arr {
        let kid_ref = match kid_val {
            Object::Reference(r) => *r,
            _ => continue,
        };

        // A pure widget kid is directly in widget_to_page.
        if widget_to_page.contains_key(&kid_ref) {
            return Ok(true);
        }

        // A sub-field kid: recurse.
        if field_has_retained_widget(pdf, kid_ref, widget_to_page, visited, depth + 1, max_depth)? {
            return Ok(true);
        }
    }

    Ok(false)
}

/// Set `/P` on `widget_ref` to `new_page_ref`.
///
/// Only updates dictionaries — streams are left unchanged (widget annotations
/// should not be streams, but we guard defensively).
fn update_widget_page_ref<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    widget_ref: ObjectRef,
    new_page_ref: ObjectRef,
) -> Result<()> {
    let widget_obj = pdf.resolve_borrowed(widget_ref)?;
    if let Some(mut dict) = widget_obj.as_dict().cloned() {
        dict.insert("P", Object::Reference(new_page_ref));
        pdf.set_object(widget_ref, Object::Dictionary(dict));
    }
    Ok(())
}

/// Walk a kept field's `/Kids` tree and remove `/P` from any widget that is
/// **not** in `widget_to_page` (i.e. its page was dropped).  This prevents
/// dangling indirect references after `prune_after_subset` GCs the orphaned
/// page objects, matching qpdf's observed output (B2 had no `/P` in the
/// pages-1,2 extraction result).
///
/// `visited` / `depth` / `max_depth` guard against cycles and over-deep trees.
fn strip_dropped_widget_p_refs<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    field_ref: ObjectRef,
    widget_to_page: &BTreeMap<ObjectRef, ObjectRef>,
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

    let field_obj = pdf.resolve_borrowed(field_ref)?;
    let Some(field_dict) = field_obj.as_dict() else {
        return Ok(());
    };

    let kids_val = match field_dict.get("Kids").cloned() {
        Some(v) => v,
        None => {
            // Leaf node with no /Kids. If it is a widget dict not in
            // widget_to_page, remove /P (it was on a dropped page).
            // Merged field+widget dicts also have /Subtype /Widget; they are
            // already handled by update_widget_page_ref for retained ones.
            // For dropped ones, field_has_retained_widget returns false so the
            // field is not in kept_fields at all — we don't reach here for them.
            return Ok(());
        }
    };

    let kids_arr: Vec<Object> = match kids_val {
        Object::Array(arr) => arr,
        Object::Reference(r) => match pdf.resolve_borrowed(r)? {
            Object::Array(arr) => arr.clone(),
            _ => return Ok(()),
        },
        _ => return Ok(()),
    };

    for kid_val in &kids_arr {
        let kid_ref = match kid_val {
            Object::Reference(r) => *r,
            _ => continue,
        };

        // Resolve the kid to check if it is a widget dict.
        let kid_obj = pdf.resolve_borrowed(kid_ref)?;
        let Some(kid_dict) = kid_obj.as_dict() else {
            continue;
        };

        let is_widget = matches!(
            kid_dict.get("Subtype"),
            Some(Object::Name(n)) if n.as_slice() == b"Widget"
        );

        if is_widget {
            if !widget_to_page.contains_key(&kid_ref) {
                // Widget on a dropped page — remove stale /P.
                let kid_obj2 = pdf.resolve_borrowed(kid_ref)?;
                if let Some(mut d) = kid_obj2.as_dict().cloned() {
                    d.remove("P");
                    pdf.set_object(kid_ref, Object::Dictionary(d));
                }
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
    use crate::page_tree_rebuild::rebuild_page_tree;
    use crate::pages::page_refs;
    use crate::writer::write_pdf;
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

    /// Widget /P updated to new page ref for retained-page widgets.
    ///
    /// The test explicitly verifies the update fires by first stripping /P
    /// from the widgets, then running prune and asserting it is re-set.
    #[test]
    fn widget_p_updated_to_new_page_ref() {
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

        // FieldA (7): /P should be set to page-1 ref 3 0 R.
        let field_a = dict_of(&mut pdf, ObjectRef::new(7, 0));
        assert_eq!(
            field_a.get("P"),
            Some(&Object::Reference(ObjectRef::new(3, 0))),
            "FieldA /P should be set to page-1 ref"
        );

        // B1 (9): /P should be set to page-2 ref 4 0 R.
        let b1 = dict_of(&mut pdf, ObjectRef::new(9, 0));
        assert_eq!(
            b1.get("P"),
            Some(&Object::Reference(ObjectRef::new(4, 0))),
            "B1 /P should be set to page-2 ref"
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

    /// Round-trip: after rebuild + prune, write_pdf and reopen, check_reader clean.
    #[test]
    fn round_trip_valid_pdf_after_prune() {
        let mut pdf = open(build_acroform_pdf());
        let sel = [ObjectRef::new(3, 0), ObjectRef::new(4, 0)];
        let result = rebuild_page_tree(&mut pdf, &sel).unwrap();
        prune_acroform_after_subset(&mut pdf, &result).unwrap();

        let mut out: Vec<u8> = Vec::new();
        write_pdf(&mut pdf, &mut out).unwrap();

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
