//! Page Offset Hint Table data structure (ISO 32000-1 Annex F.3.1).
//!
//! This module builds the **data** for the Page Offset Hint Table.  It does
//! **not** encode the table as bits/bytes — that is the responsibility of the
//! hint-stream encoder (sub-task 2.7).
//!
//! # Structure overview (Annex F.3.1)
//!
//! The hint table consists of:
//!
//! * A **header** with 12 integer items describing the ranges and bit widths
//!   needed to encode the per-page entries.
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
//! (sub-task 2.9) locates them by field name and overwrites them once the real
//! byte offsets are available.
//!
//! # Object count for pages > 0
//!
//! `LinearizationPlan` computes the precise `object_count` only for page 0
//! (= `part2_objects.len()`).  For pages 1..N the plan stores `0` as a
//! placeholder (back-patched by the layout writer).  `from_plan` therefore
//! uses `plan.page_hints[i].object_count` directly and may produce `0` for
//! pages beyond the first.  Complete multi-page object counts will be filled
//! in by a later subtask.

use super::plan::LinearizationPlan;
use super::renumber::RenumberMap;

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

/// Header for the Page Offset Hint Table (12 items per Annex F.3.1).
///
/// Items are numbered 1–12 in the spec; field names here follow the spec text.
///
/// ## Back-patch fields
///
/// * `location_of_first_page` (item 2): byte offset of the first page's page
///   object from the start of the file.  Set to `0` (placeholder); back-patched
///   by sub-task 2.9.
/// * `least_page_length` (item 6): minimum page byte length across all pages.
///   Set to `0` (placeholder); back-patched by sub-task 2.9.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageOffsetHeader {
    /// Item 1 — Object number of the first page's page object (new, linearized
    /// object number, i.e. the `/O` value in the parameter dictionary).
    pub first_page_object_number: u32,

    /// Item 2 — Byte offset of the first page's page object from the start of
    /// the file.
    ///
    /// **Placeholder: 0.  Back-patched by sub-task 2.9.**
    pub location_of_first_page: u64,

    /// Item 3 — Bits needed to represent the difference between the greatest
    /// and least number of objects in any page.
    pub bits_object_count_delta: u32,

    /// Item 4 — Least number of objects in a page across all pages.
    pub least_object_count: u32,

    /// Item 5 — Bits needed to represent the difference between the greatest
    /// and least page length in bytes.
    pub bits_page_length_delta: u32,

    /// Item 6 — Least page length in bytes.
    ///
    /// **Placeholder: 0.  Back-patched by sub-task 2.9.**
    pub least_page_length: u64,

    /// Item 7 — Bits needed to represent the greatest number of shared object
    /// references for any page.
    pub bits_shared_object_count: u32,

    /// Item 8 — Bits needed to represent the numerically greatest shared object
    /// identifier used by the pages.
    pub bits_shared_object_id: u32,

    /// Item 9 — Bits needed to represent the numerator of the fractional
    /// position for each shared object reference.
    pub bits_numerator: u32,

    /// Item 10 — Denominator of the fractional position for each shared object
    /// reference.  qpdf uses 1 (implementation-defined per spec).
    pub denominator: u32,

    /// Item 11 — Bits needed to represent the greatest content stream offset.
    pub bits_content_offset: u32,

    /// Item 12 — Bits needed to represent the greatest content stream length.
    pub bits_content_length: u32,
}

// ---------------------------------------------------------------------------
// Per-page entry
// ---------------------------------------------------------------------------

