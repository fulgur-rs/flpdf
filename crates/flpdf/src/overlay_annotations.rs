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

use crate::{Error, Object, ObjectRef, Pdf, Result};

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
/// TODO(flpdf-9hc.34): implement the survey walk (annots array + top-level
/// field lookup via /Parent chain + /AcroForm inherited entries + reachable
/// closure via crate::acroform_document_helper::collect_reachable_refs).
pub(crate) fn survey_source_annotations<R: Read + Seek>(
    _source: &mut Pdf<R>,
    _source_page_ref: ObjectRef,
) -> Result<Option<(AnnotationSurvey, BTreeSet<ObjectRef>)>> {
    // Placeholder — the plumbing below (OverlaySource carries an
    // Option<AnnotationCopyTemplate>) is designed to be zero-effect until this
    // function returns Some(...). The first PR checkpoint keeps None so the
    // existing 21 byte-gates remain green.
    Ok(None)
}

/// Materialize an [`AnnotationSurvey`]'s source-space refs into dest-space
/// refs using the `copy_map` returned by the batch cross-document copy.
///
/// Panics-free by construction: any survey ref that has no entry in `copy_map`
/// is dropped from the template (a widget without a mapped top-level field
/// falls back to `None`). This is a safety net — the survey and the copy
/// closure are built together, so a missing entry indicates a caller error.
///
/// TODO(flpdf-9hc.34): implement mapping.
pub(crate) fn template_from_survey(
    _survey: &AnnotationSurvey,
    _copy_map: &BTreeMap<ObjectRef, ObjectRef>,
) -> AnnotationCopyTemplate {
    AnnotationCopyTemplate::default()
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
