//! qpdf correspondence: QPDF_linearization.cc object classification and layout planning.
//! `LinearizationPlan` — pure data model for PDF linearization layout.
//!
//! A `LinearizationPlan` partitions all objects in a document into the four
//! body parts defined by ISO 32000-1 Annex F, and carries the raw inputs needed
//! to build the Page-offset hint table and the Shared-object hint table.
//!
//! The plan is intentionally a data-only structure: no I/O or serialization.
//! The hint-table builder and linearized writer consume this structure and fill
//! in the placeholders.
//!
//! # Part layout (Annex F summary)
//!
//! | Part | Contents |
//! |------|----------|
//! | 1    | Linearization parameter dictionary + first-page xref/trailer |
//! | 2    | First-page objects (page dict, resources, content streams) |
//! | 3    | Non-first-page shared objects (catalog, font programs, etc.) |
//! | 4    | Remaining (non-first-page) objects |
//!
//! # Object closure algorithm
//!
//! `from_pdf` now computes the transitive closure of objects reachable from the
//! first page (`/Pages /Kids[0]`) and partitions them:
//!
//! * **Part 2** — objects reachable from page 1 and *not* shared with other pages.
//! * **Part 3** — objects reachable from page 1 *and also* reachable from page 2..N
//!   (shared objects).
//! * **Part 4** — everything else (objects only reachable from pages 2..N, or from
//!   the catalog root but not from any page).
//!
//! The four parts are always disjoint (invariant preserved by construction).

use crate::linearization::renumber::RenumberMap;
use crate::object_handle::ObjectHandle;
use crate::parser::MAX_PARSE_DEPTH;
use crate::writer::object_streams::{
    compressible_objgens_qpdf_plan, eligibility_context, is_eligible_for_objstm_handle,
    ObjectStreamMode, PlannerConfig,
};
use crate::{ObjectRef, Pdf, Result};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{Read, Seek};

// ---------------------------------------------------------------------------
// Supporting types
// ---------------------------------------------------------------------------

/// Per-page entry for the **Page-offset hint table** (Annex F.3).
///
/// Byte-length and exact object indices are filled in as placeholders (zeros)
/// at construction time; a downstream writer pass must back-patch them once the
/// real file positions are known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageHintEntry {
    /// Indirect reference to the page's dictionary object.
    pub page_ref: ObjectRef,
    /// Index (0-based) of the first object belonging to this page in the
    /// object order that the linearized file will use.
    pub first_object_index: u32,
    /// Number of objects directly belonging to this page.
    pub object_count: u32,
    /// Byte length of all objects belonging to this page (placeholder: 0).
    pub byte_length: u64,
}

impl PageHintEntry {
    /// Construct a placeholder entry for `page_ref`.
    pub fn placeholder(page_ref: ObjectRef) -> Self {
        Self {
            page_ref,
            first_object_index: 0,
            object_count: 0,
            byte_length: 0,
        }
    }
}

/// Per-object entry for the **Shared-object hint table** (Annex F.4).
///
/// Annex F.4 keys shared objects by object index (within the linearized body
/// ordering), not by `ObjectRef`.  The `referencing_pages` field lists the
/// 0-based page indices (not `ObjectRef`s) that reference this shared object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedObjectHintEntry {
    /// The shared object.
    pub object_ref: ObjectRef,
    /// 0-based indices of the pages that reference this object.
    pub referencing_pages: Vec<u32>,
}

