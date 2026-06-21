//! Object renumbering for linearized PDF output (ISO 32000-1 Annex F).
//!
//! After the [`LinearizationPlan`] has partitioned all objects into Parts 2–4,
//! this module assigns new object numbers that match qpdf's renumber order.
//! qpdf's `calculateLinearizationData` + `writeLinearized` produce the layout
//! `[part7, part8, part9_head, ParamDict, lc_root, HintStream, part6/Part2,
//! Part3, part9_tail]`. We model that with the following slots:
//!
//! | New number  | Meaning |
//! |-------------|---------|
//! | 1..M        | part7: other pages' private objects (qpdf part7). |
//! | M+1..N      | part8: other pages' shared objects (qpdf part8). |
//! | N+1         | Pages tree (qpdf part9 head). Skipped if absent. |
//! | N+2         | Info dict (qpdf `lc_other`). Skipped if absent. |
//! | N+3..O      | part9 outline objects (qpdf `lc_outlines`, classic). Skipped if absent. |
//! | O+1..P      | Remaining `part4_rest` objects (e.g. `lc_thumbnail`). Skipped if absent. |
//! | param       | **Reserved** — linearization parameter dictionary (Part 1). |
//! | catalog     | Catalog (qpdf `lc_root`). Skipped if absent. |
//! | hint        | **Reserved** — primary hint stream. |
//! | next..a     | Part 2 — first-page objects (plan order). |
//! | a+1..b      | Part 3 — shared objects (plan order). |
//! | b+1..end    | Part 6 — outline objects (qpdf `lc_outlines`, `UseOutlines`). Skipped if absent. |
//!
//! The `param` and `hint` slots are *dynamic*: when an upstream object is
//! absent from the plan its slot is simply not consumed, so emitted object
//! numbers stay contiguous. Use [`RenumberMap::param_dict_ref`] and
//! [`RenumberMap::hint_stream_slot`] to query their actual positions instead
//! of assuming fixed values. With the fixture corpus (one/two/three-page PDFs
//! that all carry `/Info`) the slots are (all matching qpdf byte-for-byte):
//!
//! - one-page:   1=Pages, 2=Info, 3=ParamDict, 4=Catalog, 5=HintStream, 6+=Part2
//! - two-page:   1=Page2dict, 2=Page2content, 3=Pages, 4=Info, 5=ParamDict, 6=Catalog, 7=Hint, 8+=Part2
//! - three-page: 1-4=Page2+3 objs, 5=Pages, 6=Info, 7=ParamDict, 8=Catalog, 9=Hint, 10+=Part2
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

/// Slot numbers reserved by [`RenumberMap::place_objstm_members_per_half`] for
/// the split linearized xref-stream layout.
///
/// All zero (the [`Default`]) when there were no ObjStm batches — the writer
/// then keeps its classic Part-1 mini-xref + single Part-6 path verbatim.
///
/// Under the per-half compressed-last layout the two cross-reference streams
/// split the object-number space by file half: the main (second-half) xref
/// covers `[0, second_half_count)` and the first-page (first-half) xref covers
/// `[second_half_count, /Size)`.  Within each half the ObjStm container objects
/// (type-1) are numbered last among the uncompressed objects and the members
/// (type-2) last of all, so each `/Index` range is strictly `type-0?/type-1*`
/// then `type-2*` — the ordering qpdf's linearization checker requires.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObjStmRelocation {
    /// Object number of the first-page (first-half) cross-reference stream.
    pub first_xref_slot: u32,
    /// Object number of the main (second-half) cross-reference stream.
    pub main_xref_slot: u32,
    /// Number of objects in the second half (= the first object number of the
    /// first half).  The main xref's `/Index` covers `[0, second_half_count)`
    /// and the first-page xref's `/Index` covers `[second_half_count, /Size)`.
    pub second_half_count: u32,
    /// Per-batch ObjStm container object numbers, in flat (open-document, then
    /// Part-3, then Part-4) batch order — the order the writer's ObjStm container
    /// builder consumes them.
    pub container_numbers: Vec<u32>,
}

impl ObjStmRelocation {
    /// `true` when no relocation happened (no ObjStm batches).
    pub fn is_empty(&self) -> bool {
        self.first_xref_slot == 0 && self.main_xref_slot == 0
    }
}

impl RenumberMap {
    // -----------------------------------------------------------------------
    // Construction
    // -----------------------------------------------------------------------

    /// Build a renumber map from a [`LinearizationPlan`].
    ///
    /// Slot allocation mirrors qpdf's `writeLinearized` second-half + first-half
    /// renumber pass:
    /// 1. **part7**: `plan.part4_other_pages_private` in plan order.
    /// 2. **part8**: `plan.part4_other_pages_shared` in plan order.
    /// 3. **pages tree**: `plan.pages_tree_ref` when in `part4_rest`.
    /// 4. **info dict**: `plan.info_ref` when in `part4_rest`.
    /// 5. **Param dict** (reserved sentinel): linearization parameter dict.
    /// 6. **Catalog**: `plan.root_ref` when in `part4_rest`.
    /// 7. **Hint stream** (reserved sentinel): primary hint stream.
    /// 8. Part 2 objects in plan order.
    /// 9. Part 3 objects in plan order.
    /// 10. Remaining `part4_rest` objects in plan order (any ref already
    ///     promoted above is skipped so each ref maps exactly once).
    ///
    /// A ref counts as *promotable* when the [`Option`] is `Some(r)` and `r`
    /// is a member of `plan.part4_rest`. Absent or non-`part4_rest` refs are
    /// silently skipped — the slot is not consumed, so subsequent slots
    /// shift down to keep object numbers contiguous.
    ///
    /// # Panics
    ///
    /// * In any build, panics if `plan.parts_are_disjoint()` is false.
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
        let total_parts = plan.part2_objects.len()
            + plan.part3_objects.len()
            + plan.part4_other_pages_private.len()
            + plan.part4_other_pages_shared.len()
            + plan.part6_outline_objects.len()
            + plan.part9_outline_objects.len()
            + plan.part4_rest.len()
            + plan.part4_open_document_plain.len();
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

