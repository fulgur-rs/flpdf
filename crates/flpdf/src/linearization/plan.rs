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
            for (_k, v) in s.dict.iter() {
                collect_direct_refs(v, depth + 1, out)?;
            }
        }
        // Scalar types cannot contain refs.
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
) -> crate::Result<Vec<ObjectRef>> {
    let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut order: Vec<ObjectRef> = Vec::new();
    let mut queue: VecDeque<ObjectRef> = VecDeque::from([root]);

    while let Some(current) = queue.pop_front() {
        if !visited.insert(current) {
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
                        let mut seeds = Vec::new();
                        collect_direct_refs(resources, 0, &mut seeds)?;
                        // DFS via an explicit stack (no recursion) so deeply
                        // nested resource graphs cannot overflow the stack.
                        // The visited set bounds cycles; `is_page_tree_node`
                        // stops the walk if a resource value cross-links back
                        // into the page tree, so we never pull in sibling pages.
                        let mut stack: Vec<ObjectRef> = seeds.into_iter().rev().collect();
                        while let Some(r) = stack.pop() {
                            if !visited.insert(r) {
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
                            let mut child_refs = Vec::new();
                            collect_direct_refs(&child, 0, &mut child_refs)?;
                            // Push in reverse so the first reference is popped
                            // first, preserving left-to-right discovery order.
                            for cr in child_refs.into_iter().rev() {
                                if !visited.contains(&cr) {
                                    stack.push(cr);
                                }
                            }
                        }
                    }
                } // cov:ignore: llvm-cov attributes 0 to this `if is_page_leaf` closing brace; the block body (the /Resources DFS) runs and is covered above.
                for (k, v) in dict.iter() {
                    if k == b"Kids" {
                        // Pages → sibling pages — never follow.
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
                                let mut refs = Vec::new();
                                collect_direct_refs(pv, 0, &mut refs)?;
                                for r in refs {
                                    if !visited.contains(&r) {
                                        queue.push_back(r);
                                    }
                                }
                            }
                        }
                        continue;
                    }
                    let mut refs = Vec::new();
                    collect_direct_refs(v, 0, &mut refs)?;
                    for r in refs {
                        if !visited.contains(&r) {
                            queue.push_back(r);
                        }
                    }
                }
            }
        } else {
            let mut refs = Vec::new();
            collect_direct_refs(&obj, 0, &mut refs)?;
            for r in refs {
                if !visited.contains(&r) {
                    queue.push_back(r);
                }
            }
        }
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
    pub fn from_pdf<R: Read + Seek>(pdf: &mut Pdf<R>) -> crate::Result<Self> {
        // ----------------------------------------------------------------
        // Step 1: collect all known object refs (Part 4 initial state).
        // The free-list head at object 0 is excluded per ISO 32000-1 §7.5.4.
        // ----------------------------------------------------------------
        let all_refs: Vec<ObjectRef> = pdf
            .object_refs()
            .into_iter()
            .filter(|r| r.number != 0)
            .collect();

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
        // Step 2: collect page references.
        // Propagate page-tree errors so a malformed /Pages does not silently
        // produce an empty page_hints (which would corrupt downstream hint tables).
        // ----------------------------------------------------------------
        let page_refs: Vec<ObjectRef> = crate::pages::page_refs(pdf)?;

        // ----------------------------------------------------------------
        // Step 3: compute first-page closure
        // ----------------------------------------------------------------
        let first_page_closure: Vec<ObjectRef> = if let Some(&first_page) = page_refs.first() {
            compute_closure(pdf, first_page)?
        } else {
            Vec::new()
        };
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
            let closure = compute_closure(pdf, page_ref)?;
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
            if Some(*obj_ref) == first_page_ref {
                part2_objects.push(*obj_ref);
            } else if shared_page_indices.contains_key(obj_ref) {
                part3_objects.push(*obj_ref);
            } else {
                part2_objects.push(*obj_ref);
            }
        }

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
                    !part2_set.contains(r)
                        && !part3_set.contains(r)
                        && page_reach.get(r).copied() == Some(1)
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
        let mut part4_other_pages_private: Vec<ObjectRef> = Vec::new();
        let mut part4_other_pages_shared: Vec<ObjectRef> = Vec::new();
        let mut part4_rest: Vec<ObjectRef> = Vec::new();
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
            let reach = page_reach.get(&r).copied().unwrap_or(0);
            let in_first_page = first_page_set.contains(&r);
            if in_first_page {
                // Should have been in Part 2 or Part 3 — skip (defensive).
                continue;
            }
            if reach >= 2 {
                part4_other_pages_shared.push(r);
            } else {
                // reach == 0 or reach == 1 but not private (shouldn't happen
                // since per_page_private_objects captures all reach-1 non-first
                // objects).  Everything else goes to part9.
                part4_rest.push(r);
            }
        }

        debug_assert_eq!(
            part4_other_pages_private.len() + part4_other_pages_shared.len() + part4_rest.len(),
            part4_provisional.len(),
            "Part-4 sub-partition must preserve membership"
        );

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
        //   [part2 entries]  - first-page section private objects (page 0 owns
        //                      them by physical position; referencing_pages = [])
        //   [part3 entries]  - first-page section shared objects (also owned by
        //                      page 0 physically; referencing_pages lists pages
        //                      1..N that also use them, NOT page 0)
        //   [part4_shared]   - Part-4 shared objects (after /E; owned by no
        //                      page via physical position; referencing_pages lists
        //                      ALL pages that reference them)
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
            .chain(part4_shared_entries)
            .collect();

        Ok(Self {
            part1_objects: Vec::new(),
            part2_objects,
            part3_objects,
            part4_other_pages_private,
            part4_other_pages_shared,
            part4_rest,
            total_object_count,
            root_ref,
            pages_tree_ref,
            info_ref,
            page_hints,
            shared_hints,
            per_page_private_objects,
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
    /// (placed at the first member's position, with the `referencing_pages` of
    /// all that container's members unioned), and the second-and-later members
    /// of the same container are dropped.  Non-member entries are kept verbatim
    /// and in order.
    ///
    /// The container entry's `object_ref` carries the container's *new* object
    /// number with generation `u16::MAX` — a sentinel no live object uses,
    /// marking it as a synthetic container entry rather than a resolvable PDF
    /// object.  The shared-object / page-offset hint encoders
    /// key shared objects by their position in this list (qpdf assigns the
    /// physical object numbers positionally from the first page object), so the
    /// synthetic ref is never resolved through a
    /// [`RenumberMap`](crate::linearization::renumber::RenumberMap).
    ///
    /// An empty `member_to_container` yields a clone of `shared_hints` (the
    /// no-ObjStm / classic path is unchanged).
    pub(crate) fn canonical_shared_hints(
        &self,
        member_to_container: &BTreeMap<ObjectRef, (u32, u32)>,
    ) -> Vec<SharedObjectHintEntry> {
        if member_to_container.is_empty() {
            return self.shared_hints.clone();
        }

        // Position (index into the output list) at which each container was
        // first emitted, so later members of the same container fold into it.
        let mut container_pos: BTreeMap<u32, usize> = BTreeMap::new();
        let mut out: Vec<SharedObjectHintEntry> = Vec::with_capacity(self.shared_hints.len());

        for entry in &self.shared_hints {
            match member_to_container.get(&entry.object_ref) {
                Some(&(container_num, _idx)) => {
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
            .chain(&self.part4_rest)
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
            total_object_count: 0,
            root_ref: None,
            pages_tree_ref: None,
            info_ref: None,
            page_hints: Vec::new(),
            shared_hints: Vec::new(),
            per_page_private_objects: Vec::new(),
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
/// * `part3_batches` — containers that belong in the first-page section
///   (ISO 32000-1 Annex F Part 3: shared/catalog objects).
/// * `part4_batches` — containers that belong after `/E` (Part 4: remaining
///   document objects from `part4_other_pages_private`, `part4_other_pages_shared`,
///   and `part4_rest`).
///
/// ObjStm containers can never span the Part-3 / Part-4 boundary.
/// `part2_objects` (first-page closure exclusives) are **never** placed in
/// either batch list — they stay as plain indirect objects.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ObjStmBatchPlan {
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
    /// | `Generate` | Eligible Part-3 (first-page shared) objects are packed into `part3_batches`; eligible Part-4 objects are packed into `part4_batches` (then canonicalised, see Note). |
    /// | `Preserve` | Existing source ObjStm membership is re-used for Part-4, but any member in `part2_objects` or ineligible per [`is_eligible_for_objstm`] is silently dropped. Members that span the Part-3/Part-4 boundary are split into separate batches per part. If the source document contained no ObjStms, both batch lists are **empty** — Preserve does **not** fall through to Generate; it mirrors the behaviour of the non-linearized `writer::object_streams::plan_preserve` and qpdf's `--object-streams=preserve` semantics (preserve means "keep what was there", not "invent new ObjStms"). |
    ///
    /// **Note:** after the per-mode packing, the member set is canonicalised to
    /// qpdf 11.9.0's linearized layout (see `canonicalise_first_half_batch`):
    /// the `/Pages` tree and `/Info` dictionary are folded into the first-half
    /// (Part-3) batch and `/Catalog` is kept standalone, so the resulting
    /// container membership matches qpdf for both Generate and Preserve.
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

        let mut plan = match config.mode {
            ObjectStreamMode::Disable => unreachable!(),
            ObjectStreamMode::Generate => {
                self.objstm_batches_generate(pdf, config, &ctx, &length_exclusions)?
            }
            ObjectStreamMode::Preserve => {
                self.objstm_batches_preserve(pdf, config, &ctx, &length_exclusions)?
            }
        };

        // Canonicalise the first-half ObjStm member set to qpdf's linearized
        // layout.  qpdf 11.9.0 packs exactly the first page's shared resource
        // dicts (its `/Font` dict + `/Font`) PLUS the `/Pages` tree and `/Info`
        // dictionary into ONE first-half ObjStm container, and keeps the
        // document `/Catalog` standalone (uncompressed).  flpdf's planner emits
        // the first-page shared objects as `part3_batches` and would otherwise
        // route `/Pages`/`/Info`/`/Catalog` through `part4_batches`; here we
        // fold `/Pages` + `/Info` into the first-half (Part-3) batch and drop
        // `/Catalog`/`/Pages`/`/Info` from the Part-4 batches so the resulting
        // container membership matches qpdf for both Generate and Preserve.
        //
        // qpdf re-derives this canonical partition regardless of the source
        // ObjStm boundaries (observed: `--object-streams=preserve` on a
        // single-container source still emits the canonical first-half
        // container).  Apply it only when there is already a first-half
        // (Part-3) batch to augment — i.e. the document has first-page SHARED
        // objects (multi-page).  Single-page files have no first-page shared
        // objects (`part3_batches` is empty), so qpdf's first-half member set
        // does not apply: `/Pages` + `/Info` stay in the second-half Part-4
        // container, matching flpdf's prior single-page output (and avoiding a
        // first-half container that would leave the page-private resource dicts
        // — which flpdf keeps plain — straddling `/E`).  A Preserve run over a
        // source with NO ObjStms likewise has no batches, so nothing is folded.
        //
        // The document `/Catalog`, however, is NEVER compressed by qpdf in any
        // object-stream mode, so its Part-4 exclusion is unconditional — the
        // `/Pages` + `/Info` first-half fold is what depends on a first-half
        // batch existing.  Without this, a single-page / no-first-page-shared
        // Generate document (where `part3_batches` is empty and
        // `canonicalise_first_half_batch` is skipped) would pack an eligible
        // `/Catalog` into a Part-4 ObjStm container.
        if let Some(catalog_ref) = self.root_ref {
            Self::drop_from_part4_batches(&mut plan, &BTreeSet::from([catalog_ref]));
        }

        if !plan.part3_batches.is_empty() {
            // cov:ignore-start: the `?` error arm is unreachable in tests —
            // `canonicalise_first_half_batch` only propagates a
            // `pdf.resolve_borrowed` failure on /Info or the /Pages tree, which
            // does not occur for well-formed inputs.
            self.canonicalise_first_half_batch(
                pdf,
                &ctx,
                &length_exclusions,
                config.batch_size_cap.get(),
                &mut plan,
            )?;
            // cov:ignore-end
        }

        Ok(plan)
    }

    /// Remove every ref in `excluded` from each Part-4 ObjStm batch, dropping any
    /// batch left empty.  An empty `excluded` set is a harmless no-op.
    fn drop_from_part4_batches(plan: &mut ObjStmBatchPlan, excluded: &BTreeSet<ObjectRef>) {
        plan.part4_batches = std::mem::take(&mut plan.part4_batches)
            .into_iter()
            .filter_map(|batch| {
                let kept: Vec<ObjectRef> =
                    batch.into_iter().filter(|r| !excluded.contains(r)).collect();
                (!kept.is_empty()).then_some(kept)
            })
            .collect();
    }

    /// Fold the `/Pages` tree and `/Info` dictionary into the first-half
    /// (Part-3) ObjStm batch(es) and exclude `/Catalog`, `/Pages`, and `/Info`
    /// from the Part-4 batches, matching qpdf 11.9.0's linearized member set
    /// ({first-page `/Font` dict, `/Font`, `/Info`, `/Pages` tree} in one
    /// first-half container, `/Catalog` standalone).
    ///
    /// The combined first-half member set (existing Part-3 members, then
    /// `/Info`, then the `/Pages` tree) is re-chunked into batches of at most
    /// `cap` members, so the qpdf single-container layout holds for the default
    /// cap (100) while a smaller cap still produces conforming containers.
    ///
    /// Refs that are not eligible per [`is_eligible_for_objstm`] (or absent)
    /// are left as plain indirects; only eligible refs are moved.
    fn canonicalise_first_half_batch<R: Read + Seek>(
        &self,
        pdf: &mut Pdf<R>,
        ctx: &crate::writer::object_streams::EligibilityContext,
        length_exclusions: &BTreeSet<ObjectRef>,
        cap: usize,
        plan: &mut ObjStmBatchPlan,
    ) -> crate::Result<()> {
        // Objects qpdf never compresses in a linearized file: the document
        // Catalog stays a standalone indirect object.
        let catalog_ref = self.root_ref;

        // First-half shared dicts qpdf packs alongside the Part-3 font/resource
        // dicts: the /Info dictionary and the /Pages tree.  Collect the eligible
        // ones (skip free/length-exclusion/ineligible refs) in qpdf's order
        // (Info before Pages-tree, matching the measured member set
        // {Font dict, Font, Info, Pages-tree}).  Skip any already present in the
        // existing Part-3 batches so the fold is idempotent.
        let existing_part3: BTreeSet<ObjectRef> = plan
            .part3_batches
            .iter()
            .flat_map(|b| b.iter().copied())
            .collect();
        let mut first_half_extra: Vec<ObjectRef> = Vec::new();
        for candidate in [self.info_ref, self.pages_tree_ref].into_iter().flatten() {
            // cov:ignore-start: idempotency / exclusion guard — /Info and the
            // /Pages tree are never first-page SHARED objects (they are not in
            // the planner's Part-3 set) and are not indirect-/Length stream
            // targets, so neither predicate fires on real documents; the guard
            // keeps the fold idempotent if a caller ever pre-seeds them.
            if existing_part3.contains(&candidate) || length_exclusions.contains(&candidate) {
                continue;
            }
            // cov:ignore-end
            let obj = pdf.resolve_borrowed(candidate)?;
            if is_eligible_for_objstm(candidate, obj, ctx) {
                first_half_extra.push(candidate);
            }
        }

        // Exclude /Catalog, /Pages, /Info from the Part-4 batches: /Catalog
        // stays standalone, /Pages and /Info migrate into the first-half batch.
        let excluded_from_part4: BTreeSet<ObjectRef> = catalog_ref
            .into_iter()
            .chain([self.info_ref, self.pages_tree_ref].into_iter().flatten())
            .collect();
        // Filter the excluded refs out of every Part-4 batch (dropping any batch
        // that becomes empty).  When `excluded_from_part4` is empty this is a
        // harmless no-op, so no guard is needed — avoiding an unreachable
        // empty-set branch (a linearizable document always has a /Catalog).
        Self::drop_from_part4_batches(plan, &excluded_from_part4);

        // cov:ignore-start: defensive — this method is only invoked when
        // `part3_batches` is non-empty (multi-page docs with first-page shared
        // objects), and such docs always have an eligible /Pages tree (a plain
        // dict) to fold, so `first_half_extra` is non-empty in practice; the
        // early return guards the degenerate "no eligible /Info or /Pages" case.
        if first_half_extra.is_empty() {
            return Ok(());
        }
        // cov:ignore-end

        // Flatten the existing Part-3 members (preserving order), append the
        // extras, and re-chunk by the cap so no container exceeds the limit.
        // qpdf's default cap (100) keeps everything in one first-half container.
        let mut members: Vec<ObjectRef> = plan
            .part3_batches
            .iter()
            .flat_map(|b| b.iter().copied())
            .collect();
        members.extend(first_half_extra);
        plan.part3_batches = members.chunks(cap).map(|chunk| chunk.to_vec()).collect();

        Ok(())
    }

    /// Generate mode: pack eligible Part-3 and Part-4 objects into fresh ObjStm batches.
    fn objstm_batches_generate<R: Read + Seek>(
        &self,
        pdf: &mut Pdf<R>,
        config: &PlannerConfig,
        ctx: &crate::writer::object_streams::EligibilityContext,
        length_exclusions: &BTreeSet<ObjectRef>,
    ) -> crate::Result<ObjStmBatchPlan> {
        use crate::XrefOffset;

        let cap = config.batch_size_cap.get();

        // Build a free-ref exclusion set so we don't accidentally pack deleted
        // objects (resolves to Null but may be in object_refs()).
        let source_entries = pdf.source_xref_entries();
        let free_refs: BTreeSet<ObjectRef> = source_entries
            .iter()
            .filter_map(|(r, offset)| {
                if matches!(offset, XrefOffset::Free { .. }) {
                    Some(*r)
                } else {
                    None
                }
            })
            .collect();

        let part2_set: BTreeSet<ObjectRef> = self.part2_objects.iter().copied().collect();

        // Pack Part 3 eligible objects.
        let part3_batches = Self::pack_into_batches(
            self.part3_objects.iter().copied(),
            &part2_set,
            &free_refs,
            length_exclusions,
            ctx,
            pdf,
            cap,
        )?;

        // Pack Part 4 eligible objects.  Part 4 is NOT a single flat group: a
        // batch must never co-locate objects with different page ownership,
        // otherwise the resulting ObjStm container cannot be placed in a single
        // page-private span and the Page Offset Hint Table's object_count /
        // byte_length (which assume per-page ownership) and the linearization
        // order would be corrupted.  Batch each ownership group independently:
        //   (1) each non-first page's private objects (per_page_private_objects
        //       index >= 1; index 0 is part2_objects, already excluded),
        //   (2) part4_other_pages_shared (qpdf part8),
        //   (3) part4_rest (qpdf part9: pages tree / info / orphans).
        let mut part4_batches: Vec<Vec<ObjectRef>> = Vec::new();
        for page_private in self.per_page_private_objects.iter().skip(1) {
            part4_batches.extend(Self::pack_into_batches(
                page_private.iter().copied(),
                &part2_set,
                &free_refs,
                length_exclusions,
                ctx,
                pdf,
                cap,
            )?);
        }
        part4_batches.extend(Self::pack_into_batches(
            self.part4_other_pages_shared.iter().copied(),
            &part2_set,
            &free_refs,
            length_exclusions,
            ctx,
            pdf,
            cap,
        )?);
        part4_batches.extend(Self::pack_into_batches(
            self.part4_rest.iter().copied(),
            &part2_set,
            &free_refs,
            length_exclusions,
            ctx,
            pdf,
            cap,
        )?);

        Ok(ObjStmBatchPlan {
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
        let part3_set: BTreeSet<ObjectRef> = self.part3_objects.iter().copied().collect();
        // Only objects actually in the linearization plan's Part-4 set have a
        // RenumberMap entry. A source ObjStm may carry eligible-but-unplanned
        // objects (unreachable / trailer-only); batching those would make
        // ObjStmLayout::build fail with "has no renumber entry". Skip them.
        let part4_set: BTreeSet<ObjectRef> = self
            .part4_other_pages_private
            .iter()
            .chain(&self.part4_other_pages_shared)
            .chain(&self.part4_rest)
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
        let rest_set: BTreeSet<ObjectRef> = self.part4_rest.iter().copied().collect();
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
                if part3_set.contains(&obj_ref) {
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
            part3_batches,
            part4_batches,
        })
    }

    /// Helper: iterate `candidates`, filter by eligibility, and pack into cap-sized batches.
    ///
    /// Objects in `part2_set` or `free_refs` are skipped unconditionally.
    fn pack_into_batches<R: Read + Seek>(
        candidates: impl Iterator<Item = ObjectRef>,
        part2_set: &BTreeSet<ObjectRef>,
        free_refs: &BTreeSet<ObjectRef>,
        length_exclusions: &BTreeSet<ObjectRef>,
        ctx: &crate::writer::object_streams::EligibilityContext,
        pdf: &mut Pdf<R>,
        cap: usize,
    ) -> crate::Result<Vec<Vec<ObjectRef>>> {
        let mut current_batch: Vec<ObjectRef> = Vec::new();
        let mut batches: Vec<Vec<ObjectRef>> = Vec::new();

        for obj_ref in candidates {
            if part2_set.contains(&obj_ref) || free_refs.contains(&obj_ref) {
                continue;
            }
            if length_exclusions.contains(&obj_ref) {
                continue;
            }
            let obj = pdf.resolve_borrowed(obj_ref)?;
            if !is_eligible_for_objstm(obj_ref, obj, ctx) {
                continue;
            }
            current_batch.push(obj_ref);
            if current_batch.len() >= cap {
                batches.push(std::mem::take(&mut current_batch));
            }
        }
        if !current_batch.is_empty() {
            batches.push(current_batch);
        }

        Ok(batches)
    }
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
        let plan = LinearizationPlan::from_pdf(&mut pdf).expect("plan construction must succeed");
        assert!(plan.total_object_count > 0);
    }

    // -----------------------------------------------------------------------
    // 2. Struct fields have expected types / accessors
    // -----------------------------------------------------------------------
    #[test]
    fn plan_fields_accessible() {
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();

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
        let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();

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

    // -----------------------------------------------------------------------
    // 4. Part 4 receives objects not in Part 2 or 3
    // -----------------------------------------------------------------------
    #[test]
    fn part4_contains_only_remaining_objects() {
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();

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
        let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();
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
        let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();

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
        let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();

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
        let plan =
            LinearizationPlan::from_pdf(&mut pdf).expect("cycle PDF must not cause infinite loop");

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
        let plan = LinearizationPlan::from_pdf(&mut pdf).expect("plan");
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
        let plan = LinearizationPlan::from_pdf(&mut pdf).expect("plan");
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
        let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();

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
        let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();

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
        let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();

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
        let result = LinearizationPlan::from_pdf(&mut pdf);
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
        let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();

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
        let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();

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
        let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();

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
        let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();

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
        let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();

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
        let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();

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
        let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();

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
        let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();

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
        let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();

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

        // ── Invariant 4: /Pages tree is folded into the first-half batch ──────
        // 2 0 R (Pages) is folded into part3_batches (the qpdf member set), so
        // it must NOT remain in part4_batches.  4 0 R (Page 2) is a Part-4
        // page-private dict and stays in part4_batches at the planner level
        // (the writer drops it later via the page-private filter).
        let pages_ref = ObjectRef::new(2, 0);
        assert!(
            all_part3_batched.contains(&pages_ref),
            "the /Pages tree (2 0 R) must be folded into part3_batches"
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
        // qpdf 11.9.0 packs the first-page shared Resources dict (5 0 R) plus
        // the /Pages tree (2 0 R) into the first-half ObjStm; ObjStm 8
        // contributes Page 2 (4 0 R) to part4.  With cap=1 the combined
        // first-half member set is re-chunked so each batch holds at most one
        // member.
        let bytes = two_page_two_objstm_pdf_bytes();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("two-ObjStm PDF should parse");
        let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();

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
            "2 0 R (/Pages tree, folded into the first-half batch) must be batched even with cap=1"
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
    fn objstm_two_part4_members_pdf_bytes() -> Vec<u8> {
        let mut bytes = b"%PDF-1.5\n".to_vec();

        let catalog_offset = bytes.len() as u32;
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let pages_offset = bytes.len() as u32;
        bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        let page1_offset = bytes.len() as u32;
        bytes.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        // ObjStm #7: members [10, 11] — two eligible plain dicts, referenced by
        // nothing, so both land in part4_rest (same part, same source ObjStm).
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
        let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();

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
        let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();

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
    /// empty and `canonicalise_first_half_batch` never runs): the document
    /// `/Catalog` must still be excluded from the Part-4 ObjStm batches, because
    /// qpdf never compresses the Catalog.  Guards the unconditional Catalog
    /// exclusion in `objstm_batches` against regressing back under the
    /// `part3_batches`-non-empty gate.
    #[test]
    fn objstm_batches_generate_keeps_catalog_standalone_single_page() {
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();

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

        let folded = plan.canonical_shared_hints(&m2c);
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
        let folded = plan.canonical_shared_hints(&BTreeMap::new());
        assert_eq!(folded, plan.shared_hints);
    }
}
