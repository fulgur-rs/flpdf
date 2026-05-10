//! Shared Object Hint Table data structure (ISO 32000-1 Annex F.3.2).
//!
//! This module builds the **data** for the Shared Object Hint Table.  It does
//! **not** encode the table as bits/bytes — that is the responsibility of the
//! hint-stream encoder (sub-task 2.7).
//!
//! # Structure overview (Annex F.3.2)
//!
//! The hint table consists of:
//!
//! * A **header** with 7 integer items describing the ranges and bit widths
//!   needed to encode the per-group and per-object entries.
//! * **M per-group entries** (one per group of shared objects).
//! * **S per-shared-object entries** (one per shared object).
//!
//! # Simplifications (relative to full Annex F.3.2)
//!
//! This implementation adopts the same simplifications used by qpdf:
//!
//! * **1-group model**: all shared objects are placed in a single group
//!   (M = 1).  This is spec-compliant; the group structure is
//!   implementation-defined.
//! * **Signature suppressed**: the per-object signature flag is always 0
//!   (signature computation, e.g. MD5, is not performed).  When the flag is 0
//!   the 16-byte signature field is omitted from the encoded stream.
//!
//! # Back-patch discipline
//!
//! Several fields depend on byte offsets that are only known after the file
//! has been written:
//!
//! | Field | Location | Placeholder |
//! |-------|----------|-------------|
//! | `header.location` (item 2) | header | `0` |
//! | `header.least_length` (item 6) | header | `0` |
//! | `entry.length_minus_least` (item 2) per object | entries | `0` |
//! | `entry.group_offset` (item 4) per object | entries | `0` |
//!
//! These fields are stored as `0` in the returned structs.  The back-patcher
//! (sub-task 2.9) locates them by field name and overwrites them once the real
//! byte offsets are available.

use super::hint_page::bits_needed;
use super::plan::LinearizationPlan;
use super::renumber::RenumberMap;

// ---------------------------------------------------------------------------
// Header
// ---------------------------------------------------------------------------

/// Header for the Shared Object Hint Table (7 items per Annex F.3.2).
///
/// Items are numbered 1–7 in the spec; field names here follow the spec text.
///
/// ## Back-patch fields
///
/// * `location` (item 2): byte offset of the first object in the shared
///   objects section.  Set to `0` (placeholder); back-patched by sub-task 2.9.
/// * `least_length` (item 6): minimum byte length of an object in the shared
///   objects section.  Set to `0` (placeholder); back-patched by sub-task 2.9.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedObjectHeader {
    /// Item 1 — Object number of the first object in the shared objects
    /// section (i.e. the first new number assigned to a Part-3 object).
    ///
    /// `0` if there are no shared objects (degenerate case).
    pub first_object_number: u32,

    /// Item 2 — Byte offset of the first object in the shared objects section.
    ///
    /// **Placeholder: 0.  Back-patched by sub-task 2.9.**
    pub location: u64,

    /// Item 3 — Number of shared object entries for the first page
    /// (= count of Part-3 objects whose `referencing_pages` includes page 0).
    pub first_page_entries: u32,

    /// Item 4 — Total number of shared object entries (= `plan.shared_hints.len()`).
    pub section_entries: u32,

    /// Item 5 — Bits needed to represent the greatest number of objects in a
    /// shared object group (item 1 of the per-group entry).
    pub bits_group_object_count: u32,

    /// Item 6 — Least byte length of an object in the shared objects section.
    ///
    /// **Placeholder: 0.  Back-patched by sub-task 2.9.**
    pub least_length: u64,

    /// Item 7 — Bits needed to represent the difference between the greatest
    /// and least length of an object in the shared objects section (item 2 of
    /// the per-shared-object entry).
    ///
    /// Since all lengths are 0 (placeholder) at this stage the delta is 0 and
    /// bits = 0.  The encoder must re-derive bit widths at encode time using
    /// the final values after back-patching.
    pub bits_length_delta: u32,
}

// ---------------------------------------------------------------------------
// Per-group entry
// ---------------------------------------------------------------------------

/// Per-group entry for the Shared Object Hint Table (1 item per Annex F.3.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedGroupEntry {
    /// Item 1 — Number of objects in this group.
    pub object_count: u32,
}

