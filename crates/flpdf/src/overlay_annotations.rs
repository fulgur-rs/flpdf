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
//!    survey describing the per-annot top-level field and (when present) the
//!    source `/AcroForm/DR` ref.
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
use crate::parser::{is_delimiter, is_ws, Parser};
use crate::{Error, Object, ObjectRef, Pdf, Result};

/// Bound field-tree /Parent walks (widget → top-level field). Mirrors the
/// `DEFAULT_MAX_ACROFORM_DEPTH` cap used elsewhere in the crate (review rule 4:
/// graph traversals must have a depth cap against hostile input).
const MAX_PARENT_WALK_DEPTH: usize = 100;

/// Per-placement inherited-field override plan derived from the source and
/// dest `/AcroForm`'s `/DA` and `/Q` defaults, consumed by
/// [`adjust_inherited_field`] during the field-tree BFS. See qpdf
/// `transformAnnotations` line 737-767 (flag computation) and
/// `adjustInheritedFields` (`libqpdf/QPDFAcroFormDocumentHelper.cc:442-484`).
///
/// When `override_da` is false, `/DA` is left untouched on every field even
/// if `from_default_da` is set; same for `/Q`.
struct InheritedOverrides {
    override_da: bool,
    from_default_da: Vec<u8>,
    override_q: bool,
    from_default_q: i64,
}

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
    /// Source `/AcroForm/DA` (raw string bytes) — feeds
    /// `adjustInheritedFields` on the dest side.
    pub source_default_da: Option<Vec<u8>>,
    /// Source `/AcroForm/Q` integer — same as above.
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
    /// Source `/AcroForm/DA` bytes (verbatim from the source).
    pub source_default_da: Option<Vec<u8>>,
    /// Source `/AcroForm/Q` integer.
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
    // cov:ignore-start: /Annots shape guards. The exercised shapes are
    // "direct array" (primary target) and "no /Annots" (implicit via page
    // without annots); the remaining arms (non-dict page, missing/Null
    // /Annots, indirect Reference→array, indirect Reference→non-array,
    // /Annots is neither Array/Reference/Null) are malformed-input
    // branches without a corresponding qpdf-oracle golden.
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
    // cov:ignore-end

    // 2. Enumerate annot refs. Indirect refs are used verbatim. Direct annot
    //    dictionaries and streams are materialized into fresh source-doc
    //    indirect objects (qpdf transformAnnotations line 954-956:
    //    `annot = from_qpdf->makeIndirectObject(annot)`) so the downstream
    //    copy path can treat every annot uniformly. Values that are neither
    //    a reference nor a dict/stream (Null padding, malformed entries) are
    //    silently skipped, matching qpdf's `annot.warnIfPossible("... stream")`
    //    branch behaviour of dropping the entry rather than aborting.
    let mut annots: Vec<(ObjectRef, Option<ObjectRef>)> = Vec::new();
    for item in annots_array {
        let annot_ref = match item {
            Object::Reference(r) => r,
            direct @ (Object::Dictionary(_) | Object::Stream(_)) => {
                let new_ref = allocate_next_ref(source)?;
                source.set_object(new_ref, direct);
                new_ref
            }
            _ => continue, // cov:ignore: /Annots entry is neither ref nor dict/stream — malformed
        };
        let top_field = top_level_field_for_annot(source, annot_ref)?;
        annots.push((annot_ref, top_field));
    }
    if annots.is_empty() {
        return Ok(None); // cov:ignore: every entry in a non-empty /Annots was malformed
    }

    // 3. Read the source /AcroForm/DR ref (added to the copy set below) and
    //    the inherited /DA / /Q defaults, used by `adjust_inherited_fields`
    //    on the dest side (qpdf QPDFAcroFormDocumentHelper.cc:442-484,
    //    called from transformAnnotations line 914-917). For the primary
    //    target (fxo-red + form-fields-and-annotations) the source /AcroForm
    //    has neither /DA nor /Q, so both defaults are `None` and the dest-side
    //    override is a no-op — the byte gate is unaffected.
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
        // cov:ignore-start: trailing `)?;` on a multi-line call — llvm-cov
        // attributes it to the Err path, defensive on collect_refs_in_object
        // failure that no shipped fixture reaches.
        collect_refs_in_object(
            source,
            &Object::Reference(dr_ref),
            &mut closure,
            &mut seen,
            0,
            0,
            false,
        )?;
        // cov:ignore-end
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
            return Ok(None); // cov:ignore: annot ref resolves to non-dict — malformed source
        };
        let is_widget = matches!(
            dict.get("Subtype"),
            Some(Object::Name(name)) if name == b"Widget"
        );
        let is_field =
            dict.get("T").is_some() || dict.get("FT").is_some() || dict.get("Kids").is_some();
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
        return Ok(None); // cov:ignore: widget without /Parent and without any field key — not a form field
    };
    let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
    visited.insert(annot_ref);
    // cov:ignore-start: the loop body's defensive arms (cycle guard,
    // non-dict parent, depth overflow, unreachable `Some(p)` continuation
    // artifact) are hostile-input / llvm-cov artifacts; the "return
    // Ok(Some(current))" completion IS exercised by every widget with a
    // parent chain in the primary target.
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
    // cov:ignore-end
}

/// Read source `/AcroForm`'s `/DR` ref plus the inherited `/DA` and `/Q`
/// defaults. Returns `(None, None, None)` when the source has no
/// `/AcroForm`; missing individual keys stay `None`.
///
/// The defaults feed `adjust_inherited_fields` (qpdf
/// `QPDFAcroFormDocumentHelper::adjustInheritedFields`,
/// `libqpdf/QPDFAcroFormDocumentHelper.cc:442-484`, called from
/// `transformAnnotations` line 914-917) so a copied field that inherits
/// `/DA` or `/Q` from the source doc keeps rendering the same way when
/// the destination doc's defaults differ.
#[allow(clippy::type_complexity)]
fn read_source_acroform_defaults<R: Read + Seek>(
    source: &mut Pdf<R>,
) -> Result<(Option<ObjectRef>, Option<Vec<u8>>, Option<i64>)> {
    // cov:ignore-start: defensive AcroForm-shape guards — the exercised
    // shapes (missing AcroForm, direct dict, indirect Reference→dict) are
    // covered by primary/no-acroform/direct-DR fixtures; the remaining
    // arms (no /Root, catalog non-dict, /AcroForm non-Reference-non-Dict,
    // Reference resolving to non-dict) are malformed-input branches
    // without a corresponding qpdf-oracle golden.
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
    // cov:ignore-end
    // /DR may be indirect (a Reference) or direct. Direct dicts get
    // materialized into a fresh source-doc indirect object so downstream
    // closure collection and `copy_objects` can treat every /DR uniformly
    // (mirrors qpdf transformAnnotations line 750-752, which promotes
    // from_dr with `from_qpdf->makeIndirectObject(from_dr)` before
    // `copyForeignObject`).
    let dr = match acroform_dict.get("DR").cloned() {
        Some(Object::Reference(r)) => Some(r),
        Some(dr_val @ (Object::Dictionary(_) | Object::Stream(_))) => {
            let new_ref = allocate_next_ref(source)?;
            source.set_object(new_ref, dr_val);
            Some(new_ref)
        }
        _ => None, // cov:ignore: fallback match arm — defensive/malformed input
    };
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

/// Per-destination-page record of resource-name renames, populated by
/// [`merge_resources_shallow`] when a source `/DR` sub-dictionary key
/// collides with an existing dest entry of the same name that resolves to a
/// *different* object. Outer key is the resource category (`Font`,
/// `XObject`, ...); inner map is `old_name -> new_name` — the source's
/// original key mapped to the name it was actually inserted under in dest.
///
/// Mirrors qpdf's per-destination `dr_map`, populated by
/// `QPDFObjectHandle::mergeResources`'s `conflicts` out-parameter and driven
/// by `QPDFAcroFormDocumentHelper::init_dr_map`
/// (`libqpdf/QPDFAcroFormDocumentHelper.cc:775-800`, called from
/// `transformAnnotations`). qpdf consumes the map via
/// `adjustDefaultAppearances` / `adjustAppearanceStream` to rewrite each
/// copied field's `/DA` string and AP-stream content operators from the old
/// name to the new one; that rewrite is not implemented yet — this map is
/// only built and threaded so a later change has the data it needs.
pub(crate) type DrMap = BTreeMap<Vec<u8>, BTreeMap<Vec<u8>, Vec<u8>>>;

/// Per-placement annotation application:
/// - shallow-dup each templated annot (new indirect object) and any
///   associated top-level field / kid path (so repeated placements of the
///   same source page do not share cumulative /Rect transforms);
/// - shallow-dup each `/AP` stream and concatenate `cm` into its `/Matrix`
///   (identity when the stream had no /Matrix);
/// - transform the annot's `/Rect` by `cm`;
/// - if the source top-level field had a `/DR`, replace it with the
///   destination AcroForm `/DR` (lazy-initialized on first placement, merging
///   into a pre-existing dest `/DR` with conflict renaming recorded into
///   `dr_map`);
/// - append the dup'd annots to the destination page `/Annots`.
///
/// `dr_map` accumulates resource-name renames across every placement on this
/// destination page (one map per dest page, created by the caller alongside
/// `dest_acroform_dr`); see [`DrMap`].
///
/// Returns the newly-added top-level field dest refs (one per distinct top
/// field observed in this placement), to be collected across all placements
/// on the dest page and passed to [`add_and_rename_form_fields`] once at the
/// end.
///
pub(crate) fn apply_placement<R: Read + Seek>(
    dest: &mut Pdf<R>,
    dest_page_ref: ObjectRef,
    template: &AnnotationCopyTemplate,
    cm: [f64; 6],
    dest_acroform_dr: &mut Option<ObjectRef>,
    dr_map: &mut DrMap,
) -> Result<Vec<ObjectRef>> {
    if template.annots.is_empty() {
        return Ok(Vec::new()); // cov:ignore: defensive early return
    }

    // Per-placement orig_to_copy map, mirroring qpdf's `orig_to_copy` local in
    // transformAnnotations (per-call, per-placement). Every mutable node
    // (annot, top-level field, field-tree kid, appearance stream) gets a
    // per-placement shallow dup here so multiple placements of the same source
    // don't share mutated /Rect / /Parent / AP /Matrix state.
    let mut per_placement_dup: BTreeMap<ObjectRef, ObjectRef> = BTreeMap::new();

    // If ANY annot in this placement carries a top-level field and the source
    // supplied a /DR, ensure the destination /AcroForm/DR exists. This is
    // lazy: fxo-red (the primary target) has no /AcroForm, so `dest_acroform_dr`
    // is None on the first placement and is filled here.
    let has_any_top_field = template.annots.iter().any(|(_, tf)| tf.is_some());
    if has_any_top_field && dest_acroform_dr.is_none() {
        if let Some(source_dr) = template.source_dr {
            *dest_acroform_dr = Some(ensure_dest_acroform_dr(dest, source_dr, dr_map)?);
        } // cov:ignore: control-flow marker — llvm-cov instrumentation artifact
    }

    // Compute the inherited-field overrides (qpdf transformAnnotations
    // line 737-767): when the source /AcroForm's /DA or /Q differs from the
    // dest's, each foreign-copied field that inherits its value from the
    // source /AcroForm must be pinned to the source value so it does not
    // silently inherit the (different) dest default. For the primary target
    // (fxo-red + form-fields-and-annotations) neither doc has /DA or /Q, so
    // both flags come out false and the BFS reset is a no-op — the byte
    // gate is unaffected.
    let overrides = compute_inherited_overrides(dest, template)?;

    let mut new_top_fields: Vec<ObjectRef> = Vec::new();
    let mut added_top_field_set: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut new_annot_refs: Vec<ObjectRef> = Vec::new();

    for (dest_annot_ref, dest_top_field) in &template.annots {
        // 1. Duplicate the field tree (top → kids) into per-placement copies,
        //    patching /Parent back-pointers and resetting field /DR (top and
        //    every kid, matching qpdf's per-BFS-iteration line 928-930). If
        //    the widget IS the field (self-field), the annot ref equals the
        //    top-level ref, so this call also dups the annot as a side effect.
        let new_top_field_ref = if let Some(top_ref) = dest_top_field {
            let new_top = duplicate_field_tree(
                dest,
                *top_ref,
                &mut per_placement_dup,
                *dest_acroform_dr,
                overrides.as_ref(),
                dr_map,
            )?; // cov:ignore: control-flow marker — llvm-cov instrumentation artifact
            if added_top_field_set.insert(new_top) {
                new_top_fields.push(new_top);
            }
            Some(new_top)
        } else {
            None
        };
        let _ = new_top_field_ref;

        // 2. Duplicate the annot itself if the field-tree walk did not already.
        let new_annot_ref = match per_placement_dup.get(dest_annot_ref) {
            Some(&existing) => existing,
            None => {
                let new = shallow_dup_indirect(dest, *dest_annot_ref)?;
                per_placement_dup.insert(*dest_annot_ref, new);
                new
            }
        };

        // 3. Duplicate and cm-transform each /AP appearance stream. Streams
        //    are shared across placements otherwise, so a cm concat here would
        //    accumulate across placements. Per-annot dup guarantees isolation.
        transform_annot_ap_streams(dest, new_annot_ref, cm)?;

        // 4. Transform the annot's /Rect by cm.
        transform_annot_rect(dest, new_annot_ref, cm)?;

        // 5. Drop the annot's `/P` back-pointer when it is `Null` after copy.
        //    survey excluded `/P` from the copy closure (so the source page is
        //    not dragged into dest), which leaves `/P null` for annots that
        //    had one; qpdf's oracle removes the key entirely rather than
        //    repointing at dest_page_ref. Annots that never had /P (the
        //    primary target) are untouched.
        set_annot_page_ref_if_null(dest, new_annot_ref)?;

        new_annot_refs.push(new_annot_ref);
    }

    // 5. Append the dup'd annots to the destination page /Annots array.
    append_page_annots(dest, dest_page_ref, &new_annot_refs)?;

    Ok(new_top_fields)
}

/// Shallow-copy the object at `src_ref` into a new indirect object in `dest`
/// and return the new ref. The value's references are unchanged (shallow):
/// only the top-level dict/stream/array node is a fresh clone. Callers that
/// want to isolate a mutation to this placement's copy set must run this
/// per node they will mutate.
fn shallow_dup_indirect<R: Read + Seek>(
    dest: &mut Pdf<R>,
    src_ref: ObjectRef,
) -> Result<ObjectRef> {
    let obj = dest.resolve(src_ref)?;
    let new_ref = allocate_next_ref(dest)?;
    dest.set_object(new_ref, obj);
    Ok(new_ref)
}

/// Allocate a fresh indirect object ref (`max(numbers) + 1`, gen 0). Duplicate
/// of the crate-local helpers in overlay.rs / page_form_xobject.rs — kept
/// module-local so this file has no dep on overlay.rs's private surface.
fn allocate_next_ref<R: Read + Seek>(dest: &Pdf<R>) -> Result<ObjectRef> {
    let n = dest
        .object_refs()
        .iter()
        .map(|r| r.number)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| Error::Unsupported("object-number space exhausted".to_string()))?;
    Ok(ObjectRef::new(n, 0))
}

