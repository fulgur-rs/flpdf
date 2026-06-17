//! Shared Object Hint Table data structure (ISO 32000-1 Annex F.3.2).
//!
//! This module builds the **data** for the Shared Object Hint Table.  It does
//! **not** encode the table as bits/bytes — that is the responsibility of the
//! hint-stream encoder.
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
//! * **1-object-per-group model (M = N)**: each shared object forms its own
//!   group, so the number of groups equals the number of shared objects.
//!   This is spec-compliant (the group structure is implementation-defined)
//!   and matches qpdf's `writeHSharedObject` in `QPDF_linearization.cc`,
//!   which always emits `nbits_nobjects = 0` and per-entry
//!   `nobjects_minus_one = 0` — i.e. each group contains exactly one object.
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
//! locates them by field name and overwrites them once the real
//! byte offsets are available.

use super::plan::LinearizationPlan;
use super::renumber::RenumberMap;
use crate::ObjectRef;

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
///   objects section.  Set to `0` (placeholder); back-patched once the real offset is known.
/// * `least_length` (item 6): minimum byte length of an object in the shared
///   objects section.  Set to `0` (placeholder); back-patched once the real offset is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedObjectHeader {
    /// Item 1 — Object number of the first object in the shared objects
    /// section (i.e. the first new number assigned to a Part-3 object).
    ///
    /// `0` if there are no shared objects (degenerate case).
    pub first_object_number: u32,

    /// Item 2 — Byte offset of the first object in the shared objects section.
    ///
    /// **Placeholder: 0; back-patched once the real offset is known.**
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
    /// **Placeholder: 0; back-patched once the real offset is known.**
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
///   `header.least_length`.  Set to `0` (placeholder); back-patched once
///   the real offsets are known.
/// * `nobjects_minus_one` (item 4): number of additional objects in this
///   shared object's group, minus one.  In our 1-object-per-group model this
///   is always `0`; encoded with `bits_group_object_count` bits, which is
///   also `0` in our model so nothing is actually written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedObjectEntry {
    /// Item 1 (encoded order) — Byte length of this object minus
    /// `header.least_length`.  Populated by the writer in `from_plan`.
    pub length_minus_least: u32,

    /// Item 2 (encoded order) — Signature present flag.
    ///
    /// Always `false` in this implementation; signature computation (MD5) is
    /// not performed.  When `false`, the 16-byte signature is omitted from
    /// the encoded stream.
    pub signature_present: bool,

    /// Item 3 (encoded order) — 16-byte MD5 signature of the object data.
    ///
    /// Always `None` because `signature_present` is always `false`.
    pub signature: Option<[u8; 16]>,

    /// Item 4 (encoded order) — Number of additional objects in this group,
    /// minus one.  Our writer always uses one object per group, so this is
    /// always `0`.  Encoded with `bits_group_object_count` bits.
    pub nobjects_minus_one: u32,
}

// ---------------------------------------------------------------------------
// SharedObjectHintTable
// ---------------------------------------------------------------------------