// ---------------------------------------------------------------------------
// Per-shared-object entry
// ---------------------------------------------------------------------------

/// Per-shared-object entry for the Shared Object Hint Table (Annex F.3.2).
///
/// ## Back-patch fields
///
/// * `length_minus_least` (item 2): byte length of this object minus
///   `header.least_length`.  Set to `0` (placeholder); back-patched by
///   sub-task 2.9.
/// * `group_offset` (item 4): byte offset of this object from the location of
///   the first object in the shared objects section.  Set to `0` (placeholder);
///   back-patched by sub-task 2.9.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedObjectEntry {
    /// Item 1 — Signature present flag.
    ///
    /// Always `false` in this implementation; signature computation (MD5) is
    /// not performed.  When `false`, the 16-byte signature (item 3) is omitted
    /// from the encoded stream.
    pub signature_present: bool,

    /// Item 2 — Byte length of this object minus `header.least_length`.
    ///
    /// **Placeholder: 0.  Back-patched by sub-task 2.9.**
    pub length_minus_least: u32,

    /// Item 3 — 16-byte MD5 signature of the object data.
    ///
    /// Always `None` because `signature_present` is always `false`.
    pub signature: Option<[u8; 16]>,

    /// Item 4 — Byte offset of this object from the location of the first
    /// object in the shared objects section.
    ///
    /// **Placeholder: 0.  Back-patched by sub-task 2.9.**
    pub group_offset: u32,
}

// ---------------------------------------------------------------------------
// SharedObjectHintTable
// ---------------------------------------------------------------------------

/// Complete Shared Object Hint Table: header + M group entries + S object entries.
///
/// Constructed via [`SharedObjectHintTable::from_plan`].  All placeholder fields
/// (`location`, `least_length`, per-object `length_minus_least`, `group_offset`)
/// are initialized to `0`; sub-task 2.9 back-patches them.
///
/// The sub-task 2.7 encoder serializes this struct into the binary bit-packed
/// format required by Annex F.
///
/// # Group model
///
/// This implementation uses the **1-group model**: all shared objects are placed
/// in a single group (M = 1), or M = 0 when there are no shared objects.  This
/// matches qpdf's default behaviour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedObjectHintTable {
    /// The 7-item header.
    pub header: SharedObjectHeader,
    /// One entry per group (M entries).  Empty when there are no shared objects.
    pub groups: Vec<SharedGroupEntry>,
    /// One entry per shared object, in `plan.shared_hints` order.
    pub objects: Vec<SharedObjectEntry>,
}

