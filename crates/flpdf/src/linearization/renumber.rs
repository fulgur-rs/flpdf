//! Object renumbering for linearized PDF output (ISO 32000-1 Annex F).
//!
//! After the [`LinearizationPlan`] has partitioned all objects into Parts 2–4,
//! this module assigns new object numbers that place them in the correct
//! linearized order:
//!
//! | New number | Meaning |
//! |------------|---------|
//! | 1          | Reserved for the linearization parameter dictionary (Part 1). |
//! | 2 .. a     | Part 2 — first-page objects. |
//! | a+1 .. b   | Part 3 — shared objects. |
//! | b+1 .. N   | Part 4 — remaining objects. |
//!
//! All new object numbers carry `generation = 0` (the linearization spec does
//! not require preserving generation numbers, and the writer starts fresh).
//!
//! # Determinism
//!
//! The assignment is deterministic: given the same [`LinearizationPlan`], the
//! same map is produced every time.  Within each part the plan's `Vec` order is
//! respected, so the caller controls ordering by controlling the plan.
//!
//! # Non-goals
//!
//! * This module does **not** rewrite byte streams or fix up cross-references —
//!   that is the writer's responsibility.
//! * It does **not** move objects between parts (e.g. relocating the Catalog to
//!   Part 3) — that is deferred to a later task.

use crate::ObjectRef;

use super::plan::LinearizationPlan;
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// RenumberMap
// ---------------------------------------------------------------------------

/// A bijective mapping from original [`ObjectRef`]s to new (linearized) object
/// numbers, following the Annex F part ordering.
///
/// ## Layout
///
/// Index 0 in the internal `by_new_number` vector is unused (object numbers
/// are 1-based).  Index 1 is the *reserved* slot for the linearization
/// parameter dictionary and is never added to `by_original`.
///
/// ```text
/// by_new_number[0] = sentinel (ObjectRef::new(0, 0))   — unused
/// by_new_number[1] = sentinel (ObjectRef::new(0, 0))   — reserved for param dict
/// by_new_number[2] = original ref of first Part-2 object
/// ...
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenumberMap {
    /// `new_number → original_ref`.  Index 0 and 1 hold sentinels (number == 0).
    by_new_number: Vec<ObjectRef>,
    /// `original_ref → new_ref`.
    by_original: BTreeMap<ObjectRef, ObjectRef>,
}

/// Sentinel value stored at slots 0 and 1 in `by_new_number`.
const SENTINEL: ObjectRef = ObjectRef {
    number: 0,
    generation: 0,
};

impl RenumberMap {
    // -----------------------------------------------------------------------
    // Construction
    // -----------------------------------------------------------------------

    /// Build a renumber map from a [`LinearizationPlan`].
    ///
    /// New object numbers are assigned in this order:
    /// 1. Object number **1** is reserved for the linearization parameter
    ///    dictionary.  It is **not** present in the plan, so it is only
    ///    recorded as a reserved slot and does not appear in `by_original`.
    /// 2. Numbers **2 ..** are assigned to Part-2 objects in plan order.
    /// 3. Continuing, Part-3 objects in plan order.
    /// 4. Then Part-4 objects in plan order.
    ///
    /// # Panics
    ///
    /// * In debug builds, panics if `plan.parts_are_disjoint()` is false.
    /// * In any build, panics if the same original `ObjectRef` appears more
    ///   than once across parts (defence-in-depth against a broken plan whose
    ///   `parts_are_disjoint()` would lie). A duplicate would silently corrupt
    ///   the bijective `original ↔ new` mapping if not caught.
    pub fn from_plan(plan: &LinearizationPlan) -> Self {
        debug_assert!(
            plan.parts_are_disjoint(),
            "LinearizationPlan invariant violated: parts must be disjoint"
        );

        // Total mapped objects = Part 2 + Part 3 + Part 4.
        // We add 2 extra slots: index 0 (unused) and index 1 (param dict sentinel).
        let total_parts =
            plan.part2_objects.len() + plan.part3_objects.len() + plan.part4_objects.len();
        let capacity = total_parts + 2; // slots 0 and 1 are sentinels

        let mut by_new_number: Vec<ObjectRef> = Vec::with_capacity(capacity);
        let mut by_original: BTreeMap<ObjectRef, ObjectRef> = BTreeMap::new();

        // Slot 0: unused (PDF object numbers start at 1).
        by_new_number.push(SENTINEL);
        // Slot 1: reserved for linearization parameter dictionary (Part 1).
        by_new_number.push(SENTINEL);

        // Assign numbers starting at 2.
        let parts: [&[ObjectRef]; 3] = [
            &plan.part2_objects,
            &plan.part3_objects,
            &plan.part4_objects,
        ];

        for part in parts {
            for &original in part {
                let new_number = by_new_number.len() as u32; // current length == next index == new number
                let new_ref = ObjectRef::new(new_number, 0);
                by_new_number.push(original);
                // Reject duplicates in release too — silent overwrite would
                // break the bijective invariant used by reverse lookups and
                // by the writer's renumber-during-serialize path.
                assert!(
                    by_original.insert(original, new_ref).is_none(),
                    "duplicate original ObjectRef in LinearizationPlan: {original:?}"
                );
            }
        }

        Self {
            by_new_number,
            by_original,
        }
    }