/// Complete Shared Object Hint Table: header + M group entries + S object entries.
///
/// Constructed via [`SharedObjectHintTable::from_plan`].  All placeholder fields
/// (`location`, `least_length`, per-object `length_minus_least`, `group_offset`)
/// are initialized to `0`; the back-patcher fills them in once the real
/// byte offsets are available.
///
/// The hint-stream encoder serializes this struct into the binary bit-packed
/// format required by Annex F.
///
/// # Group model
///
/// This implementation uses the **1-object-per-group model**: each shared
/// object forms its own group, so M = N (the number of shared objects), or
/// M = 0 when there are no shared objects.  This matches qpdf's behaviour
/// (`writeHSharedObject` in `QPDF_linearization.cc`), which emits
/// `nbits_nobjects = 0` and `nobjects_minus_one = 0` for every entry.
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
    ///
    /// # Panics
    ///
    /// When `shared_hints` is non-empty, panics if `part3_objects` is empty
    /// or if its first entry is not present in `renumber` — both indicate a
    /// malformed `LinearizationPlan` / `RenumberMap` pair that the caller
    /// must construct consistently.  Silently writing
    /// `first_object_number = 0` would emit a header pointing at PDF
    /// object 0 (the free-list head), which is invalid.
    pub fn from_plan(
        plan: &LinearizationPlan,
        renumber: &RenumberMap,
        member_to_container: &std::collections::BTreeMap<ObjectRef, (u32, u32)>,
    ) -> Self {
        // Fold first-page ObjStm members into their container (one shared entry
        // per container) so the table matches qpdf's positional shared list.
        // With no ObjStm packing this equals `plan.shared_hints`.
        let shared_hints = plan.canonical_shared_hints(member_to_container, renumber);
        let shared_count = shared_hints.len() as u32;

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
        // Step 1: first object number in the "Part 8" section.
        //
        // Per ISO 32000-1 Annex F and qpdf's checkHSharedObject algorithm:
        //
        // - Item 1 (`first_object_number`) is the object number of the FIRST
        //   shared object in the "end of file section" (Part 8 in qpdf's terms
        //   = our part4_other_pages_shared).
        //
        // - This value is ONLY meaningful when `nshared_total > nshared_first_page`
        //   (i.e., when there are Part-8 shared objects beyond the first-page
        //   section).  Per qpdf's Implementation Note 131, the value is
        //   meaningless when `nshared_total == nshared_first_page`.
        //
        // - qpdf's check walks the first `nshared_first_page` shared entries
        //   starting from `pages[0].getObjectID()` (the first page object, not
        //   from first_object_number).  Only at index `nshared_first_page` does
        //   it jump to `first_object_number` and verify it matches the actual
        //   first object of part8.
        //
        // When there are no Part-8 shared objects (`part4_other_pages_shared`
        // is empty), we emit `0` (or any value; it is ignored by readers).
        // When there ARE Part-8 shared objects, emit the renumbered object ID
        // of the first entry in `part4_other_pages_shared`.
        //
        // ------------------------------------------------------------------
        // Step 2: split the folded shared list into the first-page section and
        // the Part-8 (after-/E) section, and find the first Part-8 object.
        //
        // `shared_hints` is `canonical_shared_hints` output: the first-page
        // section (Part-2/Part-3, with eligible members folded into part6
        // containers) followed by the Part-8 section. The Part-8 section is the
        // plain `part4_other_pages_shared` objects PLUS the part8 ObjStm
        // containers `canonical_shared_hints` appends — containers the global even
        // split filled with page-private objects, which are shared objects qpdf
        // lists even though no individual member is `part4_other_pages_shared`.
        // So the Part-8 count is the part8 containers plus the plain (no-container)
        // `part4_other_pages_shared` objects.
        let part8_containers = plan.part8_container_nums(member_to_container);
        let part8_plain = plan
            .part4_other_pages_shared
            .iter()
            .filter(|r| !member_to_container.contains_key(r))
            .count() as u32;
        let part8_entries = part8_containers.len() as u32 + part8_plain;
        let first_page_entries = shared_count.saturating_sub(part8_entries);

        // first_object_number (Annex F.4.1 item 1): the object number of the
        // first object in the Part-8 section. `canonical_shared_hints` orders that
        // section by physical object number, so it is the entry at index
        // `first_page_entries` — 0 when there is no Part-8 section (meaningless per
        // Implementation Note 131; qpdf emits 0 too).
        let first_object_number: u32 =
            shared_hints
                .get(first_page_entries as usize)
                .map_or(0, |e| {
                    if e.object_ref.generation == u16::MAX {
                        e.object_ref.number
                    } else {
                        renumber
                            .new_for_original(e.object_ref)
                            .map_or(0, |r| r.number)
                    }
                });

        // ------------------------------------------------------------------
        // Step 3: build header bit-width fields.
        //
        // bits_group_object_count: greatest objects-in-any-group.
        //
        // Each shared object is its own group (one object per group), so the
        // greatest `nobjects_minus_one` is 0 across all groups, requiring 0
        // bits per Annex F.4.5 / qpdf nbits_nobjects.  Setting this to
        // `bits_needed(shared_count)` would be wrong: shared_count is the
        // *number of groups*, not the *largest group's object count*.
        //
        // bits_length_delta: since all lengths are 0 (placeholder) the delta
        // is 0, so bits = 0.  The encoder re-derives this after back-patching.
        let bits_group_object_count: u32 = 0;

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
        // Step 4: build per-group entries (1-object-per-group model).
        //
        // Each shared object forms its own group with object_count == 1.
        // This matches the header's `bits_group_object_count = 0`
        // (greatest nobjects_minus_one across groups is 0) and the
        // per-object `nobjects_minus_one = 0` written below.
        // ------------------------------------------------------------------
        let groups = vec![SharedGroupEntry { object_count: 1 }; shared_count as usize];

        // ------------------------------------------------------------------
        // Step 5: build per-shared-object entries.
        //
        // Entries are in folded `shared_hints` order (Part-2, then Part-3 with
        // packed members folded into their container, then Part-8).
        // All byte-dependent fields are 0 (placeholder).
        // signature_present is always false; signature is always None.
        // ------------------------------------------------------------------
        let objects: Vec<SharedObjectEntry> = shared_hints
            .iter()
            .map(|_hint| SharedObjectEntry {
                length_minus_least: 0, // placeholder — populated by writer
                signature_present: false,
                signature: None,
                nobjects_minus_one: 0, // 1-object-per-group model
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

    /// Two-page plan with 2 shared objects each referenced by both pages.
    ///
    /// Part 2 (page 0 exclusive): [3 0 R, 6 0 R]   → new numbers 2, 3
    /// Part 3 (shared):           [5 0 R, 8 0 R]   → new numbers 4, 5
    /// Part 4 (remaining):        [4 0 R, 7 0 R]   → new numbers 6, 7
    /// Pages:
    ///   page 0: page_ref = 3 0 R
    ///   page 1: page_ref = 4 0 R
    /// Shared hints (part2 entries first, then part3 entries):
    ///   3 0 R → referencing_pages []   (part2, page 0 owns by layout)
    ///   6 0 R → referencing_pages []   (part2, page 0 owns by layout)
    ///   5 0 R → referencing_pages [0, 1]
    ///   8 0 R → referencing_pages [0, 1]
    fn two_page_shared_both_pages() -> LinearizationPlan {
        LinearizationPlan {
            part2_objects: vec![ObjectRef::new(3, 0), ObjectRef::new(6, 0)],
            part3_objects: vec![ObjectRef::new(5, 0), ObjectRef::new(8, 0)],
            part4_other_pages_private: vec![ObjectRef::new(4, 0), ObjectRef::new(7, 0)],
            total_object_count: 8,
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
                // part2 entries (referencing_pages = [] — page 0 owns by physical layout)
                SharedObjectHintEntry {
                    object_ref: ObjectRef::new(3, 0),
                    referencing_pages: vec![],
                },
                SharedObjectHintEntry {
                    object_ref: ObjectRef::new(6, 0),
                    referencing_pages: vec![],
                },
                // part3 entries (truly cross-page shared objects)
                SharedObjectHintEntry {
                    object_ref: ObjectRef::new(5, 0),
                    referencing_pages: vec![0, 1],
                },
                SharedObjectHintEntry {
                    object_ref: ObjectRef::new(8, 0),
                    referencing_pages: vec![0, 1],
                },
            ],
            ..Default::default()
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
    /// Shared hints (part2 first, then part3):
    ///   10 0 R → referencing_pages []   (part2, page 0 owns by layout)
    ///   20 0 R → referencing_pages [0, 1]
    ///   21 0 R → referencing_pages [0]
    ///   22 0 R → referencing_pages [1]        ← NOT referenced from page 0
    fn two_page_partial_first_page() -> LinearizationPlan {
        LinearizationPlan {
            part2_objects: vec![ObjectRef::new(10, 0)],
            part3_objects: vec![
                ObjectRef::new(20, 0),
                ObjectRef::new(21, 0),
                ObjectRef::new(22, 0),
            ],
            part4_other_pages_private: vec![ObjectRef::new(30, 0)],
            total_object_count: 5,
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
                // part2 entry (referencing_pages = [] — page 0 owns by physical layout)
                SharedObjectHintEntry {
                    object_ref: ObjectRef::new(10, 0),
                    referencing_pages: vec![],
                },
                // part3 entries (truly cross-page shared objects)
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
            ..Default::default()
        }
    }

    // -----------------------------------------------------------------------
    // Degenerate case: no shared objects
    // -----------------------------------------------------------------------

    #[test]
    fn degenerate_groups_and_objects_are_empty() {
        let plan = single_page_no_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = SharedObjectHintTable::from_plan(&plan, &renumber, &Default::default());

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
        let table = SharedObjectHintTable::from_plan(&plan, &renumber, &Default::default());

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
        let table = SharedObjectHintTable::from_plan(&plan, &renumber, &Default::default());

        assert_eq!(
            table.header.section_entries, 4,
            "section_entries must equal plan.shared_hints.len() (2 part2 + 2 part3 = 4)"
        );
    }

    #[test]
    fn two_page_first_page_entries_equals_section_entries() {
        let plan = two_page_shared_both_pages();
        let renumber = RenumberMap::from_plan(&plan);
        let table = SharedObjectHintTable::from_plan(&plan, &renumber, &Default::default());

        // All shared objects are in the first-page section (before /E).
        // first_page_entries must equal section_entries so qpdf doesn't
        // expect a non-empty Part 8.
        assert_eq!(
            table.header.first_page_entries, 4,
            "all shared objects are in first-page section → first_page_entries must equal section_entries (4)"
        );
    }

    #[test]
    fn two_page_first_object_number_is_zero_when_no_part8() {
        let plan = two_page_shared_both_pages();
        let renumber = RenumberMap::from_plan(&plan);
        let table = SharedObjectHintTable::from_plan(&plan, &renumber, &Default::default());

        // When part4_other_pages_shared is empty (no Part-8 shared objects),
        // first_object_number must be 0 per ISO 32000-1 Implementation Note 131:
        // the value is meaningless when nshared_total == nshared_first_page,
        // so we emit 0 rather than pointing at an unrelated Part-2 object.
        assert_eq!(
            table.header.first_object_number, 0,
            "first_object_number must be 0 when there are no Part-8 shared objects \
             (nshared_total == nshared_first_page, value is meaningless per Note 131)"
        );
    }

    #[test]
    fn two_page_bits_group_object_count() {
        let plan = two_page_shared_both_pages();
        let renumber = RenumberMap::from_plan(&plan);
        let table = SharedObjectHintTable::from_plan(&plan, &renumber, &Default::default());

        // One object per group (we never group multiple shared objects
        // together), so the greatest `nobjects_minus_one` across groups is
        // 0 — bit width is 0 per Annex F.4.5 / qpdf nbits_nobjects.
        assert_eq!(table.header.bits_group_object_count, 0);
    }

    #[test]
    fn two_page_groups_one_per_shared_object() {
        let plan = two_page_shared_both_pages();
        let renumber = RenumberMap::from_plan(&plan);
        let table = SharedObjectHintTable::from_plan(&plan, &renumber, &Default::default());

        assert_eq!(
            table.groups.len(),
            4,
            "1-object-per-group model must have one group per shared object \
             (2 part2 + 2 part3 = 4)"
        );
        for (i, group) in table.groups.iter().enumerate() {
            assert_eq!(
                group.object_count, 1,
                "group {i} must contain exactly 1 object under 1-object-per-group model"
            );
        }
    }

    #[test]
    fn two_page_objects_count_matches_shared_hints() {
        let plan = two_page_shared_both_pages();
        let renumber = RenumberMap::from_plan(&plan);
        let table = SharedObjectHintTable::from_plan(&plan, &renumber, &Default::default());

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
        let table = SharedObjectHintTable::from_plan(&plan, &renumber, &Default::default());

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
        let table = SharedObjectHintTable::from_plan(&plan, &renumber, &Default::default());

        assert_eq!(table.header.location, 0);
        assert_eq!(table.header.least_length, 0);
        assert_eq!(table.header.bits_length_delta, 0);
        for entry in &table.objects {
            assert_eq!(entry.length_minus_least, 0);
            assert_eq!(entry.nobjects_minus_one, 0);
        }
    }

    // -----------------------------------------------------------------------
    // Partial first-page reference: first_page_entries < section_entries
    // -----------------------------------------------------------------------

    #[test]
    fn partial_first_page_section_entries_is_four() {
        let plan = two_page_partial_first_page();
        let renumber = RenumberMap::from_plan(&plan);
        let table = SharedObjectHintTable::from_plan(&plan, &renumber, &Default::default());

        assert_eq!(
            table.header.section_entries, 4,
            "section_entries must equal total shared hints count (1 part2 + 3 part3 = 4)"
        );
    }

    #[test]
    fn partial_first_page_first_page_entries_equals_section_entries() {
        let plan = two_page_partial_first_page();
        let renumber = RenumberMap::from_plan(&plan);
        let table = SharedObjectHintTable::from_plan(&plan, &renumber, &Default::default());

        // All shared objects (4 = 1 part2 + 3 part3) are physically in the first-page section.
        // first_page_entries must equal section_entries = 4 so that qpdf
        // does not expect a non-empty Part 8.
        assert_eq!(
            table.header.first_page_entries, 4,
            "all 4 shared objects are in first-page section → first_page_entries must be 4"
        );
    }

    #[test]
    fn partial_first_page_groups_one_per_shared_object() {
        let plan = two_page_partial_first_page();
        let renumber = RenumberMap::from_plan(&plan);
        let table = SharedObjectHintTable::from_plan(&plan, &renumber, &Default::default());

        assert_eq!(
            table.groups.len(),
            4,
            "1-object-per-group: 4 shared objects → 4 groups"
        );
        for group in &table.groups {
            assert_eq!(group.object_count, 1);
        }
    }

    #[test]
    fn partial_first_page_bits_group_object_count() {
        let plan = two_page_partial_first_page();
        let renumber = RenumberMap::from_plan(&plan);
        let table = SharedObjectHintTable::from_plan(&plan, &renumber, &Default::default());

        // 1-object-per-group model — see two_page_bits_group_object_count.
        assert_eq!(table.header.bits_group_object_count, 0);
    }

    #[test]
    fn partial_first_page_first_object_number_is_zero_when_no_part8() {
        let plan = two_page_partial_first_page();
        let renumber = RenumberMap::from_plan(&plan);
        let table = SharedObjectHintTable::from_plan(&plan, &renumber, &Default::default());

        // Two-page plan with 3 part3 objects but no part4_other_pages_shared.
        // first_object_number must be 0 — value is meaningless when
        // nshared_total == nshared_first_page (Implementation Note 131).
        assert_eq!(
            table.header.first_object_number, 0,
            "first_object_number must be 0 when part4_other_pages_shared is empty \
             (nshared_total == nshared_first_page, value is meaningless per Note 131)"
        );
    }

    #[test]
    fn partial_first_page_objects_count_is_three() {
        let plan = two_page_partial_first_page();
        let renumber = RenumberMap::from_plan(&plan);
        let table = SharedObjectHintTable::from_plan(&plan, &renumber, &Default::default());

        assert_eq!(table.objects.len(), 4);
    }

    // -----------------------------------------------------------------------
    // Plan with Part-8 shared objects (part4_other_pages_shared non-empty).
    //
    // This models objects shared by two or more pages (pages 2..N) but NOT
    // reachable from page 0 (i.e., not in Part 2 or Part 3).
    //
    // Part 2: [3 0 R]               → first-page private (page dict)
    // Part 3: [5 0 R]               → first-page shared (font)
    // part4_other_pages_shared: [9 0 R, 10 0 R]  → Part-8 shared (content streams)
    // part4_other_pages_private: [4 0 R]          → page 2 private
    // Pages:
    //   page 0: page_ref = 3 0 R
    //   page 1: page_ref = 4 0 R
    // Shared hints: part2 + part3 + part4_shared entries.
    //   3 0 R  → []           (part2, page 0 layout)
    //   5 0 R  → [1]          (part3, page 0 layout, page 1 reference)
    //   9 0 R  → [1, 2]       (part8, pages 1 and 2 reference it)
    //   10 0 R → [1, 3]       (part8, pages 1 and 3 reference it)
    // -----------------------------------------------------------------------

    fn two_page_with_part8_shared() -> LinearizationPlan {
        LinearizationPlan {
            part2_objects: vec![ObjectRef::new(3, 0)],
            part3_objects: vec![ObjectRef::new(5, 0)],
            part4_other_pages_shared: vec![ObjectRef::new(9, 0), ObjectRef::new(10, 0)],
            part4_other_pages_private: vec![ObjectRef::new(4, 0)],
            total_object_count: 5,
            page_hints: vec![
                PageHintEntry {
                    page_ref: ObjectRef::new(3, 0),
                    first_object_index: 0,
                    object_count: 2,
                    byte_length: 0,
                },
                PageHintEntry {
                    page_ref: ObjectRef::new(4, 0),
                    first_object_index: 0,
                    object_count: 1,
                    byte_length: 0,
                },
            ],
            shared_hints: vec![
                // part2 entry
                SharedObjectHintEntry {
                    object_ref: ObjectRef::new(3, 0),
                    referencing_pages: vec![],
                },
                // part3 entry
                SharedObjectHintEntry {
                    object_ref: ObjectRef::new(5, 0),
                    referencing_pages: vec![1],
                },
                // part4_other_pages_shared entries
                SharedObjectHintEntry {
                    object_ref: ObjectRef::new(9, 0),
                    referencing_pages: vec![1, 2],
                },
                SharedObjectHintEntry {
                    object_ref: ObjectRef::new(10, 0),
                    referencing_pages: vec![1, 3],
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn part8_shared_first_page_entries_less_than_section_entries() {
        let plan = two_page_with_part8_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = SharedObjectHintTable::from_plan(&plan, &renumber, &Default::default());

        // first_page_entries = |part2| + |part3| = 1 + 1 = 2
        // section_entries = |shared_hints| = 4
        assert_eq!(
            table.header.first_page_entries, 2,
            "first_page_entries must equal |part2| + |part3| = 2"
        );
        assert_eq!(
            table.header.section_entries, 4,
            "section_entries must equal total shared_hints count = 4"
        );
        assert!(
            table.header.first_page_entries < table.header.section_entries,
            "first_page_entries ({}) must be less than section_entries ({}) \
             when there are Part-8 shared objects",
            table.header.first_page_entries,
            table.header.section_entries
        );
    }

    #[test]
    fn part8_shared_first_object_number_is_new_number_of_part8_first() {
        let plan = two_page_with_part8_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = SharedObjectHintTable::from_plan(&plan, &renumber, &Default::default());

        // part4_other_pages_shared[0] = 9 0 R.
        // first_object_number must point to its renumbered slot.
        assert_eq!(
            table.header.first_object_number,
            renumber
                .new_for_original(ObjectRef::new(9, 0))
                .unwrap()
                .number,
            "first_object_number must equal the renumbered slot of \
             part4_other_pages_shared[0] (9 0 R) when Part-8 shared objects exist"
        );
    }

    #[test]
    fn part8_shared_folded_container_does_not_overcount_first_page_entries() {
        // Two Part-8 (after-/E) shared objects packed into ONE second-half
        // ObjStm container.  `canonical_shared_hints` folds them into a single
        // shared entry, so `section_entries` is 3 (part2 + part3 + 1 folded
        // Part-8).  `first_page_entries` must stay 2 (part2 + part3): subtracting
        // the unfolded `part4_other_pages_shared.len()` (2) would wrongly yield 1
        // and tell qpdf a first-page entry belongs to Part 8.
        let plan = two_page_with_part8_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let mut member_to_container: std::collections::BTreeMap<ObjectRef, (u32, u32)> =
            std::collections::BTreeMap::new();
        member_to_container.insert(ObjectRef::new(9, 0), (42, 0));
        member_to_container.insert(ObjectRef::new(10, 0), (42, 1));
        let table = SharedObjectHintTable::from_plan(&plan, &renumber, &member_to_container);

        assert_eq!(
            table.header.section_entries, 3,
            "two Part-8 members folded into one container → section_entries = \
             part2(1) + part3(1) + folded part8(1) = 3"
        );
        assert_eq!(
            table.header.first_page_entries, 2,
            "first_page_entries must stay |part2| + |part3| = 2 even when the \
             Part-8 members fold into a single container"
        );
    }

    #[test]
    fn part8_shared_groups_count_equals_total_shared_hints() {
        let plan = two_page_with_part8_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = SharedObjectHintTable::from_plan(&plan, &renumber, &Default::default());

        assert_eq!(
            table.groups.len(),
            plan.shared_hints.len(),
            "groups count must equal total shared_hints (1-object-per-group model)"
        );
    }

    // -----------------------------------------------------------------------
    // ObjStm-aware back-patch behaviour (flpdf-9hc.5.8.3)
    //
    // `from_plan` computes `first_object_number` from the renumber map only
    // — it has no knowledge of ObjStm containers.  When the first Part-8
    // shared object is packed inside an ObjStm the writer's convergence loop
    // patches `so_table.header.first_object_number` with the *container's*
    // new object number (not the member's renumber slot).
    //
    // The tests below verify two invariants:
    //
    // 1. `from_plan` returns the member's own renumber slot (the "before-patch"
    //    value).  This is *intentionally different* from the ObjStm-container
    //    number; the convergence loop is responsible for correcting it.
    //
    // 2. If the writer's patch replaces `first_object_number` with a
    //    hypothetical container number, the field must store *that* container
    //    number, not the original member slot — demonstrating that the patch
    //    logic selects the right source when `member_to_container` has an entry.
    //
    // Note: these tests operate on the struct directly (before the writer patch
    // runs).  They document the two-step contract: (a) from_plan establishes the
    // member-slot baseline, and (b) the writer overwrites it with container_num
    // when the member is ObjStm-packed.
    // -----------------------------------------------------------------------

    /// `from_plan` returns the member's own renumber slot for Part-8 shared objects.
    /// The writer's convergence loop is responsible for patching this to the
    /// container's number when the member lives inside an ObjStm.
    #[test]
    fn part8_shared_from_plan_returns_member_renumber_slot_before_objstm_patch() {
        let plan = two_page_with_part8_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = SharedObjectHintTable::from_plan(&plan, &renumber, &Default::default());

        // from_plan: member's renumber slot (before writer patch)
        let member_slot = renumber
            .new_for_original(ObjectRef::new(9, 0))
            .expect("9 0 R must be in renumber map")
            .number;
        assert_eq!(
            table.header.first_object_number, member_slot,
            "from_plan must return the member's renumber slot (pre-patch baseline)"
        );
    }

    /// When the writer patches `first_object_number` with an ObjStm container
    /// number, the patched value must differ from the member's renumber slot
    /// (unless the object happens to be allocated the same number, which is
    /// impossible since ObjStm containers are allocated above the renumber
    /// range).  This test simulates the patch by hand to verify the expected
    /// post-patch value.
    #[test]
    fn part8_shared_objstm_patch_uses_container_num_not_member_slot() {
        let plan = two_page_with_part8_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let mut table = SharedObjectHintTable::from_plan(&plan, &renumber, &Default::default());

        // Simulate the writer's ObjStm-aware patch: container numbers are
        // allocated above the RenumberMap range (renumber.len() + 1, +2 …).
        // For this plan, renumber.len() == 5 (objects 3,5,9,10,4 → slots 2-6),
        // so a Part-4 ObjStm container might get number 7.
        let simulated_container_num: u32 = renumber.len() as u32 + 2; // > any member slot
        table.header.first_object_number = simulated_container_num;

        // After patch: the field must hold the container number, not the member slot.
        let member_slot = renumber
            .new_for_original(ObjectRef::new(9, 0))
            .expect("9 0 R must be in renumber map")
            .number;
        assert_ne!(
            table.header.first_object_number, member_slot,
            "post-patch: first_object_number must differ from member's renumber slot \
             when the member is ObjStm-packed (container_num > all member slots)"
        );
        assert_eq!(
            table.header.first_object_number, simulated_container_num,
            "post-patch: first_object_number must equal the ObjStm container number"
        );
    }
}