impl SharedObjectHintTable {
    /// Build the Shared Object Hint Table from a [`LinearizationPlan`] and a
    /// [`RenumberMap`].
    ///
    /// # Placeholder conventions
    ///
    /// Fields that depend on byte offsets not yet known are set to `0` and
    /// must be filled in by the back-patcher:
    ///
    /// * `header.location`
    /// * `header.least_length`
    /// * `entry.length_minus_least` (all entries)
    /// * `entry.group_offset` (all entries)
    ///
    /// # Degenerate case (no shared objects)
    ///
    /// When `plan.shared_hints` is empty the table is almost empty:
    /// * All header numeric fields are `0`.
    /// * `groups` is empty (M = 0).
    /// * `objects` is empty.
    pub fn from_plan(plan: &LinearizationPlan, renumber: &RenumberMap) -> Self {
        let shared_count = plan.shared_hints.len() as u32;

        // ------------------------------------------------------------------
        // Degenerate case: no shared objects.
        // ------------------------------------------------------------------
        if shared_count == 0 {
            return Self {
                header: SharedObjectHeader {
                    first_object_number: 0,
                    location: 0,
                    first_page_entries: 0,
                    section_entries: 0,
                    bits_group_object_count: 0,
                    least_length: 0,
                    bits_length_delta: 0,
                },
                groups: vec![],
                objects: vec![],
            };
        }

        // ------------------------------------------------------------------
        // Step 1: first object number in the shared objects section.
        //
        // This is the new (linearized) object number of the first Part-3
        // object as assigned by the renumber map.
        // ------------------------------------------------------------------
        let first_object_number = plan
            .part3_objects
            .first()
            .and_then(|r| renumber.new_for_original(*r))
            .map(|r| r.number)
            .unwrap_or(0);

        // ------------------------------------------------------------------
        // Step 2: count how many shared objects reference page 0 (first page).
        // ------------------------------------------------------------------
        let first_page_entries = plan
            .shared_hints
            .iter()
            .filter(|h| h.referencing_pages.contains(&0))
            .count() as u32;

        // ------------------------------------------------------------------
        // Step 3: build header bit-width fields.
        //
        // bits_group_object_count: greatest objects-in-any-group.
        // With 1-group model this equals shared_count.
        //
        // bits_length_delta: since all lengths are 0 (placeholder) the delta
        // is 0, so bits = 0.  The encoder re-derives this after back-patching.
        // ------------------------------------------------------------------
        let bits_group_object_count = bits_needed(shared_count as u64);

        let header = SharedObjectHeader {
            first_object_number,
            location: 0, // placeholder — back-patched by sub-task 2.9
            first_page_entries,
            section_entries: shared_count,
            bits_group_object_count,
            least_length: 0,      // placeholder — back-patched by sub-task 2.9
            bits_length_delta: 0, // placeholder — re-derived by encoder after back-patch
        };

        // ------------------------------------------------------------------
        // Step 4: build per-group entries (1-group model).
        // ------------------------------------------------------------------
        let groups = vec![SharedGroupEntry {
            object_count: shared_count,
        }];

        // ------------------------------------------------------------------
        // Step 5: build per-shared-object entries.
        //
        // Entries are in plan.shared_hints order (= Part-3 order).
        // All byte-dependent fields are 0 (placeholder).
        // signature_present is always false; signature is always None.
        // ------------------------------------------------------------------
        let objects: Vec<SharedObjectEntry> = plan
            .shared_hints
            .iter()
            .map(|_hint| SharedObjectEntry {
                signature_present: false,
                length_minus_least: 0, // placeholder — back-patched by sub-task 2.9
                signature: None,
                group_offset: 0, // placeholder — back-patched by sub-task 2.9
            })
            .collect();

        Self {
            header,
            groups,
            objects,
        }
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
    // Fixture helpers (mirrors hint_page.rs fixtures for symmetry)
    // -----------------------------------------------------------------------

    /// Single-page plan with no shared objects (degenerate case).
    ///
    /// Part 2: [3 0 R, 2 0 R, 1 0 R]
    /// Part 3: []
    /// Pages:  [page_ref = 3 0 R, object_count = 3]
    fn single_page_no_shared() -> LinearizationPlan {
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

    /// Two-page plan with 2 shared objects each referenced by both pages.
    ///
    /// Part 2 (page 0 exclusive): [3 0 R, 6 0 R]   → new numbers 2, 3
    /// Part 3 (shared):           [5 0 R, 8 0 R]   → new numbers 4, 5
    /// Part 4 (remaining):        [4 0 R, 7 0 R]   → new numbers 6, 7
    /// Pages:
    ///   page 0: page_ref = 3 0 R
    ///   page 1: page_ref = 4 0 R
    /// Shared hints:
    ///   5 0 R → referencing_pages [0, 1]
    ///   8 0 R → referencing_pages [0, 1]
    fn two_page_shared_both_pages() -> LinearizationPlan {
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

    /// Two-page plan with 3 shared objects: 2 referenced from page 0, 1 only from page 1.
    ///
    /// Part 2: [10 0 R]             → new number 2
    /// Part 3: [20 0 R, 21 0 R, 22 0 R]  → new numbers 3, 4, 5
    /// Part 4: [30 0 R]             → new number 6
    /// Pages:
    ///   page 0: page_ref = 10 0 R
    ///   page 1: page_ref = 30 0 R
    /// Shared hints:
    ///   20 0 R → referencing_pages [0, 1]
    ///   21 0 R → referencing_pages [0]
    ///   22 0 R → referencing_pages [1]        ← NOT referenced from page 0
    fn two_page_partial_first_page() -> LinearizationPlan {
        LinearizationPlan {
            part1_objects: vec![],
            part2_objects: vec![ObjectRef::new(10, 0)],
            part3_objects: vec![
                ObjectRef::new(20, 0),
                ObjectRef::new(21, 0),
                ObjectRef::new(22, 0),
            ],
            part4_objects: vec![ObjectRef::new(30, 0)],
            total_object_count: 5,
            root_ref: None,
            page_hints: vec![
                PageHintEntry {
                    page_ref: ObjectRef::new(10, 0),
                    first_object_index: 0,
                    object_count: 1,
                    byte_length: 0,
                },
                PageHintEntry {
                    page_ref: ObjectRef::new(30, 0),
                    first_object_index: 0,
                    object_count: 1,
                    byte_length: 0,
                },
            ],
            shared_hints: vec![
                SharedObjectHintEntry {
                    object_ref: ObjectRef::new(20, 0),
                    referencing_pages: vec![0, 1],
                },
                SharedObjectHintEntry {
                    object_ref: ObjectRef::new(21, 0),
                    referencing_pages: vec![0],
                },
                SharedObjectHintEntry {
                    object_ref: ObjectRef::new(22, 0),
                    referencing_pages: vec![1],
                },
            ],
        }
    }

    // -----------------------------------------------------------------------
    // Degenerate case: no shared objects
    // -----------------------------------------------------------------------

    #[test]
    fn degenerate_groups_and_objects_are_empty() {
        let plan = single_page_no_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = SharedObjectHintTable::from_plan(&plan, &renumber);

        assert!(
            table.groups.is_empty(),
            "no shared objects → groups must be empty"
        );
        assert!(
            table.objects.is_empty(),
            "no shared objects → objects must be empty"
        );
    }

    #[test]
    fn degenerate_header_all_zero() {
        let plan = single_page_no_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = SharedObjectHintTable::from_plan(&plan, &renumber);

        assert_eq!(table.header.first_object_number, 0);
        assert_eq!(table.header.location, 0);
        assert_eq!(table.header.first_page_entries, 0);
        assert_eq!(table.header.section_entries, 0);
        assert_eq!(table.header.bits_group_object_count, 0);
        assert_eq!(table.header.least_length, 0);
        assert_eq!(table.header.bits_length_delta, 0);
    }

    // -----------------------------------------------------------------------
    // Two-page plan: both shared objects referenced from both pages
    // -----------------------------------------------------------------------

    #[test]
    fn two_page_section_entries_equals_shared_hint_count() {
        let plan = two_page_shared_both_pages();
        let renumber = RenumberMap::from_plan(&plan);
        let table = SharedObjectHintTable::from_plan(&plan, &renumber);

        assert_eq!(
            table.header.section_entries, 2,
            "section_entries must equal plan.shared_hints.len()"
        );
    }

    #[test]
    fn two_page_first_page_entries_both_referenced() {
        let plan = two_page_shared_both_pages();
        let renumber = RenumberMap::from_plan(&plan);
        let table = SharedObjectHintTable::from_plan(&plan, &renumber);

        // Both shared objects reference page 0.
        assert_eq!(
            table.header.first_page_entries, 2,
            "both shared objects reference page 0 → first_page_entries must be 2"
        );
    }

    #[test]
    fn two_page_first_object_number_is_new_number_of_first_part3() {
        let plan = two_page_shared_both_pages();
        let renumber = RenumberMap::from_plan(&plan);
        let table = SharedObjectHintTable::from_plan(&plan, &renumber);

        // Part 2: [3 0 R → new 2, 6 0 R → new 3]
        // Part 3: [5 0 R → new 4, ...]
        assert_eq!(
            table.header.first_object_number, 4,
            "first Part-3 object (5 0 R) must map to new number 4"
        );
    }

    #[test]
    fn two_page_bits_group_object_count() {
        let plan = two_page_shared_both_pages();
        let renumber = RenumberMap::from_plan(&plan);
        let table = SharedObjectHintTable::from_plan(&plan, &renumber);

        // 1-group model: 2 objects in group → bits_needed(2) = 2
        assert_eq!(table.header.bits_group_object_count, 2);
    }

    #[test]
    fn two_page_one_group_with_correct_object_count() {
        let plan = two_page_shared_both_pages();
        let renumber = RenumberMap::from_plan(&plan);
        let table = SharedObjectHintTable::from_plan(&plan, &renumber);

        assert_eq!(
            table.groups.len(),
            1,
            "1-group model must have exactly 1 group"
        );
        assert_eq!(
            table.groups[0].object_count, 2,
            "group must contain all 2 shared objects"
        );
    }

    #[test]
    fn two_page_objects_count_matches_shared_hints() {
        let plan = two_page_shared_both_pages();
        let renumber = RenumberMap::from_plan(&plan);
        let table = SharedObjectHintTable::from_plan(&plan, &renumber);

        assert_eq!(
            table.objects.len(),
            plan.shared_hints.len(),
            "objects vec length must match shared_hints count"
        );
    }

    #[test]
    fn two_page_signature_always_absent() {
        let plan = two_page_shared_both_pages();
        let renumber = RenumberMap::from_plan(&plan);
        let table = SharedObjectHintTable::from_plan(&plan, &renumber);

        for entry in &table.objects {
            assert!(
                !entry.signature_present,
                "signature_present must be false (signature not computed)"
            );
            assert!(entry.signature.is_none(), "signature must be None");
        }
    }

    #[test]
    fn two_page_placeholder_fields_are_zero() {
        let plan = two_page_shared_both_pages();
        let renumber = RenumberMap::from_plan(&plan);
        let table = SharedObjectHintTable::from_plan(&plan, &renumber);

        assert_eq!(table.header.location, 0);
        assert_eq!(table.header.least_length, 0);
        assert_eq!(table.header.bits_length_delta, 0);
        for entry in &table.objects {
            assert_eq!(entry.length_minus_least, 0);
            assert_eq!(entry.group_offset, 0);
        }
    }

    // -----------------------------------------------------------------------
    // Partial first-page reference: first_page_entries < section_entries
    // -----------------------------------------------------------------------

    #[test]
    fn partial_first_page_section_entries_is_three() {
        let plan = two_page_partial_first_page();
        let renumber = RenumberMap::from_plan(&plan);
        let table = SharedObjectHintTable::from_plan(&plan, &renumber);

        assert_eq!(
            table.header.section_entries, 3,
            "section_entries must equal total shared hints count (3)"
        );
    }

    #[test]
    fn partial_first_page_first_page_entries_is_two() {
        let plan = two_page_partial_first_page();
        let renumber = RenumberMap::from_plan(&plan);
        let table = SharedObjectHintTable::from_plan(&plan, &renumber);

        // 20 0 R → [0, 1]: references page 0 ✓
        // 21 0 R → [0]: references page 0 ✓
        // 22 0 R → [1]: does NOT reference page 0 ✗
        assert_eq!(
            table.header.first_page_entries, 2,
            "only 2 out of 3 shared objects reference page 0"
        );
    }

    #[test]
    fn partial_first_page_group_has_three_objects() {
        let plan = two_page_partial_first_page();
        let renumber = RenumberMap::from_plan(&plan);
        let table = SharedObjectHintTable::from_plan(&plan, &renumber);

        assert_eq!(table.groups.len(), 1);
        assert_eq!(table.groups[0].object_count, 3);
    }

    #[test]
    fn partial_first_page_bits_group_object_count() {
        let plan = two_page_partial_first_page();
        let renumber = RenumberMap::from_plan(&plan);
        let table = SharedObjectHintTable::from_plan(&plan, &renumber);

        // 3 objects in group → bits_needed(3) = 2
        assert_eq!(table.header.bits_group_object_count, 2);
    }

    #[test]
    fn partial_first_page_first_object_number() {
        let plan = two_page_partial_first_page();
        let renumber = RenumberMap::from_plan(&plan);
        let table = SharedObjectHintTable::from_plan(&plan, &renumber);

        // Part 2: [10 0 R → new 2]
        // Part 3: [20 0 R → new 3, ...]
        assert_eq!(
            table.header.first_object_number, 3,
            "first Part-3 object (20 0 R) must map to new number 3"
        );
    }

    #[test]
    fn partial_first_page_objects_count_is_three() {
        let plan = two_page_partial_first_page();
        let renumber = RenumberMap::from_plan(&plan);
        let table = SharedObjectHintTable::from_plan(&plan, &renumber);

        assert_eq!(table.objects.len(), 3);
    }
}
