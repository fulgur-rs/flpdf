//! Page Offset Hint Table data structure (ISO 32000-1 Annex F.3.1).
//!
//! This module builds the **data** for the Page Offset Hint Table.  It does
//! **not** encode the table as bits/bytes — that is the responsibility of the
//! hint-stream encoder.
//!
//! # Structure overview (Annex F.3.1)
//!
//! The hint table consists of:
//!
//! * A **header** with 13 integer items describing the ranges and bit widths
//!   needed to encode the per-page entries (5 × 32-bit + 8 × 16-bit = 36 bytes).
//! * **N per-page entries** (one per page), each with 7 items.
//!
//! # Back-patch discipline
//!
//! Several fields depend on byte offsets that are only known after the file has
//! been written:
//!
//! | Field | Location | Placeholder |
//! |-------|----------|-------------|
//! | `header.least_page_length` (item 6) | header | `0` |
//! | `header.location_of_first_page` (item 2) | header | `0` |
//! | `entry.page_length_minus_least` (item 2) per page | entries | `0` |
//!
//! These fields are stored as `0` in the returned structs.  The back-patcher
//! locates them by field name and overwrites them once the real
//! byte offsets are available.
//!
//! # Object count
//!
//! `LinearizationPlan` computes `object_count` for page 0 as
//! `part2_objects.len() + part3_objects.len()` — all objects in the
//! first-page section (Part-2 private + Part-3 shared) written before `/E`.
//! For pages 1..N the count reflects only the page-private objects in
//! `per_page_private_objects[i]`.

use super::plan::LinearizationPlan;
use super::renumber::RenumberMap;
use crate::ObjectRef;

// ---------------------------------------------------------------------------
// Bit-width helper
// ---------------------------------------------------------------------------

/// Return the number of bits required to represent `value`.
///
/// Follows the formula from ISO 32000-1 Annex F:
/// `bits_needed = 0` if `value == 0`, otherwise `64 - value.leading_zeros()`.
///
/// Examples:
/// ```
/// use flpdf::linearization::hint_page::bits_needed;
/// assert_eq!(bits_needed(0), 0);
/// assert_eq!(bits_needed(1), 1);
/// assert_eq!(bits_needed(2), 2);
/// assert_eq!(bits_needed(3), 2);
/// assert_eq!(bits_needed(7), 3);
/// assert_eq!(bits_needed(8), 4);
/// ```
pub fn bits_needed(value: u64) -> u32 {
    if value == 0 {
        0
    } else {
        64 - value.leading_zeros()
    }
}

// ---------------------------------------------------------------------------
// Header
// ---------------------------------------------------------------------------

/// Header for the Page Offset Hint Table (13 items per Annex F.3.1).
///
/// Items are numbered 1–13 in the spec; field names here follow the spec text.
/// The encoded format is 5 × 32-bit + 8 × 16-bit = **36 bytes** total.
///
/// ## Correct field order (5 × u32 + 8 × u16 = 36 bytes)
///
/// | Byte | Width | Item | Field |
/// |------|-------|------|-------|
/// | 0-3  | 32    | 1    | `least_object_count` |
/// | 4-7  | 32    | 2    | `location_of_first_page` |
/// | 8-9  | 16    | 3    | `bits_object_count_delta` |
/// | 10-13| 32    | 4    | `least_page_length` |
/// | 14-15| 16    | 5    | `bits_page_length_delta` |
/// | 16-19| 32    | 6    | `least_content_offset` |
/// | 20-21| 16    | 7    | `bits_content_offset_delta` |
/// | 22-25| 32    | 8    | `least_content_length` |
/// | 26-27| 16    | 9    | `bits_content_length_delta` |
/// | 28-29| 16    | 10   | `bits_shared_object_count` |
/// | 30-31| 16    | 11   | `bits_shared_object_id` |
/// | 32-33| 16    | 12   | `bits_numerator` |
/// | 34-35| 16    | 13   | `denominator` |
///
/// Note: `first_page_object_number` (the `/O` value) does **not** appear in
/// the hint stream header — it belongs only in the linearization parameter
/// dictionary.
///
/// ## Back-patch fields
///
/// * `location_of_first_page` (item 2): byte offset of the first page's page
///   object from the start of the file.  Set to `0` (placeholder); back-patched
///   once the real offsets are known.
/// * `least_page_length` (item 4): minimum page byte length across all pages.
///   Set to `0` (placeholder); back-patched once the real offsets are known.
/// * `least_content_offset` (item 6): minimum content stream offset.
///   Set to `0` (placeholder); back-patched once the real offsets are known.
/// * `least_content_length` (item 8): minimum content stream length.
///   Set to `0` (placeholder); back-patched once the real offsets are known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageOffsetHeader {
    /// Item 1 — Least number of objects in a page across all pages (32-bit).
    pub least_object_count: u32,

    /// Item 2 — Byte offset of the first page's page object from the start of
    /// the file (32-bit).
    ///
    /// **Placeholder: 0; back-patched once the real offsets are known.**
    pub location_of_first_page: u64,

    /// Item 3 — Bits needed to represent the difference between the greatest
    /// and least number of objects in any page (16-bit).
    pub bits_object_count_delta: u32,

    /// Item 4 — Least page length in bytes (32-bit).
    ///
    /// **Placeholder: 0; back-patched once the real offsets are known.**
    pub least_page_length: u64,

    /// Item 5 — Bits needed to represent the difference between the greatest
    /// and least page length in bytes (16-bit).
    pub bits_page_length_delta: u32,

    /// Item 6 — Least content stream offset from the start of the page's data (32-bit).
    ///
    /// **Placeholder: 0; back-patched once the real offsets are known.**
    pub least_content_offset: u64,

    /// Item 7 — Bits needed to represent the difference between the greatest
    /// and least content stream offset (16-bit).
    pub bits_content_offset_delta: u32,

    /// Item 8 — Least content stream length in bytes (32-bit).
    ///
    /// **Placeholder: 0; back-patched once the real offsets are known.**
    pub least_content_length: u64,

    /// Item 9 — Bits needed to represent the difference between the greatest
    /// and least content stream length (16-bit).
    pub bits_content_length_delta: u32,

    /// Item 10 — Bits needed to represent the greatest number of shared object
    /// references for any page (16-bit).
    pub bits_shared_object_count: u32,

    /// Item 11 — Bits needed to represent the numerically greatest shared object
    /// identifier used by the pages (16-bit).
    pub bits_shared_object_id: u32,

    /// Item 12 — Bits needed to represent the numerator of the fractional
    /// position for each shared object reference (16-bit).
    pub bits_numerator: u32,

    /// Item 13 — Denominator of the fractional position for each shared object
    /// reference (16-bit).  qpdf uses 4 (implementation-defined per spec).
    pub denominator: u32,
}

