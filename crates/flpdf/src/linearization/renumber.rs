//! Object renumbering for linearized PDF output (ISO 32000-1 Annex F).
//!
//! After the [`LinearizationPlan`] has partitioned all objects into Parts 2–4,
//! this module assigns new object numbers that place them in the correct
//! linearized order:
//!
//! | New number | Meaning |
//! |------------|---------|
//! | 1          | Reserved for the linearization parameter dictionary (Part 1). |
//! | 2 .. a     | Part 2 — first-page objects (plan order). |
//! | a+1 .. b   | Part 3 — shared objects (plan order). |
//! | b+1 ..     | Part 4 head — promoted refs: pages tree, info, catalog. |
//! | .. N       | Part 4 remaining — anything not promoted (plan order). |
//!
//! The Part 4 head promotion mirrors qpdf's `calculateLinearizationData`
//! ordering of its `lc_root` / `lc_other` sets and is what brings flpdf's
//! emitted object numbers closer to qpdf's. Each promoted slot is filled
//! only when the corresponding [`LinearizationPlan`] field is `Some` **and**
//! the ref is actually a member of `part4_objects`; otherwise it is silently
//! skipped (no slot is reserved for an absent ref).
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
//! * The param-dict and hint-stream slots stay at the legacy positions
//!   (slot 1 and `next_free()`); shifting them to qpdf's positions
//!   (slot 3 and slot 5) is a separate change that touches `Part1Bytes`,
//!   the linearization writer, back-patch, and check together.

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
    /// 4. **Part-4 head promotion**: if [`LinearizationPlan::pages_tree_ref`],
    ///    [`info_ref`], or [`root_ref`] points at an object that is actually
    ///    in `part4_objects`, those refs are appended next, in that exact
    ///    order. Promoted refs are skipped during the natural Part 4 pass so
    ///    each ref is mapped exactly once.
    /// 5. Then the remaining Part-4 objects in plan order.
    ///
    /// # Panics
    ///
    /// * In any build, panics if `plan.parts_are_disjoint()` is false. The
    ///   check runs in release too because the Part 4 head promotion (step 4
    ///   above) skips refs that are already in `by_original`, and a duplicate
    ///   inside `part4_objects` whose value also happens to be one of the
    ///   promoted refs would be silently dropped instead of detected by the
    ///   inner `push` assert.
    /// * In any build, panics if the inner `push` helper encounters a
    ///   duplicate while inserting into `by_original` — kept as
    ///   defence-in-depth even though the disjointness check above already
    ///   forbids that state.
    pub fn from_plan(plan: &LinearizationPlan) -> Self {
        assert!(
            plan.parts_are_disjoint(),
            "LinearizationPlan invariant violated: parts must be disjoint \
             (Part 2 ∪ Part 3 ∪ Part 4 must contain each ObjectRef at most once)"
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

        let push = |original: ObjectRef,
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

        // Slots 2..a: Part 2 in plan order.
        for &original in &plan.part2_objects {
            push(original, &mut by_new_number, &mut by_original);
        }

        // Slots a+1..b: Part 3 in plan order.
        for &original in &plan.part3_objects {
            push(original, &mut by_new_number, &mut by_original);
        }

        // Slots b+1..N: Part 4.
        //
        // Promote the qpdf "part9 head" objects to the front of Part 4 so the
        // emitted object numbers move closer to qpdf's. Order mirrors qpdf
        // `calculateLinearizationData`: pages tree first (root key `/Pages`
        // user set), then any catalog-level entries surfaced through
        // `lc_other`. `root_ref` (Catalog) ends the promoted prefix because
        // qpdf places `lc_root` in part4 (PDF 1.4 numbering = catalog
        // section), which lands immediately after the linearization
        // parameter dict in the final file even though the renumber pass
        // visits the rest of part9 first.
        //
        // Bytes-identical numbering with qpdf also requires reserving the
        // linearization parameter dict and hint stream slots at qpdf's
        // positions; that change is deferred to a follow-up so this PR can
        // land in isolation. The promotion below alone is enough to make
        // [pages_tree, info, catalog] adjacent in the layout.
        let promoted: Vec<ObjectRef> = [plan.pages_tree_ref, plan.info_ref, plan.root_ref]
            .into_iter()
            .flatten()
            .collect();
        let promoted_set: BTreeSet<ObjectRef> = promoted.iter().copied().collect();

        for &original in &promoted {
            if plan.part4_objects.contains(&original) && !by_original.contains_key(&original) {
                push(original, &mut by_new_number, &mut by_original);
            }
        }

        for &original in &plan.part4_objects {
            if promoted_set.contains(&original) {
                continue;
            }
            push(original, &mut by_new_number, &mut by_original);
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

    /// Returns `true` when slot 1 is still the reserved sentinel (i.e. no
    /// plan object has been mapped to new object number 1).
    ///
    /// `original_for_new(ObjectRef::new(1, 0))` deliberately rejects any
    /// `idx < 2` and returns `None` regardless of whether the slot is
    /// reserved or not, so it cannot be used as a slot-1 collision check.
    /// This helper inspects `by_new_number[1]` directly: it is `SENTINEL`
    /// (number == 0) when the slot is intact, or a real `ObjectRef` if
    /// `from_plan` has been modified to allocate plan objects starting at
    /// new number 1.  Used by `Part1Bytes::build` to assert that slot 1
    /// stays reserved for the linearization parameter dictionary.
    pub fn slot_one_is_reserved(&self) -> bool {
        matches!(
            self.by_new_number.get(1),
            Some(r) if r.number == 0 && r.generation == 0
        )
    }

    /// The next object number that is NOT used by any plan object or by the
    /// reserved param-dict slot.
    ///
    /// Use this when you need to allocate an extra object number (e.g. the
    /// hint stream) that must not collide with the renumbered body objects.
    /// Equivalent to `by_new_number.len() as u32` — the slot just past the
    /// highest allocated number.  Returning this through a named helper makes
    /// the contract explicit and decouples the writer from the internal
    /// `len()` semantics (which returns "highest allocated number" — easy to
    /// read off-by-one from).
    pub fn next_free(&self) -> u32 {
        self.by_new_number.len() as u32
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
            pages_tree_ref: None,
            info_ref: None,
            page_hints: vec![],
            shared_hints: vec![],
            per_page_private_objects: vec![],
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

    // -----------------------------------------------------------------------
    // next_free helper
    // -----------------------------------------------------------------------

    #[test]
    fn next_free_returns_slot_past_highest_allocated() {
        let plan = single_page_plan();
        let rn = RenumberMap::from_plan(&plan);
        // single_page_plan: 3 plan objects + slot 0 (sentinel) + slot 1 (param dict)
        // = highest allocated is 4, next_free should be 5.
        assert_eq!(rn.len(), 4);
        assert_eq!(rn.next_free(), 5);
    }

    #[test]
    fn slot_one_is_reserved_after_from_plan() {
        // The from_plan constructor pushes SENTINEL into slot 1 explicitly,
        // so a freshly built map always has the slot reserved.
        let plan = single_page_plan();
        let rn = RenumberMap::from_plan(&plan);
        assert!(rn.slot_one_is_reserved());
    }

    #[test]
    fn slot_one_is_reserved_returns_false_when_overwritten() {
        // Defensive: simulate a corrupted map where slot 1 has been
        // overwritten with a real ObjectRef.  slot_one_is_reserved must
        // detect this — that is the entire reason for the helper
        // (`original_for_new(1, 0)` always returns None and so cannot).
        let plan = single_page_plan();
        let mut rn = RenumberMap::from_plan(&plan);
        rn.by_new_number[1] = ObjectRef::new(99, 0);
        assert!(!rn.slot_one_is_reserved());
    }

    #[test]
    fn next_free_does_not_collide_with_any_allocated() {
        let plan = two_page_plan();
        let rn = RenumberMap::from_plan(&plan);
        let nf = rn.next_free();
        // next_free is the slot AFTER the last allocated number — by design
        // there is no original mapped to it.
        assert!(rn.original_for_new(ObjectRef::new(nf, 0)).is_none());
        // And it is one past `len()` (which itself is the highest allocated).
        assert_eq!(nf, (rn.len() as u32) + 1);
    }

    // -----------------------------------------------------------------------
    // Part 4 head promotion (qpdf alignment, ステージ A)
    // -----------------------------------------------------------------------

    /// When `plan` exposes `pages_tree_ref`, `info_ref`, and `root_ref` and
    /// all three are members of `part4_objects`, they must come out of
    /// `from_plan` in that exact order (Pages → Info → Catalog) at the start
    /// of the Part 4 slot range. The remaining Part 4 objects keep their plan
    /// order behind them.
    #[test]
    fn part4_head_is_pages_info_catalog_when_refs_are_provided() {
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

        // Slot 1 stays reserved for the param dict; Part 2 fills slot 2;
        // Part 4 head should then be Pages(3), Info(4), Catalog(5), other(6).
        assert_eq!(rn.new_for_original(pages_ref).unwrap().number, 3);
        assert_eq!(rn.new_for_original(info_ref).unwrap().number, 4);
        assert_eq!(rn.new_for_original(catalog_ref).unwrap().number, 5);
        assert_eq!(rn.new_for_original(other_part4).unwrap().number, 6);
    }

    /// `None` refs fall through silently — the plan's natural Part 4 order
    /// is preserved when there is nothing to promote.
    #[test]
    fn part4_promotion_is_inert_when_refs_are_none() {
        let plan = single_page_plan(); // pages_tree_ref = None, info_ref = None
        let rn = RenumberMap::from_plan(&plan);
        // single_page_plan's Part 4 is [1 0 R]; it should be the next slot
        // after Part 2 (which occupies slots 2, 3).
        assert_eq!(rn.new_for_original(ObjectRef::new(1, 0)).unwrap().number, 4);
    }

    /// If the promoted ref is already in Part 2 or Part 3 (atypical input —
    /// the catalog reaches into the first-page closure), the promotion
    /// silently skips it so the slot bijection stays intact.
    #[test]
    fn part4_promotion_skips_refs_not_in_part4() {
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
        // pages_ref stays in its Part 2 slot (= 2).
        assert_eq!(rn.new_for_original(pages_ref).unwrap().number, 2);
        // The remaining Part 4 object lands right after Part 2.
        assert_eq!(rn.new_for_original(ObjectRef::new(3, 0)).unwrap().number, 4);
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
