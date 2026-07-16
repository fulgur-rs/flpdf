//! Overlay/underlay annotation copy, mirroring qpdf 11.9.0's
//! `QPDFPageObjectHelper::copyAnnotations` +
//! `QPDFAcroFormDocumentHelper::transformAnnotations` +
//! `QPDFAcroFormDocumentHelper::addAndRenameFormFields`
//! (`libqpdf/QPDFPageObjectHelper.cc:991-1039`,
//! `libqpdf/QPDFAcroFormDocumentHelper.cc:698-1014`,
//! `libqpdf/QPDFAcroFormDocumentHelper.cc:61-110`).
//!
//! Split into two phases so the cross-document copy runs exactly once per
//! source document (unioned with the Form XObject closure), matching qpdf's
//! per-document `copyForeignObject` foreign→local map:
//!
//! 1. [`survey_source_annotations`] — walk the source page's `/Annots` in
//!    source space and return the reachable-ref closure to add to the batch
//!    [`copy_objects`](crate::object_copy::copy_objects) call, alongside a
//!    survey describing the per-annot top-level field and the source
//!    `/AcroForm` inherited entries.
//! 2. [`template_from_survey`] — after the batch copy returns its map, remap
//!    the survey's source-space refs into dest-space refs.
//! 3. [`apply_placement`] — per placement, shallow-duplicate each annotation
//!    (and its field-tree top) so that repeated placements of the same source
//!    page do NOT share (transformation would otherwise cumulate), transform
//!    `/Rect` and each `/AP` stream's `/Matrix` by `cm`, reset field `/DR`
//!    references to the dest AcroForm `/DR`, and append the transformed annots
//!    to the destination page `/Annots`. Returns the list of new top-level
//!    field dest refs added by this placement.
//! 4. [`add_and_rename_form_fields`] — once per destination page after all
//!    placements: BFS the collected new top-level fields, rename `/T` on
//!    fully-qualified-name collision with existing dest fields, and append to
//!    `/AcroForm/Fields`.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

use crate::acroform_document_helper::{collect_reachable_refs, collect_refs_in_object};
use crate::{Error, Object, ObjectRef, Pdf, Result};

/// Bound field-tree /Parent walks (widget → top-level field). Mirrors the
/// `DEFAULT_MAX_ACROFORM_DEPTH` cap used elsewhere in the crate (review rule 4:
/// graph traversals must have a depth cap against hostile input).
const MAX_PARENT_WALK_DEPTH: usize = 100;

/// Survey of a source page's annotation graph, in SOURCE object space,
/// computed BEFORE the cross-document copy.
#[derive(Debug, Default)]
pub(crate) struct AnnotationSurvey {
    /// One entry per source annotation in `/Annots` array order:
    /// `(annot_ref, Option<top_level_field_ref>)`. `None` when the annot is
    /// not a widget or is a widget without an associated form field.
    pub annots: Vec<(ObjectRef, Option<ObjectRef>)>,
    /// Source `/AcroForm/DR` ref, if present.
    pub source_dr: Option<ObjectRef>,
    /// Source `/AcroForm/DA` value (verbatim), if present.
    pub source_default_da: Option<Vec<u8>>,
    /// Source `/AcroForm/Q` integer, if present.
    pub source_default_q: Option<i64>,
}

/// Dest-space refs derived from an [`AnnotationSurvey`] after cross-doc copy;
/// consumed by [`apply_placement`] once per placement.
#[derive(Debug, Default, Clone)]
pub(crate) struct AnnotationCopyTemplate {
    /// Dest-space per-annot pairs, in the same order as the survey.
    pub annots: Vec<(ObjectRef, Option<ObjectRef>)>,
    /// Dest-space source `/DR` ref, if the source had one.
    pub source_dr: Option<ObjectRef>,
    /// Source `/AcroForm/DA` value (verbatim); used by `adjustInheritedFields`
    /// when it differs from the dest's default.
    pub source_default_da: Option<Vec<u8>>,
    /// Source `/AcroForm/Q` integer; same as above.
    pub source_default_q: Option<i64>,
}