// ---------------------------------------------------------------------------
// Per-page entry
// ---------------------------------------------------------------------------

/// Per-page entry for the Page Offset Hint Table (7 items per Annex F.3.1).
///
/// ## Back-patch fields
///
/// * `page_length_minus_least` (item 2): byte length of this page minus
///   `header.least_page_length`.  Set to `0` (placeholder); back-patched
///   once the real offsets are known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageOffsetEntry {
    /// Item 1 — Number of objects in this page minus
    /// `header.least_object_count`.
    pub object_count_minus_least: u32,

    /// Item 2 — Page length in bytes minus `header.least_page_length`.
    ///
    /// **Placeholder: 0; back-patched once the real offsets are known.**
    pub page_length_minus_least: u64,

    /// Item 3 — Number of shared object references for this page.
    pub shared_object_count: u32,

    /// Item 4..N — Identifiers (new object numbers) of the shared objects
    /// referenced by this page.
    ///
    /// Each value is the linearized (new) object number of a Part-3 shared
    /// object.  The order matches the Shared-object hint table.
    pub shared_object_ids: Vec<u32>,

    /// Item 5 — Numerators of the fractional position for each shared object
    /// reference (one per shared object in `shared_object_ids`).
    ///
    /// qpdf sets these to 0 (start of page); implementation-defined per spec.
    pub shared_object_numerators: Vec<u32>,

    /// Item 6 — Byte offset of the start of the content stream of this page,
    /// measured from the start of the page's data.
    ///
    /// Set to `0` (placeholder); content-stream offsets are only known after
    /// writing.
    pub content_stream_offset: u64,

    /// Item 7 — Length of the content stream of this page in bytes.
    ///
    /// Set to `0` (placeholder); content-stream lengths are only known after
    /// writing.
    pub content_stream_length: u64,
}

// ---------------------------------------------------------------------------
// PageOffsetHintTable
// ---------------------------------------------------------------------------

/// Complete Page Offset Hint Table: header + one entry per page.
///
/// Constructed via [`PageOffsetHintTable::from_plan`].  All placeholder fields
/// (`location_of_first_page`, `least_page_length`, `least_content_offset`,
/// `least_content_length`, per-page `page_length_minus_least`,
/// `content_stream_offset`, `content_stream_length`)
/// are initialized to `0` and back-patched once the real offsets are known.
///
/// The hint-stream encoder serializes this struct into the binary bit-packed
/// format required by Annex F.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageOffsetHintTable {
    /// The 13-item header (36 bytes: 5 × 32-bit + 8 × 16-bit).
    pub header: PageOffsetHeader,
    /// One entry per page, in page order (page 0 = first page).
    pub entries: Vec<PageOffsetEntry>,
}

