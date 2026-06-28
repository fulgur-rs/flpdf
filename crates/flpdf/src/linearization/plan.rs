//! `LinearizationPlan` — pure data model for PDF linearization layout.
//!
//! A `LinearizationPlan` partitions all objects in a document into the four
//! body parts defined by ISO 32000-1 Annex F, and carries the raw inputs needed
//! to build the Page-offset hint table and the Shared-object hint table.
//!
//! The plan is intentionally a dumb data struct: no I/O, no serialization.
//! Higher-level subtasks (e.g. the hint-table byte-builder and the linearized
//! writer) consume this struct and fill in the placeholders.
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
//! # Object closure algorithm (subtask 2.2)
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
use crate::object::MAX_INLINE_DEPTH;
use crate::writer::object_streams::{
    collect_indirect_objstm_length_refs, eligibility_context, is_eligible_for_objstm,
    ObjectStreamMode, PlannerConfig,
};
use crate::{Object, ObjectRef, Pdf, Result};
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

/// Collect all `ObjectRef` values directly referenced by `obj`.
///
/// Walks arrays, dictionaries, and stream dictionaries (but NOT stream data
/// bytes). A `Reference(r)` is pushed to `out` as-is.  The caller is
/// responsible for cycle detection and transitive expansion.
fn collect_direct_refs(obj: &Object, depth: usize, out: &mut Vec<ObjectRef>) -> Result<()> {
    if depth > MAX_INLINE_DEPTH {
        return Err(crate::Error::Unsupported(format!(
            "linearization plan: inline object nesting exceeds maximum of {MAX_INLINE_DEPTH}"
        )));
    }
    match obj {
        Object::Reference(r) => out.push(*r),
        Object::Array(arr) => {
            for elem in arr {
                collect_direct_refs(elem, depth + 1, out)?;
            }
        }
        Object::Dictionary(dict) => {
            for (_k, v) in dict.iter() {
                collect_direct_refs(v, depth + 1, out)?;
            }
        }
        Object::Stream(s) => {
            // Only walk the stream dictionary; do not scan raw data bytes.
            // Skip /Length: it only ever points to an integer holder, and qpdf
            // directizes every emitted stream's /Length before computing the
            // linearization object-user map, so a /Length holder is never
            // page-reachable. Following it would pull a holder kept alive by a
            // separate edge (e.g. a catalog reference) into the first-page
            // closure, misrouting its ObjStm container to the first half. Other
            // indirect stream-dict values (/ColorSpace, /SMask, /Mask,
            // /Alternates, ...) remain genuinely reachable and are followed.
            for (k, v) in s.dict.iter() {
                if k == b"Length" {
                    continue;
                }
                collect_direct_refs(v, depth + 1, out)?;
            }
        }
        // Scalar types cannot contain refs.
        _ => {}
    }
    Ok(())
}