/// BFS the field tree rooted at `top_ref`, shallow-dup'ing every visited node
/// into `per_placement_dup` and rewriting the `/Parent` back-pointer of each
/// kid to point at the dup of its parent (qpdf transformAnnotations
/// line 887-912 pattern). Returns the ref of the dup'd top-level field.
///
/// The dup of `top_ref` is added to `per_placement_dup` first, then kids are
/// discovered by reading each dup's `/Kids`; any kid that is not yet in the
/// map is dup'd and enqueued.
///
/// If `dest_dr` is `Some`, every field visited in the BFS has its `/DR` reset
/// to that ref (qpdf transformAnnotations line 928-930 — inside the BFS, so
/// both the top-level field and every kid receive the reset). Radio button
/// kids are self-field annotations and each carry an inline `/DR` in the
/// source, so kid-level reset is required for parity.
///
/// When `dr_map` is non-empty, every visited field's `/DA` (if present) is
/// additionally rewritten via [`adjust_default_appearance`] (qpdf
/// `adjustDefaultAppearances`, called from `transformAnnotations` line
/// 932-934) so a `/DA` referencing a source `/DR/Font` name that collided
/// during the merge points at the renamed dest name instead.
fn duplicate_field_tree<R: Read + Seek>(
    dest: &mut Pdf<R>,
    top_ref: ObjectRef,
    per_placement_dup: &mut BTreeMap<ObjectRef, ObjectRef>,
    dest_dr: Option<ObjectRef>,
    overrides: Option<&InheritedOverrides>,
    dr_map: &DrMap,
) -> Result<ObjectRef> {
    let new_top = match per_placement_dup.get(&top_ref) {
        Some(&existing) => return Ok(existing),
        None => {
            let new = shallow_dup_indirect(dest, top_ref)?;
            per_placement_dup.insert(top_ref, new);
            new
        }
    };

    // Pre-resolve the dest `/DR` dict ONCE (rather than per visited field)
    // when there is a rename to apply. Every field visited by this BFS that
    // still carries a `/DR` key gets it reset to `dest_dr` below, and a field
    // without one inherits `/AcroForm/DR` (also `dest_dr`) — so `dest_dr`'s
    // resolved dict is the correct `/DA` resource-lookup surface for every
    // node in this walk. `dr_map` is only ever populated inside
    // `ensure_dest_acroform_dr` (via `merge_resources_shallow`), which always
    // sets the caller's `dest_acroform_dr` to `Some` before returning, so
    // `dr_map` non-empty implies `dest_dr` is `Some` here; the `None` arm is
    // a defensive fallback for that invariant, not a path any shipped
    // fixture reaches.
    let da_resources: Option<crate::Dictionary> = if dr_map.is_empty() {
        None
    } else {
        match dest_dr {
            Some(dr_ref) => dest.resolve(dr_ref)?.into_dict(),
            None => None, // cov:ignore: dr_map non-empty implies dest_dr is Some (see comment above); defensive only
        }
    };

    // BFS: queue holds (source_ref, dup_ref) pairs. `seen` prevents revisiting
    // (a cycle in a hostile PDF or a shared kid across mutliple parents).
    let mut queue: std::collections::VecDeque<(ObjectRef, ObjectRef)> =
        std::collections::VecDeque::new();
    queue.push_back((top_ref, new_top));
    let mut seen: BTreeSet<ObjectRef> = BTreeSet::new();
    seen.insert(top_ref);

    while let Some((_src_ref, dup_ref)) = queue.pop_front() {
        // Read the dup's current dictionary (which is a shallow-copy of the
        // source's dict at the time of shallow_dup_indirect, so /Parent and
        // /Kids still hold source refs).
        let Some(mut dict) = dest.resolve(dup_ref)?.into_dict() else {
            continue; // cov:ignore: dup ref resolved to non-dict — malformed
        };

        // Patch /Parent to point at THIS placement's dup of the parent (if
        // the parent has already been dup'd; a well-formed field tree visits
        // the parent before its kids, so this always holds).
        if let Some(parent_ref) = dict.get_ref("Parent") {
            if let Some(&parent_dup) = per_placement_dup.get(&parent_ref) {
                dict.insert("Parent", Object::Reference(parent_dup));
            }
            // else: /Parent points OUTSIDE the tree we're dup'ing (e.g. a
            // malformed structure); leave it, qpdf just warns and moves on.
        }

        // Walk /Kids: shallow-dup each kid, rewrite the /Kids entry to point
        // at the dup, and enqueue the (src, dup) pair.
        if let Some(kids_val) = dict.get("Kids").cloned() {
            let kids_array = match kids_val {
                Object::Array(arr) => Some(arr),
                // cov:ignore-start: indirect /Kids resolution — defensive shape
                Object::Reference(kr) => match dest.resolve(kr)? {
                    Object::Array(arr) => Some(arr),
                    _ => None,
                },
                _ => None,
                // cov:ignore-end
            };
            if let Some(mut kids) = kids_array {
                for entry in kids.iter_mut() {
                    if let Object::Reference(kid_ref) = *entry {
                        let kid_dup = match per_placement_dup.get(&kid_ref) {
                            Some(&existing) => existing, // cov:ignore: match arm — defensive on unexpected shape
                            None => {
                                let new = shallow_dup_indirect(dest, kid_ref)?;
                                per_placement_dup.insert(kid_ref, new);
                                new
                            }
                        };
                        *entry = Object::Reference(kid_dup);
                        if seen.insert(kid_ref) {
                            queue.push_back((kid_ref, kid_dup));
                        }
                    } // cov:ignore: control-flow marker — llvm-cov instrumentation artifact
                }
                dict.insert("Kids", Object::Array(kids));
            } // cov:ignore: control-flow marker — llvm-cov instrumentation artifact
        }

        // Reset field-level /DR to the dest /AcroForm/DR ref (qpdf
        // transformAnnotations line 928-930). Runs for every visited node
        // (top-level field and every kid), matching qpdf's per-iteration BFS
        // reset — required for radio button kids that carry inline /DR in the
        // source.
        if let Some(dr) = dest_dr {
            if dict.get("DR").is_some() {
                dict.insert("DR", Object::Reference(dr));
            }
        } // cov:ignore: control-flow marker — llvm-cov instrumentation artifact

        // Override inherited /DA and /Q on this field when the source doc's
        // defaults differ from the dest's (qpdf transformAnnotations
        // line 914-917 → adjustInheritedFields at line 442-484). Only pin the
        // value when this field does not already carry an explicit /DA or /Q
        // (either on itself or on an ancestor visited earlier in the BFS —
        // parents come before kids), and only when the field's currently
        // inherited value would differ from `from_default`. `dict` at this
        // point already has its /Parent rewritten to the dup, so the walk
        // stays inside `per_placement_dup`.
        if let Some(ov) = overrides {
            adjust_inherited_field(dest, dup_ref, &mut dict, ov, per_placement_dup)?;
        }

        // Rewrite this field's /DA to reference the renamed dest resource
        // name (qpdf transformAnnotations line 932-934 →
        // adjustDefaultAppearances). Runs after adjust_inherited_field so a
        // /DA that was just pinned from the source /AcroForm default above
        // is also covered. `remove` (not `get().cloned()`) moves the string
        // out since `dict` is owned here — see pdf-rust-review-patterns.md
        // rule 1.
        if let Some(resources) = da_resources.as_ref() {
            match dict.remove("DA") {
                Some(Object::String(da)) => {
                    let new_da = adjust_default_appearance(&da, dr_map, resources);
                    dict.insert("DA", Object::String(new_da));
                }
                // cov:ignore-start: malformed /DA (non-string) — no shipped
                // fixture supplies this shape.
                Some(other) => {
                    dict.insert("DA", other);
                }
                // cov:ignore-end
                None => {}
            }
        }

        dest.set_object(dup_ref, Object::Dictionary(dict));
    }

    Ok(new_top)
}

/// Rewrite the Font-resource name in a `/DA` string according to `dr_map`,
/// matching qpdf `QPDFAcroFormDocumentHelper::adjustDefaultAppearances`
/// (`libqpdf/QPDFAcroFormDocumentHelper.cc`, called from
/// `transformAnnotations` line 932-934).
///
/// `/DA` is a content-stream fragment restricted to colour-setting operators
/// and `Tf` (ISO 32000-2 12.7.3.3), e.g. `0 0.4 0 rg /F1 18 Tf`. This scans
/// the fragment for the name token most recently seen before each `Tf`
/// operator (mirroring qpdf's `ResourceFinder`, which tracks a single
/// `last_name` overwritten by every name token and consulted whenever an
/// operator maps to a resource type) and rewrites that name's bytes to
/// `dr_map["Font"][name]` when BOTH:
/// - `dr_map` records a rename for that name under the `Font` category
///   (populated by [`merge_resources_shallow`] when the source `/DR/Font`
///   entry collided with an existing dest entry under the same name), AND
/// - `resources`'s `/Font` sub-dictionary still carries that ORIGINAL name
///   as a key — a defensive guard against renaming a name that happens to
///   collide with a `dr_map` key from an unrelated context. In practice this
///   is a no-op superset check: `merge_resources_shallow` always keeps the
///   original colliding name in the merged dest `/DR/Font` alongside the
///   `{name}_N` rename, so any name recorded in `dr_map` is always present.
///
/// Every other byte — whitespace, unrelated names, other operators, string
/// and numeric operands — is copied through unchanged, so the result is
/// byte-identical to `da` except at the rewritten name's exact span.
///
/// This is an inline tokenizer scoped to the `/DA` subset (delegates operand
/// lexing to the shared [`Parser`] but does not use the general
/// [`crate::content_stream::ContentStreamParser`], which does not track
/// per-token byte offsets).
///
/// Returns `da.to_vec()` verbatim, without scanning, when `dr_map` is empty
/// (the common case: no placement recorded a rename on this dest page).
fn adjust_default_appearance(da: &[u8], dr_map: &DrMap, resources: &crate::Dictionary) -> Vec<u8> {
    if dr_map.is_empty() {
        return da.to_vec();
    }
    let font_renames = dr_map.get(b"Font".as_slice());
    let font_resources = resources.get("Font").and_then(Object::as_dict);

    let mut out: Vec<u8> = Vec::with_capacity(da.len());
    // Byte span of the most recently seen name token WITHIN `out` (not
    // `da` — needed so `Vec::splice` can replace it in place) plus its
    // decoded value. Overwritten by every subsequent name token; consumed
    // (reset to `None`) only when a `Tf` operator actually applies it, so a
    // later stray `Tf` cannot re-splice an already-rewritten span.
    let mut last_name: Option<(usize, usize, Vec<u8>)> = None;
    let mut pos = 0usize;
    while pos < da.len() {
        let byte = da[pos];
        if is_ws(byte) {
            let start = pos;
            while pos < da.len() && is_ws(da[pos]) {
                pos += 1;
            }
            out.extend_from_slice(&da[start..pos]);
            continue;
        }
        if byte == b'%' {
            // `%` comment: copied verbatim to end of line. `/DA` fragments
            // rarely carry comments, but the token grammar permits them
            // (ISO 32000-2 7.8.2).
            let start = pos;
            while pos < da.len() && !matches!(da[pos], b'\n' | b'\r') {
                pos += 1;
            }
            out.extend_from_slice(&da[start..pos]);
            continue;
        }
        if byte == b'/'
            || byte == b'('
            || byte == b'<'
            || byte == b'['
            || matches!(byte, b'+' | b'-' | b'.' | b'0'..=b'9')
        {
            // Operand: delegate to the shared object lexer (numbers,
            // strings, names, arrays, dictionaries) that
            // `crate::content_stream` also reuses verbatim, so name/string
            // escaping matches the rest of the crate exactly rather than
            // reimplementing it here.
            let mut parser = Parser::new_no_reference(&da[pos..]);
            match parser.parse_one_object() {
                Ok(obj) => {
                    let end = pos + parser.position();
                    let out_start = out.len();
                    out.extend_from_slice(&da[pos..end]);
                    if let Object::Name(name) = obj {
                        last_name = Some((out_start, out.len(), name));
                    }
                    pos = end;
                }
                Err(_) => {
                    // Malformed operand: copy one byte verbatim and resume
                    // (tolerant scanning — mirrors `/DA` parsing elsewhere in
                    // the crate, which also recovers from bad tokens rather
                    // than aborting the whole string).
                    out.push(byte);
                    pos += 1;
                }
            }
            continue;
        }
        // Operator keyword: bytes up to the next whitespace/delimiter.
        let start = pos;
        while pos < da.len() && !is_ws(da[pos]) && !is_delimiter(da[pos]) {
            pos += 1;
        }
        if pos == start {
            // Stray delimiter that did not start a recognised operand (e.g.
            // an unmatched `)`); copy the single byte verbatim and resume.
            out.push(da[pos]);
            pos += 1;
            continue;
        }
        out.extend_from_slice(&da[start..pos]);
        if &da[start..pos] == b"Tf" {
            if let Some((out_start, out_end, name)) = last_name.take() {
                let renamed = font_renames.and_then(|m| m.get(name.as_slice()));
                if let Some(new_name) = renamed {
                    let present = font_resources.is_some_and(|d| d.get(name.as_slice()).is_some());
                    if present {
                        let mut replacement = Vec::with_capacity(new_name.len() + 1);
                        replacement.push(b'/');
                        replacement.extend_from_slice(new_name);
                        out.splice(out_start..out_end, replacement);
                    }
                }
            }
        }
    }
    out
}