impl SharedObjectHintEntry {
    /// Construct a shared-object entry that has no page references yet.
    pub fn new(object_ref: ObjectRef) -> Self {
        Self {
            object_ref,
            referencing_pages: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Closure helpers
// ---------------------------------------------------------------------------

/// The set of indirect page-content stream refs, matching the plain
/// writer's `contents_seq` identity gate
/// (`writer.rs`'s `options.content_normalization && contents_seq.contains_key(old_ref)`).
///
/// Empty when `options.content_normalization` is off: qpdf's own
/// `m->normalize_content && m->normalized_streams.count(old_og)` gate
/// (`QPDFWriter.cc:1277`) is false for every stream in that case, so no
/// per-stream membership computation is needed.
fn linearization_content_normalize_refs<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    options: &crate::writer::WriterOptions,
) -> Result<BTreeSet<ObjectRef>> {
    if !options.content_normalization {
        return Ok(BTreeSet::new());
    }
    let mut refs = BTreeSet::new();
    for page_ref in crate::pages::page_refs(pdf)? {
        for content_ref in crate::writer::collect_content_stream_refs(pdf, page_ref)? {
            refs.insert(content_ref);
        }
    }
    Ok(refs)
}

/// Return whether a stream's `/Filter` or `/DecodeParms` value contains an
/// indirect edge that the linearized writer may remove after refiltering.
///
/// This inspects only the stream dictionary already supplied by qpdf's
/// optimization walk. It deliberately does not resolve the parameter object:
/// qpdf's `skip_stream_parameters` callback runs before those edges are
/// traversed (`QPDF_optimization.cc:306-333`).
fn stream_has_indirect_parameter_edge(handle: &ObjectHandle) -> Result<bool> {
    let Some(stream_dict) = handle.as_stream_dict() else {
        return Ok(false); // cov:ignore: Optimization invokes the callback only for resolved stream handles
    };
    for key in [b"/Filter".as_slice(), b"/DecodeParms".as_slice()] {
        let value = stream_dict.try_get_key(key)?;
        let mut refs = Vec::new();
        collect_direct_handle_refs(&value, 0, &mut refs)?;
        if !refs.is_empty() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Probe one stream using the same policy as qpdf's linearized writer and
/// report whether `/Filter` and `/DecodeParms` will be removed.
fn stream_parameters_removed_for_linearization(
    handle: &ObjectHandle,
    stream_ref: Option<ObjectRef>,
    options: &crate::writer::WriterOptions,
    content_normalize_refs: &BTreeSet<ObjectRef>,
) -> Result<bool> {
    // qpdf's linearization optimizer probes a token-filtered stream even when
    // its /Filter and /DecodeParms entries are direct. That probe is
    // observable because ValueSetter is stateful; use the linearization probe
    // for modified streams so the first body pass sees qpdf's already-consumed
    // filter. Unmodified streams retain the plain-writer cache-aware probe used
    // to decide whether parameter edges disappear.
    let normalize_content =
        stream_ref.is_some_and(|object_ref| content_normalize_refs.contains(&object_ref));
    if handle.is_data_modified() {
        crate::writer::plain::body::canonical_stream_filter_probe_for_linearization(
            handle,
            options,
            normalize_content,
        ) // cov:ignore: LLVM attributes the covered qpdf probe continuation to the call opening line
    } else {
        crate::writer::plain::body::canonical_stream_will_be_refiltered_with_policy(
            handle,
            options,
            true,
            normalize_content,
        ) // cov:ignore: LLVM attributes the covered qpdf probe continuation to the call opening line
    }
}

/// Collect indirect references from a live qpdf-shaped handle graph without
/// materializing the graph into an independent value snapshot.
///
/// An indirect child is recorded as one edge and expanded later by the
/// closure queue. Direct arrays, dictionaries, and stream dictionaries are
/// traversed in place. This mirrors qpdf's `QPDFObjectHandle` child access:
/// the handle identity is retained at each indirect boundary, while only the
/// current direct container is inspected.
fn collect_direct_handle_refs(
    handle: &ObjectHandle,
    depth: usize,
    out: &mut Vec<ObjectRef>,
) -> Result<()> {
    if depth > MAX_PARSE_DEPTH {
        return Err(crate::Error::Unsupported(format!(
            "linearization plan: inline object nesting exceeds maximum of {MAX_PARSE_DEPTH}"
        )));
    }
    let mut contextual = Vec::new();
    collect_direct_handle_refs_with_context(handle, depth, false, &mut contextual)?;
    out.extend(contextual.into_iter().map(|(object_ref, _)| object_ref));
    Ok(())
}

fn collect_direct_handle_refs_with_stream_parameters(
    handle: &ObjectHandle,
    depth: usize,
    out: &mut Vec<ObjectRef>,
    skipped_stream_parameter_streams: &BTreeSet<ObjectRef>,
) -> Result<()> {
    let mut contextual = Vec::new();
    collect_direct_handle_refs_with_stream_parameters_context(
        handle,
        depth,
        false,
        &mut contextual,
        skipped_stream_parameter_streams,
    )?; // cov:ignore: thin projection into the context-carrying walker; its stream-policy branches are covered by the closure tests
    out.extend(contextual.into_iter().map(|(object_ref, _)| object_ref));
    Ok(())
}

/// Context-carrying form of [`collect_direct_handle_refs`]. The boolean is
/// true only when the edge to the indirect child came from an array element;
/// dictionary-value null references are removed by the qpdf writer and must
/// not resurrect a body object in the linearization plan.
fn collect_direct_handle_refs_with_context(
    handle: &ObjectHandle,
    depth: usize,
    in_array: bool,
    out: &mut Vec<(ObjectRef, bool)>,
) -> Result<()> {
    if depth > MAX_PARSE_DEPTH {
        return Err(crate::Error::Unsupported(format!(
            "linearization plan: inline object nesting exceeds maximum of {MAX_PARSE_DEPTH}"
        )));
    }
    if let Some(object_ref) = handle.object_ref() {
        out.push((object_ref, in_array));
        return Ok(());
    }
    collect_direct_handle_children(
        handle,
        depth,
        in_array,
        &mut |child, child_depth, child_in_array| {
            collect_direct_handle_refs_with_context(child, child_depth, child_in_array, out)
        },
    )
}

fn collect_direct_handle_refs_with_stream_parameters_context(
    handle: &ObjectHandle,
    depth: usize,
    in_array: bool,
    out: &mut Vec<(ObjectRef, bool)>,
    skipped_stream_parameter_streams: &BTreeSet<ObjectRef>,
) -> Result<()> {
    if depth > MAX_PARSE_DEPTH {
        return Err(crate::Error::Unsupported(format!(
            "linearization plan: inline object nesting exceeds maximum of {MAX_PARSE_DEPTH}"
        )));
    }
    if let Some(object_ref) = handle.object_ref() {
        out.push((object_ref, in_array));
        return Ok(());
    }
    collect_direct_handle_children_with_stream_parameters(
        handle,
        depth,
        in_array,
        skipped_stream_parameter_streams,
        &mut |child, child_depth, child_in_array| {
            collect_direct_handle_refs_with_stream_parameters_context(
                child,
                child_depth,
                child_in_array,
                out,
                skipped_stream_parameter_streams,
            )
        },
    )
}

/// Walk the direct children of one handle. The closure receives each child,
/// the incremented inline depth, and whether its edge came from an array.
fn collect_direct_handle_children<F>(
    handle: &ObjectHandle,
    depth: usize,
    _parent_in_array: bool,
    visit: &mut F,
) -> Result<()>
where
    F: FnMut(&ObjectHandle, usize, bool) -> Result<()>,
{
    collect_direct_handle_children_with_stream_parameters(
        handle,
        depth,
        _parent_in_array,
        &BTreeSet::new(),
        visit,
    )
}

fn collect_direct_handle_children_with_stream_parameters<F>(
    handle: &ObjectHandle,
    depth: usize,
    _parent_in_array: bool,
    skipped_stream_parameter_streams: &BTreeSet<ObjectRef>,
    visit: &mut F,
) -> Result<()>
where
    F: FnMut(&ObjectHandle, usize, bool) -> Result<()>,
{
    handle.try_dereference()?;
    if let Some(stream_dict) = handle.as_stream_dict() {
        let Some(entries) = stream_dict.try_as_dictionary()? else {
            return Ok(());
        };
        let skip_stream_parameters =
            handle_has_stream_parameter_skip(handle, skipped_stream_parameter_streams)?;
        for (key, child) in entries {
            if key == b"/Length"
                || (skip_stream_parameters
                    && matches!(key.as_slice(), b"/Filter" | b"/DecodeParms"))
            {
                continue;
            }
            visit(&child, depth + 1, false)?;
        }
        return Ok(());
    }
    if let Some(children) = handle.try_as_array()? {
        for child in children {
            visit(&child, depth + 1, true)?;
        }
        return Ok(());
    }
    if let Some(entries) = handle.try_as_dictionary()? {
        for (_key, child) in entries {
            visit(&child, depth + 1, false)?;
        }
    }
    Ok(())
}

fn handle_has_stream_parameter_skip(
    handle: &ObjectHandle,
    skipped_stream_parameter_streams: &BTreeSet<ObjectRef>,
) -> Result<bool> {
    Ok(handle
        .object_ref()
        .is_some_and(|object_ref| skipped_stream_parameter_streams.contains(&object_ref)))
}

fn collect_handle_children_with_stream_parameters(
    handle: &ObjectHandle,
    depth: usize,
    out: &mut Vec<(ObjectRef, bool)>,
    skipped_stream_parameter_streams: &BTreeSet<ObjectRef>,
) -> Result<()> {
    collect_direct_handle_children_with_stream_parameters(
        handle,
        depth,
        false,
        skipped_stream_parameter_streams,
        &mut |child, child_depth, in_array| {
            collect_direct_handle_refs_with_stream_parameters_context(
                child,
                child_depth,
                in_array,
                out,
                skipped_stream_parameter_streams,
            )
        },
    )
}

/// Returns whether a live handle is a page-tree interior or leaf node.
fn is_page_tree_handle(handle: &ObjectHandle) -> Result<bool> {
    Ok(handle.try_is_dictionary_of_type(b"Pages", b"")?
        || handle.try_is_dictionary_of_type(b"Page", b"")?)
}

fn compute_closure_with_stream_parameters<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    root: ObjectRef,
    live: &BTreeSet<ObjectRef>,
    resurrectable: &BTreeSet<ObjectRef>,
    skipped_stream_parameter_streams: &BTreeSet<ObjectRef>,
) -> crate::Result<Vec<ObjectRef>> {
    let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut order: Vec<ObjectRef> = Vec::new();
    // Each entry carries (ref, via_array): whether the ref was enqueued via an
    // array element (true) or a dictionary value (false).  Used at dequeue time
    // to admit resurrectable null refs only when the reaching edge is a
    // surviving array slot — matching qpdf's object-user classification, which
    // excludes dict-value edges that the writer drops entirely.
    let mut queue: VecDeque<(ObjectRef, bool)> = VecDeque::from([(root, false)]);
    // Resurrectable refs that were not admissible at dequeue time (seen_as_array
    // did not yet contain them — the live object holding the array edge had not
    // been expanded yet).  After the full BFS completes, seen_as_array is
    // exhaustive; we then admit each deferred ref that appears there.  This
    // handles the "revorder" case: resurrectable ref number < live descendant
    // number, so sort-at-enqueue puts the resurrectable ref in the queue before
    // the live object that would reveal the array edge.
    let mut deferred_resurrect: BTreeSet<ObjectRef> = BTreeSet::new();
    // Tracks every ref that has been enqueued (or pushed to the Resources DFS
    // stack) via an array-element edge within this closure walk.  A resurrectable
    // ref that appears in *both* a dict-value slot (dropped by the writer) and an
    // array slot (survives as null) within the same page's closure must still be
    // admitted: the dict-value tuple (r, false) may be dequeued before the
    // array-edge tuple (r, true), but `seen_as_array` lets the dequeue check
    // consult all edges discovered so far, not just the one in the current tuple.
    let mut seen_as_array: BTreeSet<ObjectRef> = BTreeSet::new();

    // Object 0 (free-list head / null singleton, ISO 32000-1 §7.3.10) and all
    // null-resolving references have no ordinary body here. The holding dict key
    // is dropped, while an array keeps the indirect null identity only through
    // the `resurrectable` path below. Keeping other nulls in `visited` but out of
    // `order` avoids a stray numbered body and an inflated first-page count.
    //
    // Exception: null-resolving refs reached via a surviving array edge in this
    // page's object graph are resurrectable (see `resurrectable_null_refs`). qpdf
    // classifies them as first-page users when reached from a first-page object,
    // giving them HIGH object numbers inside Part 2. A null ref reached only via
    // a dict value is dropped (not resurrectable), so it stays excluded.
    let admits_body_object = |r: ObjectRef| -> bool { r.number != 0 && live.contains(&r) };

    while let Some((current, via_array)) = queue.pop_front() {
        if !visited.insert(current) {
            continue;
        }
        if resurrectable.contains(&current) {
            // Null-resolving ref: the writer preserves array-element slots as
            // null but drops dict-value slots (the key is omitted entirely). Only
            // admit to the closure when an array edge exists — a ref reached
            // solely via dict-value edges has no surviving body object in the
            // first-page section, matching qpdf's object-user map which does not
            // count dropped dict-value edges as page uses.
            // `via_array` covers the common case; `seen_as_array` covers the case
            // where the same ref appears in both a dict-value slot and an array
            // slot in the same closure (the dict-value tuple may be dequeued first,
            // but the array edge was already recorded at enqueue time).
            // `deferred_resurrect` handles the "revorder" case: resurrectable ref
            // number < live descendant number, so the resurrectable ref is dequeued
            // before seen_as_array is populated.  Deferred refs are re-checked
            // after the full BFS completes.
            if via_array || seen_as_array.contains(&current) {
                order.push(current);
            } else {
                deferred_resurrect.insert(current);
            }
            continue;
        }
        if !admits_body_object(current) {
            continue;
        }
        order.push(current);

        let current_handle = pdf.get_object_handle(current);
        pdf.resolve(&current_handle)?;

        // Determine whether this is a Pages node (intermediate page-tree node)
        // or a Page leaf node.
        let is_pages_node = current_handle.try_is_dictionary_of_type(b"Pages", b"")?;
        let is_page_leaf = current_handle.try_is_dictionary_of_type(b"Page", b"")?;

        if is_pages_node || is_page_leaf {
            if let Some(dict) = current_handle.try_as_dictionary()? {
                // For a page leaf, expand the `/Resources` subtree depth-first
                // and append it to `order` *before* the generic key loop runs.
                // flpdf's `Dictionary` is a `BTreeMap`, so a plain key walk
                // would visit `/Contents` (alphabetically first) before
                // `/Resources`; qpdf instead numbers the Resources dictionary
                // and the fonts/XObjects it references ahead of the content
                // stream. Reproducing that order here is what makes the
                // first-page object numbering match qpdf (e.g. one-page:
                // Page, Resources, Font, Content). The depth-first walk is
                // required because the content stream sits at depth 1 while a
                // font hangs at depth 2 under `/Resources`; a breadth-first
                // pass would otherwise emit the content stream first.
                if is_page_leaf {
                    {
                        let resources = current_handle.try_get_key(b"/Resources")?;
                        let mut seeds: Vec<(ObjectRef, bool)> = Vec::new();
                        collect_direct_handle_refs_with_context(&resources, 0, false, &mut seeds)?;
                        for &(r, va) in &seeds {
                            if va {
                                seen_as_array.insert(r);
                            }
                        }
                        // DFS via an explicit stack (no recursion) so deeply
                        // nested resource graphs cannot overflow the stack.
                        // The visited set bounds cycles; `is_page_tree_handle`
                        // stops the walk if a resource value cross-links back
                        // into the page tree, so we never pull in sibling pages.
                        let mut stack: Vec<(ObjectRef, bool)> = seeds.into_iter().rev().collect();
                        while let Some((r, via_array)) = stack.pop() {
                            if !visited.insert(r) {
                                continue;
                            }
                            if resurrectable.contains(&r) {
                                // Same edge-type rule as the main BFS: admit the
                                // null body object only when an array edge exists.
                                // Defer if seen_as_array not yet populated (revorder
                                // case); the post-BFS pass will re-check.
                                if via_array || seen_as_array.contains(&r) {
                                    order.push(r);
                                } else {
                                    // cov:ignore-start: fires when a resurrectable ref is reachable via dict-value in the Resources subtree before the array edge in the same subtree is discovered; requires two resource objects cross-referencing the same xref-absent object via different edge types, which is extremely contrived
                                    deferred_resurrect.insert(r);
                                } // cov:ignore-end
                                continue;
                            }
                            if !admits_body_object(r) {
                                // Null-resolving resource ref (object 0 / missing
                                // xref): no body object, same as the main loop.
                                continue;
                            }
                            let child_handle = pdf.get_object_handle(r);
                            pdf.resolve(&child_handle)?;
                            // Stop at a page-tree boundary BEFORE adding `r` to
                            // the closure: a resource that malformedly cross-links
                            // to a sibling `/Page` or the `/Pages` node must be
                            // kept in `visited` (so it is never revisited) but
                            // excluded from the first-page closure entirely — per
                            // the page-closure boundary rule, we neither descend
                            // into it nor pull the boundary node itself into
                            // Part 2/3.
                            if is_page_tree_handle(&child_handle)? {
                                continue;
                            }
                            order.push(r);
                            let mut child_refs: Vec<(ObjectRef, bool)> = Vec::new();
                            collect_handle_children_with_stream_parameters(
                                &child_handle,
                                0,
                                &mut child_refs,
                                skipped_stream_parameter_streams,
                            )?;
                            // Push in reverse so the first reference is popped
                            // first, preserving left-to-right discovery order.
                            for cr in child_refs.into_iter().rev() {
                                if cr.1 {
                                    seen_as_array.insert(cr.0);
                                }
                                if !visited.contains(&cr.0) {
                                    stack.push(cr);
                                }
                            }
                        }
                    }
                } // cov:ignore: llvm-cov attributes 0 to this `if is_page_leaf` closing brace; the block body (the /Resources DFS) runs and is covered above.
                let mut refs_raw: Vec<(ObjectRef, bool)> = Vec::new();
                for (k, v) in dict.iter() {
                    if k == b"/Kids" {
                        // Pages → sibling pages — never follow.
                        continue;
                    }
                    if k == b"/Thumb" {
                        // qpdf gives thumbnail objects the separate ou_thumb
                        // user (not a page user), so page closures never
                        // include /Thumb targets. Skipping here ensures
                        // thumbnail objects land in part4_rest (part 9)
                        // rather than the per-page private/shared sections.
                        continue;
                    }
                    if k == b"/Parent" {
                        // Walk the /Parent chain up to the root Pages node so
                        // inherited /Resources, /MediaBox, /Rotate, etc. from
                        // any ancestor (not just the immediate parent) end up
                        // in this page's closure. Without iterating to the
                        // root, a `/Page → /Pages → /Pages` tree with the
                        // inherited resource attached to the grandparent
                        // would leave that resource unreachable from any
                        // page's closure and land it in `part4_rest`,
                        // misclassifying it relative to qpdf's part7/8/9
                        // partition.
                        //
                        // The ancestor /Pages dicts themselves are NOT added
                        // to this page's closure — adding them would inflate
                        // the page's object_count beyond what qpdf computes
                        // from the linearized layout. We follow each
                        // ancestor's non-/Kids, non-/Parent entries and let
                        // the queue traverse into ref targets normally.
                        let mut to_visit: Vec<ObjectRef> = Vec::new();
                        let mut seen_parents: BTreeSet<ObjectRef> = BTreeSet::new();
                        collect_direct_handle_refs_with_stream_parameters(
                            v,
                            0,
                            &mut to_visit,
                            skipped_stream_parameter_streams,
                        )?; // cov:ignore: LLVM maps this covered parent-seed call terminator to a zero-count continuation region

                        while let Some(parent_ref) = to_visit.pop() {
                            if !seen_parents.insert(parent_ref) {
                                continue;
                            }
                            // Resolve the parent. Genuine resolve failures
                            // (I/O or parse errors) propagate via `?` instead
                            // of silently degrading the closure — mirroring
                            // the main BFS loop's `pdf.resolve(&parent_handle)?`.
                            let parent_handle = pdf.get_object_handle(parent_ref);
                            pdf.resolve(&parent_handle)?;
                            // `parent_handle` is the canonical object for
                            // `parent_ref`; its `object_ref()` is identity,
                            // not a stored reference value. Inspect the live
                            // dictionary below so the ancestor's inherited
                            // entries are actually added to the closure.
                            let Some(parent_dict) = parent_handle.try_as_dictionary()? else {
                                // Any other non-dictionary parent (a free or
                                // missing object resolving to Null, etc.) is
                                // tolerated: the walk just climbs past it.
                                continue;
                            };
                            for (pk, pv) in parent_dict.iter() {
                                if pk == b"/Kids" {
                                    continue;
                                }
                                if pk == b"/Parent" {
                                    // Climb to the next ancestor instead of
                                    // stopping at one level.
                                    collect_direct_handle_refs(pv, 0, &mut to_visit)?;
                                    continue;
                                }
                                let mut refs: Vec<(ObjectRef, bool)> = Vec::new();
                                collect_direct_handle_refs_with_stream_parameters_context(
                                    pv,
                                    0,
                                    false,
                                    &mut refs,
                                    skipped_stream_parameter_streams,
                                )?; // cov:ignore: LLVM maps this covered ancestor-entry call terminator to a zero-count continuation region
                                for (r, va) in refs {
                                    if va {
                                        seen_as_array.insert(r);
                                    }
                                    if !visited.contains(&r) {
                                        queue.push_back((r, va));
                                    }
                                }
                            }
                        }
                        continue;
                    }
                    collect_direct_handle_refs_with_stream_parameters_context(
                        v,
                        0,
                        false,
                        &mut refs_raw,
                        skipped_stream_parameter_streams,
                    )?; // cov:ignore: LLVM maps this covered page-entry call terminator to a zero-count continuation region
                }
                for &(r, va) in &refs_raw {
                    if va {
                        seen_as_array.insert(r);
                    }
                }
                // Sort by original object number: qpdf assigns first-page slots in
                // ascending original-number order regardless of dict key alphabetical
                // order (empirically verified; see discriminator-fixture analysis).
                refs_raw.sort_by_key(|(r, _)| r.number);
                for (r, va) in refs_raw {
                    if !visited.contains(&r) {
                        queue.push_back((r, va));
                    }
                }
            }
        } else {
            let mut refs: Vec<(ObjectRef, bool)> = Vec::new();
            collect_handle_children_with_stream_parameters(
                &current_handle,
                0,
                &mut refs,
                skipped_stream_parameter_streams,
            )?; // cov:ignore: LLVM maps this covered ordinary-object call terminator to a zero-count continuation region
            for &(r, va) in &refs {
                if va {
                    seen_as_array.insert(r);
                }
            }
            // Same number-ordering rule as the page-dict loop above: qpdf enqueues
            // a non-page object's children in ascending original-object-number order,
            // not in dict-key (alphabetical) order.
            refs.sort_by_key(|(r, _)| r.number);
            for (r, va) in refs {
                if !visited.contains(&r) {
                    queue.push_back((r, va));
                }
            }
        }
    }

    // Sort the non-page tail (order[1..]) by original object number.  qpdf emits
    // first-page objects in ascending original-number order, with the Page leaf
    // always in the first slot.  Two discriminator fixtures confirm this:
    // (a) Resources(5)/Font(6) at higher numbers than Content(4) → numeric order
    //     wins over Resources-DFS-first order;
    // (b) Page(orig 10) with Content(orig 3) → Page stays first despite having a
    //     higher original number, so a fully-global sort would misplace it.
    // Sorting only order[1..] satisfies both invariants simultaneously.
    if order.len() > 1 {
        order[1..].sort_by_key(|r| r.number);
    }
    // Deferred resurrectable refs: now that the full BFS is complete and
    // seen_as_array is exhaustive, admit those that turn out to be reachable
    // via an array edge (i.e. they appear in seen_as_array).  Insert each at
    // the numerically correct position in the already-sorted non-page tail so
    // the Page leaf remains first and the overall tail ordering stays ascending.
    // BTreeSet iterates in ascending order, so each insertion preserves the
    // invariant for subsequent entries.
    for r in deferred_resurrect {
        if seen_as_array.contains(&r) {
            let tail_pos = order[1..].partition_point(|existing| existing.number < r.number);
            order.insert(1 + tail_pos, r);
        } // cov:ignore: false branch = deferred ref that has no array edge in the entire closure; qpdf correctly drops it (same as the dict-value-only drop path). Requires a resurrectable ref that appears *only* in a dict-value slot with no array counterpart anywhere in the closure — correctly exercised by the existing behaviour but the closing brace hits only the zero-count path.
    }

    Ok(order)
}

// ---------------------------------------------------------------------------
// LinearizationPlan
// ---------------------------------------------------------------------------

/// Partition of a PDF document's objects into the four linearization parts
/// defined by ISO 32000-1 Annex F, together with the raw inputs for the
/// page-offset and shared-object hint tables.
///
/// Constructed from a [`Pdf`] handle via [`LinearizationPlan::from_pdf`].
/// This struct owns all data it needs and holds no reference into the source
/// document.
///
/// # Object disjointness
///
/// The four part lists are disjoint by construction.  `from_pdf` computes the
/// first-page closure, partitions it into Part 2 (exclusive) and Part 3
/// (shared with other pages), and removes the moved objects from Part 4 so
/// the invariant is always maintained.  The free-list head at object 0 is
/// excluded from Part 4 entirely (ISO 32000-1 §7.5.4).
#[derive(Debug, Clone)]
pub struct LinearizationPlan {
    // ------------------------------------------------------------------
    // Part membership
    // ------------------------------------------------------------------
    /// Part 1: linearization parameter dictionary and its xref stream.
    /// Reserved for the linearization parameter dictionary and its xref
    /// stream; plans built from a PDF leave this list empty.
    pub part1_objects: Vec<ObjectRef>,
    /// Part 2: first-page objects (page dict, resources, content streams).
    /// Computed by `from_pdf` using the first-page closure algorithm.
    pub part2_objects: Vec<ObjectRef>,
    /// Part 3: non-first-page shared objects (objects referenced by both
    /// page 1 and at least one other page).
    /// Computed by `from_pdf`.
    pub part3_objects: Vec<ObjectRef>,
    /// qpdf part7: objects private to exactly one other page (pages 2..N).
    ///
    /// Ordered by page index, then by BFS closure order within each page.
    pub part4_other_pages_private: Vec<ObjectRef>,
    /// qpdf part8: objects shared by two or more other pages (pages 2..N),
    /// but NOT reachable from page 1.
    pub part4_other_pages_shared: Vec<ObjectRef>,
    /// qpdf part9: all Part-4 objects that are not in part7 or part8.
    /// Includes the Pages tree, Info dict, lc_other objects, and any objects
    /// not reachable from any page closure (trailer-only refs, etc.).
    pub part4_rest: Vec<ObjectRef>,

    // ------------------------------------------------------------------
    // Document summary (copied from the source at construction time)
    // ------------------------------------------------------------------
    /// Total number of objects as reported by the xref table.
    pub total_object_count: u32,
    /// `/Root` reference from the trailer, if present.
    pub root_ref: Option<ObjectRef>,
    /// `/Pages` tree root reference (catalog's `/Pages` entry).
    ///
    /// Promoted into the renumber map's reserved prefix so the resulting
    /// object number matches qpdf's `part9` head (qpdf assigns the pages
    /// tree to object 1). May be `None` for malformed inputs missing this
    /// entry; in that case no promotion happens.
    pub pages_tree_ref: Option<ObjectRef>,
    /// `/Info` reference from the trailer, if present.
    ///
    /// Promoted into the renumber map's reserved prefix to mirror qpdf's
    /// `lc_other` ordering (Info follows pages tree in the second-half
    /// renumber pass).
    pub info_ref: Option<ObjectRef>,

    // ------------------------------------------------------------------
    // Hint table inputs
    // ------------------------------------------------------------------
    /// Page-offset hint table inputs (one entry per page).
    ///
    /// Entry 0 has `object_count` set to the number of Part-2 objects and
    /// `first_object_index` set to 0.  `byte_length` remains a placeholder (0)
    /// for back-patching by the writer.
    pub page_hints: Vec<PageHintEntry>,
    /// Shared-object hint table inputs.
    ///
    /// One entry per Part-3 object; `referencing_pages` lists the 0-based
    /// page indices (across all pages) that reach this object.
    pub shared_hints: Vec<SharedObjectHintEntry>,

    /// Per-page private object lists for byte-length computation.
    ///
    /// `per_page_private_objects[i]` is the list of objects that belong
    /// exclusively to page `i` (not shared with any other page):
    ///
    /// * For page 0: equal to `part2_objects`.
    /// * For pages 1..N: the objects in that page's closure that are
    ///   **not** in Part 2 or Part 3 (i.e. they are private to this page
    ///   within Part 4).
    ///
    /// The writer uses these lists to compute `page_hints[i].byte_length`
    /// and to populate the Page Offset Hint Table's `page_length_minus_least`
    /// and `least_page_length` fields.
    pub per_page_private_objects: Vec<Vec<ObjectRef>>,

    /// Full object → referencing-page inverse map: `all_referenced_pages[r]` is
    /// the set of 0-based page users assigned to `r` by qpdf's ordered
    /// `updateObjectMaps` traversal.
    ///
    /// Used to compute a shared ObjStm container's referencing pages from its
    /// FULL membership — the global even split can place a page's *private*
    /// object inside a container in another section (the first-page part6
    /// container or a part8 shared container), and the page then references that
    /// container as a shared object. Keyed by original ref.
    pub all_referenced_pages: BTreeMap<ObjectRef, BTreeSet<u32>>,

    /// Outline objects routed to the first-page section (part6) when the catalog
    /// specifies `/PageMode /UseOutlines`, in emitted order (root first, then items
    /// in traversal order). Empty when the predicate is false.
    ///
    /// Ordered to match qpdf's `lc_outlines` traversal order so that `shared_hints`
    /// entries are in the same sequence as physically emitted objects.
    /// Used by `page0_object_count_with_objstm` to include the outline ObjStm
    /// container in the page-0 object count (qpdf counts all part6 objects in
    /// `entries.at(0).nobjects`, including outlines placed there when
    /// `outlines_in_first_page` is set).
    pub(crate) outline_first_page_members: Vec<ObjectRef>,

    /// Outline objects for the classic (non-ObjStm) linearize path when
    /// `/PageMode` is NOT `/UseOutlines`.  Extracted from `part4_rest` and
    /// assigned consecutive second-half object numbers (between `pages_tree`
    /// and `info/param_dict` in the renumber map), then emitted after /E.
    /// Matches qpdf's `lc_outlines` (part9) placement.  Empty when
    /// `UseOutlines` is active or when there are no outlines.
    pub(crate) part9_outline_objects: Vec<ObjectRef>,

    /// Outline objects for the classic (non-ObjStm) linearize path when
    /// `/PageMode /UseOutlines` is set.  Extracted from `part4_rest` and
    /// given first-half numbers (after Part 3 in the renumber map), then
    /// emitted **before** /E (between Part 3 and the /E boundary).  Matches
    /// qpdf's `lc_outlines` (part6) placement.  Empty when `UseOutlines` is
    /// not set or when there are no outlines.
    pub(crate) part6_outline_objects: Vec<ObjectRef>,

    /// Open-document objects (qpdf part4 = `lc_open_document`) emitted as plain
    /// indirect objects in the pre-/O region, immediately after the Catalog, in
    /// ascending source object number order.
    ///
    /// In disable/preserve mode this holds the FULL open-document set — every
    /// object reachable from the catalog open-document keys (`/OpenAction`,
    /// `/AcroForm`, `/PageMode`, `/Threads`, `/ViewerPreferences`) and trailer
    /// `/Encrypt`. In generate mode it holds only the ObjStm-ineligible subset
    /// (e.g. stream objects such as `/AP /N` appearance streams that cannot be
    /// ObjStm members); the eligible ones are packed into the open-document ObjStm
    /// container instead.
    ///
    /// Empty only when the document has no open-document objects (or, in generate
    /// mode, when all of them are ObjStm-eligible).
    pub part4_open_document_plain: Vec<ObjectRef>,

    /// Retained qpdf-style bidirectional object-user map used to route generated
    /// and preserved ObjStm containers without re-reading the PDF.
    pub(crate) optimization: Option<crate::optimization::Optimization>,

    /// Terminal indirect page-content stream refs, matching the plain
    /// writer's `contents_seq` identity gate. Empty when
    /// `options.content_normalization` was off. The writer must reuse this
    /// exact set (not recompute it) so a stream's plan-time refilter probe
    /// and its real emission agree on whether content normalization
    /// applies, mirroring qpdf's single `m->normalized_streams` set shared
    /// between `willFilterStream` and emission (`QPDFWriter.cc:1277`).
    pub(crate) content_normalize_refs: BTreeSet<ObjectRef>,

    /// Live source generations that qpdf drops while planning generated or
    /// preserved object streams. References to these objects are rewritten as
    /// qpdf-null values even though the source xref entry itself is live.
    pub(crate) removed_refs: BTreeSet<ObjectRef>,

    /// Object-stream mode whose traversal and stale-generation rules produced
    /// this plan. The writer reconciles a legacy boolean plan with its final
    /// [`WriterOptions`](crate::writer::WriterOptions) mode before using the plan's
    /// partitions and renumbering.
    pub(crate) object_stream_mode: crate::writer::ObjectStreamMode,
}

impl LinearizationPlan {
    /// Construct a `LinearizationPlan` from a parsed PDF document.
    ///
    /// This method:
    ///
    /// 1. Collects all known object refs into Part 4.
    /// 2. Computes the transitive closure of objects reachable from page 1
    ///    (`/Pages /Kids[0]`).
    /// 3. Computes closures for pages 2..N to identify shared objects.
    /// 4. Partitions the page-1 closure into Part 2 (exclusive) and Part 3
    ///    (shared), removing them from Part 4.
    /// 5. Fills `page_hints[0]` with the correct `object_count`; all
    ///    `byte_length` fields remain 0 (back-patched by the writer).
    /// 6. Fills `shared_hints` with one entry per Part-3 object, listing
    ///    every page index that references it.
    ///
    /// # Errors
    ///
    /// Propagates any error from [`crate::pages::page_refs`] when collecting the
    /// document's page references (e.g. a malformed or unresolvable `/Pages`
    /// tree). Also propagates any error from resolving objects while computing
    /// each page's reachability closure (via [`Pdf::resolve`] /
    /// [`Pdf::resolve`]) — typically an [`crate::Error::Io`] or
    /// [`crate::Error::Parse`] on a truncated or malformed object. Before any
    /// of that, this also pushes inherited page attributes down the `/Pages`
    /// tree, which propagates the same object-resolution errors and returns
    /// [`crate::Error::Unsupported`] if the tree exceeds the page-tree depth
    /// bound.
    pub fn from_pdf<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        use_generate_objstm: bool,
    ) -> crate::Result<Self> {
        let mode = if use_generate_objstm {
            crate::writer::ObjectStreamMode::Generate
        } else {
            crate::writer::ObjectStreamMode::Disable
        };
        Self::from_pdf_with_object_stream_mode(pdf, mode)
    }

    /// Construct a mode-aware linearization plan.
    ///
    /// Unlike the historical boolean API, this distinguishes standard
    /// Preserve from source-ObjStm Preserve. qpdf runs its
    /// `getCompressibleObjGens` stale-generation removal only for Generate and
    /// when Preserve actually consumes source object streams.
    pub fn from_pdf_with_object_stream_mode<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        object_stream_mode: crate::writer::ObjectStreamMode,
    ) -> crate::Result<Self> {
        let options = crate::writer::WriterOptions {
            object_streams: object_stream_mode,
            ..crate::writer::WriterOptions::default()
        };
        Self::from_pdf_with_writer_options(pdf, &options)
    }

    pub(crate) fn from_pdf_with_writer_options<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        options: &crate::writer::WriterOptions,
    ) -> crate::Result<Self> {
        let object_stream_mode = options.object_streams;
        let use_generate_objstm = matches!(
            object_stream_mode,
            crate::writer::ObjectStreamMode::Generate
        );
        let capture_pre_optimization_objects =
            !matches!(object_stream_mode, crate::writer::ObjectStreamMode::Disable);
        let pre_optimization_object_refs =
            capture_pre_optimization_objects.then(|| pdf.live_object_refs().into_iter().collect());
        // QPDFWriter::doWriteSetup fixes Generate's eligible object set before
        // writeLinearized calls QPDF::optimize. The latter may mint indirect
        // inherited-attribute objects, which must remain plain in the output.
        let generate_objstm_eligible = use_generate_objstm
            .then(|| crate::writer::object_streams::get_compressible_objgens(pdf))
            .transpose()?;
        // qpdf optimization performs direct-outline normalization, page-tree
        // preparation, inherited-attribute push, and object-user traversal in
        // this order. It must run before object-ref capture because those
        // preparations may mint indirect objects.
        //
        // Warm the canonical qpdf object cache before the Optimization
        // compatibility consumer runs. qpdf has one object cache: if the
        // malformed stream framing below is first parsed through the
        // ObjectHandle route, Optimization observes that same cached value
        // instead of reparsing the source and duplicating its recovery
        // diagnostics. The producer must not use a resolve/materialize bridge
        // in the other direction.
        if pdf.root_ref().is_some() {
            crate::writer::rewrite_renumber::CanonicalCatalogFirstRenumber::build_qpdf_with_stream_policy(
                pdf,
                true,
                false,
                &BTreeSet::new(),
                None)?;
        }
        let content_normalize_refs = linearization_content_normalize_refs(pdf, options)?;
        let mut skipped_stream_parameter_streams = BTreeSet::new();
        let mut optimization = crate::optimization::Optimization::optimize(
            pdf,
            &BTreeMap::new(),
            true,
            |stream_ref, stream| {
                // qpdf invokes this callback only while traversing objects
                // reachable from a page, trailer key, or root key. Keep the
                // stream-parameter probe inside that callback so an orphaned
                // stream cannot make linearization read its source data.
                if stream_ref.is_some_and(|object_ref| {
                    skipped_stream_parameter_streams.contains(&object_ref)
                }) {
                    // A stream can be visited by more than one object user.
                    // Preserve the old prepass's one-probe state boundary:
                    // once the first reachable visit established that qpdf
                    // removes the parameter edges, later visits must not
                    // consume a stateful token filter again.
                    return Ok(2);
                }
                let has_indirect_parameter = stream_has_indirect_parameter_edge(stream)?;
                // The parameter-edge helper is itself the qpdf-shaped
                // `willFilterStream` probe. Reuse its result when the stream
                // has an indirect parameter edge; a second probe here would
                // consume a stateful provider/filter a second time within the
                // same optimization callback.
                let refiltered = if has_indirect_parameter {
                    stream_parameters_removed_for_linearization(
                        stream,
                        stream_ref,
                        options,
                        &content_normalize_refs,
                    )? // cov:ignore: LLVM attributes this covered reachable-stream branch continuation to the call opening line
                } else {
                    crate::writer::plain::body::canonical_stream_filter_probe_for_linearization(
                        stream,
                        options,
                        stream_ref
                            .is_some_and(|object_ref| content_normalize_refs.contains(&object_ref)),
                    )? // cov:ignore: LLVM attributes this covered reachable-stream branch continuation to the call opening line
                };
                if refiltered {
                    if has_indirect_parameter {
                        if let Some(object_ref) = stream_ref {
                            skipped_stream_parameter_streams.insert(object_ref);
                        }
                    }
                    // The probe above already consumed qpdf's stateful filter
                    // check; the optimization callback must not run it again.
                    Ok(2)
                } else {
                    Ok(1)
                }
            },
        )?;
        if let Some(eligible) = generate_objstm_eligible {
            optimization.set_generate_objstm_eligible(eligible);
        }
        if let Some(refs) = pre_optimization_object_refs {
            optimization.set_pre_optimization_object_refs(refs);
        }
        if pdf.root_ref().is_some()
            && optimization
                .objects_for(&crate::optimization::ObjectUser::Page(0))
                .is_empty()
        {
            return Err(crate::Error::Unsupported(
                "no pages found while calculating linearization data".to_string(),
            ));
        }

        // ----------------------------------------------------------------
        // Step 1: collect all known object refs (Part 4 initial state).
        // The free-list head at object 0 is excluded per ISO 32000-1 §7.5.4.
        // ----------------------------------------------------------------
        // Drop the source's structural containers (`/Type /ObjStm`, `/Type
        // /XRef`) from the live object set. qpdf rebuilds the cross-reference
        // and repacks ObjStm members into fresh containers, so the source
        // containers are never live body objects (their members survive as
        // individual objects via the compressed xref entries). Carrying them
        // through would shift every offset and make qpdf's linearization
        // length-calc reject them ("found unknown object"). This keeps the
        // linearization universe aligned with the writer's structural-container
        // emission rule.
        // qpdf garbage-collects objects unreachable from the trailer roots (it
        // only enqueues reachable objects). The plain full-rewrite path does this
        // via the canonical handle renumber walk's trailer-seeded BFS; the linearize universe
        // must too, or re-linearizing an already-linearized source leaks its old
        // /Linearized parameter dict + hint stream — both unreachable structural
        // artifacts — into the second half.
        //
        // `skip_length = true`: the linearized writer always emits a direct
        // `/Length` — re-encoded and lone-`/FlateDecode`-verbatim streams alike,
        // and `renumber_object` substitutes a direct length for a dropped holder's
        // dangling reference — so a stream's indirect `/Length` edge is dead in the
        // output regardless of object-stream mode. Not following it drops a holder
        // reachable only through it, matching qpdf's reachability GC.
        let reachable =
            crate::writer::rewrite_renumber::reachable_object_set_with_stream_parameters(
                pdf,
                true,
                &skipped_stream_parameter_streams,
            )?;
        let object_refs = pdf.object_refs();
        let mut all_refs: Vec<ObjectRef> = Vec::with_capacity(object_refs.len());
        for r in object_refs {
            if r.number == 0 {
                continue;
            }
            if !reachable.contains(&r) {
                // Unreachable from the trailer roots — qpdf drops it before
                // any linearization part is formed. Check this before
                // resolving so a discarded object cannot make planning read
                // its source bytes.
                continue;
            }
            let object_handle = pdf.get_object_handle(r);
            pdf.resolve(&object_handle)?;
            // Both `/Type /XRef` and `/Type /ObjStm` objects are required to
            // carry stream data (ISO 32000-1 §7.5.7/§7.5.8), so the genuine
            // article is always `ObjectValue::Stream`, never a plain
            // dictionary. `try_is_dictionary_of_type` only matches
            // `ObjectValue::Dictionary` (mirroring qpdf's own
            // `isDictionaryOfType`, `libqpdf/QPDFObjectHandle.cc:461-466`),
            // so it can never match here; `try_is_stream_of_type` mirrors
            // qpdf's dedicated `isStreamOfType` (`:468-471`) instead.
            if object_handle.try_is_stream_of_type(b"XRef", b"")?
                || object_handle.try_is_stream_of_type(b"ObjStm", b"")?
            {
                continue;
            }
            all_refs.push(r);
        }

        // Resurrect null-resolving references reached via a surviving (array)
        // edge that have NO xref entry (truly missing). Free entries are already
        // admitted above — they are in `object_refs()` (CacheEntry::Deleted) and
        // pass the `reachable` filter — so add only the missing ones. qpdf treats
        // a missing array ref exactly like a free one: a renumbered `null` body
        // object the array points at (verified byte-identical, /ID masked). The
        // set is drop-aware (a null ref reached only as a dict value is omitted),
        // so a dict-only missing ref stays dropped, not resurrected.
        // `all_refs` is sorted (it filters the sorted `object_refs()` in order),
        // so a binary search rejects the already-admitted (free) refs without
        // allocating a temporary set.
        //
        // `resurrectable` is kept alive to pass to `compute_closure`, which
        // includes these refs in the page closure they're first reached from.
        // qpdf classifies them as first-page section objects (Part 2) when reached
        // from the first page, giving them HIGH object numbers. Without this, they
        // land in part4_rest with LOW numbers.
        let source_had_compressed_objects = pdf
            .source_xref_entries()
            .iter()
            .any(|(_reference, offset)| matches!(offset, crate::XrefEntry::Compressed { .. }));
        let operation_removes_stale_generations = use_generate_objstm
            || (matches!(
                object_stream_mode,
                crate::writer::ObjectStreamMode::Preserve
            ) && source_had_compressed_objects);
        let removed_refs = if operation_removes_stale_generations {
            crate::writer::object_streams::compressible_objgens_qpdf_plan(pdf)?.removed_refs
        } else {
            BTreeSet::new()
        };
        all_refs.retain(|reference| !removed_refs.contains(reference));
        let resurrectable =
            crate::writer::rewrite_renumber::resurrectable_null_refs_excluding(pdf, &removed_refs)?;
        let mut resurrected: Vec<ObjectRef> = resurrectable
            .iter()
            .filter(|r| all_refs.binary_search(r).is_err())
            .copied()
            .collect();
        if !resurrected.is_empty() {
            all_refs.append(&mut resurrected);
            // Keep source-object-number order (object_refs() is already sorted);
            // the resurrected refs slot in at their numeric position.
            all_refs.sort();
        }

        let total_object_count = all_refs.len() as u32;
        let root_ref = pdf.root_ref();
        let info_handle = pdf.trailer().try_get_key(b"/Info")?;
        let info_ref = info_handle
            .object_ref()
            .or_else(|| info_handle.object_ref());
        let pages_tree_ref = if let Some(root_ref) = root_ref {
            let root_handle = pdf.get_object_handle(root_ref);
            pdf.resolve(&root_handle)?;
            let pages_handle = root_handle.try_get_key(b"/Pages")?;
            pages_handle
                .object_ref()
                .or_else(|| pages_handle.object_ref())
        } else {
            None // cov:ignore: reachable_object_set rejects a rootless document before this successful-plan path
        };

        // ----------------------------------------------------------------
        // Step 2: collect page references.
        // Propagate page-tree errors so a malformed /Pages does not silently
        // produce an empty page_hints (which would corrupt downstream hint tables).
        // ----------------------------------------------------------------
        let page_refs: Vec<ObjectRef> = crate::pages::page_refs(pdf)?;

        // The live object set is invariant across every page's closure; compute it
        // once so the per-page `compute_closure` calls below do not each re-scan
        // the whole xref table (which would be O(pages × objects)).
        let live: BTreeSet<ObjectRef> = pdf.live_object_refs().into_iter().collect();
        let mut all_referenced_pages: BTreeMap<ObjectRef, BTreeSet<u32>> = BTreeMap::new();
        for (object_ref, _) in optimization.object_users() {
            let pages = optimization.referenced_pages(object_ref);
            if !pages.is_empty() {
                all_referenced_pages.insert(object_ref, pages);
            }
        }
        let first_page_users = optimization
            .objects_for(&crate::optimization::ObjectUser::Page(0))
            .clone();
        let root_objects = optimization
            .objects_for(&crate::optimization::ObjectUser::Root)
            .clone();
        let mut open_document_objects = optimization.objects_for_trailer_key(b"Encrypt");
        for key in OPEN_DOCUMENT_CATALOG_KEYS {
            open_document_objects.extend(optimization.objects_for_root_key(key));
        }
        let all_outline_refs = optimization.objects_for_root_key(b"Outlines");
        let document_other_objects: BTreeSet<ObjectRef> = optimization
            .object_users()
            .filter_map(|(object_ref, users)| {
                users
                    .iter()
                    .any(is_document_other_user)
                    .then_some(object_ref)
            })
            .collect();
        let elig_ctx = if use_generate_objstm {
            Some(eligibility_context(pdf)?)
        } else {
            None
        };

        // ----------------------------------------------------------------
        // Step 3: compute first-page closure
        // ----------------------------------------------------------------
        let mut first_page_closure: Vec<ObjectRef> = if let Some(&first_page) = page_refs.first() {
            compute_closure_with_stream_parameters(
                pdf,
                first_page,
                &live,
                &resurrectable,
                &skipped_stream_parameter_streams,
            )? // cov:ignore: LLVM maps this covered first-page closure call terminator to a zero-count continuation region
        } else {
            Vec::new()
        };
        first_page_closure.retain(|object_ref| first_page_users.contains(object_ref));

        // compute_closure does not follow a stream dict's /Length (qpdf directizes
        // /Length before computing object users), so a stream's indirect /Length
        // holder never enters a page closure: an orphan holder (referenced only via
        // /Length) is GC'd by the all_refs filter above, and a kept holder is
        // reached only via its other (e.g. catalog) edge and partitions from there
        // (for the indirect-length and shared-stream cases).
        let first_page_set: BTreeSet<ObjectRef> = first_page_closure.iter().copied().collect();

        // ----------------------------------------------------------------
        // Step 4: compute closures for pages 2..N and find shared objects
        // ----------------------------------------------------------------
        // Build a full inverse map: (object_ref → set of page indices) across
        // ALL pages (0..N).  This is used to determine which objects are shared
        // between multiple pages regardless of whether they appear in the
        // first-page closure.
        //
        // `shared_page_indices` retains the old semantics for Part 3 partitioning
        // (only first-page-set objects that also appear in other pages).
        // `all_referenced_pages` was derived above from the exact qpdf-style
        // page-user traversal and is the sole retained object-to-page inverse
        // map. Closures here remain the authoritative ordered inputs for page
        // partitioning and hint construction.
        let mut shared_page_indices: BTreeMap<ObjectRef, BTreeSet<u32>> = BTreeMap::new();
        let mut other_page_closures: Vec<Vec<ObjectRef>> =
            Vec::with_capacity(page_refs.len().saturating_sub(1));

        for (page_idx, &page_ref) in page_refs.iter().enumerate().skip(1) {
            let mut closure = compute_closure_with_stream_parameters(
                pdf,
                page_ref,
                &live,
                &resurrectable,
                &skipped_stream_parameter_streams,
            )?; // cov:ignore: LLVM maps this covered later-page closure call terminator to a zero-count continuation region
            let page_users =
                optimization.objects_for(&crate::optimization::ObjectUser::Page(page_idx as u32));
            closure.retain(|object_ref| page_users.contains(object_ref));
            for obj_ref in &closure {
                // Track cross-page sharing for first-page objects (used by Part 3 partition).
                if first_page_set.contains(obj_ref) {
                    shared_page_indices
                        .entry(*obj_ref)
                        .or_default()
                        .insert(page_idx as u32);
                }
            }
            other_page_closures.push(closure);
        }

        // ----------------------------------------------------------------
        // Step 4b: thumb-set for the first-page private/shared split.
        // ----------------------------------------------------------------
        // qpdf gives a page's /Thumb descendants the separate `ou_thumb` user
        // while sharing the page traversal's one ordered `visited` set
        // (QPDF_optimization.cc:261-337). The same exact map supplies ordinary
        // page membership above and thumbnail membership here.
        let thumbnail_user_set = optimization.thumbnail_objects();

        // ----------------------------------------------------------------
        // Step 5: partition into Part 2 (exclusive) and Part 3 (shared)
        // ----------------------------------------------------------------
        // Maintain closure discovery order from first_page_closure for Part 2
        // (page dict first, then its `/Resources` subtree, then `/Contents`,
        // matching qpdf's first-page object numbering).
        //
        // The page-1 dictionary itself is pinned to Part 2 even if another
        // page directly references it; the linearization layout requires
        // that the first page object live at the start of Part 2 (it is the
        // anchor reached via /O in the parameter dict).  Without this pin
        // a circular page-tree reference (or a deliberately-shared page
        // dict) would silently demote the page object into Part 3.
        let first_page_ref = page_refs.first().copied();
        let mut part2_objects: Vec<ObjectRef> = Vec::new();
        let mut part3_objects: Vec<ObjectRef> = Vec::new();

        for obj_ref in &first_page_closure {
            // qpdf: in_open_document takes precedence over in_first_page in EVERY
            // object-stream mode. Objects reachable from catalog open-document keys
            // (/AcroForm, /OpenAction, etc.) are placed in the open-document section
            // (part4, first half, before /O) even when they also appear in the
            // first-page closure, so peel them out of Part 2/3 here:
            //   - generate: eligible ones pack into the open-document ObjStm
            //     container (route_objstm_containers -> ContainerPart::OpenDocument);
            //     ineligible streams emit plain pre-/O (part4_open_document_plain).
            //   - disable/preserve: all are emitted plain pre-/O.
            // Verified against qpdf 11.9.0: acroform-widget-page0-5-10 in disable
            // mode places the AcroForm dict + widgets in part4, NOT the first-page
            // section (the 12 first-page objects there are the page dict + content
            // + 10 Fonts, not the widgets).
            //
            // in_outlines outranks in_first_page the same way (and outranks
            // in_open_document, so check it first): an object reached from both the
            // first page and /Outlines is lc_outlines (second-half part9, or part6
            // under /UseOutlines), never part2/part3. Peel it here so the part4
            // outline extraction in Step 8 can place it. Verified against qpdf
            // 11.9.0: outlines-shared-page-80-80's /Extra-referenced font is the
            // last second-half object, not a first-page-shared object.
            if root_objects.contains(obj_ref)
                || all_outline_refs.contains(obj_ref)
                || open_document_objects.contains(obj_ref)
            {
                continue;
            }
            if Some(*obj_ref) == first_page_ref {
                part2_objects.push(*obj_ref);
            } else if shared_page_indices.contains_key(obj_ref)
                || document_other_objects.contains(obj_ref)
                || thumbnail_user_set.contains(obj_ref)
            {
                // lc_first_page_shared: in_first_page AND (other_pages>0 ||
                // others>0 || thumbs>0). `shared_page_indices` supplies
                // other_pages (another page's closure); `document_other_objects`
                // supplies others (a document-level reference such as a Catalog
                // key); `thumbnail_user_set` supplies thumbs (a page's /Thumb
                // target). Any of these makes the object shared
                // (QPDF_linearization.cc:1124-1127), so it sorts after the
                // first-page-private objects in part 6.
                part3_objects.push(*obj_ref);
            } else {
                part2_objects.push(*obj_ref);
            }
        }
        // qpdf packs first-half shared objects in ascending source object number
        // order (observed against qpdf 11.9.0: ObjStm member ordering matches
        // source number order, not the BFS discovery order which follows dict key
        // alphabetical order). Mirror the same sort used in `fold_pages_tree_into_first_half`.
        part3_objects.sort_unstable_by_key(|r| r.number);
        // qpdf numbers the first-page section (qpdf part6) as: the first-page
        // object first, then the remaining first-page-private objects in
        // ascending source object number order — NOT compute_closure's
        // /Resources-DFS discovery order, which only coincides when resource
        // streams are numbered below the page's content stream. Pin the page
        // dict first (qpdf pushes the first-page object explicitly) and sort the
        // rest by source number. Oracle: qpdf 11.9.0 on a 1-page image fixture
        // orders Page, Contents, Image when Contents < Image by source number
        // (and Page, Image, Contents when Image < Contents), in both generate
        // and disable mode.
        part2_objects.sort_unstable_by_key(|r| (Some(*r) != first_page_ref, r.number));

        // ----------------------------------------------------------------
        // Step 6: build Part 4 by removing Part 2 and Part 3 objects.
        //
        // Provisional list — the final order (per-page private groups
        // contiguous, then leftover globally-shared) is computed below in
        // Step 7 once we know which objects belong to which page.
        let moved: BTreeSet<ObjectRef> = part2_objects
            .iter()
            .chain(&part3_objects)
            .copied()
            .collect();
        let part4_provisional: Vec<ObjectRef> = all_refs
            .into_iter()
            .filter(|r| !moved.contains(r))
            .collect();

        // ----------------------------------------------------------------
        // Step 7: build page_hints and per_page_private_objects
        // ----------------------------------------------------------------
        let mut page_hints: Vec<PageHintEntry> = page_refs
            .iter()
            .map(|&r| PageHintEntry::placeholder(r))
            .collect();

        // For quick membership checks across all pages.
        let part2_set: BTreeSet<ObjectRef> = part2_objects.iter().copied().collect();
        let part3_set: BTreeSet<ObjectRef> = part3_objects.iter().copied().collect();

        // Page 0: private objects = Part 2 objects.
        let page0_private = part2_objects.clone();

        // Fill page-0 hint: first_object_index = 0; object_count = Part 2 +
        // Part 3 (shared) objects, since the first-page section physically
        // contains both before /E.  qpdf's hint-table checker validates
        // object_count[0] against the count of objects in [first_page_offset,
        // /E), which equals |Part 2| + |Part 3|.
        if !page_hints.is_empty() {
            page_hints[0].first_object_index = 0;
            page_hints[0].object_count = (page0_private.len() + part3_objects.len()) as u32;
        }

        // Per-page private object lists, page 0 first.
        let mut per_page_private_objects: Vec<Vec<ObjectRef>> = Vec::with_capacity(page_refs.len());
        per_page_private_objects.push(page0_private);

        // Pages 1..N: private objects = closure(i) ∩ (reachable from exactly
        // 1 page).  Excluding only part2_set / part3_set is too narrow:
        // globally-shared objects like the Catalog or /Pages tree intermediate
        // nodes are reachable from EVERY page, including page 0 (via the
        // /Parent chain), so they sit in our part4_objects rather than
        // part3_objects.  qpdf's per-page object_count and page_length only
        // count objects exclusive to one page (it walks the file body forward
        // from the page object and stops at the first non-exclusive object),
        // so we mirror that by checking page-reach-count == 1.
        let mut all_closures: Vec<Vec<ObjectRef>> = Vec::with_capacity(page_refs.len());
        all_closures.push(first_page_closure.clone());
        all_closures.extend(other_page_closures.iter().cloned());
        let mut page_reach: BTreeMap<ObjectRef, u32> = BTreeMap::new();
        for closure in &all_closures {
            let unique: BTreeSet<ObjectRef> = closure.iter().copied().collect();
            for r in unique {
                *page_reach.entry(r).or_insert(0) += 1;
            }
        }

        for (i, closure) in other_page_closures.into_iter().enumerate() {
            let page_idx = i + 1; // skip(1) above started page indexing at 1
            let private: Vec<ObjectRef> = closure
                .into_iter()
                .filter(|r| {
                    if part2_set.contains(r) || part3_set.contains(r) {
                        return false;
                    }
                    // Open-document objects (AcroForm widgets, etc.) that happen
                    // to be exclusive to one later page must NOT be counted as
                    // page-private: qpdf routes them to the pre-/O open-document
                    // section (not the per-page section) in every mode, so they are
                    // absent from the second-half page objects and should not
                    // inflate page_hints[page_idx].object_count.  Excluding them
                    // here also keeps them out of per_page_private_objects, so the
                    // part7 pre-pass below never captures them and they remain
                    // available for OD routing in the part8/part9 loop.
                    if open_document_objects.contains(r) {
                        return false;
                    }
                    // Outline objects (in_outlines) outrank other-page-private the
                    // same way: keep them out of the per-page-private set so the
                    // part7 pre-pass never claims them and they stay available for
                    // outline routing in the part8/part9 loop.
                    if all_outline_refs.contains(r) {
                        return false;
                    }
                    // qpdf routes a non-first-page object to lc_other_page_private
                    // (part7) ONLY when others==0 (QPDF_linearization.cc:1128). An
                    // object also reached by a document-level `others` reference
                    // (a Catalog non-open-document key, or a trailer key other than
                    // /Root,/Encrypt) is lc_other (part9) even at other_pages==1.
                    // Keep it out of the per-page-private set so it is neither placed
                    // in part7 nor counted in this page's part7 object_count hint; it
                    // flows through part4_provisional into the part8/part9 loop and
                    // lands in part4_rest (part9).
                    if document_other_objects.contains(r) {
                        return false;
                    }
                    // qpdf's lc_other_page_private predicate also requires
                    // thumbs==0. A normal page object that is another page's
                    // thumbnail belongs to part9, not part7.
                    if thumbnail_user_set.contains(r) {
                        return false;
                    }
                    page_reach.get(r).copied() == Some(1)
                })
                .collect();
            if page_idx < page_hints.len() {
                // Use private count; guarantee at least 1 so hint table isn't all zeros.
                let count = private.len().max(1) as u32;
                page_hints[page_idx].object_count = count;
            }
            per_page_private_objects.push(private);
        }

        // ----------------------------------------------------------------
        // Step 6b: partition Part 4 into qpdf part7 / part8 / part9.
        //
        // qpdf numbers objects in the second half (Part 4) as:
        //   part7 (other pages' private): objects reached by exactly ONE
        //     other page (pages 2..N), iterated page by page in closure order.
        //   part8 (other pages' shared): objects reached by TWO OR MORE
        //     other pages (but NOT page 1), in plan order.
        //   part9 (rest): everything else — Pages tree, Info, lc_other, and
        //     objects not reached from any page closure (trailer-only refs).
        //
        // The renumber pass uses these three sub-partitions directly.
        // `part4_objects` is then built as part7 ++ part8 ++ part9 so the
        // writer (which iterates `part4_objects`) emits bytes in the same
        // order as the renumber map.

        // page_reach counts how many of (first_page_closure, other_page_closures...)
        // contain the object.  For an object NOT in first_page_set:
        //   - page_reach == 1 → exactly one other page → part7
        //   - page_reach >= 2 → two or more other pages → part8
        //   - page_reach == 0 → no page closure → part9
        let provisional_set: BTreeSet<ObjectRef> = part4_provisional.iter().copied().collect();
        // `all_outline_refs` (qpdf's `in_outlines` set) was computed in Step 1c and
        // already peeled out of part2/part3 (Step 5) and the per-page-private sets
        // (Step 7); the loop below routes its members to part4_rest with top
        // precedence so the Step 8 outline extraction places them.
        let mut part4_other_pages_private: Vec<ObjectRef> = Vec::new();
        let mut part4_other_pages_shared: Vec<ObjectRef> = Vec::new();
        let mut part4_rest: Vec<ObjectRef> = Vec::new();
        let mut part4_open_document_plain: Vec<ObjectRef> = Vec::new();
        // Track which objects are already in part7 (private) to build in page order.
        let mut placed_private: BTreeSet<ObjectRef> = BTreeSet::new();

        // part7: iterate pages 2..N in order, closure order within each page.
        // Use per_page_private_objects[1..] — these are already private (reach==1).
        for privates in per_page_private_objects.iter().skip(1) {
            for &r in privates {
                if provisional_set.contains(&r) && placed_private.insert(r) {
                    part4_other_pages_private.push(r);
                }
            }
        }

        // part8 and part9: iterate provisional in original order.
        for &r in &part4_provisional {
            if placed_private.contains(&r) {
                // Already in part7.
                continue;
            }
            // in_outlines has top precedence (QPDF_linearization.cc:1120, above
            // in_open_document and in_first_page). Route every outline object to
            // part4_rest so the Step 8 extraction lifts it into the outline section
            // (part6 under /UseOutlines, else part9). They were peeled from
            // part2/part3 (Step 5) and the per-page-private sets (Step 7), so they
            // reach this loop via part4_provisional even when first-page-reachable.
            if all_outline_refs.contains(&r) {
                part4_rest.push(r);
                continue;
            }
            let reach = page_reach.get(&r).copied().unwrap_or(0);
            // OD+first-page objects were peeled out of Part 2/3 by Step 5 in
            // every mode, so they ARE present in part4_provisional and must not be
            // treated as first-page here — they flow to the OD routing below. A
            // genuine first-page object reaching this point is skipped defensively.
            let in_first_page = first_page_set.contains(&r)
                && !root_objects.contains(&r)
                && !open_document_objects.contains(&r);
            if in_first_page {
                // Should have been in Part 2 or Part 3 — skip (defensive).
                continue;
            }
            // Route the qpdf root and open-document objects to part4 (first
            // half, before /O) in EVERY mode — qpdf's part4 is
            // [lc_root] ++ lc_open_document. Outline objects (which qpdf
            // orders above in_open_document) were already routed above.
            if root_objects.contains(&r) || open_document_objects.contains(&r) {
                if let Some(ctx) = elig_ctx.as_ref() {
                    // generate mode: an OD object eligible for ObjStm packing goes
                    // to part4_rest (the batch planner packs it into the
                    // open-document container); an ineligible stream (e.g. an /AP /N
                    // appearance stream, which cannot be an ObjStm member) emits
                    // plain pre-/O.  Oracle: qpdf --object-streams=generate places
                    // such a Form XObject at a lower object number than the OD
                    // ObjStm, physically before the hint stream.
                    let obj = pdf.get_object_handle(r);
                    if is_eligible_for_objstm_handle(r, &obj, ctx)? {
                        part4_rest.push(r);
                    } else {
                        part4_open_document_plain.push(r);
                    }
                } else {
                    // disable/preserve mode: no ObjStm, so every OD object is a
                    // plain pre-/O (part4) object emitted between the Catalog and
                    // the hint stream.
                    part4_open_document_plain.push(r);
                }
                continue;
            }
            // What remains: non-outline, non-open-document objects. Partition by
            // page reach (qpdf's other_pages count): two or more other pages →
            // lc_other_page_shared (part8); otherwise lc_other (part9).
            if reach >= 2 {
                part4_other_pages_shared.push(r);
            } else {
                // reach == 0 (trailer-/document-only), or reach == 1 with others>0
                // (excluded from per_page_private above, so it is lc_other not
                // lc_other_page_private — QPDF_linearization.cc:1128).
                // Both are qpdf part9.
                part4_rest.push(r);
            }
        }

        debug_assert_eq!(
            part4_other_pages_private.len()
                + part4_other_pages_shared.len()
                + part4_rest.len()
                + part4_open_document_plain.len(),
            part4_provisional.len(),
            "Part-4 sub-partition must preserve membership"
        );

        // qpdf builds part4 as [lc_root] ++ lc_open_document, where
        // lc_open_document is a std::set<QPDFObjGen> — i.e. ascending
        // (object number, generation) (QPDF_linearization.cc:1179-1182). The
        // Catalog (lc_root) is placed separately by the renumber map's root_ref
        // promote, so order part4_open_document_plain to match. Sort by the full
        // ObjectRef (its derived Ord is number-then-generation, mirroring
        // QPDFObjGen) rather than the object number alone, so refs that share an
        // object number across generations keep qpdf's tie-break order.
        part4_open_document_plain.sort_unstable();

        // ----------------------------------------------------------------
        // Step 8: build shared_hints
        // ----------------------------------------------------------------
        // The Shared Object Hint Table covers ALL objects in the first-page
        // section (Part 2 + Part 3) plus any Part-4 shared objects.
        //
        // qpdf always lists all objects in the first-page section in the SO
        // hint table, even for single-page PDFs where no objects are truly
        // shared across pages.  We match this behaviour unconditionally:
        // shared_hints is always non-empty whenever part2_objects is non-empty.
        //
        // Layout of shared_hints (in file order):
        //   [part2 entries]   - first-page section private objects (page 0 owns
        //                       them by physical position; referencing_pages = [])
        //   [part3 entries]   - first-page section shared objects (also owned by
        //                       page 0 physically; referencing_pages lists pages
        //                       1..N that also use them, NOT page 0)
        //   [outline entries] - outline objects routed to the first-page section
        //                       when /PageMode /UseOutlines is set; physically
        //                       owned by page 0 via layout (referencing_pages = [])
        //   [part4_shared]    - Part-4 shared objects (after /E; owned by no
        //                       page via physical position; referencing_pages lists
        //                       ALL pages that reference them)

        // Outline objects routed to the first-page section when
        // /PageMode /UseOutlines is set (QPDF_linearization.cc:1031-1043).
        // Must be built before shared_hints so they can be included in it.
        //
        // For the classic (non-ObjStm) linearize path, outlines in part4_rest
        // need to be extracted into dedicated fields so the renumber map can
        // assign them the correct half:
        //   part6_outline_objects — UseOutlines: first-half numbers, emitted before /E
        //   part9_outline_objects — !UseOutlines: second-half numbers, emitted after /E
        let outlines_in_first_page = outlines_in_first_page_predicate(pdf)?;
        // `all_outline_refs` is computed once above (step 6b), before the
        // open-document routing that defers to it.
        // Outline root reference: placed first in the extracted vectors so the
        // renumber map assigns it the lowest new unit among outline objects,
        // matching qpdf's lc_outlines traversal-from-root order (used by
        // compute_outline_hint_info's first_object).
        let outline_root_ref: Option<ObjectRef> = if let Some(root_ref) = pdf.root_ref() {
            let root_handle = pdf.get_object_handle(root_ref);
            pdf.resolve(&root_handle)?;
            let outlines = root_handle.try_get_key(b"/Outlines")?;
            outlines.object_ref()
        } else {
            None // cov:ignore: reachable_object_set rejects a rootless document before outline planning can succeed
        };

        let extract_outlines = |src: &[ObjectRef]| -> Vec<ObjectRef> {
            let mut v: Vec<ObjectRef> = src
                .iter()
                .filter(|r| all_outline_refs.contains(r))
                .copied()
                .collect();
            // Rotate root to front so it receives the lowest consecutive new number.
            if let Some(root) = outline_root_ref {
                if let Some(pos) = v.iter().position(|&r| r == root) {
                    v[..=pos].rotate_right(1);
                }
            }
            v
        };

        let (part6_outline_objects, part9_outline_objects): (Vec<ObjectRef>, Vec<ObjectRef>) =
            if outlines_in_first_page {
                (extract_outlines(&part4_rest), vec![])
            } else {
                (vec![], extract_outlines(&part4_rest))
            };
        // Remove extracted outlines from part4_rest to avoid double assignment.
        let outline_extract_set: BTreeSet<ObjectRef> = part6_outline_objects
            .iter()
            .chain(&part9_outline_objects)
            .copied()
            .collect();
        part4_rest.retain(|r| !outline_extract_set.contains(r));

        // For UseOutlines: outlines are emitted before /E and count toward page 0.
        if outlines_in_first_page && !page_hints.is_empty() {
            page_hints[0].object_count += part6_outline_objects.len() as u32;
        }

        // Use part6_outline_objects (already root-first, only objects actually
        // extracted from part4_rest) so that shared_hints iteration order matches
        // the physical emitted order and objects also reachable from a page closure
        // are not double-counted in shared_hints.
        let outline_first_page_members: Vec<ObjectRef> = if outlines_in_first_page {
            part6_outline_objects.clone()
        } else {
            vec![]
        };

        let part2_entries = part2_objects.iter().map(|&obj_ref| SharedObjectHintEntry {
            object_ref: obj_ref,
            referencing_pages: vec![],
        });
        let part3_entries = part3_objects.iter().map(|&obj_ref| {
            let pages: Vec<u32> = shared_page_indices
                .get(&obj_ref)
                .map(|s| s.iter().copied().collect())
                .unwrap_or_default();
            // Do NOT add page 0: Part-3 shared objects are in the first-page
            // section, so page 0 implicitly owns them by physical layout.
            SharedObjectHintEntry {
                object_ref: obj_ref,
                referencing_pages: pages,
            }
        });
        // Outline objects are in the first-page section (physically owned by
        // page 0), so page 0 is not listed in referencing_pages.
        let outline_entries =
            outline_first_page_members
                .iter()
                .map(|&obj_ref| SharedObjectHintEntry {
                    object_ref: obj_ref,
                    referencing_pages: vec![],
                });
        // Part-4 shared objects: referenced by ≥ 2 pages but NOT in the
        // first-page closure.  These live after /E (not physically owned
        // by any page via layout), so ALL referencing pages are listed.
        let part4_shared_entries = part4_other_pages_shared.iter().map(|&obj_ref| {
            let pages: Vec<u32> = all_referenced_pages
                .get(&obj_ref)
                .map(|s| s.iter().copied().collect())
                .unwrap_or_default();
            SharedObjectHintEntry {
                object_ref: obj_ref,
                referencing_pages: pages,
            }
        });
        let shared_hints: Vec<SharedObjectHintEntry> = part2_entries
            .chain(part3_entries)
            .chain(outline_entries)
            .chain(part4_shared_entries)
            .collect();

        Ok(Self {
            part1_objects: Vec::new(),
            part2_objects,
            part3_objects,
            part4_other_pages_private,
            part4_other_pages_shared,
            part4_rest,
            part4_open_document_plain,
            total_object_count,
            root_ref,
            pages_tree_ref,
            info_ref,
            page_hints,
            shared_hints,
            per_page_private_objects,
            all_referenced_pages,
            outline_first_page_members,
            part9_outline_objects,
            part6_outline_objects,
            object_stream_mode,
            removed_refs,
            optimization: Some(optimization),
            content_normalize_refs,
        })
    }