/// Page-0 object count when first-page shared objects are packed into a
/// first-half ObjStm.
///
/// qpdf counts page 0's objects as the size of its first-page section (`part6_`
/// in qpdf), i.e. the objects physically before `/E`.  When the first-page
/// shared dicts are compressed, the ObjStm *container* (one object) replaces
/// its members there.  This returns
/// `|part2_objects| + |part3 objects left plain| + |distinct first-page
/// containers|` — the count of plain indirects plus containers in the
/// first-page section, with compressed members counted only via their
/// container.
fn page0_object_count_with_objstm(
    plan: &LinearizationPlan,
    member_to_container: &std::collections::BTreeMap<ObjectRef, (u32, u32)>,
) -> u32 {
    use std::collections::BTreeSet;

    // Part-2 objects are always plain indirects in the first-page section.
    let part2 = plan.part2_objects.len() as u32;

    // Part-3 objects either stay plain or are folded into a container.
    let mut part3_plain = 0u32;
    let mut first_page_containers: BTreeSet<u32> = BTreeSet::new();
    for r in &plan.part3_objects {
        match member_to_container.get(r) {
            Some(&(container_num, _)) => {
                first_page_containers.insert(container_num);
            }
            None => part3_plain += 1,
        }
    }

    // /Info and the /Pages tree are folded into the first-half ObjStm container
    // alongside the Part-3 members — but ONLY when a first-half (Part-3) batch
    // exists to canonicalize.  `canonicalise_first_half_batch` runs only when
    // `part3_batches` is non-empty (a multi-page document with first-page shared
    // objects), which is exactly when at least one Part-3 object was packed into
    // a first-half container, i.e. `first_page_containers` is non-empty here.
    //
    // In that case /Info and /Pages live in the first-half section: when they
    // exactly fill a separate chunk they form their own first-half container,
    // invisible to the `part3_objects` scan above (they are not Part-3 objects),
    // so it must be counted.  When there is NO first-half Part-3 container (e.g.
    // single-page `--object-streams=generate`), /Info and /Pages remain in the
    // second-half Part-4 batches and are emitted AFTER /E, so they must NOT be
    // counted toward page 0 even though they are compressed.
    if !first_page_containers.is_empty() {
        for r in [plan.info_ref, plan.pages_tree_ref].into_iter().flatten() {
            if let Some(&(container_num, _)) = member_to_container.get(&r) {
                first_page_containers.insert(container_num);
            }
        }
    }

    part2 + part3_plain + first_page_containers.len() as u32
}