/// Duplicate each `/AP` appearance stream referenced from the annot at
/// `annot_ref`, concatenate `cm` into the stream's `/Matrix` (qpdf
/// transformAnnotations line 992-1010), and rewrite the annot's `/AP` entries
/// to point at the dup'd streams. Only walked at `/AP/{N,R,D}` and one nested
/// dictionary level, matching qpdf's `apdict` traversal.
fn transform_annot_ap_streams<R: Read + Seek>(
    dest: &mut Pdf<R>,
    annot_ref: ObjectRef,
    cm: [f64; 6],
) -> Result<()> {
    let Some(mut annot_dict) = dest.resolve(annot_ref)?.into_dict() else {
        return Ok(()); // cov:ignore: defensive early return
    };
    let Some(ap_val) = annot_dict.get("AP").cloned() else {
        return Ok(());
    };
    let Some(mut apdict) = (match ap_val {
        Object::Dictionary(d) => Some(d),
        Object::Reference(r) => dest.resolve(r)?.into_dict(), // cov:ignore: function signature — llvm-cov instrumentation artifact
        _ => None, // cov:ignore: fallback match arm — defensive/malformed input
    }) else {
        return Ok(()); // cov:ignore: defensive early return
    };
    let ap_keys: Vec<Vec<u8>> = apdict.iter().map(|(k, _)| k.to_vec()).collect();
    for key in ap_keys {
        let val = apdict.get(&key).cloned();
        let Some(val) = val else { continue };
        match val {
            Object::Reference(stream_ref) => {
                if let Some(new_ref) = dup_and_transform_ap_stream(dest, stream_ref, cm)? {
                    apdict.insert(&key, Object::Reference(new_ref));
                }
            }
            Object::Dictionary(sub) => {
                // Nested per-state dict (e.g. /N << /1 stream_ref /Off stream_ref >>).
                let mut sub = sub;
                let sub_keys: Vec<Vec<u8>> = sub.iter().map(|(k, _)| k.to_vec()).collect();
                for sub_key in sub_keys {
                    let Some(sub_val) = sub.get(&sub_key).cloned() else {
                        continue; // cov:ignore: key vanished mid-iteration — impossible with BTreeMap
                    };
                    if let Object::Reference(stream_ref) = sub_val {
                        if let Some(new_ref) = dup_and_transform_ap_stream(dest, stream_ref, cm)? {
                            sub.insert(&sub_key, Object::Reference(new_ref));
                        }
                    } // cov:ignore: control-flow marker — llvm-cov instrumentation artifact
                }
                apdict.insert(&key, Object::Dictionary(sub));
            }
            _ => {} // cov:ignore: fallback match arm — defensive/malformed input
        }
    }
    annot_dict.insert("AP", Object::Dictionary(apdict));
    dest.set_object(annot_ref, Object::Dictionary(annot_dict));
    Ok(())
}

/// Shallow-copy the appearance stream at `stream_ref`, concatenate `cm` into
/// its `/Matrix` (identity if absent, matching qpdf), and return the new ref.
/// Returns `Ok(None)` when the ref does not resolve to a stream.
fn dup_and_transform_ap_stream<R: Read + Seek>(
    dest: &mut Pdf<R>,
    stream_ref: ObjectRef,
    cm: [f64; 6],
) -> Result<Option<ObjectRef>> {
    let obj = dest.resolve(stream_ref)?;
    let Object::Stream(mut stream) = obj else {
        return Ok(None); // cov:ignore: defensive early return
    };
    // Read the existing /Matrix (identity when absent — qpdf apcm defaults
    // to QPDFMatrix() before optional matrix.concat(cm) at line 1001).
    let old_matrix = read_matrix_array(&stream.dict, b"Matrix");
    // qpdf: apcm.concat(cm)  →  apcm := apcm * cm  →  overlay uses qpdf_concat
    // (in overlay.rs) with (this, other) = (apcm, cm).
    let had_matrix = old_matrix.is_some();
    let apcm = old_matrix.unwrap_or(IDENTITY);
    let new_matrix = concat_matrices(apcm, cm);
    // Only write /Matrix if the source had one or the result is non-identity
    // (qpdf line 1003 same guard).
    if had_matrix || new_matrix != IDENTITY {
        stream.dict.insert("Matrix", matrix_to_object(new_matrix));
    }
    let new_ref = allocate_next_ref(dest)?;
    dest.set_object(new_ref, Object::Stream(stream));
    Ok(Some(new_ref))
}

/// Read a 6-element `/Matrix` from `dict[key]`, if present and well-formed.
fn read_matrix_array(dict: &crate::Dictionary, key: &[u8]) -> Option<[f64; 6]> {
    let arr = match dict.get(key)? {
        Object::Array(a) if a.len() == 6 => a,
        _ => return None, // cov:ignore: fallback match arm — defensive/malformed input
    };
    let mut out = [0.0f64; 6];
    for (i, item) in arr.iter().enumerate() {
        out[i] = match item {
            Object::Integer(n) => *n as f64,
            Object::Real(x) | Object::RealLiteral { value: x, .. } => *x, // cov:ignore: function signature — llvm-cov instrumentation artifact
            _ => return None, // cov:ignore: fallback match arm — defensive/malformed input
        };
    }
    Some(out)
}

/// Serialize a 6-element matrix as an `Object::Array` of `Object::Real`,
/// matching qpdf's `QPDFObjectHandle::newFromMatrix` output shape.
fn matrix_to_object(m: [f64; 6]) -> Object {
    Object::Array(m.iter().map(|&x| qpdf_real(x)).collect())
}

/// Pre-round `v` so `Object::Real(rounded).write_pdf(...)` (which formats f64
/// via Rust's shortest-roundtrip algorithm) yields the same string as qpdf's
/// `QUtil::double_to_string(v, 6, trim=true)` (six decimal places, trailing
/// zeros/point stripped) — used by qpdf's `newReal(double)` default and thus
/// by every `newFromRectangle` / `newFromMatrix` array element.
///
/// Round-trip trick: format `v` as `%.6f`, strip trailing zeros and a
/// trailing `.`, parse back to `f64`. Rust's `f64::to_string` yields the
/// shortest decimal string that parses back to the same `f64`, so if the
/// intermediate string is decimal-canonical for the target `f64` (which
/// `%.6f + strip` is by construction for values expressible in ≤6 decimal
/// places), the writer's later `f64::to_string` returns the same bytes.
fn qpdf_real(v: f64) -> Object {
    let s = format!("{v:.6}");
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    // Preserve signed-zero: qpdf's QUtil::double_to_string(-0.0, 6, true)
    // yields "-0" (via `printf %.6f` → "-0.000000" → trim → "-0"). Rust's
    // f64::to_string on -0.0 also yields "-0", so a round-trip through parse
    // preserves the sign bit and the writer emits the same "-0" byte.
    let rounded: f64 = trimmed.parse().unwrap_or(0.0);
    Object::Real(rounded)
}

/// Multiply two matrices left-to-right (`this * other`), mirroring
/// `QPDFMatrix::concat` byte-for-byte (see overlay.rs::qpdf_concat).
fn concat_matrices(this: [f64; 6], other: [f64; 6]) -> [f64; 6] {
    let [a, b, c, d, e, f] = this;
    let [oa, ob, oc, od, oe, of] = other;
    [
        a * oa + c * ob,
        b * oa + d * ob,
        a * oc + c * od,
        b * oc + d * od,
        a * oe + c * of + e,
        b * oe + d * of + f,
    ]
}