    // -----------------------------------------------------------------------
    // Queries — forward direction (original → new)
    // -----------------------------------------------------------------------

    /// Return the new [`ObjectRef`] assigned to `original`, or `None` if the
    /// original was not part of the plan.
    pub fn new_for_original(&self, original: ObjectRef) -> Option<ObjectRef> {
        self.by_original.get(&original).copied()
    }

    // -----------------------------------------------------------------------
    // Queries — reverse direction (new → original)
    // -----------------------------------------------------------------------

    /// Return the original [`ObjectRef`] for a given new object number, or
    /// `None` if the number is out of range, is one of the two sentinel slots
    /// (0 = unused, 1 = param dict reservation), or carries a non-zero
    /// generation (renumbered objects are always at generation 0).
    pub fn original_for_new(&self, new: ObjectRef) -> Option<ObjectRef> {
        if new.generation != 0 {
            return None;
        }
        let idx = new.number as usize;
        if idx < 2 || idx >= self.by_new_number.len() {
            return None;
        }
        let stored = self.by_new_number[idx];
        if stored.number == 0 {
            None
        } else {
            Some(stored)
        }
    }

    // -----------------------------------------------------------------------
    // Metadata helpers
    // -----------------------------------------------------------------------

    /// The [`ObjectRef`] conventionally reserved for the linearization
    /// parameter dictionary (Part 1).  Always `1 0 R`.
    pub fn param_dict_ref() -> ObjectRef {
        ObjectRef::new(1, 0)
    }

    /// Total number of *allocated* object slots, including the reserved param
    /// dict slot.
    ///
    /// Equals `|Part 2| + |Part 3| + |Part 4| + 1`.
    pub fn len(&self) -> usize {
        // by_new_number has slots 0..N; slot 0 is unused, so "meaningful"
        // length is by_new_number.len() - 1.  But for the writer it is most
        // useful to know the highest object number allocated, which is
        // by_new_number.len() - 1 (0-indexed → that value IS the last number).
        self.by_new_number.len() - 1
    }

    /// `true` if no objects from the plan were mapped (all parts were empty).
    pub fn is_empty(&self) -> bool {
        // len() is 1 when only the param dict slot exists and no plan objects.
        self.len() <= 1
    }

    // -----------------------------------------------------------------------
    // Iteration
    // -----------------------------------------------------------------------

    /// Iterate over `(new_ref, original_ref)` pairs in layout order (new
    /// object number ascending, starting from 2).
    ///
    /// The param dict slot (new number 1) is **not** yielded because it has no
    /// corresponding original object; the writer handles it separately.
    pub fn iter_in_layout_order(&self) -> impl Iterator<Item = (ObjectRef, ObjectRef)> + '_ {
        self.by_new_number
            .iter()
            .enumerate()
            .skip(2) // skip slot 0 (unused) and slot 1 (param dict)
            .filter_map(|(new_number, &original)| {
                if original.number == 0 {
                    None
                } else {
                    Some((ObjectRef::new(new_number as u32, 0), original))
                }
            })
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linearization::plan::{LinearizationPlan, PageHintEntry};

    // -----------------------------------------------------------------------
    // Fixture helpers
    // -----------------------------------------------------------------------

    /// Minimal synthetic plan: single page, no shared objects.
    ///
    /// Part 2: [3 0 R, 2 0 R]
    /// Part 3: []
    /// Part 4: [1 0 R]
    ///
    /// Expected new numbering:
    ///   1 → reserved (param dict)
    ///   2 → 3 0 R
    ///   3 → 2 0 R
    ///   4 → 1 0 R
    fn single_page_plan() -> LinearizationPlan {
        LinearizationPlan {
            part1_objects: vec![],
            part2_objects: vec![ObjectRef::new(3, 0), ObjectRef::new(2, 0)],
            part3_objects: vec![],
            part4_objects: vec![ObjectRef::new(1, 0)],
            total_object_count: 3,
            root_ref: Some(ObjectRef::new(1, 0)),
            page_hints: vec![PageHintEntry::placeholder(ObjectRef::new(3, 0))],
            shared_hints: vec![],
        }
    }

