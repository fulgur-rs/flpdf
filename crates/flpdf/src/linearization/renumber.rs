//! Object renumbering for linearized PDF output (ISO 32000-1 Annex F).
//!
//! After the [`LinearizationPlan`] has partitioned all objects into Parts 2–4,
//! this module assigns new object numbers that match qpdf's renumber order.
//! qpdf's `calculateLinearizationData` + `writeLinearized` produce the layout
//! `[part9_head, ParamDict, lc_root, HintStream, part6, part7, part8,
//! part9_tail]`. We model that with the following slots:
//!
//! | New number | Meaning |
//! |------------|---------|
//! | 1          | Pages tree (qpdf `obj_user_to_objects_["/Pages"]` head). Skipped if absent. |
//! | 2          | Info dict (qpdf `lc_other` lead). Skipped if absent. |
//! | param      | **Reserved** — linearization parameter dictionary (Part 1). |
//! | catalog    | Catalog (qpdf `lc_root`). Skipped if absent. |
//! | hint       | **Reserved** — primary hint stream. |
//! | next..a    | Part 2 — first-page objects (plan order). |
//! | a+1..b     | Part 3 — shared objects (plan order). |
//! | b+1..N     | Part 4 remaining — everything not promoted (plan order). |
//!
//! The `param` and `hint` slots are *dynamic*: when an upstream object is
//! absent from the plan its slot is simply not consumed, so emitted object
//! numbers stay contiguous. Use [`RenumberMap::param_dict_ref`] and
//! [`RenumberMap::hint_stream_slot`] to query their actual positions instead
//! of assuming `1` / `next_free()`. With the fixture corpus (one/two/three-
//! page PDFs that all carry `/Info`) the slots are 1=Pages, 2=Info,
//! 3=ParamDict, 4=Catalog, 5=HintStream, 6+=Part 2, matching qpdf
//! byte-for-byte.
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
//! * It does **not** move objects between parts (e.g. relocating the Catalog
//!   into Part 3 / first-page section) — that is a partition-level decision
//!   for [`LinearizationPlan`].

use crate::ObjectRef;

