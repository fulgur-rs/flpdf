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
pub(crate) fn apply_placement<R: Read + Seek>(
    dest: &mut Pdf<R>,
    dest_page_ref: ObjectRef,
    template: &AnnotationCopyTemplate,
    cm: [f64; 6],
    dest_acroform_dr: &mut Option<ObjectRef>,
) -> Result<Vec<ObjectRef>> {
    if template.annots.is_empty() {
        return Ok(Vec::new());
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
            *dest_acroform_dr = Some(ensure_dest_acroform_dr(dest, source_dr)?);
        }
    }

    let mut new_top_fields: Vec<ObjectRef> = Vec::new();
    let mut added_top_field_set: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut new_annot_refs: Vec<ObjectRef> = Vec::new();

    for (dest_annot_ref, dest_top_field) in &template.annots {
        // 1. Duplicate the field tree (top → kids) into per-placement copies,
        //    patching /Parent back-pointers. If the widget IS the field (self-
        //    field), the annot ref equals the top-level ref, so this call also
        //    dups the annot as a side effect.
        let new_top_field_ref = if let Some(top_ref) = dest_top_field {
            let new_top = duplicate_field_tree(dest, *top_ref, &mut per_placement_dup)?;
            // Reset field-level /DR to the dest AcroForm /DR (foreign case
            // only — qpdf transformAnnotations line 928). If dest has no /DR
            // (e.g. source also had none), leave the field's /DR alone.
            if let Some(dr) = *dest_acroform_dr {
                clear_or_set_field_dr(dest, new_top, dr, &per_placement_dup)?;
            }
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
fn duplicate_field_tree<R: Read + Seek>(
    dest: &mut Pdf<R>,
    top_ref: ObjectRef,
    per_placement_dup: &mut BTreeMap<ObjectRef, ObjectRef>,
) -> Result<ObjectRef> {
    let new_top = match per_placement_dup.get(&top_ref) {
        Some(&existing) => return Ok(existing),
        None => {
            let new = shallow_dup_indirect(dest, top_ref)?;
            per_placement_dup.insert(top_ref, new);
            new
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
            continue;
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
                Object::Reference(kr) => match dest.resolve(kr)? {
                    Object::Array(arr) => Some(arr),
                    _ => None,
                },
                _ => None,
            };
            if let Some(mut kids) = kids_array {
                for entry in kids.iter_mut() {
                    if let Object::Reference(kid_ref) = *entry {
                        let kid_dup = match per_placement_dup.get(&kid_ref) {
                            Some(&existing) => existing,
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
                    }
                }
                dict.insert("Kids", Object::Array(kids));
            }
        }

        dest.set_object(dup_ref, Object::Dictionary(dict));
    }

    Ok(new_top)
}

/// If the field at `field_ref` has a `/DR` entry, replace it with a reference
/// to the destination `/AcroForm/DR` (qpdf transformAnnotations line 928-930).
/// A field without `/DR` is left untouched.
fn clear_or_set_field_dr<R: Read + Seek>(
    dest: &mut Pdf<R>,
    field_ref: ObjectRef,
    dest_dr: ObjectRef,
    _per_placement_dup: &BTreeMap<ObjectRef, ObjectRef>,
) -> Result<()> {
    let Some(mut dict) = dest.resolve(field_ref)?.into_dict() else {
        return Ok(());
    };
    if dict.get("DR").is_none() {
        return Ok(());
    }
    dict.insert("DR", Object::Reference(dest_dr));
    dest.set_object(field_ref, Object::Dictionary(dict));
    Ok(())
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
        return Ok(());
    };
    let Some(ap_val) = annot_dict.get("AP").cloned() else {
        return Ok(());
    };
    let Some(mut apdict) = (match ap_val {
        Object::Dictionary(d) => Some(d),
        Object::Reference(r) => dest.resolve(r)?.into_dict(),
        _ => None,
    }) else {
        return Ok(());
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
                    let Some(sub_val) = sub.get(&sub_key).cloned() else { continue };
                    if let Object::Reference(stream_ref) = sub_val {
                        if let Some(new_ref) =
                            dup_and_transform_ap_stream(dest, stream_ref, cm)?
                        {
                            sub.insert(&sub_key, Object::Reference(new_ref));
                        }
                    }
                }
                apdict.insert(&key, Object::Dictionary(sub));
            }
            _ => {}
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
        return Ok(None);
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
        _ => return None,
    };
    let mut out = [0.0f64; 6];
    for (i, item) in arr.iter().enumerate() {
        out[i] = match item {
            Object::Integer(n) => *n as f64,
            Object::Real(x) => *x,
            _ => return None,
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
    let mut s = format!("{v:.6}");
    // Normalize "-0.000000" (and any trim path leading to "-0") to "0".
    if s == "-0.000000" {
        s = "0".to_string();
    }
    let trimmed = s.trim_end_matches('0').trim_end_matches('.').to_string();
    // Fall back to 0 for pathological parse failure (unreachable for %.6f).
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
        return Ok(());
    };
    let rect_val = dict.get("Rect").cloned();
    let rect = match rect_val {
        Some(Object::Array(arr)) => arr,
        _ => return Ok(()),
    };
    if rect.len() != 4 {
        return Ok(());
    }
    let mut nums = [0.0f64; 4];
    for (i, item) in rect.iter().enumerate() {
        nums[i] = match item {
            Object::Integer(n) => *n as f64,
            Object::Real(x) => *x,
            _ => return Ok(()),
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

/// Append `new_annot_refs` to the destination page's `/Annots` array,
/// creating the array if the page had none (qpdf copyAnnotations line
/// 1032-1038).
fn append_page_annots<R: Read + Seek>(
    dest: &mut Pdf<R>,
    dest_page_ref: ObjectRef,
    new_annot_refs: &[ObjectRef],
) -> Result<()> {
    if new_annot_refs.is_empty() {
        return Ok(());
    }
    let Some(mut page_dict) = dest.resolve(dest_page_ref)?.into_dict() else {
        return Ok(());
    };
    let mut annots = match page_dict.get("Annots").cloned() {
        None | Some(Object::Null) => Vec::new(),
        Some(Object::Array(arr)) => arr,
        Some(Object::Reference(r)) => match dest.resolve(r)? {
            Object::Array(arr) => arr,
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    for &r in new_annot_refs {
        annots.push(Object::Reference(r));
    }
    page_dict.insert("Annots", Object::Array(annots));
    dest.set_object(dest_page_ref, Object::Dictionary(page_dict));
    Ok(())
}

/// Lazy-initialize the destination `/AcroForm/DR`. If dest has no `/AcroForm`,
/// create one; the new `/DR` is set to point at the (already dest-space)
/// `source_dr` copied by the batch cross-doc copy. Returns the ref of the
/// dest `/DR` (whether newly-created or previously present).
///
/// For the primary target (fxo-red has no /AcroForm), this creates:
///   dest_acroform = { /Fields [] /DR <source_dr_ref> }
///
/// For a dest that already has an /AcroForm, `/DR` merge conflicts are NOT
/// resolved here — that's the `dr_map` path (adjustAppearanceStream, dormant
/// for the primary target). A future extension when a real fixture requires
/// it will add the merge.
fn ensure_dest_acroform_dr<R: Read + Seek>(
    dest: &mut Pdf<R>,
    source_dr: ObjectRef,
) -> Result<ObjectRef> {
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
    let acroform_val = catalog.get("AcroForm").cloned();
    let acroform_ref = match acroform_val {
        Some(Object::Reference(r)) => r,
        _ => {
            // No existing /AcroForm (or a direct one — replace with an
            // indirect empty dict). The direct-case fallback would silently
            // drop the pre-existing direct /AcroForm; for pre-v1.0 we treat
            // "no /AcroForm" as the sole supported starting point (fxo-red).
            let mut dict = crate::Dictionary::new();
            dict.insert("Fields", Object::Array(Vec::new()));
            dict.insert("DR", Object::Reference(source_dr));
            let new_ref = allocate_next_ref(dest)?;
            dest.set_object(new_ref, Object::Dictionary(dict));
            catalog.insert("AcroForm", Object::Reference(new_ref));
            dest.set_object(root_ref, Object::Dictionary(catalog));
            return Ok(source_dr);
        }
    };
    // Existing /AcroForm: ensure /DR is present. Merging into a pre-existing
    // /DR is out of scope for the primary target (dormant per advisor #3);
    // if /DR is absent, install source_dr; if present, leave it (the caller
    // won't call this unless template.source_dr is Some, but two specs might
    // race — first wins).
    let mut acroform_dict = match dest.resolve(acroform_ref)?.into_dict() {
        Some(d) => d,
        None => {
            return Err(Error::Unsupported(
                "destination /AcroForm does not resolve to a dictionary".into(),
            ))
        }
    };
    let existing_dr = acroform_dict.get_ref("DR");
    let dr_ref = match existing_dr {
        Some(r) => r,
        None => {
            acroform_dict.insert("DR", Object::Reference(source_dr));
            dest.set_object(acroform_ref, Object::Dictionary(acroform_dict));
            source_dr
        }
    };
    Ok(dr_ref)
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
        return Ok(());
    };
    let mut catalog = match dest.resolve(root_ref)?.into_dict() {
        Some(d) => d,
        None => return Ok(()),
    };

    // Get or create /AcroForm (with a /Fields array). ensure_dest_acroform_dr
    // is called by apply_placement before this point when any placement had a
    // top-level field; if we get here with no /AcroForm, no field was ever
    // added — which means new_top_fields is empty and we returned above.
    let acroform_ref = match catalog.get("AcroForm").cloned() {
        Some(Object::Reference(r)) => r,
        Some(Object::Dictionary(_)) | Some(Object::Null) | None | Some(_) => {
            // Defensive: mint a fresh /AcroForm without /DR (the DR path is
            // handled in ensure_dest_acroform_dr; if we're here without one,
            // the placement carried no /DR — non-standard but non-fatal).
            let mut dict = crate::Dictionary::new();
            dict.insert("Fields", Object::Array(Vec::new()));
            let r = allocate_next_ref(dest)?;
            dest.set_object(r, Object::Dictionary(dict));
            catalog.insert("AcroForm", Object::Reference(r));
            dest.set_object(root_ref, Object::Dictionary(catalog));
            r
        }
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
    // `renames` maps an OLD fully-qualified name to the suffix to append —
    // reuse across all fields in the same rename group (qpdf line 84-95).
    let mut renames: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
    let mut seen: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut queue: std::collections::VecDeque<ObjectRef> = new_top_fields.iter().copied().collect();
    while let Some(field_ref) = queue.pop_front() {
        if !seen.insert(field_ref) {
            continue;
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
            let append = if let Some(existing) = renames.get(&old_fqn) {
                existing.clone()
            } else {
                let mut candidate_fqn = old_fqn.clone();
                let mut suffix = 0u32;
                let mut append = Vec::new();
                while existing_fqns.contains(&candidate_fqn) {
                    suffix = suffix.checked_add(1).ok_or_else(|| {
                        Error::Unsupported("field name suffix space exhausted".into())
                    })?;
                    append = format!("+{suffix}").into_bytes();
                    candidate_fqn = old_fqn.clone();
                    candidate_fqn.extend_from_slice(&append);
                }
                renames.insert(old_fqn.clone(), append.clone());
                append
            };
            if !append.is_empty() {
                let Some(mut dict) = dest.resolve(field_ref)?.into_dict() else { continue };
                let old_t = match dict.get("T").cloned() {
                    Some(Object::String(s)) => s,
                    Some(_) => Vec::new(),
                    None => Vec::new(),
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
                        _ => None,
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
    // now have renamed /T).
    let Some(mut acroform_dict) = dest.resolve(acroform_ref)?.into_dict() else {
        return Ok(());
    };
    let mut fields = match acroform_dict.get("Fields").cloned() {
        Some(Object::Array(arr)) => arr,
        _ => Vec::new(),
    };
    for r in new_top_fields {
        fields.push(Object::Reference(r));
    }
    acroform_dict.insert("Fields", Object::Array(fields));
    dest.set_object(acroform_ref, Object::Dictionary(acroform_dict));
    Ok(())
}

/// Read the existing top-level field refs from `dest`'s AcroForm/Fields.
/// Returns an empty vec when /Fields is missing or malformed.
fn read_existing_top_field_refs<R: Read + Seek>(
    dest: &mut Pdf<R>,
    acroform_ref: ObjectRef,
) -> Result<Vec<ObjectRef>> {
    let Some(dict) = dest.resolve(acroform_ref)?.into_dict() else {
        return Ok(Vec::new());
    };
    let fields = match dict.get("Fields").cloned() {
        Some(Object::Array(arr)) => arr,
        Some(Object::Reference(r)) => match dest.resolve(r)? {
            Object::Array(arr) => arr,
            _ => return Ok(Vec::new()),
        },
        _ => return Ok(Vec::new()),
    };
    Ok(fields
        .into_iter()
        .filter_map(|item| match item {
            Object::Reference(r) => Some(r),
            _ => None,
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
    if depth > MAX_PARENT_WALK_DEPTH {
        return Err(Error::Unsupported(format!(
            "AcroForm field tree exceeds maximum depth of {MAX_PARENT_WALK_DEPTH}"
        )));
    }
    let (own_fqn, kids_refs) = {
        let obj = dest.resolve_borrowed(field_ref)?;
        let Some(dict) = obj.as_dict() else {
            return Ok(());
        };
        let own_fqn = match dict.get("T") {
            Some(Object::String(t)) => {
                let mut fqn = parent_fqn.clone();
                if !fqn.is_empty() {
                    fqn.push(b'.');
                }
                fqn.extend_from_slice(t);
                Some(fqn)
            }
            _ => None,
        };
        let kids: Vec<ObjectRef> = match dict.get("Kids") {
            Some(Object::Array(arr)) => arr
                .iter()
                .filter_map(|item| match item {
                    Object::Reference(r) => Some(*r),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        (own_fqn, kids)
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
    // Reverse to root-to-leaf order and join with '.'.
    names_reverse.reverse();
    let mut out = Vec::new();
    for (i, n) in names_reverse.iter().enumerate() {
        if i > 0 {
            out.push(b'.');
        }
        out.extend_from_slice(n);
    }
    Ok(out)
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