    /// Return the set of all objects assigned to at least one part.
    ///
    /// Part-4 body objects (Annex F Part 5), in the order the writer emits them.
    ///
    /// This is a derived view: the ordered concatenation of
    /// [`part4_other_pages_private`](Self::part4_other_pages_private),
    /// [`part4_other_pages_shared`](Self::part4_other_pages_shared), and
    /// [`part4_rest`](Self::part4_rest). Callers that previously read a
    /// `part4_objects` field should call this getter instead — it cannot
    /// drift from the three sub-partitions because there is no separate
    /// backing storage.
    pub fn part4_objects(&self) -> Vec<ObjectRef> {
        let mut objects: Vec<ObjectRef> = self
            .part4_other_pages_private
            .iter()
            .chain(&self.part4_other_pages_shared)
            .copied()
            .collect();

        // qpdf's root /Pages user can contain several nested Pages nodes. It
        // places every such node that remains in lc_other before the rest of
        // part9, not only the Catalog's direct /Pages object.
        let part9_pages: BTreeSet<ObjectRef> = self
            .optimization
            .as_ref()
            .map(|optimization| optimization.objects_for_root_key(b"Pages"))
            .filter(|pages| !pages.is_empty())
            .unwrap_or_else(|| self.pages_tree_ref.into_iter().collect());
        for pages_tree in part9_pages.iter().copied() {
            if self.part4_rest.contains(&pages_tree) {
                objects.push(pages_tree);
            }
        }
        objects.extend(self.part9_outline_objects.iter().copied());
        objects.extend(
            self.part4_rest
                .iter()
                .copied()
                .filter(|object| !part9_pages.contains(object)),
        );
        objects
    }