/// Per-page entry for the Page Offset Hint Table (7 items per Annex F.3.1).
///
/// ## Back-patch fields
///
/// * `page_length_minus_least` (item 2): byte length of this page minus
///   `header.least_page_length`.  Set to `0` (placeholder); back-patched by
///   sub-task 2.9.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageOffsetEntry {
    /// Item 1 — Number of objects in this page minus
    /// `header.least_object_count`.
    pub object_count_minus_least: u32,

    /// Item 2 — Page length in bytes minus `header.least_page_length`.
    ///
    /// **Placeholder: 0.  Back-patched by sub-task 2.9.**
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
/// (`location_of_first_page`, `least_page_length`, per-page
/// `page_length_minus_least`, `content_stream_offset`, `content_stream_length`)
/// are initialized to `0`; sub-task 2.9 back-patches them.
///
/// The sub-task 2.7 encoder serializes this struct into the binary bit-packed
/// format required by Annex F.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageOffsetHintTable {
    /// The 12-item header.
    pub header: PageOffsetHeader,
    /// One entry per page, in page order (page 0 = first page).
    pub entries: Vec<PageOffsetEntry>,
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
    /// * `entry.page_length_minus_least` (all entries)
    /// * `entry.content_stream_offset` (all entries)
    /// * `entry.content_stream_length` (all entries)
    ///
    /// # Object count for pages > 0
    ///
    /// The plan only computes a precise `object_count` for page 0.  For pages
    /// 1..N the plan stores `0` as a placeholder.  `from_plan` uses the plan
    /// values as-is; a future subtask will fill in the remaining counts.
    pub fn from_plan(plan: &LinearizationPlan, renumber: &RenumberMap) -> Self {
        let page_count = plan.page_hints.len();

        // ------------------------------------------------------------------
        // Step 1: collect object counts per page from the plan.
        // ------------------------------------------------------------------
        let object_counts: Vec<u32> = plan.page_hints.iter().map(|h| h.object_count).collect();

        let least_object_count = object_counts.iter().copied().min().unwrap_or(0);
        let greatest_object_count = object_counts.iter().copied().max().unwrap_or(0);
        let object_count_delta = (greatest_object_count - least_object_count) as u64;

        // ------------------------------------------------------------------
        // Step 2: compute shared-object counts per page.
        //
        // For each page i, count the number of Part-3 (shared) objects that
        // list page i in their referencing_pages.
        // ------------------------------------------------------------------
        let mut shared_counts: Vec<u32> = vec![0u32; page_count];
        // Also collect, per page, the list of new object numbers for shared
        // objects referencing that page (in shared_hints order).
        let mut shared_ids_per_page: Vec<Vec<u32>> = vec![Vec::new(); page_count];

        for shared_hint in &plan.shared_hints {
            // Resolve the new (linearized) object number for this shared object.
            let new_number = renumber
                .new_for_original(shared_hint.object_ref)
                .map(|r| r.number)
                .unwrap_or(0);

            for &page_idx in &shared_hint.referencing_pages {
                let idx = page_idx as usize;
                if idx < page_count {
                    shared_counts[idx] += 1;
                    shared_ids_per_page[idx].push(new_number);
                }
            }
        }

        let greatest_shared_count = shared_counts.iter().copied().max().unwrap_or(0);

        // ------------------------------------------------------------------
        // Step 3: compute the greatest shared object identifier (new number).
        // ------------------------------------------------------------------
        let greatest_shared_id = plan
            .shared_hints
            .iter()
            .filter_map(|h| renumber.new_for_original(h.object_ref))
            .map(|r| r.number as u64)
            .max()
            .unwrap_or(0);

        // ------------------------------------------------------------------
        // Step 4: first-page object new number (/O in the parameter dict).
        // ------------------------------------------------------------------
        let first_page_object_number = plan
            .page_hints
            .first()
            .and_then(|h| renumber.new_for_original(h.page_ref))
            .map(|r| r.number)
            .unwrap_or(2); // fallback to 2 (conventional default)

        // ------------------------------------------------------------------
        // Step 5: build the header.
        //
        // Byte-dependent fields are set to 0 (placeholder).
        // Bit-width fields are derived from the deltas and max values computed
        // above.
        //
        // item 5 / bits_page_length_delta: since all page_length values are 0
        // (placeholder) at this stage, the delta is 0 and bits = 0.  The
        // back-patcher will update least_page_length and each
        // page_length_minus_least; the encoder must re-derive bit widths at
        // encode time using the final values.
        // ------------------------------------------------------------------
        let header = PageOffsetHeader {
            first_page_object_number,
            location_of_first_page: 0, // placeholder — back-patched by sub-task 2.9
            bits_object_count_delta: bits_needed(object_count_delta),
            least_object_count,
            bits_page_length_delta: 0, // placeholder — re-derived by encoder after back-patch
            least_page_length: 0,      // placeholder — back-patched by sub-task 2.9
            bits_shared_object_count: bits_needed(greatest_shared_count as u64),
            bits_shared_object_id: bits_needed(greatest_shared_id),
            bits_numerator: 0, // numerators are all 0 (qpdf default), so 0 bits needed
            denominator: 1,    // qpdf default; spec says implementation-defined
            bits_content_offset: 0, // placeholder — re-derived by encoder after back-patch
            bits_content_length: 0, // placeholder — re-derived by encoder after back-patch
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
            part1_objects: vec![],
            part2_objects: vec![
                ObjectRef::new(3, 0),
                ObjectRef::new(2, 0),
                ObjectRef::new(1, 0),
            ],
            part3_objects: vec![],
            part4_objects: vec![],
            total_object_count: 3,
            root_ref: None,
            page_hints: vec![PageHintEntry {
                page_ref,
                first_object_index: 0,
                object_count: 3,
                byte_length: 0,
            }],
            shared_hints: vec![],
        }
    }

    /// Two-page plan:
    ///
    /// Part 2 (page 0 exclusive): [3 0 R, 6 0 R]  → object_count = 2 (min)
    /// Part 3 (shared):           [5 0 R, 8 0 R]
    /// Part 4 (remaining):        [4 0 R, 7 0 R]
    /// Page 0: page_ref = 3 0 R, object_count = 3
    /// Page 1: page_ref = 4 0 R, object_count = 5
    /// Shared hints:
    ///   5 0 R → referencing_pages [0, 1]
    ///   8 0 R → referencing_pages [0, 1]
    fn two_page_plan_with_shared() -> LinearizationPlan {
        LinearizationPlan {
            part1_objects: vec![],
            part2_objects: vec![ObjectRef::new(3, 0), ObjectRef::new(6, 0)],
            part3_objects: vec![ObjectRef::new(5, 0), ObjectRef::new(8, 0)],
            part4_objects: vec![ObjectRef::new(4, 0), ObjectRef::new(7, 0)],
            total_object_count: 8,
            root_ref: None,
            page_hints: vec![
                PageHintEntry {
                    page_ref: ObjectRef::new(3, 0),
                    first_object_index: 0,
                    object_count: 3,
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
                SharedObjectHintEntry {
                    object_ref: ObjectRef::new(5, 0),
                    referencing_pages: vec![0, 1],
                },
                SharedObjectHintEntry {
                    object_ref: ObjectRef::new(8, 0),
                    referencing_pages: vec![0, 1],
                },
            ],
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
        let table = PageOffsetHintTable::from_plan(&plan, &renumber);

        assert_eq!(
            table.entries.len(),
            1,
            "single-page must have exactly 1 entry"
        );
    }

    #[test]
    fn single_page_first_page_object_number() {
        let plan = single_page_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber);

        // page_ref = 3 0 R → new number 2 (first Part-2 object)
        assert_eq!(
            table.header.first_page_object_number, 2,
            "first page object new number must be 2"
        );
    }

    #[test]
    fn single_page_all_deltas_zero_so_bit_widths_zero() {
        let plan = single_page_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber);

        // Single page → delta = 0 → bits = 0
        assert_eq!(table.header.bits_object_count_delta, 0);
        assert_eq!(table.header.bits_shared_object_count, 0);
        assert_eq!(table.header.bits_shared_object_id, 0);
    }

    #[test]
    fn single_page_least_object_count() {
        let plan = single_page_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber);

        assert_eq!(table.header.least_object_count, 3);
    }

    #[test]
    fn single_page_entry_object_count_minus_least_is_zero() {
        let plan = single_page_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber);

        assert_eq!(table.entries[0].object_count_minus_least, 0);
    }

    #[test]
    fn single_page_placeholder_fields_are_zero() {
        let plan = single_page_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber);

        assert_eq!(table.header.location_of_first_page, 0);
        assert_eq!(table.header.least_page_length, 0);
        assert_eq!(table.entries[0].page_length_minus_least, 0);
        assert_eq!(table.entries[0].content_stream_offset, 0);
        assert_eq!(table.entries[0].content_stream_length, 0);
    }

    #[test]
    fn single_page_denominator_is_one() {
        let plan = single_page_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber);

        assert_eq!(
            table.header.denominator, 1,
            "denominator must be 1 (qpdf default)"
        );
    }

    #[test]
    fn single_page_no_shared_objects() {
        let plan = single_page_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber);

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
        let table = PageOffsetHintTable::from_plan(&plan, &renumber);

        assert_eq!(table.entries.len(), 2, "two-page plan must have 2 entries");
    }

    #[test]
    fn two_page_least_object_count() {
        let plan = two_page_plan_with_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber);

        // min(3, 5) = 3
        assert_eq!(table.header.least_object_count, 3);
    }

    #[test]
    fn two_page_bits_object_count_delta() {
        let plan = two_page_plan_with_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber);

        // delta = 5 - 3 = 2 → bits_needed(2) = 2
        assert_eq!(table.header.bits_object_count_delta, 2);
    }

    #[test]
    fn two_page_entry_object_count_minus_least() {
        let plan = two_page_plan_with_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber);

        // page 0: 3 - 3 = 0
        assert_eq!(table.entries[0].object_count_minus_least, 0);
        // page 1: 5 - 3 = 2
        assert_eq!(table.entries[1].object_count_minus_least, 2);
    }

    #[test]
    fn two_page_shared_object_count_per_page() {
        let plan = two_page_plan_with_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber);

        // Both pages reference 2 shared objects (5 0 R and 8 0 R).
        assert_eq!(table.entries[0].shared_object_count, 2);
        assert_eq!(table.entries[1].shared_object_count, 2);
    }

    #[test]
    fn two_page_bits_shared_object_count() {
        let plan = two_page_plan_with_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber);

        // greatest shared count = 2 → bits_needed(2) = 2
        assert_eq!(table.header.bits_shared_object_count, 2);
    }

    #[test]
    fn two_page_bits_shared_object_id() {
        let plan = two_page_plan_with_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber);

        // Part-2 objects: 3 0 R → new 2, 6 0 R → new 3
        // Part-3 objects: 5 0 R → new 4, 8 0 R → new 5
        // greatest shared id = 5 → bits_needed(5) = 3
        assert_eq!(table.header.bits_shared_object_id, 3);
    }

    #[test]
    fn two_page_shared_object_ids_are_new_numbers() {
        let plan = two_page_plan_with_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber);

        // Shared objects 5 0 R → new 4, 8 0 R → new 5.
        // Both pages reference both shared objects.
        let mut ids0 = table.entries[0].shared_object_ids.clone();
        ids0.sort_unstable();
        assert_eq!(
            ids0,
            vec![4, 5],
            "page 0 shared object ids must be new numbers 4 and 5"
        );

        let mut ids1 = table.entries[1].shared_object_ids.clone();
        ids1.sort_unstable();
        assert_eq!(
            ids1,
            vec![4, 5],
            "page 1 shared object ids must be new numbers 4 and 5"
        );
    }

    #[test]
    fn two_page_shared_numerators_are_zero() {
        let plan = two_page_plan_with_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber);

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
        let table = PageOffsetHintTable::from_plan(&plan, &renumber);

        for entry in &table.entries {
            assert_eq!(
                entry.shared_object_numerators.len(),
                entry.shared_object_ids.len(),
                "numerators count must match shared_object_ids count"
            );
        }
    }

    #[test]
    fn two_page_denominator_is_one() {
        let plan = two_page_plan_with_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber);

        assert_eq!(table.header.denominator, 1);
    }

    #[test]
    fn two_page_placeholder_fields_are_zero() {
        let plan = two_page_plan_with_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(&plan, &renumber);

        assert_eq!(table.header.location_of_first_page, 0);
        assert_eq!(table.header.least_page_length, 0);
        for entry in &table.entries {
            assert_eq!(entry.page_length_minus_least, 0);
            assert_eq!(entry.content_stream_offset, 0);
            assert_eq!(entry.content_stream_length, 0);
        }
    }
}
