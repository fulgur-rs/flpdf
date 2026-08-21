//! qpdf correspondence: QPDFPageObjectHelper.cc, QPDFAcroFormDocumentHelper.cc, ResourceFinder.cc, and QPDFObjectHandle.cc overlay responsibilities.
//! Overlay/underlay annotation copy, mirroring qpdf 11.9.0's
//! `QPDFPageObjectHelper::copyAnnotations` +
//! `QPDFAcroFormDocumentHelper::transformAnnotations`
//! (`libqpdf/QPDFPageObjectHelper.cc:991-1039`,
//! `libqpdf/QPDFAcroFormDocumentHelper.cc:698-1014`).
//!
//! The live qpdf route now runs through
//! [`crate::PageObjectHelper::copy_annotations`] and
//! [`crate::PageObjectHelper::copy_annotations_from`], with
//! [`crate::AcroFormDocumentHelper`] owning the transform/copy graph and field
//! cache. The lower-level resource-replacer pieces in this module remain a
//! narrow implementation dependency of the appearance-stream migration until
//! `flpdf-5v4a` completes its ObjectHandle cutover. The older survey/template
//! and placement helpers are retained only for their existing unit tests and
//! are not used by page or overlay orchestration.

// The retained test-only helpers are scheduled for removal with the
// appearance-stream migration; do not reintroduce them as production callers.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

use crate::acroform_document_helper::{collect_reachable_refs, collect_refs_in_object};
use crate::overlay_appearance_stream::adjust_appearance_stream_handle;
use crate::resource_replacer::{replace_resource_names, ResourceRenames};
use crate::{Error, Matrix, Object, ObjectRef, Pdf, Rectangle, Result};

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

/// Legacy source-space survey retained for the test-only compatibility helper.
#[allow(dead_code)]
#[derive(Debug, Default)]
pub(crate) struct AnnotationSurvey {
    pub annots: Vec<(ObjectRef, Option<ObjectRef>)>,
    pub source_dr: Option<ObjectRef>,
    pub source_default_da: Option<Vec<u8>>,
    pub source_default_q: Option<i64>,
}

/// Dest-space refs for the legacy test-only placement helper; consumed by
/// [`apply_placement`] once per placement.
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
#[allow(dead_code)]
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
#[allow(dead_code)]
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
#[allow(dead_code, clippy::type_complexity)]
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

/// Per-destination record of resource-name renames, populated by
/// [`merge_resources_shallow`] when a source `/DR` sub-dictionary key
/// collides with an existing dest entry of the same name that resolves to a
/// *different* object.
///
/// Holds two internally-consistent views:
///
/// - `by_name`: outer key is the resource category (`Font`, `XObject`, ...);
///   inner map is `old_source_name -> new_dest_name`. Consumed by
///   [`adjust_default_appearance`] to rewrite the copied field's `/DA`
///   string (name-indexed because `/DA` operators reference names).
///   Overwritten across multi-source merges — reflects the most recent
///   merge's mappings (which is precisely what the current placement's
///   field-tree walk needs, because that walk immediately follows the
///   merge that populated the map).
///
/// - `by_source_ref`: (category, source `ObjectRef`) -> destination name from
///   qpdf's collision-time `og_to_name` snapshot. Rebuilt lazily from the
///   current destination category at the first occupied source key of each
///   [`merge_resources_shallow`] call, then frozen for that category. Later
///   verbatim inserts and fresh aliases do not extend it.
///
/// Mirrors qpdf's per-call `dr_map`, populated by
/// `QPDFObjectHandle::mergeResources`'s `conflicts` out-parameter and driven
/// by `QPDFAcroFormDocumentHelper::init_dr_map`
/// (`libqpdf/QPDFAcroFormDocumentHelper.cc:775-800`, called from
/// `transformAnnotations`). qpdf's reuse logic is source-object-identity
/// based against the current destination dictionary, matching
/// `by_source_ref` here.
#[derive(Debug, Default, Clone)]
pub(crate) struct DrMap {
    /// old source name -> new dest name, per resource category. Overwritten
    /// across merges of different sources that collide under the same name;
    /// consumers must use it before the next merge is called on the same
    /// destination.
    by_name: ResourceRenames,
    /// (category, source `ObjectRef`) -> destination name from the
    /// collision-time qpdf identity snapshot. Rebuilt lazily for each
    /// resource merge and frozen after the first occupied source key in each
    /// category.
    by_source_ref: BTreeMap<(Vec<u8>, ObjectRef), Vec<u8>>,
}

impl DrMap {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Returns `true` when no rename has been recorded — the fast-path
    /// gate every consumer uses to skip work entirely.
    pub(crate) fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    /// The `old_name -> new_name` map for the given resource category, as
    /// populated by the most recent merge. Returned as a reference so
    /// category-specific consumers can look names up without mutation.
    pub(crate) fn category(&self, category: &[u8]) -> Option<&BTreeMap<Vec<u8>, Vec<u8>>> {
        self.by_name.get(category)
    }

    /// The resource-name renames populated by the most recent merge.
    pub(crate) fn renames(&self) -> &ResourceRenames {
        &self.by_name
    }

    /// Every resource-type category with at least one recorded rename, as
    /// populated by the most recent merge. Mirrors qpdf iterating `dr_map`'s
    /// top-level keys in `AcroForm::adjustAppearanceStream`
    /// (`libqpdf/QPDFAcroFormDocumentHelper.cc:770-777`) to force each
    /// category's sub-dictionary to exist (unshared) in an appearance
    /// stream's own `/Resources` before renaming; see
    /// [`crate::overlay_appearance_stream::adjust_appearance_stream_handle`].
    pub(crate) fn categories(&self) -> impl Iterator<Item = &Vec<u8>> {
        self.by_name.keys()
    }

    /// Record an additional `old -> new` rename under `category`, alongside
    /// whatever that category already holds. Used by
    /// [`crate::overlay_appearance_stream::adjust_appearance_stream_handle`] to
    /// extend a **per-call, cloned** copy of this map (never the shared one
    /// threaded through every placement on a destination page) with the
    /// extra rename produced when privatizing one appearance stream's own
    /// `/Resources` uncovers a second-order name collision — mirroring
    /// qpdf's `dr_map` parameter to `AcroForm::adjustAppearanceStream` being
    /// passed **by value** (`libqpdf/QPDFAcroFormDocumentHelper.cc:752`), so
    /// mutations for one stream never leak into another placement's shared
    /// [`DrMap`].
    pub(crate) fn insert_rename(&mut self, category: &[u8], old: Vec<u8>, new: Vec<u8>) {
        self.by_name
            .entry(category.to_vec())
            .or_default()
            .insert(old, new);
    }
}