    /// Fold first-page-section ObjStm members into their containers to match
    /// qpdf's shared-object hint list.
    ///
    /// When the first-page shared dicts are packed into a first-half ObjStm
    /// container, qpdf lists the *container* (one entry) in the shared-object
    /// hint table — not each compressed member.  This rewrites
    /// [`shared_hints`](Self::shared_hints): every member present in
    /// `member_to_container` is replaced by a single entry for its container
    /// (with the `referencing_pages` of all that container's members unioned),
    /// and the second-and-later members of the same container are dropped.
    /// Non-member entries are kept verbatim.
    ///
    /// The first-page section of the result is then emitted in **ascending
    /// physical object-number order**.  qpdf's `checkHSharedObject` walks the
    /// first-page shared entries positionally, starting from the first page
    /// object, so the hint list must follow the order in which
    /// [`RenumberMap::place_objstm_members_per_half`](crate::linearization::renumber::RenumberMap::place_objstm_members_per_half)
    /// numbers the first half (plain objects first, then containers, then
    /// compressed members).  A plain (ineligible) shared stream can therefore be
    /// numbered *before* the container of the eligible dicts even when the
    /// container appeared earlier in `shared_hints`; the sort restores the
    /// physical order.  Part-8 entries (`part4_other_pages_shared`, after /E)
    /// are left in place.
    ///
    /// The container entry's `object_ref` carries the container's *new* object
    /// number with generation `u16::MAX` — a sentinel no live object uses,
    /// marking it as a synthetic container entry rather than a resolvable PDF
    /// object.  A plain entry carries an original ref whose physical number is
    /// resolved through `renumber`; a synthetic container entry already carries
    /// its new number, so it is never resolved through the
    /// [`RenumberMap`].
    ///
    /// ObjStm container object numbers that are qpdf **part8** (other-page-shared)
    /// objects: their members reach two or more pages but none is a first-page
    /// (Part-2 / Part-3) object.
    ///
    /// The global even split can fill such a container entirely with objects that
    /// are individually page-*private* (one page's privates co-located with
    /// another's), so the container does not appear in `shared_hints` (built from
    /// the per-object part2/part3/part4_shared partition) even though it is a
    /// shared object that belongs in the shared-object hint table. This enumerates
    /// those containers so the table and its entry counts include them.
    pub(crate) fn part8_container_nums(
        &self,
        member_to_container: &BTreeMap<ObjectRef, (u32, u32)>,
    ) -> BTreeSet<u32> {
        let first_page: BTreeSet<ObjectRef> = self
            .part2_objects
            .iter()
            .chain(&self.part3_objects)
            .copied()
            .collect();
        let part4_shared: BTreeSet<ObjectRef> =
            self.part4_other_pages_shared.iter().copied().collect();

        let mut all_containers: BTreeSet<u32> = BTreeSet::new();
        let mut container_pages: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
        let mut has_first_page_member: BTreeSet<u32> = BTreeSet::new();
        let mut has_shared_member: BTreeSet<u32> = BTreeSet::new();
        for (member, &(cnum, _)) in member_to_container {
            all_containers.insert(cnum);
            if first_page.contains(member) {
                has_first_page_member.insert(cnum);
            }
            // A reach-≥2 (part4_other_pages_shared) member makes the container a
            // shared object directly — used when `all_referenced_pages` is absent
            // (e.g. manually-built plans) and as a robust signal otherwise.
            if part4_shared.contains(member) {
                has_shared_member.insert(cnum);
            }
            if let Some(pages) = self.all_referenced_pages.get(member) {
                container_pages
                    .entry(cnum)
                    .or_default()
                    .extend(pages.iter().copied().filter(|&p| p != 0));
            }
        }
        // A container is part8 when no member is a first-page object AND it is
        // shared — either it holds an explicitly-shared (reach-≥2) member, or its
        // members span two or more pages (the even split co-located two pages'
        // privates).
        all_containers
            .into_iter()
            .filter(|cnum| {
                !has_first_page_member.contains(cnum)
                    && (has_shared_member.contains(cnum)
                        || container_pages.get(cnum).is_some_and(|p| p.len() >= 2))
            })
            .collect()
    }