impl PageOffsetHintTable {
    /// Build the Page Offset Hint Table from a [`LinearizationPlan`] and a
    /// [`RenumberMap`].
    ///
    /// # Placeholder conventions
    ///
    /// Fields that depend on byte offsets not yet known are set to `0` and
    /// must be filled in by the back-patcher:
    ///
    /// * `header.location_of_first_page`
    /// * `header.least_page_length`
    /// * `header.least_content_offset`
    /// * `header.least_content_length`
    /// * `entry.page_length_minus_least` (all entries)
    /// * `entry.content_stream_offset` (all entries)
    /// * `entry.content_stream_length` (all entries)
    ///
    /// # Panics
    ///
    /// Panics if any `page_hints[i].object_count == 0` (these values feed
    /// `least_object_count`, `bits_object_count_delta`, and per-entry
    /// `object_count_minus_least`; unlike byte-offset placeholders they are
    /// not back-patched, so a placeholder zero would bake an incorrect
    /// header / entries into the table).  Also panics if the plan has zero
    /// pages — these all indicate a malformed `LinearizationPlan` that the
    /// caller is expected to construct consistently.  The `_renumber` parameter
    /// is retained for API stability (the `RenumberMap` is no longer needed
    /// internally since `shared_object_ids` are now 0-based hint table indices).
    pub fn from_plan(
        plan: &LinearizationPlan,
        _renumber: &RenumberMap,
        member_to_container: &std::collections::BTreeMap<ObjectRef, (u32, u32)>,
    ) -> Self {
        assert!(
            !plan.page_hints.is_empty(),
            "PageOffsetHintTable::from_plan requires at least one page in the plan"
        );
        assert!(
            plan.page_hints.iter().all(|hint| hint.object_count > 0),
            "PageOffsetHintTable::from_plan requires finalized per-page object counts \
             (every page_hints[i].object_count must be > 0; ensure LinearizationPlan \
             populates counts for all pages, not just page 0)"
        );

        let page_count = plan.page_hints.len();

        // Shared-object list folded to match qpdf's hint table: first-page
        // ObjStm members are replaced by their container (one entry), so a page
        // that references a compressed first-page shared object points at the
        // container's index, and page 0's object_count counts the container —
        // not its members.  With no ObjStm packing this is `plan.shared_hints`.
        let shared_hints = plan.canonical_shared_hints(member_to_container);

        // ------------------------------------------------------------------
        // Step 1: collect object counts per page from the plan.
        //
        // Page 0's object_count is the number of objects physically in the
        // first-page section before /E.  When first-page shared objects are
        // packed into a first-half ObjStm, the container (one object) replaces
        // its members there, so subtract the folded members and add back one
        // per distinct first-page container — matching qpdf's `part6_` size.
        // ------------------------------------------------------------------
        let mut object_counts: Vec<u32> = plan.page_hints.iter().map(|h| h.object_count).collect();
        if !member_to_container.is_empty() {
            object_counts[0] = page0_object_count_with_objstm(plan, member_to_container);
        }

        let least_object_count = object_counts.iter().copied().min().unwrap_or(0);
        let greatest_object_count = object_counts.iter().copied().max().unwrap_or(0);
        let object_count_delta = (greatest_object_count - least_object_count) as u64;

        // ------------------------------------------------------------------
        // Step 2: compute shared-object counts per page.
        //
        // For each page i, count the number of (folded) shared objects that
        // list page i in their referencing_pages.
        // ------------------------------------------------------------------
        let mut shared_counts: Vec<u32> = vec![0u32; page_count];
        // Collect, per page, the list of 0-based indices into the folded shared
        // hint list for shared objects referencing that page.  Per qpdf's
        // checkHPageOffset algorithm, shared_identifiers are interpreted as
        // indices into the shared object hint table (not as new object numbers).
        let mut shared_ids_per_page: Vec<Vec<u32>> = vec![Vec::new(); page_count];

        for (shared_idx, shared_hint) in shared_hints.iter().enumerate() {
            for &page_idx in &shared_hint.referencing_pages {
                let idx = page_idx as usize;
                // Out-of-range page indexes indicate plan corruption (a
                // shared hint claims to be referenced by a page that doesn't
                // exist).  Silently dropping them undercounts shared
                // references and produces inconsistent hint table entries.
                assert!(
                    idx < page_count,
                    "shared hint object {:?} references out-of-range page index {} (page_count={})",
                    shared_hint.object_ref,
                    page_idx,
                    page_count
                );
                shared_counts[idx] += 1;
                shared_ids_per_page[idx].push(shared_idx as u32);
            }
        }

        // qpdf rejects page 0 entries that list shared identifiers
        // ("page 0 has shared identifier entries"): page 0 OWNS the shared
        // objects physically (they sit before /E in the first-page section),
        // so it does not need them in its hint-table entry.  Only pages
        // 1..N list the shared identifiers they reference.

        let greatest_shared_count = shared_counts.iter().copied().max().unwrap_or(0);

        // ------------------------------------------------------------------
        // Step 3: compute bits needed to encode shared object identifiers.
        //
        // qpdf computes nbits_per_shared_object as nbits(nshared_total) — the
        // number of bits needed to represent the COUNT of shared objects, not
        // the maximum 0-based index (which would be count - 1).  This matches
        // qpdf's writeHPageOffset / readHPageOffset behavior: for N shared
        // objects, qpdf allocates bits_needed(N) bits, one more than strictly
        // required to encode indices 0..N-1.  We match qpdf byte-for-byte by
        // using the count here.
        // ------------------------------------------------------------------
        let shared_hint_count = shared_hints.len() as u64;

        // ------------------------------------------------------------------
        // Step 4: build the header.
        //
        // Byte-dependent fields are set to 0 (placeholder).
        // Bit-width fields are derived from the deltas and max values computed
        // above.
        //
        // Note: `first_page_object_number` (the `/O` value in the param dict)
        // does NOT appear in the hint stream header per Annex F.3.1.  It is
        // stored separately in `LinearizedOffsets.first_page_object_new_num`.
        //
        // items 5 / bits_page_length_delta and 9 / bits_content_length_delta:
        // since all page_length and content values are 0 (placeholder) at this
        // stage, the deltas are 0 and bits = 0.  The writer's back-patch pass
        // updates the least_* fields, the bit-width fields, and the per-page
        // entries together.
        //
        // items 6 / least_content_offset and 7 / bits_content_offset_delta
        // remain 0 in the final stream too — qpdf does not compute real
        // content-stream offsets (see writer.rs / Adobe implementation note
        // 127).
        // ------------------------------------------------------------------
        let header = PageOffsetHeader {
            least_object_count,
            location_of_first_page: 0, // placeholder — back-patched by writer
            bits_object_count_delta: bits_needed(object_count_delta),
            least_page_length: 0,         // placeholder — back-patched by writer
            bits_page_length_delta: 0,    // placeholder — back-patched by writer
            least_content_offset: 0,      // qpdf-equivalent: always 0 in final output
            bits_content_offset_delta: 0, // qpdf-equivalent: always 0 in final output
            least_content_length: 0,      // placeholder — back-patched by writer
            bits_content_length_delta: 0, // placeholder — back-patched by writer
            bits_shared_object_count: bits_needed(greatest_shared_count as u64),
            bits_shared_object_id: bits_needed(shared_hint_count),
            bits_numerator: 0, // numerators are all 0 (qpdf default), so 0 bits needed
            denominator: 4,    // qpdf default; spec says implementation-defined
        };

        // ------------------------------------------------------------------
        // Step 6: build per-page entries.
        // ------------------------------------------------------------------
        let entries: Vec<PageOffsetEntry> = (0..page_count)
            .map(|i| {
                let obj_count = object_counts[i];
                let obj_count_minus_least = obj_count.saturating_sub(least_object_count);
                let count = shared_counts[i];
                let ids = shared_ids_per_page[i].clone();
                // One numerator (0) per shared object reference (qpdf convention).
                let numerators = vec![0u32; ids.len()];

                PageOffsetEntry {
                    object_count_minus_least: obj_count_minus_least,
                    page_length_minus_least: 0, // placeholder — back-patched by sub-task 2.9
                    shared_object_count: count,
                    shared_object_ids: ids,
                    shared_object_numerators: numerators,
                    content_stream_offset: 0, // placeholder
                    content_stream_length: 0, // placeholder
                }
            })
            .collect();

        Self { header, entries }
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linearization::plan::{LinearizationPlan, PageHintEntry, SharedObjectHintEntry};
    use crate::linearization::renumber::RenumberMap;
    use crate::ObjectRef;

    // -----------------------------------------------------------------------
    // Fixture helpers
    // -----------------------------------------------------------------------

    /// Single-page plan: 3 objects in Part 2, no shared objects.
    ///
    /// Part 2: [3 0 R, 2 0 R, 1 0 R]
    /// Part 3: []
    /// Part 4: []
    /// Pages:  [page_ref = 3 0 R, object_count = 3]
    fn single_page_plan() -> LinearizationPlan {
        let page_ref = ObjectRef::new(3, 0);
        LinearizationPlan {
            part2_objects: vec![
                ObjectRef::new(3, 0),
                ObjectRef::new(2, 0),
                ObjectRef::new(1, 0),
            ],
            total_object_count: 3,
            page_hints: vec![PageHintEntry {
                page_ref,
                first_object_index: 0,
                object_count: 3,
                byte_length: 0,
            }],
            ..Default::default()
        }
    }

    /// Two-page plan:
    ///
    /// Part 2 (page 0 exclusive): [3 0 R, 6 0 R]  → new numbers 2, 3
    /// Part 3 (shared):           [5 0 R, 8 0 R]  → new numbers 4, 5
    /// Part 4 (remaining):        [4 0 R, 7 0 R]  → new numbers 6, 7
    /// Page 0: page_ref = 3 0 R, object_count = 4 (2 Part-2 + 2 Part-3)
    /// Page 1: page_ref = 4 0 R, object_count = 5
    /// Shared hints (part2 first, then part3):
    ///   3 0 R (idx 0) → referencing_pages []   (part2, page 0 owns by layout)
    ///   6 0 R (idx 1) → referencing_pages []   (part2, page 0 owns by layout)
    ///   5 0 R (idx 2) → referencing_pages [1]  (part3, page 0 owns via layout; page 1 references)
    ///   8 0 R (idx 3) → referencing_pages [1]  (part3, page 1 references)
    fn two_page_plan_with_shared() -> LinearizationPlan {
        LinearizationPlan {
            part2_objects: vec![ObjectRef::new(3, 0), ObjectRef::new(6, 0)],
            part3_objects: vec![ObjectRef::new(5, 0), ObjectRef::new(8, 0)],
            part4_other_pages_private: vec![ObjectRef::new(4, 0), ObjectRef::new(7, 0)],
            total_object_count: 8,
            page_hints: vec![
                PageHintEntry {
                    page_ref: ObjectRef::new(3, 0),
                    first_object_index: 0,
                    object_count: 4, // 2 Part-2 + 2 Part-3 = all objects in first-page section
                    byte_length: 0,
                },
                PageHintEntry {
                    page_ref: ObjectRef::new(4, 0),
                    first_object_index: 0,
                    object_count: 5,
                    byte_length: 0,
                },
            ],
            shared_hints: vec![
                // part2 entries (referencing_pages = [] — page 0 owns by physical layout)
                SharedObjectHintEntry {
                    object_ref: ObjectRef::new(3, 0),
                    referencing_pages: vec![],
                },
                SharedObjectHintEntry {
                    object_ref: ObjectRef::new(6, 0),
                    referencing_pages: vec![],
                },
                // part3 entries (page 0 owns physically; only pages 1..N listed here)
                SharedObjectHintEntry {
                    object_ref: ObjectRef::new(5, 0),
                    referencing_pages: vec![1], // page 0 owns via physical layout
                },
                SharedObjectHintEntry {
                    object_ref: ObjectRef::new(8, 0),
                    referencing_pages: vec![1], // page 0 owns via physical layout
                },
            ],
            ..Default::default()
        }
    }

    // -----------------------------------------------------------------------
    // bits_needed helper unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn bits_needed_zero() {
        assert_eq!(bits_needed(0), 0);
    }

