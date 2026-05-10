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

use crate::{Object, ObjectRef, Pdf};
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
fn collect_direct_refs(obj: &Object, out: &mut Vec<ObjectRef>) {
    match obj {
        Object::Reference(r) => out.push(*r),
        Object::Array(arr) => {
            for elem in arr {
                collect_direct_refs(elem, out);
            }
        }
        Object::Dictionary(dict) => {
            for (_k, v) in dict.iter() {
                collect_direct_refs(v, out);
            }
        }
        Object::Stream(s) => {
            // Only walk the stream dictionary; do not scan raw data bytes.
            for (_k, v) in s.dict.iter() {
                collect_direct_refs(v, out);
            }
        }
        // Scalar types cannot contain refs.
        _ => {}
    }
}

/// Compute the transitive closure of objects reachable from `root`.
///
/// Returns the list in BFS discovery order (root first).
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
            if let Object::Dictionary(dict) = &obj {
                for (k, v) in dict.iter() {
                    if k == b"Kids" {
                        // Pages → sibling pages — never follow.
                        continue;
                    }
                    if k == b"Parent" {
                        // Walk the parent /Pages dict for inherited
                        // /Resources, /MediaBox, /Rotate, etc., but DO
                        // NOT add the parent object itself to the
                        // closure.  Adding it would inflate this page's
                        // object_count beyond what qpdf computes from the
                        // linearized layout (qpdf never counts ancestor
                        // /Pages dicts in any page's object_count).
                        //
                        // The walk follows non-/Kids, non-/Parent keys of
                        // the parent and descends into ref values
                        // recursively via the queue.  /Kids and /Parent
                        // on the parent are suppressed here to avoid
                        // pulling sibling pages or recursing into
                        // grandparents — for pages that genuinely
                        // inherit from multi-level page trees, multi-level
                        // ancestor walking can be added later.
                        let mut parent_refs = Vec::new();
                        collect_direct_refs(v, &mut parent_refs);
                        for parent_ref in parent_refs {
                            if let Ok(Object::Dictionary(parent_dict)) = pdf.resolve(parent_ref) {
                                for (pk, pv) in parent_dict.iter() {
                                    if pk == b"Kids" || pk == b"Parent" {
                                        continue;
                                    }
                                    let mut refs = Vec::new();
                                    collect_direct_refs(pv, &mut refs);
                                    for r in refs {
                                        if !visited.contains(&r) {
                                            queue.push_back(r);
                                        }
                                    }
                                }
                            }
                        }
                        continue;
                    }
                    let mut refs = Vec::new();
                    collect_direct_refs(v, &mut refs);
                    for r in refs {
                        if !visited.contains(&r) {
                            queue.push_back(r);
                        }
                    }
                }
            }
        } else {
            let mut refs = Vec::new();
            collect_direct_refs(&obj, &mut refs);
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
    /// Part 4: remaining body objects not in Parts 1–3.
    pub part4_objects: Vec<ObjectRef>,

    // ------------------------------------------------------------------
    // Document summary (copied from the source at construction time)
    // ------------------------------------------------------------------
    /// Total number of objects as reported by the xref table.
    pub total_object_count: u32,
    /// `/Root` reference from the trailer, if present.
    pub root_ref: Option<ObjectRef>,

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
    /// Returns an error if reading page references from the document fails.
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
        // For each object in the first-page closure, record which other
        // pages (by 0-based index) also reference it.  Closures are retained
        // so Step 7 can populate per-page object_count without re-walking.
        let mut shared_page_indices: BTreeMap<ObjectRef, BTreeSet<u32>> = BTreeMap::new();
        let mut other_page_closures: Vec<Vec<ObjectRef>> =
            Vec::with_capacity(page_refs.len().saturating_sub(1));

        for (page_idx, &page_ref) in page_refs.iter().enumerate().skip(1) {
            let closure = compute_closure(pdf, page_ref)?;
            for obj_ref in &closure {
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
        // Step 5: partition into Part 2 (exclusive) and Part 3 (shared)
        // ----------------------------------------------------------------
        // Maintain BFS order from first_page_closure for Part 2 (page dict
        // first, then resources, fonts, images, etc.).
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
        // Step 6b: order Part 4 so each page's private objects are
        // contiguous in plan / write order.
        //
        // qpdf's per-page length validator walks the file body forward
        // from the page object and stops at the first object that doesn't
        // belong to that page.  If page 1's content stream is interleaved
        // with page 2's page object, qpdf reports "page length mismatch".
        // The hint table records the COUNT of private objects, so the
        // writer must place them physically together.
        //
        // Order: per_page_private[1] then per_page_private[2] … then any
        // leftover Part-4 objects (globally shared between pages 1..N but
        // not page 0).
        let mut placed: BTreeSet<ObjectRef> = BTreeSet::new();
        let mut part4_objects: Vec<ObjectRef> = Vec::with_capacity(part4_provisional.len());
        for privates in per_page_private_objects.iter().skip(1) {
            for r in privates {
                if placed.insert(*r) {
                    part4_objects.push(*r);
                }
            }
        }
        for r in &part4_provisional {
            if placed.insert(*r) {
                part4_objects.push(*r);
            }
        }
        debug_assert_eq!(
            part4_objects.len(),
            part4_provisional.len(),
            "Part-4 reordering must preserve membership"
        );

        // ----------------------------------------------------------------
        // Step 8: build shared_hints
        // ----------------------------------------------------------------
        // `referencing_pages` lists the 0-based page indices of the pages
        // that reference this shared object VIA THE HINT TABLE.  Per qpdf's
        // Annex F implementation, page 0 does NOT appear here — shared objects
        // are physically owned by the first-page section (written before /E),
        // so page 0 "has" them by physical position without needing a hint
        // table reference.  Only pages 1..N that also use these objects appear
        // in `referencing_pages`.
        // Per qpdf's checkHSharedObject algorithm, the shared object hint table
        // must start at the first object of the first-page section (part2[0] =
        // page dict) and cover ALL first-page section objects before the truly
        // shared (part3) objects.  So when there are any part3 objects we
        // prepend part2 entries (referencing_pages = [] since page 0 physically
        // owns them) before the part3 entries.  When part3 is empty we keep
        // shared_hints empty (no shared objects at all).
        let shared_hints: Vec<SharedObjectHintEntry> = if part3_objects.is_empty() {
            Vec::new()
        } else {
            let part2_entries = part2_objects.iter().map(|&obj_ref| SharedObjectHintEntry {
                object_ref: obj_ref,
                referencing_pages: vec![],
            });
            let part3_entries = part3_objects.iter().map(|&obj_ref| {
                let pages: Vec<u32> = shared_page_indices
                    .get(&obj_ref)
                    .map(|s| s.iter().copied().collect())
                    .unwrap_or_default();
                // Do NOT add page 0: shared objects are in the first-page
                // section, so page 0 implicitly owns them by physical layout.
                SharedObjectHintEntry {
                    object_ref: obj_ref,
                    referencing_pages: pages,
                }
            });
            part2_entries.chain(part3_entries).collect()
        };

        Ok(Self {
            part1_objects: Vec::new(),
            part2_objects,
            part3_objects,
            part4_objects,
            total_object_count,
            root_ref,
            page_hints,
            shared_hints,
            per_page_private_objects,
        })
    }

    /// Return the set of all objects assigned to at least one part.
    ///
    /// Useful for callers that want to verify the disjoint invariant.
    pub fn all_assigned_refs(&self) -> BTreeSet<ObjectRef> {
        self.part1_objects
            .iter()
            .chain(&self.part2_objects)
            .chain(&self.part3_objects)
            .chain(&self.part4_objects)
            .copied()
            .collect()
    }

    /// Return `true` if every object appears in **at most** one part.
    pub fn parts_are_disjoint(&self) -> bool {
        let mut seen = BTreeSet::new();
        for r in self
            .part1_objects
            .iter()
            .chain(&self.part2_objects)
            .chain(&self.part3_objects)
            .chain(&self.part4_objects)
        {
            if !seen.insert(*r) {
                return false;
            }
        }
        true
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

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

        // shared_hints must be empty when Part 3 is empty.
        assert!(plan.shared_hints.is_empty());
    }

    // -----------------------------------------------------------------------
    // 4. Part 4 receives objects not in Part 2 or 3
    // -----------------------------------------------------------------------
    #[test]
    fn part4_contains_only_remaining_objects() {
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();

        // No object should appear in both Part 4 and Part 2/3.
        let part4_set: BTreeSet<_> = plan.part4_objects.iter().copied().collect();
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
            plan.part4_objects.contains(&page2_content),
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
}