    /// ObjStm container new-numbers routed to part9 (Rest) by qpdf's outline
    /// priority (QPDF_linearization.cc:1118-1122): every container carrying a
    /// `part9_outline_objects` member.
    ///
    /// Such a container is placed in the second half even when the global even
    /// split co-locates a `part4_other_pages_shared` object in it, and it is NOT
    /// a shared object in qpdf's Shared Object Hint Table. Both
    /// [`Self::canonical_shared_hints`] (the Part-8 main-loop guard and the
    /// `part8_container_nums` enumeration tail) and
    /// `SharedObjectHintTable::from_plan` (the Part-8 entry COUNT, which feeds
    /// `first_page_entries`) must exclude it, or the table's entry list and its
    /// header counts disagree.
    ///
    /// A member reachable from BOTH `/Outlines` and ≥2 non-first pages stays in
    /// `part4_other_pages_shared` rather than `part9_outline_objects`, so a
    /// container carrying ONLY such a member (no other `part9_outline_objects`
    /// member) is missed here. That can only happen across a 2+-container even
    /// split, which is blocked by the page-dict-erasure boundary divergence
    /// (flpdf-g1eu); the robust fix (keying on the actual Rest routing) lands with
    /// that issue's 2-container fixture. `part9_outline_objects` is small, so look
    /// each up in `member_to_container` rather than scanning all members.
    pub(crate) fn rest_container_nums(
        &self,
        member_to_container: &BTreeMap<ObjectRef, (u32, u32)>,
    ) -> BTreeSet<u32> {
        self.part9_outline_objects
            .iter()
            .filter_map(|member| member_to_container.get(member).map(|&(cnum, _)| cnum))
            .collect()
    }

