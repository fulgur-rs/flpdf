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
    second_half_container_nums: &std::collections::BTreeSet<u32>,
    open_document_container_nums: &std::collections::BTreeSet<u32>,
) -> u32 {
    // Page 0's section is Part 2 (always plain) followed by Part 3 (plain or
    // folded into a first-half container), plus any outline objects routed to
    // the first-page section when /PageMode /UseOutlines is set. qpdf counts
    // all part6 objects in entries.at(0).nobjects
    // (QPDF_linearization.cc:1222).
    //
    // Open-document containers are placed before /O (not in [/O,/E)), so they
    // are not part of the first-page section and must not be counted here.
    let exclude: std::collections::BTreeSet<u32> = second_half_container_nums
        .union(open_document_container_nums)
        .copied()
        .collect();
    objstm_folded_count(
        plan.part2_objects
            .iter()
            .chain(&plan.part3_objects)
            .chain(&plan.outline_first_page_members),
        member_to_container,
        &exclude,
    )
}

/// Container numbers that a NON-first page must not count toward its page-offset
/// object count (or page byte length): only a container owned entirely by one
/// non-first page (a part7 container) is a section object of that page. A
/// container is excluded when it touches no non-first page, more than one
/// non-first page (part8 — the even split can co-locate two pages' privates), or
/// any non-page member (a first-page part2/part3 object, a part8-shared, or a
/// part9 object). Page 0 is handled separately (it owns its first-page
/// containers) and is not consulted here.
pub(crate) fn non_page_owned_containers(
    plan: &LinearizationPlan,
    member_to_container: &std::collections::BTreeMap<ObjectRef, (u32, u32)>,
) -> std::collections::BTreeSet<u32> {
    use std::collections::{BTreeMap, BTreeSet};

    let page_private_sets: Vec<BTreeSet<ObjectRef>> = plan
        .per_page_private_objects
        .iter()
        .map(|v| v.iter().copied().collect())
        .collect();

    let mut container_pages: BTreeMap<u32, BTreeSet<usize>> = BTreeMap::new();
    let mut has_nonpage_member: BTreeSet<u32> = BTreeSet::new();
    let mut all_containers: BTreeSet<u32> = BTreeSet::new();
    for (member, &(cnum, _)) in member_to_container {
        all_containers.insert(cnum);
        match (1..page_private_sets.len()).find(|&i| page_private_sets[i].contains(member)) {
            Some(i) => {
                container_pages.entry(cnum).or_default().insert(i);
            }
            None => {
                has_nonpage_member.insert(cnum);
            }
        }
    }

    all_containers
        .into_iter()
        .filter(|c| {
            let single_page = container_pages.get(c).is_some_and(|s| s.len() == 1);
            !single_page || has_nonpage_member.contains(c)
        })
        .collect()
}