    /// Two-page plan with a shared resource in Part 3.
    ///
    /// Part 2: [3 0 R, 6 0 R]          (page 1 dict + page-1-only content)
    /// Part 3: [5 0 R, 8 0 R]          (shared Resources + Font)
    /// Part 4: [1 0 R, 2 0 R, 4 0 R, 7 0 R]  (Catalog, Pages node, page 2 dict, page-2 content)
    ///
    /// Expected new numbering (starting at 2):
    ///   1 → reserved
    ///   2 → 3 0 R,  3 → 6 0 R   (Part 2)
    ///   4 → 5 0 R,  5 → 8 0 R   (Part 3)
    ///   6 → 1 0 R,  7 → 2 0 R,  8 → 4 0 R,  9 → 7 0 R   (Part 4)
    fn two_page_plan() -> LinearizationPlan {
        LinearizationPlan {
            part1_objects: vec![],
            part2_objects: vec![ObjectRef::new(3, 0), ObjectRef::new(6, 0)],
            part3_objects: vec![ObjectRef::new(5, 0), ObjectRef::new(8, 0)],
            part4_objects: vec![
                ObjectRef::new(1, 0),
                ObjectRef::new(2, 0),
                ObjectRef::new(4, 0),
                ObjectRef::new(7, 0),
            ],
            total_object_count: 8,
            root_ref: Some(ObjectRef::new(1, 0)),
            page_hints: vec![],
            shared_hints: vec![],
        }
    }

    // -----------------------------------------------------------------------
    // 1. param_dict_ref is always 1 0 R
    // -----------------------------------------------------------------------
    #[test]
    fn param_dict_ref_is_one_zero_r() {
        let pdr = RenumberMap::param_dict_ref();
        assert_eq!(pdr.number, 1);
        assert_eq!(pdr.generation, 0);
    }

    // -----------------------------------------------------------------------
    // 2. Single-page: Part 2 gets numbers 2..k, Part 4 follows
    // -----------------------------------------------------------------------
    #[test]
    fn single_page_part2_gets_low_numbers() {
        let plan = single_page_plan();
        let rn = RenumberMap::from_plan(&plan);

        // Part-2 objects start at new number 2.
        assert_eq!(
            rn.new_for_original(ObjectRef::new(3, 0)),
            Some(ObjectRef::new(2, 0))
        );
        assert_eq!(
            rn.new_for_original(ObjectRef::new(2, 0)),
            Some(ObjectRef::new(3, 0))
        );

        // Part-4 object follows.
        assert_eq!(
            rn.new_for_original(ObjectRef::new(1, 0)),
            Some(ObjectRef::new(4, 0))
        );
    }

    // -----------------------------------------------------------------------
    // 3. Two-page: Part 2 → Part 3 → Part 4 ordering is preserved
    // -----------------------------------------------------------------------
    #[test]
    fn two_page_part_ordering_correct() {
        let plan = two_page_plan();
        let rn = RenumberMap::from_plan(&plan);

        // Part 2
        assert_eq!(
            rn.new_for_original(ObjectRef::new(3, 0)),
            Some(ObjectRef::new(2, 0))
        );
        assert_eq!(
            rn.new_for_original(ObjectRef::new(6, 0)),
            Some(ObjectRef::new(3, 0))
        );

        // Part 3 follows immediately after Part 2
        assert_eq!(
            rn.new_for_original(ObjectRef::new(5, 0)),
            Some(ObjectRef::new(4, 0))
        );
        assert_eq!(
            rn.new_for_original(ObjectRef::new(8, 0)),
            Some(ObjectRef::new(5, 0))
        );

        // Part 4 follows after Part 3
        assert_eq!(
            rn.new_for_original(ObjectRef::new(1, 0)),
            Some(ObjectRef::new(6, 0))
        );
        assert_eq!(
            rn.new_for_original(ObjectRef::new(2, 0)),
            Some(ObjectRef::new(7, 0))
        );
        assert_eq!(
            rn.new_for_original(ObjectRef::new(4, 0)),
            Some(ObjectRef::new(8, 0))
        );
        assert_eq!(
            rn.new_for_original(ObjectRef::new(7, 0)),
            Some(ObjectRef::new(9, 0))
        );
    }

    // -----------------------------------------------------------------------
    // 4. Reverse lookup (original_for_new) is correct
    // -----------------------------------------------------------------------
    #[test]
    fn reverse_lookup_single_page() {
        let plan = single_page_plan();
        let rn = RenumberMap::from_plan(&plan);

        assert_eq!(
            rn.original_for_new(ObjectRef::new(2, 0)),
            Some(ObjectRef::new(3, 0))
        );
        assert_eq!(
            rn.original_for_new(ObjectRef::new(3, 0)),
            Some(ObjectRef::new(2, 0))
        );
        assert_eq!(
            rn.original_for_new(ObjectRef::new(4, 0)),
            Some(ObjectRef::new(1, 0))
        );
    }