    /// An empty `member_to_container` yields a clone of `shared_hints` (the
    /// no-ObjStm / classic path is unchanged).
    pub(crate) fn canonical_shared_hints(
        &self,
        member_to_container: &BTreeMap<ObjectRef, (u32, u32)>,
        renumber: &RenumberMap,
        second_half_container_nums: &BTreeSet<u32>,
        open_document_container_nums: &BTreeSet<u32>,
    ) -> Vec<SharedObjectHintEntry> {
        if member_to_container.is_empty() {
            return self.shared_hints.clone();
        }

        // The first-page section of `shared_hints` is the leading part2 ++ part3
        // ++ outline entries; trailing entries are Part-8 (`part4_other_pages_shared`,
        // after /E).
        // Invariant: this split is only correct because `Self::new` builds
        // `shared_hints` as exactly `part2_entries ++ part3_entries ++
        // outline_entries ++ part4_shared_entries` (one entry per object, no filter).
        // Keep the two in lockstep — reordering the construction there silently
        // breaks this boundary.
        let first_page_input = self.part2_objects.len()
            + self.part3_objects.len()
            + self.outline_first_page_members.len();

        // Containers routed to part9 (Rest) by qpdf's outline priority: never
        // shared objects in the SOHT, so skip them in the Part-8 section here AND
        // in the `part8_container_nums` enumeration tail below. The first-page
        // section is already covered by the `second_half_container_nums` guard
        // (it skips ALL second-half containers there); in the Part-8 section that
        // guard cannot be reused because it would also drop legitimate part8
        // containers. See [`Self::rest_container_nums`].
        let rest_container_nums = self.rest_container_nums(member_to_container);

        // Position (index into the output list) at which each container was
        // first emitted, so later members of the same container fold into it.
        let mut container_pos: BTreeMap<u32, usize> = BTreeMap::new();
        let mut out: Vec<SharedObjectHintEntry> = Vec::with_capacity(self.shared_hints.len());
        let mut first_page_out_end: Option<usize> = None;

        for (input_idx, entry) in self.shared_hints.iter().enumerate() {
            if input_idx == first_page_input {
                // Crossed into the Part-8 region: freeze the first-page boundary.
                first_page_out_end = Some(out.len());
            }
            match member_to_container.get(&entry.object_ref) {
                Some(&(container_num, _idx)) => {
                    // Open-document containers live in the pre-/O region (before
                    // the first-page section and before /E), so qpdf excludes
                    // them from the SOHT unconditionally — regardless of whether
                    // the triggering entry is in the first-page section or in the
                    // Part-8 section.
                    if open_document_container_nums.contains(&container_num) {
                        continue;
                    }
                    // Within the first-page section: skip second-half
                    // (outline-routed) ObjStm containers placed after /E.
                    if input_idx < first_page_input
                        && second_half_container_nums.contains(&container_num)
                    {
                        continue;
                    }
                    // Within the Part-8 section: skip part9 (Rest) containers that
                    // qpdf's outline priority placed in the second half. The
                    // first-page guard above cannot fire here (input_idx is past
                    // the boundary), and `second_half_container_nums` would wrongly
                    // also drop legitimate part8 containers — so key on the
                    // part9-only `rest_container_nums` instead.
                    if input_idx >= first_page_input && rest_container_nums.contains(&container_num)
                    {
                        continue;
                    }
                    if let Some(&pos) = container_pos.get(&container_num) {
                        // Fold into the already-emitted container entry: union
                        // the referencing pages (dedup, keep ascending order).
                        let merged: &mut Vec<u32> = &mut out[pos].referencing_pages;
                        for &p in &entry.referencing_pages {
                            if let Err(insert_at) = merged.binary_search(&p) {
                                merged.insert(insert_at, p);
                            }
                        }
                    } else {
                        // First member of this container: emit one entry for the
                        // container, carrying its new object number. The
                        // generation is the sentinel `u16::MAX`: no live object
                        // ever uses it, so consumers can identify this synthetic
                        // container entry unambiguously — even when `container_num`
                        // coincides with a surviving original object's number,
                        // which would otherwise resolve through a `RenumberMap`.
                        let mut pages = entry.referencing_pages.clone();
                        pages.sort_unstable();
                        pages.dedup();
                        container_pos.insert(container_num, out.len());
                        out.push(SharedObjectHintEntry {
                            object_ref: ObjectRef::new(container_num, u16::MAX),
                            referencing_pages: pages,
                        });
                    }
                }
                None => out.push(entry.clone()),
            }
        }

        // Reorder the first-page section to ascending physical object number —
        // the order qpdf's `checkHSharedObject` walks (positionally from the
        // first page object). `place_objstm_members_per_half` numbers the first
        // half as plain… then containers…, so a plain ObjStm-ineligible shared
        // stream is numbered BEFORE the container of the eligible dicts. A
        // folded container entry carries its new number with the sentinel
        // generation `u16::MAX`; a plain entry carries an original ref resolved
        // through `renumber`. Part-8 entries (after the boundary) stay in place.
        let boundary = first_page_out_end.unwrap_or(out.len());
        let new_number = |e: &SharedObjectHintEntry| -> u32 {
            if e.object_ref.generation == u16::MAX {
                e.object_ref.number
            } else {
                // A malformed source can carry a reserved/dangling reference
                // (object number 0, any generation) into the first-page closure
                // and thus into `shared_hints`; `place_objstm_members_per_half`
                // rebuilds the forward renumber index while dropping object
                // number 0 (it doubles as the reserved-slot sentinel), so such an
                // entry has no mapping here. Sort it deterministically last
                // instead of panicking — the writer's shared-hint back-patch
                // (`new_for_original(..).ok_or_else(Err)`, gated only on a
                // non-empty `shared_hints` and run before any output) then
                // surfaces the planner/renumber inconsistency as a structured
                // error. For a well-formed PDF every shared-hint object has a
                // mapping, so this fallback never triggers and the order is
                // unchanged. Mirrors `SharedObjectHintTable::from_plan`'s
                // `new_for_original(..).map_or(0, ..)` graceful handling.
                renumber
                    .new_for_original(e.object_ref)
                    .map_or(u32::MAX, |r| r.number)
            }
        };
        out[..boundary].sort_unstable_by_key(&new_number);

        // Append any qpdf part8 (other-page-shared) ObjStm container that the
        // even split filled entirely with page-PRIVATE objects: such a container
        // never appears in `shared_hints` (no part2/part3/part4_shared member) but
        // IS a shared object in qpdf's hint table. Skip containers already folded
        // into `out` (those carry a part4_shared member). Then order the whole
        // Part-8 section by physical object number, matching qpdf's ObjGen-keyed
        // `lc_other_page_shared`.
        for cnum in self.part8_container_nums(member_to_container) {
            // Open-document containers live in the pre-/O region (before the
            // first-page section), so qpdf excludes them from the SOHT even
            // when their members span multiple later pages (which would
            // otherwise qualify them as Part-8 shared containers via the
            // `container_pages.len() >= 2` criterion in `part8_container_nums`).
            //
            // A part9 (Rest) container routed there by outline priority must also
            // be excluded here, not just in the main loop above. `part8_container_nums`
            // keys on page reachability (`!has_first_page_member && shared/≥2 pages`),
            // so when the co-located part9 container has NO ObjStm-eligible
            // first-page member (e.g. page 0 carries no compressible private object)
            // it satisfies that predicate and would be re-added as a Part-8 entry —
            // re-introducing exactly the SOHT divergence the main-loop guard removes.
            if !container_pos.contains_key(&cnum)
                && !open_document_container_nums.contains(&cnum)
                && !rest_container_nums.contains(&cnum)
            {
                out.push(SharedObjectHintEntry {
                    object_ref: ObjectRef::new(cnum, u16::MAX),
                    referencing_pages: Vec::new(), // recomputed below
                });
            }
        }
        out[boundary..].sort_unstable_by_key(&new_number);

        // Recompute each entry's referencing pages from its FULL membership via
        // `all_referenced_pages` (excluding page 0, which owns the first-page
        // section and lists no shared identifiers). The fold above unions only
        // the `shared_hints` inputs (part2/part3/part4_shared); the global even
        // split can also place a page's PRIVATE object inside a shared container
        // (the first-page part6 container, or a part8 container co-locating two
        // pages' privates), and the page then references that container through
        // the private object — a reference the input entries do not record. This
        // is a no-op for documents whose containers hold only shared_hints
        // objects (the union is identical).
        if !self.all_referenced_pages.is_empty() {
            let mut container_members: BTreeMap<u32, Vec<ObjectRef>> = BTreeMap::new();
            for (&member, &(cnum, _)) in member_to_container {
                container_members.entry(cnum).or_default().push(member);
            }
            let pages_excluding_first = |refs: &mut dyn Iterator<Item = ObjectRef>| -> Vec<u32> {
                let mut pages: BTreeSet<u32> = BTreeSet::new();
                for r in refs {
                    if let Some(ps) = self.all_referenced_pages.get(&r) {
                        pages.extend(ps.iter().copied().filter(|&p| p != 0));
                    }
                }
                pages.into_iter().collect()
            };
            for entry in &mut out {
                entry.referencing_pages = if entry.object_ref.generation == u16::MAX {
                    let members = container_members
                        .get(&entry.object_ref.number)
                        .cloned()
                        .unwrap_or_default();
                    pages_excluding_first(&mut members.into_iter())
                } else {
                    pages_excluding_first(&mut std::iter::once(entry.object_ref))
                };
            }
        }

        out
    }

    /// Useful for callers that want to verify the disjoint invariant.
    /// Uses the three fine-grained Part-4 sub-partitions as the canonical
    /// source of truth.
    pub fn all_assigned_refs(&self) -> BTreeSet<ObjectRef> {
        self.part1_objects
            .iter()
            .chain(&self.part2_objects)
            .chain(&self.part3_objects)
            .chain(&self.part4_other_pages_private)
            .chain(&self.part4_other_pages_shared)
            .chain(&self.part4_rest)
            .copied()
            .collect()
    }

    /// The refs that [`RenumberMap::from_plan`] assigns a renumber slot
    /// (`by_original` key set).
    ///
    /// This is the exact union of the parts `from_plan` walks
    /// (`crates/flpdf/src/linearization/renumber.rs`): part2, part3, the two
    /// Part-4 page sets, part4_rest (which subsumes the promoted pages-tree /
    /// info / catalog refs), the two outline sets, and the open-document plain
    /// set. `part1_objects` is deliberately excluded — `from_plan` never maps
    /// it (and it is always empty today).
    ///
    /// A generated ObjStm may only batch members drawn from this set: an
    /// ObjStm member that lacks a renumber slot makes
    /// [`RenumberMap::place_objstm_members_per_half`] panic. The
    /// `renumber_assigned_refs_match_from_plan` test pins this set to
    /// `from_plan`'s `by_original` keys so the two cannot drift apart.
    pub(crate) fn renumber_assigned_refs(&self) -> BTreeSet<ObjectRef> {
        self.part2_objects
            .iter()
            .chain(&self.part3_objects)
            .chain(&self.part4_other_pages_private)
            .chain(&self.part4_other_pages_shared)
            .chain(&self.part4_rest)
            .chain(&self.part6_outline_objects)
            .chain(&self.part9_outline_objects)
            .chain(&self.part4_open_document_plain)
            .copied()
            .collect()
    }

    /// Return `true` if every object appears in **at most** one part.
    /// Uses the three fine-grained Part-4 sub-partitions as the canonical
    /// source of truth.
    pub fn parts_are_disjoint(&self) -> bool {
        let mut seen = BTreeSet::new();
        for r in self
            .part1_objects
            .iter()
            .chain(&self.part2_objects)
            .chain(&self.part3_objects)
            .chain(&self.part4_other_pages_private)
            .chain(&self.part4_other_pages_shared)
            .chain(&self.part6_outline_objects)
            .chain(&self.part9_outline_objects)
            .chain(&self.part4_rest)
            .chain(&self.part4_open_document_plain)
        {
            if !seen.insert(*r) {
                return false;
            }
        }
        true
    }
}