/// Walk a source page's `/Annots` and return:
/// - the [`AnnotationSurvey`] (source-space refs and inherited defaults),
/// - the reachable-ref closure to add to a batch
///   [`copy_objects`](crate::object_copy::copy_objects) call so the annots,
///   their fields, appearance streams, and the source `/AcroForm/DR` fonts
///   are copied into the destination in one pass (advisor: one shared
///   foreign→local map per source document prevents duplicated fonts).
///
/// Returns `Ok(None)` when the source page has no `/Annots` array (nothing to
/// copy) — the caller should skip the placement's annotation phase entirely.
///
/// # Errors
///
/// Propagates any error from [`Pdf::resolve`] or from
/// [`collect_reachable_refs`] (excessive graph depth on hostile input).
pub(crate) fn survey_source_annotations<R: Read + Seek>(
    source: &mut Pdf<R>,
    source_page_ref: ObjectRef,
) -> Result<Option<(AnnotationSurvey, BTreeSet<ObjectRef>)>> {
    // 1. Read the page's /Annots (may be inline array or an indirect ref to one).
    let annots_val = {
        let obj = source.resolve_borrowed(source_page_ref)?;
        let Some(dict) = obj.as_dict() else {
            return Ok(None);
        };
        dict.get("Annots").cloned()
    };
    let annots_array = match annots_val {
        None | Some(Object::Null) => return Ok(None),
        Some(Object::Array(arr)) => arr,
        Some(Object::Reference(r)) => match source.resolve(r)? {
            Object::Array(arr) => arr,
            _ => return Ok(None),
        },
        _ => return Ok(None),
    };
    if annots_array.is_empty() {
        return Ok(None);
    }

    // 2. Enumerate annot refs. qpdf materializes direct annots via
    //    `from_qpdf->makeIndirectObject(annot)` so they can be shallow-copied
    //    per placement (transformAnnotations line 954). Materialising the
    //    source is out of scope for now; we skip direct annots and record a
    //    warning-free None. If a real fixture has direct annots the byte gate
    //    will flag it as a missing annot in the output.
    let mut annots: Vec<(ObjectRef, Option<ObjectRef>)> = Vec::new();
    for item in annots_array {
        let annot_ref = match item {
            Object::Reference(r) => r,
            _ => continue,
        };
        let top_field = top_level_field_for_annot(source, annot_ref)?;
        annots.push((annot_ref, top_field));
    }
    if annots.is_empty() {
        return Ok(None);
    }

    // 3. Read source /AcroForm inherited entries. /DA and /Q are the ones
    //    qpdf's transformAnnotations reads for override_da/override_q
    //    (QPDFAcroFormDocumentHelper.cc:737-745). /DR is added to the copy set.
    let (source_dr, source_default_da, source_default_q) = read_source_acroform_defaults(source)?;

    // 4. Build the reachable-ref closure to feed the batch copy_objects call.
    //    Every annot ref is seeded (with /P skipped, since a widget's /P is a
    //    page back-pointer that must not drag the source page into the copy
    //    closure). Every top-level field ref is seeded the same way. The
    //    source /DR value is seeded with /P collection ENABLED because a
    //    resource may legitimately be named /P.
    let mut closure: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut seen: BTreeSet<ObjectRef> = BTreeSet::new();
    for (annot_ref, top_field) in &annots {
        collect_reachable_refs(source, *annot_ref, &mut closure, &mut seen, 0, true)?;
        if let Some(tf) = top_field {
            collect_reachable_refs(source, *tf, &mut closure, &mut seen, 0, true)?;
        }
    }
    if let Some(dr_ref) = source_dr {
        // Same call shape as page_merge / source_field_copy_set for /DR: /P is
        // a resource name inside /DR, not a back-pointer, so skip_parent_key
        // is false.
        collect_refs_in_object(
            source,
            &Object::Reference(dr_ref),
            &mut closure,
            &mut seen,
            0,
            0,
            false,
        )?;
    }

    Ok(Some((
        AnnotationSurvey {
            annots,
            source_dr,
            source_default_da,
            source_default_q,
        },
        closure,
    )))
}