use super::plan::LinearizationPlan;
use std::collections::{BTreeMap, BTreeSet};

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
    /// `new_number → original_ref`. Sentinels (number == 0) mark the two
    /// reserved slots (param dict, hint stream) and slot 0 (unused).
    by_new_number: Vec<ObjectRef>,
    /// `original_ref → new_ref`.
    by_original: BTreeMap<ObjectRef, ObjectRef>,
    /// Slot reserved for the linearization parameter dictionary (Part 1).
    /// The writer emits this object number on its `1 0 obj`-equivalent line
    /// when serialising Part 1.
    param_dict_slot: u32,
    /// Slot reserved for the primary hint stream.
    /// The linearization writer allocates this number for the hint stream
    /// object it emits between Part 4 head and Part 6 (first-page section).
    hint_stream_slot: u32,
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
    /// Slot allocation mirrors qpdf's `writeLinearized` first-/second-half
    /// renumber pass:
    /// 1. **Slot 1**: pages tree (`plan.pages_tree_ref`) when promotable.
    /// 2. **Slot 2**: info dict (`plan.info_ref`) when promotable.
    /// 3. **Param dict** (reserved sentinel): linearization parameter dict.
    /// 4. **Catalog**: `plan.root_ref` when promotable.
    /// 5. **Hint stream** (reserved sentinel): primary hint stream.
    /// 6. Part 2 objects in plan order.
    /// 7. Part 3 objects in plan order.
    /// 8. Remaining Part 4 objects in plan order (any ref already promoted
    ///    above is skipped here so each ref maps exactly once).
    ///
    /// A ref counts as *promotable* when [`Option`] is `Some(r)` and `r` is
    /// a member of `plan.part4_objects`. Absent or non-Part-4 refs are
    /// silently skipped — the slot is not consumed, so subsequent slots
    /// shift down to keep object numbers contiguous.
    ///
    /// # Panics
    ///
    /// * In any build, panics if `plan.parts_are_disjoint()` is false. A
    ///   duplicate inside `part4_objects` whose value also happens to be
    ///   one of the promoted refs would be silently dropped by the
    ///   skip-already-promoted branch of the Part 4 loop, so the
    ///   disjointness check is the only path that catches it.
    /// * In any build, panics if the inner `push` helper encounters a
    ///   duplicate while inserting into `by_original` — kept as
    ///   defence-in-depth.
    pub fn from_plan(plan: &LinearizationPlan) -> Self {
        assert!(
            plan.parts_are_disjoint(),
            "LinearizationPlan invariant violated: parts must be disjoint \
             (Part 2 ∪ Part 3 ∪ Part 4 must contain each ObjectRef at most once)"
        );

        // Two sentinel slots are always reserved (param dict + hint stream).
        // The capacity hint is best-effort; the actual length is determined
        // by which optional refs are promotable.
        let total_parts =
            plan.part2_objects.len() + plan.part3_objects.len() + plan.part4_objects.len();
        let capacity = total_parts + 3; // slots 0, param dict, hint stream

        let mut by_new_number: Vec<ObjectRef> = Vec::with_capacity(capacity);
        let mut by_original: BTreeMap<ObjectRef, ObjectRef> = BTreeMap::new();

        // Slot 0: unused (PDF object numbers start at 1).
        by_new_number.push(SENTINEL);

        let push_real = |original: ObjectRef,
                         by_new_number: &mut Vec<ObjectRef>,
                         by_original: &mut BTreeMap<ObjectRef, ObjectRef>| {
            let new_number = by_new_number.len() as u32;
            let new_ref = ObjectRef::new(new_number, 0);
            by_new_number.push(original);
            assert!(
                by_original.insert(original, new_ref).is_none(),
                "duplicate original ObjectRef in LinearizationPlan: {original:?}"
            );
        };

        let part4_membership: BTreeSet<ObjectRef> = plan.part4_objects.iter().copied().collect();
        let promote = |slot_owner: Option<ObjectRef>,
                       by_new_number: &mut Vec<ObjectRef>,
                       by_original: &mut BTreeMap<ObjectRef, ObjectRef>| {
            if let Some(r) = slot_owner {
                if part4_membership.contains(&r) && !by_original.contains_key(&r) {
                    push_real(r, by_new_number, by_original);
                }
            }
        };

        // 1. Pages tree, 2. Info — the two "part9 head" promotions.
        promote(plan.pages_tree_ref, &mut by_new_number, &mut by_original);
        promote(plan.info_ref, &mut by_new_number, &mut by_original);

        // 3. Param dict (reserved).
        let param_dict_slot = by_new_number.len() as u32;
        by_new_number.push(SENTINEL);

        // 4. Catalog.
        promote(plan.root_ref, &mut by_new_number, &mut by_original);

        // 5. Hint stream (reserved).
        let hint_stream_slot = by_new_number.len() as u32;
        by_new_number.push(SENTINEL);

        // 6. Part 2 in plan order.
        for &original in &plan.part2_objects {
            push_real(original, &mut by_new_number, &mut by_original);
        }

        // 7. Part 3 in plan order.
        for &original in &plan.part3_objects {
            push_real(original, &mut by_new_number, &mut by_original);
        }

        // 8. Part 4 remaining (skip refs already promoted above).
        for &original in &plan.part4_objects {
            if by_original.contains_key(&original) {
                continue;
            }
            push_real(original, &mut by_new_number, &mut by_original);
        }

        Self {
            by_new_number,
            by_original,
            param_dict_slot,
            hint_stream_slot,
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
    /// `None` if the number is out of range, points at a sentinel slot
    /// (slot 0, param dict, or hint stream), or carries a non-zero
    /// generation (renumbered objects are always at generation 0).
    pub fn original_for_new(&self, new: ObjectRef) -> Option<ObjectRef> {
        if new.generation != 0 {
            return None;
        }
        let idx = new.number as usize;
        if idx == 0 || idx >= self.by_new_number.len() {
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

    /// The [`ObjectRef`] reserved for the linearization parameter dictionary
    /// (Part 1). With qpdf-aligned slot allocation this is no longer a
    /// constant — it shifts depending on which "part9 head" objects sit
    /// ahead of it. Use this getter instead of hard-coding `1 0 R`.
    pub fn param_dict_ref(&self) -> ObjectRef {
        ObjectRef::new(self.param_dict_slot, 0)
    }

    /// Object number reserved for the primary hint stream. Allocated between
    /// the catalog and the Part 2 (first-page) section, matching qpdf's
    /// `hint_id` placement in `writeLinearized`.
    pub fn hint_stream_slot(&self) -> u32 {
        self.hint_stream_slot
    }

    /// Total number of *allocated* object slots, including the reserved
    /// param-dict and hint-stream sentinels.
    ///
    /// Equals `|promoted refs| + |Part 2| + |Part 3| + |remaining Part 4| + 2`
    /// (the `+2` accounts for the two sentinel reservations).
    pub fn len(&self) -> usize {
        // by_new_number has slots 0..N; slot 0 is unused, so the highest
        // allocated number is by_new_number.len() - 1.
        self.by_new_number.len() - 1
    }

    /// `true` if no plan objects were mapped (only reservations exist).
    pub fn is_empty(&self) -> bool {
        self.by_original.is_empty()
    }

    /// Returns `true` when the param-dict slot still holds its sentinel.
    /// Used by `Part1Bytes::build` to assert that no plan object has been
    /// mapped on top of the reservation.
    pub fn param_dict_slot_is_reserved(&self) -> bool {
        matches!(
            self.by_new_number.get(self.param_dict_slot as usize),
            Some(r) if r.number == 0 && r.generation == 0
        )
    }

    // -----------------------------------------------------------------------
    // Iteration
    // -----------------------------------------------------------------------

    /// Iterate over `(new_ref, original_ref)` pairs in layout order (new
    /// object number ascending).
    ///
    /// Sentinel slots (0, param dict, hint stream) are filtered out because
    /// they have no corresponding original object; the writer handles them
    /// separately.
    pub fn iter_in_layout_order(&self) -> impl Iterator<Item = (ObjectRef, ObjectRef)> + '_ {
        self.by_new_number
            .iter()
            .enumerate()
            .skip(1) // slot 0 is the always-unused sentinel
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
    /// Expected new numbering with qpdf-aligned slot allocation
    /// (pages_tree_ref / info_ref are None so those slots are not consumed;
    /// root_ref = 1 is promotable from Part 4):
    ///
    ///   slot 1 → reserved (param dict)
    ///   slot 2 → 1 0 R   (Catalog, promoted from Part 4)
    ///   slot 3 → reserved (hint stream)
    ///   slot 4 → 3 0 R   (Part 2 first)
    ///   slot 5 → 2 0 R   (Part 2 second)
    fn single_page_plan() -> LinearizationPlan {
        LinearizationPlan {
            part1_objects: vec![],
            part2_objects: vec![ObjectRef::new(3, 0), ObjectRef::new(2, 0)],
            part3_objects: vec![],
            part4_objects: vec![ObjectRef::new(1, 0)],
            total_object_count: 3,
            root_ref: Some(ObjectRef::new(1, 0)),
            pages_tree_ref: None,
            info_ref: None,
            page_hints: vec![PageHintEntry::placeholder(ObjectRef::new(3, 0))],
            shared_hints: vec![],
            per_page_private_objects: vec![],
        }
    }

    /// Two-page plan with a shared resource in Part 3.
    ///
    /// Part 2: [3 0 R, 6 0 R]          (page 1 dict + page-1-only content)
    /// Part 3: [5 0 R, 8 0 R]          (shared Resources + Font)
    /// Part 4: [1 0 R, 2 0 R, 4 0 R, 7 0 R]  (Catalog, Pages node, page 2 dict, page-2 content)
    ///
    /// Expected new numbering with qpdf-aligned slot allocation
    /// (pages_tree_ref / info_ref are None; root_ref = 1 is promoted):
    ///
    ///   slot 1 → reserved (param dict)
    ///   slot 2 → 1 0 R   (Catalog, promoted from Part 4)
    ///   slot 3 → reserved (hint stream)
    ///   slot 4 → 3 0 R,  slot 5 → 6 0 R   (Part 2)
    ///   slot 6 → 5 0 R,  slot 7 → 8 0 R   (Part 3)
    ///   slot 8 → 2 0 R,  slot 9 → 4 0 R, slot 10 → 7 0 R   (Part 4 remainder)
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
            pages_tree_ref: None,
            info_ref: None,
            page_hints: vec![],
            shared_hints: vec![],
            per_page_private_objects: vec![],
        }
    }

    // -----------------------------------------------------------------------
    // 1. param_dict_ref reflects the dynamic param-dict slot
    // -----------------------------------------------------------------------
    #[test]
    fn param_dict_ref_matches_param_dict_slot() {
        let plan = single_page_plan();
        let rn = RenumberMap::from_plan(&plan);
        // single_page_plan has no promotable pages/info refs, so the param
        // dict lands at slot 1.
        assert_eq!(rn.param_dict_ref(), ObjectRef::new(1, 0));
    }

    // -----------------------------------------------------------------------
    // 2. Single-page slot layout
    // -----------------------------------------------------------------------
    #[test]
    fn single_page_slot_layout() {
        let plan = single_page_plan();
        let rn = RenumberMap::from_plan(&plan);

        // Catalog (root_ref) lands at slot 2 (right after the param dict).
        assert_eq!(rn.new_for_original(ObjectRef::new(1, 0)).unwrap().number, 2);
        // Hint stream takes slot 3 (reserved).
        assert_eq!(rn.hint_stream_slot(), 3);
        // Part 2 starts at slot 4.
        assert_eq!(rn.new_for_original(ObjectRef::new(3, 0)).unwrap().number, 4);
        assert_eq!(rn.new_for_original(ObjectRef::new(2, 0)).unwrap().number, 5);
    }

    // -----------------------------------------------------------------------
    // 3. Two-page: Part 2 → Part 3 → Part 4 remainder ordering is preserved
    // -----------------------------------------------------------------------
    #[test]
    fn two_page_part_ordering_correct() {
        let plan = two_page_plan();
        let rn = RenumberMap::from_plan(&plan);

        // Catalog promoted to slot 2.
        assert_eq!(rn.new_for_original(ObjectRef::new(1, 0)).unwrap().number, 2);
        // Hint stream at slot 3.
        assert_eq!(rn.hint_stream_slot(), 3);
        // Part 2 starts at slot 4.
        assert_eq!(rn.new_for_original(ObjectRef::new(3, 0)).unwrap().number, 4);
        assert_eq!(rn.new_for_original(ObjectRef::new(6, 0)).unwrap().number, 5);
        // Part 3 follows.
        assert_eq!(rn.new_for_original(ObjectRef::new(5, 0)).unwrap().number, 6);
        assert_eq!(rn.new_for_original(ObjectRef::new(8, 0)).unwrap().number, 7);
        // Part 4 remainder (root_ref already promoted, so 2, 4, 7 only).
        assert_eq!(rn.new_for_original(ObjectRef::new(2, 0)).unwrap().number, 8);
        assert_eq!(rn.new_for_original(ObjectRef::new(4, 0)).unwrap().number, 9);
        assert_eq!(
            rn.new_for_original(ObjectRef::new(7, 0)).unwrap().number,
            10
        );
    }

    // -----------------------------------------------------------------------
    // 4. Reverse lookup
    // -----------------------------------------------------------------------
    #[test]
    fn reverse_lookup_single_page() {
        let plan = single_page_plan();
        let rn = RenumberMap::from_plan(&plan);

        // Slot 2 → Catalog (Part 4 promoted).
        assert_eq!(
            rn.original_for_new(ObjectRef::new(2, 0)),
            Some(ObjectRef::new(1, 0))
        );
        // Slot 4 → first Part 2 object.
        assert_eq!(
            rn.original_for_new(ObjectRef::new(4, 0)),
            Some(ObjectRef::new(3, 0))
        );
        // Slot 5 → second Part 2 object.
        assert_eq!(
            rn.original_for_new(ObjectRef::new(5, 0)),
            Some(ObjectRef::new(2, 0))
        );
    }

    // -----------------------------------------------------------------------
    // 5. Sentinel slots return None
    // -----------------------------------------------------------------------
    #[test]
    fn sentinel_slots_return_none() {
        let plan = single_page_plan();
        let rn = RenumberMap::from_plan(&plan);

        // Slot 0 is the always-unused sentinel.
        assert_eq!(rn.original_for_new(ObjectRef::new(0, 0)), None);
        // Param dict reservation — no original.
        assert_eq!(
            rn.original_for_new(ObjectRef::new(rn.param_dict_ref().number, 0)),
            None
        );
        // Hint stream reservation — no original.
        assert_eq!(
            rn.original_for_new(ObjectRef::new(rn.hint_stream_slot(), 0)),
            None
        );
        // Out-of-range slot.
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
    // 8. len() includes the two reserved slots (param dict + hint stream)
    // -----------------------------------------------------------------------
    #[test]
    fn len_includes_param_dict_and_hint_reservations() {
        let plan = two_page_plan();
        let total = plan.part2_objects.len() + plan.part3_objects.len() + plan.part4_objects.len();
        let rn = RenumberMap::from_plan(&plan);

        assert_eq!(
            rn.len(),
            total + 2,
            "len() must be total objects + 2 for the param dict and hint stream slots"
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
        assert_eq!(rn.original_for_new(ObjectRef::new(4, 99)), None);
        // Sanity: gen 0 still works (slot 4 is the first Part 2 object).
        assert_eq!(
            rn.original_for_new(ObjectRef::new(4, 0)),
            Some(ObjectRef::new(3, 0))
        );
    }

    // -----------------------------------------------------------------------
    // Reserved-slot helpers
    // -----------------------------------------------------------------------

    #[test]
    fn param_dict_slot_is_reserved_after_from_plan() {
        // The from_plan constructor pushes SENTINEL into the param-dict
        // slot, so a freshly built map always has it reserved.
        let plan = single_page_plan();
        let rn = RenumberMap::from_plan(&plan);
        assert!(rn.param_dict_slot_is_reserved());
    }

    #[test]
    fn param_dict_slot_is_reserved_returns_false_when_overwritten() {
        // Defensive: simulate a corrupted map where the param-dict slot has
        // been overwritten with a real ObjectRef.
        let plan = single_page_plan();
        let mut rn = RenumberMap::from_plan(&plan);
        let slot = rn.param_dict_ref().number as usize;
        rn.by_new_number[slot] = ObjectRef::new(99, 0);
        assert!(!rn.param_dict_slot_is_reserved());
    }

    #[test]
    fn hint_stream_slot_points_to_unused_slot() {
        let plan = two_page_plan();
        let rn = RenumberMap::from_plan(&plan);
        let h = rn.hint_stream_slot();
        // No plan original maps onto the hint stream slot.
        assert!(rn.original_for_new(ObjectRef::new(h, 0)).is_none());
        // The hint stream sits immediately after the catalog (slot 2).
        assert_eq!(h, 3);
    }

    // -----------------------------------------------------------------------
    // Part 4 head promotion (qpdf alignment, ステージ A)
    // -----------------------------------------------------------------------

    /// With qpdf-aligned slot allocation, `[pages, info, catalog]` consume
    /// slots 1, 2, 4 (slot 3 is the param-dict reservation; slot 5 is the
    /// hint stream). Pin that full layout.
    #[test]
    fn qpdf_layout_pages_info_paramdict_catalog_hint_part2() {
        let pages_ref = ObjectRef::new(8, 0);
        let info_ref = ObjectRef::new(7, 0);
        let catalog_ref = ObjectRef::new(6, 0);
        let other_part4 = ObjectRef::new(9, 0);

        let plan = LinearizationPlan {
            part1_objects: vec![],
            part2_objects: vec![ObjectRef::new(2, 0)],
            part3_objects: vec![],
            // Plan-order is deliberately scrambled so we can prove the
            // promotion runs ahead of the natural iteration.
            part4_objects: vec![catalog_ref, info_ref, pages_ref, other_part4],
            total_object_count: 5,
            root_ref: Some(catalog_ref),
            pages_tree_ref: Some(pages_ref),
            info_ref: Some(info_ref),
            page_hints: vec![],
            shared_hints: vec![],
            per_page_private_objects: vec![],
        };

        let rn = RenumberMap::from_plan(&plan);

        assert_eq!(rn.new_for_original(pages_ref).unwrap().number, 1);
        assert_eq!(rn.new_for_original(info_ref).unwrap().number, 2);
        assert_eq!(rn.param_dict_ref().number, 3);
        assert_eq!(rn.new_for_original(catalog_ref).unwrap().number, 4);
        assert_eq!(rn.hint_stream_slot(), 5);
        // Part 2 starts immediately after the hint stream slot.
        assert_eq!(rn.new_for_original(ObjectRef::new(2, 0)).unwrap().number, 6);
        // Remaining Part 4 follows Part 2.
        assert_eq!(rn.new_for_original(other_part4).unwrap().number, 7);
    }

    /// When pages/info refs are absent, their slots collapse and the param
    /// dict / catalog / hint stream shift earlier accordingly.
    #[test]
    fn missing_promotion_refs_shift_param_and_hint_slots_earlier() {
        let plan = single_page_plan(); // pages_tree_ref = None, info_ref = None
        let rn = RenumberMap::from_plan(&plan);
        // Param dict goes to slot 1 because no promotable refs precede it.
        assert_eq!(rn.param_dict_ref().number, 1);
        // Catalog (1 0 R, promoted via root_ref) goes to slot 2.
        assert_eq!(rn.new_for_original(ObjectRef::new(1, 0)).unwrap().number, 2);
        // Hint stream slot is right after the catalog.
        assert_eq!(rn.hint_stream_slot(), 3);
    }

    /// If the promoted ref is already in Part 2 or Part 3 (atypical input —
    /// the catalog reaches into the first-page closure), the promotion
    /// silently skips it so the slot bijection stays intact.
    #[test]
    fn promotion_skips_refs_not_in_part4() {
        let pages_ref = ObjectRef::new(10, 0);
        let plan = LinearizationPlan {
            part1_objects: vec![],
            part2_objects: vec![pages_ref, ObjectRef::new(2, 0)],
            part3_objects: vec![],
            part4_objects: vec![ObjectRef::new(3, 0)],
            total_object_count: 3,
            root_ref: None,
            pages_tree_ref: Some(pages_ref),
            info_ref: None,
            page_hints: vec![],
            shared_hints: vec![],
            per_page_private_objects: vec![],
        };
        let rn = RenumberMap::from_plan(&plan);
        // Layout: param=1, hint=2 (nothing promoted, no catalog either),
        // then Part 2 starts at slot 3.
        assert_eq!(rn.param_dict_ref().number, 1);
        assert_eq!(rn.hint_stream_slot(), 2);
        assert_eq!(rn.new_for_original(pages_ref).unwrap().number, 3);
        assert_eq!(rn.new_for_original(ObjectRef::new(2, 0)).unwrap().number, 4);
        // Part 4 remainder lands last.
        assert_eq!(rn.new_for_original(ObjectRef::new(3, 0)).unwrap().number, 5);
    }

    /// A duplicated ref inside `part4_objects` whose value also happens to
    /// be a promotion target would be silently dropped by the promoted-set
    /// skip if the disjointness check did not run in release. Pin both the
    /// detection and the message.
    #[test]
    #[should_panic(expected = "parts must be disjoint")]
    fn from_plan_panics_on_duplicate_in_part4_even_in_release() {
        let dup = ObjectRef::new(5, 0);
        let plan = LinearizationPlan {
            part1_objects: vec![],
            part2_objects: vec![ObjectRef::new(2, 0)],
            part3_objects: vec![],
            part4_objects: vec![dup, dup],
            total_object_count: 3,
            root_ref: Some(dup),
            pages_tree_ref: Some(dup),
            info_ref: None,
            page_hints: vec![],
            shared_hints: vec![],
            per_page_private_objects: vec![],
        };
        let _ = RenumberMap::from_plan(&plan);
    }
}