impl Default for LinearizationPlan {
    /// Construct a blank plan with no objects in any part.
    ///
    /// Useful in test fixtures via `LinearizationPlan { part2_objects: ...,
    /// ..Default::default() }` to avoid repeating empty-vec boilerplate for
    /// fields that are not under test.
    fn default() -> Self {
        Self {
            part1_objects: Vec::new(),
            part2_objects: Vec::new(),
            part3_objects: Vec::new(),
            part4_other_pages_private: Vec::new(),
            part4_other_pages_shared: Vec::new(),
            part4_rest: Vec::new(),
            part4_open_document_plain: Vec::new(),
            total_object_count: 0,
            root_ref: None,
            pages_tree_ref: None,
            info_ref: None,
            page_hints: Vec::new(),
            shared_hints: Vec::new(),
            per_page_private_objects: Vec::new(),
            all_referenced_pages: BTreeMap::new(),
            outline_first_page_members: Vec::new(),
            part9_outline_objects: Vec::new(),
            part6_outline_objects: Vec::new(),
            optimization: None,
            removed_refs: BTreeSet::new(),
            object_stream_mode: crate::writer::ObjectStreamMode::Disable,
            content_normalize_refs: BTreeSet::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// ObjStm batch planning
// ---------------------------------------------------------------------------

/// Part-tagged ObjStm batch plan produced by `LinearizationPlan::objstm_batches`.
///
/// Each inner `Vec<ObjectRef>` describes one ObjStm container; the contained
/// refs are still **original** (pre-renumber) object references.  Renumbering
/// and actual container-object allocation happen in downstream subtasks (5.8.2+).
///
/// # Part constraints
///
/// * `open_document_batches` — containers qpdf categorizes `in_open_document`
///   (qpdf part4: the open-document objects placed FIRST in the first half,
///   right after the Catalog and before the first-page section). A container
///   lands here when any member is reachable from the catalog's `/OpenAction`,
///   `/AcroForm`, `/ViewerPreferences`, `/PageMode`, `/Threads`, or the
///   trailer's `/Encrypt`.
/// * `part3_batches` — containers that belong in the first-page section
///   (ISO 32000-1 Annex F Part 3 = qpdf part6: shared/catalog objects).
/// * `part4_batches` — containers that belong after `/E` (Part 4 = qpdf
///   part7/8/9: remaining document objects from `part4_other_pages_private`,
///   `part4_other_pages_shared`, and `part4_rest`).
///
/// Every ObjStm container has one union-derived route. Its members may have
/// different classic per-object parts, but the container itself is emitted
/// wholly in that one qpdf-selected part. Page dictionaries and the Catalog
/// are never placed in any batch list.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ObjStmBatchPlan {
    /// ObjStm batches for qpdf part4 (open-document objects). Numbered and
    /// emitted in the first half, before the first-page section.
    pub(crate) open_document_batches: Vec<Vec<ObjectRef>>,
    /// ObjStm batches for Part 3 (shared/catalog) objects.
    pub(crate) part3_batches: Vec<Vec<ObjectRef>>,
    /// ObjStm batches for Part 4 (rest-of-document) objects.
    pub(crate) part4_batches: Vec<RoutedObjStmBatch>,
}

impl LinearizationPlan {
    /// Build a Part-tagged ObjStm packing plan from this `LinearizationPlan`.
    ///
    /// # Mode behaviour
    ///
    /// | Mode | Result |
    /// |------|--------|
    /// | `Disable` | Both batch lists are empty (no ObjStms emitted). |
    /// | `Generate` | Eligible Part-3 (first-page shared) objects are packed into `part3_batches`; eligible Part-4 objects are packed into `part4_batches`. The membership is qpdf-canonical by construction (a global even split with the page dictionaries and `/Catalog` erased), so no post-packing reshape is applied. |
    /// | `Preserve` | Each surviving source ObjStm is retained as one container and routed once from the union of its members' users. Page dictionaries, the `/Catalog`, unassigned objects, and ineligible members are removed; `/Pages` and `/Info` stay with their source container. The configured Generate batch cap is ignored because qpdf Preserve neither splits nor re-chunks source containers. If the source document contained no ObjStms, all batch lists are empty. |
    ///
    /// **Note:** the `/Pages` tree and `/Info` dictionary are not relocated.
    /// qpdf's `preserveObjectStreams` copies the source object->stream
    /// assignment and the linearized pass only erases the `/Page` dictionaries
    /// and the `/Catalog` (QPDFWriter.cc:1939, 2141-2161); the `/Pages` tree and
    /// `/Info` stay in their source container. Preserve mirrors that by routing
    /// them into their source container's first-half batch, so the resulting
    /// container membership matches qpdf.
    ///
    /// # Invariants
    ///
    /// * No page dictionary or Catalog ref appears in any batch.
    /// * Ineligible objects (streams, gen > 0, encryption dict, `/Type /ObjStm`,
    ///   `/Type /XRef`) are excluded via the
    ///   shared [`crate::writer::object_streams::is_eligible_for_objstm_handle`]
    ///   predicate.
    /// * Generate uses qpdf's fixed even split; Preserve retains source
    ///   container boundaries regardless of the configured planner cap.
    ///
    /// # Snapshot contract
    ///
    /// For Generate and Preserve, construct this plan with [`Self::from_pdf`]
    /// from the same `Pdf` passed here, and pass `true` to `from_pdf` exactly
    /// for Generate mode. Both modes classify their container-member unions
    /// from that one retained object-user snapshot. A hand-built plan without
    /// the snapshot is accepted only in Disable mode.
    pub(crate) fn objstm_batches<R: Read + Seek>(
        &self,
        pdf: &mut Pdf<R>,
        config: &PlannerConfig,
    ) -> crate::Result<ObjStmBatchPlan> {
        if config.mode == ObjectStreamMode::Disable {
            return Ok(ObjStmBatchPlan::default());
        }

        let optimization = self.optimization.as_ref().ok_or_else(|| {
            crate::Error::Unsupported(
                "linearization plan: missing optimization snapshot".to_string(),
            )
        })?;

        let ctx = eligibility_context(pdf)?;
        let length_exclusions =
            if config.mode == ObjectStreamMode::Preserve && config.preserve_unreferenced_objects {
                BTreeSet::new()
            } else {
                compressible_objgens_qpdf_plan(pdf)?.indirect_objstm_length_refs
            };

        let plan = match config.mode {
            ObjectStreamMode::Disable => unreachable!(),
            ObjectStreamMode::Generate => {
                // The Generate path reproduces qpdf's linearized
                // `generateObjectStreams` directly: a GLOBAL even split over the
                // compressible set, with the page dictionaries + root Catalog
                // erased and `/Info` / the `/Pages` tree kept as ordinary
                // members. That membership is already qpdf-canonical, so no
                // post-packing reshape is applied.
                self.objstm_batches_generate(pdf, config, &ctx, &length_exclusions, optimization)?
            }
            ObjectStreamMode::Preserve => {
                self.objstm_batches_preserve(pdf, config, &ctx, &length_exclusions, optimization)?
            }
        };

        Ok(plan)
    }

    /// Generate mode: reproduce qpdf's linearized `generateObjectStreams`.
    ///
    /// A GLOBAL even split over the compressible set
    /// ([`objstm_membership_linearized_with_eligibility`]), with the page dictionaries + root
    /// Catalog erased, then each container routed to a linearization part by the
    /// union of its members' page users ([`route_objstm_containers`]). Containers
    /// routed to part 6 ([`ContainerPart::FirstPagePrivate`],
    /// [`ContainerPart::FirstPageShared`], or
    /// [`ContainerPart::FirstPageOutlines`]) become first-half
    /// (`part3_batches`); every other container becomes second-half
    /// (`part4_batches`). Within a container, members are ordered by ascending
    /// source object number (qpdf's `object_stream_to_objects` is a
    /// `std::set<QPDFObjGen>`).
    ///
    /// This replaces flpdf's earlier per-part greedy chunking, which diverged
    /// from qpdf at `>cap` (see `docs/plans/2026-06-17-objstm-generate-linearized-phase2.md`).
    ///
    /// The `config` / `ctx` / `length_exclusions` arguments are unused: the
    /// compressible-set traversal applies qpdf's own eligibility and a fixed
    /// 100-per-stream split (not the planner cap).
    fn objstm_batches_generate<R: Read + Seek>(
        &self,
        pdf: &mut Pdf<R>,
        _config: &PlannerConfig,
        _ctx: &crate::writer::object_streams::EligibilityContext,
        _length_exclusions: &BTreeSet<ObjectRef>,
        optimization: &crate::optimization::Optimization,
    ) -> crate::Result<ObjStmBatchPlan> {
        // `objstm_membership_linearized` filters its containers to the plan's
        // renumber-assigned set BEFORE the even split, so a trailer-only ref with
        // no slot neither inflates the split boundary nor reaches
        // `place_objstm_members_per_half` without a slot (which would panic).
        // Dangling/missing refs (e.g. `/Info 99 0 R` with no xref entry) are
        // already excluded upstream by `get_compressible_objgens`. For a well-formed
        // PDF every member is assigned, so the filter is a no-op and the batches
        // stay byte-identical.
        let assigned = self.renumber_assigned_refs();
        let containers = objstm_membership_linearized_with_eligibility(
            pdf,
            &assigned,
            optimization.generate_objstm_eligible(),
        )?; // cov:ignore: closing line of a multi-line call; llvm-cov misattributes the hit count to the previous line, not an untested branch
        let routes = route_objstm_containers(
            optimization,
            !self.outline_first_page_members.is_empty(),
            &containers,
        );
        let mut open_document_batches: Vec<Vec<ObjectRef>> = Vec::new();
        // qpdf part 6 is private, shared, then outline containers. Preserve
        // first-encounter order within each bucket.
        let mut part3_private: Vec<Vec<ObjectRef>> = Vec::new();
        let mut part3_shared: Vec<Vec<ObjectRef>> = Vec::new();
        let mut part3_outlines: Vec<Vec<ObjectRef>> = Vec::new();
        // Second-half containers, grouped by part so they can be emitted in qpdf's
        // strict part order (part7, then part8, then part9 — QPDF_linearization.cc:1342).
        // qpdf's file layout writes lc_other_page_private, lc_other_page_shared, then
        // lc_other/lc_outlines; the even-split (DFS) order a container arrives in is
        // NOT that order (a DFS-early /Outlines container routes to part9 yet precedes
        // a part8 shared-font container in the split). Bucketing into three vectors and
        // concatenating them (like the part3 regular/outlines split below) reorders only
        // ACROSS parts, leaving within-part even-split arrival order intact.
        //
        // For part8 that within-part order is provably qpdf's: lc_other_page_shared
        // is a std::set keyed on container objgen, and a generate-mode container's
        // objgen comes from makeIndirectObject in even-split order — so set order ==
        // even-split order. part7 (page order) and part9 (pages-tree / outlines /
        // lc_other sub-order) only have one container each in the fixtures seen so
        // far, so their within-part multi-container order has not been exercised;
        // if such a case arises, a finer per-part sort may be needed.
        let mut part4_private: Vec<RoutedObjStmBatch> = Vec::new();
        let mut part4_shared: Vec<RoutedObjStmBatch> = Vec::new();
        let mut part4_rest: Vec<RoutedObjStmBatch> = Vec::new();
        for (mut members, route) in containers.into_iter().zip(routes) {
            members.sort_unstable_by_key(|r| r.number);
            push_routed_objstm_batch(
                members,
                route,
                None,
                &mut open_document_batches,
                &mut part3_private,
                &mut part3_shared,
                &mut part3_outlines,
                &mut part4_private,
                &mut part4_shared,
                &mut part4_rest,
            );
        }
        // Concatenate the buckets in part order (part7, part8, part9).
        let mut part4_batches = part4_private;
        part4_batches.extend(part4_shared);
        part4_batches.extend(part4_rest);

        // qpdf part 6 order: private, shared, then outlines.
        let mut part3_batches = part3_private;
        part3_batches.extend(part3_shared);
        part3_batches.extend(part3_outlines);

        Ok(ObjStmBatchPlan {
            open_document_batches,
            part3_batches,
            part4_batches,
        })
    }

    /// Preserve mode: retain each surviving source ObjStm and route its member-user union.
    fn objstm_batches_preserve<R: Read + Seek>(
        &self,
        pdf: &mut Pdf<R>,
        _config: &PlannerConfig,
        ctx: &crate::writer::object_streams::EligibilityContext,
        length_exclusions: &BTreeSet<ObjectRef>,
        optimization: &crate::optimization::Optimization,
    ) -> crate::Result<ObjStmBatchPlan> {
        use crate::XrefEntry;

        let entries = pdf.source_xref_entries();

        // Build source ObjStm groups: container_number → [(index, ref)]
        let mut groups: BTreeMap<u32, Vec<(u32, ObjectRef)>> = BTreeMap::new();
        for (obj_ref, offset) in &entries {
            if let XrefEntry::Compressed { stream, index } = offset {
                groups.entry(*stream).or_default().push((*index, *obj_ref));
            }
        }

        let assigned = self.renumber_assigned_refs();
        let page_dicts: BTreeSet<ObjectRef> = crate::pages::page_refs(pdf)?.into_iter().collect();
        let catalog = self.root_ref;
        let mut containers: Vec<Vec<ObjectRef>> = Vec::new();
        let mut source_container_numbers: Vec<u32> = Vec::new();

        // Iterate containers in ascending source-container number and retain
        // their member-index order. qpdf erases page dictionaries and the
        // Catalog from the source mapping, but otherwise classifies the
        // surviving container as a unit.
        for (container_num, mut members) in groups {
            members.sort_by_key(|(idx, _)| *idx);
            let mut surviving = Vec::new();
            for (_idx, obj_ref) in members {
                if page_dicts.contains(&obj_ref) || Some(obj_ref) == catalog {
                    continue;
                }
                if length_exclusions.contains(&obj_ref) || !assigned.contains(&obj_ref) {
                    continue;
                }
                let eligible = {
                    let obj = pdf.get_object_handle(obj_ref);
                    is_eligible_for_objstm_handle(obj_ref, &obj, ctx)?
                };
                let obj = pdf.get_object_handle(obj_ref);
                if eligible && !crate::writer::object_streams::is_qpdf_signature_dict(pdf, &obj)? {
                    surviving.push(obj_ref);
                }
            }
            if !surviving.is_empty() {
                source_container_numbers.push(container_num);
                containers.push(surviving);
            }
        }

        let routes = route_objstm_containers(
            optimization,
            !self.outline_first_page_members.is_empty(),
            &containers,
        );
        let mut open_document_batches = Vec::new();
        let mut part3_private = Vec::new();
        let mut part3_shared = Vec::new();
        let mut part3_outlines = Vec::new();
        let mut part4_private = Vec::new();
        let mut part4_shared = Vec::new();
        let mut part4_rest = Vec::new();

        for ((members, route), source_container_number) in containers
            .into_iter()
            .zip(routes)
            .zip(source_container_numbers)
        {
            push_routed_objstm_batch(
                members,
                route,
                Some(source_container_number),
                &mut open_document_batches,
                &mut part3_private,
                &mut part3_shared,
                &mut part3_outlines,
                &mut part4_private,
                &mut part4_shared,
                &mut part4_rest,
            );
        }

        let mut part3_batches = part3_private;
        part3_batches.extend(part3_shared);
        part3_batches.extend(part3_outlines);
        let mut part4_batches = part4_private;
        part4_batches.extend(part4_shared);
        part4_batches.extend(part4_rest);

        Ok(ObjStmBatchPlan {
            open_document_batches,
            part3_batches,
            part4_batches,
        })
    }
}

// ---------------------------------------------------------------------------
// Linearized generate-mode ObjStm membership + container part routing
//
// These mirror qpdf 11.9.0's linearized `--object-streams=generate` pipeline:
//   * `objstm_membership_linearized` = `generateObjectStreams` (global even
//     split over `getCompressibleObjGens`) then the linearized erasure of every
//     page dictionary and the root Catalog (QPDFWriter.cc:2141-2161).
//   * `route_objstm_containers` = `filterCompressedObjects`
//     (QPDF_optimization.cc:340-380) folding each member's obj_users onto its
//     container, then `calculateLinearizationData`'s `lc_*` categorization
//     (QPDF_linearization.cc:963-1200) applied to the container's union.
// ---------------------------------------------------------------------------

/// Linearization part a generate-mode ObjStm container is routed to, by the
/// union of its members' object users.
///
/// `OpenDocument` is qpdf part 4 (open-document objects, first half), the three
/// `FirstPage*` variants are part 6 (first-page section),
/// `OtherPagePrivate` is part 7, `OtherPageShared` is part 8, and `Rest` is part
/// 9. qpdf checks outlines, open-document objects, and first-page objects in
/// that precedence order; [`route_objstm_containers`] retains it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContainerPart {
    /// qpdf part 4 — the container holds at least one open-document object
    /// (reachable from the catalog's `/OpenAction`, `/AcroForm`,
    /// `/ViewerPreferences`, `/PageMode`, `/Threads`, or the trailer's
    /// `/Encrypt`). Takes precedence over every page category.
    OpenDocument,
    /// qpdf part 6 — every container member user is compatible with
    /// `lc_first_page_private`.
    FirstPagePrivate,
    /// qpdf part 6 — the container reaches the first page and also has a
    /// document-other, non-first-page, or thumbnail user.
    FirstPageShared,
    /// qpdf part 6 — an outline container when `/PageMode /UseOutlines`.
    FirstPageOutlines,
    /// qpdf part 7 — the container's members are private to exactly one
    /// non-first page.
    OtherPagePrivate,
    /// qpdf part 8 — the container's members are shared by two or more
    /// non-first pages.
    OtherPageShared,
    /// qpdf part 9 — the container reaches no page (trailer-only members).
    Rest,
}

/// One second-half ObjStm batch paired with qpdf's canonical container route.
///
/// Keeping the route beside the members prevents the writer from reclassifying
/// the container from per-object classic partitions after qpdf's member-user
/// union has already selected its part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RoutedObjStmBatch {
    pub(crate) members: Vec<ObjectRef>,
    pub(crate) route: ContainerPart,
    /// Original ObjStm object number in Preserve mode. Generate mode creates a
    /// fresh container after the source objects, so it uses `None`.
    pub(crate) source_container_number: Option<u32>,
}

#[allow(clippy::too_many_arguments)]
fn push_routed_objstm_batch(
    members: Vec<ObjectRef>,
    route: ContainerPart,
    source_container_number: Option<u32>,
    open_document_batches: &mut Vec<Vec<ObjectRef>>,
    part3_private: &mut Vec<Vec<ObjectRef>>,
    part3_shared: &mut Vec<Vec<ObjectRef>>,
    part3_outlines: &mut Vec<Vec<ObjectRef>>,
    part4_private: &mut Vec<RoutedObjStmBatch>,
    part4_shared: &mut Vec<RoutedObjStmBatch>,
    part4_rest: &mut Vec<RoutedObjStmBatch>,
) {
    match route {
        ContainerPart::OpenDocument => open_document_batches.push(members),
        ContainerPart::FirstPagePrivate => part3_private.push(members),
        ContainerPart::FirstPageShared => part3_shared.push(members),
        ContainerPart::FirstPageOutlines => part3_outlines.push(members),
        ContainerPart::OtherPagePrivate => part4_private.push(RoutedObjStmBatch {
            members,
            route,
            source_container_number,
        }),
        ContainerPart::OtherPageShared => part4_shared.push(RoutedObjStmBatch {
            members,
            route,
            source_container_number,
        }),
        ContainerPart::Rest => part4_rest.push(RoutedObjStmBatch {
            members,
            route,
            source_container_number,
        }),
    }
}

impl std::ops::Deref for RoutedObjStmBatch {
    type Target = [ObjectRef];

    fn deref(&self) -> &Self::Target {
        &self.members
    }
}