        // Promotion targets (pages tree, info, catalog) must come from
        // part4_rest to avoid double-counting with part7/part8 objects.
        let part4_rest_membership: BTreeSet<ObjectRef> = plan.part4_rest.iter().copied().collect();
        let promote = |slot_owner: Option<ObjectRef>,
                       by_new_number: &mut Vec<ObjectRef>,
                       by_original: &mut BTreeMap<ObjectRef, ObjectRef>| {
            if let Some(r) = slot_owner {
                if part4_rest_membership.contains(&r) && !by_original.contains_key(&r) {
                    push_real(r, by_new_number, by_original);
                }
            }
        };

        // Second-half renumber order (qpdf slot assignment):
        //
        //  slot 1..    part7 (other pages' private) in plan order
        //  slot N+1..  part8 (other pages' shared) in plan order
        //  slot ..     pages_tree (if in part4_rest)
        //  slot ..     info (if in part4_rest)
        //  slot ..     part9 outline objects (classic, !UseOutlines) — qpdf lc_outlines
        //  slot ..     part4_rest remaining (e.g. lc_thumbnail_private/shared)
        //  slot ..     <param dict reserved>
        //  slot ..     root_ref / Catalog (if in part4_rest)
        //  slot ..     <hint stream reserved>
        //
        // First-half follows:
        //  slot ..     Part 2 in plan order
        //  slot ..     Part 3 in plan order
        //  slot ..     part6 outline objects (classic, UseOutlines) — qpdf lc_outlines

        // 1. part7 (other pages' private) in plan order.
        for &original in &plan.part4_other_pages_private {
            push_real(original, &mut by_new_number, &mut by_original);
        }

        // 2. part8 (other pages' shared) in plan order.
        for &original in &plan.part4_other_pages_shared {
            push_real(original, &mut by_new_number, &mut by_original);
        }

        // 3. pages_tree, 4. info — the two "part9 head" promotions from part4_rest.
        promote(plan.pages_tree_ref, &mut by_new_number, &mut by_original);
        promote(plan.info_ref, &mut by_new_number, &mut by_original);

        // 3b. part9 outline objects (classic, !UseOutlines).
        // qpdf places lc_outlines between info and the param dict in the
        // second-half renumber pass, giving them consecutive second-half numbers.
        for &original in &plan.part9_outline_objects {
            push_real(original, &mut by_new_number, &mut by_original);
        }

        // 3c. Remaining part4_rest objects (e.g. lc_thumbnail_private/shared) go to
        // the second half, after the part9 head promotions and before the param dict.
        // root_ref (catalog) is excluded — it is a first-half standalone object
        // that gets its slot via the promote() call at step 6 below.
        for &original in &plan.part4_rest {
            if by_original.contains_key(&original) {
                continue; // already placed: pages_tree, info, or outline objects
            }
            if plan.root_ref == Some(original) {
                continue; // catalog stays first-half; promoted at step 6
            }
            push_real(original, &mut by_new_number, &mut by_original);
        }

        // 5. Param dict (reserved).
        let param_dict_slot = by_new_number.len() as u32;
        by_new_number.push(SENTINEL);

        // 6. Catalog (promoted from part4_rest).
        promote(plan.root_ref, &mut by_new_number, &mut by_original);

        // 6b. Ineligible open-document plain objects (generate mode only).
        // These objects are in the open-document set but cannot be packed into
        // an ObjStm (e.g. stream objects such as /AP /N appearance streams).
        // qpdf emits them as plain indirect objects between the Catalog and the
        // OD ObjStm containers, so they occupy object numbers immediately after
        // the Catalog and before the hint stream sentinel.
        for &original in &plan.part4_open_document_plain {
            push_real(original, &mut by_new_number, &mut by_original);
        }

        // 7. Hint stream (reserved).
        let hint_stream_slot = by_new_number.len() as u32;
        by_new_number.push(SENTINEL);

        // 8. Part 2 in plan order.
        for &original in &plan.part2_objects {
            push_real(original, &mut by_new_number, &mut by_original);
        }

        // 9. Part 3 in plan order.
        for &original in &plan.part3_objects {
            push_real(original, &mut by_new_number, &mut by_original);
        }