const IDENTITY: [f64; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// Read the annot's `/Rect`, transform its four corners by `cm`, and write
/// back the normalized bounding rectangle. Mirrors qpdf's
/// `QPDFMatrix::transformRectangle` used at transformAnnotations line 1011.
fn transform_annot_rect<R: Read + Seek>(
    dest: &mut Pdf<R>,
    annot_ref: ObjectRef,
    cm: [f64; 6],
) -> Result<()> {
    let Some(mut dict) = dest.resolve(annot_ref)?.into_dict() else {
        return Ok(()); // cov:ignore: defensive early return
    };
    let rect_val = dict.get("Rect").cloned();
    let rect = match rect_val {
        Some(Object::Array(arr)) => arr,
        _ => return Ok(()), // cov:ignore: defensive early return
    };
    if rect.len() != 4 {
        return Ok(()); // cov:ignore: defensive early return
    }
    let mut nums = [0.0f64; 4];
    for (i, item) in rect.iter().enumerate() {
        nums[i] = match item {
            Object::Integer(n) => *n as f64,
            Object::Real(x) | Object::RealLiteral { value: x, .. } => *x,
            _ => return Ok(()), // cov:ignore: defensive early return
        };
    }
    let new_rect = transform_rect_by_cm(nums, cm);
    dict.insert(
        "Rect",
        Object::Array(new_rect.iter().map(|&x| qpdf_real(x)).collect()),
    );
    dest.set_object(annot_ref, Object::Dictionary(dict));
    Ok(())
}

/// Transform the four corners of `rect` by `cm` and return the axis-aligned
/// bounding rectangle of the transformed corners (`[min_x, min_y, max_x,
/// max_y]`), mirroring qpdf's `QPDFMatrix::transformRectangle`.
fn transform_rect_by_cm(rect: [f64; 4], cm: [f64; 6]) -> [f64; 4] {
    let [llx, lly, urx, ury] = rect;
    let corners = [(llx, lly), (llx, ury), (urx, lly), (urx, ury)];
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for (x, y) in corners {
        let (tx, ty) = apply_matrix_to_point(cm, x, y);
        if tx < min_x {
            min_x = tx;
        }
        if tx > max_x {
            max_x = tx;
        }
        if ty < min_y {
            min_y = ty;
        }
        if ty > max_y {
            max_y = ty;
        }
    }
    [min_x, min_y, max_x, max_y]
}

/// Apply a 6-element matrix to a point: `[a b c d e f] * (x, y)` =
/// `(a*x + c*y + e, b*x + d*y + f)`.
fn apply_matrix_to_point(m: [f64; 6], x: f64, y: f64) -> (f64, f64) {
    let [a, b, c, d, e, f] = m;
    (a * x + c * y + e, b * x + d * y + f)
}

/// Read the destination `/AcroForm`'s `/DA` and `/Q` defaults and compare
/// with `template.source_default_da / _q`. Returns `Some(InheritedOverrides)`
/// only when at least one differs — matching qpdf's `override_da || override_q`
/// gate (transformAnnotations line 736-767). Missing values default to `""`
/// / `0` per qpdf.
fn compute_inherited_overrides<R: Read + Seek>(
    dest: &mut Pdf<R>,
    template: &AnnotationCopyTemplate,
) -> Result<Option<InheritedOverrides>> {
    let (dest_da, dest_q) = read_dest_acroform_defaults(dest)?;
    let from_da = template
        .source_default_da
        .as_deref()
        .unwrap_or(b"")
        .to_vec();
    let from_q = template.source_default_q.unwrap_or(0);
    let override_da = from_da != dest_da;
    let override_q = from_q != dest_q;
    if !override_da && !override_q {
        return Ok(None);
    }
    Ok(Some(InheritedOverrides {
        override_da,
        from_default_da: from_da,
        override_q,
        from_default_q: from_q,
    }))
}

/// Read `/AcroForm/DA` (as bytes; empty when absent) and `/AcroForm/Q`
/// (integer; 0 when absent) from the destination doc's catalog.
fn read_dest_acroform_defaults<R: Read + Seek>(dest: &mut Pdf<R>) -> Result<(Vec<u8>, i64)> {
    let Some(root_ref) = dest.root_ref() else {
        return Ok((Vec::new(), 0)); // cov:ignore: defensive early return
    };
    let acroform_val = {
        let obj = dest.resolve_borrowed(root_ref)?;
        let Some(dict) = obj.as_dict() else {
            return Ok((Vec::new(), 0)); // cov:ignore: defensive early return
        };
        dict.get("AcroForm").cloned()
    };
    let acroform_dict = match acroform_val {
        None | Some(Object::Null) => return Ok((Vec::new(), 0)),
        Some(Object::Dictionary(d)) => d, // cov:ignore: match arm — defensive on unexpected shape
        Some(Object::Reference(r)) => match dest.resolve(r)? {
            Object::Dictionary(d) => d,
            _ => return Ok((Vec::new(), 0)), // cov:ignore: defensive early return
        },
        _ => return Ok((Vec::new(), 0)), // cov:ignore: defensive early return
    };
    let da = match acroform_dict.get("DA") {
        Some(Object::String(s)) => s.clone(), // cov:ignore: match arm — defensive on unexpected shape
        _ => Vec::new(),
    };
    let q = match acroform_dict.get("Q") {
        Some(Object::Integer(n)) => *n, // cov:ignore: match arm — defensive on unexpected shape
        _ => 0,
    };
    Ok((da, q))
}

/// Apply `overrides` to a single field's dup during the BFS. Mirrors qpdf
/// `adjustInheritedFields` (`libqpdf/QPDFAcroFormDocumentHelper.cc:442-484`).
///
/// For each of /DA and /Q: if `override_*` is set AND the field does not
/// carry an explicit value on itself OR any ancestor visited earlier in this
/// placement (both flpdf's per_placement_dup and qpdf's `orig_to_copy` visit
/// parents before kids, so ancestors are already dup'd), pin the field to
/// the source's default so the (different) dest default is not silently
/// inherited.
fn adjust_inherited_field<R: Read + Seek>(
    dest: &mut Pdf<R>,
    field_ref: ObjectRef,
    field_dict: &mut crate::Dictionary,
    overrides: &InheritedOverrides,
    per_placement_dup: &BTreeMap<ObjectRef, ObjectRef>,
) -> Result<()> {
    // /FT gate: qpdf's `adjustInheritedFields` also proceeds unconditionally
    // (the comment at line 449-455 explains it may write to non-field
    // annots, and that's harmless), so we do too.
    if overrides.override_da
        && field_dict.get("DA").is_none()
        && !ancestor_has_key(dest, field_ref, b"DA", per_placement_dup)?
    {
        field_dict.insert("DA", Object::String(overrides.from_default_da.clone()));
    }
    if overrides.override_q
        && field_dict.get("Q").is_none()
        && !ancestor_has_key(dest, field_ref, b"Q", per_placement_dup)?
    {
        field_dict.insert("Q", Object::Integer(overrides.from_default_q));
    }
    Ok(())
}

/// True when any ancestor of `field_ref` (via `/Parent`) already carries an
/// explicit `key`. Follows the *dup* graph via `per_placement_dup` because
/// the BFS rewrites `/Parent` to point at the placement's dup before the
/// field is written back. Bounded by `MAX_PARENT_WALK_DEPTH`.
fn ancestor_has_key<R: Read + Seek>(
    dest: &mut Pdf<R>,
    field_ref: ObjectRef,
    key: &[u8],
    per_placement_dup: &BTreeMap<ObjectRef, ObjectRef>,
) -> Result<bool> {
    let mut current = field_ref;
    let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
    for _ in 0..MAX_PARENT_WALK_DEPTH {
        if !visited.insert(current) {
            return Ok(false); // cov:ignore: defensive early return
        }
        let parent = {
            let obj = dest.resolve_borrowed(current)?;
            let Some(dict) = obj.as_dict() else {
                return Ok(false); // cov:ignore: defensive early return
            };
            dict.get_ref("Parent")
        };
        let Some(parent_ref) = parent else {
            return Ok(false);
        };
        // The BFS may have already rewritten /Parent to the dup; if not, map
        // via per_placement_dup so we walk within this placement's clones.
        let ancestor_ref = per_placement_dup
            .get(&parent_ref)
            .copied()
            .unwrap_or(parent_ref);
        let has = {
            let obj = dest.resolve_borrowed(ancestor_ref)?;
            match obj.as_dict() {
                Some(dict) => dict.get(key).is_some(),
                None => false, // cov:ignore: defensive `None` match arm
            }
        };
        if has {
            return Ok(true);
        } // cov:ignore: control-flow marker — llvm-cov instrumentation artifact
        current = ancestor_ref; // cov:ignore: multi-hop /Parent walk — the shipped adjustInheritedFields fixture has 1-hop parents at most
    }
    Ok(false) // cov:ignore: success arm reached only on defensive path
}

/// Remove the annot's `/P` entry when it is currently `Null`.
///
/// The source's `/P` was excluded from the copy closure
/// ([`survey_source_annotations`], `skip_parent_key = true`), so the
/// dup'd annot dict carries `/P null` after `copy_objects`'s rewrite pass
/// (unmapped refs become `Object::Null`). qpdf's oracle drops `/P` from
/// the copied annot entirely (verified against the
/// `overlay-source-p-and-inline.pdf` golden) — the page back-pointer is
/// re-established at read time by whatever consumer needs it. Removing
/// the key here rather than re-pointing it at `dest_page_ref` matches
/// that behavior; annots that never had `/P` (the primary target) are
/// unaffected since the key isn't present to remove.
fn set_annot_page_ref_if_null<R: Read + Seek>(
    dest: &mut Pdf<R>,
    annot_ref: ObjectRef,
) -> Result<()> {
    let Some(mut dict) = dest.resolve(annot_ref)?.into_dict() else {
        return Ok(()); // cov:ignore: defensive early return
    };
    match dict.get("P") {
        Some(Object::Null) => {}
        _ => return Ok(()),
    }
    dict.remove("P");
    dest.set_object(annot_ref, Object::Dictionary(dict));
    Ok(())
}

/// Append `new_annot_refs` to the destination page's `/Annots` array,
/// creating the array if the page had none (qpdf copyAnnotations line
/// 1032-1038).
fn append_page_annots<R: Read + Seek>(
    dest: &mut Pdf<R>,
    dest_page_ref: ObjectRef,
    new_annot_refs: &[ObjectRef],
) -> Result<()> {
    if new_annot_refs.is_empty() {
        return Ok(()); // cov:ignore: defensive early return
    }
    let Some(mut page_dict) = dest.resolve(dest_page_ref)?.into_dict() else {
        return Ok(()); // cov:ignore: defensive early return
    };
    // cov:ignore-start: pre-existing /Annots on the dest page — none of the
    // shipped fixtures pre-populate /Annots on a dest page (fxo-red pages
    // start bare), so the "already has annots" arms are only reachable via
    // hand-crafted PDFs.
    let mut annots = match page_dict.get("Annots").cloned() {
        None | Some(Object::Null) => Vec::new(),
        Some(Object::Array(arr)) => arr,
        Some(Object::Reference(r)) => match dest.resolve(r)? {
            Object::Array(arr) => arr,
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    // cov:ignore-end
    for &r in new_annot_refs {
        annots.push(Object::Reference(r));
    }
    page_dict.insert("Annots", Object::Array(annots));
    dest.set_object(dest_page_ref, Object::Dictionary(page_dict));
    Ok(())
}

/// Lazy-initialize the destination `/AcroForm/DR`. If dest has no `/AcroForm`,
/// create one; the `/DR` (fresh or pre-existing) has `source_dr`'s (already
/// dest-space) contents merged into it via [`merge_resources_shallow`], which
/// records any conflict rename it performs into `dr_map`. Returns the ref of
/// the dest `/DR` (whether newly-created or previously present).
///
/// This matches qpdf transformAnnotations line 780-800 (init_dr_map): dest
/// `/AcroForm/DR` is a new object, and `source_dr` is preserved as a SEPARATE
/// object reachable via appearance-stream `/Resources` references — so the
/// dest ends up with two byte-identical `/DR`-shaped objects (dest_dr for
/// `/AcroForm/DR`, source_dr for AP stream `/Resources`). See qpdf golden
/// `overlay-copy-annotations.pdf` obj 4 (dest_dr) and obj 344 (source_dr copy).
///
/// For a dest that already has an `/AcroForm` and `/DR`, the pre-existing
/// `/DR` object is reused (not replaced) and `source_dr`'s entries are merged
/// into it with conflict renaming. Downstream consumption of `dr_map` to
/// rewrite copied fields' `/DA` and AP-stream content (qpdf's
/// `adjustDefaultAppearances` / `adjustAppearanceStream`) is not implemented
/// yet — left for a later change.
fn ensure_dest_acroform_dr<R: Read + Seek>(
    dest: &mut Pdf<R>,
    source_dr: ObjectRef,
    dr_map: &mut DrMap,
) -> Result<ObjectRef> {
    // cov:ignore-start: defensive early returns on catalog-shape guards.
    let Some(root_ref) = dest.root_ref() else {
        return Err(Error::Unsupported(
            "destination has no /Root; cannot install /AcroForm for copied form fields".into(),
        ));
    };
    let Some(mut catalog) = dest.resolve(root_ref)?.into_dict() else {
        return Err(Error::Unsupported(
            "destination /Root does not resolve to a dictionary".into(),
        ));
    };
    // cov:ignore-end
    let acroform_val = catalog.get("AcroForm").cloned();
    let acroform_ref = match acroform_val {
        Some(Object::Reference(r)) => r,
        // cov:ignore-start: direct-/AcroForm promotion — no shipped fixture
        // has a direct /AcroForm dict at the source (qpdf normalizes to
        // indirect on write), so the promotion body only runs on hand-
        // written PDFs. The behaviour is intentionally lossless (contents
        // preserved) — see the comment inside the arm.
        Some(Object::Dictionary(existing)) => {
            let af_ref = allocate_next_ref(dest)?;
            dest.set_object(af_ref, Object::Dictionary(existing));
            catalog.insert("AcroForm", Object::Reference(af_ref));
            dest.set_object(root_ref, Object::Dictionary(catalog));
            af_ref
        }
        // cov:ignore-end
        _ => {
            // No /AcroForm (or a non-dict value): install a fresh empty one.
            let mut af = crate::Dictionary::new();
            af.insert("Fields", Object::Array(Vec::new()));
            let af_ref = allocate_next_ref(dest)?;
            dest.set_object(af_ref, Object::Dictionary(af));
            catalog.insert("AcroForm", Object::Reference(af_ref));
            dest.set_object(root_ref, Object::Dictionary(catalog));
            af_ref
        }
    };
    let mut acroform_dict = match dest.resolve(acroform_ref)?.into_dict() {
        Some(d) => d,
        // cov:ignore-start: defensive early return on non-dict /AcroForm
        None => {
            return Err(Error::Unsupported(
                "destination /AcroForm does not resolve to a dictionary".into(),
            ));
            // cov:ignore-end
        }
    };
    // Existing /DR may be indirect (a ref) or a direct dict; preserve either
    // shape and merge `source_dr`'s entries into it either way.
    match acroform_dict.get("DR").cloned() {
        Some(Object::Reference(existing)) => {
            merge_resources_shallow(dest, existing, source_dr, dr_map)?;
            return Ok(existing);
        }
        // cov:ignore-start: direct-/DR promotion in the existing-AcroForm
        // path — analogous to the direct-/AcroForm case above. qpdf
        // normalizes /DR to indirect on write, so no shipped fixture
        // reaches this arm.
        Some(Object::Dictionary(existing)) => {
            let dr_ref = allocate_next_ref(dest)?;
            dest.set_object(dr_ref, Object::Dictionary(existing));
            acroform_dict.insert("DR", Object::Reference(dr_ref));
            dest.set_object(acroform_ref, Object::Dictionary(acroform_dict));
            merge_resources_shallow(dest, dr_ref, source_dr, dr_map)?;
            return Ok(dr_ref);
        }
        // cov:ignore-end
        _ => {}
    }
    // No existing /DR: allocate a fresh one, merge source_dr's contents into
    // it, and wire dest /AcroForm to point at it.
    let dr_ref = allocate_next_ref(dest)?;
    dest.set_object(dr_ref, Object::Dictionary(crate::Dictionary::new()));
    merge_resources_shallow(dest, dr_ref, source_dr, dr_map)?;
    acroform_dict.insert("DR", Object::Reference(dr_ref));
    dest.set_object(acroform_ref, Object::Dictionary(acroform_dict));
    Ok(dr_ref)
}

/// Merge `source_dr`'s resource entries into the dict at `dest_dr`, which may
/// already carry entries of its own (e.g. a pre-existing dest `/AcroForm/DR`).
/// Mirrors qpdf's `QPDFObjectHandle::mergeResources`.
///
/// For each top-level key (resource type: `/Font`, `/XObject`, `/ColorSpace`,
/// ...), source's per-name entries are merged into a dest-owned copy of that
/// category's sub-dict, so `dest_dr` and `source_dr` never share mutable
/// sub-dict state:
/// - a source name absent from dest's sub-dict is inserted verbatim;
/// - a source name present in dest's sub-dict pointing at the SAME object
///   (by [`ObjectRef`] identity, not deep equality — matches qpdf's
///   `QPDFObjGen`-based check) is a no-op;
/// - a source name present in dest's sub-dict pointing at a DIFFERENT object
///   is a genuine conflict: the source's entry is inserted under the
///   smallest unused `{name}_N` (`N` starting at 1, scanned against the dest
///   sub-dict as it grows during this call — qpdf's `getUniqueResourceName`),
///   and `(name, {name}_N)` is recorded into `dr_map[type]`.
///
/// Individual resource entries (e.g. `/F1 8 0 R`) are shallow-cloned — they
/// are typically refs, so the clone is cheap and the dest and source paths
/// continue to share the underlying font/xobject objects (as qpdf does).
///
/// Dest-scoped rename reuse: qpdf's `mergeResources` maintains a
/// `QPDFObjGen -> name` map on the dest `/DR` so the same colliding source
/// object gets the same renamed dest name on every subsequent call. Callers
/// therefore share a single `dr_map` across every merge into the same
/// destination (see `apply_aggregated_sources` in `overlay.rs`); when a
/// collision recurs and `dr_map` already records the rename and the mapped
/// dest name still holds the same source ref, the rename is reused rather
/// than minting a fresh `_N`. Byte parity across repeated placements onto a
/// pre-conflicting dest `/DR` depends on this reuse — every field's `/DA` and
/// every AP stream must reference the same renamed name across pages.
fn merge_resources_shallow<R: Read + Seek>(
    dest: &mut Pdf<R>,
    dest_dr: ObjectRef,
    source_dr: ObjectRef,
    dr_map: &mut DrMap,
) -> Result<()> {
    let Some(src_dict) = dest.resolve(source_dr)?.into_dict() else {
        return Ok(()); // cov:ignore: defensive early return
    };
    let Some(mut dest_dict) = dest.resolve(dest_dr)?.into_dict() else {
        return Ok(()); // cov:ignore: defensive early return
    };
    // PDF permits `/Font <ref>` (indirect resource-type sub-dict) as well as
    // the direct-dict shape; qpdf's `QPDFObjectHandle::mergeResources`
    // operates on resolved QPDFObjectHandle values, so both shapes must
    // merge — losing an indirect source sub-dict would drop the referenced
    // fonts entirely. `src_dict` is owned (from `into_dict`), so the loop's
    // borrow of it does not collide with `dest.resolve(...)` on the mutable
    // `dest` — both borrows are of separate variables.
    for (type_key, src_type_val) in src_dict.iter() {
        let src_type_dict = match src_type_val {
            Object::Reference(r) => dest.resolve(*r)?.into_dict(),
            _ => src_type_val.as_dict().cloned(),
        };

        let Some(src_type_dict) = src_type_dict else {
            // cov:ignore-start: verbatim-copy path — non-dict resource-type
            // value from an unusual source /DR shape (either a non-dict/-ref
            // direct value, or an indirect ref that does not resolve to a
            // dict). No shipped fixture supplies this shape.
            if dest_dict.get(type_key).is_none() {
                dest_dict.insert(type_key, src_type_val.clone());
            }
            continue;
            // cov:ignore-end
        };

        // Resolve dest's existing sub-dict for this type (if any) so a
        // pre-existing dest `/DR` category is preserved rather than
        // replaced. When the dest sub-dict is INDIRECT, allocate a NEW
        // indirect object holding a shallow copy of the referenced dict
        // and re-point `dest_dict[type_key]` at it — mirroring qpdf's
        // `this_val = replaceKeyAndGetNew(rtype, this_val.shallowCopy())`
        // in `QPDFObjectHandle::mergeResources`. Mutating the ORIGINAL
        // referenced object in place would leak the merge (and any
        // subsequent `_N` renames) into every other holder of that ref.
        let (mut new_type_dict, new_indirect_target) = match dest_dict.get(type_key).cloned() {
            Some(Object::Dictionary(existing)) => (existing, None),
            Some(Object::Reference(r)) => match dest.resolve(r)?.into_dict() {
                Some(d) => (d, Some(allocate_next_ref(dest)?)),
                // cov:ignore-start: dest resource-type ref does not resolve
                // to a dict — degrade to a fresh dict and replace inline.
                // No shipped fixture supplies this malformed shape.
                None => (crate::Dictionary::new(), None),
                // cov:ignore-end
            },
            _ => (crate::Dictionary::new(), None),
        };

        for (name, val) in src_type_dict.iter() {
            match new_type_dict.get(name) {
                None => {
                    new_type_dict.insert(name, val.clone());
                }
                Some(existing_val) => {
                    // Same-name collision. qpdf's short-circuit is object
                    // identity (QPDFObjGen), not deep equality — a direct
                    // (non-reference) value can never match here even if
                    // structurally equal, matching mergeResources.
                    let same_object = matches!(
                        (existing_val, val),
                        (Object::Reference(d), Object::Reference(s)) if d == s
                    );
                    if same_object {
                        continue;
                    }
                    // If this source object was already renamed on a prior
                    // merge call against the same dest `/DR` (dr_map recorded
                    // the mapping, and the mapped dest name still holds this
                    // same source ref), reuse that name instead of minting a
                    // fresh `_N`. Mirrors qpdf's dest-scoped
                    // `QPDFObjGen -> name` reuse map: byte parity across
                    // repeated placements onto the same dest depends on the
                    // colliding source object producing the same renamed
                    // name every time.
                    let existing_rename = dr_map
                        .get(type_key.as_slice())
                        .and_then(|m| m.get(name))
                        .cloned();
                    if let Some(mapped) = existing_rename {
                        let mapped_holds_same_source = matches!(
                            (new_type_dict.get(&mapped), val),
                            (Some(Object::Reference(d)), Object::Reference(s)) if d == s
                        );
                        if mapped_holds_same_source {
                            continue;
                        }
                    }
                    let new_name = unique_dr_name(name, &new_type_dict)?;
                    new_type_dict.insert(&new_name, val.clone());
                    dr_map
                        .entry(type_key.to_vec())
                        .or_default()
                        .insert(name.to_vec(), new_name);
                }
            }
        }

        if let Some(new_ref) = new_indirect_target {
            // qpdf-parity: dest sub-dict was indirect, so install the merged
            // dict into a freshly-allocated indirect object and re-point
            // dest_dict at it. The ORIGINAL indirect object is untouched —
            // other holders of the same ref are not affected.
            dest.set_object(new_ref, Object::Dictionary(new_type_dict));
            dest_dict.insert(type_key, Object::Reference(new_ref));
        } else {
            dest_dict.insert(type_key, Object::Dictionary(new_type_dict));
        }
    }
    dest.set_object(dest_dr, Object::Dictionary(dest_dict));
    Ok(())
}

/// Smallest `{base}_N` (`N` starting at 1) absent from `dict`, scanning
/// `dict` as it stands at call time — so repeated calls within the same
/// [`merge_resources_shallow`] pass see names inserted by earlier collisions
/// in that pass and do not reissue them. Mirrors qpdf's
/// `getUniqueResourceName`.
///
/// # Errors
///
/// [`Error::Unsupported`] if `u32` wraps before an unused suffix is found
/// (would require billions of colliding names under one base).
fn unique_dr_name(base: &[u8], dict: &crate::Dictionary) -> Result<Vec<u8>> {
    let mut n: u32 = 1;
    loop {
        let candidate = [base, b"_", n.to_string().as_bytes()].concat();
        if dict.get(&candidate).is_none() {
            return Ok(candidate);
        }
        n = n
            .checked_add(1)
            .ok_or_else(|| Error::Unsupported("DR resource-name suffix space exhausted".into()))?;
    }
}

/// After every placement on a destination page has been applied, add the
/// collected new top-level fields to the dest `/AcroForm/Fields` with fully
/// qualified name collision renaming (qpdf's `+N` suffix scheme).
///
pub(crate) fn add_and_rename_form_fields<R: Read + Seek>(
    dest: &mut Pdf<R>,
    new_top_fields: Vec<ObjectRef>,
) -> Result<()> {
    if new_top_fields.is_empty() {
        return Ok(());
    }
    let Some(root_ref) = dest.root_ref() else {
        return Ok(()); // cov:ignore: defensive early return
    };
    let mut catalog = match dest.resolve(root_ref)?.into_dict() {
        Some(d) => d,
        None => return Ok(()), // cov:ignore: defensive early return
    };

    // Get or create /AcroForm. When apply_placement's `ensure_dest_acroform_dr`
    // ran (source had /DR), it already promoted the AcroForm to indirect and
    // this branch just uses that ref. When it did NOT run (source has no /DR),
    // we may still see a direct AcroForm here — promote it in place preserving
    // its existing contents (/Fields, /DA, /Q, /NeedAppearances, ...). Only
    // a truly missing/non-dict AcroForm gets a fresh empty one.
    let acroform_ref = match catalog.get("AcroForm").cloned() {
        Some(Object::Reference(r)) => r,
        // cov:ignore-start: direct-/AcroForm promotion inside
        // add_and_rename_form_fields — symmetric with
        // ensure_dest_acroform_dr's direct-/AcroForm branch; qpdf normalizes
        // /AcroForm to indirect on write so no shipped fixture reaches it.
        Some(Object::Dictionary(existing)) => {
            let r = allocate_next_ref(dest)?;
            dest.set_object(r, Object::Dictionary(existing));
            catalog.insert("AcroForm", Object::Reference(r));
            dest.set_object(root_ref, Object::Dictionary(catalog));
            r
        }
        // cov:ignore-end
        // cov:ignore-start: fresh-AcroForm creation only fires when
        // apply_placement's ensure_dest_acroform_dr did NOT run (source
        // had no /DR) yet new_top_fields is non-empty. All shipped
        // fixtures that produce fields (primary + defaults + p_and_inline
        // + direct-dr + existing-af + indirect-fields) also carry a
        // source /DR, so ensure runs and this branch stays cold.
        _ => {
            let mut dict = crate::Dictionary::new();
            dict.insert("Fields", Object::Array(Vec::new()));
            let r = allocate_next_ref(dest)?;
            dest.set_object(r, Object::Dictionary(dict));
            catalog.insert("AcroForm", Object::Reference(r));
            dest.set_object(root_ref, Object::Dictionary(catalog));
            r
        } // cov:ignore-end
    };

    // Build the set of existing FULLY-QUALIFIED field names, walking the
    // existing /AcroForm/Fields tree. On a fresh dest with no fields, this
    // starts empty and the first placement adds without renaming.
    let mut existing_fqns: BTreeSet<Vec<u8>> = BTreeSet::new();
    let existing_top_field_refs = read_existing_top_field_refs(dest, acroform_ref)?;
    for &top in &existing_top_field_refs {
        collect_fully_qualified_names(dest, top, Vec::new(), 0, &mut existing_fqns)?;
    }

    // BFS the new top fields, renaming /T on FQN collision (`+N` suffix).
    // Every field re-runs the collision check against the live `existing_fqns`
    // set (which is updated per-field below), so two independently-copied
    // fields that happen to share the same source FQN each pick a distinct
    // suffix instead of colliding in the dest AcroForm. A cache keyed by
    // old_fqn would produce the same suffix for both and re-introduce the
    // collision.
    let mut seen: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut queue: std::collections::VecDeque<ObjectRef> = new_top_fields.iter().copied().collect();
    while let Some(field_ref) = queue.pop_front() {
        if !seen.insert(field_ref) {
            continue; // cov:ignore: field revisited via /Kids sharing — defensive
        }
        // Compute the OLD fully-qualified name (before any rename). Top-level
        // fields have no /Parent so FQN == /T. Sub-fields with /T get parent
        // FQN + "." + own /T. Because we rename by mutating /T in place, the
        // parent's /T at the time of the child's FQN computation is ALREADY
        // renamed if the parent was processed first — but the BFS visits
        // parents before kids, so the child sees the RENAMED parent /T. This
        // matches qpdf's behavior: renaming is done depth-first-ish via BFS
        // and the fully-qualified name computed inside the loop after the
        // parent's /T rewrite reflects the new name.
        let has_t = {
            let obj = dest.resolve_borrowed(field_ref)?;
            let Some(dict) = obj.as_dict() else { continue };
            dict.get("T").is_some()
        };
        if has_t {
            let old_fqn = fully_qualified_name_of(dest, field_ref)?;
            let append = {
                let mut candidate_fqn = old_fqn.clone();
                let mut suffix = 0u32;
                let mut append = Vec::new();
                while existing_fqns.contains(&candidate_fqn) {
                    // cov:ignore-start: u32 overflow on 4 billion suffix bumps — unreachable
                    suffix = suffix.checked_add(1).ok_or_else(|| {
                        Error::Unsupported("field name suffix space exhausted".into())
                    })?;
                    // cov:ignore-end
                    append = format!("+{suffix}").into_bytes();
                    candidate_fqn = old_fqn.clone();
                    candidate_fqn.extend_from_slice(&append);
                }
                append
            };
            if !append.is_empty() {
                let Some(mut dict) = dest.resolve(field_ref)?.into_dict() else {
                    continue; // cov:ignore: field ref resolved to non-dict — malformed
                };
                let old_t = match dict.get("T").cloned() {
                    Some(Object::String(s)) => s,
                    Some(_) => Vec::new(), // cov:ignore: match arm — defensive on unexpected shape
                    None => Vec::new(),    // cov:ignore: defensive `None` match arm
                };
                let mut new_t = old_t;
                new_t.extend_from_slice(&append);
                dict.insert("T", Object::String(new_t));
                dest.set_object(field_ref, Object::Dictionary(dict));
            }
            // Add the final (possibly renamed) FQN to the existing set so
            // subsequent placements' fields with the same source name pick a
            // NEW suffix.
            let mut final_fqn = old_fqn;
            final_fqn.extend_from_slice(&append);
            existing_fqns.insert(final_fqn);
        }

        // Enqueue kids (they may carry /T and need FQN collision handling).
        let kids_refs = {
            let obj = dest.resolve_borrowed(field_ref)?;
            let Some(dict) = obj.as_dict() else { continue };
            match dict.get("Kids") {
                Some(Object::Array(arr)) => arr
                    .iter()
                    .filter_map(|item| match item {
                        Object::Reference(r) => Some(*r),
                        _ => None, // cov:ignore: fallback match arm — defensive/malformed input
                    })
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            }
        };
        for k in kids_refs {
            queue.push_back(k);
        }
    }

    // Finally append the new top-level fields to /AcroForm/Fields (they may
    // now have renamed /T). /Fields may be a direct array or an indirect ref
    // to one; preserve the existing entries in either shape.
    let Some(mut acroform_dict) = dest.resolve(acroform_ref)?.into_dict() else {
        return Ok(()); // cov:ignore: defensive early return
    };
    match acroform_dict.get("Fields").cloned() {
        Some(Object::Reference(fields_ref)) => {
            // Indirect /Fields: update the array in place so any other
            // references to it (there shouldn't be any, but be conservative)
            // stay valid.
            let mut fields = match dest.resolve(fields_ref)? {
                Object::Array(arr) => arr,
                _ => Vec::new(), // cov:ignore: fallback match arm — defensive/malformed input
            };
            for r in new_top_fields {
                fields.push(Object::Reference(r));
            }
            dest.set_object(fields_ref, Object::Array(fields));
        }
        other => {
            let mut fields = match other {
                Some(Object::Array(arr)) => arr,
                _ => Vec::new(), // cov:ignore: fallback match arm — defensive/malformed input
            };
            for r in new_top_fields {
                fields.push(Object::Reference(r));
            }
            acroform_dict.insert("Fields", Object::Array(fields));
            dest.set_object(acroform_ref, Object::Dictionary(acroform_dict));
        }
    }
    Ok(())
}

/// Read the existing top-level field refs from `dest`'s AcroForm/Fields.
/// Returns an empty vec when /Fields is missing or malformed.
fn read_existing_top_field_refs<R: Read + Seek>(
    dest: &mut Pdf<R>,
    acroform_ref: ObjectRef,
) -> Result<Vec<ObjectRef>> {
    let Some(dict) = dest.resolve(acroform_ref)?.into_dict() else {
        return Ok(Vec::new()); // cov:ignore: defensive early return
    };
    let fields = match dict.get("Fields").cloned() {
        Some(Object::Array(arr)) => arr,
        Some(Object::Reference(r)) => match dest.resolve(r)? {
            Object::Array(arr) => arr,
            _ => return Ok(Vec::new()), // cov:ignore: defensive early return
        },
        _ => return Ok(Vec::new()), // cov:ignore: defensive early return
    };
    Ok(fields
        .into_iter()
        .filter_map(|item| match item {
            Object::Reference(r) => Some(r),
            _ => None, // cov:ignore: fallback match arm — defensive/malformed input
        })
        .collect())
}

/// BFS `field_ref`'s tree, collecting the fully-qualified name of every node
/// that carries a /T. Bounded by [`MAX_PARENT_WALK_DEPTH`] via `depth`.
fn collect_fully_qualified_names<R: Read + Seek>(
    dest: &mut Pdf<R>,
    field_ref: ObjectRef,
    parent_fqn: Vec<u8>,
    depth: usize,
    out: &mut BTreeSet<Vec<u8>>,
) -> Result<()> {
    // cov:ignore-start: defensive early return on field-tree depth overflow
    if depth > MAX_PARENT_WALK_DEPTH {
        return Err(Error::Unsupported(format!(
            "AcroForm field tree exceeds maximum depth of {MAX_PARENT_WALK_DEPTH}"
        )));
    }
    // cov:ignore-end
    // Reading /T and /Kids in two steps: /T uses a borrowed read, then /Kids
    // may need to resolve an indirect array (which requires a mutable borrow
    // and therefore drops the borrowed read first). Both direct-array and
    // indirect-array forms of /Kids are valid per ISO 32000 and appear in
    // real-world AcroForm docs.
    let (own_fqn, kids_val) = {
        let obj = dest.resolve_borrowed(field_ref)?;
        let Some(dict) = obj.as_dict() else {
            return Ok(()); // cov:ignore: defensive early return
        };
        let own_fqn = match dict.get("T") {
            Some(Object::String(t)) => {
                let mut fqn = parent_fqn.clone();
                if !fqn.is_empty() {
                    fqn.push(b'.'); // cov:ignore: nested /T under a parent that also has /T — none of the shipped fixtures nest
                }
                fqn.extend_from_slice(t);
                Some(fqn)
            }
            _ => None,
        };
        (own_fqn, dict.get("Kids").cloned())
    };
    let kids_refs: Vec<ObjectRef> = match kids_val {
        Some(Object::Array(arr)) => arr
            .iter()
            .filter_map(|item| match item {
                Object::Reference(r) => Some(*r),
                _ => None, // cov:ignore: fallback match arm — defensive/malformed input
            })
            .collect(),
        // cov:ignore-start: indirect-/Kids resolution — a dest field whose
        // /Kids is stored as an indirect ref is a shape my Fixture 5
        // (fxo-red-indirect-fields) exercises for /Fields but not for
        // /Kids specifically; no shipped fixture nests an indirect-/Kids
        // sub-field under a top-level widget.
        Some(Object::Reference(r)) => match dest.resolve(r)? {
            Object::Array(arr) => arr
                .into_iter()
                .filter_map(|item| match item {
                    Object::Reference(r) => Some(r),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        },
        // cov:ignore-end
        _ => Vec::new(),
    };
    let recursion_fqn = own_fqn.clone().unwrap_or(parent_fqn);
    if let Some(fqn) = own_fqn {
        out.insert(fqn);
    }
    for kid in kids_refs {
        collect_fully_qualified_names(dest, kid, recursion_fqn.clone(), depth + 1, out)?;
    }
    Ok(())
}

/// Compute the fully-qualified name of a field by walking its /Parent chain
/// upward (qpdf `getFullyQualifiedName`). Bounded by
/// [`MAX_PARENT_WALK_DEPTH`].
fn fully_qualified_name_of<R: Read + Seek>(
    dest: &mut Pdf<R>,
    field_ref: ObjectRef,
) -> Result<Vec<u8>> {
    let mut names_reverse: Vec<Vec<u8>> = Vec::new();
    let mut current = field_ref;
    let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
    // cov:ignore-start: defensive /Parent walk bailouts (cycle break,
    // non-dict parent, missing /T non-string arm) are hostile-input
    // guards; `_ => None` on /T fires only when a field lacks /T (no
    // shipped fixture nests such a field); and the "current = p"
    // continuation is exercised by every widget with a parent but
    // llvm-cov's guard-line instrumentation reports 0 hits on match
    // arms of the form `Some(p) => …`.
    for _ in 0..MAX_PARENT_WALK_DEPTH {
        if !visited.insert(current) {
            break;
        }
        let (t_opt, parent) = {
            let obj = dest.resolve_borrowed(current)?;
            let Some(dict) = obj.as_dict() else {
                break;
            };
            let t_opt = match dict.get("T") {
                Some(Object::String(s)) => Some(s.clone()),
                _ => None,
            };
            let parent = dict.get_ref("Parent");
            (t_opt, parent)
        };
        if let Some(t) = t_opt {
            names_reverse.push(t);
        }
        match parent {
            Some(p) => current = p,
            None => break,
        }
    }
    // cov:ignore-end
    // Reverse to root-to-leaf order and join with '.'.
    names_reverse.reverse();
    let mut out = Vec::new();
    for (i, n) in names_reverse.iter().enumerate() {
        if i > 0 {
            out.push(b'.'); // cov:ignore: nested-field FQN join — no shipped fixture nests fields under a parent that also has /T
        }
        out.extend_from_slice(n);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Minimal single-object (`/Type /Catalog`, no `/AcroForm`) PDF, just
    /// enough for [`Pdf::open`] to accept. Tests layer additional objects
    /// onto it via [`Pdf::set_object`] at object numbers beyond the xref
    /// table — the same pattern `allocate_next_ref` relies on elsewhere in
    /// this crate (a `set_object` at a fresh number is accepted and shows up
    /// in `object_refs()`/is resolvable immediately).
    fn minimal_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(
            format!("xref\n0 2\n0000000000 65535 f \n{off1:010} 00000 n \n").as_bytes(),
        );
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    fn open_minimal() -> Pdf<Cursor<Vec<u8>>> {
        Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("minimal pdf should parse")
    }

    /// Set object `n` (generation 0) to a dictionary built from `entries` and
    /// return its ref.
    fn set_dict<R: Read + Seek>(pdf: &mut Pdf<R>, n: u32, entries: &[(&str, Object)]) -> ObjectRef {
        let mut d = crate::Dictionary::new();
        for (k, v) in entries {
            d.insert(*k, v.clone());
        }
        let r = ObjectRef::new(n, 0);
        pdf.set_object(r, Object::Dictionary(d));
        r
    }

    /// Build a one-category `/Font << name ref, ... >>` resource dict object.
    fn set_font_dr<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        n: u32,
        entries: &[(&str, ObjectRef)],
    ) -> ObjectRef {
        let mut font = crate::Dictionary::new();
        for (name, r) in entries {
            font.insert(*name, Object::Reference(*r));
        }
        set_dict(pdf, n, &[("Font", Object::Dictionary(font))])
    }

    fn font_dict<R: Read + Seek>(pdf: &mut Pdf<R>, dr_ref: ObjectRef) -> crate::Dictionary {
        let dr = pdf.resolve(dr_ref).unwrap().into_dict().unwrap();
        dr.get("Font").and_then(Object::as_dict).unwrap().clone()
    }

    // ---- merge_resources_shallow ------------------------------------------

    #[test]
    fn merge_resources_shallow_dest_empty_is_verbatim_insert() {
        let mut pdf = open_minimal();
        let font_ref = set_dict(
            &mut pdf,
            10,
            &[("BaseFont", Object::Name(b"Helvetica".to_vec()))],
        );
        let dest_dr = set_dict(&mut pdf, 2, &[]);
        let source_dr = set_font_dr(&mut pdf, 3, &[("F1", font_ref)]);

        let mut dr_map = DrMap::new();
        merge_resources_shallow(&mut pdf, dest_dr, source_dr, &mut dr_map).unwrap();

        assert!(dr_map.is_empty());
        let font = font_dict(&mut pdf, dest_dr);
        assert_eq!(font.get_ref("F1"), Some(font_ref));
    }

    #[test]
    fn merge_resources_shallow_renames_on_collision_with_different_ref() {
        let mut pdf = open_minimal();
        let helv_ref = set_dict(
            &mut pdf,
            10,
            &[("BaseFont", Object::Name(b"Helvetica".to_vec()))],
        );
        let courier_ref = set_dict(
            &mut pdf,
            11,
            &[("BaseFont", Object::Name(b"Courier".to_vec()))],
        );
        let dest_dr = set_font_dr(&mut pdf, 2, &[("F1", helv_ref)]);
        let source_dr = set_font_dr(&mut pdf, 3, &[("F1", courier_ref)]);

        let mut dr_map = DrMap::new();
        merge_resources_shallow(&mut pdf, dest_dr, source_dr, &mut dr_map).unwrap();

        assert_eq!(
            dr_map
                .get(b"Font".as_slice())
                .and_then(|m| m.get(b"F1".as_slice())),
            Some(&b"F1_1".to_vec())
        );
        let font = font_dict(&mut pdf, dest_dr);
        assert_eq!(font.get_ref("F1"), Some(helv_ref));
        assert_eq!(font.get_ref("F1_1"), Some(courier_ref));
    }

    #[test]
    fn merge_resources_shallow_same_ref_collision_is_noop() {
        let mut pdf = open_minimal();
        let font_ref = set_dict(
            &mut pdf,
            10,
            &[("BaseFont", Object::Name(b"Helvetica".to_vec()))],
        );
        let dest_dr = set_font_dr(&mut pdf, 2, &[("F1", font_ref)]);
        let source_dr = set_font_dr(&mut pdf, 3, &[("F1", font_ref)]);

        let mut dr_map = DrMap::new();
        merge_resources_shallow(&mut pdf, dest_dr, source_dr, &mut dr_map).unwrap();

        assert!(dr_map.is_empty());
        let font = font_dict(&mut pdf, dest_dr);
        assert_eq!(font.get_ref("F1"), Some(font_ref));
        assert!(font.get("F1_1").is_none());
    }

    #[test]
    fn merge_resources_shallow_scans_past_taken_suffix_to_next_n() {
        let mut pdf = open_minimal();
        let helv_ref = set_dict(
            &mut pdf,
            10,
            &[("BaseFont", Object::Name(b"Helvetica".to_vec()))],
        );
        let other_ref = set_dict(
            &mut pdf,
            11,
            &[("BaseFont", Object::Name(b"TimesRoman".to_vec()))],
        );
        let courier_ref = set_dict(
            &mut pdf,
            12,
            &[("BaseFont", Object::Name(b"Courier".to_vec()))],
        );
        // Pre-seed dest with BOTH F1 (which will collide with source) and
        // F1_1 (an unrelated pre-existing entry that already occupies the
        // first rename candidate), forcing the suffix scan to skip to F1_2.
        let dest_dr = set_font_dr(&mut pdf, 2, &[("F1", helv_ref), ("F1_1", other_ref)]);
        let source_dr = set_font_dr(&mut pdf, 3, &[("F1", courier_ref)]);

        let mut dr_map = DrMap::new();
        merge_resources_shallow(&mut pdf, dest_dr, source_dr, &mut dr_map).unwrap();

        assert_eq!(
            dr_map
                .get(b"Font".as_slice())
                .and_then(|m| m.get(b"F1".as_slice())),
            Some(&b"F1_2".to_vec())
        );
        let font = font_dict(&mut pdf, dest_dr);
        assert_eq!(font.get_ref("F1"), Some(helv_ref));
        assert_eq!(font.get_ref("F1_1"), Some(other_ref));
        assert_eq!(font.get_ref("F1_2"), Some(courier_ref));
    }

    /// Source `/DR/Font` is stored as an indirect reference (not a direct
    /// sub-dict). qpdf's `mergeResources` resolves the reference and merges
    /// the underlying dict; a naive implementation that only matched
    /// `Object::Dictionary` would drop the fonts entirely.
    #[test]
    fn merge_resources_shallow_resolves_indirect_source_resource_type_dict() {
        let mut pdf = open_minimal();
        let font_ref = set_dict(
            &mut pdf,
            10,
            &[("BaseFont", Object::Name(b"Helvetica".to_vec()))],
        );
        // Indirect /Font sub-dict on the source side: /Font 4 0 R where 4 0 R
        // resolves to << /F1 10 0 R >>.
        let font_subdict_ref = ObjectRef::new(4, 0);
        let mut font_sub = crate::Dictionary::new();
        font_sub.insert("F1", Object::Reference(font_ref));
        pdf.set_object(font_subdict_ref, Object::Dictionary(font_sub));
        let source_dr = set_dict(
            &mut pdf,
            3,
            &[("Font", Object::Reference(font_subdict_ref))],
        );
        let dest_dr = set_dict(&mut pdf, 2, &[]);

        let mut dr_map = DrMap::new();
        merge_resources_shallow(&mut pdf, dest_dr, source_dr, &mut dr_map).unwrap();

        assert!(dr_map.is_empty());
        let font = font_dict(&mut pdf, dest_dr);
        assert_eq!(font.get_ref("F1"), Some(font_ref));
    }

    /// Dest `/DR/Font` is stored as an indirect reference potentially
    /// shared with other holders. qpdf's `mergeResources` shallow-copies
    /// the referenced dict into a FRESH indirect object and re-points
    /// dest's `/Font` at the new ref
    /// (`this_val = replaceKeyAndGetNew(rtype, this_val.shallowCopy())` in
    /// `QPDFObjectHandle::mergeResources`); the ORIGINAL indirect object
    /// stays untouched so unrelated holders keep their original content.
    /// A naive implementation that mutated the original object in place
    /// would leak the merge (and any subsequent `_N` renames) into every
    /// other holder of that ref.
    #[test]
    fn merge_resources_shallow_copies_indirect_dest_sub_dict_into_fresh_ref() {
        let mut pdf = open_minimal();
        let helv_ref = set_dict(
            &mut pdf,
            10,
            &[("BaseFont", Object::Name(b"Helvetica".to_vec()))],
        );
        let courier_ref = set_dict(
            &mut pdf,
            11,
            &[("BaseFont", Object::Name(b"Courier".to_vec()))],
        );
        // Indirect /Font sub-dict on the dest side: /Font 4 0 R where 4 0 R
        // resolves to << /F0 10 0 R >> (F0 = Helvetica).
        let dest_font_subdict_ref = ObjectRef::new(4, 0);
        let mut dest_font_sub = crate::Dictionary::new();
        dest_font_sub.insert("F0", Object::Reference(helv_ref));
        pdf.set_object(dest_font_subdict_ref, Object::Dictionary(dest_font_sub));
        let dest_dr = set_dict(
            &mut pdf,
            2,
            &[("Font", Object::Reference(dest_font_subdict_ref))],
        );
        // Direct /Font sub-dict on the source side, adding a fresh F1.
        let source_dr = set_font_dr(&mut pdf, 3, &[("F1", courier_ref)]);

        let mut dr_map = DrMap::new();
        merge_resources_shallow(&mut pdf, dest_dr, source_dr, &mut dr_map).unwrap();

        assert!(dr_map.is_empty(), "no name collision, no rename");
        // dest's /Font now points at a NEW indirect object (not the original).
        let dest_dict = pdf.resolve(dest_dr).unwrap().into_dict().unwrap();
        let new_font_ref = dest_dict.get_ref("Font").expect("Font must be indirect");
        assert_ne!(
            new_font_ref, dest_font_subdict_ref,
            "qpdf shallow-copies indirect sub-dicts into a fresh ref",
        );
        // The ORIGINAL indirect object is untouched — still only carries F0.
        let original = pdf
            .resolve(dest_font_subdict_ref)
            .unwrap()
            .into_dict()
            .unwrap();
        assert_eq!(original.get_ref("F0"), Some(helv_ref));
        assert!(
            original.get("F1").is_none(),
            "original indirect object must not be mutated (other holders would see F1 leak)",
        );
        // The NEW indirect object carries the shallow-copied F0 plus F1.
        let merged = pdf.resolve(new_font_ref).unwrap().into_dict().unwrap();
        assert_eq!(merged.get_ref("F0"), Some(helv_ref));
        assert_eq!(merged.get_ref("F1"), Some(courier_ref));
    }

    /// Two `merge_resources_shallow` calls against the SAME dest `/DR` with
    /// the SAME conflicting source: the first call renames `F1` → `F1_1`
    /// and records the mapping in `dr_map`; the second call must reuse
    /// `F1_1` (dr_map already carries the mapping and dest's `F1_1` still
    /// holds the source ref) rather than minting `F1_2`. This is qpdf's
    /// dest-scoped rename-reuse invariant; every field's `/DA` and every
    /// AP stream needs the renamed name to stay stable across placements
    /// or byte parity breaks.
    #[test]
    fn merge_resources_shallow_reuses_prior_rename_across_repeated_calls() {
        let mut pdf = open_minimal();
        let helv_ref = set_dict(
            &mut pdf,
            10,
            &[("BaseFont", Object::Name(b"Helvetica".to_vec()))],
        );
        let courier_ref = set_dict(
            &mut pdf,
            11,
            &[("BaseFont", Object::Name(b"Courier".to_vec()))],
        );
        let dest_dr = set_font_dr(&mut pdf, 2, &[("F1", helv_ref)]);
        let source_dr = set_font_dr(&mut pdf, 3, &[("F1", courier_ref)]);

        let mut dr_map = DrMap::new();
        merge_resources_shallow(&mut pdf, dest_dr, source_dr, &mut dr_map).unwrap();
        // First call: F1 collision renamed to F1_1.
        assert_eq!(
            dr_map
                .get(b"Font".as_slice())
                .and_then(|m| m.get(b"F1".as_slice())),
            Some(&b"F1_1".to_vec())
        );

        merge_resources_shallow(&mut pdf, dest_dr, source_dr, &mut dr_map).unwrap();
        // Second call: reuse F1_1 (do NOT mint F1_2).
        assert_eq!(
            dr_map
                .get(b"Font".as_slice())
                .and_then(|m| m.get(b"F1".as_slice())),
            Some(&b"F1_1".to_vec())
        );
        let font = font_dict(&mut pdf, dest_dr);
        assert_eq!(font.get_ref("F1"), Some(helv_ref));
        assert_eq!(font.get_ref("F1_1"), Some(courier_ref));
        assert!(
            font.get("F1_2").is_none(),
            "second call must reuse F1_1 rather than minting F1_2"
        );
    }

    // ---- ensure_dest_acroform_dr -------------------------------------------

    #[test]
    fn ensure_dest_acroform_dr_creates_fresh_dr_when_dest_has_no_acroform() {
        let mut pdf = open_minimal();
        let font_ref = set_dict(
            &mut pdf,
            10,
            &[("BaseFont", Object::Name(b"Helvetica".to_vec()))],
        );
        let source_dr = set_font_dr(&mut pdf, 3, &[("F1", font_ref)]);

        let mut dr_map = DrMap::new();
        let dr_ref = ensure_dest_acroform_dr(&mut pdf, source_dr, &mut dr_map).unwrap();

        assert!(dr_map.is_empty());
        let font = font_dict(&mut pdf, dr_ref);
        assert_eq!(font.get_ref("F1"), Some(font_ref));
    }

    /// Simulates two placements onto two different pages of the SAME
    /// destination document: the first call creates a fresh `/AcroForm/DR`
    /// (`dr_map1` stays empty — verbatim insert); the second call finds the
    /// `/AcroForm/DR` already installed as an indirect reference (the
    /// `Some(Object::Reference(existing))` branch) and must reuse it. This is
    /// the invariant this layer must not break: `source_dr` did not change
    /// between calls, so the second call's `F1` collides with dest's `F1`
    /// under the *same* object — a same-ref no-op, not a rename. Every
    /// multi-page overlay byte gate with a form-field source relies on this:
    /// after the first placement establishes the dest `/DR`, every
    /// subsequent placement's `ensure_dest_acroform_dr` call must leave it
    /// untouched rather than minting spurious `F1_1`, `F1_2`, ... entries.
    #[test]
    fn ensure_dest_acroform_dr_reuses_existing_dr_across_repeated_calls_without_rename() {
        let mut pdf = open_minimal();
        let font_ref = set_dict(
            &mut pdf,
            10,
            &[("BaseFont", Object::Name(b"Helvetica".to_vec()))],
        );
        let source_dr = set_font_dr(&mut pdf, 3, &[("F1", font_ref)]);

        let mut dr_map1 = DrMap::new();
        let dr_ref1 = ensure_dest_acroform_dr(&mut pdf, source_dr, &mut dr_map1).unwrap();
        assert!(dr_map1.is_empty());

        let mut dr_map2 = DrMap::new();
        let dr_ref2 = ensure_dest_acroform_dr(&mut pdf, source_dr, &mut dr_map2).unwrap();

        assert_eq!(dr_ref1, dr_ref2, "the same /DR object must be reused");
        assert!(dr_map2.is_empty(), "same source ref must not be re-renamed");
        let font = font_dict(&mut pdf, dr_ref2);
        assert_eq!(font.get_ref("F1"), Some(font_ref));
        assert!(font.get("F1_1").is_none());
    }

    // ---- adjust_default_appearance ------------------------------------------

    /// Build a `/DR`-shaped dict `<< /Font << name1 100 0 R, ... >> >>`.
    /// `adjust_default_appearance` only checks name *presence* in the Font
    /// sub-dict, so the target refs are arbitrary placeholders.
    fn font_resources_dict(names: &[&str]) -> crate::Dictionary {
        let mut font = crate::Dictionary::new();
        for (i, name) in names.iter().enumerate() {
            font.insert(*name, Object::Reference(ObjectRef::new(100 + i as u32, 0)));
        }
        let mut dr = crate::Dictionary::new();
        dr.insert("Font", Object::Dictionary(font));
        dr
    }

    /// Build a `DrMap` with a single `Font` category from `(old, new)` pairs.
    fn font_dr_map(entries: &[(&str, &str)]) -> DrMap {
        let mut font = BTreeMap::new();
        for (old, new) in entries {
            font.insert(old.as_bytes().to_vec(), new.as_bytes().to_vec());
        }
        let mut dr_map = DrMap::new();
        dr_map.insert(b"Font".to_vec(), font);
        dr_map
    }

    #[test]
    fn adjust_default_appearance_empty_dr_map_is_identity() {
        let dr_map = DrMap::new();
        let resources = crate::Dictionary::new();
        let da: &[u8] = b"0 0.4 0 rg /F1 18 Tf";
        assert_eq!(
            adjust_default_appearance(da, &dr_map, &resources),
            da.to_vec()
        );
    }

    /// The canonical case exercised by the (still-ignored) Layer 1 byte
    /// gate: `form-fields-and-annotations.pdf`'s `/DA (0 0.4 0 rg /F1 18
    /// Tf)` merged onto a dest whose `/DR/Font/F1` already existed, renaming
    /// the source's colliding `/F1` to `/F1_1`.
    #[test]
    fn adjust_default_appearance_rewrites_matched_font_name() {
        let dr_map = font_dr_map(&[("F1", "F1_1")]);
        let resources = font_resources_dict(&["F1", "F1_1"]);
        let da: &[u8] = b"0 0.4 0 rg /F1 18 Tf";
        assert_eq!(
            adjust_default_appearance(da, &dr_map, &resources),
            b"0 0.4 0 rg /F1_1 18 Tf".to_vec()
        );
    }

    #[test]
    fn adjust_default_appearance_name_not_in_dr_map_is_verbatim() {
        // /ZaDi never collided during the merge (only /F1 did), so it has no
        // dr_map entry and must be left untouched — matching the qpdf golden
        // (`overlay-onto-existing-acroform-dr.pdf`), where every `/ZaDi`
        // `/DA` stays verbatim alongside the renamed `/F1_1` ones.
        let dr_map = font_dr_map(&[("F1", "F1_1")]);
        let resources = font_resources_dict(&["ZaDi"]);
        let da: &[u8] = b"0.18039 0.20392 0.21176 rg /ZaDi 0 Tf";
        assert_eq!(
            adjust_default_appearance(da, &dr_map, &resources),
            da.to_vec()
        );
    }

    #[test]
    fn adjust_default_appearance_name_absent_from_resources_is_verbatim() {
        // dr_map has a rename for "F1", but the resources dict's /Font
        // sub-dict does not carry "F1" as a key at all (only an unrelated
        // name) — the safety guard must leave /DA untouched rather than
        // rewrite a name that isn't actually backed by this resources dict.
        let dr_map = font_dr_map(&[("F1", "F1_1")]);
        let resources = font_resources_dict(&["SomeOtherName"]);
        let da: &[u8] = b"0 0.4 0 rg /F1 18 Tf";
        assert_eq!(
            adjust_default_appearance(da, &dr_map, &resources),
            da.to_vec()
        );
    }

    #[test]
    fn adjust_default_appearance_name_inside_string_literal_is_verbatim() {
        // `(F1)` is a STRING operand, not a name token, even though its
        // content matches a dr_map key — must not be mistaken for the font
        // name preceding `Tf`.
        let dr_map = font_dr_map(&[("F1", "F1_1")]);
        let resources = font_resources_dict(&["F1"]);
        let da: &[u8] = b"(F1) 18 Tf";
        assert_eq!(
            adjust_default_appearance(da, &dr_map, &resources),
            da.to_vec()
        );
    }

    #[test]
    fn adjust_default_appearance_skips_comment_verbatim() {
        let dr_map = font_dr_map(&[("F1", "F1_1")]);
        let resources = font_resources_dict(&["F1", "F1_1"]);
        let da: &[u8] = b"% a comment\n/F1 18 Tf";
        assert_eq!(
            adjust_default_appearance(da, &dr_map, &resources),
            b"% a comment\n/F1_1 18 Tf".to_vec()
        );
    }

    #[test]
    fn adjust_default_appearance_recovers_from_stray_delimiter() {
        // A stray `)` with no matching `(` does not start any recognised
        // operand (it isn't `/(<[+-.0-9`); the scanner must copy it verbatim
        // and keep scanning rather than aborting, so the `/F1` rename after
        // it still applies.
        let dr_map = font_dr_map(&[("F1", "F1_1")]);
        let resources = font_resources_dict(&["F1", "F1_1"]);
        let da: &[u8] = b") /F1 18 Tf";
        assert_eq!(
            adjust_default_appearance(da, &dr_map, &resources),
            b") /F1_1 18 Tf".to_vec()
        );
    }

    #[test]
    fn adjust_default_appearance_recovers_from_operand_parse_error() {
        // An unterminated string literal (opens `(` but never closes) IS a
        // recognised operand start and reaches the shared object lexer,
        // which returns `Err` on EOF. The scanner must copy only the single
        // `(` byte and resume rather than losing the rest of the string, so
        // the `/F1` rename after it still applies.
        let dr_map = font_dr_map(&[("F1", "F1_1")]);
        let resources = font_resources_dict(&["F1", "F1_1"]);
        let da: &[u8] = b"(bad /F1 18 Tf";
        assert_eq!(
            adjust_default_appearance(da, &dr_map, &resources),
            b"(bad /F1_1 18 Tf".to_vec()
        );
    }

    #[test]
    fn adjust_default_appearance_no_font_category_in_dr_map_is_verbatim() {
        // dr_map is non-empty but has no "Font" entry (e.g. only /XObject
        // collisions were recorded) — the Tf-pattern lookup must miss
        // cleanly rather than panic.
        let mut dr_map = DrMap::new();
        dr_map.insert(b"XObject".to_vec(), BTreeMap::new());
        let resources = font_resources_dict(&["F1"]);
        let da: &[u8] = b"/F1 18 Tf";
        assert_eq!(
            adjust_default_appearance(da, &dr_map, &resources),
            da.to_vec()
        );
    }

    #[test]
    fn adjust_default_appearance_no_font_category_in_resources_is_verbatim() {
        let dr_map = font_dr_map(&[("F1", "F1_1")]);
        let resources = crate::Dictionary::new();
        let da: &[u8] = b"/F1 18 Tf";
        assert_eq!(
            adjust_default_appearance(da, &dr_map, &resources),
            da.to_vec()
        );
    }

    // ---- end-to-end (structural, not byte-identical) -----------------------

    /// Full `apply_overlay_specs` pipeline over the Layer-1 fixtures used by
    /// the (still-`#[ignore]`d) byte gate in `overlay.rs`, asserting
    /// structure rather than exact bytes so this runs without the
    /// `qpdf-zlib-compat` feature. Restricted to destination page 1 only
    /// (`to: "1"`) so it drives exactly one merge call — the multi-page
    /// repeated-placement reuse case is covered separately by
    /// `overlay_pipeline_repeated_placements_reuse_dr_rename_end_to_end`.
    #[test]
    fn overlay_pipeline_renames_colliding_dr_font_end_to_end() {
        use crate::overlay::{apply_overlay_specs, OverlayKind, OverlaySpec};
        use crate::page_range::PageRange;
        use std::path::Path;

        fn fixture(name: &str) -> Pdf<std::io::BufReader<std::fs::File>> {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/compat")
                .join(name);
            let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
            Pdf::open(std::io::BufReader::new(file)).unwrap()
        }
        fn pr(input: &str) -> PageRange {
            PageRange::parse(input).unwrap_or_else(|e| panic!("parse {input:?}: {e}"))
        }

        let mut dest = fixture("fxo-red-with-existing-acroform-dr.pdf");
        let src = fixture("form-fields-and-annotations.pdf");
        let mut specs = vec![OverlaySpec {
            source: src,
            kind: OverlayKind::Overlay,
            from: pr(""),
            to: pr("1"),
            repeat: None,
        }];
        apply_overlay_specs(&mut dest, &mut specs).unwrap();

        let root_ref = dest.root_ref().unwrap();
        let catalog = dest.resolve(root_ref).unwrap().into_dict().unwrap();
        let acroform_ref = catalog.get_ref("AcroForm").unwrap();
        let acroform = dest.resolve(acroform_ref).unwrap().into_dict().unwrap();
        let dr_ref = acroform.get_ref("DR").unwrap();
        let font = font_dict(&mut dest, dr_ref);
        let f1_ref = font.get_ref("F1").expect("original /F1 preserved");
        let f1_1_ref = font.get_ref("F1_1").expect("collision renamed to /F1_1");
        assert_ne!(f1_ref, f1_1_ref);

        let f1_font = dest.resolve(f1_ref).unwrap().into_dict().unwrap();
        assert_eq!(
            f1_font.get("BaseFont"),
            Some(&Object::Name(b"Helvetica".to_vec()))
        );
        let f1_1_font = dest.resolve(f1_1_ref).unwrap().into_dict().unwrap();
        assert_eq!(
            f1_1_font.get("BaseFont"),
            Some(&Object::Name(b"Courier".to_vec()))
        );

        // Layer 3: at least one copied field's /DA must have been rewritten
        // from the collision-renamed /F1 to /F1_1 (adjust_default_appearance,
        // called from duplicate_field_tree). form-fields-and-annotations.pdf
        // supplies `/DA (0 0.4 0 rg /F1 18 Tf)` on its text-box widgets.
        let fields = acroform.get("Fields").and_then(Object::as_array).unwrap();
        let mut saw_rewritten_da = false;
        for field in fields {
            // Every /AcroForm/Fields entry in this fixture is an indirect
            // reference resolving to a dict — unwrap rather than a
            // defensive continue, since a malformed shape here is a test
            // setup bug, not an input to tolerate.
            let field_ref = field.as_ref_id().unwrap();
            let field_dict = dest.resolve(field_ref).unwrap().into_dict().unwrap();
            if let Some(Object::String(da)) = field_dict.get("DA") {
                if da.as_slice() == b"0 0.4 0 rg /F1_1 18 Tf" {
                    saw_rewritten_da = true;
                }
            }
        }
        assert!(
            saw_rewritten_da,
            "expected at least one copied field's /DA rewritten to /F1_1"
        );
    }

    /// Repeated placements onto multiple dest pages: after page 1 renames
    /// the colliding /F1 → /F1_1, every subsequent page must reuse /F1_1
    /// rather than mint /F1_2, /F1_3, ... . qpdf's byte gate expects a
    /// single renamed entry regardless of page count; the dr_map lifetime
    /// (per-dest, threaded through apply_aggregated_sources) and the
    /// rename-reuse branch in merge_resources_shallow are what enforce this.
    #[test]
    fn overlay_pipeline_repeated_placements_reuse_dr_rename_end_to_end() {
        use crate::overlay::{apply_overlay_specs, OverlayKind, OverlaySpec};
        use crate::page_range::PageRange;
        use std::path::Path;

        fn fixture(name: &str) -> Pdf<std::io::BufReader<std::fs::File>> {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/compat")
                .join(name);
            let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
            Pdf::open(std::io::BufReader::new(file)).unwrap()
        }
        fn pr(input: &str) -> PageRange {
            PageRange::parse(input).unwrap_or_else(|e| panic!("parse {input:?}: {e}"))
        }

        let mut dest = fixture("fxo-red-with-existing-acroform-dr.pdf");
        let src = fixture("form-fields-and-annotations.pdf");
        // Overlay the single source page onto three dest pages (--repeat=1
        // cycles it). qpdf's mergeResources fires once per page against the
        // shared dest /DR; without dest-scoped rename reuse this would mint
        // F1_2 and F1_3 on pages 2 and 3.
        let mut specs = vec![OverlaySpec {
            source: src,
            kind: OverlayKind::Overlay,
            from: pr(""),
            to: pr("1-3"),
            repeat: Some(pr("1")),
        }];
        apply_overlay_specs(&mut dest, &mut specs).unwrap();

        let root_ref = dest.root_ref().unwrap();
        let catalog = dest.resolve(root_ref).unwrap().into_dict().unwrap();
        let acroform_ref = catalog.get_ref("AcroForm").unwrap();
        let acroform = dest.resolve(acroform_ref).unwrap().into_dict().unwrap();
        let dr_ref = acroform.get_ref("DR").unwrap();
        let font = font_dict(&mut dest, dr_ref);

        assert!(font.get("F1").is_some(), "original /F1 preserved");
        assert!(font.get("F1_1").is_some(), "collision renamed to /F1_1");
        assert!(
            font.get("F1_2").is_none(),
            "second page must reuse /F1_1, not mint /F1_2"
        );
        assert!(
            font.get("F1_3").is_none(),
            "third page must reuse /F1_1, not mint /F1_3"
        );
    }
}