/// Count the objects a page contributes to its linearization section when its
/// compressed members are folded into ObjStm containers: each plain indirect
/// counts once, and each *distinct* container counts once (its members are not
/// counted individually). Mirrors qpdf, where a page's section holds the
/// container object — not the members inside it.
///
/// `exclude` lists container numbers that belong to a *different* section: a
/// page's private object can land (via the global even split) in a first-page
/// (part6) container, which is physically in page 0's section, not this page's.
/// Such a member is counted only via page 0's container, so it must add nothing
/// here.
fn objstm_folded_count<'a>(
    objects: impl Iterator<Item = &'a ObjectRef>,
    member_to_container: &std::collections::BTreeMap<ObjectRef, (u32, u32)>,
    exclude: &std::collections::BTreeSet<u32>,
) -> u32 {
    use std::collections::BTreeSet;

    let mut plain = 0u32;
    let mut containers: BTreeSet<u32> = BTreeSet::new();
    for r in objects {
        match member_to_container.get(r) {
            Some(&(container_num, _)) => {
                if !exclude.contains(&container_num) {
                    containers.insert(container_num);
                }
            }
            None => plain += 1,
        }
    }
    plain + containers.len() as u32
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
    /// caller is expected to construct consistently.
    ///
    /// # Shared-hint ordering
    ///
    /// The `renumber` map orders the folded first-page shared-hint section by
    /// physical object number — the order in which a linearized-PDF reader
    /// walks first-page shared objects positionally.
    pub fn from_plan(
        plan: &LinearizationPlan,
        renumber: &RenumberMap,
        member_to_container: &std::collections::BTreeMap<ObjectRef, (u32, u32)>,
        container_shared_rank: &std::collections::BTreeMap<u32, u32>,
        second_half_container_nums: &std::collections::BTreeSet<u32>,
        open_document_container_nums: &std::collections::BTreeSet<u32>,
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
        let shared_hints = plan.canonical_shared_hints(
            member_to_container,
            renumber,
            second_half_container_nums,
            open_document_container_nums,
        );

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
            object_counts[0] = page0_object_count_with_objstm(
                plan,
                member_to_container,
                second_half_container_nums,
                open_document_container_nums,
            );
            // Pages 1..N: fold each page's private objects into their containers
            // too. A page whose private resources are compressed into a part7
            // ObjStm holds the container (one object) in its section, not the
            // members — so its object_count must count the container once, exactly
            // like page 0. Without this a page with a part7 container reports its
            // members individually, inflating `bits_object_count_delta`.
            //
            // A non-first page may count only a container that is ENTIRELY its
            // own private section object (a part7 container). The global even
            // split can place a page's private object in the first-page (part6)
            // container — physically page 0's — or co-locate two pages' privates
            // in one container that qpdf then routes to part8 (shared). Neither
            // belongs to this page's section, so exclude every container that is
            // not owned by a single non-first page.
            let non_page_owned = non_page_owned_containers(plan, member_to_container);
            for (i, count) in object_counts.iter_mut().enumerate().skip(1) {
                if let Some(privates) = plan.per_page_private_objects.get(i) {
                    *count =
                        objstm_folded_count(privates.iter(), member_to_container, &non_page_owned);
                }
            }
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

        // Order each page's shared identifiers the way qpdf does: by the shared
        // object's number in qpdf's ObjGen-keyed `obj_user_to_objects`
        // (QPDF_linearization.cc:1388-1402). A plain object keeps its source
        // number; an ObjStm container is ranked by `container_shared_rank`, which
        // captures the container's pre-renumber object-number order per mode:
        //   * Generate — containers are fresh `makeIndirectObject` objects
        //     allocated AFTER every source object in even-split order, so all
        //     plain shared objects sort before all containers (the `(0, ..)` vs
        //     `(1, ..)` split), containers by even-split rank.
        //   * Preserve — containers reuse the source ObjStm objects (their source
        //     numbers), so the rank is ascending source-container order.
        // Without this the identifiers come out in shared-table-index
        // (physical-number) order, which differs when a page references two
        // containers whose pre-renumber order differs from their physical order.
        let shared_sort_key = |shared_idx: u32| -> (u8, u32) {
            let entry = &shared_hints[shared_idx as usize];
            if entry.object_ref.generation == u16::MAX {
                (
                    1,
                    container_shared_rank
                        .get(&entry.object_ref.number)
                        .copied()
                        .unwrap_or(0),
                )
            } else {
                (0, entry.object_ref.number)
            }
        };
        for ids in &mut shared_ids_per_page {
            ids.sort_by_key(|&shared_idx| shared_sort_key(shared_idx));
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
        let table = PageOffsetHintTable::from_plan(
            &plan,
            &renumber,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
        );

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
        let table = PageOffsetHintTable::from_plan(
            &plan,
            &renumber,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
        );

        // Single page → delta = 0 → bits = 0
        assert_eq!(table.header.bits_object_count_delta, 0);
        assert_eq!(table.header.bits_shared_object_count, 0);
        assert_eq!(table.header.bits_shared_object_id, 0);
    }

    #[test]
    fn single_page_least_object_count() {
        let plan = single_page_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(
            &plan,
            &renumber,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
        );

        assert_eq!(table.header.least_object_count, 3);
    }

    #[test]
    fn single_page_entry_object_count_minus_least_is_zero() {
        let plan = single_page_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(
            &plan,
            &renumber,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
        );

        assert_eq!(table.entries[0].object_count_minus_least, 0);
    }

    #[test]
    fn single_page_placeholder_fields_are_zero() {
        let plan = single_page_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(
            &plan,
            &renumber,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
        );

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
        let table = PageOffsetHintTable::from_plan(
            &plan,
            &renumber,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
        );

        assert_eq!(
            table.header.denominator, 4,
            "denominator must be 4 (qpdf default)"
        );
    }

    #[test]
    fn single_page_no_shared_objects() {
        let plan = single_page_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(
            &plan,
            &renumber,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
        );

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
        let table = PageOffsetHintTable::from_plan(
            &plan,
            &renumber,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
        );

        assert_eq!(table.entries.len(), 2, "two-page plan must have 2 entries");
    }

    #[test]
    fn two_page_least_object_count() {
        let plan = two_page_plan_with_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(
            &plan,
            &renumber,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
        );

        // min(4, 5) = 4 (page 0 object_count = Part-2 + Part-3 = 2+2 = 4)
        assert_eq!(table.header.least_object_count, 4);
    }

    #[test]
    fn two_page_bits_object_count_delta() {
        let plan = two_page_plan_with_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(
            &plan,
            &renumber,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
        );

        // delta = 5 - 4 = 1 → bits_needed(1) = 1
        assert_eq!(table.header.bits_object_count_delta, 1);
    }

    #[test]
    fn two_page_entry_object_count_minus_least() {
        let plan = two_page_plan_with_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(
            &plan,
            &renumber,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
        );

        // page 0: 4 - 4 = 0
        assert_eq!(table.entries[0].object_count_minus_least, 0);
        // page 1: 5 - 4 = 1
        assert_eq!(table.entries[1].object_count_minus_least, 1);
    }

    #[test]
    fn two_page_shared_object_count_per_page() {
        let plan = two_page_plan_with_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(
            &plan,
            &renumber,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
        );

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
        let table = PageOffsetHintTable::from_plan(
            &plan,
            &renumber,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
        );

        // greatest shared count = 2 → bits_needed(2) = 2
        assert_eq!(table.header.bits_shared_object_count, 2);
    }

    #[test]
    fn two_page_bits_shared_object_id() {
        let plan = two_page_plan_with_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(
            &plan,
            &renumber,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
        );

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
        let table = PageOffsetHintTable::from_plan(
            &plan,
            &renumber,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
        );

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
        let table = PageOffsetHintTable::from_plan(
            &plan,
            &renumber,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
        );

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
        let table = PageOffsetHintTable::from_plan(
            &plan,
            &renumber,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
        );

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
        let table = PageOffsetHintTable::from_plan(
            &plan,
            &renumber,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
        );

        assert_eq!(table.header.denominator, 4);
    }

    #[test]
    fn two_page_placeholder_fields_are_zero() {
        let plan = two_page_plan_with_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let table = PageOffsetHintTable::from_plan(
            &plan,
            &renumber,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
        );

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
        let table = PageOffsetHintTable::from_plan(
            &plan,
            &renumber,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
        );
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
        let table = PageOffsetHintTable::from_plan(
            &plan,
            &renumber,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
        );
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
        assert_eq!(
            page0_object_count_with_objstm(&plan, &m2c, &Default::default(), &Default::default()),
            4
        );
    }

    /// When outline objects are routed to the first-page section (UseOutlines),
    /// page0_object_count must include the outline ObjStm container once.
    #[test]
    fn page0_count_includes_outline_container_when_first_page() {
        let outline_ref = ObjectRef::new(10, 0);
        let container_num = 99u32;
        let plan = LinearizationPlan {
            part2_objects: vec![ObjectRef::new(5, 0)],
            outline_first_page_members: vec![outline_ref],
            ..Default::default()
        };
        // outline_ref lives in container 99.
        let mut m2c: std::collections::BTreeMap<ObjectRef, (u32, u32)> =
            std::collections::BTreeMap::new();
        m2c.insert(outline_ref, (container_num, 0));

        // |part2| (1 plain) + |outline container| (1) = 2.
        assert_eq!(
            page0_object_count_with_objstm(&plan, &m2c, &Default::default(), &Default::default()),
            2,
            "outline container must be counted when outline_first_page_members is non-empty"
        );
    }
}