/// Compute the linearized generate-mode ObjStm membership.
///
/// Runs qpdf's `generateObjectStreams` even split
/// ([`get_compressible_objgens`](crate::writer::object_streams::get_compressible_objgens)
/// →
/// [`even_split_into_streams`](crate::writer::object_streams::even_split_into_streams),
/// hard-coded 100 per stream — *not* the planner cap) over the whole document,
/// then erases every page dictionary and the root Catalog from the resulting
/// containers (qpdf's linearized exclusion at QPDFWriter.cc:2141-2161; the
/// `/Pages` tree node and `/Info` dictionary are *not* erased — they stay ObjStm
/// members). Containers are returned in even-split order; each inner vector is
/// one container's surviving members in even-split (DFS) order. A container left
/// empty by the erasure is dropped.
///
/// `assigned` is the set of refs that receive a renumber slot
/// ([`LinearizationPlan::renumber_assigned_refs`]). A live, reachable object that
/// [`get_compressible_objgens`](crate::writer::object_streams::get_compressible_objgens)
/// admits but the linearization plan places in no part — a trailer-only object
/// with no slot — is dropped **before** the even split, so it cannot inflate the
/// split boundary and scatter real members across separate ObjStms. (Dangling /
/// missing refs are already excluded upstream by `get_compressible_objgens`, which
/// qpdf treats as null, so they never reach this retain.) The page dictionaries
/// and root Catalog are in `assigned`, so they still consume split positions and
/// are erased afterwards, exactly as qpdf does.
///
/// # Errors
///
/// Propagates reader errors from the compressible-set traversal or the page-tree
/// walk used to build the erase set.
pub(crate) fn objstm_membership_linearized_with_eligibility<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    assigned: &BTreeSet<ObjectRef>,
    eligibility_override: Option<&[ObjectRef]>,
) -> crate::Result<Vec<Vec<ObjectRef>>> {
    // qpdf's DFS already preserves the indirect identity of null-resolving
    // references reached from arrays. Do not append `resurrectable_null_refs`
    // here: that set remains a planner/all-refs aid, and appending it would give
    // array-null objects duplicate ObjStm membership.
    let mut eligible = match eligibility_override {
        Some(eligible) => eligible.to_vec(),
        None => crate::writer::object_streams::get_compressible_objgens(pdf)?,
    };
    // Drop refs without a renumber slot before the split (see doc above).
    eligible.retain(|r| assigned.contains(r));
    let streams = crate::writer::object_streams::even_split_into_streams(&eligible);

    // Erase set: every page dictionary plus the root Catalog. qpdf cannot place
    // a page dict in an ObjStm (the linearization layout addresses pages by
    // file offset) and never compresses the Catalog in a linearized file.
    let mut erase: BTreeSet<ObjectRef> = crate::pages::page_refs(pdf)?.into_iter().collect();
    if let Some(root) = pdf.root_ref() {
        erase.insert(root);
    }

    Ok(streams
        .into_iter()
        .map(|stream| {
            stream
                .into_iter()
                .filter(|r| !erase.contains(r))
                .collect::<Vec<ObjectRef>>()
        })
        .filter(|container| !container.is_empty())
        .collect())
}

/// Catalog keys qpdf treats as `open_document_keys` in
/// `calculateLinearizationData` (QPDF_linearization.cc:1045-1050): a catalog
/// reference through one of these is `in_open_document`, while a reference
/// through any OTHER catalog key (except `/Outlines`) increments `others`.
/// Used to classify the retained optimization users into open-document and
/// document-other categories from one source of truth.
const OPEN_DOCUMENT_CATALOG_KEYS: [&[u8]; 5] = [
    b"ViewerPreferences",
    b"PageMode",
    b"Threads",
    b"OpenAction",
    b"AcroForm",
];

fn is_open_document_user(user: &crate::optimization::ObjectUser) -> bool {
    match user {
        crate::optimization::ObjectUser::TrailerKey(key) => key == b"Encrypt",
        crate::optimization::ObjectUser::RootKey(key) => {
            OPEN_DOCUMENT_CATALOG_KEYS.contains(&key.as_slice())
        }
        _ => false,
    }
}

fn is_document_other_user(user: &crate::optimization::ObjectUser) -> bool {
    match user {
        crate::optimization::ObjectUser::TrailerKey(key) => key != b"Encrypt",
        crate::optimization::ObjectUser::RootKey(key) => {
            key != b"Outlines" && !OPEN_DOCUMENT_CATALOG_KEYS.contains(&key.as_slice())
        }
        _ => false,
    }
}

/// Returns `true` when the catalog specifies `/PageMode /UseOutlines` AND has
/// an `/Outlines` entry (QPDF_linearization.cc:1031-1043).
///
/// When `true`, outline objects are routed to the first-page section (part6)
/// rather than part9 by [`route_objstm_containers`].
fn outlines_in_first_page_predicate<R: Read + Seek>(pdf: &mut Pdf<R>) -> crate::Result<bool> {
    let Some(root) = pdf.root_ref() else {
        return Ok(false); // cov:ignore: root_ref None ⇒ from_pdf fails earlier via catalog()?
    };
    let root_handle = pdf.get_object_handle(root);
    pdf.resolve(&root_handle)?;
    if !root_handle.try_has_key(b"/Outlines")? {
        return Ok(false);
    }
    Ok(root_handle
        .try_get_key(b"/PageMode")?
        .try_as_name()?
        .is_some_and(|name| name == b"UseOutlines"))
}

/// Route each ObjStm container to a linearization part by the union of its
/// members' object users.
///
/// Mirrors qpdf's `filterCompressedObjects` (the container inherits the union of
/// every member's obj_users) followed by the `lc_*` categorization. In qpdf's
/// precedence order: a container holding any outline object is part 6
/// ([`ContainerPart::FirstPageOutlines`]) when `/PageMode /UseOutlines` is set,
/// or part 9 ([`ContainerPart::Rest`]) otherwise; a container holding any
/// open-document object is part 4 ([`ContainerPart::OpenDocument`]).
/// A remaining container holding a first-page object is private only when its
/// union has no non-first-page, document-other, or thumbnail user; otherwise it
/// is shared. Containers without a first-page object route to part 7 / part 8 /
/// part 9 by the number of *distinct non-first* pages their members reach (one →
/// [`ContainerPart::OtherPagePrivate`], two or more →
/// [`ContainerPart::OtherPageShared`], none → [`ContainerPart::Rest`]). The
/// one-page case is part 7 ONLY when the member union has neither a
/// document-level `others` object nor a thumbnail user
/// (QPDF_linearization.cc:1128 gates lc_other_page_private on `others==0` and
/// `thumbs==0`); either signal demotes it to part 9
/// ([`ContainerPart::Rest`]). The two-or-more case is part 8 regardless of
/// `others` or thumbnails (QPDF_linearization.cc:1130).
///
/// The retained routing snapshot comes from [`LinearizationPlan::from_pdf`].
/// This classifier does not resolve objects or read the PDF.
///
/// # Deviation
///
/// **Multiple open-document containers (verified, flpdf-699x):** qpdf assigns
/// container `ObjGen`s sequentially in even-split order, so its
/// `std::set<QPDFObjGen>` (used for `lc_open_document`) iterates them in the
/// same DFS / even-split order that this function preserves.  The ordering is
/// therefore byte-identical to qpdf for ≥2 open-document containers; verified
/// with `objstm-lin-openaction-multi-od` (two OD containers whose min-member
/// numbers are non-ascending in DFS order).
///
pub(crate) fn route_objstm_containers(
    optimization: &crate::optimization::Optimization,
    outlines_in_first_page: bool,
    containers: &[Vec<ObjectRef>],
) -> Vec<ContainerPart> {
    containers
        .iter()
        .map(|members| {
            let users = optimization.users_for_members(members.iter());
            classify_container_users(&users, outlines_in_first_page)
        })
        .collect()
}

fn classify_container_users(
    users: &BTreeSet<crate::optimization::ObjectUser>,
    outlines_in_first_page: bool,
) -> ContainerPart {
    if users.iter().any(is_outline_user) {
        return if outlines_in_first_page {
            ContainerPart::FirstPageOutlines
        } else {
            ContainerPart::Rest
        };
    }
    if users.iter().any(is_open_document_user) {
        return ContainerPart::OpenDocument;
    }

    let page_numbers: BTreeSet<u32> = users
        .iter()
        .filter_map(|user| match user {
            crate::optimization::ObjectUser::Page(page_number) => Some(*page_number),
            _ => None,
        })
        .collect();
    let has_document_other = users.iter().any(is_document_other_user);
    let has_thumbnail = users.iter().any(is_thumbnail_user);
    if page_numbers.contains(&0) {
        return if page_numbers.iter().any(|&page| page != 0) || has_document_other || has_thumbnail
        {
            ContainerPart::FirstPageShared
        } else {
            ContainerPart::FirstPagePrivate
        };
    }

    match page_numbers.len() {
        0 => ContainerPart::Rest,
        1 if has_document_other || has_thumbnail => ContainerPart::Rest,
        1 => ContainerPart::OtherPagePrivate,
        _ => ContainerPart::OtherPageShared,
    }
}

fn is_outline_user(user: &crate::optimization::ObjectUser) -> bool {
    matches!(
        user,
        crate::optimization::ObjectUser::RootKey(key) if key == b"Outlines"
    )
}

fn is_thumbnail_user(user: &crate::optimization::ObjectUser) -> bool {
    matches!(user, crate::optimization::ObjectUser::Thumbnail(_))
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{
        collect_direct_handle_refs, collect_direct_handle_refs_with_context,
        collect_direct_handle_refs_with_stream_parameters_context, LinearizationPlan,
    };
    use crate::acroform_document_helper::AcroFormDocumentHelper;
    use crate::object_handle::ObjectHandle;
    use crate::parser::MAX_PARSE_DEPTH;
    use crate::writer::{ObjectStreamMode, WriterOptions};
    use crate::Pdf;
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::cell::Cell;
    use std::collections::BTreeSet;
    use std::io::{Cursor, Write};
    use std::rc::Rc;

    fn flate(data: &[u8]) -> Vec<u8> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).expect("compress fixture stream");
        encoder.finish().expect("finish fixture stream")
    }

    fn stream_object(number: u32, dictionary: &str, data: &[u8]) -> Vec<u8> {
        let mut object = format!(
            "{number} 0 obj\n<< {dictionary} /Length {} >>\nstream\n",
            data.len()
        )
        .into_bytes();
        object.extend_from_slice(data);
        object.extend_from_slice(b"\nendstream\nendobj\n");
        object
    }

    fn parameter_probe_fixture() -> Vec<u8> {
        let page_content_indirect_filter = flate(b"q Q\n");
        let page_content_direct_filter = flate(b"q Q\n");
        let existing_appearance = flate(b"/Tx BMC\nEMC\n");
        // The inline `/Resources` and ancestor `/ColorSpace` arrays exercise
        // the generic array-edge context used by the qpdf-shaped closure walk.
        let objects = vec![
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /AcroForm 6 0 R >>\nendobj\n"
                .to_vec(),
            b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] /ColorSpace [10 0 R] >>\nendobj\n".to_vec(),
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /ProcSet [10 0 R] >> /Contents [4 0 R 12 0 R] /Annots [5 0 R] >>\nendobj\n".to_vec(),
            stream_object(4, "/Filter 9 0 R", &page_content_indirect_filter),
            b"5 0 obj\n<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /V (Hello) /DA (/Helv 12 Tf 0 g) /Rect [10 10 200 30] /P 3 0 R /AP << /N 8 0 R >> >>\nendobj\n".to_vec(),
            b"6 0 obj\n<< /Fields [5 0 R] /NeedAppearances true /DR << >> /DA (/Helv 12 Tf 0 g) >>\nendobj\n".to_vec(),
            b"7 0 obj\nnull\nendobj\n".to_vec(),
            stream_object(
                8,
                "/Type /XObject /Subtype /Form /BBox [0 0 190 20] /Resources << >> /Filter 9 0 R",
                &existing_appearance,
            ),
            b"9 0 obj\n/FlateDecode\nendobj\n".to_vec(),
            b"10 0 obj\n<< /Type /ExtGState >>\nendobj\n".to_vec(),
            b"11 0 obj\nnull\nendobj\n".to_vec(),
            stream_object(12, "/Filter /FlateDecode", &page_content_direct_filter),
        ];
        let mut pdf = b"%PDF-1.7\n".to_vec();
        let object_count = objects.len();
        let mut offsets = Vec::with_capacity(object_count);
        for object in objects {
            offsets.push(pdf.len());
            pdf.extend_from_slice(&object);
        }
        let xref = pdf.len();
        let _ = writeln!(&mut pdf, "xref\n0 {}", object_count + 1);
        pdf.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets {
            let _ = writeln!(&mut pdf, "{offset:010} 00000 n ");
        }
        let _ = writeln!(
            &mut pdf,
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF",
            object_count + 1
        );
        pdf
    }

    struct PassThroughTokenFilter;

    impl crate::token_filter::TokenFilter for PassThroughTokenFilter {
        fn handle_token(
            &mut self,
            token: &crate::tokenizer::Token,
            output: &mut crate::token_filter::TokenFilterOutput<'_>,
        ) -> crate::pipeline::PipelineResult<()> {
            output.write_token(token)
        }
    }

    #[test]
    fn qpdf_linearization_probe_consumes_modified_and_parameterized_streams() {
        let mut pdf = Pdf::open(Cursor::new(parameter_probe_fixture())).expect("parse fixture");
        AcroFormDocumentHelper::new(&mut pdf)
            .expect("AcroForm")
            .generate_appearances_if_needed()
            .expect("generate appearance");

        let options = WriterOptions {
            object_streams: ObjectStreamMode::Disable,
            ..WriterOptions::default()
        };
        let plan = LinearizationPlan::from_pdf_with_writer_options(&mut pdf, &options)
            .expect("build linearization plan");
        assert_eq!(plan.page_hints.len(), 1);
        assert!(!plan.part2_objects.is_empty());
        assert!(
            plan.all_assigned_refs()
                .contains(&crate::ObjectRef::new(10, 0)),
            "inline resource and ancestor array references must reach the plan"
        );
    }

    #[test]
    fn qpdf_linearization_parameter_probe_runs_once_before_raw_retry() {
        let mut pdf = Pdf::open(Cursor::new(parameter_probe_fixture())).expect("parse fixture");
        let stream = pdf.get_object_handle(crate::ObjectRef::new(4, 0));
        pdf.resolve(&stream).expect("resolve page content");

        let token = crate::tokenizer::Token::new(crate::tokenizer::TokenType::Word, b"q".to_vec());
        let mut filter = PassThroughTokenFilter;
        let mut output = crate::token_filter::TokenFilterOutput::new(None);
        crate::token_filter::TokenFilter::handle_token(&mut filter, &token, &mut output)
            .expect("pass-through filter token");

        let provider_calls = Rc::new(Cell::new(0));
        let provider_calls_for_callback = Rc::clone(&provider_calls);
        stream
            .replace_stream_data_with_retry_callback(
                move |_pipeline, _suppress_warnings, _will_retry| {
                    provider_calls_for_callback.set(provider_calls_for_callback.get() + 1);
                    Ok(false)
                },
                None,
                None,
            )
            .expect("install stateful stream provider");
        stream
            .add_token_filter(Rc::new(std::cell::RefCell::new(PassThroughTokenFilter)))
            .expect("register token filter");
        pdf.mark_object_handle_dirty(&stream)
            .expect("mark modified content");

        let options = WriterOptions {
            object_streams: ObjectStreamMode::Disable,
            ..WriterOptions::default()
        };
        LinearizationPlan::from_pdf_with_writer_options(&mut pdf, &options)
            .expect("build linearization plan");
        assert_eq!(
            provider_calls.get(),
            2,
            "one qpdf parameter probe may retry once with raw data, but the optimization callback must not probe the same stream again"
        );
    }

    #[test]
    fn direct_reference_walkers_reject_depth_beyond_parser_limit() {
        let handle = ObjectHandle::null();
        let mut refs = Vec::new();
        let error = collect_direct_handle_refs(&handle, MAX_PARSE_DEPTH + 1, &mut refs)
            .expect_err("the direct reference walk has a parser-depth guard");
        assert!(error.to_string().contains("maximum of 500"));

        let mut contextual = Vec::new();
        let error = collect_direct_handle_refs_with_context(
            &handle,
            MAX_PARSE_DEPTH + 1,
            false,
            &mut contextual,
        )
        .expect_err("the contextual reference walk has a parser-depth guard");
        assert!(error.to_string().contains("maximum of 500"));

        let mut stream_contextual = Vec::new();
        let error = collect_direct_handle_refs_with_stream_parameters_context(
            &handle,
            MAX_PARSE_DEPTH + 1,
            false,
            &mut stream_contextual,
            &BTreeSet::new(),
        )
        .expect_err("the stream-policy reference walk has a parser-depth guard");
        assert!(error.to_string().contains("maximum of 500"));
    }
}
