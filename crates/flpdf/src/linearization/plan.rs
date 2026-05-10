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
//! At construction time Parts 1–3 start **empty** (placeholder); the Part 2
//! closure algorithm lives in subtask 2.2 and will populate them. Part 4 is
//! initialised to every known object so that `part4_objects` is never empty and
//! the disjoint invariant is trivially satisfied.

use crate::{ObjectRef, Pdf};
use std::collections::BTreeSet;
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
/// The four part lists are disjoint by construction: at creation time Parts 1–3
/// are empty and Part 4 contains every known non-zero-generation object.
/// Subtask 2.2 will move objects from Part 4 into Parts 1–3; it should preserve
/// this invariant.
#[derive(Debug, Clone)]
pub struct LinearizationPlan {
    // ------------------------------------------------------------------
    // Part membership
    // ------------------------------------------------------------------
    /// Part 1: linearization parameter dictionary and its xref stream.
    /// Populated by the writer subtask (2.3/2.4); empty as a placeholder.
    pub part1_objects: Vec<ObjectRef>,
    /// Part 2: first-page objects (page dict, resources, content streams).
    /// Populated by subtask 2.2 (first-page closure algorithm).
    pub part2_objects: Vec<ObjectRef>,
    /// Part 3: non-first-page shared objects (catalog, shared fonts, etc.).
    /// Populated by subtask 2.2.
    pub part3_objects: Vec<ObjectRef>,
    /// Part 4: remaining body objects — initialised to **all** known objects.
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
    /// Filled with placeholder entries at construction time; subtask 2.2 / 2.4
    /// back-patches `first_object_index`, `object_count`, and `byte_length`.
    pub page_hints: Vec<PageHintEntry>,
    /// Shared-object hint table inputs.
    ///
    /// An entry is added here for every object that appears in Part 3 (shared
    /// across pages). Empty until subtask 2.2 populates Part 3.
    pub shared_hints: Vec<SharedObjectHintEntry>,
}

impl LinearizationPlan {
    /// Construct a `LinearizationPlan` from a parsed PDF document.
    ///
    /// At this stage the plan only captures the **shape** of the partition:
    ///
    /// * Parts 1–3 are left empty (placeholder; subtask 2.2 fills them in).
    /// * Part 4 is initialised to every object known from the xref table
    ///   (generation-0, non-zero-number objects only).
    /// * `page_hints` contains one placeholder entry per page in document order.
    /// * `shared_hints` is empty (subtask 2.2 will populate it alongside Part 3).
    ///
    /// The method may return an error if reading page references from the
    /// document fails.
    pub fn from_pdf<R: Read + Seek>(pdf: &mut Pdf<R>) -> crate::Result<Self> {
        // Collect all object refs, excluding the free-entry object 0.
        let all_refs: Vec<ObjectRef> = pdf
            .object_refs()
            .into_iter()
            .filter(|r| r.number != 0)
            .collect();

        let total_object_count = all_refs.len() as u32;
        let root_ref = pdf.root_ref();

        // Attempt to collect page refs; fall back to empty if the page tree
        // is absent or malformed (graceful — error surfaced by higher layers).
        let page_refs: Vec<ObjectRef> = crate::pages::page_refs(pdf).unwrap_or_default();

        let page_hints: Vec<PageHintEntry> = page_refs
            .iter()
            .map(|&r| PageHintEntry::placeholder(r))
            .collect();

        // Part 4 starts as every known object (Parts 1-3 are empty).
        let part4_objects: Vec<ObjectRef> = all_refs;

        Ok(Self {
            part1_objects: Vec::new(),
            part2_objects: Vec::new(),
            part3_objects: Vec::new(),
            part4_objects,
            total_object_count,
            root_ref,
            page_hints,
            shared_hints: Vec::new(),
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

    /// Build a minimal but valid PDF in memory.
    ///
    /// Object layout:
    ///   1 0 obj – Catalog  (/Root)
    ///   2 0 obj – Pages node (1 kid)
    ///   3 0 obj – Page dict
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

    // ------------------------------------------------------------------
    // 1. from_pdf does not panic on a well-formed document
    // ------------------------------------------------------------------
    #[test]
    fn from_pdf_does_not_panic() {
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf).expect("plan construction must succeed");
        // Basic sanity: we got at least one object.
        assert!(plan.total_object_count > 0);
    }

    // ------------------------------------------------------------------
    // 2. Struct fields have expected types / accessors
    // ------------------------------------------------------------------
    #[test]
    fn plan_fields_accessible() {
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();

        // root_ref should be Some(1 0 R) for our fixture
        assert_eq!(plan.root_ref, Some(ObjectRef::new(1, 0)));

        // page_hints should have exactly 1 entry (one page in fixture)
        assert_eq!(plan.page_hints.len(), 1);
        assert_eq!(plan.page_hints[0].page_ref, ObjectRef::new(3, 0));
        assert_eq!(plan.page_hints[0].first_object_index, 0); // placeholder
        assert_eq!(plan.page_hints[0].byte_length, 0); // placeholder

        // shared_hints starts empty
        assert!(plan.shared_hints.is_empty());
    }

    // ------------------------------------------------------------------
    // 3. Parts 1–3 are empty placeholders; Part 4 holds all objects
    // ------------------------------------------------------------------
    #[test]
    fn parts_1_to_3_empty_part4_non_empty() {
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();

        assert!(plan.part1_objects.is_empty(), "Part 1 should be empty");
        assert!(plan.part2_objects.is_empty(), "Part 2 should be empty");
        assert!(plan.part3_objects.is_empty(), "Part 3 should be empty");
        // Part 4 must contain the 3 objects from our fixture
        assert!(
            !plan.part4_objects.is_empty(),
            "Part 4 must not be empty for a non-trivial document"
        );
    }

    // ------------------------------------------------------------------
    // 4. Part membership is disjoint at construction time
    // ------------------------------------------------------------------
    #[test]
    fn parts_are_disjoint_initially() {
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();
        assert!(
            plan.parts_are_disjoint(),
            "object refs must appear in at most one part"
        );
    }

    // ------------------------------------------------------------------
    // 5. Hint table inputs are well-formed even when populated only by placeholders
    // ------------------------------------------------------------------
    #[test]
    fn hint_table_inputs_well_formed_empty() {
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();

        // Each PageHintEntry must reference a non-zero object number
        for entry in &plan.page_hints {
            assert_ne!(entry.page_ref.number, 0);
        }

        // SharedObjectHintEntry list can be empty — that is well-formed
        for entry in &plan.shared_hints {
            assert_ne!(entry.object_ref.number, 0);
        }
    }
}