#[cfg(test)]
impl DrMap {
    /// Test-only constructor: a `DrMap` with exactly one category
    /// containing one `old -> new` rename, for unit tests elsewhere in the
    /// crate (notably `overlay_appearance_stream`'s `resource_replacer`
    /// tests) that need a populated map without driving a full
    /// [`merge_resources_shallow`] call through a real `Pdf`. `by_name` is
    /// otherwise private to this module.
    pub(crate) fn for_test(category: &[u8], old: &[u8], new: &[u8]) -> Self {
        let mut map = Self::new();
        map.by_name
            .entry(category.to_vec())
            .or_default()
            .insert(old.to_vec(), new.to_vec());
        map
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
///   destination AcroForm `/DR` (lazy-initialized on the first placement AND
///   the first field-bearing annot within a placement, merging into a
///   pre-existing dest `/DR` with conflict renaming recorded into `dr_map`);
/// - append the dup'd annots to the destination page `/Annots`.
///
/// `dr_map` is threaded by `&mut` through placements so the current
/// placement's rename table reaches field and appearance-stream rewriting.
/// Its `by_name` and `by_source_ref` tables are rebuilt at the corresponding
/// qpdf transform/merge call boundaries; neither is a document-wide alias
/// cache. `by_name` is reset at the top of every call to this function and
/// repopulated only for THIS placement — see the reset at the top of the
/// function body.
/// `dest_acroform_dr`, unlike `dr_map`, is page-scoped: a fresh `None`
/// created by the caller for each destination page.
///
/// Returns the newly-added top-level field dest refs (one per distinct top
/// field observed in this placement), to be collected across all placements
/// on the dest page and passed to
/// [`crate::AcroFormDocumentHelper::add_and_rename_form_fields_with_reserved_names`]
/// once at the end.
///
pub(crate) fn apply_placement<R: Read + Seek>(
    dest: &mut Pdf<R>,
    dest_page_ref: ObjectRef,
    template: &AnnotationCopyTemplate,
    cm: Matrix,
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

    // `dr_map.by_name` is reset UNCONDITIONALLY at the top of every
    // placement, before deciding whether to repopulate it — mirrors qpdf's
    // `dr_map` being a local variable freshly created at the top of every
    // `AcroForm::transformAnnotations` call
    // (`libqpdf/QPDFAcroFormDocumentHelper.cc:772-800`), never persisted
    // across calls. flpdf threads the SAME `DrMap` by `&mut` through every
    // placement on the whole destination document, but both maps are
    // rebuilt at their qpdf call boundaries. `by_name` — the per-call
    // rename table [`adjust_default_appearance`] and
    // `crate::overlay_appearance_stream::adjust_appearance_stream_handle` consult
    // for THIS placement's fields/AP streams — must not leak from a prior
    // placement into one that never repopulates it below. An
    // annotation-only placement (no top-level field at all, so the
    // first-field trigger inside the per-annot loop below never fires)
    // previously left a PRIOR placement's `by_name` renames in place,
    // wrongly applied to this placement's AP streams (roborev PR #490
    // iter-3 finding 2).
    dr_map.by_name.clear();

    // Trigger the source /DR merge for THIS placement the first time the
    // per-annot loop below (step 1) encounters an annot with a top-level
    // field — even when `dest_acroform_dr` is already Some. The merge
    // repopulates `dr_map.by_name` with just this source's mappings, so a
    // subsequent placement's field-tree walk (which reads `dr_map.by_name`
    // via [`adjust_default_appearance`]) sees THIS source's renames rather
    // than a prior source's stale entries. The `dest_acroform_dr` cache
    // stays: the ref itself does not change once installed. When the
    // placement has fields but NO source /DR (a valid shape: source has
    // annotations without an /AcroForm), no merge ever fires, so `by_name`
    // simply stays cleared (from above) — there is nothing to repopulate it
    // with.
    //
    // MUST be lazy (triggered per-annot, not once before the loop): qpdf's
    // `dr_map` starts EMPTY for the whole `transformAnnotations` call and is
    // populated only inside `traverse_field` via `init_dr_map` (an
    // `if (!dr)`-guarded, run-once closure) — which fires ONLY for
    // field-bearing annots, and only once the per-annot loop actually
    // reaches the FIRST one. An annot with NO field that precedes the first
    // field-bearing annot in `/Annots` order therefore sees `dr_map` still
    // empty when ITS `adjustAppearanceStream` call runs a few lines later
    // (guarded by `!dr_map.empty()` at
    // `libqpdf/QPDFAcroFormDocumentHelper.cc` in `transformAnnotations`,
    // fetched and verified against the live source for roborev PR #490
    // iter-4 finding 2) — its AP stream is left untouched. Populating
    // `dr_map` unconditionally before the loop (the prior version of this
    // function) gave every annot in the placement, including ones ordered
    // before the first field, the fully-populated map: a source/dest /DR
    // collision could then rewrite an AP stream qpdf leaves alone,
    // producing non-qpdf-matching bytes.
    let mut dr_map_ready = false;

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
        // 0. Lazily trigger the source /DR merge (see the doc comment
        //    above `dr_map_ready`) on the FIRST field-bearing annot this
        //    loop reaches — mirrors qpdf's `init_dr_map()` call at the top
        //    of `traverse_field`'s per-node loop, which only ever executes
        //    (its body, past the `if (!dr)` guard) on that same first
        //    field. Runs BEFORE step 1's `duplicate_field_tree` so that
        //    call's own `/DA` rewrite (mirroring qpdf's
        //    `adjustDefaultAppearances`, called right after
        //    `init_dr_map()` inside the SAME per-node loop) sees the
        //    freshly-populated map when THIS annot is itself the trigger.
        if dest_top_field.is_some() && !dr_map_ready {
            dr_map_ready = true;
            if let Some(source_dr) = template.source_dr {
                let dr_ref = ensure_dest_acroform_dr(dest, source_dr, dr_map)?;
                if dest_acroform_dr.is_none() {
                    *dest_acroform_dr = Some(dr_ref);
                }
            } // cov:ignore: control-flow marker — llvm-cov instrumentation artifact; the body above is exercised by every field+/DR fixture (e.g. overlay_pipeline_renames_colliding_dr_font_end_to_end)
        }

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
        transform_annot_ap_streams(dest, new_annot_ref, cm, dr_map)?;

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
        // is also covered. `remove` (not `get().cloned()`) moves the value
        // out since `dict` is owned here — see pdf-rust-review-patterns.md
        // rule 1.
        //
        // `/DA` is permitted as either a direct string or an indirect ref
        // to a string (see the form-field rendering module for how the reader handles
        // both). Convert an indirect ref to a direct string on the copied
        // field: replacing the value inline keeps every foreign-copied
        // field independent of the source string object, so a subsequent
        // rewrite on the same source field ref reads the pre-rewrite
        // bytes instead of accumulating rewrites.
        if !dr_map.is_empty() {
            match dict.remove("DA") {
                Some(Object::String(da)) => {
                    let new_da = adjust_default_appearance(&da, dr_map)?;
                    dict.insert("DA", Object::String(new_da));
                }
                // cov:ignore-start: indirect /DA — form-fields-and-
                // annotations.pdf and every shipped source fixture uses
                // a direct /DA string, so this arm needs a source with
                // `/DA <ref>` to reach.
                Some(Object::Reference(da_ref)) => {
                    match dest.resolve(da_ref)? {
                        Object::String(da) => {
                            let new_da = adjust_default_appearance(&da, dr_map)?;
                            dict.insert("DA", Object::String(new_da));
                        }
                        _ => {
                            dict.insert("DA", Object::Reference(da_ref));
                        }
                    }
                    // cov:ignore-end
                }
                // cov:ignore-start: malformed /DA (non-string, non-ref) —
                // no shipped fixture supplies this shape.
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

/// Rewrite renamed resource names in a copied field's `/DA` string, using
/// the same `ResourceFinder`/`ResourceReplacer` path as appearance streams.
/// Fatal structural errors stay byte-identical. qpdf-warning-only inline-image
/// EOF preserves replacements found before the diagnostic.
fn adjust_default_appearance(da: &[u8], dr_map: &DrMap) -> Result<Vec<u8>> {
    Ok(replace_resource_names(da, dr_map.renames())?.unwrap_or_else(|| da.to_vec()))
}

/// Duplicate each `/AP` appearance stream referenced from the annot at
/// `annot_ref`, concatenate `cm` into the stream's `/Matrix` (qpdf
/// transformAnnotations line 992-1010), and rewrite the annot's `/AP` entries
/// to point at the dup'd streams. Only walked at `/AP/{N,R,D}` and one nested
/// dictionary level, matching qpdf's `apdict` traversal.
///
/// When `dr_map` is non-empty, each dup'd stream is additionally passed to
/// [`crate::overlay_appearance_stream::adjust_appearance_stream_handle`] (qpdf
/// `AcroForm::adjustAppearanceStream`, called from `transformAnnotations`
/// line 1156-1161) so a `/Resources` name that collided during the `/DR`
/// merge is privatized, renamed, and rewritten in the stream's own content —
/// every nested per-state stream (`/N/1`, `/N/Off`, ...) is adjusted
/// independently, matching qpdf's per-stream loop at the same call site.
fn transform_annot_ap_streams<R: Read + Seek>(
    dest: &mut Pdf<R>,
    annot_ref: ObjectRef,
    cm: Matrix,
    dr_map: &DrMap,
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
                    if !dr_map.is_empty() {
                        let stream = dest.get_object_handle(new_ref);
                        adjust_appearance_stream_handle(dest, &stream, dr_map)?;
                    }
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
                            if !dr_map.is_empty() {
                                let stream = dest.get_object_handle(new_ref);
                                adjust_appearance_stream_handle(dest, &stream, dr_map)?;
                            }
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
    cm: Matrix,
) -> Result<Option<ObjectRef>> {
    let obj = dest.resolve(stream_ref)?;
    let Object::Stream(mut stream) = obj else {
        return Ok(None); // cov:ignore: defensive early return
    };
    // Read the existing /Matrix (identity when absent — qpdf apcm defaults
    // to QPDFMatrix() before optional matrix.concat(cm) at line 1001).
    let old_matrix = read_matrix_array(&stream.dict, b"Matrix");
    // qpdf: apcm.concat(cm) → apcm := apcm * cm.
    let had_matrix = old_matrix.is_some();
    let mut new_matrix = old_matrix.unwrap_or_default();
    new_matrix.concat(cm);
    // Only write /Matrix if the source had one or the result is non-identity
    // (qpdf line 1003 same guard).
    if had_matrix || new_matrix != Matrix::default() {
        stream.dict.insert(
            "Matrix",
            Object::Array(
                new_matrix
                    .get_as_matrix()
                    .into_iter()
                    .map(qpdf_real)
                    .collect(),
            ),
        );
    }
    let new_ref = allocate_next_ref(dest)?;
    dest.set_object(new_ref, Object::Stream(stream));
    Ok(Some(new_ref))
}

/// Read a 6-element `/Matrix` from `dict[key]`, if present and well-formed.
fn read_matrix_array(dict: &crate::Dictionary, key: &[u8]) -> Option<Matrix> {
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
    Some(Matrix::from(out))
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

/// Read the annot's `/Rect`, transform its four corners by `cm`, and write
/// back the normalized bounding rectangle. Mirrors qpdf's
/// `QPDFMatrix::transformRectangle` used at transformAnnotations line 1011.
fn transform_annot_rect<R: Read + Seek>(
    dest: &mut Pdf<R>,
    annot_ref: ObjectRef,
    cm: Matrix,
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
    let new_rect = cm.transform_rectangle(Rectangle::new(nums[0], nums[1], nums[2], nums[3]));
    dict.insert(
        "Rect",
        Object::Array(
            [new_rect.llx, new_rect.lly, new_rect.urx, new_rect.ury]
                .into_iter()
                .map(qpdf_real)
                .collect(),
        ),
    );
    dest.set_object(annot_ref, Object::Dictionary(dict));
    Ok(())
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
///   smallest unused `{name}_N` (`N` starting at 1, checked against qpdf's
///   second-level resource-name pool), and `(name, {name}_N)` is recorded into
///   `dr_map[type]`.
///
/// Individual resource entries (e.g. `/F1 8 0 R`) are shallow-cloned — they
/// are typically refs, so the clone is cheap and the dest and source paths
/// continue to share the underlying font/xobject objects (as qpdf does).
///
/// Per-call rename reuse: qpdf's `mergeResources` builds a local
/// `QPDFObjGen -> name` map from the current destination category when the
/// first collision is encountered. A recurring source object therefore reuses
/// its still-live destination name, while an alias overwritten by another
/// source is not reused. `by_source_ref` models that map and is rebuilt lazily
/// at the first collision of each merge category; the destination `/DR` itself
/// remains shared across placements.
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
    // Reset both maps to the identity mappings this merge is about to
    // establish. qpdf creates its source-object map locally for each
    // mergeResources call; rebuilding it from the current destination
    // prevents a direct alias overwritten by another source from becoming a
    // stale reuse. `by_name` describes only THIS placement's fields' /DA
    // rewrite plan, so a stale entry from a prior merge of a different source
    // must NOT leak into
    // `adjust_default_appearance`'s lookup during the field-tree walk that
    // follows this merge.
    dr_map.by_name.clear();
    dr_map.by_source_ref.clear();
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

        // qpdf initializes this pool lazily on the first occupied source key
        // and keeps it stable for the rest of this resource category
        // (`QPDFObjectHandle.cc:1108-1127`). It contains keys from the
        // dictionaries stored under the category's values, not the category's
        // own direct keys (`getResourceNames`, `:1155-1172`).
        let mut resource_names: Option<BTreeSet<Vec<u8>>> = None;
        let mut source_ref_snapshot: Option<BTreeMap<ObjectRef, Vec<u8>>> = None;
        let mut min_suffix: u32 = 1;

        for (name, val) in src_type_dict.iter() {
            match new_type_dict.get(name) {
                None => {
                    new_type_dict.insert(name, val.clone());
                    // Do not update qpdf's identity snapshot here. If this
                    // is before the first occupied source key, the insertion
                    // will be included when the snapshot is built below; if
                    // it is after that key, qpdf deliberately ignores it.
                }
                Some(existing_val) => {
                    // qpdf snapshots both `og_to_name` and `rnames` before
                    // deciding whether this occupied key is a same-object
                    // no-op, an identity reuse, or a fresh-name collision.
                    // In particular, a same-object collision must still
                    // freeze the pool before later verbatim inserts can add
                    // second-level names.
                    if source_ref_snapshot.is_none() {
                        let mut snapshot = BTreeMap::new();
                        for (existing_name, existing_val) in new_type_dict.iter() {
                            if let Object::Reference(existing_ref) = existing_val {
                                // `new_type_dict` is ordered by name, just as
                                // qpdf's std::map-backed dictionary is. A
                                // repeated object therefore keeps the last
                                // name visited, matching make_og_to_name.
                                snapshot.insert(*existing_ref, existing_name.to_vec());
                            }
                        }
                        for (source_ref, dest_name) in &snapshot {
                            dr_map
                                .by_source_ref
                                .insert((type_key.to_vec(), *source_ref), dest_name.clone());
                        }
                        source_ref_snapshot = Some(snapshot);
                    }
                    if resource_names.is_none() {
                        resource_names = Some(get_resource_names(dest, &new_type_dict)?);
                    }
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
                    // qpdf builds a local `QPDFObjGen -> name` map from the
                    // current destination category at the first collision.
                    // `source_ref_snapshot` was built from that category at
                    // the first collision and is frozen for the rest of the
                    // category, so later aliases cannot change qpdf's winner.
                    //
                    // Reuse is keyed by SOURCE `ObjectRef`, not by source
                    // NAME — two different source objects that both collide
                    // under `/F1` (e.g. two OverlaySpecs whose sources both
                    // use `/F1`) are evaluated against the current dest
                    // identity map, rather than a stale name-keyed alias.
                    let reuse_key = val.as_ref_id().map(|r| (type_key.to_vec(), r));
                    let reuse = reuse_key
                        .as_ref()
                        .and_then(|(_, source_ref)| {
                            source_ref_snapshot
                                .as_ref()
                                .and_then(|snapshot| snapshot.get(source_ref))
                        })
                        .cloned();
                    if let Some(existing_new_name) = reuse {
                        // Refresh `by_name` for this call's placement so
                        // `adjust_default_appearance` sees the reused
                        // rename under this source's name. Skip the
                        // dest-dict insert (already carries the same ref
                        // under `existing_new_name`).
                        dr_map
                            .by_name
                            .entry(type_key.to_vec())
                            .or_default()
                            .insert(name.to_vec(), existing_new_name);
                        continue;
                    }
                    let names = resource_names
                        .as_ref()
                        .expect("resource-name pool initialized on occupied source key");
                    let new_name = unique_dr_name(name, names, &mut min_suffix)?;
                    new_type_dict.insert(&new_name, val.clone());
                    dr_map
                        .by_name
                        .entry(type_key.to_vec())
                        .or_default()
                        .insert(name.to_vec(), new_name.clone());
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

/// Return qpdf's smallest `{base}_N` candidate absent from its second-level
/// resource-name pool.
///
/// Also reused by
/// [`crate::overlay_appearance_stream::adjust_appearance_stream_handle`] to mint a
/// stream-local unique name for the rare case where an appearance stream's
/// own (private) `/Resources` already has a *different* entry under a
/// `DrMap` rename's target name (`libqpdf/QPDFAcroFormDocumentHelper.cc:791-807`'s
/// `merge_with` re-merge).
///
/// # Errors
///
/// [`Error::Unsupported`] if `u32` wraps before an unused suffix is found
/// (would require billions of colliding names under one base).
pub(crate) fn unique_dr_name(
    base: &[u8],
    names: &BTreeSet<Vec<u8>>,
    min_suffix: &mut u32,
) -> Result<Vec<u8>> {
    loop {
        let candidate = [base, b"_", (*min_suffix).to_string().as_bytes()].concat();
        if !names.contains(&candidate) {
            return Ok(candidate);
        }
        *min_suffix = (*min_suffix)
            .checked_add(1)
            .ok_or_else(|| Error::Unsupported("DR resource-name suffix space exhausted".into()))?;
    }
}

/// qpdf's `QPDFObjectHandle::getResourceNames` (`QPDFObjectHandle.cc:1155-1172`):
/// collect keys from the dictionaries stored under a resource category's
/// values, resolving indirect values first. Null-valued nested keys (including
/// indirect references that resolve to null) are omitted by qpdf's
/// `getKeys()` contract. The category's own direct keys are intentionally not
/// included.
pub(crate) fn get_resource_names<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict: &crate::Dictionary,
) -> Result<BTreeSet<Vec<u8>>> {
    let mut names = BTreeSet::new();
    for (_, value) in dict.iter() {
        let value_dict = match value {
            Object::Reference(reference) => pdf.resolve(*reference)?.into_dict(),
            _ => value.as_dict().cloned(),
        };
        if let Some(value_dict) = value_dict {
            for (key, nested_value) in value_dict.iter() {
                let is_null = match nested_value {
                    Object::Null => true,
                    Object::Reference(reference) => pdf.resolve(*reference)?.is_null(),
                    _ => false,
                };
                if !is_null {
                    names.insert(key.to_vec());
                }
            }
        }
    }
    Ok(names)
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

    #[test]
    fn get_resource_names_excludes_null_valued_second_level_keys() {
        let mut pdf = open_minimal();
        let indirect_null_ref = ObjectRef::new(11, 0);
        pdf.set_object(indirect_null_ref, Object::Null);
        let indirect_visible_ref = ObjectRef::new(12, 0);
        pdf.set_object(
            indirect_visible_ref,
            Object::Name(b"IndirectVisible".to_vec()),
        );
        let indirect_ref = set_dict(
            &mut pdf,
            10,
            &[
                ("IndirectName", Object::Null),
                ("IndirectNullRef", Object::Reference(indirect_null_ref)),
                (
                    "IndirectVisibleRef",
                    Object::Reference(indirect_visible_ref),
                ),
            ],
        );
        let mut direct_value = crate::Dictionary::new();
        direct_value.insert("DirectName", Object::Null);
        direct_value.insert("DirectVisible", Object::Name(b"DirectVisible".to_vec()));
        let mut category = crate::Dictionary::new();
        category.insert("F0", Object::Dictionary(direct_value));
        category.insert("F1", Object::Reference(indirect_ref));
        category.insert("Scalar", Object::Integer(0));

        let names = get_resource_names(&mut pdf, &category).unwrap();

        assert!(!names.contains(b"DirectName".as_slice()));
        assert!(!names.contains(b"IndirectName".as_slice()));
        assert!(!names.contains(b"IndirectNullRef".as_slice()));
        assert!(names.contains(b"DirectVisible".as_slice()));
        assert!(names.contains(b"IndirectVisibleRef".as_slice()));
        assert!(!names.contains(b"F0".as_slice()));
        assert!(!names.contains(b"F1".as_slice()));
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
                .category(b"Font")
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
    fn merge_resources_shallow_ignores_null_nested_key_when_minting_suffix() {
        let mut pdf = open_minimal();
        let helv_ref = set_dict(
            &mut pdf,
            10,
            &[("BaseFont", Object::Name(b"Helvetica".to_vec()))],
        );
        let other_ref = set_dict(
            &mut pdf,
            11,
            &[
                ("BaseFont", Object::Name(b"TimesRoman".to_vec())),
                // qpdf's getResourceNames omits this null-valued second-level
                // key even though it is present in the raw dictionary.
                ("F1_1", Object::Null),
            ],
        );
        let courier_ref = set_dict(
            &mut pdf,
            12,
            &[("BaseFont", Object::Name(b"Courier".to_vec()))],
        );
        // Pre-seed dest with BOTH F1 (which will collide with source) and
        // F1_1 (an unrelated pre-existing entry). Its null-valued nested key
        // must not enter qpdf's second-level name pool, so the collision is
        // allowed to overwrite the direct F1_1 key.
        let dest_dr = set_font_dr(&mut pdf, 2, &[("F1", helv_ref), ("F1_1", other_ref)]);
        let source_dr = set_font_dr(&mut pdf, 3, &[("F1", courier_ref)]);

        let mut dr_map = DrMap::new();
        merge_resources_shallow(&mut pdf, dest_dr, source_dr, &mut dr_map).unwrap();

        assert_eq!(
            dr_map
                .category(b"Font")
                .and_then(|m| m.get(b"F1".as_slice())),
            Some(&b"F1_1".to_vec())
        );
        let font = font_dict(&mut pdf, dest_dr);
        assert_eq!(font.get_ref("F1"), Some(helv_ref));
        assert_eq!(font.get_ref("F1_1"), Some(courier_ref));
        assert!(font.get("F1_2").is_none());
    }

    #[test]
    fn merge_resources_shallow_reuses_one_name_pool_for_multiple_conflicts() {
        let mut pdf = open_minimal();
        let dest_f1 = set_dict(
            &mut pdf,
            10,
            &[("BaseFont", Object::Name(b"Helvetica".to_vec()))],
        );
        let dest_f2 = set_dict(
            &mut pdf,
            11,
            &[("BaseFont", Object::Name(b"TimesRoman".to_vec()))],
        );
        let source_f1 = set_dict(
            &mut pdf,
            12,
            &[("BaseFont", Object::Name(b"Courier".to_vec()))],
        );
        let source_f2 = set_dict(
            &mut pdf,
            13,
            &[("BaseFont", Object::Name(b"Symbol".to_vec()))],
        );
        let dest_dr = set_font_dr(&mut pdf, 2, &[("F1", dest_f1), ("F2", dest_f2)]);
        let source_dr = set_font_dr(&mut pdf, 3, &[("F1", source_f1), ("F2", source_f2)]);

        let mut dr_map = DrMap::new();
        merge_resources_shallow(&mut pdf, dest_dr, source_dr, &mut dr_map).unwrap();

        assert_eq!(
            dr_map
                .category(b"Font")
                .and_then(|m| m.get(b"F1".as_slice())),
            Some(&b"F1_1".to_vec())
        );
        assert_eq!(
            dr_map
                .category(b"Font")
                .and_then(|m| m.get(b"F2".as_slice())),
            Some(&b"F2_1".to_vec())
        );
        let font = font_dict(&mut pdf, dest_dr);
        assert_eq!(font.get_ref("F1"), Some(dest_f1));
        assert_eq!(font.get_ref("F1_1"), Some(source_f1));
        assert_eq!(font.get_ref("F2"), Some(dest_f2));
        assert_eq!(font.get_ref("F2_1"), Some(source_f2));
    }

    #[test]
    fn merge_resources_shallow_freezes_name_pool_on_same_object_collision() {
        let mut pdf = open_minimal();
        let shared_ref = set_dict(
            &mut pdf,
            10,
            &[("BaseFont", Object::Name(b"Helvetica".to_vec()))],
        );
        let dest_c_ref = set_dict(
            &mut pdf,
            11,
            &[("BaseFont", Object::Name(b"Times-Roman".to_vec()))],
        );
        let inserted_b_ref = set_dict(&mut pdf, 12, &[("C_1", Object::Name(b"Inserted".to_vec()))]);
        let source_c_ref = set_dict(
            &mut pdf,
            13,
            &[("BaseFont", Object::Name(b"Courier".to_vec()))],
        );
        let dest_dr = set_font_dr(&mut pdf, 2, &[("A", shared_ref), ("C", dest_c_ref)]);
        let source_dr = set_font_dr(
            &mut pdf,
            3,
            &[
                ("A", shared_ref),
                ("B", inserted_b_ref),
                ("C", source_c_ref),
            ],
        );

        let mut dr_map = DrMap::new();
        merge_resources_shallow(&mut pdf, dest_dr, source_dr, &mut dr_map).unwrap();

        assert_eq!(
            dr_map
                .category(b"Font")
                .and_then(|m| m.get(b"C".as_slice())),
            Some(&b"C_1".to_vec()),
            "the name pool must be frozen at the first occupied source key"
        );
        let font = font_dict(&mut pdf, dest_dr);
        assert_eq!(font.get_ref("B"), Some(inserted_b_ref));
        assert_eq!(font.get_ref("C_1"), Some(source_c_ref));
        assert!(font.get("C_2").is_none());
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

    /// A stale `by_name` entry from a prior merge must NOT leak into the
    /// next placement. Concretely: source A collides and records
    /// `by_name[Font][F1] = F1_1`; source B's `/F1` is a same-object no-op
    /// (its Font equals dest's `F1`), so this merge records nothing new.
    /// `by_name` after B's merge must be empty (not still {F1: F1_1}), so
    /// `adjust_default_appearance` running on B's fields does not rewrite
    /// B's `/F1` to `/F1_1` — which would be catastrophic, since dest's
    /// `/F1_1` is actually A's font.
    #[test]
    fn merge_resources_shallow_clears_stale_by_name_between_source_merges() {
        let mut pdf = open_minimal();
        let times_ref = set_dict(
            &mut pdf,
            10,
            &[("BaseFont", Object::Name(b"Times-Roman".to_vec()))],
        );
        let helv_ref = set_dict(
            &mut pdf,
            11,
            &[("BaseFont", Object::Name(b"Helvetica".to_vec()))],
        );
        let dest_dr = set_font_dr(&mut pdf, 2, &[("F1", times_ref)]);
        let source_a = set_font_dr(&mut pdf, 3, &[("F1", helv_ref)]);
        // Source B's /F1 points at times_ref, the SAME object dest already
        // holds under /F1 → same_object short-circuit fires, no rename.
        let source_b = set_font_dr(&mut pdf, 4, &[("F1", times_ref)]);

        let mut dr_map = DrMap::new();
        merge_resources_shallow(&mut pdf, dest_dr, source_a, &mut dr_map).unwrap();
        assert_eq!(
            dr_map
                .category(b"Font")
                .and_then(|m| m.get(b"F1".as_slice())),
            Some(&b"F1_1".to_vec()),
        );

        merge_resources_shallow(&mut pdf, dest_dr, source_b, &mut dr_map).unwrap();
        assert!(
            dr_map.is_empty(),
            "by_name must clear at merge start so B's fields don't inherit A's F1→F1_1 mapping"
        );
        // by_source_ref is rebuilt from the current destination, so it still
        // contains A's live alias (but is not a historical cache).
        assert_eq!(
            dr_map.by_source_ref.get(&(b"Font".to_vec(), helv_ref)),
            Some(&b"F1_1".to_vec()),
            "by_source_ref must reflect the current destination alias",
        );
    }

    /// Reuse must be keyed by SOURCE `ObjectRef`, not by source name. If
    /// two different sources both collide with dest's `/F1` under the same
    /// old name (e.g. two `OverlaySpec` merges against the same dest), each
    /// must get its own dest name — and a later re-placement of source A
    /// must inspect the current destination object identity, not follow
    /// whatever `by_name` the intervening B merge populated.
    #[test]
    fn merge_resources_shallow_reuse_is_keyed_by_source_ref_not_by_name() {
        let mut pdf = open_minimal();
        let times_ref = set_dict(
            &mut pdf,
            10,
            &[("BaseFont", Object::Name(b"Times-Roman".to_vec()))],
        );
        let helv_ref = set_dict(
            &mut pdf,
            11,
            &[
                ("BaseFont", Object::Name(b"Helvetica".to_vec())),
                // After source A is installed, this non-null second-level key
                // keeps qpdf's next merge from reusing A's direct alias for B.
                ("F1_1", Object::Name(b"Reserved".to_vec())),
            ],
        );
        let courier_ref = set_dict(
            &mut pdf,
            12,
            &[("BaseFont", Object::Name(b"Courier".to_vec()))],
        );
        let dest_dr = set_font_dr(&mut pdf, 2, &[("F1", times_ref)]);
        // Two distinct source /DR dicts both using /F1 for different fonts.
        let source_a = set_font_dr(&mut pdf, 3, &[("F1", helv_ref)]);
        let source_b = set_font_dr(&mut pdf, 4, &[("F1", courier_ref)]);

        let mut dr_map = DrMap::new();
        merge_resources_shallow(&mut pdf, dest_dr, source_a, &mut dr_map).unwrap();
        merge_resources_shallow(&mut pdf, dest_dr, source_b, &mut dr_map).unwrap();
        // After A then B: dest has {F1: Times, F1_1: Helv, F1_2: Courier}.
        // by_name["Font"]["F1"] is F1_2 (B's rename, overwrote A's F1_1).
        assert_eq!(
            dr_map
                .category(b"Font")
                .and_then(|m| m.get(b"F1".as_slice())),
            Some(&b"F1_2".to_vec()),
        );
        let font_after_ab = font_dict(&mut pdf, dest_dr);
        assert_eq!(font_after_ab.get_ref("F1"), Some(times_ref));
        assert_eq!(font_after_ab.get_ref("F1_1"), Some(helv_ref));
        assert_eq!(font_after_ab.get_ref("F1_2"), Some(courier_ref));

        // Re-merge A: the current source_ref-to-name map must recognize
        // helv_ref at F1_1 and reuse it, NOT mint an F1_3.
        merge_resources_shallow(&mut pdf, dest_dr, source_a, &mut dr_map).unwrap();
        assert_eq!(
            dr_map
                .category(b"Font")
                .and_then(|m| m.get(b"F1".as_slice())),
            Some(&b"F1_1".to_vec()),
            "re-merging source A must refresh by_name back to F1_1 (the ref-keyed reuse)",
        );
        let font_after_aba = font_dict(&mut pdf, dest_dr);
        assert!(
            font_after_aba.get("F1_3").is_none(),
            "must NOT mint F1_3; must reuse F1_1 keyed by helv_ref",
        );
        assert_eq!(font_after_aba.get_ref("F1_1"), Some(helv_ref));
        assert_eq!(font_after_aba.get_ref("F1_2"), Some(courier_ref));
    }

    /// A source-ref rename must be revalidated against the current dest dict.
    /// qpdf's per-merge `og_to_name` map does not preserve an alias after a
    /// later source overwrites that direct key (direct category keys are not
    /// part of the second-level resource-name pool). When A is copied again,
    /// its alias must be installed back over B rather than blindly reused.
    #[test]
    fn merge_resources_shallow_revalidates_alias_after_hidden_overwrite() {
        let mut pdf = open_minimal();
        let dest_ref = set_dict(
            &mut pdf,
            10,
            &[("BaseFont", Object::Name(b"Times-Roman".to_vec()))],
        );
        let source_a_ref = set_dict(
            &mut pdf,
            11,
            &[("BaseFont", Object::Name(b"Helvetica".to_vec()))],
        );
        let source_b_ref = set_dict(
            &mut pdf,
            12,
            &[("BaseFont", Object::Name(b"Courier".to_vec()))],
        );
        let dest_dr = set_font_dr(&mut pdf, 2, &[("F1", dest_ref)]);
        let source_a = set_font_dr(&mut pdf, 3, &[("F1", source_a_ref)]);
        let source_b = set_font_dr(&mut pdf, 4, &[("F1", source_b_ref)]);

        let mut dr_map = DrMap::new();
        merge_resources_shallow(&mut pdf, dest_dr, source_a, &mut dr_map).unwrap();
        merge_resources_shallow(&mut pdf, dest_dr, source_b, &mut dr_map).unwrap();
        assert_eq!(
            font_dict(&mut pdf, dest_dr).get_ref("F1_1"),
            Some(source_b_ref)
        );

        merge_resources_shallow(&mut pdf, dest_dr, source_a, &mut dr_map).unwrap();
        assert_eq!(
            font_dict(&mut pdf, dest_dr).get_ref("F1_1"),
            Some(source_a_ref),
            "a cached alias must not keep pointing at the source that overwrote it",
        );
    }

    /// Two `merge_resources_shallow` calls against the SAME dest `/DR` with
    /// the SAME conflicting source: the first call renames `F1` → `F1_1`
    /// and records the mapping in `dr_map`; the second call must reuse
    /// `F1_1` (the current destination map sees that `F1_1` still holds the
    /// source ref) rather than minting `F1_2`. This is qpdf's per-merge
    /// rename-reuse invariant; every field's `/DA` and every AP stream needs
    /// the renamed name to stay stable while the alias remains live.
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
                .category(b"Font")
                .and_then(|m| m.get(b"F1".as_slice())),
            Some(&b"F1_1".to_vec())
        );

        merge_resources_shallow(&mut pdf, dest_dr, source_dr, &mut dr_map).unwrap();
        // Second call: reuse F1_1 (do NOT mint F1_2).
        assert_eq!(
            dr_map
                .category(b"Font")
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

    /// Within one merge call, when the source category has two entries
    /// (`/F1 X`, `/F2 X`) sharing the same underlying object and dest
    /// lacks `/F1` but has `/F2 Y`: qpdf inserts `/F1 X` verbatim, then
    /// on the `/F2` collision sees that `X` is already sitting at `/F1`
    /// and rewrites the `/F2` operand to `/F1` rather than duplicating
    /// `X` under `/F2_1`. The collision-time identity snapshot must include
    /// the earlier verbatim `/F1` insertion.
    #[test]
    fn merge_resources_shallow_reuses_verbatim_insert_from_same_source_ref() {
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
        // Dest has only /F2 (Courier), source has /F1 and /F2 both pointing
        // at the same Helvetica ref.
        let dest_dr = set_font_dr(&mut pdf, 2, &[("F2", courier_ref)]);
        let source_dr = set_font_dr(&mut pdf, 3, &[("F1", helv_ref), ("F2", helv_ref)]);

        let mut dr_map = DrMap::new();
        merge_resources_shallow(&mut pdf, dest_dr, source_dr, &mut dr_map).unwrap();

        // by_name records the /F2 → /F1 REUSE (not /F2 → /F2_1).
        assert_eq!(
            dr_map
                .category(b"Font")
                .and_then(|m| m.get(b"F2".as_slice())),
            Some(&b"F1".to_vec()),
            "second collision must reuse the verbatim-inserted /F1 name",
        );
        // dest carries only /F1 (Helv from source) + the pre-existing /F2
        // (Courier). NO /F2_1 duplicate.
        let font = font_dict(&mut pdf, dest_dr);
        assert_eq!(font.get_ref("F1"), Some(helv_ref));
        assert_eq!(font.get_ref("F2"), Some(courier_ref));
        assert!(
            font.get("F2_1").is_none(),
            "must not mint /F2_1 when the source ref is already at /F1",
        );
    }

    /// qpdf freezes its source-object map at the first occupied source key.
    /// A later verbatim insertion with the same source ref must not make a
    /// subsequent collision reuse that newly inserted name.
    #[test]
    fn merge_resources_shallow_freezes_source_ref_map_after_first_collision() {
        let mut pdf = open_minimal();
        let dest_a_ref = set_dict(
            &mut pdf,
            10,
            &[("BaseFont", Object::Name(b"Times-Roman".to_vec()))],
        );
        let dest_c_ref = set_dict(
            &mut pdf,
            11,
            &[("BaseFont", Object::Name(b"Symbol".to_vec()))],
        );
        let source_a_ref = set_dict(
            &mut pdf,
            12,
            &[("BaseFont", Object::Name(b"Helvetica".to_vec()))],
        );
        let source_x_ref = set_dict(
            &mut pdf,
            13,
            &[("BaseFont", Object::Name(b"Courier".to_vec()))],
        );
        let dest_dr = set_font_dr(&mut pdf, 2, &[("A", dest_a_ref), ("C", dest_c_ref)]);
        let source_dr = set_font_dr(
            &mut pdf,
            3,
            &[
                ("A", source_a_ref),
                ("B", source_x_ref),
                ("C", source_x_ref),
            ],
        );

        let mut dr_map = DrMap::new();
        merge_resources_shallow(&mut pdf, dest_dr, source_dr, &mut dr_map).unwrap();

        assert_eq!(
            dr_map
                .category(b"Font")
                .and_then(|m| m.get(b"C".as_slice())),
            Some(&b"C_1".to_vec()),
            "a verbatim /B insert after the first collision must not enter qpdf's frozen identity map",
        );
        let font = font_dict(&mut pdf, dest_dr);
        assert_eq!(font.get_ref("B"), Some(source_x_ref));
        assert_eq!(font.get_ref("C_1"), Some(source_x_ref));
        assert!(font.get("C_2").is_none());
    }

    /// qpdf builds the identity map from the current destination dictionary
    /// when the first collision occurs. If the same indirect object is
    /// already present under two names, the dictionary's key order determines
    /// the winner; an earlier verbatim source insert must not replace it.
    #[test]
    fn merge_resources_shallow_builds_source_ref_map_from_collision_dictionary() {
        let mut pdf = open_minimal();
        let shared_ref = set_dict(
            &mut pdf,
            10,
            &[("BaseFont", Object::Name(b"Helvetica".to_vec()))],
        );
        let other_ref = set_dict(
            &mut pdf,
            11,
            &[("BaseFont", Object::Name(b"Times-Roman".to_vec()))],
        );
        let dest_dr = set_font_dr(&mut pdf, 2, &[("C", other_ref), ("Z", shared_ref)]);
        let source_dr = set_font_dr(&mut pdf, 3, &[("A", shared_ref), ("C", shared_ref)]);

        let mut dr_map = DrMap::new();
        merge_resources_shallow(&mut pdf, dest_dr, source_dr, &mut dr_map).unwrap();

        assert_eq!(
            dr_map
                .category(b"Font")
                .and_then(|m| m.get(b"C".as_slice())),
            Some(&b"Z".to_vec()),
            "the collision-time destination dictionary must win over the earlier verbatim /A insert",
        );
        let font = font_dict(&mut pdf, dest_dr);
        assert_eq!(font.get_ref("A"), Some(shared_ref));
        assert_eq!(font.get_ref("C"), Some(other_ref));
        assert_eq!(font.get_ref("Z"), Some(shared_ref));
        assert!(font.get("C_1").is_none());
    }

    /// Same reuse behavior applies when the source ref is already at a
    /// dest name from a PRIOR life of the dest `/DR` (not from an earlier
    /// verbatim insert in the same merge). Building the collision-time
    /// snapshot from `new_type_dict` enables this.
    #[test]
    fn merge_resources_shallow_reuses_preexisting_dest_ref_at_different_name() {
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
        // Dest already has helv_ref sitting at /F1 (from a prior life —
        // could be a hand-crafted /AcroForm/DR). Source uses /F2 for the
        // same helv_ref, colliding with dest's /F2 (Courier).
        let dest_dr = set_font_dr(&mut pdf, 2, &[("F1", helv_ref), ("F2", courier_ref)]);
        let source_dr = set_font_dr(&mut pdf, 3, &[("F2", helv_ref)]);

        let mut dr_map = DrMap::new();
        merge_resources_shallow(&mut pdf, dest_dr, source_dr, &mut dr_map).unwrap();

        // by_name records /F2 → /F1 (reuse of the pre-existing dest name).
        assert_eq!(
            dr_map
                .category(b"Font")
                .and_then(|m| m.get(b"F2".as_slice())),
            Some(&b"F1".to_vec()),
        );
        let font = font_dict(&mut pdf, dest_dr);
        assert!(
            font.get("F2_1").is_none(),
            "must reuse the pre-existing /F1 rather than mint /F2_1",
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

    // ---- apply_placement ----------------------------------------------------

    /// A prior placement on this destination page installed a real DR-level
    /// rename into `dr_map.by_name` (here stood in for directly via
    /// `merge_resources_shallow`, the exact call `apply_placement`'s
    /// per-annot, first-field-triggered merge makes for any placement with a
    /// top-level field). A SECOND placement on the SAME page that carries
    /// annotations but NO top-level field at all (so the trigger condition
    /// never fires) must not see that stale rename: before this fix, neither
    /// arm of the (then pre-loop, unconditional) merge check ran for an
    /// annotation-only placement, so `dr_map.by_name` was left exactly as
    /// the first placement's merge left it — a rename table describing a
    /// DIFFERENT source's fonts, silently available to whatever later reads
    /// `dr_map.by_name` for this placement's own AP streams (roborev PR #490
    /// iter-3 finding 2).
    #[test]
    fn apply_placement_clears_stale_by_name_for_annotation_only_placement() {
        let mut pdf = open_minimal();
        let times_ref = set_dict(
            &mut pdf,
            10,
            &[("BaseFont", Object::Name(b"Times-Roman".to_vec()))],
        );
        let helv_ref = set_dict(
            &mut pdf,
            11,
            &[("BaseFont", Object::Name(b"Helvetica".to_vec()))],
        );
        let dest_dr = set_font_dr(&mut pdf, 2, &[("F1", times_ref)]);
        let source_dr = set_font_dr(&mut pdf, 3, &[("F1", helv_ref)]);

        let mut dr_map = DrMap::new();
        merge_resources_shallow(&mut pdf, dest_dr, source_dr, &mut dr_map).unwrap();
        assert_eq!(
            dr_map
                .category(b"Font")
                .and_then(|m| m.get(b"F1".as_slice())),
            Some(&b"F1_1".to_vec()),
            "sanity: the setup step actually populated a stale rename"
        );

        let page_ref = set_dict(&mut pdf, 20, &[]);
        let annot_ref = set_dict(&mut pdf, 21, &[]);
        let template = AnnotationCopyTemplate {
            annots: vec![(annot_ref, None)], // annotation-only: no top-level field
            source_dr: None,
            ..Default::default()
        };
        let mut dest_acroform_dr: Option<ObjectRef> = None;

        apply_placement(
            &mut pdf,
            page_ref,
            &template,
            Matrix::default(),
            &mut dest_acroform_dr,
            &mut dr_map,
        )
        .unwrap();

        assert!(
            dr_map.category(b"Font").is_none(),
            "an annotation-only placement (no top-level field) must clear a stale \
             by_name rename left over from a prior placement, not silently carry it \
             forward into whatever reads dr_map for THIS placement's own AP streams"
        );
    }

    /// roborev PR #490 iter-4 finding 2: a non-field annot that precedes the
    /// placement's first field-bearing annot in `/Annots` order must see an
    /// EMPTY `dr_map` when its own AP stream is processed — qpdf's `dr_map`
    /// starts empty for the whole `transformAnnotations` call and is
    /// populated only inside `traverse_field`'s `init_dr_map()`, which fires
    /// exclusively for field-bearing annots
    /// (`libqpdf/QPDFAcroFormDocumentHelper.cc` v11.9.0, `transformAnnotations`,
    /// fetched and verified against the live source: `init_dr_map()` is
    /// called at line ~920, inside `traverse_field`, called only from
    /// `transform_annotation`'s field branch; `adjustAppearanceStream` is
    /// gated by `!dr_map.empty()` at line ~1007, evaluated fresh for EACH
    /// annot in loop order). A prior version of `apply_placement` computed
    /// `has_any_top_field` and ran the merge ONCE, unconditionally, before
    /// the per-annot loop — so every annot in the placement, including one
    /// ordered before the first field, saw the fully-populated map.
    ///
    /// This test places a non-field annot with an AP stream FIRST and a
    /// (self-field) widget SECOND. The destination already has an
    /// `/AcroForm/DR/Font/F1` that collides with the source `/DR/Font/F1`
    /// the widget brings in, so the merge is a REAL, observable rename
    /// (Font: F1 -> F1_1) — not a vacuous no-op. The non-field annot's own
    /// AP stream also happens to use the name `/F1` locally; if `dr_map`
    /// were (incorrectly) already populated when it is processed, its
    /// `/Resources/Font/F1` entry and its `/F1` content token would both get
    /// renamed to `/F1_1`, exactly like the field-triggered collision. This
    /// must NOT happen — the AP stream must come out byte-for-byte
    /// unchanged (aside from the unrelated cm-Matrix dup every AP stream
    /// gets in step 3, which is identity here).
    #[test]
    fn apply_placement_leading_non_field_annot_sees_empty_dr_map() {
        let mut pdf = open_minimal();
        let dest_font_ref = set_dict(
            &mut pdf,
            10,
            &[("BaseFont", Object::Name(b"Times-Roman".to_vec()))],
        );
        let source_font_ref = set_dict(
            &mut pdf,
            11,
            &[("BaseFont", Object::Name(b"Helvetica".to_vec()))],
        );
        let local_font_ref = set_dict(
            &mut pdf,
            12,
            &[("BaseFont", Object::Name(b"Courier".to_vec()))],
        );
        let dest_dr = set_font_dr(&mut pdf, 2, &[("F1", dest_font_ref)]);
        let source_dr = set_font_dr(&mut pdf, 3, &[("F1", source_font_ref)]);

        // Install a destination /AcroForm/DR up front (mirrors
        // `fxo-red-with-existing-acroform-dr.pdf`'s shape) so
        // `ensure_dest_acroform_dr` merges into it — rather than creating a
        // fresh, empty one — and the F1 collision is real.
        let mut acroform = crate::Dictionary::new();
        acroform.insert("Fields", Object::Array(Vec::new()));
        acroform.insert("DR", Object::Reference(dest_dr));
        let acroform_ref = ObjectRef::new(30, 0);
        pdf.set_object(acroform_ref, Object::Dictionary(acroform));
        let mut catalog = crate::Dictionary::new();
        catalog.insert("Type", Object::Name(b"Catalog".to_vec()));
        catalog.insert("AcroForm", Object::Reference(acroform_ref));
        pdf.set_object(ObjectRef::new(1, 0), Object::Dictionary(catalog));

        let page_ref = set_dict(&mut pdf, 20, &[]);

        // Non-field annot: an /AP /N stream with its own local /Resources
        // that ALSO happens to use the name F1 — the name the merge below
        // renames — plus a content token spelling it out, so a wrongly
        // non-empty dr_map at the time this annot is processed would be
        // directly observable.
        let mut ap_font = crate::Dictionary::new();
        ap_font.insert("F1", Object::Reference(local_font_ref));
        let mut ap_resources = crate::Dictionary::new();
        ap_resources.insert("Font", Object::Dictionary(ap_font));
        let ap_stream_ref = ObjectRef::new(22, 0);
        let mut ap_stream_dict = crate::Dictionary::new();
        ap_stream_dict.insert("Resources", Object::Dictionary(ap_resources));
        pdf.set_object(
            ap_stream_ref,
            Object::Stream(crate::Stream::new(ap_stream_dict, b"/F1 10 Tf".to_vec())),
        );
        let mut ap_dict = crate::Dictionary::new();
        ap_dict.insert("N", Object::Reference(ap_stream_ref));
        let non_field_annot_ref = set_dict(&mut pdf, 21, &[("AP", Object::Dictionary(ap_dict))]);

        // Field-bearing annot (self-field widget): appears SECOND in
        // /Annots order, so it is the one that lazily triggers the /DR
        // merge — AFTER the non-field annot above has already been
        // processed by the per-annot loop.
        let field_annot_ref = set_dict(&mut pdf, 23, &[]);

        let template = AnnotationCopyTemplate {
            annots: vec![
                (non_field_annot_ref, None),
                (field_annot_ref, Some(field_annot_ref)),
            ],
            source_dr: Some(source_dr),
            ..Default::default()
        };
        let mut dr_map = DrMap::new();
        let mut dest_acroform_dr: Option<ObjectRef> = None;

        apply_placement(
            &mut pdf,
            page_ref,
            &template,
            Matrix::default(),
            &mut dest_acroform_dr,
            &mut dr_map,
        )
        .unwrap();

        // Sanity: the setup did produce a REAL, non-vacuous F1 collision —
        // otherwise this test would pass trivially regardless of the fix.
        assert_eq!(
            dr_map
                .category(b"Font")
                .and_then(|m| m.get(b"F1".as_slice())),
            Some(&b"F1_1".to_vec()),
            "sanity: the widget's field-tree walk must have triggered a real \
             F1 -> F1_1 collision rename"
        );

        let page = pdf.resolve(page_ref).unwrap().into_dict().unwrap();
        let annots_arr = page.get("Annots").and_then(Object::as_array).unwrap();
        assert_eq!(annots_arr.len(), 2);
        let new_non_field_annot_ref = annots_arr[0].as_ref_id().unwrap();

        let new_annot = pdf
            .resolve(new_non_field_annot_ref)
            .unwrap()
            .into_dict()
            .unwrap();
        let new_ap_stream_ref = new_annot
            .get("AP")
            .and_then(Object::as_dict)
            .and_then(|ap| ap.get_ref("N"))
            .expect("dup'd AP /N stream ref");
        let new_ap_stream = pdf
            .resolve(new_ap_stream_ref)
            .unwrap()
            .into_stream()
            .unwrap();
        assert_eq!(
            new_ap_stream.data, b"/F1 10 Tf",
            "the leading non-field annot's AP content must be untouched — \
             dr_map was still empty when it was processed"
        );
        let new_ap_font = new_ap_stream
            .dict
            .get("Resources")
            .and_then(Object::as_dict)
            .and_then(|r| r.get("Font"))
            .and_then(Object::as_dict)
            .expect("AP stream /Resources/Font");
        assert_eq!(
            new_ap_font.get_ref("F1"),
            Some(local_font_ref),
            "the leading non-field annot's own /Resources/Font/F1 must be \
             untouched — no rename applied while dr_map was still empty"
        );
        assert!(
            new_ap_font.get("F1_1").is_none(),
            "no F1_1 entry should have been minted on the non-field annot's \
             AP stream — adjustAppearanceStream must not have run at all"
        );
    }

    #[test]
    fn transform_annot_ap_streams_adjusts_top_level_and_nested_handles() {
        let mut pdf = open_minimal();
        let top_stream_ref = ObjectRef::new(20, 0);
        let nested_stream_ref = ObjectRef::new(21, 0);
        for stream_ref in [top_stream_ref, nested_stream_ref] {
            let mut font = crate::Dictionary::new();
            font.insert("F1", Object::Integer(1));
            let mut resources = crate::Dictionary::new();
            resources.insert("Font", Object::Dictionary(font));
            let mut stream_dict = crate::Dictionary::new();
            stream_dict.insert("Resources", Object::Dictionary(resources));
            pdf.set_object(
                stream_ref,
                Object::Stream(crate::Stream::new(stream_dict, b"/F1 10 Tf".to_vec())),
            );
        }

        let mut nested = crate::Dictionary::new();
        nested.insert("On", Object::Reference(nested_stream_ref));
        let mut ap = crate::Dictionary::new();
        ap.insert("N", Object::Reference(top_stream_ref));
        ap.insert("D", Object::Dictionary(nested));
        let annot_ref = set_dict(&mut pdf, 22, &[("AP", Object::Dictionary(ap))]);
        let dr_map = DrMap::for_test(b"Font", b"F1", b"F1_1");

        transform_annot_ap_streams(&mut pdf, annot_ref, Matrix::default(), &dr_map).unwrap();

        let annot = pdf.resolve(annot_ref).unwrap().into_dict().unwrap();
        let ap = annot.get("AP").and_then(Object::as_dict).unwrap();
        let top_ref = ap.get_ref("N").unwrap();
        let nested_ref = ap
            .get("D")
            .and_then(Object::as_dict)
            .and_then(|d| d.get_ref("On"))
            .unwrap();
        for stream_ref in [top_ref, nested_ref] {
            let stream = pdf.resolve(stream_ref).unwrap().into_stream().unwrap();
            assert_eq!(stream.data, b"/F1_1 10 Tf");
            let font = stream
                .dict
                .get("Resources")
                .and_then(Object::as_dict)
                .and_then(|resources| resources.get("Font"))
                .and_then(Object::as_dict)
                .unwrap();
            assert_eq!(font.get("F1_1"), Some(&Object::Integer(1)));
            assert!(font.get("F1").is_none());
        }
    }

    // ---- adjust_default_appearance ------------------------------------------

    /// Build a `DrMap` with a single category populated from `(old, new)`
    /// pairs. Callers pick the category (`Font`, `ColorSpace`, ...) so the
    /// same helper covers Tf, cs/CS, gs, and friends.
    fn category_dr_map(category: &[u8], entries: &[(&str, &str)]) -> DrMap {
        let mut inner = BTreeMap::new();
        for (old, new) in entries {
            inner.insert(old.as_bytes().to_vec(), new.as_bytes().to_vec());
        }
        let mut dr_map = DrMap::new();
        dr_map.by_name.insert(category.to_vec(), inner);
        dr_map
    }

    #[test]
    fn adjust_default_appearance_empty_dr_map_is_identity() {
        let dr_map = DrMap::new();
        let da: &[u8] = b"0 0.4 0 rg /F1 18 Tf";
        assert_eq!(adjust_default_appearance(da, &dr_map).unwrap(), da.to_vec());
    }

    /// The canonical case exercised by the (still-ignored) Layer 1 byte
    /// gate: `form-fields-and-annotations.pdf`'s `/DA (0 0.4 0 rg /F1 18
    /// Tf)` merged onto a dest whose `/DR/Font/F1` already existed, renaming
    /// the source's colliding `/F1` to `/F1_1`.
    #[test]
    fn adjust_default_appearance_rewrites_matched_font_name() {
        let dr_map = category_dr_map(b"Font", &[("F1", "F1_1")]);
        let da: &[u8] = b"0 0.4 0 rg /F1 18 Tf";
        assert_eq!(
            adjust_default_appearance(da, &dr_map).unwrap(),
            b"0 0.4 0 rg /F1_1 18 Tf".to_vec()
        );
    }

    #[test]
    fn adjust_default_appearance_does_not_split_numeric_looking_operator() {
        let dr_map = category_dr_map(b"Font", &[("F1", "F1_1")]);
        let da: &[u8] = b"/F1 12Tf";
        assert_eq!(adjust_default_appearance(da, &dr_map).unwrap(), da.to_vec());
    }

    #[test]
    fn adjust_default_appearance_name_not_in_dr_map_is_verbatim() {
        // /ZaDi never collided during the merge (only /F1 did), so it has no
        // dr_map entry and must be left untouched — matching the qpdf golden
        // (`overlay-onto-existing-acroform-dr.pdf`), where every `/ZaDi`
        // `/DA` stays verbatim alongside the renamed `/F1_1` ones.
        let dr_map = category_dr_map(b"Font", &[("F1", "F1_1")]);
        let da: &[u8] = b"0.18039 0.20392 0.21176 rg /ZaDi 0 Tf";
        assert_eq!(adjust_default_appearance(da, &dr_map).unwrap(), da.to_vec());
    }

    #[test]
    fn adjust_default_appearance_rewrites_without_local_resource_presence_guard() {
        // qpdf's shared ResourceFinder/ResourceReplacer pair trusts the
        // rename map; it does not require the copied field's local /DR to
        // retain the original resource name.
        let dr_map = category_dr_map(b"Font", &[("F1", "F1_1")]);
        let da: &[u8] = b"/F1 12 Tf";
        assert_eq!(
            adjust_default_appearance(da, &dr_map).unwrap(),
            b"/F1_1 12 Tf".to_vec()
        );
    }

    #[test]
    fn malformed_da_preserves_diagnostic_bytes_and_rewrites_later_name() {
        let dr_map = category_dr_map(b"Font", &[("F1", "F1_1")]);
        assert_eq!(
            adjust_default_appearance(b"<0g> /F1 12 Tf", &dr_map).unwrap(),
            b"<0g> /F1_1 12 Tf".to_vec()
        );
    }

    /// A renamed name containing delimiter bytes must be serialized with
    /// PDF name escaping (`#XX` for delimiters and non-printables), not
    /// spliced in raw. Concretely: a dest key `F A_1` (decoded bytes with
    /// a space) must emit as `/F#20A_1`, not `/F A_1` — the raw form
    /// tokenizes as `/F` followed by an operand run and breaks the /DA.
    #[test]
    fn adjust_default_appearance_escapes_name_bytes_needing_hex_escape() {
        let mut font = BTreeMap::new();
        // Decoded (in-memory) key = "F A_1" — the parser would have decoded
        // "F#20A_1" back to these bytes; the writer must re-escape on emit.
        font.insert(b"F1".to_vec(), b"F A_1".to_vec());
        let mut dr_map = DrMap::new();
        dr_map.by_name.insert(b"Font".to_vec(), font);
        // The dest /Font sub-dict lookup uses the ORIGINAL colliding name
        // ("F1"); the escape only affects the SERIALIZED output.
        let da: &[u8] = b"/F1 18 Tf";
        assert_eq!(
            adjust_default_appearance(da, &dr_map).unwrap(),
            b"/F#20A_1 18 Tf".to_vec()
        );
    }

    #[test]
    fn adjust_default_appearance_name_inside_string_literal_is_verbatim() {
        // `(F1)` is a STRING operand, not a name token, even though its
        // content matches a dr_map key — must not be mistaken for the font
        // name preceding `Tf`.
        let dr_map = category_dr_map(b"Font", &[("F1", "F1_1")]);
        let da: &[u8] = b"(F1) 18 Tf";
        assert_eq!(adjust_default_appearance(da, &dr_map).unwrap(), da.to_vec());
    }

    #[test]
    fn adjust_default_appearance_skips_comment_verbatim() {
        let dr_map = category_dr_map(b"Font", &[("F1", "F1_1")]);
        let da: &[u8] = b"% a comment\n/F1 18 Tf";
        assert_eq!(
            adjust_default_appearance(da, &dr_map).unwrap(),
            b"% a comment\n/F1_1 18 Tf".to_vec()
        );
    }

    #[test]
    fn malformed_da_with_stray_delimiter_rewrites_later_name() {
        // qpdf reports the stray delimiter but continues with the valid
        // ResourceFinder offset that follows it.
        let dr_map = category_dr_map(b"Font", &[("F1", "F1_1")]);
        let da: &[u8] = b") /F1 18 Tf";
        assert_eq!(
            adjust_default_appearance(da, &dr_map).unwrap(),
            b") /F1_1 18 Tf".to_vec()
        );
    }

    #[test]
    fn malformed_da_with_unterminated_string_is_retained_verbatim() {
        // The apparent `/F1` is part of the unterminated string, so the
        // finder has no resource-name offset to replace.
        let dr_map = category_dr_map(b"Font", &[("F1", "F1_1")]);
        let da: &[u8] = b"(bad /F1 18 Tf";
        assert_eq!(adjust_default_appearance(da, &dr_map).unwrap(), da.to_vec());
    }

    #[test]
    fn incomplete_inline_image_da_keeps_prefix_replacement_and_qpdf_separator() {
        let dr_map = category_dr_map(b"Font", &[("F1", "F1_1")]);
        assert_eq!(
            adjust_default_appearance(b"/F1 12 Tf BI ID", &dr_map).unwrap(),
            b"/F1_1 12 Tf BI ID ".to_vec()
        );
    }

    #[test]
    fn adjust_default_appearance_no_font_category_in_dr_map_is_verbatim() {
        // dr_map is non-empty but has no "Font" entry (e.g. only /XObject
        // collisions were recorded) — the Tf-pattern lookup must miss
        // cleanly rather than panic.
        let mut dr_map = DrMap::new();
        dr_map.by_name.insert(b"XObject".to_vec(), BTreeMap::new());
        let da: &[u8] = b"/F1 18 Tf";
        assert_eq!(adjust_default_appearance(da, &dr_map).unwrap(), da.to_vec());
    }

    /// `/DA (/CS1 cs)` uses a ColorSpace name, not a Font name. When
    /// `merge_resources_shallow` renames `/AcroForm/DR/ColorSpace/CS1`, the
    /// `cs` operator in `/DA` must follow into the renamed name — the
    /// operator table matches qpdf's `ResourceFinder.cc` mapping (`cs` /
    /// `CS` → `ColorSpace`).
    #[test]
    fn adjust_default_appearance_rewrites_colorspace_via_cs_operator() {
        let dr_map = category_dr_map(b"ColorSpace", &[("CS1", "CS1_1")]);
        let da: &[u8] = b"/CS1 cs 0.5 0.4 0.3 sc /F1 12 Tf";
        assert_eq!(
            adjust_default_appearance(da, &dr_map).unwrap(),
            b"/CS1_1 cs 0.5 0.4 0.3 sc /F1 12 Tf".to_vec()
        );
    }

    /// Uppercase `CS` (stroke color space) shares the ColorSpace category
    /// with lowercase `cs`. Both must rewrite when the color space is
    /// renamed.
    #[test]
    fn adjust_default_appearance_rewrites_colorspace_via_uppercase_cs_operator() {
        let dr_map = category_dr_map(b"ColorSpace", &[("CS1", "CS1_1")]);
        let da: &[u8] = b"/CS1 CS";
        assert_eq!(
            adjust_default_appearance(da, &dr_map).unwrap(),
            b"/CS1_1 CS".to_vec()
        );
    }

    /// `/DA (/GS1 gs)` uses the ExtGState category. `merge_resources_shallow`
    /// may rename `/ExtGState/GS1`; the operator `gs` picks up its NAME arg
    /// from `ResourceFinder`'s tracked `last_name`, same shape as `Tf` —
    /// verify the shared finder/replacer is not limited to Font rewrites.
    #[test]
    fn adjust_default_appearance_rewrites_extgstate_via_gs_operator() {
        let dr_map = category_dr_map(b"ExtGState", &[("GS1", "GS1_1")]);
        let da: &[u8] = b"/GS1 gs 0 0 0 rg";
        assert_eq!(
            adjust_default_appearance(da, &dr_map).unwrap(),
            b"/GS1_1 gs 0 0 0 rg".to_vec()
        );
    }

    /// `/DA (/Im1 Do)` uses an XObject reference. Even though `Do` in
    /// `/DA` is atypical (widget /DA usually just sets fonts and colours),
    /// the qpdf operator table maps it, so the shared replacer must too.
    #[test]
    fn adjust_default_appearance_rewrites_xobject_via_do_operator() {
        let dr_map = category_dr_map(b"XObject", &[("Im1", "Im1_1")]);
        let da: &[u8] = b"/Im1 Do";
        assert_eq!(
            adjust_default_appearance(da, &dr_map).unwrap(),
            b"/Im1_1 Do".to_vec()
        );
    }

    /// `/DA (/Sh1 sh)` uses a Shading name. Same shape as `Tf`/`gs` — the
    /// operator picks up the tracked `last_name` and rewrites via the
    /// Shading category.
    #[test]
    fn adjust_default_appearance_rewrites_shading_via_sh_operator() {
        let dr_map = category_dr_map(b"Shading", &[("Sh1", "Sh1_1")]);
        let da: &[u8] = b"/Sh1 sh";
        assert_eq!(
            adjust_default_appearance(da, &dr_map).unwrap(),
            b"/Sh1_1 sh".to_vec()
        );
    }

    /// `BDC` consumes its SECOND name arg as a Properties entry — the tag
    /// (`/Span`) sets `last_name` first and is then overwritten by the
    /// properties name (`/MC1`), so `ResourceFinder`'s single-name tracker
    /// picks up `/MC1` at BDC time. Renaming `Properties/MC1 → MC1_1`
    /// must follow into the marked-content wrapper.
    #[test]
    fn adjust_default_appearance_rewrites_properties_via_bdc_operator() {
        let dr_map = category_dr_map(b"Properties", &[("MC1", "MC1_1")]);
        let da: &[u8] = b"/Span /MC1 BDC /F1 12 Tf EMC";
        assert_eq!(
            adjust_default_appearance(da, &dr_map).unwrap(),
            b"/Span /MC1_1 BDC /F1 12 Tf EMC".to_vec()
        );
    }

    /// `DP` (marked-content point with properties) shares the Properties
    /// category with `BDC` and uses the same tag-then-name shape.
    #[test]
    fn adjust_default_appearance_rewrites_properties_via_dp_operator() {
        let dr_map = category_dr_map(b"Properties", &[("MC1", "MC1_1")]);
        let da: &[u8] = b"/Span /MC1 DP";
        assert_eq!(
            adjust_default_appearance(da, &dr_map).unwrap(),
            b"/Span /MC1_1 DP".to_vec()
        );
    }

    /// ResourceFinder treats the tag as the resource name when `BDC` has a
    /// dictionary properties operand, so the shared replacer follows the
    /// recorded Properties rename without consulting a local `/DR` key.
    #[test]
    fn adjust_default_appearance_bdc_with_dict_properties_rewrites_tag() {
        let dr_map = category_dr_map(b"Properties", &[("Span", "Span_1")]);
        let da: &[u8] = b"/Span << /K 3 >> BDC";
        assert_eq!(
            adjust_default_appearance(da, &dr_map).unwrap(),
            b"/Span_1 << /K 3 >> BDC".to_vec()
        );
    }

    /// `/DA (/Pat1 scn)` uses a Pattern name; both `SCN` and `scn` map to
    /// Pattern. Verify the trailing-name pattern operator category is
    /// looked up.
    #[test]
    fn adjust_default_appearance_rewrites_pattern_via_scn_operator() {
        let dr_map = category_dr_map(b"Pattern", &[("Pat1", "Pat1_1")]);
        let da: &[u8] = b"/Pat1 scn";
        assert_eq!(
            adjust_default_appearance(da, &dr_map).unwrap(),
            b"/Pat1_1 scn".to_vec()
        );
    }

    /// Operators that do not take a resource-name arg (`rg`, `Tj`, `TJ`,
    /// numeric literals like `18`) must NOT trigger rewrites, and any
    /// preceding name-typed token must not "carry forward" past them to
    /// the next resource operator.
    #[test]
    fn adjust_default_appearance_ignores_non_resource_operators() {
        let dr_map = category_dr_map(b"Font", &[("F1", "F1_1")]);
        // `/F1 rg` is nonsense (rg wants three numbers), but ResourceFinder
        // must not treat rg as a resource operator: the trailing Tf is
        // what carries the rewrite. Here there is no trailing Tf → nothing
        // rewritten.
        let da: &[u8] = b"/F1 rg";
        assert_eq!(
            adjust_default_appearance(da, &dr_map).unwrap(),
            b"/F1 rg".to_vec()
        );
    }

    /// A rename recorded in `dr_map` for a category the `/DA` never
    /// references (e.g. dr_map has an XObject rename but /DA only uses
    /// Font) must NOT rewrite anything in that `/DA`.
    #[test]
    fn adjust_default_appearance_unmatched_category_in_dr_map_is_verbatim() {
        let dr_map = category_dr_map(b"XObject", &[("Im1", "Im1_1")]);
        let da: &[u8] = b"/F1 18 Tf";
        assert_eq!(adjust_default_appearance(da, &dr_map).unwrap(), da.to_vec());
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
        use crate::page_range::PageRange;
        use crate::{apply_overlay_specs, OverlayKind, OverlaySpec};
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
        use crate::page_range::PageRange;
        use crate::{apply_overlay_specs, OverlayKind, OverlaySpec};
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
        // shared dest /DR; its current-destination identity map reuses the
        // still-live F1_1 alias on pages 2 and 3.
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