    #[test]
    fn bits_needed_one() {
        assert_eq!(bits_needed(1), 1);
    }

    #[test]
    fn bits_needed_two() {
        assert_eq!(bits_needed(2), 2);
    }

    #[test]
    fn bits_needed_three() {
        assert_eq!(bits_needed(3), 2);
    }

    #[test]
    fn bits_needed_seven() {
        assert_eq!(bits_needed(7), 3);
    }

    #[test]
    fn bits_needed_eight() {
        assert_eq!(bits_needed(8), 4);
    }

    #[test]
    fn bits_needed_large() {
        assert_eq!(bits_needed(255), 8);
        assert_eq!(bits_needed(256), 9);
        assert_eq!(bits_needed(u64::MAX), 64);
    }

    // -----------------------------------------------------------------------
    // Single-page fixture tests
    // -----------------------------------------------------------------------

    #[test]
    fn single_page_entries_len_is_one() {
        let plan = single_page_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber, &Default::default());

        assert_eq!(
            table.entries.len(),
            1,
            "single-page must have exactly 1 entry"
        );
    }

    #[test]
    fn single_page_all_deltas_zero_so_bit_widths_zero() {
        let plan = single_page_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber, &Default::default());

        // Single page → delta = 0 → bits = 0
        assert_eq!(table.header.bits_object_count_delta, 0);
        assert_eq!(table.header.bits_shared_object_count, 0);
        assert_eq!(table.header.bits_shared_object_id, 0);
    }

    #[test]
    fn single_page_least_object_count() {
        let plan = single_page_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber, &Default::default());

        assert_eq!(table.header.least_object_count, 3);
    }

    #[test]
    fn single_page_entry_object_count_minus_least_is_zero() {
        let plan = single_page_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber, &Default::default());

        assert_eq!(table.entries[0].object_count_minus_least, 0);
    }

    #[test]
    fn single_page_placeholder_fields_are_zero() {
        let plan = single_page_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber, &Default::default());

        assert_eq!(table.header.location_of_first_page, 0);
        assert_eq!(table.header.least_page_length, 0);
        assert_eq!(table.header.least_content_offset, 0);
        assert_eq!(table.header.least_content_length, 0);
        assert_eq!(table.entries[0].page_length_minus_least, 0);
        assert_eq!(table.entries[0].content_stream_offset, 0);
        assert_eq!(table.entries[0].content_stream_length, 0);
    }

    #[test]
    fn single_page_denominator_is_four() {
        let plan = single_page_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber, &Default::default());

        assert_eq!(
            table.header.denominator, 4,
            "denominator must be 4 (qpdf default)"
        );
    }

    #[test]
    fn single_page_no_shared_objects() {
        let plan = single_page_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber, &Default::default());

        assert_eq!(table.entries[0].shared_object_count, 0);
        assert!(table.entries[0].shared_object_ids.is_empty());
        assert!(table.entries[0].shared_object_numerators.is_empty());
    }

    // -----------------------------------------------------------------------
    // Two-page with shared objects fixture tests
    // -----------------------------------------------------------------------

    #[test]
    fn two_page_entries_len_is_two() {
        let plan = two_page_plan_with_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber, &Default::default());

        assert_eq!(table.entries.len(), 2, "two-page plan must have 2 entries");
    }

    #[test]
    fn two_page_least_object_count() {
        let plan = two_page_plan_with_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber, &Default::default());

        // min(4, 5) = 4 (page 0 object_count = Part-2 + Part-3 = 2+2 = 4)
        assert_eq!(table.header.least_object_count, 4);
    }

    #[test]
    fn two_page_bits_object_count_delta() {
        let plan = two_page_plan_with_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber, &Default::default());

        // delta = 5 - 4 = 1 → bits_needed(1) = 1
        assert_eq!(table.header.bits_object_count_delta, 1);
    }

    #[test]
    fn two_page_entry_object_count_minus_least() {
        let plan = two_page_plan_with_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber, &Default::default());

        // page 0: 4 - 4 = 0
        assert_eq!(table.entries[0].object_count_minus_least, 0);
        // page 1: 5 - 4 = 1
        assert_eq!(table.entries[1].object_count_minus_least, 1);
    }

    #[test]
    fn two_page_shared_object_count_per_page() {
        let plan = two_page_plan_with_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber, &Default::default());

        // qpdf rejects "page 0 has shared identifier entries" — page 0 owns
        // shared objects physically (they sit in the first-page section
        // before /E) so its entry must NOT list shared identifiers.  Only
        // pages 1..N list the shared objects they reference.
        assert_eq!(table.entries[0].shared_object_count, 0);
        assert_eq!(table.entries[1].shared_object_count, 2);
    }

    #[test]
    fn two_page_bits_shared_object_count() {
        let plan = two_page_plan_with_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber, &Default::default());

        // greatest shared count = 2 → bits_needed(2) = 2
        assert_eq!(table.header.bits_shared_object_count, 2);
    }

    #[test]
    fn two_page_bits_shared_object_id() {
        let plan = two_page_plan_with_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber, &Default::default());

        // shared_hints has 4 entries.
        // qpdf computes nbits_per_shared_object = nbits(nshared_total) = nbits(4) = 3,
        // using the COUNT of shared objects (not the max 0-based index 3 = nbits 2).
        // We match qpdf's byte-exact behavior by using bits_needed(count).
        assert_eq!(table.header.bits_shared_object_id, 3);
    }

    #[test]
    fn two_page_shared_object_ids_are_hint_indices() {
        let plan = two_page_plan_with_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber, &Default::default());

        // Page 0 must NOT list shared identifiers (qpdf rejects them).
        assert!(
            table.entries[0].shared_object_ids.is_empty(),
            "page 0 must not list shared identifiers"
        );

        // Page 1 references part3 entries (indices 2 and 3 in shared_hints).
        // shared_hints: [3 0 R (idx 0), 6 0 R (idx 1), 5 0 R (idx 2), 8 0 R (idx 3)]
        // Page 1 has referencing_pages = [1] for entries at idx 2 and 3.
        let mut ids1 = table.entries[1].shared_object_ids.clone();
        ids1.sort_unstable();
        assert_eq!(
            ids1,
            vec![2, 3],
            "page 1 shared object ids must be 0-based indices 2 and 3 into shared_hints"
        );
    }

    #[test]
    fn two_page_shared_numerators_are_zero() {
        let plan = two_page_plan_with_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber, &Default::default());

        for entry in &table.entries {
            for &num in &entry.shared_object_numerators {
                assert_eq!(num, 0, "numerators must all be 0 (qpdf convention)");
            }
        }
    }

    #[test]
    fn numerators_len_matches_shared_count() {
        let plan = two_page_plan_with_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber, &Default::default());

        for entry in &table.entries {
            assert_eq!(
                entry.shared_object_numerators.len(),
                entry.shared_object_ids.len(),
                "numerators count must match shared_object_ids count"
            );
        }
    }

    #[test]
    fn two_page_denominator_is_four() {
        let plan = two_page_plan_with_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber, &Default::default());

        assert_eq!(table.header.denominator, 4);
    }

    #[test]
    fn two_page_placeholder_fields_are_zero() {
        let plan = two_page_plan_with_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber, &Default::default());

        assert_eq!(table.header.location_of_first_page, 0);
        assert_eq!(table.header.least_page_length, 0);
        assert_eq!(table.header.least_content_offset, 0);
        assert_eq!(table.header.least_content_length, 0);
        for entry in &table.entries {
            assert_eq!(entry.page_length_minus_least, 0);
            assert_eq!(entry.content_stream_offset, 0);
            assert_eq!(entry.content_stream_length, 0);
        }
    }

    // -----------------------------------------------------------------------
    // bits_shared_object_id: count-based (qpdf-matching) formula
    //
    // qpdf computes nbits_per_shared_object = nbits(nshared_total), using the
    // COUNT of shared objects rather than the max 0-based index (count - 1).
    // For N shared hints the count gives bits_needed(N); max_id gives
    // bits_needed(N-1), which is one bit fewer for all N where N is a power
    // of 2 or one more than a power of 2.
    //
    // These tests pin the qpdf-matching count-based formula for each
    // boundary case.  Verified against qpdf --show-linearization output.
    // -----------------------------------------------------------------------

    #[test]
    fn bits_shared_object_id_zero_shared_hints() {
        // 0 shared hints → bits_needed(0) = 0 (same for both count and max_id)
        let plan = single_page_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber, &Default::default());
        assert_eq!(table.header.bits_shared_object_id, 0);
    }

    #[test]
    fn bits_shared_object_id_four_shared_hints_matches_qpdf() {
        // 4 shared hints:
        //   count-based (qpdf): bits_needed(4) = 3
        //   max_id-based:       bits_needed(3) = 2   ← wrong
        // Verified: qpdf --show-linearization reports nbits_shared_identifier=3
        // for a two-page.pdf fixture with 4 shared objects.
        let plan = two_page_plan_with_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber, &Default::default());
        assert_eq!(table.header.bits_shared_object_id, 3);
    }

    // -----------------------------------------------------------------------
    // page0_object_count_with_objstm: count container, not packed members;
    // count Part-3 objects left plain individually
    // -----------------------------------------------------------------------

    /// When some Part-3 objects are packed into a first-half container and others
    /// stay plain, page 0's object count = |part2| + |part3 left plain| +
    /// |distinct containers|.  This exercises both the packed (container) and
    /// the plain branch of `page0_object_count_with_objstm`.
    #[test]
    fn page0_count_mixes_plain_and_packed_part3() {
        let page = ObjectRef::new(3, 0);
        let content = ObjectRef::new(9, 0);
        let packed_a = ObjectRef::new(1, 0);
        let packed_b = ObjectRef::new(2, 0);
        let plain_part3 = ObjectRef::new(5, 0);
        let plan = LinearizationPlan {
            part2_objects: vec![page, content],
            part3_objects: vec![packed_a, packed_b, plain_part3],
            ..Default::default()
        };
        // packed_a + packed_b → container 12; plain_part3 left plain.
        let mut m2c: std::collections::BTreeMap<ObjectRef, (u32, u32)> =
            std::collections::BTreeMap::new();
        m2c.insert(packed_a, (12, 0));
        m2c.insert(packed_b, (12, 1));

        // |part2| (2) + |part3 plain| (1: plain_part3) + |containers| (1) = 4.
        assert_eq!(page0_object_count_with_objstm(&plan, &m2c), 4);
    }

    /// When the Part-3 members exactly fill their first-half batch, /Info and the
    /// /Pages tree are re-chunked into a SEPARATE first-half container.  That
    /// container is referenced only by /Info /Pages (not by `part3_objects`), so
    /// it must still be counted in page 0's object count — otherwise the
    /// first-page section is undercounted and `qpdf --check-linearization` fails.
    #[test]
    fn page0_count_includes_stranded_info_pages_container() {
        let page = ObjectRef::new(3, 0);
        let part3_a = ObjectRef::new(5, 0);
        let part3_b = ObjectRef::new(6, 0);
        let info = ObjectRef::new(7, 0);
        let pages = ObjectRef::new(8, 0);
        let plan = LinearizationPlan {
            part2_objects: vec![page],
            part3_objects: vec![part3_a, part3_b],
            info_ref: Some(info),
            pages_tree_ref: Some(pages),
            ..Default::default()
        };
        // Part-3 members fill container 12; /Info + /Pages strand into a separate
        // first-half container 13.
        let mut m2c: std::collections::BTreeMap<ObjectRef, (u32, u32)> =
            std::collections::BTreeMap::new();
        m2c.insert(part3_a, (12, 0));
        m2c.insert(part3_b, (12, 1));
        m2c.insert(info, (13, 0));
        m2c.insert(pages, (13, 1));

        // |part2| (1) + |part3 plain| (0) + |first-half containers| (2: 12, 13) = 3.
        assert_eq!(page0_object_count_with_objstm(&plan, &m2c), 3);
    }

    /// In the common case /Info + /Pages share the single first-half container
    /// with the Part-3 members, so the count is unchanged (no double counting).
    #[test]
    fn page0_count_info_pages_share_part3_container() {
        let page = ObjectRef::new(3, 0);
        let part3_a = ObjectRef::new(5, 0);
        let info = ObjectRef::new(7, 0);
        let pages = ObjectRef::new(8, 0);
        let plan = LinearizationPlan {
            part2_objects: vec![page],
            part3_objects: vec![part3_a],
            info_ref: Some(info),
            pages_tree_ref: Some(pages),
            ..Default::default()
        };
        // Everything in one first-half container 12 (qpdf default-cap layout).
        let mut m2c: std::collections::BTreeMap<ObjectRef, (u32, u32)> =
            std::collections::BTreeMap::new();
        m2c.insert(part3_a, (12, 0));
        m2c.insert(info, (12, 1));
        m2c.insert(pages, (12, 2));

        // |part2| (1) + |part3 plain| (0) + |containers| (1) = 2.
        assert_eq!(page0_object_count_with_objstm(&plan, &m2c), 2);
    }

    /// ObjStm enabled but no first-half Part-3 container (e.g. single-page
    /// `--object-streams=generate`, where `canonicalise_first_half_batch` does
    /// not run): /Info and /Pages stay in second-half Part-4 batches, emitted
    /// after /E.  They must NOT be counted toward page 0 even though they are
    /// compressed — counting them would over-report the first-page object count
    /// and break the page-offset hint table.
    #[test]
    fn page0_count_excludes_second_half_info_pages_when_no_first_half_container() {
        let page = ObjectRef::new(3, 0);
        let info = ObjectRef::new(7, 0);
        let pages = ObjectRef::new(8, 0);
        let plan = LinearizationPlan {
            part2_objects: vec![page],
            part3_objects: vec![], // single page: no first-page shared objects
            info_ref: Some(info),
            pages_tree_ref: Some(pages),
            ..Default::default()
        };
        // /Info + /Pages packed into a SECOND-half (Part-4) container 20.
        let mut m2c: std::collections::BTreeMap<ObjectRef, (u32, u32)> =
            std::collections::BTreeMap::new();
        m2c.insert(info, (20, 0));
        m2c.insert(pages, (20, 1));

        // |part2| (1) + |part3 plain| (0) + |first-half containers| (0) = 1.
        // The Part-4 container 20 (after /E) must not be counted.
        assert_eq!(page0_object_count_with_objstm(&plan, &m2c), 1);
    }
}