/// Walk `annot_ref`'s `/Parent` chain in source space to find its top-level
/// AcroForm field. Returns `None` when:
/// - `annot_ref` is not a widget (has no `/Subtype /Widget`), OR
/// - the annot is a widget but is not part of the field tree (no `/Parent`
///   AND its own `/T` is absent — it is a "self-field" so it is its own top),
///   in which case we DO return `Some(annot_ref)` when it looks like a field
///   itself (has any of `/T`, `/FT`, `/Kids`), matching qpdf's
///   `getFieldForAnnotation` + `getTopLevelField` composition.
///
/// Cycle-guarded by `visited` and depth-capped by [`MAX_PARENT_WALK_DEPTH`].
fn top_level_field_for_annot<R: Read + Seek>(
    source: &mut Pdf<R>,
    annot_ref: ObjectRef,
) -> Result<Option<ObjectRef>> {
    // Read the annot dict, deciding whether it is a widget and whether it
    // itself is a field.
    let (is_widget, is_field, parent_ref) = {
        let obj = source.resolve_borrowed(annot_ref)?;
        let Some(dict) = obj.as_dict() else {
            return Ok(None);
        };
        let is_widget = matches!(
            dict.get("Subtype"),
            Some(Object::Name(name)) if name.as_slice() == b"Widget"
        );
        let is_field = dict.get("T").is_some()
            || dict.get("FT").is_some()
            || dict.get("Kids").is_some();
        let parent_ref = dict.get_ref("Parent");
        (is_widget, is_field, parent_ref)
    };
    if !is_widget {
        return Ok(None);
    }
    let mut current = if let Some(p) = parent_ref {
        p
    } else if is_field {
        // Widget IS its own field (self-field, no /Parent) — it is the top.
        return Ok(Some(annot_ref));
    } else {
        return Ok(None);
    };
    let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
    visited.insert(annot_ref);
    for _ in 0..MAX_PARENT_WALK_DEPTH {
        if !visited.insert(current) {
            return Ok(None); // /Parent cycle — malformed input
        }
        let parent_of_current = {
            let obj = source.resolve_borrowed(current)?;
            let Some(dict) = obj.as_dict() else {
                return Ok(None);
            };
            dict.get_ref("Parent")
        };
        match parent_of_current {
            Some(p) => current = p,
            None => return Ok(Some(current)),
        }
    }
    Err(Error::Unsupported(format!(
        "AcroForm /Parent chain from {annot_ref} exceeds maximum depth of {MAX_PARENT_WALK_DEPTH}"
    )))
}

/// Read source `/AcroForm/DR`, `/DA`, `/Q` for later use by `apply_placement`
/// (adjustInheritedFields overrides /DA and /Q on each copied field when they
/// differ from the destination's defaults). Returns default (None, None,
/// None) when the source has no `/AcroForm`.
fn read_source_acroform_defaults<R: Read + Seek>(
    source: &mut Pdf<R>,
) -> Result<(Option<ObjectRef>, Option<Vec<u8>>, Option<i64>)> {
    let Some(root_ref) = source.root_ref() else {
        return Ok((None, None, None));
    };
    let acroform_val = {
        let obj = source.resolve_borrowed(root_ref)?;
        let Some(dict) = obj.as_dict() else {
            return Ok((None, None, None));
        };
        dict.get("AcroForm").cloned()
    };
    let acroform_dict = match acroform_val {
        None | Some(Object::Null) => return Ok((None, None, None)),
        Some(Object::Dictionary(d)) => d,
        Some(Object::Reference(r)) => match source.resolve(r)? {
            Object::Dictionary(d) => d,
            _ => return Ok((None, None, None)),
        },
        _ => return Ok((None, None, None)),
    };
    let dr = acroform_dict.get_ref("DR");
    let da = match acroform_dict.get("DA") {
        Some(Object::String(s)) => Some(s.clone()),
        _ => None,
    };
    let q = match acroform_dict.get("Q") {
        Some(Object::Integer(n)) => Some(*n),
        _ => None,
    };
    Ok((dr, da, q))
}