/// Like [`collect_direct_refs`] but tracks whether each ref was discovered
/// inside an array element (`true`) or a dictionary value (`false`).
///
/// Array-element missing-xref refs survive as `null` in the writer output;
/// dictionary-value missing-xref refs cause the key to be dropped entirely.
/// `compute_closure` uses this to restrict resurrectable-null admission to
/// refs that were actually reached via a surviving array edge rather than
/// a dropped dict-value edge.
fn collect_direct_refs_with_context(
    obj: &Object,
    depth: usize,
    in_array: bool,
    out: &mut Vec<(ObjectRef, bool)>,
) -> Result<()> {
    if depth > MAX_INLINE_DEPTH {
        return Err(crate::Error::Unsupported(format!(
            "linearization plan: inline object nesting exceeds maximum of {MAX_INLINE_DEPTH}"
        )));
    }
    match obj {
        Object::Reference(r) => out.push((*r, in_array)),
        Object::Array(arr) => {
            for elem in arr {
                collect_direct_refs_with_context(elem, depth + 1, true, out)?;
            }
        }
        Object::Dictionary(dict) => {
            for (_k, v) in dict.iter() {
                collect_direct_refs_with_context(v, depth + 1, false, out)?;
            }
        }
        Object::Stream(s) => {
            for (k, v) in s.dict.iter() {
                if k == b"Length" {
                    continue;
                }
                collect_direct_refs_with_context(v, depth + 1, false, out)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Returns whether a resolved object is a page-tree node we must not descend
/// into during a subtree expansion (a `/Type /Page` leaf or `/Type /Pages`
/// interior node). Used to stop a `/Resources` subtree walk from pulling in
/// sibling pages if a resource value cross-links back into the page tree.
fn is_page_tree_node(obj: &Object) -> bool {
    matches!(obj, Object::Dictionary(d)
        if matches!(d.get("Type"), Some(Object::Name(n))
            if n.as_slice() == b"Pages" || n.as_slice() == b"Page"))
}

/// Compute the transitive closure of objects reachable from `root`.
///
/// Returns the list in discovery order (root first). The walk is breadth-first
/// over the object graph in general, with one exception for page leaves: a
/// page's `/Resources` subtree is expanded depth-first and placed ahead of its
/// `/Contents`. This reproduces qpdf's physical ordering for the first-page
/// section, where the Resources dictionary (and the fonts/XObjects it points
/// at) precede the content stream.
///
/// ### `/Parent` / `/Kids` handling
///
/// When the walker enters a node whose `/Type` is `Pages` (an intermediate
/// page-tree node), it follows all dictionary entries **except `/Kids`**.
/// This means page-tree interior nodes (and their inherited `/Resources`) are
/// included in the closure, but the *sibling pages* hanging off `/Kids` are
/// not pulled in. The `/Parent` chain is therefore followed at most until the
/// root Pages node without capturing other pages.
fn compute_closure<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    root: ObjectRef,
    live: &BTreeSet<ObjectRef>,
    resurrectable: &BTreeSet<ObjectRef>,
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

    // A reference to object 0 (free-list head / null singleton, ISO 32000-1
    // §7.3.10) or to a missing-xref object resolves to null. qpdf admits no body
    // object for such a reference (the holding dict key is dropped / the array
    // element is inlined as `null` at emission), so it must never receive an
    // object number here. Keeping it in `visited` but out of `order` mirrors
    // that: a numbered stray null object would diverge from qpdf and inflate the
    // first-page object count. `live` is computed once by the caller — the
    // per-page closure loop would otherwise re-scan the whole xref table O(pages).
    //
    // Exception: missing-xref refs reached via a surviving array edge in this
    // page's object graph are resurrectable (see `resurrectable_null_refs`). qpdf
    // classifies them as first-page users when reached from a first-page object,
    // giving them HIGH object numbers inside Part 2. A missing-xref ref reached
    // only via a dict value is dropped (not resurrectable), so it stays excluded.
    let admits_body_object = |r: ObjectRef| -> bool { r.number != 0 && live.contains(&r) };

    while let Some((current, via_array)) = queue.pop_front() {
        if !visited.insert(current) {
            continue;
        }
        if resurrectable.contains(&current) {
            // Missing-xref ref: the writer survives array-element slots as null
            // but drops dict-value slots (the key is omitted entirely).  Only
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

        let obj = pdf.resolve(current)?;

        // Determine whether this is a Pages node (intermediate page-tree node)
        // or a Page leaf node.
        let is_pages_node = matches!(&obj, Object::Dictionary(d)
            if matches!(d.get("Type"), Some(Object::Name(n)) if n.as_slice() == b"Pages"));
        let is_page_leaf = matches!(&obj, Object::Dictionary(d)
            if matches!(d.get("Type"), Some(Object::Name(n)) if n.as_slice() == b"Page"));

        if is_pages_node || is_page_leaf {
            if let Some(dict) = obj.as_dict() {
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
                    if let Some(resources) = dict.get("Resources") {
                        let mut seeds: Vec<(ObjectRef, bool)> = Vec::new();
                        collect_direct_refs_with_context(resources, 0, false, &mut seeds)?;
                        for &(r, va) in &seeds {
                            if va {
                                seen_as_array.insert(r); // cov:ignore: fires only when /Resources is an inline dict/array with array-element refs; normal PDFs use an indirect /Resources ref, so seeds come from collect_direct_refs_with_context on a Reference (va=false)
                            }
                        }
                        // DFS via an explicit stack (no recursion) so deeply
                        // nested resource graphs cannot overflow the stack.
                        // The visited set bounds cycles; `is_page_tree_node`
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
                                    deferred_resurrect.insert(r); // cov:ignore: fires when a resurrectable ref is reachable via dict-value in the Resources subtree before the array edge in the same subtree is discovered; requires two resource objects cross-referencing the same xref-absent object via different edge types, which is extremely contrived
                                }
                                continue;
                            }
                            if !admits_body_object(r) {
                                // Null-resolving resource ref (object 0 / missing
                                // xref): no body object, same as the main loop.
                                continue;
                            }
                            let child = pdf.resolve(r)?;
                            // Stop at a page-tree boundary BEFORE adding `r` to
                            // the closure: a resource that malformedly cross-links
                            // to a sibling `/Page` or the `/Pages` node must be
                            // kept in `visited` (so it is never revisited) but
                            // excluded from the first-page closure entirely — per
                            // the page-closure boundary rule, we neither descend
                            // into it nor pull the boundary node itself into
                            // Part 2/3.
                            if is_page_tree_node(&child) {
                                continue;
                            }
                            order.push(r);
                            let mut child_refs: Vec<(ObjectRef, bool)> = Vec::new();
                            collect_direct_refs_with_context(&child, 0, false, &mut child_refs)?;
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
                    if k == b"Kids" {
                        // Pages → sibling pages — never follow.
                        continue;
                    }
                    if k == b"Thumb" {
                        // qpdf gives thumbnail objects the separate ou_thumb
                        // user (not a page user), so page closures never
                        // include /Thumb targets. Skipping here ensures
                        // thumbnail objects land in part4_rest (part 9)
                        // rather than the per-page private/shared sections.
                        continue;
                    }
                    if k == b"Parent" {
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
                        collect_direct_refs(v, 0, &mut to_visit)?;

                        while let Some(parent_ref) = to_visit.pop() {
                            if !seen_parents.insert(parent_ref) {
                                continue;
                            }
                            // Resolve the parent. Genuine resolve failures
                            // (I/O or parse errors) propagate via `?` instead
                            // of silently degrading the closure — mirroring
                            // the main BFS loop's `pdf.resolve_borrowed(current)?`.
                            let parent_dict = match pdf.resolve_borrowed(parent_ref)? {
                                Object::Dictionary(dict) => dict,
                                // A /Parent that indirects through a plain
                                // reference object: follow the chain so the
                                // real ancestor still joins the closure, as
                                // the main BFS loop does via collect_direct_refs.
                                // seen_parents bounds any reference cycle.
                                Object::Reference(r) => {
                                    to_visit.push(*r);
                                    continue;
                                }
                                // Any other non-dictionary parent (a free or
                                // missing object resolving to Null, etc.) is
                                // tolerated: the walk just climbs past it.
                                _ => continue,
                            };
                            for (pk, pv) in parent_dict.iter() {
                                if pk == b"Kids" {
                                    continue;
                                }
                                if pk == b"Parent" {
                                    // Climb to the next ancestor instead of
                                    // stopping at one level.
                                    collect_direct_refs(pv, 0, &mut to_visit)?;
                                    continue;
                                }
                                let mut refs: Vec<(ObjectRef, bool)> = Vec::new();
                                collect_direct_refs_with_context(pv, 0, false, &mut refs)?;
                                for (r, va) in refs {
                                    if va {
                                        seen_as_array.insert(r); // cov:ignore: fires when an ancestor /Pages node has a value with array-element refs (e.g. inherited /ColorSpace [X 0 R]); rare in practice and hard to construct as a minimal fixture
                                    }
                                    if !visited.contains(&r) {
                                        queue.push_back((r, va));
                                    }
                                }
                            }
                        }
                        continue;
                    }
                    collect_direct_refs_with_context(v, 0, false, &mut refs_raw)?;
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
            collect_direct_refs_with_context(&obj, 0, false, &mut refs)?;
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

    // Deferred resurrectable refs: now that the full BFS is complete and
    // seen_as_array is exhaustive, admit those that turned out to be reachable
    // via an array edge (i.e. they appear in seen_as_array).
    for r in deferred_resurrect {
        if seen_as_array.contains(&r) {
            order.push(r);
        }
    }
    // Sort the closure by original object number.  qpdf emits first-page section
    // objects in ascending original-number order (confirmed by discriminator
    // fixtures: Resources-DFS-first order and sort-at-enqueue are both subsumed
    // by this global sort).
    order.sort_by_key(|r| r.number);

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
    /// Populated by the writer subtask (2.3/2.4); empty as a placeholder.
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
    /// the set of 0-based page indices whose closure reaches `r`.
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
    /// [`Pdf::resolve_borrowed`]) — typically an [`crate::Error::Io`] or
    /// [`crate::Error::Parse`] on a truncated or malformed object.
    pub fn from_pdf<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        use_generate_objstm: bool,
    ) -> crate::Result<Self> {
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
        // length-calc reject them ("found unknown object"). This mirrors the
        // plain rewrite path's emission-time skip (see
        // [`crate::writer::is_source_structural_container`]).
        // qpdf garbage-collects objects unreachable from the trailer roots (it
        // only enqueues reachable objects). The plain full-rewrite path does this
        // via CatalogFirstRenumber's trailer-seeded BFS; the linearize universe
        // must too, or re-linearizing an already-linearized source leaks its old
        // /Linearized parameter dict + hint stream — both unreachable structural
        // artifacts — into the second half (flpdf-phfu).
        //
        // `skip_length = true`: the linearized writer always emits a direct
        // `/Length` — re-encoded and lone-`/FlateDecode`-verbatim streams alike,
        // and `renumber_object` substitutes a direct length for a dropped holder's
        // dangling reference — so a stream's indirect `/Length` edge is dead in the
        // output regardless of object-stream mode. Not following it drops a holder
        // reachable only through it, matching qpdf's reachability GC.
        let reachable = crate::rewrite_renumber::reachable_object_set(pdf, true)?;
        let object_refs = pdf.object_refs();
        let mut all_refs: Vec<ObjectRef> = Vec::with_capacity(object_refs.len());
        for r in object_refs {
            if r.number == 0 {
                continue;
            }
            if crate::writer::is_source_structural_container(pdf.resolve_borrowed(r)?) {
                continue;
            }
            if !reachable.contains(&r) {
                // Unreachable from the trailer roots — qpdf drops it. This also
                // drops an orphaned indirect `/Length` holder: the reachability
                // walk does not follow the dead `/Length` edge, so a holder
                // reachable only through it is absent from `reachable`.
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
        // land in part4_rest with LOW numbers (flpdf-o9im).
        let resurrectable = crate::rewrite_renumber::resurrectable_null_refs(pdf)?;
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
        let info_ref = pdf.trailer().get_ref("Info");
        let pages_tree_ref = root_ref
            .and_then(|r| pdf.resolve_borrowed(r).ok())
            .and_then(|obj| match obj {
                Object::Dictionary(d) => d.get_ref("Pages"),
                _ => None,
            });

        // ----------------------------------------------------------------
        // Step 1b: compute open-document set for qpdf precedence.
        // ----------------------------------------------------------------
        // qpdf's in_open_document category takes precedence over in_first_page:
        // objects reachable from catalog open-document keys (/OpenAction,
        // /AcroForm, /ViewerPreferences, /PageMode, /Threads, /Encrypt) are
        // placed in the open-document section (part4, first half, before /O),
        // even if they are also in the first-page closure. Computing this set
        // here ensures Step 5 can exclude them from part2/part3 without
        // requiring the hint builders or container router to compensate.
        //
        // This is object-stream-mode-independent: qpdf runs the same
        // calculateLinearizationData partition in every mode, so disable/preserve
        // must peel these objects to part4 (first half) just like generate
        // (otherwise they sweep into the second half and the param-dict object
        // number, /O, and the whole layout diverge). The eligibility context is
        // only consulted for the generate-mode ObjStm split in Step 7.
        let open_document_set = open_document_set(pdf)?;
        let elig_ctx = if use_generate_objstm {
            Some(eligibility_context(pdf)?)
        } else {
            None
        };

        // ----------------------------------------------------------------
        // Step 1c: compute the in_outlines set for qpdf precedence.
        // ----------------------------------------------------------------
        // qpdf's in_outlines category outranks BOTH in_open_document and
        // in_first_page (QPDF_linearization.cc:1120-1126): an object reachable
        // from the catalog's /Outlines entry is lc_outlines (part6 when
        // /PageMode /UseOutlines, else part9 — both via pushOutlinesToPart) even
        // when it is also reached by the first page or an open-document key. The
        // generate-mode container router (`route_objstm_containers`) already
        // applies this precedence; the classic from_pdf partition below must too,
        // so peel outline objects out of part2/part3 / the per-page-private sets
        // (Steps 5 and 7) and route them through part4_rest to the outline
        // extraction (Step 8). Mode-independent, like open_document above.
        let all_outline_refs: BTreeSet<ObjectRef> = outlines_set(pdf)?;

        // ----------------------------------------------------------------
        // Step 1d: compute the document-level `others` set for the first-page
        // private/shared split.
        // ----------------------------------------------------------------
        // qpdf marks a first-page object as lc_first_page_shared (not private)
        // when it also has a non-zero `others` count — i.e. it is reachable from
        // a catalog key outside {open-document keys, /Outlines, /Pages} or a
        // trailer key outside {/Root, /Encrypt} (QPDF_linearization.cc:1124-1127).
        // Step 5 uses this to route such objects to Part 3 so they sort after the
        // first-page-private objects, matching qpdf's part6 order. Like
        // open_document/outlines above, this is object-stream-mode-independent.
        let document_other_set = document_other_set(pdf)?;

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

        // ----------------------------------------------------------------
        // Step 3: compute first-page closure
        // ----------------------------------------------------------------
        let first_page_closure: Vec<ObjectRef> = if let Some(&first_page) = page_refs.first() {
            compute_closure(pdf, first_page, &live, &resurrectable)?
        } else {
            Vec::new()
        };
        // compute_closure does not follow a stream dict's /Length (qpdf directizes
        // /Length before computing object users), so a stream's indirect /Length
        // holder never enters a page closure: an orphan holder (referenced only via
        // /Length) is GC'd by the all_refs filter above, and a kept holder is
        // reached only via its other (e.g. catalog) edge and partitions from there
        // (flpdf-hwx0 / flpdf-2vfg).
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
        // `all_referenced_pages` is the new full inverse map used for Step 8.
        let mut shared_page_indices: BTreeMap<ObjectRef, BTreeSet<u32>> = BTreeMap::new();
        let mut all_referenced_pages: BTreeMap<ObjectRef, BTreeSet<u32>> = BTreeMap::new();
        let mut other_page_closures: Vec<Vec<ObjectRef>> =
            Vec::with_capacity(page_refs.len().saturating_sub(1));

        // Record page 0 references in the full inverse map.
        for obj_ref in &first_page_closure {
            all_referenced_pages
                .entry(*obj_ref)
                .or_default()
                .insert(0u32);
        }

        for (page_idx, &page_ref) in page_refs.iter().enumerate().skip(1) {
            let closure = compute_closure(pdf, page_ref, &live, &resurrectable)?;
            for obj_ref in &closure {
                // Track cross-page sharing for first-page objects (used by Part 3 partition).
                if first_page_set.contains(obj_ref) {
                    shared_page_indices
                        .entry(*obj_ref)
                        .or_default()
                        .insert(page_idx as u32);
                }
                // Track all page references in the full inverse map.
                all_referenced_pages
                    .entry(*obj_ref)
                    .or_default()
                    .insert(page_idx as u32);
            }
            other_page_closures.push(closure);
        }

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
            // last second-half object, not a first-page-shared object (flpdf-q2zw).
            if all_outline_refs.contains(obj_ref) || open_document_set.contains(obj_ref) {
                continue;
            }
            if Some(*obj_ref) == first_page_ref {
                part2_objects.push(*obj_ref);
            } else if shared_page_indices.contains_key(obj_ref)
                || document_other_set.contains(obj_ref)
            {
                // lc_first_page_shared: in_first_page AND (other_pages>0 ||
                // others>0). `shared_page_indices` supplies other_pages (another
                // page's closure); `document_other_set` supplies others (a
                // document-level reference such as a Catalog key). Either makes
                // the object shared (QPDF_linearization.cc:1124-1127), so it
                // sorts after the first-page-private objects in part 6.
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
                    if open_document_set.contains(r) {
                        return false;
                    }
                    // Outline objects (in_outlines) outrank other-page-private the
                    // same way: keep them out of the per-page-private set so the
                    // part7 pre-pass never claims them and they stay available for
                    // outline routing in the part8/part9 loop (flpdf-q2zw).
                    if all_outline_refs.contains(r) {
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
            let in_first_page = first_page_set.contains(&r) && !open_document_set.contains(&r);
            if in_first_page {
                // Should have been in Part 2 or Part 3 — skip (defensive).
                continue;
            }
            // Route open-document objects to part4 (first half, before /O) in
            // EVERY mode — qpdf's part4 = [lc_root] ++ lc_open_document. Outline
            // objects (which qpdf orders above in_open_document) were already
            // routed above, so anything reaching here that is in open_document_set
            // is a genuine lc_open_document object.
            if open_document_set.contains(&r) {
                if let Some(ctx) = elig_ctx.as_ref() {
                    // generate mode: an OD object eligible for ObjStm packing goes
                    // to part4_rest (the batch planner packs it into the
                    // open-document container); an ineligible stream (e.g. an /AP /N
                    // appearance stream, which cannot be an ObjStm member) emits
                    // plain pre-/O.  Oracle: qpdf --object-streams=generate places
                    // such a Form XObject at a lower object number than the OD
                    // ObjStm, physically before the hint stream.
                    let obj = pdf.resolve_borrowed(r)?;
                    if is_eligible_for_objstm(r, obj, ctx) {
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
                // reach == 0, or reach == 1 but not captured as private (shouldn't
                // happen since per_page_private_objects captures all reach-1
                // non-first objects). Everything else goes to part9.
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
        let outline_root_ref: Option<ObjectRef> = pdf
            .root_ref()
            .and_then(|r| pdf.resolve_borrowed(r).ok()?.as_dict()?.get_ref("Outlines"));

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
        self.part4_other_pages_private
            .iter()
            .chain(&self.part4_other_pages_shared)
            .chain(&self.part9_outline_objects)
            .chain(&self.part4_rest)
            .copied()
            .collect()
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
/// ObjStm containers can never span a part boundary. `part2_objects`
/// (first-page closure exclusives) are **never** placed in any batch list —
/// they stay as plain indirect objects.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ObjStmBatchPlan {
    /// ObjStm batches for qpdf part4 (open-document objects). Numbered and
    /// emitted in the first half, before the first-page section.
    pub open_document_batches: Vec<Vec<ObjectRef>>,
    /// ObjStm batches for Part 3 (shared/catalog) objects.
    pub part3_batches: Vec<Vec<ObjectRef>>,
    /// ObjStm batches for Part 4 (rest-of-document) objects.
    pub part4_batches: Vec<Vec<ObjectRef>>,
}

// These items are consumed by the upcoming ObjStm linearized writer (5.8.2+);
// suppress dead_code until that code lands.
#[allow(dead_code)]
impl LinearizationPlan {
    /// Build a Part-tagged ObjStm packing plan from this `LinearizationPlan`.
    ///
    /// # Mode behaviour
    ///
    /// | Mode | Result |
    /// |------|--------|
    /// | `Disable` | Both batch lists are empty (no ObjStms emitted). |
    /// | `Generate` | Eligible Part-3 (first-page shared) objects are packed into `part3_batches`; eligible Part-4 objects are packed into `part4_batches`. The membership is qpdf-canonical by construction (a global even split with the page dictionaries and `/Catalog` erased), so no post-packing reshape is applied. |
    /// | `Preserve` | The source ObjStm grouping is reproduced verbatim: each source container's eligible members are routed to their linearization part (first half → `part3_batches`, second half → `part4_batches`), and the `/Pages` tree and `/Info` dictionary ride along in whatever source container held them. Members in `part2_objects` or ineligible per [`is_eligible_for_objstm`] are dropped, and the `/Catalog` is excluded (qpdf never compresses it). Members that span the Part-3/Part-4 boundary are split into separate batches per part. There is no fold and no re-chunk across containers. If the source document contained no ObjStms, both batch lists are **empty** — Preserve does **not** fall through to Generate; it mirrors the behaviour of the non-linearized `writer::object_streams::plan_preserve` and qpdf's `--object-streams=preserve` semantics (preserve means "keep what was there", not "invent new ObjStms"). |
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
    /// * No ref from `part2_objects` appears in any batch.
    /// * Ineligible objects (streams, gen > 0, encryption dict, linearization
    ///   param dict, `/Type /ObjStm`, `/Type /XRef`) are excluded via the
    ///   shared [`is_eligible_for_objstm`] predicate.
    /// * Cap (`DEFAULT_BATCH_SIZE_CAP`) is applied independently per Part.
    pub(crate) fn objstm_batches<R: Read + Seek>(
        &self,
        pdf: &mut Pdf<R>,
        config: &PlannerConfig,
    ) -> crate::Result<ObjStmBatchPlan> {
        if config.mode == ObjectStreamMode::Disable {
            return Ok(ObjStmBatchPlan::default());
        }

        let ctx = eligibility_context(pdf)?;
        let length_exclusions = collect_indirect_objstm_length_refs(pdf)?;

        let plan = match config.mode {
            ObjectStreamMode::Disable => unreachable!(),
            ObjectStreamMode::Generate => {
                // The Generate path reproduces qpdf's linearized
                // `generateObjectStreams` directly: a GLOBAL even split over the
                // compressible set, with the page dictionaries + root Catalog
                // erased and `/Info` / the `/Pages` tree kept as ordinary
                // members. That membership is already qpdf-canonical, so no
                // post-packing reshape is applied.
                self.objstm_batches_generate(pdf, config, &ctx, &length_exclusions)?
            }
            ObjectStreamMode::Preserve => {
                let mut plan =
                    self.objstm_batches_preserve(pdf, config, &ctx, &length_exclusions)?;

                // qpdf's preserveObjectStreams copies the source object->stream
                // assignment verbatim and the linearized pass only erases the
                // /Page dicts and the /Catalog (QPDFWriter.cc:1939, 2141-2161).
                // objstm_batches_preserve already reproduces that grouping: the
                // /Pages tree and /Info ride along in their source container's
                // first-half batch, and the /Page dicts are excluded. Only the
                // /Catalog still needs dropping here — qpdf never compresses it,
                // but a source ObjStm may have held it, so remove it from Part 4.
                if let Some(catalog_ref) = self.root_ref {
                    Self::drop_from_part4_batches(&mut plan, &BTreeSet::from([catalog_ref]));
                }
                plan
            }
        };

        Ok(plan)
    }

    /// Remove every ref in `excluded` from each Part-4 ObjStm batch, dropping any
    /// batch left empty.  An empty `excluded` set is a harmless no-op.
    fn drop_from_part4_batches(plan: &mut ObjStmBatchPlan, excluded: &BTreeSet<ObjectRef>) {
        plan.part4_batches = std::mem::take(&mut plan.part4_batches)
            .into_iter()
            .filter_map(|batch| {
                let kept: Vec<ObjectRef> = batch
                    .into_iter()
                    .filter(|r| !excluded.contains(r))
                    .collect();
                (!kept.is_empty()).then_some(kept)
            })
            .collect();
    }

    /// Generate mode: reproduce qpdf's linearized `generateObjectStreams`.
    ///
    /// A GLOBAL even split over the compressible set
    /// ([`objstm_membership_linearized`]), with the page dictionaries + root
    /// Catalog erased, then each container routed to a linearization part by the
    /// union of its members' page users ([`route_objstm_containers`]). Containers
    /// routed to part 6 ([`ContainerPart::FirstPage`]) become first-half
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
    ) -> crate::Result<ObjStmBatchPlan> {
        // `objstm_membership_linearized` filters its containers to the plan's
        // renumber-assigned set BEFORE the even split, so a trailer-only ref with
        // no slot neither inflates the split boundary nor reaches
        // `place_objstm_members_per_half` without a slot (which would panic).
        // Dangling/missing refs (e.g. `/Info 99 0 R` with no xref entry) are
        // already excluded upstream by `compressible_objgens`. For a well-formed
        // PDF every member is assigned, so the filter is a no-op and the batches
        // stay byte-identical.
        let assigned = self.renumber_assigned_refs();
        let containers = objstm_membership_linearized(pdf, &assigned)?;
        let routes = route_objstm_containers(pdf, &containers)?;

        let outline_set = &self.outline_first_page_members;

        let mut open_document_batches: Vec<Vec<ObjectRef>> = Vec::new();
        // Separate first-page containers into regular (fonts/shared) and
        // outline-routed.  qpdf places outline containers AFTER the regular
        // first-page containers in the first half, so regular go first and
        // outline containers are appended last (QPDF_linearization.cc:1031-1043).
        let mut part3_regular: Vec<Vec<ObjectRef>> = Vec::new();
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
        // far, so their within-part multi-container order is untested (see flpdf-g1eu
        // follow-up); if such a case ever arises a finer per-part sort may be needed.
        let mut part4_private: Vec<Vec<ObjectRef>> = Vec::new();
        let mut part4_shared: Vec<Vec<ObjectRef>> = Vec::new();
        let mut part4_rest: Vec<Vec<ObjectRef>> = Vec::new();
        for (mut members, route) in containers.into_iter().zip(routes) {
            members.sort_unstable_by_key(|r| r.number);
            match route {
                ContainerPart::OpenDocument => open_document_batches.push(members),
                ContainerPart::FirstPage => {
                    if !outline_set.is_empty() && members.iter().any(|m| outline_set.contains(m)) {
                        part3_outlines.push(members);
                    } else {
                        part3_regular.push(members);
                    }
                }
                ContainerPart::OtherPagePrivate => part4_private.push(members),
                ContainerPart::OtherPageShared => part4_shared.push(members),
                ContainerPart::Rest => part4_rest.push(members),
            }
        }
        // Concatenate the buckets in part order (part7, part8, part9).
        let mut part4_batches = part4_private;
        part4_batches.extend(part4_shared);
        part4_batches.extend(part4_rest);

        // Regular first-page containers numbered before outline containers so
        // that outline ObjStms get higher golden object numbers (matching qpdf).
        let mut part3_batches = part3_regular;
        part3_batches.extend(part3_outlines);

        Ok(ObjStmBatchPlan {
            open_document_batches,
            part3_batches,
            part4_batches,
        })
    }

    /// Preserve mode: reconstruct source ObjStm grouping, splitting cross-boundary batches.
    fn objstm_batches_preserve<R: Read + Seek>(
        &self,
        pdf: &mut Pdf<R>,
        config: &PlannerConfig,
        ctx: &crate::writer::object_streams::EligibilityContext,
        length_exclusions: &BTreeSet<ObjectRef>,
    ) -> crate::Result<ObjStmBatchPlan> {
        use crate::XrefOffset;

        let cap = config.batch_size_cap.get();
        let entries = pdf.source_xref_entries();

        // Build source ObjStm groups: container_number → [(index, ref)]
        let mut groups: BTreeMap<u32, Vec<(u32, ObjectRef)>> = BTreeMap::new();
        for (obj_ref, offset) in &entries {
            if let XrefOffset::Compressed { stream, index } = offset {
                groups.entry(*stream).or_default().push((*index, *obj_ref));
            }
        }

        let part2_set: BTreeSet<ObjectRef> = self.part2_objects.iter().copied().collect();
        // part6_outline_objects are first-half objects (like Part-3 members) when
        // UseOutlines is set; include them so ObjStm-compressed outline objects are
        // batched into first-half containers rather than silently dropped to plain.
        let part3_set: BTreeSet<ObjectRef> = self
            .part3_objects
            .iter()
            .chain(&self.part6_outline_objects)
            .copied()
            .collect();
        // Only objects actually in the linearization plan's Part-4 set have a
        // RenumberMap entry. A source ObjStm may carry eligible-but-unplanned
        // objects (unreachable / trailer-only); batching those would make
        // ObjStmLayout::build fail with "has no renumber entry". Skip them.
        // part9_outline_objects are second-half objects; include them for the same
        // reason as part6 above.
        let part4_set: BTreeSet<ObjectRef> = self
            .part4_other_pages_private
            .iter()
            .chain(&self.part4_other_pages_shared)
            .chain(&self.part4_rest)
            .chain(&self.part9_outline_objects)
            .copied()
            .collect();

        // Part-4 ownership classification (same rationale as the Generate path):
        // a batch must not co-locate objects with different page ownership, so
        // Part-4 members are bucketed by owner — per non-first page private set,
        // shared (qpdf part8), or rest (qpdf part9) — and chunked per bucket.
        #[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
        enum Owner {
            Page(usize),
            Shared,
            Rest,
        }
        let page_private_sets: Vec<BTreeSet<ObjectRef>> = self
            .per_page_private_objects
            .iter()
            .skip(1)
            .map(|v| v.iter().copied().collect())
            .collect();
        let shared_set: BTreeSet<ObjectRef> =
            self.part4_other_pages_shared.iter().copied().collect();
        // part9_outline_objects are "rest" objects: not page-private or shared.
        let rest_set: BTreeSet<ObjectRef> = self
            .part4_rest
            .iter()
            .chain(&self.part9_outline_objects)
            .copied()
            .collect();
        let owner_of = |r: &ObjectRef| -> Option<Owner> {
            for (i, s) in page_private_sets.iter().enumerate() {
                if s.contains(r) {
                    return Some(Owner::Page(i));
                }
            }
            if shared_set.contains(r) {
                return Some(Owner::Shared);
            }
            if rest_set.contains(r) {
                return Some(Owner::Rest);
            }
            None
        };

        let mut part3_batches: Vec<Vec<ObjectRef>> = Vec::new();
        let mut part4_batches: Vec<Vec<ObjectRef>> = Vec::new();

        // Iterate containers in ascending container-number order.
        for (_container_num, mut members) in groups {
            members.sort_by_key(|(idx, _)| *idx);

            // Partition eligible members by destination part, and within Part 4
            // by owner so cross-page-boundary co-location cannot occur.
            let mut p3_eligible: Vec<ObjectRef> = Vec::new();
            let mut p4_by_owner: BTreeMap<Owner, Vec<ObjectRef>> = BTreeMap::new();

            for (_idx, obj_ref) in members {
                // Part-2 objects must never enter ObjStms.
                if part2_set.contains(&obj_ref) {
                    continue;
                }
                if length_exclusions.contains(&obj_ref) {
                    continue;
                }
                let obj = pdf.resolve_borrowed(obj_ref)?;
                if !is_eligible_for_objstm(obj_ref, obj, ctx) {
                    continue;
                }
                if Some(obj_ref) == self.pages_tree_ref || Some(obj_ref) == self.info_ref {
                    // qpdf's preserveObjectStreams copies the source
                    // object->stream assignment verbatim; the linearized pass
                    // only erases the /Page dicts and the /Catalog from it
                    // (QPDFWriter.cc:1939, 2141-2161). It never relocates the
                    // /Pages tree or /Info, so they ride along in whatever
                    // source ObjStm container they were in. The planner puts
                    // both in part4_rest, so the owner gating below would route
                    // them to a second-half bucket; route them into THIS
                    // container's first-half (part3) bucket instead so the
                    // source grouping survives. They carry a valid RenumberMap
                    // slot via part4_rest (renumber promotes them to the first
                    // half), so part3 batching is sound.
                    p3_eligible.push(obj_ref);
                } else if part3_set.contains(&obj_ref) {
                    p3_eligible.push(obj_ref);
                } else if part4_set.contains(&obj_ref) {
                    // part4_set gates owner bucketing: only objects with a
                    // linearization-plan slot (RenumberMap entry) may be
                    // batched. owner_of's page-private set is the raw
                    // per_page_private list (unfiltered), so an unplanned
                    // member could otherwise be bucketed and crash
                    // ObjStmLayout::build with "has no renumber entry".
                    if let Some(owner) = owner_of(&obj_ref) {
                        p4_by_owner.entry(owner).or_default().push(obj_ref);
                    }
                }
                // else: eligible but not in any linearization part (no
                // RenumberMap entry) — leave it as a plain indirect object.
            }

            // Split into cap-sized batches per part / per owner.
            for chunk in p3_eligible.chunks(cap) {
                if !chunk.is_empty() {
                    part3_batches.push(chunk.to_vec());
                }
            }
            for refs in p4_by_owner.values() {
                for chunk in refs.chunks(cap) {
                    if !chunk.is_empty() {
                        part4_batches.push(chunk.to_vec());
                    }
                }
            }
        }

        Ok(ObjStmBatchPlan {
            // Preserve mode does not model the open-document category (no such
            // fixture in the supported corpus); it reconstructs the source
            // grouping verbatim. Generate mode (`objstm_batches_generate`)
            // routes open-document containers.
            open_document_batches: Vec::new(),
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
/// `OpenDocument` is qpdf part 4 (open-document objects, first half), `FirstPage`
/// part 6 (first-page section), `OtherPagePrivate` part 7, `OtherPageShared`
/// part 8, and `Rest` part 9. qpdf checks `in_open_document` *before*
/// `in_first_page`, so [`route_objstm_containers`] tests it first. The outline
/// and thumbnail categories qpdf also checks before `in_first_page` are not yet
/// modeled (see [`route_objstm_containers`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContainerPart {
    /// qpdf part 4 — the container holds at least one open-document object
    /// (reachable from the catalog's `/OpenAction`, `/AcroForm`,
    /// `/ViewerPreferences`, `/PageMode`, `/Threads`, or the trailer's
    /// `/Encrypt`). Takes precedence over every page category.
    OpenDocument,
    /// qpdf part 6 — the container holds at least one first-page object.
    FirstPage,
    /// qpdf part 7 — the container's members are private to exactly one
    /// non-first page.
    OtherPagePrivate,
    /// qpdf part 8 — the container's members are shared by two or more
    /// non-first pages.
    OtherPageShared,
    /// qpdf part 9 — the container reaches no page (trailer-only members).
    Rest,
}

/// Compute the linearized generate-mode ObjStm membership.
///
/// Runs qpdf's `generateObjectStreams` even split
/// ([`compressible_objgens`](crate::writer::object_streams::compressible_objgens)
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
/// [`compressible_objgens`](crate::writer::object_streams::compressible_objgens)
/// admits but the linearization plan places in no part — a trailer-only object
/// with no slot — is dropped **before** the even split, so it cannot inflate the
/// split boundary and scatter real members across separate ObjStms. (Dangling /
/// missing refs are already excluded upstream by `compressible_objgens`, which
/// qpdf treats as null, so they never reach this retain.) The page dictionaries
/// and root Catalog are in `assigned`, so they still consume split positions and
/// are erased afterwards, exactly as qpdf does.
///
/// # Errors
///
/// Propagates reader errors from the compressible-set traversal or the page-tree
/// walk used to build the erase set.
pub(crate) fn objstm_membership_linearized<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    assigned: &BTreeSet<ObjectRef>,
) -> crate::Result<Vec<Vec<ObjectRef>>> {
    let mut eligible = crate::writer::object_streams::compressible_objgens(pdf)?;
    // Resurrected null-resolving array refs (flpdf-0gyq): `compressible_objgens`
    // filters to live objects, so it excludes these free/missing refs — but qpdf
    // compresses them as the TRAILING members of the ObjStm. Append them after the
    // live members (in source-object-number order). They have renumber slots from
    // the plan's all_refs admission, so they survive the `assigned` retain below.
    let resurrected = crate::rewrite_renumber::resurrectable_null_refs(pdf)?;
    eligible.extend(resurrected);
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
/// Shared by [`open_document_set`] (which collects them) and
/// [`document_other_set`] (which collects the complement) so the partition
/// stays a single source of truth.
const OPEN_DOCUMENT_CATALOG_KEYS: [&[u8]; 5] = [
    b"ViewerPreferences",
    b"PageMode",
    b"Threads",
    b"OpenAction",
    b"AcroForm",
];

/// Compute the set of objects qpdf categorizes `in_open_document`.
///
/// Mirrors qpdf's `optimize()` open-document object users
/// (QPDF_optimization.cc:91-110) followed by the `open_document_keys` test in
/// `calculateLinearizationData` (QPDF_linearization.cc:1045-1097): every
/// indirect object reachable from the document catalog's `/ViewerPreferences`,
/// `/PageMode`, `/Threads`, `/OpenAction`, or `/AcroForm` entries, or from the
/// trailer's `/Encrypt` entry.
///
/// The traversal mirrors `updateObjectMapsInternal`
/// (QPDF_optimization.cc:271-337): it records every indirect object it reaches
/// but STOPS at a `/Page` leaf (a page boundary), so an `/OpenAction` destination
/// like `[page /Fit]` drops the page and keeps only the non-page objects. A
/// single shared `visited` set is sufficient because the result is the union
/// over all keys.
///
/// qpdf categorizes `in_open_document` with HIGHER precedence than
/// `in_first_page`, so [`route_objstm_containers`] tests this set before the
/// page categories.
///
/// # Errors
///
/// Propagates reader errors from resolving the catalog, the trailer values, or
/// any reached object.
fn open_document_set<R: Read + Seek>(pdf: &mut Pdf<R>) -> crate::Result<BTreeSet<ObjectRef>> {
    // Seed refs: the indirect refs inside each open-document key's value. A
    // direct value (e.g. an inline /OpenAction action dict) contributes only the
    // indirect refs it contains; qpdf records indirect objects, not inline ones.
    let mut seeds: Vec<ObjectRef> = Vec::new();
    if let Some(enc) = pdf.trailer().get("Encrypt") {
        // cov:ignore-start: /Encrypt is only meaningful for encrypted+linearized
        // output (deferred to flpdf-j4ph); the linearize write path rejects
        // encrypted input (`reject_encrypted_write`) before this helper runs, so
        // it only ever sees plaintext documents (no trailer /Encrypt).
        collect_direct_refs(enc, 0, &mut seeds)?;
        // cov:ignore-end
    }
    // Resolve the catalog, propagating real read errors. `Pdf::open` guarantees
    // a `/Root`, so the `Option` is `Some` in practice; a `/Root` that resolves
    // to a non-dictionary (malformed) simply yields no open-document seeds.
    let catalog = pdf
        .root_ref()
        .map(|root| pdf.resolve_borrowed(root))
        .transpose()?;
    if let Some(Object::Dictionary(catalog)) = catalog {
        for key in OPEN_DOCUMENT_CATALOG_KEYS {
            if let Some(v) = catalog.get(key) {
                collect_direct_refs(v, 0, &mut seeds)?;
            }
        }
    }

    let page_tree = page_tree_node_refs(pdf)?;
    closure_from_seeds(pdf, seeds, &page_tree)
}

/// Compute the set of objects qpdf categorizes with a non-zero `others` count:
/// objects reachable from a document-level reference that is neither an
/// open-document key, `/Outlines`, the page tree, nor the trailer `/Root` /
/// `/Encrypt`.
///
/// Mirrors qpdf's `optimize()` `ou_trailer_key` / `ou_root_key` users
/// (QPDF_optimization.cc:92-110) and the `++others` arms of
/// `calculateLinearizationData` (QPDF_linearization.cc:1077-1096): a catalog key
/// other than [`OPEN_DOCUMENT_CATALOG_KEYS`] and `/Outlines`, or a trailer key
/// other than `/Root` (`ou_root`) and `/Encrypt` (`in_open_document`),
/// increments `others`. A first-page object that also appears here is therefore
/// `lc_first_page_shared` (QPDF_linearization.cc:1124-1127), not
/// `lc_first_page_private`, and so sorts after the private objects in part 6.
///
/// `/Pages` IS seeded (qpdf records `ou_root_key("/Pages")` like any other root
/// key), so a first-page object reached via a custom extension key on a `/Pages`
/// node is correctly `others`. [`closure_from_seeds`] reproduces qpdf's
/// post-`pushInheritedAttributesToPage` tree by skipping the inherited
/// attributes on `/Pages` nodes, so the page tree's `/Resources` etc. do NOT
/// over-classify first-page resources. Cross-page sharing is still handled
/// separately by the per-page closures (`shared_page_indices`).
///
/// # Errors
///
/// Propagates reader errors from resolving the catalog, the trailer values, or
/// any reached object.
fn document_other_set<R: Read + Seek>(pdf: &mut Pdf<R>) -> crate::Result<BTreeSet<ObjectRef>> {
    let mut seeds: Vec<ObjectRef> = Vec::new();
    // Trailer keys except /Root (ou_root, the catalog) and /Encrypt
    // (in_open_document, already seeded by open_document_set).
    {
        let trailer = pdf.trailer();
        for (key, v) in trailer.iter() {
            if key != b"Root" && key != b"Encrypt" {
                collect_direct_refs(v, 0, &mut seeds)?;
            }
        }
    }
    // Catalog keys except the open-document keys (in_open_document) and
    // /Outlines (in_outlines). /Pages IS included: closure_from_seeds skips the
    // inherited attributes qpdf strips from /Pages nodes, so only genuine custom
    // keys on the page tree contribute `others`.
    let catalog = pdf
        .root_ref()
        .map(|root| pdf.resolve_borrowed(root))
        .transpose()?;
    if let Some(Object::Dictionary(catalog)) = catalog {
        for (key, v) in catalog.iter() {
            if !OPEN_DOCUMENT_CATALOG_KEYS.contains(&key) && key != b"Outlines" {
                collect_direct_refs(v, 0, &mut seeds)?;
            }
        }
    }

    let page_tree = page_tree_node_refs(pdf)?;
    closure_from_seeds(pdf, seeds, &page_tree)
}

/// The inheritable page attributes (ISO 32000-1 Table 30) that qpdf's
/// `pushInheritedAttributesToPage` (QPDF_optimization.cc:165-205) removes from
/// interior `/Pages` nodes before the object-user maps are built.
/// [`closure_from_seeds`] skips them so an object reached ONLY through an
/// inherited attribute on a `/Pages` node is not recorded as a document-level
/// user (it would already be reached, post-push, from its `/Page` leaf instead).
const INHERITABLE_PAGE_KEYS: [&[u8]; 4] = [b"MediaBox", b"CropBox", b"Resources", b"Rotate"];

/// The indirect refs of the interior `/Type /Pages` nodes of the catalog's
/// actual page tree (reachable from `/Root` → `/Pages` → `/Kids`).
///
/// qpdf's `pushInheritedAttributesToPage` (QPDF_optimization.cc:159-235) strips
/// inheritable attributes from EXACTLY these nodes — it walks only `/Root`
/// `/Pages` — so [`closure_from_seeds`] must restrict its inherited-key skip to
/// this set. An unrelated `/Type /Pages` dictionary reached from some other
/// document-level structure is left intact by qpdf and so must be descended in
/// full (including its `/Resources`). The traversal is iterative and bounded by
/// `visited`, so a cyclic or adversarial `/Kids` graph terminates.
///
/// # Errors
///
/// Propagates reader errors from resolving the catalog or a page-tree node.
fn page_tree_node_refs<R: Read + Seek>(pdf: &mut Pdf<R>) -> crate::Result<BTreeSet<ObjectRef>> {
    let mut out: BTreeSet<ObjectRef> = BTreeSet::new();
    let pages_ref = match pdf
        .root_ref()
        .map(|root| pdf.resolve_borrowed(root))
        .transpose()?
    {
        Some(Object::Dictionary(d)) => d.get_ref("Pages"),
        _ => None,
    };
    let Some(pages_ref) = pages_ref else {
        return Ok(out);
    };
    let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut queue: VecDeque<ObjectRef> = VecDeque::new();
    queue.push_back(pages_ref);
    while let Some(r) = queue.pop_front() {
        if !visited.insert(r) {
            continue;
        }
        let Object::Dictionary(d) = pdf.resolve_borrowed(r)? else {
            continue;
        };
        // Only interior `/Pages` nodes are stripped; a `/Page` leaf (or any
        // non-`/Pages` dict reached via a malformed `/Kids`) is not collected.
        if !matches!(d.get("Type"), Some(Object::Name(n)) if n.as_slice() == b"Pages") {
            continue;
        }
        out.insert(r);
        if let Some(kids) = d.get("Kids") {
            let mut kid_refs = Vec::new();
            collect_direct_refs(kids, 0, &mut kid_refs)?;
            for kr in kid_refs {
                if !visited.contains(&kr) {
                    queue.push_back(kr);
                }
            }
        }
    }
    Ok(out)
}

/// Transitive closure of indirect objects reachable from `seeds`, stopping at
/// `/Page` leaves and skipping inherited attributes on page-tree `/Pages` nodes.
///
/// Mirrors qpdf's `updateObjectMapsInternal` (QPDF_optimization.cc:271-337) used
/// to record a document-level key's object users, run on the tree AFTER
/// `pushInheritedAttributesToPage` (QPDFWriter calls `optimize(allow_changes=true)`):
/// it records every indirect object it reaches but
/// - neither records nor descends a non-top `/Page` leaf (a page boundary), so a
///   destination like `[page /Fit]` drops the page; and
/// - on an interior `/Pages` node THAT IS IN THE PAGE TREE (`page_tree`), skips
///   the [`INHERITABLE_PAGE_KEYS`] (which qpdf has stripped) while still
///   following `/Kids` and any custom keys, so a first-page resource reached only
///   via an inherited `/Resources` on a page-tree node is NOT mistaken for a
///   document-level user, whereas a genuine custom extension key on such a node
///   still is. An unrelated `/Type /Pages` dict outside `page_tree` is descended
///   in full (qpdf does not strip it).
///
/// `page_tree` is [`page_tree_node_refs`]. A single shared `visited` set suffices
/// because the result is the union over all seeds.
///
/// # Errors
///
/// Propagates reader errors from resolving any reached object.
fn closure_from_seeds<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    seeds: Vec<ObjectRef>,
    page_tree: &BTreeSet<ObjectRef>,
) -> crate::Result<BTreeSet<ObjectRef>> {
    let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut out: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut queue: VecDeque<ObjectRef> = seeds.into_iter().collect();
    while let Some(r) = queue.pop_front() {
        if !visited.insert(r) {
            continue;
        }
        let obj = pdf.resolve_borrowed(r)?;
        let mut children = Vec::new();
        match obj {
            // Page boundary: qpdf neither records nor descends a non-top `/Page`
            // leaf reached while tracing a document-level key.
            Object::Dictionary(d) if matches!(d.get("Type"), Some(Object::Name(n)) if n.as_slice() == b"Page") =>
            {
                continue;
            }
            // Page-tree interior `/Pages` node: skip the inherited attributes
            // qpdf has stripped, but follow `/Kids` (→ the `/Page` leaves above)
            // and any custom extension keys. Restricted to `page_tree` — a
            // `/Type /Pages` dict outside the real page tree is NOT stripped by
            // qpdf, so it falls through to the `_` arm and is descended in full.
            Object::Dictionary(d) if page_tree.contains(&r) => {
                for (k, v) in d.iter() {
                    if !INHERITABLE_PAGE_KEYS.contains(&k) {
                        collect_direct_refs(v, 0, &mut children)?;
                    }
                }
            }
            _ => collect_direct_refs(obj, 0, &mut children)?,
        }
        out.insert(r);
        for cr in children {
            if !visited.contains(&cr) {
                queue.push_back(cr);
            }
        }
    }
    Ok(out)
}

/// Compute the set of objects qpdf categorizes `in_outlines`.
///
/// Mirrors qpdf's `optimize()` ou_root_key "/Outlines" users
/// (QPDF_optimization.cc:103-110) and the `in_outlines` test in
/// `calculateLinearizationData` (QPDF_linearization.cc:1092-1093): every indirect
/// object reachable from the document catalog's `/Outlines` entry, with the same
/// `/Page`-boundary traversal as [`open_document_set`].
///
/// qpdf categorizes `in_outlines` with HIGHER precedence than both
/// `in_open_document` and `in_first_page` (QPDF_linearization.cc:1118-1122).
///
/// # Errors
///
/// Propagates reader errors from resolving the catalog or any reached object.
pub(crate) fn outlines_set<R: Read + Seek>(pdf: &mut Pdf<R>) -> crate::Result<BTreeSet<ObjectRef>> {
    let mut seeds: Vec<ObjectRef> = Vec::new();
    let catalog = pdf
        .root_ref()
        .map(|root| pdf.resolve_borrowed(root))
        .transpose()?;
    if let Some(Object::Dictionary(catalog)) = catalog {
        if let Some(v) = catalog.get("Outlines") {
            collect_direct_refs(v, 0, &mut seeds)?;
        }
    }
    let page_tree = page_tree_node_refs(pdf)?;
    closure_from_seeds(pdf, seeds, &page_tree)
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
    let Object::Dictionary(cat) = pdf.resolve(root)? else {
        return Ok(false); // cov:ignore: non-dictionary catalog unreachable on valid linearizable PDF
    };
    if cat.get("Outlines").is_none() {
        return Ok(false);
    }
    match cat.get("PageMode") {
        Some(Object::Name(n)) => Ok(n == b"UseOutlines"),
        // cov:ignore-start: /PageMode as indirect reference; structurally identical to
        // the direct-name arm; exercising requires a dedicated fixture with indirect PageMode
        Some(Object::Reference(r)) => {
            let r = *r;
            Ok(matches!(pdf.resolve(r)?, Object::Name(n) if n == b"UseOutlines"))
        }
        // cov:ignore-end
        _ => Ok(false),
    }
}

/// Route each ObjStm container to a linearization part by the union of its
/// members' object users.
///
/// Mirrors qpdf's `filterCompressedObjects` (the container inherits the union of
/// every member's obj_users) followed by the `lc_*` categorization. In qpdf's
/// precedence order: a container holding any outline object is part 6
/// ([`ContainerPart::FirstPage`]) when `/PageMode /UseOutlines` is set, or
/// part 9 ([`ContainerPart::Rest`]) otherwise; a container holding any
/// [`open_document_set`] object is part 4 ([`ContainerPart::OpenDocument`]);
/// otherwise a container holding any first-page object is part 6
/// ([`ContainerPart::FirstPage`]); otherwise it is part 7 / part 8 / part 9 by
/// the number of *distinct non-first* pages its members reach (one →
/// [`ContainerPart::OtherPagePrivate`], two or more →
/// [`ContainerPart::OtherPageShared`], none → [`ContainerPart::Rest`]).
///
/// The page-user signals (first-page closure and the per-object referencing-page
/// map) are recomputed exactly as [`LinearizationPlan::from_pdf`] derives them.
///
/// # Deviation
///
/// **Multiple open-document containers (verified, flpdf-699x):** qpdf assigns
/// container `ObjGen`s sequentially in even-split order, so its
/// `std::set<QPDFObjGen>` (used for `lc_open_document`) iterates them in the
/// same DFS / even-split order that this function preserves.  The ordering is
/// therefore byte-identical to qpdf for ≥2 open-document containers; verified
/// with `objstm-lin-openaction-multi-od` (two OD containers whose min-member
/// numbers are non-ascending in DFS order).  Thumbnail categories are handled
/// implicitly: `compute_closure` skips `/Thumb`, so thumbnail objects have
/// page_reach 0 and any container holding only thumbnail members already maps
/// to [`ContainerPart::Rest`] via the `other_pages.len() == 0` branch.
///
/// # Errors
///
/// Propagates reader errors from the page-tree walk, the per-page closures, or
/// the open-document traversal.
pub(crate) fn route_objstm_containers<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    containers: &[Vec<ObjectRef>],
) -> crate::Result<Vec<ContainerPart>> {
    // in_outlines takes precedence over in_open_document and in_first_page
    // (QPDF_linearization.cc:1118-1122).
    let outline_set = outlines_set(pdf)?;
    let outlines_first_page = if outline_set.is_empty() {
        false
    } else {
        outlines_in_first_page_predicate(pdf)?
    };

    let open_doc_set = open_document_set(pdf)?;

    let page_refs = crate::pages::page_refs(pdf)?;

    // Computed once and shared across every per-page closure below (the loop
    // would otherwise re-scan the whole xref table O(pages) times).
    let live: BTreeSet<ObjectRef> = pdf.live_object_refs().into_iter().collect();
    let resurrectable = crate::rewrite_renumber::resurrectable_null_refs(pdf)?;

    let first_page_set: BTreeSet<ObjectRef> = match page_refs.first() {
        Some(&first_page) => compute_closure(pdf, first_page, &live, &resurrectable)?
            .into_iter()
            .collect(),
        // A linearizable document always has at least one page, so the page-less
        // branch never fires on the generate-mode call path.
        None => BTreeSet::new(), // cov:ignore: page-less catalog unreachable here
    };

    // obj_user page map: object -> set of page indices whose closure reaches it.
    let mut referenced_pages: BTreeMap<ObjectRef, BTreeSet<u32>> = BTreeMap::new();
    for &r in &first_page_set {
        referenced_pages.entry(r).or_default().insert(0);
    }
    for (page_idx, &page_ref) in page_refs.iter().enumerate().skip(1) {
        for r in compute_closure(pdf, page_ref, &live, &resurrectable)? {
            referenced_pages
                .entry(r)
                .or_default()
                .insert(page_idx as u32);
        }
    }

    Ok(containers
        .iter()
        .map(|members| {
            // in_outlines is checked first (QPDF_linearization.cc:1118-1122).
            if !outline_set.is_empty() && members.iter().any(|m| outline_set.contains(m)) {
                return if outlines_first_page {
                    ContainerPart::FirstPage
                } else {
                    ContainerPart::Rest
                };
            }
            // in_open_document takes precedence over every page category.
            if members.iter().any(|m| open_doc_set.contains(m)) {
                return ContainerPart::OpenDocument;
            }
            if members.iter().any(|m| first_page_set.contains(m)) {
                return ContainerPart::FirstPage;
            }
            let mut other_pages: BTreeSet<u32> = BTreeSet::new();
            for m in members {
                if let Some(pages) = referenced_pages.get(m) {
                    other_pages.extend(pages.iter().copied().filter(|&p| p != 0));
                }
            }
            match other_pages.len() {
                0 => ContainerPart::Rest,
                1 => ContainerPart::OtherPagePrivate,
                _ => ContainerPart::OtherPageShared,
            }
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::MAX_INLINE_DEPTH;
    use std::io::Cursor;

    // -----------------------------------------------------------------------
    // Inline-depth guard
    // -----------------------------------------------------------------------

    fn nested_arrays(depth: usize) -> Object {
        let mut o = Object::Null;
        for _ in 0..depth {
            o = Object::Array(vec![o]);
        }
        o
    }

    #[test]
    fn collect_direct_refs_errors_on_excessive_nesting() {
        let mut out = Vec::new();
        let err = collect_direct_refs(&nested_arrays(MAX_INLINE_DEPTH + 5), 0, &mut out);
        assert!(matches!(err, Err(crate::Error::Unsupported(_))));
    }

    #[test]
    fn collect_direct_refs_accepts_nesting_up_to_the_limit() {
        let mut out = Vec::new();
        // Bury one Reference so it is visited at exactly inline depth
        // MAX_INLINE_DEPTH (the deepest accepted level under the strict `>`
        // guard); it must be collected, not errored.
        let mut o = Object::Array(vec![Object::Reference(ObjectRef::new(4, 0))]);
        for _ in 0..(MAX_INLINE_DEPTH - 1) {
            o = Object::Array(vec![o]);
        }
        collect_direct_refs(&o, 0, &mut out).unwrap();
        assert_eq!(out, vec![ObjectRef::new(4, 0)]);
    }

    #[test]
    fn collect_direct_refs_with_context_errors_on_excessive_nesting() {
        let mut out = Vec::new();
        let err = collect_direct_refs_with_context(
            &nested_arrays(MAX_INLINE_DEPTH + 5),
            0,
            false,
            &mut out,
        );
        assert!(matches!(err, Err(crate::Error::Unsupported(_))));
    }

    // -----------------------------------------------------------------------
    // Fixture builders
    // -----------------------------------------------------------------------

    /// Build a minimal single-page PDF in memory.
    ///
    /// Object layout:
    ///   1 0 obj – Catalog   (/Root)
    ///   2 0 obj – Pages node
    ///   3 0 obj – Page dict  (Kids[0])
    fn tiny_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        // Object 1: Catalog
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        // Object 2: Pages
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        // Object 3: Page
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        // xref table
        let xref_start = pdf.len() as u64;
        let xref_section = format!(
            "xref\n0 4\n0000000000 65535 f \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \n",
            off1, off2, off3,
        );
        pdf.extend_from_slice(xref_section.as_bytes());

        // Trailer
        let trailer = format!(
            "trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            xref_start,
        );
        pdf.extend_from_slice(trailer.as_bytes());

        pdf
    }

    fn open_tiny_pdf() -> Pdf<Cursor<Vec<u8>>> {
        let bytes = tiny_pdf_bytes();
        Pdf::open(Cursor::new(bytes)).expect("tiny PDF should parse")
    }

    /// Build a two-page PDF with a shared font.
    ///
    /// Object layout:
    ///   1 0 obj – Catalog
    ///   2 0 obj – Pages node  (Kids: [3 0 R, 4 0 R])
    ///   3 0 obj – Page 1 dict  → references 5 0 R (Resources), 6 0 R (Contents)
    ///   4 0 obj – Page 2 dict  → references 5 0 R (Resources), 7 0 R (Contents)
    ///   5 0 obj – Resources dict  → /Font << /F1 8 0 R >>
    ///   6 0 obj – Content stream (page 1 only)
    ///   7 0 obj – Content stream (page 2 only)
    ///   8 0 obj – Font dict (shared by both pages via Resources)
    fn two_page_shared_font_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources 5 0 R /Contents 6 0 R >>\nendobj\n");

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources 5 0 R /Contents 7 0 R >>\nendobj\n");

        // Shared resources (font dict)
        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(b"5 0 obj\n<< /Font << /F1 8 0 R >> >>\nendobj\n");

        // Page 1 content stream
        let off6 = pdf.len() as u64;
        pdf.extend_from_slice(b"6 0 obj\n<< /Length 5 >>\nstream\nBT ET\nendstream\nendobj\n");

        // Page 2 content stream
        let off7 = pdf.len() as u64;
        pdf.extend_from_slice(b"7 0 obj\n<< /Length 5 >>\nstream\nBT ET\nendstream\nendobj\n");

        // Font (shared)
        let off8 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"8 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        let xref_section = format!(
            "xref\n0 9\n\
            0000000000 65535 f \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n",
            off1, off2, off3, off4, off5, off6, off7, off8,
        );
        pdf.extend_from_slice(xref_section.as_bytes());

        let trailer = format!(
            "trailer\n<< /Size 9 /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            xref_start,
        );
        pdf.extend_from_slice(trailer.as_bytes());

        pdf
    }

    fn open_two_page_shared_font() -> Pdf<Cursor<Vec<u8>>> {
        let bytes = two_page_shared_font_bytes();
        Pdf::open(Cursor::new(bytes)).expect("two-page PDF should parse")
    }

    /// Build a two-page PDF whose shared font is ALSO referenced by an outline
    /// item (via a non-standard `/Extra` key), reproducing flpdf-q2zw in miniature.
    /// With `use_outlines`, the catalog also sets `/PageMode /UseOutlines` so the
    /// outline section routes to part6 (first half) instead of part9.
    ///
    /// Object layout (extends [`two_page_shared_font_bytes`]):
    ///   1 0 obj – Catalog  (/Pages 2 0 R, /Outlines 9 0 R [, /PageMode /UseOutlines])
    ///   2 0 obj – Pages node  (Kids: [3 0 R, 4 0 R])
    ///   3 0 obj – Page 1  → /Resources 5 0 R, /Contents 6 0 R
    ///   4 0 obj – Page 2  → /Resources 5 0 R, /Contents 7 0 R
    ///   5 0 obj – Resources  → /Font << /F1 8 0 R >>
    ///   6 0 obj – Content stream (page 1 only)
    ///   7 0 obj – Content stream (page 2 only)
    ///   8 0 obj – Font  (shared by both pages AND reached from the outline)
    ///   9 0 obj – Outlines dict  (/First 10 0 R, /Last 10 0 R)
    ///  10 0 obj – Outline item  (/Extra 8 0 R → the font)
    fn two_page_shared_font_outline_ref_bytes(use_outlines: bool) -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        if use_outlines {
            pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Outlines 9 0 R /PageMode /UseOutlines >>\nendobj\n");
        } else {
            pdf.extend_from_slice(
                b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Outlines 9 0 R >>\nendobj\n",
            );
        }

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources 5 0 R /Contents 6 0 R >>\nendobj\n");

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources 5 0 R /Contents 7 0 R >>\nendobj\n");

        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(b"5 0 obj\n<< /Font << /F1 8 0 R >> >>\nendobj\n");

        let off6 = pdf.len() as u64;
        pdf.extend_from_slice(b"6 0 obj\n<< /Length 5 >>\nstream\nBT ET\nendstream\nendobj\n");

        let off7 = pdf.len() as u64;
        pdf.extend_from_slice(b"7 0 obj\n<< /Length 5 >>\nstream\nBT ET\nendstream\nendobj\n");

        let off8 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"8 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
        );

        let off9 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"9 0 obj\n<< /Type /Outlines /First 10 0 R /Last 10 0 R /Count 1 >>\nendobj\n",
        );

        let off10 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"10 0 obj\n<< /Title (Item) /Parent 9 0 R /Extra 8 0 R >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        let xref_section = format!(
            "xref\n0 11\n\
            0000000000 65535 f \n\
            {off1:010} 00000 n \n\
            {off2:010} 00000 n \n\
            {off3:010} 00000 n \n\
            {off4:010} 00000 n \n\
            {off5:010} 00000 n \n\
            {off6:010} 00000 n \n\
            {off7:010} 00000 n \n\
            {off8:010} 00000 n \n\
            {off9:010} 00000 n \n\
            {off10:010} 00000 n \n",
        );
        pdf.extend_from_slice(xref_section.as_bytes());

        let trailer =
            format!("trailer\n<< /Size 11 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n",);
        pdf.extend_from_slice(trailer.as_bytes());

        pdf
    }

    fn open_two_page_shared_font_outline_ref() -> Pdf<Cursor<Vec<u8>>> {
        let bytes = two_page_shared_font_outline_ref_bytes(false);
        Pdf::open(Cursor::new(bytes)).expect("two-page outline-ref PDF should parse")
    }

    fn open_two_page_shared_font_outline_ref_use_outlines() -> Pdf<Cursor<Vec<u8>>> {
        let bytes = two_page_shared_font_outline_ref_bytes(true);
        Pdf::open(Cursor::new(bytes)).expect("two-page UseOutlines outline-ref PDF should parse")
    }

    /// Build a PDF where the page's Resources dictionary references objects in
    /// a cycle: A → B → A (both are XObject-style objects hanging off /Resources).
    ///
    /// Object layout:
    ///   1 0 obj – Catalog
    ///   2 0 obj – Pages node
    ///   3 0 obj – Page dict  → /Resources 4 0 R
    ///   4 0 obj – Resources  → /XObject << /ImA 5 0 R >>
    ///   5 0 obj – XObject A  → /SomeRef 6 0 R
    ///   6 0 obj – XObject B  → /SomeRef 5 0 R   (cycle: B → A)
    fn cycle_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources 4 0 R >>\nendobj\n",
        );

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(b"4 0 obj\n<< /XObject << /ImA 5 0 R >> >>\nendobj\n");

        // XObject A -> references B
        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(b"5 0 obj\n<< /SomeRef 6 0 R >>\nendobj\n");

        // XObject B -> references A (cycle)
        let off6 = pdf.len() as u64;
        pdf.extend_from_slice(b"6 0 obj\n<< /SomeRef 5 0 R >>\nendobj\n");

        let xref_start = pdf.len() as u64;
        let xref_section = format!(
            "xref\n0 7\n\
            0000000000 65535 f \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n",
            off1, off2, off3, off4, off5, off6,
        );
        pdf.extend_from_slice(xref_section.as_bytes());

        let trailer = format!(
            "trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            xref_start,
        );
        pdf.extend_from_slice(trailer.as_bytes());

        pdf
    }

    fn open_cycle_pdf() -> Pdf<Cursor<Vec<u8>>> {
        let bytes = cycle_pdf_bytes();
        Pdf::open(Cursor::new(bytes)).expect("cycle PDF should parse")
    }

    // -----------------------------------------------------------------------
    // 1. from_pdf does not panic on a well-formed document
    // -----------------------------------------------------------------------
    #[test]
    fn from_pdf_does_not_panic() {
        let mut pdf = open_tiny_pdf();
        let plan =
            LinearizationPlan::from_pdf(&mut pdf, false).expect("plan construction must succeed");
        assert!(plan.total_object_count > 0);
    }

    // -----------------------------------------------------------------------
    // 2. Struct fields have expected types / accessors
    // -----------------------------------------------------------------------
    #[test]
    fn plan_fields_accessible() {
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();

        // root_ref should be Some(1 0 R)
        assert_eq!(plan.root_ref, Some(ObjectRef::new(1, 0)));

        // page_hints should have exactly 1 entry
        assert_eq!(plan.page_hints.len(), 1);
        assert_eq!(plan.page_hints[0].page_ref, ObjectRef::new(3, 0));
        assert_eq!(plan.page_hints[0].first_object_index, 0);
        assert_eq!(plan.page_hints[0].byte_length, 0); // placeholder
    }

    // -----------------------------------------------------------------------
    // 3. Single-page fixture: Part 2 non-empty, Part 3 empty
    // -----------------------------------------------------------------------
    #[test]
    fn single_page_part2_non_empty_part3_empty() {
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();

        // Part 2 must contain the page dict (object 3) and the Pages node
        // (reached via /Parent).
        assert!(
            !plan.part2_objects.is_empty(),
            "Part 2 must contain first-page objects"
        );

        // With a single page, nothing is shared → Part 3 must be empty.
        assert!(
            plan.part3_objects.is_empty(),
            "Part 3 must be empty for a single-page document"
        );

        // The page object (3 0 R) must be in Part 2.
        let page_ref = ObjectRef::new(3, 0);
        assert!(
            plan.part2_objects.contains(&page_ref),
            "page dict must be in Part 2"
        );

        // Part 1 stays empty (populated by a later subtask).
        assert!(plan.part1_objects.is_empty());

        // qpdf always populates shared_hints with all first-page section
        // objects (Part 2 + Part 3), even for single-page PDFs.
        // shared_hints = part2_objects (Part 3 is empty for single-page).
        assert_eq!(
            plan.shared_hints.len(),
            plan.part2_objects.len(),
            "shared_hints must contain all part2 objects for single-page PDF"
        );
        for hint in &plan.shared_hints {
            assert!(
                plan.part2_objects.contains(&hint.object_ref),
                "single-page shared_hint {:?} must be a Part-2 object",
                hint.object_ref
            );
            assert!(
                hint.referencing_pages.is_empty(),
                "Part-2 shared hint must have empty referencing_pages (page 0 owns by layout)"
            );
        }
    }

    /// One-page PDF whose Catalog references a first-page object (the Font, obj
    /// 5) via a non-open-document key (/Ref2), mirroring the
    /// `catalog-firstpage-shared-one-page.pdf` byte fixture. The page reaches the
    /// same Font through /Resources -> obj 4 -> /F1.
    fn catalog_firstpage_shared_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let mut offs = [0u64; 7];
        let mut push = |pdf: &mut Vec<u8>, n: usize, body: &str| {
            offs[n] = pdf.len() as u64;
            pdf.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
        };
        push(&mut pdf, 1, "<< /Type /Catalog /Pages 2 0 R /Ref2 5 0 R >>");
        push(&mut pdf, 2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push(
            &mut pdf,
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font 4 0 R >> /Contents 6 0 R >>",
        );
        push(&mut pdf, 4, "<< /F1 5 0 R >>");
        push(
            &mut pdf,
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        );
        push(&mut pdf, 6, "<< /Length 2 >>\nstream\nBT\nendstream");
        let xref_start = pdf.len() as u64;
        let mut xref = String::from("xref\n0 7\n0000000000 65535 f \n");
        for off in offs.iter().skip(1) {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        pdf.extend_from_slice(xref.as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    // Non-gated integration guard for the Step-5 document-`others` route: the
    // Font reached from the Catalog /Ref2 key must land in Part 3 (shared), not
    // Part 2 (private), so it sorts after the first-page-private Content. This
    // pins the classification independently of the qpdf-zlib-compat byte tests.
    #[test]
    fn document_other_ref_routes_first_page_object_to_part3() {
        let mut pdf = Pdf::open(Cursor::new(catalog_firstpage_shared_pdf_bytes())).unwrap();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();
        let font = ObjectRef::new(5, 0); // shared via Catalog /Ref2
        assert!(
            plan.part3_objects.contains(&font),
            "Font reached via Catalog /Ref2 must be Part 3 (lc_first_page_shared); part3={:?}",
            plan.part3_objects
        );
        assert!(
            !plan.part2_objects.contains(&font),
            "the shared Font must not also be in Part 2"
        );
        // The page dict, its /Resources Font dict (obj 4), and the Content stream
        // (obj 6) stay first-page-private (Part 2).
        for r in [
            ObjectRef::new(3, 0),
            ObjectRef::new(4, 0),
            ObjectRef::new(6, 0),
        ] {
            assert!(
                plan.part2_objects.contains(&r),
                "first-page-private object {r:?} must be in Part 2"
            );
        }
    }

    // -----------------------------------------------------------------------
    // 4. Part 4 receives objects not in Part 2 or 3
    // -----------------------------------------------------------------------
    #[test]
    fn part4_contains_only_remaining_objects() {
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();

        // No object should appear in both Part 4 and Part 2/3.
        let part4_set: BTreeSet<_> = plan.part4_objects().into_iter().collect();
        for r in &plan.part2_objects {
            assert!(
                !part4_set.contains(r),
                "Part-2 object {r} must not appear in Part 4"
            );
        }
        for r in &plan.part3_objects {
            assert!(
                !part4_set.contains(r),
                "Part-3 object {r} must not appear in Part 4"
            );
        }
    }

    // -----------------------------------------------------------------------
    // 5. Disjoint invariant holds after from_pdf
    // -----------------------------------------------------------------------
    #[test]
    fn parts_are_disjoint_after_closure() {
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();
        assert!(
            plan.parts_are_disjoint(),
            "object refs must appear in at most one part"
        );
    }

    // -----------------------------------------------------------------------
    // 6. Two-page, shared font: font in Part 3; page-1 content in Part 2;
    //    page-2 content in Part 4.
    // -----------------------------------------------------------------------
    #[test]
    fn two_page_shared_font_partitioned_correctly() {
        let mut pdf = open_two_page_shared_font();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();

        // Resources (5 0 R) and Font (8 0 R) are shared → Part 3.
        let shared_ref_resources = ObjectRef::new(5, 0);
        let shared_ref_font = ObjectRef::new(8, 0);
        assert!(
            plan.part3_objects.contains(&shared_ref_resources),
            "shared Resources dict must be in Part 3"
        );
        assert!(
            plan.part3_objects.contains(&shared_ref_font),
            "shared Font must be in Part 3"
        );

        // Page-1 content stream (6 0 R) is exclusive to page 1 → Part 2.
        let page1_content = ObjectRef::new(6, 0);
        assert!(
            plan.part2_objects.contains(&page1_content),
            "page-1-only content stream must be in Part 2"
        );

        // Page-2 content stream (7 0 R) is only reachable from page 2 → Part 4.
        let page2_content = ObjectRef::new(7, 0);
        assert!(
            plan.part4_objects().contains(&page2_content),
            "page-2-only content stream must be in Part 4"
        );

        // Disjoint invariant must hold.
        assert!(plan.parts_are_disjoint());
    }

    // -----------------------------------------------------------------------
    // 6b. Outline-referenced shared font: in_outlines outranks in_first_page,
    //     so a font referenced by BOTH pages AND an outline item is lc_outlines
    //     (part9, !UseOutlines) — NOT first-page-shared (part3). qpdf
    //     categorization QPDF_linearization.cc:1120-1126 (flpdf-q2zw).
    // -----------------------------------------------------------------------
    #[test]
    fn outline_referenced_shared_font_routes_to_outline_section_not_first_page() {
        let mut pdf = open_two_page_shared_font_outline_ref();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();

        let font = ObjectRef::new(8, 0);
        let outline_root = ObjectRef::new(9, 0);
        let outline_item = ObjectRef::new(10, 0);

        // The font is reached from page 0, page 1, AND the outline. in_outlines
        // wins, so it must be in the outline section, peeled out of the first-page
        // section (part2/part3) and never captured as other-page-private (part7).
        assert!(
            plan.part9_outline_objects.contains(&font),
            "outline-referenced font must be in part9_outline_objects, got {:?}",
            plan.part9_outline_objects
        );
        assert!(
            !plan.part2_objects.contains(&font) && !plan.part3_objects.contains(&font),
            "outline-referenced font must be peeled out of the first-page section"
        );
        assert!(
            !plan.part4_other_pages_private.contains(&font),
            "outline-referenced font must not be captured as other-page-private"
        );

        // pushOutlinesToPart emits the root first, then the remaining outline
        // objects in ascending source-object-number order
        // (QPDF_linearization.cc:1426-1431): [root 9] ++ [font 8, item 10].
        assert_eq!(
            plan.part9_outline_objects,
            vec![outline_root, font, outline_item],
            "outline order must be [root] ++ ascending source number"
        );

        assert!(plan.parts_are_disjoint());
    }

    // -----------------------------------------------------------------------
    // 6c. Symmetric UseOutlines case: the same page+outline-shared font flows
    //     through the part6 (first-half) outline branch instead of part9. qpdf
    //     routes lc_outlines to part6 under /PageMode /UseOutlines
    //     (QPDF_linearization.cc:1214-1216) and counts those objects toward
    //     page 0 (line 1222). Byte parity for this combination needs a dedicated
    //     fixture (filed as a follow-up); this pins flpdf's internal accounting.
    // -----------------------------------------------------------------------
    #[test]
    fn outline_referenced_shared_font_routes_to_part6_under_use_outlines() {
        let mut pdf = open_two_page_shared_font_outline_ref_use_outlines();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();

        let font = ObjectRef::new(8, 0);
        let outline_root = ObjectRef::new(9, 0);
        let outline_item = ObjectRef::new(10, 0);

        // /UseOutlines → outline objects (including the page-shared font) move to
        // the first-half part6 outline section, not part9.
        assert!(
            plan.part6_outline_objects.contains(&font),
            "UseOutlines: outline-referenced font must be in part6_outline_objects, got {:?}",
            plan.part6_outline_objects
        );
        assert!(
            plan.part9_outline_objects.is_empty(),
            "UseOutlines: part9 outlines must be empty"
        );
        assert_eq!(
            plan.part6_outline_objects,
            vec![outline_root, font, outline_item],
            "UseOutlines outline order must be [root] ++ ascending source number"
        );
        assert!(
            !plan.part2_objects.contains(&font) && !plan.part3_objects.contains(&font),
            "outline-referenced font must be peeled out of the first-page section"
        );

        // part6 outline objects are emitted before /E and count toward page 0
        // (QPDF_linearization.cc:1222): object_count = part2 + part3 + part6_outline.
        let expected_page0 = (plan.part2_objects.len()
            + plan.part3_objects.len()
            + plan.part6_outline_objects.len()) as u32;
        assert_eq!(
            plan.page_hints[0].object_count, expected_page0,
            "page-0 object_count must include the part6 outline objects"
        );

        assert!(plan.parts_are_disjoint());
    }

    // -----------------------------------------------------------------------
    // 7. Shared font hint entries include page 1 but NOT page 0.
    //
    //    Per qpdf's Annex F layout, the shared hint table starts with all
    //    first-page section (part2) objects (referencing_pages = []) followed
    //    by the truly shared (part3) objects that carry cross-page references.
    //    Part3 entries must reference pages 1..N that use them; page 0 owns
    //    them by physical position so it must NOT appear in referencing_pages.
    // -----------------------------------------------------------------------
    #[test]
    fn shared_hints_reference_correct_pages() {
        let mut pdf = open_two_page_shared_font();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();

        let part2_len = plan.part2_objects.len();

        // Part2 entries (indices 0..part2_len) must have empty referencing_pages.
        for hint in &plan.shared_hints[..part2_len] {
            assert!(
                hint.referencing_pages.is_empty(),
                "part2 shared hint for {} must have empty referencing_pages",
                hint.object_ref
            );
        }

        // Part3 entries (indices part2_len..) must reference page 1 but NOT page 0.
        for hint in &plan.shared_hints[part2_len..] {
            assert!(
                !hint.referencing_pages.contains(&0),
                "part3 shared hint for {} must NOT reference page 0 (physical ownership via first-page section)",
                hint.object_ref
            );
            assert!(
                hint.referencing_pages.contains(&1),
                "part3 shared hint for {} must reference page 1 (second page)",
                hint.object_ref
            );
        }
    }

    // -----------------------------------------------------------------------
    // 8. Cycle fixture: BFS must terminate without panicking.
    // -----------------------------------------------------------------------
    #[test]
    fn cycle_does_not_loop_forever() {
        let mut pdf = open_cycle_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false)
            .expect("cycle PDF must not cause infinite loop");

        // Basic sanity: we got a plan with objects in Part 2.
        assert!(!plan.part2_objects.is_empty(), "Part 2 must be non-empty");

        // The cyclic XObjects (5 0 R and 6 0 R) must each appear in exactly
        // one part (BFS visited-set prevents duplication).
        assert!(plan.parts_are_disjoint());

        let xobj_a = ObjectRef::new(5, 0);
        let xobj_b = ObjectRef::new(6, 0);
        let in_part2_a = plan.part2_objects.contains(&xobj_a);
        let in_part2_b = plan.part2_objects.contains(&xobj_b);
        // Both should be reachable from page 1 (single page → no sharing).
        assert!(in_part2_a, "XObject A must be in Part 2");
        assert!(in_part2_b, "XObject B must be in Part 2");
    }

    // -----------------------------------------------------------------------
    // 8b. First-page closure /Resources DFS — defensive branch coverage.
    //
    // The page-leaf `/Resources` subtree walk (a) dedups via the visited set
    // when the same object is reached twice before it is popped, and (b) stops
    // at any page-tree node a resource value cross-links to, so it never pulls
    // in sibling pages. These fixtures exercise both guards directly.
    // -----------------------------------------------------------------------

    /// Page whose inline `/Resources` lists the SAME object ref twice, so the
    /// DFS stack holds two copies of obj 4 before either is popped — exercising
    /// the `visited.insert` dedup `continue` in the subtree walk.
    fn resources_duplicate_ref_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]               /Resources << /A 4 0 R /B 4 0 R >> >>\nendobj\n",
        );
        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(b"4 0 obj\n<< /Font /Helvetica >>\nendobj\n");
        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(
            format!(
                "xref\n0 5\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n                 {off3:010} 00000 n \n{off4:010} 00000 n \n"
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    /// Page whose `/Resources` value cross-links back to the `/Pages` node, so
    /// the subtree walk must STOP there (the `is_page_tree_node` guard) instead
    /// of pulling the page tree into the first-page closure.
    fn resources_crosslink_page_tree_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let off3 = pdf.len() as u64;
        // /Resources references obj 2 (the Pages node) — a malformed cross-link.
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]               /Resources << /Bad 2 0 R >> >>\nendobj\n",
        );
        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(
            format!(
                "xref\n0 4\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n                 {off3:010} 00000 n \n"
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn resources_subtree_dedups_duplicate_ref() {
        let mut pdf = Pdf::open(Cursor::new(resources_duplicate_ref_pdf_bytes()))
            .expect("duplicate-ref PDF must parse");
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        // The doubly-referenced resource object appears exactly once across all
        // parts (the visited-set dedup prevents a duplicate).
        assert!(plan.parts_are_disjoint());
        let res = ObjectRef::new(4, 0);
        assert!(
            plan.part2_objects.contains(&res),
            "the resource object must be in the first-page section exactly once"
        );
        assert_eq!(
            plan.part2_objects.iter().filter(|r| **r == res).count(),
            1,
            "duplicate /Resources ref must not duplicate the object in Part 2"
        );
    }

    #[test]
    fn resources_subtree_stops_at_page_tree_crosslink() {
        let mut pdf = Pdf::open(Cursor::new(resources_crosslink_page_tree_pdf_bytes()))
            .expect("crosslink PDF must parse");
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        // The walk reaches the /Pages node via the bad /Resources link but does
        // not descend into it (no sibling-page content pulled in); the plan is
        // still well-formed and disjoint.
        assert!(plan.parts_are_disjoint());
        // The page itself is the first-page anchor.
        assert!(
            plan.part2_objects.contains(&ObjectRef::new(3, 0)),
            "the page leaf must anchor the first-page section"
        );
        // The cross-linked /Pages node (obj 2) must be EXCLUDED from the
        // first-page closure entirely — it is a page-tree boundary, so it is
        // kept in `visited` but neither descended into nor added to Part 2/3.
        // (Before the boundary check moved ahead of `order.push`, the node was
        // wrongly pulled into Part 2.)
        let pages_node = ObjectRef::new(2, 0);
        assert!(
            !plan.part2_objects.contains(&pages_node) && !plan.part3_objects.contains(&pages_node),
            "the cross-linked /Pages node must not be pulled into the first-page section"
        );
    }

    // -----------------------------------------------------------------------
    // 9. Page-1 hint entry has correct object_count after closure.
    //
    //    object_count[0] must include both Part-2 (page-0 private) and
    //    Part-3 (shared) objects, because both live in the first-page section
    //    (before /E) in the linearized file layout.
    // -----------------------------------------------------------------------
    #[test]
    fn page1_hint_object_count_matches_part2_plus_part3() {
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();

        assert_eq!(
            plan.page_hints[0].object_count,
            (plan.part2_objects.len() + plan.part3_objects.len()) as u32,
            "page-0 hint object_count must match Part-2 + Part-3 length (all objects in first-page section)"
        );
    }

    // -----------------------------------------------------------------------
    // 10. Hint table inputs are well-formed even when populated only by placeholders
    // -----------------------------------------------------------------------
    #[test]
    fn hint_table_inputs_well_formed_empty() {
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();

        // Each PageHintEntry must reference a non-zero object number.
        for entry in &plan.page_hints {
            assert_ne!(entry.page_ref.number, 0);
        }

        // SharedObjectHintEntry objects must be non-zero.
        for entry in &plan.shared_hints {
            assert_ne!(entry.object_ref.number, 0);
        }
    }

    // -----------------------------------------------------------------------
    // Multi-level /Parent inheritance: a /Resources attached to the grandparent
    // /Pages node must end up in the page's closure (so it is partitioned as a
    // shared object or first-page private, not stranded in part4_rest).
    // -----------------------------------------------------------------------

    fn two_level_pages_inherited_resources_bytes() -> Vec<u8> {
        // Object layout:
        //   1 0 obj — Catalog
        //   2 0 obj — Outer /Pages (Kids [3 0 R]) with inherited /Resources 6 0 R
        //   3 0 obj — Inner /Pages (Parent 2 0 R, Kids [4 0 R, 5 0 R])
        //   4 0 obj — Page 1 (Parent 3 0 R) — NO own /Resources, inherits 6 0 R
        //   5 0 obj — Page 2 (Parent 3 0 R) — NO own /Resources, inherits 6 0 R
        //   6 0 obj — Shared /Resources (inherited via the outer /Pages)
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 2 /Resources 6 0 R >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [4 0 R 5 0 R] /Count 2 >>\nendobj\n",
        );

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<< /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"5 0 obj\n<< /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let off6 = pdf.len() as u64;
        pdf.extend_from_slice(b"6 0 obj\n<< /Font << /F1 7 0 R >> >>\nendobj\n");

        // Font referenced by the inherited Resources, so the closure walker
        // also has a deeper reachable object beyond the grandparent.
        let off7 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"7 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        let xref_section = format!(
            "xref\n0 8\n\
            0000000000 65535 f \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n",
            off1, off2, off3, off4, off5, off6, off7,
        );
        pdf.extend_from_slice(xref_section.as_bytes());
        let trailer = format!(
            "trailer\n<< /Size 8 /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            xref_start,
        );
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    /// Without ancestor-walking, the shared /Resources attached to the outer
    /// /Pages (2 0 R) would never appear in any page's closure: the leaf page
    /// (4 0 R) only walks one level up to 3 0 R, which has no /Resources of
    /// its own. The resource (6 0 R) and its referenced font (7 0 R) would
    /// then fall into part4_rest, causing qpdf-divergent renumbering.
    ///
    /// With the fix, both pages reach the inherited resource and font, so
    /// the resource and font are classified as Part-3 (shared between both
    /// pages) — not stranded in part4_rest.
    #[test]
    fn multilevel_pages_inherited_resources_join_page_closure() {
        let bytes = two_level_pages_inherited_resources_bytes();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("multi-level Pages PDF should parse");
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();

        let resources_ref = ObjectRef::new(6, 0);
        let font_ref = ObjectRef::new(7, 0);
        let all_refs = plan.all_assigned_refs();
        assert!(
            all_refs.contains(&resources_ref),
            "inherited /Resources (6 0 R) must be reachable from page closures, \
             not stranded outside every part"
        );
        assert!(
            all_refs.contains(&font_ref),
            "/Font (7 0 R) reached via inherited /Resources must be classified"
        );
        // The /Resources is referenced by both pages, so it is a Part-3 (shared)
        // object — not in part4_rest where the pre-fix code would have placed it.
        assert!(
            plan.part3_objects.contains(&resources_ref),
            "shared inherited /Resources must end up in Part 3"
        );
        assert!(
            !plan.part4_rest.contains(&resources_ref),
            "shared inherited /Resources must NOT end up in part4_rest"
        );
    }

    // -----------------------------------------------------------------------
    // flpdf-ws2: compute_closure's /Parent-chain walk must propagate
    // pdf.resolve errors instead of swallowing them.
    // -----------------------------------------------------------------------

    /// Build a single-page PDF whose page `/Parent` points at a stream object
    /// with a `/Length` that overshoots its payload.
    ///
    /// `page_refs` walks `/Kids` downward only, so it never resolves object 4;
    /// the only code path that resolves it is the `/Parent`-chain walk inside
    /// `compute_closure`. Resolving object 4 yields a genuine parse error
    /// (not `Ok(Null)`), so a correct walker must surface that error.
    fn page_parent_resolve_error_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        // Page leaf: /Parent deliberately points at the malformed object 4,
        // not the Pages node. page_refs collects this page via /Kids without
        // ever touching /Parent, so object 4 is resolved exclusively by
        // compute_closure's ancestor walk.
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 4 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        // Object 4: a stream whose /Length (9) overshoots the 2-byte payload,
        // so parse_indirect_object_detailed rejects it and resolve returns Err.
        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(b"4 0 obj\n<< /Length 9 >>\nstream\nab\nendstream\nendobj\n");

        let xref_start = pdf.len() as u64;
        let xref_section = format!(
            "xref\n0 5\n\
            0000000000 65535 f \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n",
            off1, off2, off3, off4,
        );
        pdf.extend_from_slice(xref_section.as_bytes());

        let trailer = format!(
            "trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            xref_start,
        );
        pdf.extend_from_slice(trailer.as_bytes());

        pdf
    }

    /// `compute_closure`'s `/Parent`-chain walk must
    /// propagate `pdf.resolve` errors rather than swallowing them with
    /// `let Ok(..) else { continue }`. A swallowed error lets `from_pdf`
    /// return a degraded plan (truncated closure / hint tables) for a
    /// malformed document, which a downstream writer would then emit as an
    /// invalid linearized PDF.
    #[test]
    fn from_pdf_propagates_parent_chain_resolve_error() {
        let bytes = page_parent_resolve_error_bytes();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("fixture xref/trailer must parse");
        let result = LinearizationPlan::from_pdf(&mut pdf, false);
        assert!(
            result.is_err(),
            "from_pdf must propagate a /Parent-chain resolve error, got Ok"
        );
    }

    /// Build a single-page PDF whose page `/Parent` indirects through a plain
    /// reference object (`4 0 obj  5 0 R  endobj`) before reaching the real
    /// ancestor `/Pages` node (object 5), which carries an inherited
    /// `/Resources`.
    ///
    /// PDF allows an indirect object to hold a bare reference, so `resolve`
    /// can legitimately return `Object::Reference`. The `/Parent` walk must
    /// follow that chain — exactly as the main BFS loop does via
    /// `collect_direct_refs` — or the inherited resource is silently dropped
    /// from the closure.
    fn reference_chain_parent_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        // Real page tree used by page_refs (walks /Kids downward only).
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        // Page leaf: /Parent points at object 4, a reference-chain hop.
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 4 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        // Object 4: a plain indirect reference to object 5 — resolve(4 0 R)
        // returns Object::Reference(5 0 R), not a dictionary.
        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(b"4 0 obj\n5 0 R\nendobj\n");

        // Object 5: the real ancestor /Pages node, carrying inherited
        // /Resources that must join the page closure once the chain is walked.
        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(b"5 0 obj\n<< /Type /Pages /Resources 6 0 R >>\nendobj\n");

        let off6 = pdf.len() as u64;
        pdf.extend_from_slice(b"6 0 obj\n<< /Font << /F1 7 0 R >> >>\nendobj\n");

        let off7 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"7 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        let xref_section = format!(
            "xref\n0 8\n\
            0000000000 65535 f \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n",
            off1, off2, off3, off4, off5, off6, off7,
        );
        pdf.extend_from_slice(xref_section.as_bytes());

        let trailer = format!(
            "trailer\n<< /Size 8 /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            xref_start,
        );
        pdf.extend_from_slice(trailer.as_bytes());

        pdf
    }

    /// The `/Parent`-chain walk must follow a parent
    /// that resolves to a bare `Object::Reference`, mirroring the main BFS
    /// loop. Otherwise inherited resources reached through a reference-chain
    /// `/Parent` are silently stranded outside the page closure.
    #[test]
    fn from_pdf_follows_reference_chain_parent() {
        let bytes = reference_chain_parent_bytes();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("fixture must parse");
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();

        // Single-page document: the page's whole closure is Part 2.
        let resources_ref = ObjectRef::new(6, 0);
        let font_ref = ObjectRef::new(7, 0);
        assert!(
            plan.part2_objects.contains(&resources_ref),
            "/Resources reached through a reference-chain /Parent must join the page closure"
        );
        assert!(
            plan.part2_objects.contains(&font_ref),
            "/Font reached transitively via the inherited /Resources must join the closure"
        );
    }

    // -----------------------------------------------------------------------
    // 12. Three-page PDF where pages 2 and 3 share a content stream.
    //
    // This tests that objects referenced by 2+ pages but NOT by page 1
    // (i.e., part4_other_pages_shared) are included in shared_hints.
    //
    // Object layout:
    //   1 0 obj – Catalog
    //   2 0 obj – Pages node  (Kids: [3, 4, 5])
    //   3 0 obj – Page 1 dict (unique content 6 0 R, resources 7 0 R)
    //   4 0 obj – Page 2 dict (shared content 8 0 R, resources 7 0 R)
    //   5 0 obj – Page 3 dict (shared content 8 0 R, resources 7 0 R)
    //   6 0 obj – Content stream (page 1 only)
    //   7 0 obj – Resources dict (shared by pages 1, 2, 3 → Part 3)
    //   8 0 obj – Content stream shared by pages 2 and 3 (NOT page 1)
    //            → part4_other_pages_shared
    //
    // Expected:
    //   Part 2: [3 0 R, 6 0 R, ...] (page 1 exclusive)
    //   Part 3: [7 0 R]             (first-page shared: resources)
    //   part4_other_pages_shared: [8 0 R]  (non-first-page shared)
    //   shared_hints contains 8 0 R with referencing_pages = [1, 2] or [2, 3]
    //   depending on 0-based page indices.
    // -----------------------------------------------------------------------

    fn three_page_shared_content_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources 7 0 R /Contents 6 0 R >>\nendobj\n");

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources 7 0 R /Contents 8 0 R >>\nendobj\n");

        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(b"5 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources 7 0 R /Contents 8 0 R >>\nendobj\n");

        // Page 1 content stream (unique)
        let off6 = pdf.len() as u64;
        pdf.extend_from_slice(b"6 0 obj\n<< /Length 5 >>\nstream\nBT ET\nendstream\nendobj\n");

        // Shared resources (pages 1, 2, 3) → Part 3
        let off7 = pdf.len() as u64;
        pdf.extend_from_slice(b"7 0 obj\n<< /Font << /F1 9 0 R >> >>\nendobj\n");

        // Content stream shared by pages 2 and 3 (NOT page 1) → part4_other_pages_shared
        let off8 = pdf.len() as u64;
        pdf.extend_from_slice(b"8 0 obj\n<< /Length 5 >>\nstream\nBT ET\nendstream\nendobj\n");

        // Font shared via resources
        let off9 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"9 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        let xref_section = format!(
            "xref\n0 10\n\
            0000000000 65535 f \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n\
            {:010} 00000 n \n",
            off1, off2, off3, off4, off5, off6, off7, off8, off9,
        );
        pdf.extend_from_slice(xref_section.as_bytes());
        let trailer = format!(
            "trailer\n<< /Size 10 /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            xref_start,
        );
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    /// Non-first-page shared objects (pages 2..N share content stream 8 0 R)
    /// must appear in shared_hints and part4_other_pages_shared.
    ///
    /// This validates the fix for the "in computed list but not hint table"
    /// qpdf warning that occurred when shared objects were only tracked for
    /// objects also reachable from page 1.
    #[test]
    fn non_first_page_shared_objects_in_shared_hints() {
        let bytes = three_page_shared_content_bytes();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("three-page PDF should parse");
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();

        // The shared content stream (8 0 R) is referenced by pages 2 and 3
        // (0-based: pages 1 and 2) but NOT page 1 (0-based: page 0).
        let shared_content = ObjectRef::new(8, 0);

        // It must land in part4_other_pages_shared (not part4_rest).
        assert!(
            plan.part4_other_pages_shared.contains(&shared_content),
            "content stream shared by pages 2 and 3 must be in part4_other_pages_shared"
        );
        assert!(
            !plan.part4_rest.contains(&shared_content),
            "content stream shared by pages 2 and 3 must NOT be in part4_rest"
        );

        // It must appear in shared_hints.
        let in_shared_hints = plan
            .shared_hints
            .iter()
            .any(|h| h.object_ref == shared_content);
        assert!(
            in_shared_hints,
            "content stream shared by pages 2 and 3 must appear in shared_hints"
        );

        // Its referencing_pages must include pages 1 and 2 (0-based).
        let hint = plan
            .shared_hints
            .iter()
            .find(|h| h.object_ref == shared_content)
            .unwrap();
        assert!(
            hint.referencing_pages.contains(&1),
            "shared content stream must reference page 1 (0-based)"
        );
        assert!(
            hint.referencing_pages.contains(&2),
            "shared content stream must reference page 2 (0-based)"
        );
        assert!(
            !hint.referencing_pages.contains(&0),
            "shared content stream must NOT reference page 0 (not reachable from page 1)"
        );

        // Disjoint invariant must hold.
        assert!(plan.parts_are_disjoint());
    }

    // =======================================================================
    // Tests for ObjStmBatchPlan (flpdf-9hc.5.8.1)
    // =======================================================================

    /// Config helpers
    fn generate_config() -> PlannerConfig {
        PlannerConfig {
            mode: ObjectStreamMode::Generate,
            batch_size_cap: std::num::NonZeroUsize::new(100).unwrap(),
        }
    }

    fn preserve_config() -> PlannerConfig {
        PlannerConfig {
            mode: ObjectStreamMode::Preserve,
            batch_size_cap: std::num::NonZeroUsize::new(100).unwrap(),
        }
    }

    fn disable_config() -> PlannerConfig {
        PlannerConfig {
            mode: ObjectStreamMode::Disable,
            batch_size_cap: std::num::NonZeroUsize::new(100).unwrap(),
        }
    }

    // -----------------------------------------------------------------------
    // (d) Disable mode: empty plan
    // -----------------------------------------------------------------------
    #[test]
    fn objstm_batches_disable_yields_empty_plan() {
        let bytes = three_page_shared_content_bytes();
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();

        let batch_plan = plan.objstm_batches(&mut pdf, &disable_config()).unwrap();
        assert!(
            batch_plan.part3_batches.is_empty(),
            "Disable must produce no part3 batches"
        );
        assert!(
            batch_plan.part4_batches.is_empty(),
            "Disable must produce no part4 batches"
        );
    }

    // -----------------------------------------------------------------------
    // (a) Generate: part3/part4 eligible objects end up in correct batches
    // -----------------------------------------------------------------------
    #[test]
    fn objstm_batches_generate_assigns_part3_and_part4() {
        // three_page_shared_content fixture:
        //   Part 3: 7 0 R (Resources dict, shared) → eligible non-stream dict
        //   Part 4 (part4_other_pages_shared): 8 0 R (content stream) → INELIGIBLE (stream)
        //   Part 4 (part4_rest): 1 0 R (Catalog), 2 0 R (Pages node)
        //
        // Eligible in Part 3: 7 0 R (dict, gen 0)
        // Ineligible in Part 3: none
        // Eligible in Part 4: those among part4_* that are plain dicts (1 0 R Catalog, 2 0 R Pages)
        // Ineligible in Part 4: 8 0 R (stream), page dicts (streams? no they're dicts),
        //   page dicts 4 0 R, 5 0 R are plain dicts → eligible
        //   9 0 R (Font dict) → eligible (in part3)
        let bytes = three_page_shared_content_bytes();
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();

        let batch_plan = plan.objstm_batches(&mut pdf, &generate_config()).unwrap();

        // Every part3_object that is eligible must appear in part3_batches.
        let all_part3_batched: std::collections::BTreeSet<ObjectRef> = batch_plan
            .part3_batches
            .iter()
            .flat_map(|b| b.iter().copied())
            .collect();
        let all_part4_batched: std::collections::BTreeSet<ObjectRef> = batch_plan
            .part4_batches
            .iter()
            .flat_map(|b| b.iter().copied())
            .collect();

        // qpdf 11.9.0 packs the first-page shared dicts into a first-half
        // ObjStm: the Resources dict (7 0 R) and the Font it references are
        // Part-3 objects and MUST appear in part3_batches.  The /Pages tree
        // (2 0 R) is folded into the same first-half batch; the /Catalog
        // (1 0 R) stays standalone.
        let resources_ref = ObjectRef::new(7, 0);
        assert!(
            all_part3_batched.contains(&resources_ref),
            "Part-3 Resources dict 7 0 R must be packed into part3_batches"
        );
        let pages_tree = plan.pages_tree_ref.expect("fixture has a /Pages tree");
        assert!(
            all_part3_batched.contains(&pages_tree),
            "the /Pages tree ({pages_tree}) must be folded into the first-half batch"
        );
        let catalog = plan.root_ref.expect("fixture has a /Catalog");
        assert!(
            !all_part3_batched.contains(&catalog) && !all_part4_batched.contains(&catalog),
            "the /Catalog ({catalog}) must stay standalone (in no batch)"
        );
        assert!(
            !all_part4_batched.contains(&resources_ref),
            "Part-3 object 7 0 R must never be misrouted into part4_batches"
        );

        // Part4 objects that are plain dicts (page 2: 4 0 R, page 3: 5 0 R) → part4_batches.
        // 8 0 R is a stream → ineligible, must NOT be in any batch.
        let shared_content = ObjectRef::new(8, 0);
        assert!(
            !all_part3_batched.contains(&shared_content),
            "stream 8 0 R must NOT be in part3_batches"
        );
        assert!(
            !all_part4_batched.contains(&shared_content),
            "stream 8 0 R must NOT be in part4_batches (streams are ineligible)"
        );

        // No batch object should come from part3 AND part4 at the same time.
        let intersection: Vec<_> = all_part3_batched.intersection(&all_part4_batched).collect();
        assert!(
            intersection.is_empty(),
            "part3_batches and part4_batches must be disjoint; overlap: {intersection:?}"
        );
    }

    // -----------------------------------------------------------------------
    // (b) part2_objects must NEVER appear in any batch (all modes)
    // -----------------------------------------------------------------------
    #[test]
    fn objstm_batches_generate_never_places_part2_in_batches() {
        let bytes = three_page_shared_content_bytes();
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();

        let batch_plan = plan.objstm_batches(&mut pdf, &generate_config()).unwrap();

        let part2_set: std::collections::BTreeSet<ObjectRef> =
            plan.part2_objects.iter().copied().collect();

        let all_batched: Vec<ObjectRef> = batch_plan
            .part3_batches
            .iter()
            .chain(&batch_plan.part4_batches)
            .flat_map(|b| b.iter().copied())
            .collect();

        for r in &all_batched {
            assert!(
                !part2_set.contains(r),
                "part2 object {r} must never appear in any ObjStm batch"
            );
        }
    }

    // -----------------------------------------------------------------------
    // (b) same for two-page fixture in Generate mode
    // -----------------------------------------------------------------------
    #[test]
    fn objstm_batches_two_page_generate_part2_not_in_batches() {
        let bytes = two_page_shared_font_bytes();
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();

        let batch_plan = plan.objstm_batches(&mut pdf, &generate_config()).unwrap();

        let part2_set: std::collections::BTreeSet<ObjectRef> =
            plan.part2_objects.iter().copied().collect();
        let all_batched: Vec<ObjectRef> = batch_plan
            .part3_batches
            .iter()
            .chain(&batch_plan.part4_batches)
            .flat_map(|b| b.iter().copied())
            .collect();

        for r in &all_batched {
            assert!(
                !part2_set.contains(r),
                "part2 object {r} must never appear in any ObjStm batch (two-page fixture)"
            );
        }
    }

    // -----------------------------------------------------------------------
    // (c) Ineligible objects excluded (streams, gen>0, etc.)
    // -----------------------------------------------------------------------
    #[test]
    fn objstm_batches_generate_excludes_ineligible_objects() {
        // two_page_shared_font fixture:
        //   6 0 R content stream (part2)   → excluded by part2 rule + stream ineligibility
        //   7 0 R content stream (part4)   → ineligible (stream), must not be in any batch
        //   8 0 R font (part3)             → eligible plain dict
        let bytes = two_page_shared_font_bytes();
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();

        let batch_plan = plan.objstm_batches(&mut pdf, &generate_config()).unwrap();

        let all_batched: std::collections::BTreeSet<ObjectRef> = batch_plan
            .part3_batches
            .iter()
            .chain(&batch_plan.part4_batches)
            .flat_map(|b| b.iter().copied())
            .collect();

        // 6 0 R: content stream (page 1 only) → part2 + stream ineligible
        let page1_content = ObjectRef::new(6, 0);
        assert!(
            !all_batched.contains(&page1_content),
            "stream 6 0 R (page-1 content, part2) must not be in any batch"
        );

        // 7 0 R: content stream (page 2 only) → part4 but stream ineligible
        let page2_content = ObjectRef::new(7, 0);
        assert!(
            !all_batched.contains(&page2_content),
            "stream 7 0 R (page-2 content, part4) must not be in any batch"
        );
    }

    // -----------------------------------------------------------------------
    // (e) Preserve mode on fixture with no source ObjStms → empty plan
    //     (no fall-through to Generate)
    // -----------------------------------------------------------------------
    #[test]
    fn objstm_batches_preserve_no_source_objstms_yields_empty_plan() {
        // two_page_shared_font_bytes is a PDF-1.4 with traditional xref table.
        // No source ObjStms → Preserve must yield empty batches.
        let bytes = two_page_shared_font_bytes();
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();

        let batch_plan = plan.objstm_batches(&mut pdf, &preserve_config()).unwrap();
        assert!(
            batch_plan.part3_batches.is_empty(),
            "Preserve with no source ObjStms must produce no part3 batches (no fall-through to Generate)"
        );
        assert!(
            batch_plan.part4_batches.is_empty(),
            "Preserve with no source ObjStms must produce no part4 batches"
        );
    }

    // -----------------------------------------------------------------------
    // (f) Preserve mode with source ObjStms — the primary behaviour under test.
    //
    // Fixture (PDF-1.5, two ObjStms, two pages):
    //
    //   Object layout:
    //     0          free
    //     1 0 obj    Catalog (plain indirect)
    //     2 0 obj    Pages node (compressed in ObjStm 7, index 0)
    //     3 0 obj    Page 1 dict (compressed in ObjStm 7, index 1)  ← Part 2, EXCLUDED
    //     4 0 obj    Page 2 dict (compressed in ObjStm 8, index 0)
    //     5 0 obj    Shared Resources dict (compressed in ObjStm 7, index 2) ← Part 3
    //     6 0 obj    Ineligible dict /Type /XRef (compressed in ObjStm 8, index 1) ← EXCLUDED (ineligible)
    //     7 0 obj    ObjStm #1  (plain indirect stream)
    //     8 0 obj    ObjStm #2  (plain indirect stream)
    //     9 0 obj    XRef stream (plain indirect)
    //
    //   LinearizationPlan partition:
    //     Part 2: [3 0 R, ...]        (first-page closure exclusives)
    //     Part 3: [5 0 R]             (shared resources — first-page AND page 2)
    //     Part 4: [2 0 R, 4 0 R, ...]  (everything else)
    //
    //   Preserve batches expected:
    //     ObjStm 7 members eligible for Part 3:  [5 0 R]   → part3_batches
    //     ObjStm 7 members eligible for Part 4:  [2 0 R]   → part4_batches
    //     ObjStm 8 members eligible for Part 4:  [4 0 R]   → part4_batches
    //     3 0 R excluded (Part 2), 6 0 R excluded (ineligible /Type /XRef)
    // -----------------------------------------------------------------------

    /// Build a zlib-compressed ObjStm payload from (object-number, raw-bytes) pairs.
    /// Returns (compressed_bytes, first_offset_in_body).
    /// Mirrors the private helper in `writer/object_streams.rs`.
    fn build_objstm_payload_plan(members: &[(u32, &[u8])]) -> (Vec<u8>, usize) {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        let mut header = String::new();
        let mut body = Vec::new();
        for (index, (number, object_data)) in members.iter().enumerate() {
            let offset = body.len();
            header.push_str(&format!("{} {} ", number, offset));
            body.extend_from_slice(object_data);
            if index + 1 < members.len() {
                body.push(b'\n');
            }
        }
        let mut decoded = Vec::new();
        decoded.extend_from_slice(header.as_bytes());
        decoded.extend_from_slice(&body);

        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&decoded).unwrap();
        let encoded = enc.finish().unwrap();
        (encoded, header.len())
    }

    fn append_u24_be_plan(bytes: &mut Vec<u8>, value: u32) {
        let b = value.to_be_bytes();
        bytes.extend_from_slice(&b[1..]);
    }

    /// Append a 1+3+1 xref stream entry (W=[1 3 1]).
    fn append_xref_entry_plan(entries: &mut Vec<u8>, entry_type: u8, field1: u32, field2: u8) {
        entries.push(entry_type);
        append_u24_be_plan(entries, field1);
        entries.push(field2);
    }

    /// Build a 2-page PDF-1.5 with two source ObjStms.
    ///
    /// Object layout:
    ///   0: free
    ///   1: Catalog (plain indirect)
    ///   2: Pages node (ObjStm 7, idx 0)
    ///   3: Page 1 dict (ObjStm 7, idx 1) — Part 2 (excluded from ObjStm batches)
    ///   4: Page 2 dict (ObjStm 8, idx 0) — Part 4 private
    ///   5: Shared Resources dict (ObjStm 7, idx 2) — Part 3
    ///   6: Ineligible dict /Type /XRef (ObjStm 8, idx 1) — excluded (ineligible)
    ///   7: ObjStm stream containing [2, 3, 5] in source-index order [0, 1, 2]
    ///   8: ObjStm stream containing [4, 6] in source-index order [0, 1]
    ///   9: XRef stream
    fn two_page_two_objstm_pdf_bytes() -> Vec<u8> {
        let mut bytes = b"%PDF-1.5\n".to_vec();

        // Object 1: Catalog (plain indirect)
        let catalog_offset = bytes.len() as u32;
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        // ObjStm #7: members [2=Pages, 3=Page1, 5=Resources] at indices [0, 1, 2]
        let objstm1_members: &[(u32, &[u8])] = &[
            (2, b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources 5 0 R >>",
            ),
            (
                5,
                b"<< /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >>",
            ),
        ];
        let (stream1_data, first1) = build_objstm_payload_plan(objstm1_members);
        let n1 = objstm1_members.len() as u32;
        let objstm1_offset = bytes.len() as u32;
        bytes.extend_from_slice(
            format!(
                "7 0 obj\n<< /Type /ObjStm /N {n1} /First {first1} /Length {} /Filter /FlateDecode >>\nstream\n",
                stream1_data.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(&stream1_data);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");

        // ObjStm #8: members [4=Page2, 6=ineligible /Type /XRef dict] at indices [0, 1]
        let objstm2_members: &[(u32, &[u8])] = &[
            (
                4,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources 5 0 R >>",
            ),
            (6, b"<< /Type /XRef >>"),
        ];
        let (stream2_data, first2) = build_objstm_payload_plan(objstm2_members);
        let n2 = objstm2_members.len() as u32;
        let objstm2_offset = bytes.len() as u32;
        bytes.extend_from_slice(
            format!(
                "8 0 obj\n<< /Type /ObjStm /N {n2} /First {first2} /Length {} /Filter /FlateDecode >>\nstream\n",
                stream2_data.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(&stream2_data);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");

        // XRef stream (object 9)
        let xref_offset = bytes.len() as u32;
        // W=[1 3 1]: 10 objects (0..=9)
        let mut xref_entries: Vec<u8> = Vec::new();
        // 0: free
        append_xref_entry_plan(&mut xref_entries, 0, 0, 0);
        // 1: Catalog at catalog_offset
        append_xref_entry_plan(&mut xref_entries, 1, catalog_offset, 0);
        // 2: Pages in ObjStm 7, index 0
        append_xref_entry_plan(&mut xref_entries, 2, 7, 0);
        // 3: Page1 in ObjStm 7, index 1
        append_xref_entry_plan(&mut xref_entries, 2, 7, 1);
        // 4: Page2 in ObjStm 8, index 0
        append_xref_entry_plan(&mut xref_entries, 2, 8, 0);
        // 5: Resources in ObjStm 7, index 2
        append_xref_entry_plan(&mut xref_entries, 2, 7, 2);
        // 6: Ineligible in ObjStm 8, index 1
        append_xref_entry_plan(&mut xref_entries, 2, 8, 1);
        // 7: ObjStm #1 at objstm1_offset
        append_xref_entry_plan(&mut xref_entries, 1, objstm1_offset, 0);
        // 8: ObjStm #2 at objstm2_offset
        append_xref_entry_plan(&mut xref_entries, 1, objstm2_offset, 0);
        // 9: XRef stream at xref_offset
        append_xref_entry_plan(&mut xref_entries, 1, xref_offset, 0);

        bytes.extend_from_slice(
            format!(
                "9 0 obj\n<< /Type /XRef /Size 10 /Root 1 0 R /W [1 3 1] /Index [0 10] /Length {} >>\nstream\n",
                xref_entries.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(&xref_entries);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
        bytes
    }

    /// Preserve mode with source ObjStms: members are grouped by source ObjStm,
    /// sorted by source index, split into Part3/Part4, and ineligible + Part2
    /// members are excluded.
    #[test]
    fn objstm_batches_preserve_source_objstm_grouping_and_part_split() {
        let bytes = two_page_two_objstm_pdf_bytes();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("two-ObjStm PDF should parse");
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();

        // Verify the partition is what the fixture was designed to produce.
        // Part 2 must contain page 1 dict (3 0 R).
        let page1_ref = ObjectRef::new(3, 0);
        assert!(
            plan.part2_objects.contains(&page1_ref),
            "page 1 dict (3 0 R) must be in Part 2"
        );

        // Resources dict (5 0 R) must be in Part 3 (shared by both pages).
        let resources_ref = ObjectRef::new(5, 0);
        assert!(
            plan.part3_objects.contains(&resources_ref),
            "shared Resources dict (5 0 R) must be in Part 3"
        );

        // Page 2 dict (4 0 R) must be in Part 4.
        let page2_ref = ObjectRef::new(4, 0);
        assert!(
            plan.part4_objects().contains(&page2_ref),
            "page 2 dict (4 0 R) must be in Part 4"
        );

        // Now call objstm_batches in Preserve mode.
        let batch_plan = plan
            .objstm_batches(&mut pdf, &preserve_config())
            .expect("Preserve mode must succeed");

        let all_part3_batched: BTreeSet<ObjectRef> = batch_plan
            .part3_batches
            .iter()
            .flat_map(|b| b.iter().copied())
            .collect();
        let all_part4_batched: BTreeSet<ObjectRef> = batch_plan
            .part4_batches
            .iter()
            .flat_map(|b| b.iter().copied())
            .collect();

        // ── Invariant 1: Part 2 objects never appear in any batch ──────────
        let part2_set: BTreeSet<ObjectRef> = plan.part2_objects.iter().copied().collect();
        for r in all_part3_batched.iter().chain(all_part4_batched.iter()) {
            assert!(
                !part2_set.contains(r),
                "Part-2 object {r} must never appear in any ObjStm batch"
            );
        }

        // ── Invariant 2: Ineligible object (6 0 R, /Type /XRef dict) excluded ──
        let ineligible_ref = ObjectRef::new(6, 0);
        assert!(
            !all_part3_batched.contains(&ineligible_ref),
            "ineligible /Type /XRef dict (6 0 R) must not appear in part3_batches"
        );
        assert!(
            !all_part4_batched.contains(&ineligible_ref),
            "ineligible /Type /XRef dict (6 0 R) must not appear in part4_batches"
        );

        // ── Invariant 3: Part-3 first-page shared objects ARE packed ──────────
        // qpdf 11.9.0 packs the first-page shared Resources dict (5 0 R) into a
        // first-half ObjStm; the /Pages tree (2 0 R) is folded into the same
        // first-half batch.  5 0 R must appear in part3_batches and NOT be
        // misrouted into part4_batches.
        assert!(
            all_part3_batched.contains(&resources_ref),
            "Part-3 Resources dict 5 0 R must be packed into part3_batches"
        );
        assert!(
            !all_part4_batched.contains(&resources_ref),
            "Part-3 object 5 0 R must NOT be misrouted into part4_batches"
        );

        // ── Invariant 4: /Pages tree is routed to the first-half batch ────────
        // 2 0 R (Pages) is a member of source ObjStm 7 and is routed into that
        // container's first-half (part3) batch, so it must NOT remain in
        // part4_batches.  4 0 R (Page 2) is a Part-4 page-private dict and stays
        // in part4_batches at the planner level (the writer drops it later via
        // the page-private filter).
        let pages_ref = ObjectRef::new(2, 0);
        assert!(
            all_part3_batched.contains(&pages_ref),
            "the /Pages tree (2 0 R) must be routed into part3_batches"
        );
        assert!(
            !all_part4_batched.contains(&pages_ref),
            "the /Pages tree (2 0 R) must NOT remain in part4_batches"
        );
        assert!(
            all_part4_batched.contains(&page2_ref),
            "Part-4 eligible object 4 0 R (Page 2) must appear in part4_batches"
        );

        // ── Invariant 5: /Catalog stays standalone ────────────────────────────
        // qpdf keeps the document /Catalog uncompressed; it must appear in no
        // batch.  (In this fixture the Catalog 1 0 R was not in any source
        // ObjStm, but the canonicalisation guards against it regardless.)
        let catalog_ref = plan.root_ref.expect("fixture has a /Catalog");
        assert!(
            !all_part3_batched.contains(&catalog_ref) && !all_part4_batched.contains(&catalog_ref),
            "the /Catalog ({catalog_ref}) must stay standalone (in no batch)"
        );

        // ── Invariant 7: No overlap between part3_batches and part4_batches ────
        let overlap: Vec<ObjectRef> = all_part3_batched
            .intersection(&all_part4_batched)
            .copied()
            .collect();
        assert!(
            overlap.is_empty(),
            "part3_batches and part4_batches must be disjoint; overlap: {overlap:?}"
        );

        // ── Invariant 8: Every batched ref is eligible ──────────────────────────
        use crate::writer::object_streams::{eligibility_context, is_eligible_for_objstm};
        let ctx = eligibility_context(&mut pdf).unwrap();
        for r in all_part3_batched.iter().chain(all_part4_batched.iter()) {
            let obj = pdf.resolve_borrowed(*r).unwrap();
            assert!(
                is_eligible_for_objstm(*r, obj, &ctx),
                "batched object {r} must be eligible for ObjStm"
            );
        }

        // ── Invariant 9: Every batched ref has a linearization-plan slot ──────
        // A batched object must live in the plan's Part-3 or Part-4 set; an
        // eligible source-ObjStm member that is not in any part has no
        // RenumberMap entry and would crash ObjStmLayout::build with "has no
        // renumber entry". Guards the preserve-mode part4_set filter.
        let part3_set: BTreeSet<ObjectRef> = plan.part3_objects.iter().copied().collect();
        let part4_set: BTreeSet<ObjectRef> = plan
            .part4_other_pages_private
            .iter()
            .chain(&plan.part4_other_pages_shared)
            .chain(&plan.part4_rest)
            .copied()
            .collect();
        for r in all_part3_batched.iter().chain(all_part4_batched.iter()) {
            assert!(
                part3_set.contains(r) || part4_set.contains(r),
                "batched object {r} has no linearization-plan slot \
                 (not in Part-3 or Part-4) — would break ObjStmLayout::build"
            );
        }
    }

    /// Preserve mode with source ObjStms and a small cap: ObjStm members that
    /// exceed the cap are split into multiple batches per part.
    #[test]
    fn objstm_batches_preserve_cap_splits_large_groups() {
        // Source ObjStm 7 holds the first-page shared Resources dict (5 0 R) and
        // the /Pages tree (2 0 R), both routed to the first half; ObjStm 8
        // contributes Page 2 (4 0 R) to part4.  With cap=1 each source
        // container's first-half members are chunked so each batch holds at most
        // one member.
        let bytes = two_page_two_objstm_pdf_bytes();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("two-ObjStm PDF should parse");
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();

        let cap1_config = PlannerConfig {
            mode: crate::writer::object_streams::ObjectStreamMode::Preserve,
            batch_size_cap: std::num::NonZeroUsize::new(1).unwrap(),
        };

        let batch_plan = plan
            .objstm_batches(&mut pdf, &cap1_config)
            .expect("Preserve with cap=1 must succeed");

        // Each batch must have at most 1 member.
        for batch in batch_plan
            .part3_batches
            .iter()
            .chain(&batch_plan.part4_batches)
        {
            assert!(
                batch.len() <= 1,
                "with cap=1, each batch must have at most 1 member; got {} members: {batch:?}",
                batch.len()
            );
        }

        // All previously expected objects must still appear.
        let all_batched: BTreeSet<ObjectRef> = batch_plan
            .part3_batches
            .iter()
            .chain(&batch_plan.part4_batches)
            .flat_map(|b| b.iter().copied())
            .collect();

        let resources_ref = ObjectRef::new(5, 0);
        let pages_ref = ObjectRef::new(2, 0);
        let page2_ref = ObjectRef::new(4, 0);
        assert!(
            !batch_plan.part3_batches.is_empty(),
            "Part-3 (5 0 R) is packed into the first-half ObjStm; part3_batches must be non-empty"
        );
        assert!(
            all_batched.contains(&resources_ref),
            "Part-3 Resources dict 5 0 R must be batched (packed into the first-half ObjStm)"
        );
        assert!(
            all_batched.contains(&pages_ref),
            "2 0 R (/Pages tree, routed to the first-half batch) must be batched even with cap=1"
        );
        assert!(
            all_batched.contains(&page2_ref),
            "4 0 R must be batched even with cap=1"
        );
    }

    /// One source ObjStm whose **two** members both land in Part 4 (they are
    /// eligible plain dicts reachable from neither page-1 nor any other page,
    /// so `from_pdf` keeps them in `part4_rest`).  This is the fixture the
    /// `chunks(cap)` split path actually needs: a single source ObjStm
    /// contributing ≥2 eligible members to the *same* part.
    ///
    /// The members are referenced from a neutral catalog key (`/Aux`) so they
    /// survive `from_pdf`'s reachability GC (flpdf-phfu) — `/Aux` is neither a
    /// page nor an open-document key, so they stay out of every page closure and
    /// land in `part4_rest`.
    fn objstm_two_part4_members_pdf_bytes() -> Vec<u8> {
        let mut bytes = b"%PDF-1.5\n".to_vec();

        let catalog_offset = bytes.len() as u32;
        bytes.extend_from_slice(
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Aux [10 0 R 11 0 R] >>\nendobj\n",
        );

        let pages_offset = bytes.len() as u32;
        bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        let page1_offset = bytes.len() as u32;
        bytes.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        // ObjStm #7: members [10, 11] — two eligible plain dicts referenced only
        // by the catalog's neutral /Aux key, so both are reachable (survive the
        // reachability GC) yet land in part4_rest (same part, same source ObjStm).
        let objstm_members: &[(u32, &[u8])] =
            &[(10, b"<< /Marker 10 >>"), (11, b"<< /Marker 11 >>")];
        let (stream_data, first) = build_objstm_payload_plan(objstm_members);
        let n = objstm_members.len() as u32;
        let objstm_offset = bytes.len() as u32;
        bytes.extend_from_slice(
            format!(
                "7 0 obj\n<< /Type /ObjStm /N {n} /First {first} /Length {} /Filter /FlateDecode >>\nstream\n",
                stream_data.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(&stream_data);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");

        let xref_offset = bytes.len() as u32;
        let mut xref_entries: Vec<u8> = Vec::new();
        // 0..=11 plus xref stream obj 12 → Size 13.
        append_xref_entry_plan(&mut xref_entries, 0, 0, 0); // 0 free
        append_xref_entry_plan(&mut xref_entries, 1, catalog_offset, 0); // 1
        append_xref_entry_plan(&mut xref_entries, 1, pages_offset, 0); // 2
        append_xref_entry_plan(&mut xref_entries, 1, page1_offset, 0); // 3
        for _ in 4..=6 {
            append_xref_entry_plan(&mut xref_entries, 0, 0, 0); // 4..6 free
        }
        append_xref_entry_plan(&mut xref_entries, 1, objstm_offset, 0); // 7 ObjStm
        for _ in 8..=9 {
            append_xref_entry_plan(&mut xref_entries, 0, 0, 0); // 8..9 free
        }
        append_xref_entry_plan(&mut xref_entries, 2, 7, 0); // 10 in ObjStm 7 idx 0
        append_xref_entry_plan(&mut xref_entries, 2, 7, 1); // 11 in ObjStm 7 idx 1
        append_xref_entry_plan(&mut xref_entries, 1, xref_offset, 0); // 12 XRef

        bytes.extend_from_slice(
            format!(
                "12 0 obj\n<< /Type /XRef /Size 13 /Root 1 0 R /W [1 3 1] /Index [0 13] /Length {} >>\nstream\n",
                xref_entries.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(&xref_entries);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
        bytes
    }

    /// Regression for the `chunks(cap)` split path: a single source ObjStm
    /// contributing two eligible members to the *same* part (Part 4) must be
    /// split into two separate cap=1 batches, and coalesced into one at cap=2.
    /// (The pre-existing cap test never had ≥2 same-source/same-part members,
    /// so a broken `chunks(cap)` would have passed it unnoticed.)
    #[test]
    fn objstm_batches_preserve_cap_actually_splits_same_source_same_part() {
        let bytes = objstm_two_part4_members_pdf_bytes();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("fixture should parse");
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();

        let m10 = ObjectRef::new(10, 0);
        let m11 = ObjectRef::new(11, 0);

        // Both members are Part-4 (not in any page closure → part4_rest).
        let part4: BTreeSet<ObjectRef> = plan
            .part4_other_pages_private
            .iter()
            .chain(&plan.part4_other_pages_shared)
            .chain(&plan.part4_rest)
            .copied()
            .collect();
        assert!(
            part4.contains(&m10) && part4.contains(&m11),
            "fixture invariant: 10 0 R and 11 0 R must both be Part-4"
        );

        // cap=1: the two same-source same-part members must land in DIFFERENT
        // batches (this is exactly what `chunks(cap)` must do).
        let cap1 = PlannerConfig {
            mode: crate::writer::object_streams::ObjectStreamMode::Preserve,
            batch_size_cap: std::num::NonZeroUsize::new(1).unwrap(),
        };
        let bp1 = plan
            .objstm_batches(&mut pdf, &cap1)
            .expect("Preserve cap=1 must succeed");
        let b10 = bp1.part4_batches.iter().position(|b| b.contains(&m10));
        let b11 = bp1.part4_batches.iter().position(|b| b.contains(&m11));
        assert!(
            b10.is_some() && b11.is_some(),
            "both members must be batched at cap=1; part4_batches={:?}",
            bp1.part4_batches
        );
        assert_ne!(
            b10, b11,
            "cap=1: 10 0 R and 11 0 R (same source ObjStm, same part) must be \
             in SEPARATE batches — chunks(cap) split path"
        );
        for b in &bp1.part4_batches {
            assert!(b.len() <= 1, "cap=1 batch over capacity: {b:?}");
        }

        // cap=2: the same two members must coalesce into ONE batch, proving the
        // split is cap-driven (not unconditional).
        let cap2 = PlannerConfig {
            mode: crate::writer::object_streams::ObjectStreamMode::Preserve,
            batch_size_cap: std::num::NonZeroUsize::new(2).unwrap(),
        };
        let bp2 = plan
            .objstm_batches(&mut pdf, &cap2)
            .expect("Preserve cap=2 must succeed");
        let same_batch = bp2
            .part4_batches
            .iter()
            .any(|b| b.contains(&m10) && b.contains(&m11));
        assert!(
            same_batch,
            "cap=2: 10 0 R and 11 0 R must share one batch; part4_batches={:?}",
            bp2.part4_batches
        );
    }

    // -----------------------------------------------------------------------
    // Generate: part3 objects go only to part3_batches, part4 to part4_batches
    // -----------------------------------------------------------------------
    #[test]
    fn objstm_batches_generate_cross_part_placement() {
        // Use two-page fixture: Part3 = Resources (5 0 R) + Font (8 0 R)
        //                       Part4 = page 2 dict (4 0 R) + content stream (7 0 R ineligible)
        let bytes = two_page_shared_font_bytes();
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();

        let batch_plan = plan.objstm_batches(&mut pdf, &generate_config()).unwrap();

        let all_part3_batched: std::collections::BTreeSet<ObjectRef> = batch_plan
            .part3_batches
            .iter()
            .flat_map(|b| b.iter().copied())
            .collect();
        let all_part4_batched: std::collections::BTreeSet<ObjectRef> = batch_plan
            .part4_batches
            .iter()
            .flat_map(|b| b.iter().copied())
            .collect();

        // The first-half (Part-3) batch holds the Part-3 objects (Resources
        // 5 0 R + Font 8 0 R) PLUS the /Pages tree (2 0 R), folded in by the
        // qpdf member-set canonicalisation.  No /Info in this fixture.
        let part3_set: std::collections::BTreeSet<ObjectRef> =
            plan.part3_objects.iter().copied().collect();
        let pages_tree = plan
            .pages_tree_ref
            .expect("two-page fixture has a /Pages tree");
        for r in &all_part3_batched {
            assert!(
                part3_set.contains(r) || *r == pages_tree,
                "object {r} in part3_batches must be a Part-3 object or the folded /Pages tree"
            );
        }
        // The /Pages tree was folded into the first-half batch.
        assert!(
            all_part3_batched.contains(&pages_tree),
            "the /Pages tree ({pages_tree}) must be folded into the first-half batch"
        );
        // No Part-3 object, the /Pages tree, or the /Catalog may appear in
        // part4_batches (Catalog stays standalone; Pages migrates to part3).
        let catalog = plan.root_ref.expect("fixture has a /Catalog");
        for r in &all_part4_batched {
            assert!(
                !part3_set.contains(r),
                "part3 object {r} must not appear in part4_batches"
            );
            assert_ne!(
                *r, pages_tree,
                "the /Pages tree must not remain in part4_batches"
            );
            assert_ne!(
                *r, catalog,
                "the /Catalog must stay standalone, not in part4_batches"
            );
        }
    }

    /// Single-page Generate (no first-page shared objects, so `part3_batches` is
    /// empty): the document `/Catalog` must still be excluded from the Part-4
    /// ObjStm batches, because qpdf never compresses the Catalog (the linearized
    /// `generateObjectStreams` erases it from the compressible set).  Guards the
    /// Catalog exclusion against regressing for the no-first-half-batch case.
    #[test]
    fn objstm_batches_generate_keeps_catalog_standalone_single_page() {
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();

        // Single page → no first-page SHARED objects → no first-half batch.
        let batch_plan = plan.objstm_batches(&mut pdf, &generate_config()).unwrap();
        assert!(
            batch_plan.part3_batches.is_empty(),
            "single-page document has no first-page shared objects (no Part-3 batch)"
        );

        let all_part3_batched: std::collections::BTreeSet<ObjectRef> = batch_plan
            .part3_batches
            .iter()
            .flat_map(|b| b.iter().copied())
            .collect();
        let all_part4_batched: std::collections::BTreeSet<ObjectRef> = batch_plan
            .part4_batches
            .iter()
            .flat_map(|b| b.iter().copied())
            .collect();

        let catalog = plan.root_ref.expect("fixture has a /Catalog");
        assert!(
            !all_part3_batched.contains(&catalog) && !all_part4_batched.contains(&catalog),
            "the /Catalog ({catalog}) must stay standalone (in no ObjStm batch), \
             even when there is no first-half Part-3 batch"
        );

        // The /Pages tree is an eligible plain dict in Part-4 here (it is not
        // folded into the first half without a Part-3 batch), so it remains
        // batched — confirming only the Catalog was filtered out, not the whole
        // batch.
        let pages_tree = ObjectRef::new(2, 0);
        assert!(
            all_part4_batched.contains(&pages_tree),
            "the /Pages tree (2 0 R) must remain in part4_batches when there is \
             no first-half batch to fold it into"
        );
    }

    // -----------------------------------------------------------------------
    // canonical_shared_hints: fold first-page ObjStm members into a container
    // -----------------------------------------------------------------------

    /// Two members of the same first-half container that reference DIFFERENT
    /// pages must fold into ONE container entry whose `referencing_pages` is the
    /// sorted union of both members' pages (exercises the merge-insert path).
    #[test]
    fn canonical_shared_hints_folds_members_and_unions_pages() {
        let page = ObjectRef::new(3, 0);
        let content = ObjectRef::new(9, 0);
        let font_dict = ObjectRef::new(1, 0);
        let font = ObjectRef::new(2, 0);
        let plan = LinearizationPlan {
            part2_objects: vec![page, content],
            part3_objects: vec![font_dict, font],
            shared_hints: vec![
                SharedObjectHintEntry {
                    object_ref: page,
                    referencing_pages: vec![],
                },
                SharedObjectHintEntry {
                    object_ref: content,
                    referencing_pages: vec![],
                },
                // font_dict referenced by page 1; font by page 2 — different
                // pages, so the union must contain both (forces the insert).
                SharedObjectHintEntry {
                    object_ref: font_dict,
                    referencing_pages: vec![1],
                },
                SharedObjectHintEntry {
                    object_ref: font,
                    referencing_pages: vec![2],
                },
            ],
            ..Default::default()
        };

        // Both font_dict and font live in container 12 (the first-half ObjStm).
        let mut m2c: BTreeMap<ObjectRef, (u32, u32)> = BTreeMap::new();
        m2c.insert(font_dict, (12, 0));
        m2c.insert(font, (12, 1));

        // The first-page section is ordered by physical object number. With
        // this plan `RenumberMap::from_plan` assigns page=3, content=4 (Part 2),
        // and the container is numbered 12, so the post-sort order is
        // [page, content, container].
        let renumber = RenumberMap::from_plan(&plan);
        let folded =
            plan.canonical_shared_hints(&m2c, &renumber, &Default::default(), &Default::default());
        // page, content, and ONE container entry = 3 entries (members folded).
        assert_eq!(
            folded.len(),
            3,
            "members must fold into one container entry"
        );
        assert_eq!(folded[0].object_ref, page);
        assert_eq!(folded[1].object_ref, content);
        // The container entry carries the container's new number (12) with the
        // sentinel generation u16::MAX (so it is never mistaken for a real
        // object numbered 12).
        assert_eq!(folded[2].object_ref, ObjectRef::new(12, u16::MAX));
        // Its referencing_pages is the sorted union {1, 2}.
        assert_eq!(
            folded[2].referencing_pages,
            vec![1, 2],
            "container entry must union both members' referencing pages"
        );
    }

    /// A shared-hint entry with no `RenumberMap` mapping must not panic the
    /// first-page sort. A malformed source can carry a reserved/dangling ref
    /// (object number 0, any generation) into the first-page closure and thus
    /// into `shared_hints`; `place_objstm_members_per_half` rebuilds the forward
    /// index while dropping object number 0 (it doubles as the reserved-slot
    /// sentinel), so such an entry has no mapping here. The sort must place it
    /// deterministically last instead of aborting — the writer's shared-hint
    /// back-patch then reports the planner/renumber inconsistency as a
    /// structured error before any bytes are emitted.
    #[test]
    fn canonical_shared_hints_missing_renumber_entry_sorts_last_without_panic() {
        let page = ObjectRef::new(3, 0);
        let missing = ObjectRef::new(0, 0);
        let font_dict = ObjectRef::new(1, 0);
        let plan = LinearizationPlan {
            part2_objects: vec![page, missing],
            part3_objects: vec![font_dict],
            shared_hints: vec![
                SharedObjectHintEntry {
                    object_ref: missing,
                    referencing_pages: vec![],
                },
                SharedObjectHintEntry {
                    object_ref: page,
                    referencing_pages: vec![],
                },
                SharedObjectHintEntry {
                    object_ref: font_dict,
                    referencing_pages: vec![1],
                },
            ],
            ..Default::default()
        };

        // font_dict packs into a first-half ObjStm container; `missing` and
        // `page` stay plain. The container is folded to one entry numbered 12.
        let mut m2c: BTreeMap<ObjectRef, (u32, u32)> = BTreeMap::new();
        m2c.insert(font_dict, (12, 0));

        let mut renumber = RenumberMap::from_plan(&plan);
        renumber.place_objstm_members_per_half(
            &[],
            &[vec![font_dict]],
            &[],
            &[],
            &Default::default(),
            &Default::default(),
        );
        // Relocation drops object number 0 from the forward index, so the
        // reserved ref has no mapping — the precondition for the regression.
        assert!(
            renumber.new_for_original(missing).is_none(),
            "ObjStm relocation must drop object number 0 from the renumber map"
        );

        let folded =
            plan.canonical_shared_hints(&m2c, &renumber, &Default::default(), &Default::default());

        // No panic, and the unmapped entry sorts last (key u32::MAX) behind the
        // real page object and the folded container (new #12, sentinel gen).
        assert_eq!(folded.len(), 3);
        assert_eq!(folded[0].object_ref, page);
        assert_eq!(folded[1].object_ref, ObjectRef::new(12, u16::MAX));
        assert_eq!(folded[2].object_ref, missing);
    }

    /// With an empty member-to-container map (no ObjStm packing) the folded list
    /// is a verbatim clone of `shared_hints` (classic path unchanged).
    #[test]
    fn canonical_shared_hints_empty_map_is_identity() {
        let page = ObjectRef::new(3, 0);
        let plan = LinearizationPlan {
            part2_objects: vec![page],
            shared_hints: vec![SharedObjectHintEntry {
                object_ref: page,
                referencing_pages: vec![],
            }],
            ..Default::default()
        };
        // The empty-map branch returns before using `renumber`, so any map works.
        let renumber = RenumberMap::from_plan(&plan);
        let folded = plan.canonical_shared_hints(
            &BTreeMap::new(),
            &renumber,
            &Default::default(),
            &Default::default(),
        );
        assert_eq!(folded, plan.shared_hints);
    }

    /// A Part-8 entry (`part4_other_pages_shared`, after /E) is NOT sorted into
    /// the first-page section: only the leading part2 ++ part3 entries are
    /// reordered by physical object number. Here a plain ineligible shared
    /// stream (`image`), physically numbered BEFORE the container, must sort
    /// BEFORE the folded container within the first-page section, while the
    /// trailing Part-8 entry stays last regardless of its physical number.
    #[test]
    fn canonical_shared_hints_orders_first_page_and_keeps_part8_last() {
        let page = ObjectRef::new(8, 0);
        let content = ObjectRef::new(9, 0);
        let image = ObjectRef::new(10, 0);
        let font_dict = ObjectRef::new(1, 0);
        let other_shared = ObjectRef::new(7, 0);
        let plan = LinearizationPlan {
            // part2 ++ part3 form the first-page section; font_dict is the only
            // ObjStm-eligible member (folded into container 11).
            part2_objects: vec![page, content, image],
            part3_objects: vec![font_dict],
            // Part-8: one other-pages shared object.
            part4_other_pages_shared: vec![other_shared],
            shared_hints: vec![
                SharedObjectHintEntry {
                    object_ref: page,
                    referencing_pages: vec![],
                },
                SharedObjectHintEntry {
                    object_ref: content,
                    referencing_pages: vec![],
                },
                // font_dict is eligible → folds into container 11, which is
                // physically numbered AFTER the plain `image` stream.
                SharedObjectHintEntry {
                    object_ref: font_dict,
                    referencing_pages: vec![1],
                },
                SharedObjectHintEntry {
                    object_ref: image,
                    referencing_pages: vec![1],
                },
                // Part-8 entry (after /E): must remain last after the sort.
                SharedObjectHintEntry {
                    object_ref: other_shared,
                    referencing_pages: vec![2],
                },
            ],
            ..Default::default()
        };

        // font_dict lives in container 11 (the first-half ObjStm).
        let mut m2c: BTreeMap<ObjectRef, (u32, u32)> = BTreeMap::new();
        m2c.insert(font_dict, (11, 0));

        let renumber = RenumberMap::from_plan(&plan);
        let folded =
            plan.canonical_shared_hints(&m2c, &renumber, &Default::default(), &Default::default());

        // 3 plain first-page entries (page, content, image) + 1 folded
        // container + 1 Part-8 entry = 5 (only font_dict folded away).
        assert_eq!(folded.len(), 5, "only the eligible member folds");
        // First-page section ordered by physical number: page, content come
        // first (Part 2), then `image` (last Part 2), then the container 11.
        assert_eq!(folded[0].object_ref, page);
        assert_eq!(folded[1].object_ref, content);
        assert_eq!(folded[2].object_ref, image);
        assert_eq!(folded[3].object_ref, ObjectRef::new(11, u16::MAX));
        // Part-8 entry stays last (not pulled into the first-page sort).
        assert_eq!(folded[4].object_ref, other_shared);
    }

    /// A second-half (outline-routed, Part-4) ObjStm container that appears in
    /// the first-page section of `shared_hints` must be skipped entirely when
    /// `second_half_container_nums` contains its number.  qpdf does not emit
    /// these containers in the Shared Object Hint Table's first-page section.
    #[test]
    fn canonical_shared_hints_skips_second_half_container_in_first_page_section() {
        let page = ObjectRef::new(3, 0);
        let content = ObjectRef::new(9, 0);
        let font_dict = ObjectRef::new(1, 0);
        let plan = LinearizationPlan {
            part2_objects: vec![page, content],
            part3_objects: vec![font_dict],
            shared_hints: vec![
                SharedObjectHintEntry {
                    object_ref: page,
                    referencing_pages: vec![],
                },
                SharedObjectHintEntry {
                    object_ref: content,
                    referencing_pages: vec![],
                },
                SharedObjectHintEntry {
                    object_ref: font_dict,
                    referencing_pages: vec![1],
                },
            ],
            ..Default::default()
        };

        // font_dict lives in container 20, which is a second-half (Part-4) container.
        let mut m2c: BTreeMap<ObjectRef, (u32, u32)> = BTreeMap::new();
        m2c.insert(font_dict, (20, 0));

        // Mark container 20 as second-half so it should be skipped.
        let mut second_half: BTreeSet<u32> = BTreeSet::new();
        second_half.insert(20);

        let renumber = RenumberMap::from_plan(&plan);
        let folded =
            plan.canonical_shared_hints(&m2c, &renumber, &second_half, &Default::default());

        // The second-half container entry must be absent from the result.
        // Only the two plain first-page entries (page, content) survive;
        // font_dict's container is skipped entirely (not folded, not emitted).
        assert_eq!(
            folded.len(),
            2,
            "second-half container must be excluded from first-page section"
        );
        assert_eq!(folded[0].object_ref, page);
        assert_eq!(folded[1].object_ref, content);
        // Confirm the second-half container sentinel is absent.
        assert!(
            folded
                .iter()
                .all(|e| e.object_ref != ObjectRef::new(20, u16::MAX)),
            "container 20 sentinel must not appear"
        );
    }

    /// An open-document (before /O) ObjStm container that appears in the
    /// first-page section of `shared_hints` must be skipped entirely when
    /// `open_document_container_nums` contains its number.  Objects placed before
    /// /O are not part of the first-page [/O,/E) section and must not appear in
    /// the Shared Object Hint Table's first-page entries.
    #[test]
    fn canonical_shared_hints_skips_open_document_container_in_first_page_section() {
        let page = ObjectRef::new(3, 0);
        let content = ObjectRef::new(9, 0);
        let font_dict = ObjectRef::new(1, 0);
        let plan = LinearizationPlan {
            part2_objects: vec![page, content],
            part3_objects: vec![font_dict],
            shared_hints: vec![
                SharedObjectHintEntry {
                    object_ref: page,
                    referencing_pages: vec![],
                },
                SharedObjectHintEntry {
                    object_ref: content,
                    referencing_pages: vec![],
                },
                SharedObjectHintEntry {
                    object_ref: font_dict,
                    referencing_pages: vec![1],
                },
            ],
            ..Default::default()
        };

        // font_dict lives in container 30, which is an open-document (before /O) container.
        let mut m2c: BTreeMap<ObjectRef, (u32, u32)> = BTreeMap::new();
        m2c.insert(font_dict, (30, 0));

        // Mark container 30 as open-document so it should be skipped.
        let mut open_doc: BTreeSet<u32> = BTreeSet::new();
        open_doc.insert(30);

        let renumber = RenumberMap::from_plan(&plan);
        let folded = plan.canonical_shared_hints(&m2c, &renumber, &Default::default(), &open_doc);

        // The open-document container entry must be absent from the result.
        // Only the two plain first-page entries (page, content) survive;
        // font_dict's container is skipped entirely (not folded, not emitted).
        assert_eq!(
            folded.len(),
            2,
            "open-document container must be excluded from first-page section"
        );
        assert_eq!(folded[0].object_ref, page);
        assert_eq!(folded[1].object_ref, content);
        // Confirm the open-document container sentinel is absent.
        assert!(
            folded
                .iter()
                .all(|e| e.object_ref != ObjectRef::new(30, u16::MAX)),
            "container 30 sentinel must not appear"
        );
    }

    /// A part9 (Rest) ObjStm container routed there by qpdf's outline priority
    /// (it co-locates an in_outlines `!UseOutlines` member with a
    /// `part4_other_pages_shared` member via the even split) must be skipped in
    /// the Part-8 section of the Shared Object Hint Table — while a *legitimate*
    /// part8 container is kept. This locks the narrow part9-only exclusion: a
    /// naive `second_half_container_nums`-wide skip in the Part-8 section would
    /// wrongly drop the legitimate part8 container's referencing pages too.
    /// Mirrors the `objstm-lin-outlines-otherpage` byte fixture (flpdf-7aek).
    #[test]
    fn canonical_shared_hints_skips_part9_outline_container_in_part8_section() {
        // Container A (20): a part9 container — holds an in_outlines member
        // (`outline`, in `part9_outline_objects`), a `part4_other_pages_shared`
        // member (`shared_a`), and a first-page-private member (`fp_priv`). The
        // first-page member keeps A out of `part8_container_nums` (so the
        // enumeration tail cannot re-add it), exactly as the single-container
        // byte fixture's container carries page-0's private fonts.
        let fp_priv = ObjectRef::new(2, 0);
        let outline = ObjectRef::new(6, 0);
        let shared_a = ObjectRef::new(7, 0);
        // Container B (21): a legitimate part8 (other-page-shared) container.
        let shared_b = ObjectRef::new(8, 0);
        let plan = LinearizationPlan {
            part3_objects: vec![fp_priv],
            part4_other_pages_shared: vec![shared_a, shared_b],
            // `outline` is routed to part9 (Rest) by the !UseOutlines priority.
            part9_outline_objects: vec![outline],
            shared_hints: vec![
                // first-page section (input_idx < first_page_input = 1):
                SharedObjectHintEntry {
                    object_ref: fp_priv,
                    referencing_pages: vec![],
                },
                // Part-8 section (input_idx >= first_page_input):
                SharedObjectHintEntry {
                    object_ref: shared_a,
                    referencing_pages: vec![1],
                },
                SharedObjectHintEntry {
                    object_ref: shared_b,
                    referencing_pages: vec![2],
                },
            ],
            ..Default::default()
        };

        let mut m2c: BTreeMap<ObjectRef, (u32, u32)> = BTreeMap::new();
        m2c.insert(fp_priv, (20, 0));
        m2c.insert(outline, (20, 1));
        m2c.insert(shared_a, (20, 2));
        m2c.insert(shared_b, (21, 0));

        // Both A and B are Part-4 (second-half) containers.
        let mut second_half: BTreeSet<u32> = BTreeSet::new();
        second_half.insert(20);
        second_half.insert(21);

        let renumber = RenumberMap::from_plan(&plan);
        let folded =
            plan.canonical_shared_hints(&m2c, &renumber, &second_half, &Default::default());

        // Container A (20, part9) must be absent.
        assert!(
            folded
                .iter()
                .all(|e| e.object_ref != ObjectRef::new(20, u16::MAX)),
            "part9 outline-routed container 20 must be skipped in the Part-8 section"
        );
        // Container B (21, part8) must be kept AND carry the pages folded from its
        // `shared_hints` member (the union from the main loop, NOT the empty list
        // a naive skip + enumeration re-add would leave). `all_referenced_pages`
        // is empty here, so the recompute tail is a no-op and this distinguishes
        // the correct fold from a naive `second_half`-wide skip.
        let b_entry = folded
            .iter()
            .find(|e| e.object_ref == ObjectRef::new(21, u16::MAX))
            .expect("legitimate part8 container 21 must be kept");
        assert_eq!(
            b_entry.referencing_pages,
            vec![2],
            "part8 container 21 must keep its folded referencing page (not be \
             dropped and re-added empty by a naive second_half-wide skip)"
        );
    }

    /// A part9 (Rest) container with NO first-page member (e.g. page 0 carries no
    /// ObjStm-eligible private object) is dropped by the main-loop guard, but
    /// `part8_container_nums` (keyed on page reachability) would otherwise re-add
    /// it in the enumeration tail. The tail's `rest_container_nums` guard must
    /// prevent that re-add. Covers the `objstm-lin-outlines-otherpage-0-60-20`
    /// byte fixture (flpdf-7aek, Codex P2).
    #[test]
    fn canonical_shared_hints_part9_container_not_re_added_by_part8_enumeration() {
        let outline = ObjectRef::new(6, 0);
        let shared_a = ObjectRef::new(7, 0);
        let plan = LinearizationPlan {
            // No part2/part3: the first page contributes no shared/private ObjStm
            // member, so container 20 has no first-page member.
            part4_other_pages_shared: vec![shared_a],
            part9_outline_objects: vec![outline],
            shared_hints: vec![SharedObjectHintEntry {
                object_ref: shared_a,
                referencing_pages: vec![1, 2],
            }],
            ..Default::default()
        };

        // Container 20 holds the in_outlines member (→ part9) and the shared font.
        let mut m2c: BTreeMap<ObjectRef, (u32, u32)> = BTreeMap::new();
        m2c.insert(outline, (20, 0));
        m2c.insert(shared_a, (20, 1));
        let mut second_half: BTreeSet<u32> = BTreeSet::new();
        second_half.insert(20);

        let renumber = RenumberMap::from_plan(&plan);
        let folded =
            plan.canonical_shared_hints(&m2c, &renumber, &second_half, &Default::default());

        // `part8_container_nums` classifies 20 as part8 (no first-page member, has
        // a shared member), but the enumeration-tail guard must keep it out.
        assert!(
            plan.part8_container_nums(&m2c).contains(&20),
            "precondition: part8_container_nums would otherwise re-add container 20"
        );
        assert!(
            folded
                .iter()
                .all(|e| e.object_ref != ObjectRef::new(20, u16::MAX)),
            "part9 container 20 (no first-page member) must not be re-added by the \
             part8 enumeration tail"
        );
    }

    // -- Linearized generate-mode membership + container part routing --------
    //
    // These exercise `objstm_membership_linearized` + `route_objstm_containers`
    // against the qpdf-11.9.0-measured ground truth (the Rust builders below are
    // ports of docs/plans/tools/gen_mixed_shared.py and gen_three_page_shared.py;
    // the routing assertions match `qpdf --linearize --object-streams=generate`).

    /// Serialize objects (sorted by number) into a classic-xref PDF body with a
    /// `/Root 1 0 R /Info 6 0 R`-style trailer. `info_obj` is the /Info object
    /// number; `max_obj` is the highest object number present.
    fn assemble_classic_pdf(bodies: Vec<(u32, String)>, info_obj: u32, max_obj: u32) -> Vec<u8> {
        let mut bodies = bodies;
        bodies.sort_by_key(|(n, _)| *n);
        let mut pdf: Vec<u8> = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.5\n%\xe2\xe3\xcf\xd3\n");
        let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();
        for (n, body) in &bodies {
            offsets.insert(*n, pdf.len() as u64);
            pdf.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
        }
        let xref_start = pdf.len() as u64;
        let size = max_obj + 1;
        let mut xref = format!("xref\n0 {size}\n0000000000 65535 f \n");
        for n in 1..size {
            xref.push_str(&format!("{:010} 00000 n \n", offsets[&n]));
        }
        pdf.extend_from_slice(xref.as_bytes());
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {size} /Root 1 0 R /Info {info_obj} 0 R >>\n\
                 startxref\n{xref_start}\n%%EOF\n"
            )
            .as_bytes(),
        );
        pdf
    }

    /// Port of `gen_mixed_shared.py s p`: 2 pages, page 0 references `s` shared
    /// fonts, page 1 references those `s` shared fonts plus `p` page-1-only fonts,
    /// trailer carries /Info. Layout: 1=Catalog 2=Pages 3=Page0 4=Page1 5=Info,
    /// 6..=5+s shared fonts, then `p` page-1-only fonts, then the two content
    /// streams. Font keys `/S#`,`/P#` sort lexically (matching the python).
    fn mixed_shared_pdf_bytes(s: u32, p: u32) -> Vec<u8> {
        let shared0 = 6u32;
        let p1only0 = shared0 + s;
        let c0 = p1only0 + p;
        let c1 = c0 + 1;
        let shared_res: String = (0..s)
            .map(|i| format!("/S{} {} 0 R", i + 1, shared0 + i))
            .collect::<Vec<_>>()
            .join(" ");
        let p1_extra: String = (0..p)
            .map(|i| format!("/P{} {} 0 R", i + 1, p1only0 + i))
            .collect::<Vec<_>>()
            .join(" ");
        let mut bodies: Vec<(u32, String)> = vec![
            (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
            (2, "<< /Type /Pages /Count 2 /Kids [ 3 0 R 4 0 R ] >>".to_string()),
            (5, "<< /Producer (flpdf-g6hb mixed fixture) >>".to_string()),
            (3, format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << {shared_res} >> >> /Contents {c0} 0 R >>"
            )),
            (4, format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << {shared_res} {p1_extra} >> >> /Contents {c1} 0 R >>"
            )),
        ];
        for i in 0..s {
            let n = shared0 + i;
            bodies.push((
                n,
                format!(
                    "<< /Type /Font /Subtype /Type1 /BaseFont /S{} /Mark {n} >>",
                    i + 1
                ),
            ));
        }
        for i in 0..p {
            let n = p1only0 + i;
            bodies.push((
                n,
                format!(
                    "<< /Type /Font /Subtype /Type1 /BaseFont /P{} /Mark {n} >>",
                    i + 1
                ),
            ));
        }
        for (cnum, label) in [(c0, "Page0"), (c1, "Page1")] {
            let stream = format!("BT /S1 12 Tf 72 720 Td ({label}) Tj ET");
            bodies.push((
                cnum,
                format!(
                    "<< /Length {} >>\nstream\n{stream}\nendstream",
                    stream.len()
                ),
            ));
        }
        assemble_classic_pdf(bodies, 5, c1)
    }

    /// Port of `gen_three_page_shared.py p0 g`: 3 pages, page 0 references `p0`
    /// private fonts, pages 1 AND 2 both reference the same `g` shared fonts
    /// (reach {1,2}, never page 0). Layout: 1=Catalog 2=Pages 3=Page0 4=Page1
    /// 5=Page2 6=Info, 7..=6+p0 page-0 fonts, then `g` shared fonts, then three
    /// content streams.
    fn three_page_shared_pdf_bytes(p0: u32, g: u32) -> Vec<u8> {
        let p0_0 = 7u32;
        let g0 = p0_0 + p0;
        let c0 = g0 + g;
        let c1 = c0 + 1;
        let c2 = c1 + 1;
        let p0_res: String = (0..p0)
            .map(|i| format!("/A{} {} 0 R", i + 1, p0_0 + i))
            .collect::<Vec<_>>()
            .join(" ");
        let g_res: String = (0..g)
            .map(|i| format!("/G{} {} 0 R", i + 1, g0 + i))
            .collect::<Vec<_>>()
            .join(" ");
        let mut bodies: Vec<(u32, String)> = vec![
            (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
            (2, "<< /Type /Pages /Count 3 /Kids [ 3 0 R 4 0 R 5 0 R ] >>".to_string()),
            (6, "<< /Producer (flpdf-g6hb three-page shared fixture) >>".to_string()),
            (3, format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << {p0_res} >> >> /Contents {c0} 0 R >>"
            )),
            (4, format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << {g_res} >> >> /Contents {c1} 0 R >>"
            )),
            (5, format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << {g_res} >> >> /Contents {c2} 0 R >>"
            )),
        ];
        for i in 0..p0 {
            let n = p0_0 + i;
            bodies.push((
                n,
                format!(
                    "<< /Type /Font /Subtype /Type1 /BaseFont /A{} /Mark {n} >>",
                    i + 1
                ),
            ));
        }
        for i in 0..g {
            let n = g0 + i;
            bodies.push((
                n,
                format!(
                    "<< /Type /Font /Subtype /Type1 /BaseFont /G{} /Mark {n} >>",
                    i + 1
                ),
            ));
        }
        for (cnum, label) in [(c0, "Page0"), (c1, "Page1"), (c2, "Page2")] {
            let stream = format!("BT /A1 12 Tf 72 720 Td ({label}) Tj ET");
            bodies.push((
                cnum,
                format!(
                    "<< /Length {} >>\nstream\n{stream}\nendstream",
                    stream.len()
                ),
            ));
        }
        assemble_classic_pdf(bodies, 6, c2)
    }

    #[test]
    fn linearized_membership_even_splits_then_erases_page_dicts_and_root() {
        // gen_mixed_shared 60 70: 135 eligible => 2 streams (68 + 67); erase
        // Catalog + Page0 + Page1 (all in stream 0) => 65 + 67 members.
        let mut pdf = Pdf::open(Cursor::new(mixed_shared_pdf_bytes(60, 70))).unwrap();
        let assigned = LinearizationPlan::from_pdf(&mut pdf, true)
            .unwrap()
            .renumber_assigned_refs();
        let containers = objstm_membership_linearized(&mut pdf, &assigned).unwrap();
        assert_eq!(
            containers.len(),
            2,
            "135 eligible => 2 even-split containers"
        );
        assert_eq!(
            containers[0].len(),
            65,
            "stream 0 loses Catalog+Page0+Page1"
        );
        assert_eq!(containers[1].len(), 67, "stream 1 untouched by the erasure");
        // No page dict or root survives in any container.
        let root = pdf.root_ref().unwrap();
        let pages: BTreeSet<ObjectRef> = crate::pages::page_refs(&mut pdf)
            .unwrap()
            .into_iter()
            .collect();
        for c in &containers {
            for m in c {
                assert!(*m != root && !pages.contains(m), "{m:?} must be erased");
            }
        }
    }

    #[test]
    fn linearized_routes_mixed_shared_first_page_then_other_page_private() {
        // qpdf measured: stream 0 (shared fonts + Pages + Info) => part6;
        // stream 1 (page-1-only fonts) => part7.
        let mut pdf = Pdf::open(Cursor::new(mixed_shared_pdf_bytes(60, 70))).unwrap();
        let assigned = LinearizationPlan::from_pdf(&mut pdf, true)
            .unwrap()
            .renumber_assigned_refs();
        let containers = objstm_membership_linearized(&mut pdf, &assigned).unwrap();
        let routes = route_objstm_containers(&mut pdf, &containers).unwrap();
        assert_eq!(
            routes,
            vec![ContainerPart::FirstPage, ContainerPart::OtherPagePrivate]
        );
    }

    #[test]
    fn linearized_routes_three_page_shared_first_page_then_other_page_shared() {
        // qpdf measured: stream 0 (page-0-private fonts + Pages + Info) => part6;
        // stream 1 (fonts shared by pages 1 & 2, reach {1,2}) => part8.
        let mut pdf = Pdf::open(Cursor::new(three_page_shared_pdf_bytes(2, 120))).unwrap();
        let assigned = LinearizationPlan::from_pdf(&mut pdf, true)
            .unwrap()
            .renumber_assigned_refs();
        let containers = objstm_membership_linearized(&mut pdf, &assigned).unwrap();
        let routes = route_objstm_containers(&mut pdf, &containers).unwrap();
        assert_eq!(
            routes,
            vec![ContainerPart::FirstPage, ContainerPart::OtherPageShared]
        );
    }

    #[test]
    fn linearized_routes_trailer_only_container_to_rest() {
        // A container whose members reach no page (here /Info, which is
        // trailer-only and not in any page closure) is qpdf part 9 — lc_other.
        // This does not arise from the even split on a font corpus (/Info is
        // DFS-early and co-located with first-page objects), but the routing is
        // exercised directly here.
        let mut pdf = Pdf::open(Cursor::new(mixed_shared_pdf_bytes(60, 70))).unwrap();
        let info_ref = pdf.trailer().get_ref("Info").unwrap();
        let synthetic = vec![vec![info_ref]];
        let routes = route_objstm_containers(&mut pdf, &synthetic).unwrap();
        assert_eq!(routes, vec![ContainerPart::Rest]);
    }

    /// A catalog `/OpenAction` whose action's `/D` destination reaches a `/Page`
    /// leaf (`[3 0 R /Fit]`) and whose `/Next` reaches a dict that references one
    /// object twice. Exercises [`open_document_set`]'s page-boundary stop (the
    /// page is dropped) and the `visited`-dedup short-circuit (the twice-referenced
    /// object is queued twice but recorded once).
    fn open_action_page_dest_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.5\n");
        let mut offs = [0u64; 7];
        let mut push = |pdf: &mut Vec<u8>, n: usize, body: &str| {
            offs[n] = pdf.len() as u64;
            pdf.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
        };
        push(
            &mut pdf,
            1,
            "<< /Type /Catalog /OpenAction 5 0 R /Pages 2 0 R >>",
        );
        push(&mut pdf, 2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push(
            &mut pdf,
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
        );
        // Referenced only from the action's /Next; references object 6 twice.
        push(&mut pdf, 4, "<< /A 6 0 R /B 6 0 R >>");
        // The action: /D reaches the page (boundary), /Next reaches object 4.
        push(
            &mut pdf,
            5,
            "<< /Type /Action /S /GoTo /D [3 0 R /Fit] /Next 4 0 R >>",
        );
        push(&mut pdf, 6, "<< /Leaf true >>");

        let xref_start = pdf.len() as u64;
        let mut xref = String::from("xref\n0 7\n0000000000 65535 f \n");
        for off in offs.iter().skip(1) {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        pdf.extend_from_slice(xref.as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn open_document_set_stops_at_page_and_dedups() {
        let mut pdf = Pdf::open(Cursor::new(open_action_page_dest_pdf_bytes())).unwrap();
        let set = open_document_set(&mut pdf).unwrap();
        // The action (5), its /Next dict (4), and the twice-referenced leaf (6)
        // are open-document; the /Page leaf (3) is dropped at the boundary and
        // the /Pages tree (2) is never reached (not an open-document key).
        let r = |n: u32| ObjectRef::new(n, 0);
        assert_eq!(
            set,
            [r(4), r(5), r(6)].into_iter().collect::<BTreeSet<_>>(),
            "open-document set = {{action, /Next dict, leaf}}; page leaf dropped"
        );
    }

    /// A catalog whose keys span every [`document_other_set`] / [`closure_from_seeds`]
    /// branch: included keys (`/Names`, `/Custom`), excluded keys (`/Outlines`,
    /// `/OpenAction`), the `/Pages` tree, a trailer `/Info`, and a `/Fake` key
    /// aliasing a `/Type /Pages` dict OUTSIDE the real page tree. The real page
    /// tree node carries an inherited `/Resources` (obj 10, SKIPPED like qpdf's
    /// stripped tree) plus a custom `/Ext` key (obj 11, FOLLOWED); the fake
    /// out-of-tree `/Pages` dict (obj 12) is NOT stripped, so its `/Resources`
    /// (obj 13) IS followed. `/Custom` references one object twice for `visited`
    /// dedup.
    fn document_other_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.5\n");
        let mut offs = [0u64; 14];
        let mut push = |pdf: &mut Vec<u8>, n: usize, body: &str| {
            offs[n] = pdf.len() as u64;
            pdf.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
        };
        // /Outlines, /OpenAction are excluded; /Names, /Custom, the /Pages custom
        // key, and /Fake (an out-of-tree /Pages alias) are `others`.
        push(
            &mut pdf,
            1,
            "<< /Type /Catalog /Pages 2 0 R /Outlines 7 0 R /OpenAction 8 0 R \
             /Names 4 0 R /Custom 5 0 R /Fake 12 0 R >>",
        );
        // Real page-tree node: inherited /Resources (-> 10, skipped) + custom /Ext
        // (-> 11, followed); /Kids reaches the page leaf (dropped at the boundary).
        push(
            &mut pdf,
            2,
            "<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources << /Inh 10 0 R >> /Ext 11 0 R >>",
        );
        push(
            &mut pdf,
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
        );
        push(&mut pdf, 4, "<< /X 6 0 R >>"); // /Names target -> 6
        push(&mut pdf, 5, "<< /A 6 0 R /B 6 0 R >>"); // /Custom target -> 6 twice
        push(&mut pdf, 6, "<< /Leaf true >>");
        push(&mut pdf, 7, "<< /Type /Outlines >>"); // excluded (/Outlines)
        push(&mut pdf, 8, "<< /Type /Action /S /GoTo /D [3 0 R /Fit] >>"); // excluded (open-doc)
        push(&mut pdf, 9, "<< /Producer (x) >>"); // trailer /Info -> included
        push(&mut pdf, 10, "<< /InheritedResource true >>"); // page-tree /Resources -> SKIPPED
        push(&mut pdf, 11, "<< /CustomExtension true >>"); // page-tree /Ext -> FOLLOWED
                                                           // Out-of-tree /Type /Pages dict (reached via /Fake, not /Root/Pages/Kids):
                                                           // qpdf does NOT strip it, so its /Resources target (13) is followed.
        push(
            &mut pdf,
            12,
            "<< /Type /Pages /Resources << /FakeRes 13 0 R >> >>",
        );
        push(&mut pdf, 13, "<< /NotStripped true >>");
        let xref_start = pdf.len() as u64;
        let mut xref = String::from("xref\n0 14\n0000000000 65535 f \n");
        for off in offs.iter().skip(1) {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        pdf.extend_from_slice(xref.as_bytes());
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size 14 /Root 1 0 R /Info 9 0 R >>\nstartxref\n{xref_start}\n%%EOF\n"
            )
            .as_bytes(),
        );
        pdf
    }

    #[test]
    fn document_other_set_excludes_doc_keys_and_stops_at_page() {
        let mut pdf = Pdf::open(Cursor::new(document_other_pdf_bytes())).unwrap();
        let set = document_other_set(&mut pdf).unwrap();
        let r = |n: u32| ObjectRef::new(n, 0);
        // Included: /Names (4) + target (6), /Custom (5) + the same target (6,
        // recorded once), trailer /Info (9), the real /Pages node (2) + its custom
        // /Ext target (11), and the out-of-tree /Fake /Pages dict (12) + its
        // /Resources target (13, NOT stripped since 12 is outside the page tree).
        // Excluded: /Outlines (7), /OpenAction (8), catalog/Root (1), the page
        // leaf (3, boundary), and the page-tree node's inherited /Resources target
        // (10, skipped like qpdf's stripped tree).
        assert_eq!(
            set,
            [r(2), r(4), r(5), r(6), r(9), r(11), r(12), r(13)]
                .into_iter()
                .collect::<BTreeSet<_>>(),
            "others = {{/Names, /Custom, /Info, page-tree node + custom /Ext, \
             out-of-tree /Pages + its /Resources}}; inherited /Resources (10), \
             page leaf (3), and doc keys excluded"
        );
    }

    /// A catalog `/Outlines` -> outline dict -> one item. [`outlines_set`] must
    /// collect the outline dict + item and exclude the page tree.
    fn outlines_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.5\n");
        let mut offs = [0u64; 7];
        let mut push = |pdf: &mut Vec<u8>, n: usize, body: &str| {
            offs[n] = pdf.len() as u64;
            pdf.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
        };
        push(
            &mut pdf,
            1,
            "<< /Type /Catalog /Outlines 5 0 R /Pages 2 0 R >>",
        );
        push(&mut pdf, 2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push(
            &mut pdf,
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
        );
        // 4 is unused (keeps numbering parallel to the other crafted fixtures).
        push(&mut pdf, 4, "<< /Unused true >>");
        push(
            &mut pdf,
            5,
            "<< /Type /Outlines /First 6 0 R /Last 6 0 R /Count 1 >>",
        );
        push(&mut pdf, 6, "<< /Title (Item) /Parent 5 0 R >>");
        let xref_start = pdf.len() as u64;
        let mut xref = String::from("xref\n0 7\n0000000000 65535 f \n");
        for off in offs.iter().skip(1) {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        pdf.extend_from_slice(xref.as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn outlines_set_collects_outline_tree_and_excludes_pages() {
        let mut pdf = Pdf::open(Cursor::new(outlines_pdf_bytes())).unwrap();
        let set = outlines_set(&mut pdf).unwrap();
        let r = |n: u32| ObjectRef::new(n, 0);
        assert_eq!(
            set,
            [r(5), r(6)].into_iter().collect::<BTreeSet<_>>(),
            "outlines set = {{outline dict, item}}; pages tree excluded"
        );
    }

    #[test]
    fn outlines_set_empty_when_no_outlines() {
        // The two-page fixture has no /Outlines key, so the set is empty (no /O).
        let mut pdf = Pdf::open(Cursor::new(two_page_shared_font_bytes())).unwrap();
        assert!(outlines_set(&mut pdf).unwrap().is_empty());
    }

    #[test]
    fn outlines_set_empty_for_non_dictionary_catalog() {
        // A /Root resolving to a non-dictionary yields no outline seeds (the
        // `if let Some(Object::Dictionary(..))` does not match).
        let mut pdf = Pdf::open(Cursor::new(non_dictionary_root_pdf_bytes())).unwrap();
        assert!(outlines_set(&mut pdf).unwrap().is_empty());
    }

    // Builds a variant of outlines_pdf_bytes() with /PageMode injected into
    // the catalog. Rebuilds the xref table so offsets remain valid.
    fn outlines_pdf_bytes_with_page_mode(mode: &[u8]) -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.5\n");
        let mut offs = [0u64; 7];
        let mut push = |pdf: &mut Vec<u8>, n: usize, body: &[u8]| {
            offs[n] = pdf.len() as u64;
            pdf.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
            pdf.extend_from_slice(body);
            pdf.extend_from_slice(b"\nendobj\n");
        };
        let catalog = format!(
            "<< /Type /Catalog /PageMode /{} /Outlines 5 0 R /Pages 2 0 R >>",
            std::str::from_utf8(mode).unwrap()
        );
        push(&mut pdf, 1, catalog.as_bytes());
        push(&mut pdf, 2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push(
            &mut pdf,
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
        );
        push(&mut pdf, 4, b"<< /Unused true >>");
        push(
            &mut pdf,
            5,
            b"<< /Type /Outlines /First 6 0 R /Last 6 0 R /Count 1 >>",
        );
        push(&mut pdf, 6, b"<< /Title (Item) /Parent 5 0 R >>");
        let xref_start = pdf.len() as u64;
        let mut xref = String::from("xref\n0 7\n0000000000 65535 f \n");
        for off in offs.iter().skip(1) {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        pdf.extend_from_slice(xref.as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn outlines_in_first_page_predicate_true_when_use_outlines_and_outlines_present() {
        let mut pdf = Pdf::open(Cursor::new(outlines_pdf_bytes_with_page_mode(
            b"UseOutlines",
        )))
        .unwrap();
        assert!(
            outlines_in_first_page_predicate(&mut pdf).unwrap(),
            "/PageMode /UseOutlines + /Outlines => predicate must be true"
        );
    }

    #[test]
    fn outlines_in_first_page_predicate_false_without_page_mode() {
        let mut pdf = Pdf::open(Cursor::new(outlines_pdf_bytes())).unwrap();
        assert!(
            !outlines_in_first_page_predicate(&mut pdf).unwrap(),
            "/Outlines with no /PageMode => predicate must be false"
        );
    }

    #[test]
    fn outlines_in_first_page_predicate_false_when_page_mode_not_use_outlines() {
        let mut pdf = Pdf::open(Cursor::new(outlines_pdf_bytes_with_page_mode(
            b"FullScreen",
        )))
        .unwrap();
        assert!(
            !outlines_in_first_page_predicate(&mut pdf).unwrap(),
            "/PageMode /FullScreen (not UseOutlines) => predicate must be false"
        );
    }

    #[test]
    fn outlines_in_first_page_predicate_false_when_no_outlines() {
        // two_page_shared_font_bytes has no /Outlines in the catalog.
        let mut pdf = Pdf::open(Cursor::new(two_page_shared_font_bytes())).unwrap();
        assert!(
            !outlines_in_first_page_predicate(&mut pdf).unwrap(),
            "catalog without /Outlines => predicate must be false"
        );
    }

    #[test]
    fn linearized_routes_open_document_container_before_page_categories() {
        // A container holding an open-document member routes to part 4
        // (OpenDocument) even when it ALSO holds a first-page member — qpdf checks
        // in_open_document before in_first_page.
        let mut pdf = Pdf::open(Cursor::new(open_action_page_dest_pdf_bytes())).unwrap();
        // Object 5 is open-document; object 3 is the (first) page. A container
        // with both must route to OpenDocument (open-document precedence).
        let synthetic = vec![vec![ObjectRef::new(3, 0), ObjectRef::new(5, 0)]];
        let routes = route_objstm_containers(&mut pdf, &synthetic).unwrap();
        assert_eq!(routes, vec![ContainerPart::OpenDocument]);
    }

    #[test]
    fn route_objstm_containers_outlines_first_page_routes_to_first_page() {
        // Outline container routes to FirstPage when /PageMode /UseOutlines is set.
        // Object 5 = outline dict, object 6 = outline item in outlines_pdf_bytes.
        let mut pdf = Pdf::open(Cursor::new(outlines_pdf_bytes_with_page_mode(
            b"UseOutlines",
        )))
        .unwrap();
        let outline_ref = ObjectRef::new(5, 0); // outline dict
        let synthetic = vec![vec![outline_ref]];
        let routes = route_objstm_containers(&mut pdf, &synthetic).unwrap();
        assert_eq!(
            routes,
            vec![ContainerPart::FirstPage],
            "outline container must route to FirstPage when /PageMode /UseOutlines"
        );
    }

    #[test]
    fn route_objstm_containers_outlines_no_use_outlines_routes_to_rest() {
        // Without /PageMode /UseOutlines, outline containers stay in Rest (part9).
        let mut pdf = Pdf::open(Cursor::new(outlines_pdf_bytes())).unwrap();
        let outline_ref = ObjectRef::new(5, 0); // outline dict
        let synthetic = vec![vec![outline_ref]];
        let routes = route_objstm_containers(&mut pdf, &synthetic).unwrap();
        assert_eq!(
            routes,
            vec![ContainerPart::Rest],
            "outline container must route to Rest when no /PageMode /UseOutlines"
        );
    }

    /// A `/Root` that resolves to a NON-dictionary (a malformed catalog) yields
    /// no open-document seeds — the `if let Some(Object::Dictionary(..))` does not
    /// match and the helper returns an empty set. `Pdf::open` tolerates this (the
    /// catalog-type error only surfaces during page enumeration).
    fn non_dictionary_root_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.5\n");
        let mut offs = [0u64; 4];
        let mut push = |pdf: &mut Vec<u8>, n: usize, body: &str| {
            offs[n] = pdf.len() as u64;
            pdf.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
        };
        push(&mut pdf, 1, "42"); // /Root points here — an integer, not a dict
        push(&mut pdf, 2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push(
            &mut pdf,
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
        );
        let xref_start = pdf.len() as u64;
        let mut xref = String::from("xref\n0 4\n0000000000 65535 f \n");
        for off in offs.iter().skip(1) {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        pdf.extend_from_slice(xref.as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn open_document_set_empty_for_non_dictionary_catalog() {
        let mut pdf = Pdf::open(Cursor::new(non_dictionary_root_pdf_bytes())).unwrap();
        let set = open_document_set(&mut pdf).unwrap();
        assert!(
            set.is_empty(),
            "a non-dictionary catalog must yield no open-document objects, got {set:?}"
        );
    }

    #[test]
    fn document_other_set_empty_for_non_dictionary_catalog() {
        // /Root resolves to an integer (not a dict), so the catalog-key arm is
        // skipped; the trailer has only /Root and /Size, so no `others` seeds.
        let mut pdf = Pdf::open(Cursor::new(non_dictionary_root_pdf_bytes())).unwrap();
        let set = document_other_set(&mut pdf).unwrap();
        assert!(
            set.is_empty(),
            "a non-dictionary catalog must yield no document-other objects, got {set:?}"
        );
    }

    /// A page tree exercising every [`page_tree_node_refs`] branch: a `/Page`
    /// leaf (3), a nested childless `/Pages` node listed TWICE (4 — queued twice,
    /// so the second pop hits the `visited` short-circuit, and it has no `/Kids`
    /// so the `if let Some(kids)` is skipped), and a malformed non-dictionary
    /// `/Kids` entry (5).
    fn nested_page_tree_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.5\n");
        let mut offs = [0u64; 6];
        let mut push = |pdf: &mut Vec<u8>, n: usize, body: &str| {
            offs[n] = pdf.len() as u64;
            pdf.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
        };
        push(&mut pdf, 1, "<< /Type /Catalog /Pages 2 0 R >>");
        // Root /Pages /Kids: leaf (3), nested /Pages (4) listed twice, non-dict (5).
        push(
            &mut pdf,
            2,
            "<< /Type /Pages /Kids [3 0 R 4 0 R 4 0 R 5 0 R] /Count 1 >>",
        );
        push(
            &mut pdf,
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
        );
        push(&mut pdf, 4, "<< /Type /Pages /Parent 2 0 R /Count 0 >>"); // no /Kids
        push(&mut pdf, 5, "42"); // malformed non-dictionary /Kids entry -> skipped
        let xref_start = pdf.len() as u64;
        let mut xref = String::from("xref\n0 6\n0000000000 65535 f \n");
        for off in offs.iter().skip(1) {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        pdf.extend_from_slice(xref.as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn page_tree_node_refs_collects_interior_pages_only() {
        let mut pdf = Pdf::open(Cursor::new(nested_page_tree_pdf_bytes())).unwrap();
        let set = page_tree_node_refs(&mut pdf).unwrap();
        let r = |n: u32| ObjectRef::new(n, 0);
        // The two interior /Pages nodes (2, 4); the /Page leaf (3) and the
        // non-dictionary /Kids entry (5) are excluded. Node 4 is listed twice but
        // recorded once.
        assert_eq!(
            set,
            [r(2), r(4)].into_iter().collect::<BTreeSet<_>>(),
            "page tree = interior /Pages nodes only (leaf + non-dict kid excluded)"
        );
    }

    #[test]
    fn page_tree_node_refs_empty_for_non_dictionary_catalog() {
        // /Root is an integer, so there is no /Pages entry: empty page tree.
        let mut pdf = Pdf::open(Cursor::new(non_dictionary_root_pdf_bytes())).unwrap();
        assert!(page_tree_node_refs(&mut pdf).unwrap().is_empty());
    }

    /// One-page PDF whose catalog `/OpenAction` reaches a JavaScript action (obj
    /// 5) whose `/JS` stream (obj 6) has an INDIRECT `/Length` (`7 0 R`). The
    /// holder (obj 7) is reachable only via that `/Length` edge. flpdf-2vfg.
    fn od_indirect_length_pdf_bytes() -> Vec<u8> {
        let bodies: &[(u32, &[u8])] = &[
            (1, b"<< /Type /Catalog /Pages 2 0 R /OpenAction 5 0 R >>"),
            (2, b"<< /Type /Pages /Count 1 /Kids [ 3 0 R ] >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>",
            ),
            (4, b"<< /Length 5 >>\nstream\nBT ET\nendstream"),
            (5, b"<< /Type /Action /S /JavaScript /JS 6 0 R >>"),
            (
                6,
                b"<< /Length 7 0 R >>\nstream\napp.alert('hi');\nendstream",
            ),
            (7, b"16"),
        ];
        let mut out: Vec<u8> = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
        let total = bodies.len() as u32 + 1;
        let mut offsets = vec![0usize; total as usize];
        for (num, body) in bodies {
            offsets[*num as usize] = out.len();
            out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref_start = out.len();
        out.extend_from_slice(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes());
        for off in offsets.iter().skip(1) {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(format!("trailer\n<< /Size {total} /Root 1 0 R >>\n").as_bytes());
        out.extend_from_slice(format!("startxref\n{xref_start}\n%%EOF\n").as_bytes());
        out
    }

    #[test]
    fn generate_plan_drops_orphan_indirect_length_holder_and_writes() {
        let bytes = od_indirect_length_pdf_bytes();
        let mut pdf = Pdf::open(Cursor::new(bytes.clone())).unwrap();
        let plan = LinearizationPlan::from_pdf(&mut pdf, true).unwrap();

        // The /Length holder (obj 7), reachable only via the OD stream's indirect
        // /Length, must be excluded from the plan: every stream is written with a
        // direct /Length, so the holder orphans and qpdf garbage-collects it.
        assert!(
            !plan.all_assigned_refs().contains(&ObjectRef::new(7, 0)),
            "orphaned /Length holder must not be assigned to any linearization part"
        );

        // Writing must succeed: the now-dangling `7 0 R` /Length is direct-ized at
        // renumber time (before this fix it errored "no entry in RenumberMap").
        let renumber = RenumberMap::from_plan(&plan);
        let mut pdf2 = Pdf::open(Cursor::new(bytes)).unwrap();
        let opts = crate::WriteOptions {
            object_streams: crate::ObjectStreamMode::Generate,
            deterministic_id: true,
            newline_before_endstream: crate::NewlineBeforeEndstream::Never,
            ..Default::default()
        };
        let mut doc =
            crate::linearization::writer::write_linearized(&plan, &renumber, &mut pdf2, &opts)
                .expect("linearized write must succeed with the orphan holder dropped");
        doc.back_patch().expect("back-patch must succeed");
        // No stream may carry an indirect /Length in the output: every /Length is
        // direct, so the dropped holder is unreferenced.
        let out = doc.bytes;
        assert!(
            !out.windows(b"/Length 7 0 R".len())
                .any(|w| w == b"/Length 7 0 R"),
            "the OD stream's /Length must be direct-ized, not left as an indirect ref"
        );
    }

    /// Two-page PDF whose SECOND page's `/Contents` stream (obj 6) has an
    /// indirect `/Length` (`7 0 R`); the holder (obj 7) is reachable only via
    /// that page-2 closure edge. flpdf-2vfg / Codex review on PR #400.
    fn page2_contents_indirect_length_pdf_bytes() -> Vec<u8> {
        let bodies: &[(u32, &[u8])] = &[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Count 2 /Kids [ 3 0 R 4 0 R ] >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Resources << >> >>",
            ),
            (
                4,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources << >> >>",
            ),
            (5, b"<< /Length 5 >>\nstream\nBT ET\nendstream"),
            (6, b"<< /Length 7 0 R >>\nstream\nBT ET\nendstream"),
            (7, b"5"),
        ];
        let mut out: Vec<u8> = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
        let total = bodies.len() as u32 + 1;
        let mut offsets = vec![0usize; total as usize];
        for (num, body) in bodies {
            offsets[*num as usize] = out.len();
            out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref_start = out.len();
        out.extend_from_slice(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes());
        for off in offsets.iter().skip(1) {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(format!("trailer\n<< /Size {total} /Root 1 0 R >>\n").as_bytes());
        out.extend_from_slice(format!("startxref\n{xref_start}\n%%EOF\n").as_bytes());
        out
    }

    #[test]
    fn generate_plan_drops_orphan_length_holder_reached_via_later_page_closure() {
        // The holder (obj 7) is reached only through page 2's /Contents stream
        // /Length. It must be dropped from the later-page closure too, not just
        // the Part-4 universe — otherwise it lands in the per-page (part7) set.
        let mut pdf = Pdf::open(Cursor::new(page2_contents_indirect_length_pdf_bytes())).unwrap();
        let plan = LinearizationPlan::from_pdf(&mut pdf, true).unwrap();
        assert!(
            !plan.all_assigned_refs().contains(&ObjectRef::new(7, 0)),
            "a page-reachable orphaned /Length holder must not be assigned to any part"
        );
    }

    #[test]
    fn non_generate_plan_also_drops_orphan_indirect_length_holder() {
        // The orphan-holder pruning is NOT gated on generate mode: the linearized
        // writer emits a direct /Length in every object-stream mode, so qpdf GCs
        // the holder for plain `--linearize` (preserve) and `--object-streams=
        // disable` runs too. `use_generate_objstm = false` must still drop it.
        let mut pdf = Pdf::open(Cursor::new(od_indirect_length_pdf_bytes())).unwrap();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();
        assert!(
            !plan.all_assigned_refs().contains(&ObjectRef::new(7, 0)),
            "orphaned /Length holder must be dropped in non-generate mode too"
        );
    }

    /// Single-page PDF whose trailer references a missing `/Info` object
    /// (`99 0 R`, no xref entry). `compressible_objgens` excludes the dangling
    /// ref from the compressible set (qpdf treats a missing ref as null), so it
    /// never reaches the linearized ObjStm membership or the generate renumber
    /// walk. Mirrors the Codex Security PoC (`flpdf-4vpi`).
    fn missing_trailer_info_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 4\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n",
        );
        pdf.extend_from_slice(xref.as_bytes());
        // /Info 99 0 R has no object 99 in the body or xref.
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 100 /Root 1 0 R /Info 99 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    /// Single-page PDF with a real `/Info` (obj 4) whose trailer also carries `n`
    /// missing `/Junk` refs (objects `10..10+n`, none with an xref entry). qpdf
    /// drops each dangling ref before it can enter the compressible set, so it
    /// emits one ObjStm holding only the real members. `compressible_objgens`
    /// excludes the missing refs the same way: were they admitted, with `n` large
    /// enough they would cross the 100-member even-split boundary and scatter the
    /// two real members (the `/Info` dict and the `/Pages` tree) into separate
    /// containers (Codex review on PR #421). Byte-identical to
    /// `tests/fixtures/compat/objstm-lin-split-boundary.pdf`.
    fn info_and_missing_junk_pdf_bytes(n: u32) -> Vec<u8> {
        let mut junk = String::new();
        for i in 0..n {
            junk.push_str(&format!("/J{i} {} 0 R ", 10 + i));
        }
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(b"4 0 obj\n<< /Producer (flpdf-test) >>\nendobj\n");
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 5\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n{off4:010} 00000 n \n",
        );
        pdf.extend_from_slice(xref.as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 110 /Root 1 0 R /Info 4 0 R {junk}>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    /// Drive a linearized generate write end-to-end and return the back-patched
    /// bytes, asserting neither the planner nor the writer panics.
    fn write_linearized_generate(bytes: &[u8]) -> Vec<u8> {
        let mut pdf = Pdf::open(Cursor::new(bytes.to_vec())).unwrap();
        let plan = LinearizationPlan::from_pdf(&mut pdf, true).unwrap();
        let renumber = RenumberMap::from_plan(&plan);
        let opts = crate::WriteOptions {
            object_streams: crate::ObjectStreamMode::Generate,
            deterministic_id: true,
            newline_before_endstream: crate::NewlineBeforeEndstream::Never,
            ..Default::default()
        };
        let mut doc =
            crate::linearization::writer::write_linearized(&plan, &renumber, &mut pdf, &opts)
                .expect("linearized generate write must not panic on unplanned refs");
        doc.back_patch().expect("back-patch must succeed");
        doc.bytes
    }

    /// A missing trailer-only `/Info 99 0 R` must not receive a renumber slot or
    /// be batched into an ObjStm: `compressible_objgens` excludes the dangling
    /// ref, so it never reaches `place_objstm_members_per_half` without a slot
    /// (which would panic). End-to-end guard for the Codex Security PoC
    /// (`flpdf-4vpi`).
    #[test]
    fn objstm_batches_generate_drops_missing_trailer_only_ref() {
        let bytes = missing_trailer_info_pdf_bytes();
        let mut pdf = Pdf::open(Cursor::new(bytes.clone())).unwrap();
        let plan = LinearizationPlan::from_pdf(&mut pdf, true).unwrap();
        let missing = ObjectRef::new(99, 0);
        assert!(
            !plan.renumber_assigned_refs().contains(&missing),
            "missing /Info 99 0 R must not receive a renumber slot"
        );

        let batch_plan = plan.objstm_batches(&mut pdf, &generate_config()).unwrap();
        assert!(
            !batch_plan
                .open_document_batches
                .iter()
                .chain(&batch_plan.part3_batches)
                .chain(&batch_plan.part4_batches)
                .any(|batch| batch.contains(&missing)),
            "missing /Info 99 0 R must not be batched into a generated ObjStm"
        );

        // End-to-end: previously panicked at renumber.rs:place_objstm_members_per_half.
        write_linearized_generate(&bytes);
    }

    /// 100 missing `/Junk` refs must not inflate the even split. Dropping them
    /// from membership BEFORE the split keeps the two real members — the `/Info`
    /// dict (obj 4) and the `/Pages` tree (obj 2) — in a SINGLE container, the
    /// way qpdf does (one `/N 2` ObjStm). Filtering only AFTER the split would
    /// leave the inflated boundary in place and scatter them into two `/N 1`
    /// ObjStms (Codex review on PR #421).
    #[test]
    fn objstm_membership_keeps_real_members_together_despite_missing_refs() {
        let mut pdf = Pdf::open(Cursor::new(info_and_missing_junk_pdf_bytes(100))).unwrap();
        let assigned = LinearizationPlan::from_pdf(&mut pdf, true)
            .unwrap()
            .renumber_assigned_refs();

        let containers = objstm_membership_linearized(&mut pdf, &assigned).unwrap();

        let info = ObjectRef::new(4, 0);
        let pages = ObjectRef::new(2, 0);
        assert_eq!(
            containers.len(),
            1,
            "missing /Junk refs must not split the two real members across containers"
        );
        let members = &containers[0];
        assert!(
            members.contains(&info) && members.contains(&pages),
            "the single container must hold both real members (/Info + /Pages)"
        );
        assert!(
            !members.iter().any(|r| (10..110).contains(&r.number)),
            "no missing /Junk ref may survive into membership"
        );
    }
}