    // -----------------------------------------------------------------------
    // 5. Sentinel slots return None
    // -----------------------------------------------------------------------
    #[test]
    fn sentinel_slots_return_none() {
        let plan = single_page_plan();
        let rn = RenumberMap::from_plan(&plan);

        // Slot 0 is unused
        assert_eq!(rn.original_for_new(ObjectRef::new(0, 0)), None);
        // Slot 1 is the param dict reservation — no original
        assert_eq!(rn.original_for_new(ObjectRef::new(1, 0)), None);
        // Out-of-range slot
        assert_eq!(rn.original_for_new(ObjectRef::new(99, 0)), None);
    }

    // -----------------------------------------------------------------------
    // 6. No original ObjectRef maps to two different new refs (uniqueness)
    // -----------------------------------------------------------------------
    #[test]
    fn uniqueness_no_duplicate_new_refs() {
        let plan = two_page_plan();
        let rn = RenumberMap::from_plan(&plan);

        let mut seen_new: std::collections::BTreeSet<ObjectRef> = std::collections::BTreeSet::new();
        for (new_ref, _orig) in rn.iter_in_layout_order() {
            assert!(
                seen_new.insert(new_ref),
                "new ref {:?} appears more than once in layout order",
                new_ref
            );
        }
    }

    // -----------------------------------------------------------------------
    // 7. iter_in_layout_order length equals total plan objects
    // -----------------------------------------------------------------------
    #[test]
    fn iter_layout_order_length_equals_plan_total() {
        let plan = two_page_plan();
        let total = plan.part2_objects.len() + plan.part3_objects.len() + plan.part4_objects.len();
        let rn = RenumberMap::from_plan(&plan);

        let iter_len = rn.iter_in_layout_order().count();
        assert_eq!(
            iter_len, total,
            "iter_in_layout_order must yield one entry per plan object"
        );
    }

    // -----------------------------------------------------------------------
    // 8. len() equals total plan objects + 1 (reserved param dict slot)
    // -----------------------------------------------------------------------
    #[test]
    fn len_includes_param_dict_reservation() {
        let plan = two_page_plan();
        let total = plan.part2_objects.len() + plan.part3_objects.len() + plan.part4_objects.len();
        let rn = RenumberMap::from_plan(&plan);

        assert_eq!(
            rn.len(),
            total + 1,
            "len() must be total objects + 1 for the param dict"
        );
    }

    // -----------------------------------------------------------------------
    // 9. Determinism: two maps from the same plan are equal
    // -----------------------------------------------------------------------
    #[test]
    fn deterministic_from_same_plan() {
        let plan = two_page_plan();
        let rn1 = RenumberMap::from_plan(&plan);
        let rn2 = RenumberMap::from_plan(&plan);
        assert_eq!(rn1, rn2, "RenumberMap must be deterministic");
    }

    // -----------------------------------------------------------------------
    // 10. iter_in_layout_order yields entries in ascending new-number order
    // -----------------------------------------------------------------------
    #[test]
    fn iter_layout_order_is_ascending() {
        let plan = two_page_plan();
        let rn = RenumberMap::from_plan(&plan);

        let new_numbers: Vec<u32> = rn.iter_in_layout_order().map(|(r, _)| r.number).collect();
        let mut sorted = new_numbers.clone();
        sorted.sort_unstable();
        assert_eq!(
            new_numbers, sorted,
            "iter_in_layout_order must yield ascending new numbers"
        );
    }

    // -----------------------------------------------------------------------
    // 11. All new refs in iter_in_layout_order have generation == 0
    // -----------------------------------------------------------------------
    #[test]
    fn new_refs_have_generation_zero() {
        let plan = two_page_plan();
        let rn = RenumberMap::from_plan(&plan);

        for (new_ref, _orig) in rn.iter_in_layout_order() {
            assert_eq!(
                new_ref.generation, 0,
                "all new object refs must have generation 0"
            );
        }
    }

    // -----------------------------------------------------------------------
    // original_for_new: generation guard
    // -----------------------------------------------------------------------

    #[test]
    fn original_for_new_rejects_non_zero_generation() {
        let plan = single_page_plan();
        let rn = RenumberMap::from_plan(&plan);

        // The renumber map only assigns gen 0; any query with non-zero gen
        // must return None even if the number itself is in range.
        assert_eq!(rn.original_for_new(ObjectRef::new(2, 1)), None);
        assert_eq!(rn.original_for_new(ObjectRef::new(3, 99)), None);
        // Sanity: gen 0 still works.
        assert_eq!(
            rn.original_for_new(ObjectRef::new(2, 0)),
            Some(ObjectRef::new(3, 0))
        );
    }
}