        // 9b. part6 outline objects (classic, UseOutlines).
        // These go into the first-half section before /E, between Part 3 and
        // the hint stream, matching qpdf's lc_outlines (part6) order.
        for &original in &plan.part6_outline_objects {
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
    // ObjStm per-half compressed-last placement
    // -----------------------------------------------------------------------

    /// Renumber the ObjStm container(s) + members so that, within each file
    /// half, the compressed objects are numbered **last** — qpdf 11.9.0's
    /// `writeLinearized` per-half compressed-last order.
    ///
    /// `open_document_batches` are the qpdf part4 (open-document) ObjStm
    /// batches — numbered in the FIRST half, right after the catalog and before
    /// the hint stream (qpdf `part4_first_obj`). `first_half_batches` are the
    /// Part-3 (first-page section / qpdf part6) ObjStm batches — their container
    /// and members are numbered LAST within the first half — and
    /// `second_half_batches` are the Part-4 (rest-of-document / qpdf part7/8/9)
    /// batches — numbered last within the second half.  All are ordered lists of
    /// member original refs in pair-table order.
    ///
    /// # Layout
    ///
    /// [`from_plan`](Self::from_plan) already numbers the second-half objects
    /// (qpdf part7/part8 + the promoted Pages/Info "part9 head") at the LOW
    /// slots `1..param_dict_slot`, and the first-half objects (param dict,
    /// catalog, hint stream, Part-2, Part-3-plain, remaining Part-4) at the
    /// HIGH slots `param_dict_slot..`.  This method preserves that
    /// second-half-low / first-half-high spine and rebuilds it as:
    ///
    /// **Second half** (covered by the main xref, `/Index [0, second_half_count)`):
    /// 1. every second-half non-member object, compacted in order (type-1);
    /// 2. every second-half (Part-4) ObjStm container, batch-ordered (type-1) —
    ///    numbered AMONG the uncompressed objects, before the xref (qpdf counts
    ///    containers in `second_half_uncompressed`);
    /// 3. the main (second-half) cross-reference *stream* slot (type-1);
    /// 4. every second-half (Part-4) ObjStm member, batch-ordered (type-2).
    ///
    /// **First half** (covered by the first-page xref, `/Index [second_half_count, /Size)`):
    /// 5. the linearization parameter dictionary slot (type-1, reserved);
    /// 6. the first-page (first-half) cross-reference *stream* slot (type-1);
    /// 7. the catalog (the leading first-half non-member), then the open-document
    ///    (qpdf part4) ObjStm containers (type-1), then the hint-stream slot,
    ///    then the remaining first-half non-members compacted in order (type-1);
    /// 8. every first-half (Part-3 / qpdf part6) ObjStm container, batch-ordered
    ///    (type-1);
    /// 8b. the first-half post-container plain objects in `first_half_post_plain`
    ///    (an ineligible OD+outline stream routed to qpdf part6 under
    ///    `/PageMode /UseOutlines`) — emitted AFTER the part6 container, mirroring
    ///    the second half's `second_half_post_plain` (type-1);
    /// 9. the open-document (qpdf part4) ObjStm members, then the first-page
    ///    (qpdf part6) ObjStm members, batch-ordered (type-2) — qpdf numbers
    ///    part4 members before part6 (`vecs1 = {part4, part6}`).
    ///
    /// Within each half the container(s) (type-1) precede every member
    /// (type-2) and the members are the highest numbers of that half, so the
    /// main xref's single `/Index` range is strictly `type-0? type-1* type-2*`
    /// and the first-page xref's range is `type-1* type-2*` — the
    /// non-interleaved ordering qpdf's linearization checker requires.  This
    /// matches qpdf 11.9.0's `writeLinearized` per-half compressed-last order:
    /// the first-page shared dicts (+ `/Pages` tree + `/Info`) live in a
    /// first-half container numbered right after the first page, so `/O` (the
    /// first-page object number) equals qpdf's value.
    ///
    /// Returns an [`ObjStmRelocation`] carrying the two xref-stream object
    /// numbers, the per-batch container object numbers in container-consumption
    /// order (open-document batches first, then Part-3, then Part-4 — the order
    /// the writer's `ObjStmLayout` container builder consumes them), and
    /// `second_half_count` (the half-split point), so the writer can build its
    /// `ObjStmLayout` and emit the split xref streams against the placed map
    /// without re-deriving numbers independently.
    ///
    /// The param-dict and hint-stream sentinel reservations are preserved
    /// (their slot numbers are recomputed to track the new layout).
    ///
    /// # Panics
    ///
    /// Panics if a member ref is not present in the map (a planner / renumber
    /// inconsistency the caller must not paper over).
    pub fn place_objstm_members_per_half(
        &mut self,
        open_document_batches: &[Vec<ObjectRef>],
        first_half_batches: &[Vec<ObjectRef>],
        second_half_batches: &[Vec<ObjectRef>],
        second_half_anchors: &[Option<ObjectRef>],
        second_half_post_plain: &BTreeSet<ObjectRef>,
        first_half_post_plain: &BTreeSet<ObjectRef>,
    ) -> ObjStmRelocation {
        // Fast path: nothing to place — leave the map byte-identical.
        let no_open = open_document_batches.iter().all(|b| b.is_empty());
        let no_first = first_half_batches.iter().all(|b| b.is_empty());
        let no_second = second_half_batches.iter().all(|b| b.is_empty());
        if no_open && no_first && no_second {
            return ObjStmRelocation::default();
        }

        let member_set: BTreeSet<ObjectRef> = open_document_batches
            .iter()
            .chain(first_half_batches)
            .chain(second_half_batches)
            .flat_map(|b| b.iter().copied())
            .collect();

        // The half boundary in `from_plan`'s output: slots `1..old_param_slot`
        // are the second half (part7/part8 + promoted Pages/Info), and
        // `old_param_slot..` are the first half (param dict head onward).
        let old_param_slot = self.param_dict_slot;
        let old_hint_slot = self.hint_stream_slot;

        // Partition the existing non-member, non-sentinel objects by half,
        // preserving order.  The first-half walk also records where the hint
        // sentinel sits relative to its first-half neighbours so it can be
        // re-inserted at the same relative position below.  Members are dropped
        // here regardless of which half `from_plan` put them in (e.g. the
        // /Pages tree and /Info are promoted to the second half by `from_plan`
        // but are first-half ObjStm members under the qpdf member set); they
        // are re-placed in their batch's half below.
        let mut second_half_plain: Vec<ObjectRef> = Vec::new();
        let mut first_half_plain: Vec<ObjectRef> = Vec::new();
        // Index into `first_half_plain` at which the hint sentinel belongs
        // (i.e. how many first-half plain objects precede it).
        let mut hint_index_in_first_half: u32 = 0;
        for (old_idx, &original) in self.by_new_number.iter().enumerate().skip(1) {
            let old_idx = old_idx as u32;
            if old_idx == old_param_slot {
                // The param dict is the first-half head — handled explicitly
                // when rebuilding below, not stored in either plain list.
                continue;
            }
            if old_idx == old_hint_slot {
                // The hint stream is a first-half object; remember its relative
                // position so its number tracks the surrounding first-half
                // objects.
                hint_index_in_first_half = first_half_plain.len() as u32;
                continue;
            }
            if original.number == 0 {
                // An unexpected sentinel (defence-in-depth) — drop it; it
                // carries no object and the two real sentinels are handled
                // above.
                continue;
            }
            if member_set.contains(&original) {
                continue; // re-placed as an ObjStm member in its half below
            }
            if old_idx < old_param_slot {
                second_half_plain.push(original);
            } else {
                first_half_plain.push(original);
            }
        }

        // `container_numbers` must be returned in `build_from_batches` order
        // (open-document, then Part-3, then Part-4).  The open-document and
        // Part-3 containers are numbered in the FIRST half (high numbers) and
        // Part-4 in the SECOND half (low numbers), so the returned vector is NOT
        // ascending; the writer maps each batch to its container by position,
        // not by value.
        let count_nonempty =
            |batches: &[Vec<ObjectRef>]| batches.iter().filter(|b| !b.is_empty()).count();
        let mut first_half_container_numbers: Vec<u32> =
            Vec::with_capacity(count_nonempty(first_half_batches));
        let mut second_half_container_numbers: Vec<u32> =
            Vec::with_capacity(count_nonempty(second_half_batches));

        // Helper: append a half's container slots (type-1) then member slots
        // (type-2), recording the container numbers in batch order.
        let assert_member = |member: ObjectRef, by_original: &BTreeMap<ObjectRef, ObjectRef>| {
            assert!(
                by_original.contains_key(&member),
                "place_objstm_members_per_half: member {member:?} not present in \
                 RenumberMap (planner / renumber inconsistency)"
            );
        };

        // Rebuild the table in per-half compressed-last order.
        let mut new_by_new_number: Vec<ObjectRef> = Vec::with_capacity(self.by_new_number.len());
        new_by_new_number.push(SENTINEL); // slot 0

        // --- Second half ---
        // (1)+(2) second-half non-members (type-1) with each ObjStm container
        //     INTERLEAVED at its part-group end (type-1). qpdf numbers the
        //     second-half containers AMONG the uncompressed objects — before the
        //     main xref (QPDFWriter.cc:2578-2592 counts containers in
        //     `second_half_uncompressed`) — AND at their part position: a part7
        //     container sits at the END of its owning page's group (its synthetic
        //     ObjGen is the group's highest), so it follows that page's plain
        //     objects but precedes the next page. `second_half_anchors[bi]` is the
        //     plain object after which container `bi` is emitted (the group's last
        //     plain object); `None` means "append after all plain" (the caller's
        //     default — and the result is identical to interleaving when the
        //     container's group is the last one).
        let mut second_half_container_slot: Vec<Option<u32>> =
            vec![None; second_half_batches.len()];
        let mut emit_container = |bi: usize, table: &mut Vec<ObjectRef>| {
            if second_half_batches[bi].is_empty() || second_half_container_slot[bi].is_some() {
                return;
            }
            let container_num = table.len() as u32;
            table.push(SENTINEL); // container: a plain indirect, no original
            second_half_container_slot[bi] = Some(container_num);
        };
        // lc_thumbnail objects (non-member part4_rest streams) must be emitted
        // AFTER the ObjStm containers, matching qpdf's part9-tail placement.
        // Partition second_half_plain into pre- and post-container groups.
        let mut post_container_plain: Vec<ObjectRef> = Vec::new();
        for &original in &second_half_plain {
            if second_half_post_plain.contains(&original) {
                post_container_plain.push(original);
                continue;
            }
            new_by_new_number.push(original);
            for bi in 0..second_half_batches.len() {
                if second_half_anchors.get(bi).copied().flatten() == Some(original) {
                    emit_container(bi, &mut new_by_new_number);
                }
            }
        }
        // Containers with no anchor (or whose anchor is a post-container object)
        // go after all pre-container plain objects, in batch order.
        for bi in 0..second_half_batches.len() {
            emit_container(bi, &mut new_by_new_number);
        }
        // Post-container plain (lc_thumbnail / part9 tail): after all containers.
        for &original in &post_container_plain {
            new_by_new_number.push(original);
        }
        // Record container numbers in batch order (the writer maps batch ->
        // container by position).
        for (bi, batch) in second_half_batches.iter().enumerate() {
            if batch.is_empty() {
                continue;
            }
            second_half_container_numbers.push(
                second_half_container_slot[bi]
                    .expect("every non-empty second-half batch is assigned a container slot"),
            );
        }
        // (3) main (second-half) xref stream slot (type-1) — after every
        //     uncompressed object (plain + containers), before the members.
        let main_xref_slot = new_by_new_number.len() as u32;
        new_by_new_number.push(SENTINEL);
        // (4) Part-4 ObjStm members, batch-ordered (type-2) — last of the half.
        for batch in second_half_batches {
            for &member in batch {
                assert_member(member, &self.by_original);
                new_by_new_number.push(member);
            }
        }

        // The first first-half number = number of objects emitted so far
        // (slot 0 is the unused sentinel, so the count IS the next index).
        let second_half_count = new_by_new_number.len() as u32;

        // --- First half ---
        // (5) linearization parameter dictionary (type-1, reserved).
        let new_param_slot = new_by_new_number.len() as u32;
        new_by_new_number.push(SENTINEL);
        // (6) first-page (first-half) xref stream slot (type-1).
        let first_xref_slot = new_by_new_number.len() as u32;
        new_by_new_number.push(SENTINEL);
        // Open-document (qpdf part4) ObjStm container slots, recorded as they
        // are emitted at the hint-insertion point below.
        let mut open_document_container_numbers: Vec<u32> =
            Vec::with_capacity(count_nonempty(open_document_batches));
        // Helper: append the open-document (qpdf part4) ObjStm containers as
        // type-1 slots, recording their numbers. Called once, immediately
        // before the hint sentinel, so they are numbered right after the
        // catalog (qpdf `part4_first_obj`, before the hint stream).
        let emit_open_document_containers = |table: &mut Vec<ObjectRef>, nums: &mut Vec<u32>| {
            for batch in open_document_batches {
                if batch.is_empty() {
                    continue;
                }
                let container_num = table.len() as u32;
                table.push(SENTINEL); // container: a plain indirect, no original
                nums.push(container_num);
            }
        };

        // (7) first-half non-members, with the hint sentinel re-inserted at its
        //     recorded relative position (all type-1). At that point the
        //     open-document (qpdf part4) containers are emitted FIRST — right
        //     after the catalog and before the hint stream — to match qpdf's
        //     `part4_first_obj` … `hint_id` … `part6_first_obj` numbering.
        let mut new_hint_slot = 0u32;
        let mut open_document_emitted = false;
        // Ineligible part6 outline streams emitted AFTER the part6 container (the
        // first-half mirror of `post_container_plain`); see step (8b) below. The
        // hint-index check still uses the un-filtered enumerate index because
        // `from_plan` always places these (part6 outline objects, step 9b) after
        // the hint slot, so collecting them here never shifts the hint position.
        let mut first_half_post_container_plain: Vec<ObjectRef> = Vec::new();
        for (i, &original) in first_half_plain.iter().enumerate() {
            if i as u32 == hint_index_in_first_half {
                emit_open_document_containers(
                    &mut new_by_new_number,
                    &mut open_document_container_numbers,
                );
                open_document_emitted = true;
                new_hint_slot = new_by_new_number.len() as u32;
                new_by_new_number.push(SENTINEL);
            }
            if first_half_post_plain.contains(&original) {
                first_half_post_container_plain.push(original);
                continue;
            }
            new_by_new_number.push(original);
        }
        // Hint sentinel sits after the last first-half plain object when its
        // recorded index equals the first-half length (e.g. nothing follows it).
        // cov:ignore-start: unreachable in practice — `from_plan` always places
        // the Part-2 first-page objects (page dict + content, never ObjStm
        // members) in the first half AFTER the hint slot, so `first_half_plain`
        // is non-empty past the hint index and the loop above always emits the
        // hint sentinel; this fallback guards the degenerate empty-first-page
        // case that the planner does not produce.
        if hint_index_in_first_half as usize >= first_half_plain.len() {
            if !open_document_emitted {
                emit_open_document_containers(
                    &mut new_by_new_number,
                    &mut open_document_container_numbers,
                );
            }
            new_hint_slot = new_by_new_number.len() as u32;
            new_by_new_number.push(SENTINEL);
        }
        // cov:ignore-end
        // (8) Part-3 (first-page) ObjStm containers, batch-ordered (type-1) —
        //     last uncompressed objects of the first half (qpdf part6 containers).
        for batch in first_half_batches {
            if batch.is_empty() {
                continue;
            }
            let container_num = new_by_new_number.len() as u32;
            new_by_new_number.push(SENTINEL); // container: a plain indirect, no original
            first_half_container_numbers.push(container_num);
        }
        // (8b) First-half post-container plain objects (ineligible part6 outline
        //      streams): after the part6 container(s), before the compressed
        //      members — qpdf numbers the ineligible OD+outline stream AFTER its
        //      part6 ObjStm container (the first-half analogue of the second
        //      half's post-container plain pass).
        for &original in &first_half_post_container_plain {
            new_by_new_number.push(original);
        }
        // (9) ObjStm members, batch-ordered (type-2) — last of all. qpdf numbers
        //     the part4 (open-document) members before the part6 (first-page)
        //     members (`vecs1 = {part4, part6}`), so emit open-document first.
        for batch in open_document_batches {
            for &member in batch {
                assert_member(member, &self.by_original);
                new_by_new_number.push(member);
            }
        }
        for batch in first_half_batches {
            for &member in batch {
                assert_member(member, &self.by_original);
                new_by_new_number.push(member);
            }
        }

        // Container numbers in `build_from_batches` order: open-document, then
        // Part-3 (first-page), then Part-4 (second-half).
        let mut container_numbers = open_document_container_numbers;
        container_numbers.extend(first_half_container_numbers);
        container_numbers.extend(second_half_container_numbers);

        // Rebuild the forward index from the placed table.
        let mut new_by_original: BTreeMap<ObjectRef, ObjectRef> = BTreeMap::new();
        for (idx, &original) in new_by_new_number.iter().enumerate().skip(1) {
            if original.number == 0 {
                continue;
            }
            let prev = new_by_original.insert(original, ObjectRef::new(idx as u32, 0));
            assert!(
                prev.is_none(),
                "place_objstm_members_per_half: duplicate original {original:?} after placement"
            );
        }

        self.by_new_number = new_by_new_number;
        self.by_original = new_by_original;
        self.param_dict_slot = new_param_slot;
        self.hint_stream_slot = new_hint_slot;

        ObjStmRelocation {
            first_xref_slot,
            main_xref_slot,
            second_half_count,
            container_numbers,
        }
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
    /// part4_rest: [1 0 R]   (Catalog — no other pages so no part7/8)
    ///
    /// Expected new numbering with qpdf-aligned slot allocation
    /// (pages_tree_ref / info_ref are None so those slots are not consumed;
    /// root_ref = 1 is in part4_rest and gets promoted):
    ///
    ///   slot 1 → reserved (param dict)
    ///   slot 2 → 1 0 R   (Catalog, promoted from part4_rest)
    ///   slot 3 → reserved (hint stream)
    ///   slot 4 → 3 0 R   (Part 2 first)
    ///   slot 5 → 2 0 R   (Part 2 second)
    fn single_page_plan() -> LinearizationPlan {
        LinearizationPlan {
            part2_objects: vec![ObjectRef::new(3, 0), ObjectRef::new(2, 0)],
            part4_rest: vec![ObjectRef::new(1, 0)],
            total_object_count: 3,
            root_ref: Some(ObjectRef::new(1, 0)),
            page_hints: vec![PageHintEntry::placeholder(ObjectRef::new(3, 0))],
            ..Default::default()
        }
    }

    /// Two-page plan with a shared resource in Part 3.
    ///
    /// Part 2: [3 0 R, 6 0 R]          (page 1 dict + page-1-only content)
    /// Part 3: [5 0 R, 8 0 R]          (shared Resources + Font)
    /// part4_other_pages_private: [4 0 R, 7 0 R]  (page 2 dict + content, qpdf part7)
    /// part4_rest: [1 0 R, 2 0 R]      (Catalog, Pages node, qpdf part9)
    ///
    /// Expected new numbering with qpdf-aligned slot allocation
    /// (pages_tree_ref / info_ref are None; root_ref = 1 0 R in part4_rest):
    ///
    ///   slot 1 → 4 0 R   (part7 — page 2 dict)
    ///   slot 2 → 7 0 R   (part7 — page 2 content)
    ///   slot 3 → reserved (param dict)   -- no pages_tree/info to promote
    ///   slot 4 → 1 0 R   (Catalog, promoted from part4_rest)
    ///   slot 5 → reserved (hint stream)
    ///   slot 6 → 3 0 R,  slot 7 → 6 0 R   (Part 2)
    ///   slot 8 → 5 0 R,  slot 9 → 8 0 R   (Part 3)
    ///   slot 10 → 2 0 R   (part4_rest remainder)
    fn two_page_plan() -> LinearizationPlan {
        LinearizationPlan {
            part2_objects: vec![ObjectRef::new(3, 0), ObjectRef::new(6, 0)],
            part3_objects: vec![ObjectRef::new(5, 0), ObjectRef::new(8, 0)],
            part4_other_pages_private: vec![ObjectRef::new(4, 0), ObjectRef::new(7, 0)],
            part4_rest: vec![ObjectRef::new(1, 0), ObjectRef::new(2, 0)],
            total_object_count: 8,
            root_ref: Some(ObjectRef::new(1, 0)),
            ..Default::default()
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
    // 3. Two-page: part7 → part8 → part9_head → ParamDict → Catalog → Hint
    //              → Part 2 → Part 3 → part9 remainder ordering
    // -----------------------------------------------------------------------
    #[test]
    fn two_page_part_ordering_correct() {
        let plan = two_page_plan();
        let rn = RenumberMap::from_plan(&plan);

        // part7 (other pages' private): page-2 dict and content come first.
        assert_eq!(rn.new_for_original(ObjectRef::new(4, 0)).unwrap().number, 1);
        assert_eq!(rn.new_for_original(ObjectRef::new(7, 0)).unwrap().number, 2);
        // No pages_tree / info to promote.  step 3c places the remaining
        // part4_rest non-root object (2 0 R) in the second half at slot 3.
        assert_eq!(rn.new_for_original(ObjectRef::new(2, 0)).unwrap().number, 3);
        // Param dict lands at slot 4 (shifted by step 3c).
        assert_eq!(rn.param_dict_ref().number, 4);
        // Catalog promoted from part4_rest to slot 5.
        assert_eq!(rn.new_for_original(ObjectRef::new(1, 0)).unwrap().number, 5);
        // Hint stream at slot 6.
        assert_eq!(rn.hint_stream_slot(), 6);
        // Part 2 starts at slot 7.
        assert_eq!(rn.new_for_original(ObjectRef::new(3, 0)).unwrap().number, 7);
        assert_eq!(rn.new_for_original(ObjectRef::new(6, 0)).unwrap().number, 8);
        // Part 3 follows.
        assert_eq!(rn.new_for_original(ObjectRef::new(5, 0)).unwrap().number, 9);
        assert_eq!(
            rn.new_for_original(ObjectRef::new(8, 0)).unwrap().number,
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
        let total =
            plan.part2_objects.len() + plan.part3_objects.len() + plan.part4_objects().len();
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
        let total =
            plan.part2_objects.len() + plan.part3_objects.len() + plan.part4_objects().len();
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
        // two_page_plan: slots 1, 2 = part7; 3 = step3c (2 0 R from part4_rest);
        // 4 = param; 5 = catalog; 6 = hint.
        assert_eq!(h, 6);
    }

    // -----------------------------------------------------------------------
    // Part 4 head promotion (qpdf alignment, ステージ A)
    // -----------------------------------------------------------------------

    /// With qpdf-aligned slot allocation, `[pages, info, catalog]` consume
    /// slots 1, 2, 4 (slot 3 is the param-dict reservation; slot 5 is the
    /// hint stream). Pin that full layout.
    ///
    /// All three promotion targets (pages, info, catalog) live in `part4_rest`;
    /// `part4_other_pages_private` and `part4_other_pages_shared` are empty.
    #[test]
    fn qpdf_layout_pages_info_paramdict_catalog_hint_part2() {
        let pages_ref = ObjectRef::new(8, 0);
        let info_ref = ObjectRef::new(7, 0);
        let catalog_ref = ObjectRef::new(6, 0);
        let other_part4 = ObjectRef::new(9, 0);

        // Plan-order in part4_rest is deliberately scrambled so we can prove
        // the promotion runs ahead of the natural iteration order.
        let plan = LinearizationPlan {
            part2_objects: vec![ObjectRef::new(2, 0)],
            part4_rest: vec![catalog_ref, info_ref, pages_ref, other_part4],
            total_object_count: 5,
            root_ref: Some(catalog_ref),
            pages_tree_ref: Some(pages_ref),
            info_ref: Some(info_ref),
            ..Default::default()
        };

        let rn = RenumberMap::from_plan(&plan);

        assert_eq!(rn.new_for_original(pages_ref).unwrap().number, 1);
        assert_eq!(rn.new_for_original(info_ref).unwrap().number, 2);
        // step 3c places other_part4 (the only non-root/non-promoted part4_rest
        // entry) in the second half at slot 3 (before the param dict).
        assert_eq!(rn.new_for_original(other_part4).unwrap().number, 3);
        assert_eq!(rn.param_dict_ref().number, 4);
        assert_eq!(rn.new_for_original(catalog_ref).unwrap().number, 5);
        assert_eq!(rn.hint_stream_slot(), 6);
        // Part 2 starts immediately after the hint stream slot.
        assert_eq!(rn.new_for_original(ObjectRef::new(2, 0)).unwrap().number, 7);
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
    /// silently skips it because `pages_tree_ref` is not in `part4_rest`.
    #[test]
    fn promotion_skips_refs_not_in_part4_rest() {
        let pages_ref = ObjectRef::new(10, 0);
        let plan = LinearizationPlan {
            part2_objects: vec![pages_ref, ObjectRef::new(2, 0)],
            part4_rest: vec![ObjectRef::new(3, 0)],
            total_object_count: 3,
            pages_tree_ref: Some(pages_ref), // in Part 2, not in part4_rest → skipped
            ..Default::default()
        };
        let rn = RenumberMap::from_plan(&plan);
        // step 3c places 3 0 R (the only part4_rest entry; root_ref=None so no
        // root skip) in the second half at slot 1, before the param dict.
        assert_eq!(rn.new_for_original(ObjectRef::new(3, 0)).unwrap().number, 1);
        // param dict shifts to slot 2; hint to slot 3.
        assert_eq!(rn.param_dict_ref().number, 2);
        assert_eq!(rn.hint_stream_slot(), 3);
        // Part 2 follows: pages_ref (10 0 R) at slot 4, then 2 0 R at slot 5.
        assert_eq!(rn.new_for_original(pages_ref).unwrap().number, 4);
        assert_eq!(rn.new_for_original(ObjectRef::new(2, 0)).unwrap().number, 5);
    }

    /// A duplicated ref inside `part4_rest` (and also in `part4_other_pages_private`)
    /// would be silently dropped by the promoted-set skip if the disjointness check
    /// did not run. Pin both the detection and the message.
    #[test]
    #[should_panic(expected = "parts must be disjoint")]
    fn from_plan_panics_on_duplicate_in_part4_even_in_release() {
        let dup = ObjectRef::new(5, 0);
        // dup appears in both part4_other_pages_private and part4_rest — disjoint violation.
        let plan = LinearizationPlan {
            part2_objects: vec![ObjectRef::new(2, 0)],
            part4_other_pages_private: vec![dup],
            part4_rest: vec![dup],
            total_object_count: 3,
            root_ref: Some(dup),
            pages_tree_ref: Some(dup),
            ..Default::default()
        };
        let _ = RenumberMap::from_plan(&plan);
    }

    /// With two SECOND-half (Part-4) ObjStm batches the per-half placement
    /// must, within the second half, number **all** container slots first (among
    /// the uncompressed objects, matching qpdf's `second_half_uncompressed`
    /// count), then the main xref slot, then **all** member slots — so the main
    /// xref's single `/Index` range stays strictly `type-1* type-2*` (qpdf
    /// rejects a type-1 entry after a type-2 one in a cross-reference stream).
    /// The single-batch layout is a degenerate case of the same rule.
    #[test]
    fn per_half_orders_containers_then_main_xref_then_members() {
        let plan = two_page_plan();
        let mut rn = RenumberMap::from_plan(&plan);

        // Two Part-4 (second-half) batches; no Part-3 (first-half) batch.  All
        // refs are present in the map (part3_objects / part4_other_pages_private
        // of two_page_plan).
        let second_half_batches = vec![
            vec![ObjectRef::new(5, 0)],
            vec![ObjectRef::new(4, 0), ObjectRef::new(7, 0)],
        ];
        let relocation = rn.place_objstm_members_per_half(
            &[],
            &[],
            &second_half_batches,
            &[],
            &BTreeSet::new(),
            &BTreeSet::new(),
        );

        assert_eq!(
            relocation.container_numbers.len(),
            2,
            "two non-empty batches must yield two container numbers"
        );
        let c0 = relocation.container_numbers[0];
        let c1 = relocation.container_numbers[1];
        let members: Vec<u32> = second_half_batches
            .iter()
            .flatten()
            .map(|&m| rn.new_for_original(m).unwrap().number)
            .collect();

        // Every container number must be strictly below every member number.
        let max_container = c0.max(c1);
        let min_member = *members.iter().min().unwrap();
        assert!(
            max_container < min_member,
            "all containers ({c0}, {c1}) must precede all members ({members:?})"
        );
        // Containers are contiguous and recorded in batch order.
        assert_eq!(
            c1,
            c0 + 1,
            "container slots must be contiguous, batch-ordered"
        );
        // The container block precedes the main (second-half) xref slot: qpdf
        // numbers second-half ObjStm containers among the uncompressed objects,
        // before the xref stream (finding-4).
        assert!(
            c1 < relocation.main_xref_slot,
            "container block ({c0}, {c1}) must precede the main xref slot ({})",
            relocation.main_xref_slot
        );
        // The main xref slot still precedes every member (type-1 before type-2).
        assert!(
            relocation.main_xref_slot < min_member,
            "main xref slot ({}) must precede all members ({members:?})",
            relocation.main_xref_slot
        );
        // Per-half split: every container and member lives in the SECOND half
        // (below `second_half_count`); the param dict and first-page xref open
        // the FIRST half (at / just above `second_half_count`).
        let max_member = *members.iter().max().unwrap();
        assert!(
            max_member < relocation.second_half_count,
            "members ({members:?}) must be numbered in the second half \
             (below second_half_count = {})",
            relocation.second_half_count
        );
        assert_eq!(
            rn.param_dict_ref().number,
            relocation.second_half_count,
            "the param dict opens the first half"
        );
        assert_eq!(
            relocation.first_xref_slot,
            relocation.second_half_count + 1,
            "the first-page xref slot follows the param dict at the first half head"
        );
        // The first-page xref number is strictly above every second-half
        // object, so the first-half `/Index` range carries no member.
        assert!(
            relocation.first_xref_slot > max_member,
            "first-page xref slot ({}) must be above every member ({members:?})",
            relocation.first_xref_slot
        );
    }

    /// A FIRST-half (Part-3) batch must be numbered LAST within the first half:
    /// its container + members sit ABOVE the param dict, first-page xref, and
    /// every first-half plain object, and the members are the highest numbers
    /// of the whole map.  This is the qpdf member-set layout (first-page shared
    /// dicts + /Pages + /Info compressed into a first-half container).
    #[test]
    fn per_half_places_part3_batch_in_first_half() {
        let plan = two_page_plan();
        let mut rn = RenumberMap::from_plan(&plan);

        // One Part-3 (first-half) batch: 5 0 R + 8 0 R (both part3_objects).
        let first_half_batches = vec![vec![ObjectRef::new(5, 0), ObjectRef::new(8, 0)]];
        let relocation = rn.place_objstm_members_per_half(
            &[],
            &first_half_batches,
            &[],
            &[],
            &BTreeSet::new(),
            &BTreeSet::new(),
        );

        assert_eq!(
            relocation.container_numbers.len(),
            1,
            "one non-empty Part-3 batch yields one container number"
        );
        let container = relocation.container_numbers[0];
        let members: Vec<u32> = first_half_batches
            .iter()
            .flatten()
            .map(|&m| rn.new_for_original(m).unwrap().number)
            .collect();

        // The container + members live in the FIRST half (>= second_half_count).
        assert!(
            container >= relocation.second_half_count,
            "Part-3 container ({container}) must be in the first half \
             (>= second_half_count = {})",
            relocation.second_half_count
        );
        // No Part-4 batch → the main xref slot is the last second-half object,
        // so second_half_count = main_xref_slot + 1.
        assert_eq!(
            relocation.second_half_count,
            relocation.main_xref_slot + 1,
            "with no Part-4 batch the second half ends at the main xref slot"
        );
        // The container precedes its members, and the members are the highest
        // numbers in the whole map.
        let min_member = *members.iter().min().unwrap();
        let max_member = *members.iter().max().unwrap();
        assert!(
            container < min_member,
            "container ({container}) must precede its members ({members:?})"
        );
        assert_eq!(
            max_member as usize,
            rn.len(),
            "Part-3 members must be the highest object numbers"
        );
        // The param dict and first-page xref open the first half, BELOW the
        // container — so the first-page xref's `/Index` range is type-1* (the
        // first-half plain objects + container) then type-2* (the members).
        assert!(
            relocation.first_xref_slot < container,
            "first-page xref ({}) must precede the Part-3 container ({container})",
            relocation.first_xref_slot
        );
    }

    /// An empty inner batch (defence-in-depth: the writer normally filters these
    /// upstream) must be skipped — it yields no container slot and no members,
    /// so only the non-empty batch produces a container. Covers all three batch
    /// lists (open-document, first-half, second-half).
    #[test]
    fn per_half_skips_empty_batches() {
        let plan = two_page_plan();
        let mut rn = RenumberMap::from_plan(&plan);

        // Each batch list carries one empty and one non-empty batch.
        let open_document_batches = vec![vec![], vec![ObjectRef::new(8, 0)]];
        let first_half_batches = vec![vec![], vec![ObjectRef::new(5, 0)]];
        let second_half_batches = vec![vec![], vec![ObjectRef::new(4, 0)]];
        let relocation = rn.place_objstm_members_per_half(
            &open_document_batches,
            &first_half_batches,
            &second_half_batches,
            &[],
            &BTreeSet::new(),
            &BTreeSet::new(),
        );

        // Empty batches contribute no container numbers: one open-document + one
        // first-half + one second-half = exactly three containers.
        assert_eq!(
            relocation.container_numbers.len(),
            3,
            "empty batches must be skipped; only the three non-empty batches yield containers"
        );
        // Every non-empty member is present in the placed map.
        assert!(rn.new_for_original(ObjectRef::new(8, 0)).is_some());
        assert!(rn.new_for_original(ObjectRef::new(5, 0)).is_some());
        assert!(rn.new_for_original(ObjectRef::new(4, 0)).is_some());
    }
}