/// Materialize an [`AnnotationSurvey`]'s source-space refs into dest-space
/// refs using the `copy_map` returned by the batch cross-document copy.
///
/// Panics-free by construction: any survey ref that has no entry in `copy_map`
/// is dropped from the template (a widget without a mapped top-level field
/// falls back to `None`). This is a safety net — the survey and the copy
/// closure are built together, so a missing entry indicates a caller error.
///
pub(crate) fn template_from_survey(
    survey: &AnnotationSurvey,
    copy_map: &BTreeMap<ObjectRef, ObjectRef>,
) -> AnnotationCopyTemplate {
    let annots = survey
        .annots
        .iter()
        .filter_map(|(annot_ref, top_field)| {
            // A survey ref that is missing from the map indicates the closure
            // computation missed it; skip rather than panic (defensive).
            let dest_annot = *copy_map.get(annot_ref)?;
            let dest_top_field = top_field.and_then(|tf| copy_map.get(&tf).copied());
            Some((dest_annot, dest_top_field))
        })
        .collect();
    AnnotationCopyTemplate {
        annots,
        source_dr: survey.source_dr.and_then(|r| copy_map.get(&r).copied()),
        source_default_da: survey.source_default_da.clone(),
        source_default_q: survey.source_default_q,
    }
}

/// Per-placement annotation application:
/// - shallow-dup each templated annot (new indirect object) and any
///   associated top-level field / kid path (so repeated placements of the
///   same source page do not share cumulative /Rect transforms);
/// - shallow-dup each `/AP` stream and concatenate `cm` into its `/Matrix`
///   (identity when the stream had no /Matrix);
/// - transform the annot's `/Rect` by `cm`;
/// - if the source top-level field had a `/DR`, replace it with the
///   destination AcroForm `/DR` (lazy-initialized on first placement);
/// - append the dup'd annots to the destination page `/Annots`.
///
/// Returns the newly-added top-level field dest refs (one per distinct top
/// field observed in this placement), to be collected across all placements
/// on the dest page and passed to [`add_and_rename_form_fields`] once at the
/// end.
///
/// TODO(flpdf-9hc.34): implement per-placement dup + cm transform.
pub(crate) fn apply_placement<R: Read + Seek>(
    _dest: &mut Pdf<R>,
    _dest_page_ref: ObjectRef,
    _template: &AnnotationCopyTemplate,
    _cm: [f64; 6],
    _dest_acroform_dr: &mut Option<ObjectRef>,
) -> Result<Vec<ObjectRef>> {
    Ok(Vec::new())
}

/// After every placement on a destination page has been applied, add the
/// collected new top-level fields to the dest `/AcroForm/Fields` with fully
/// qualified name collision renaming (qpdf's `+N` suffix scheme).
///
/// TODO(flpdf-9hc.34): implement BFS rename + append.
pub(crate) fn add_and_rename_form_fields<R: Read + Seek>(
    _dest: &mut Pdf<R>,
    _new_top_fields: Vec<ObjectRef>,
) -> Result<()> {
    Ok(())
}

// Suppress dead-code warnings for the future field access; every helper here
// is called from `crate::overlay` once the wiring is in place.
#[allow(dead_code)]
fn _touch_survey_fields(s: &AnnotationSurvey, t: &AnnotationCopyTemplate) -> Option<Vec<u8>> {
    let _ = (
        &s.annots,
        s.source_dr,
        &s.source_default_da,
        s.source_default_q,
        &t.annots,
        t.source_dr,
        &t.source_default_da,
        t.source_default_q,
    );
    let _: Result<()> = Err(Error::Unsupported("dead-code sink".into()));
    None
}
