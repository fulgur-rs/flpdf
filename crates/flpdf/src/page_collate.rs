//! Round-robin page interleaving (qpdf `--collate[=N]`).
//!
//! [`collate`] takes the per-input page plans from a [`CombinedPlan`] and
//! reorders them N-at-a-time round-robin instead of concatenating them in
//! input order.
//!
//! # qpdf semantics (verified against qpdf 11.9.0)
//!
//! - `--collate` (no argument) is equivalent to `--collate=1`.
//! - With N inputs and chunk size K, each round takes up to K pages from each
//!   input in order: input0[0..K], input1[0..K], …, inputN-1[0..K].
//!   Then the next round takes the next K pages, etc.
//! - When an input is exhausted it is silently skipped; the remaining inputs
//!   continue without padding (no empty-page insertion).
//! - A single input produces the same order as concatenation.
//!
//! # Observed page orders (qpdf 11.9.0, /usr/bin/qpdf)
//!
//! ```text
//! N=1, A=[A1,A2,A3], B=[B1,B2,B3,B4,B5]
//!   → A1,B1,A2,B2,A3,B3,B4,B5
//!
//! N=2, A=[A1,A2,A3], B=[B1,B2,B3,B4,B5]
//!   → A1,A2,B1,B2,A3,B3,B4,B5
//!
//! N=3, A=[A1,A2,A3], B=[B1,B2,B3,B4,B5]   (N == len(A))
//!   → A1,A2,A3,B1,B2,B3,B4,B5
//!
//! N=10, A=[A1,A2,A3], B=[B1,B2,B3,B4,B5]  (N > all input lengths)
//!   → A1,A2,A3,B1,B2,B3,B4,B5
//!
//! N=1, A=[A1,A2], B=[B1,B2] (even length)
//!   → A1,B1,A2,B2
//!
//! N=2, A=[A1,A2], B=[B1,B2] (even length)
//!   → A1,A2,B1,B2
//!
//! N=1, 3 inputs A=[A1,A2,A3], B=[B1,B2,B3,B4,B5], C=[C1,C2]
//!   → A1,B1,C1,A2,B2,C2,A3,B3,B4,B5
//!
//! N=2, 3 inputs A=[A1,A2,A3], B=[B1,B2,B3,B4,B5], C=[C1,C2]
//!   → A1,A2,B1,B2,C1,C2,A3,B3,B4,B5
//! ```

use crate::{page_combine::CombinedPage, CombinedPlan, Error, Result};

/// Reorder pages from a [`CombinedPlan`] using round-robin interleaving.
///
/// Takes up to `n` pages from each input in sequence (input 0, input 1, …)
/// per round, repeating until all inputs are exhausted. Inputs that run out
/// of pages are silently skipped in subsequent rounds.
///
/// # Arguments
///
/// * `plan` — the combined page plan produced by [`CombinedPlan::build`] or
///   [`CombinedPlan::from_specs`].
/// * `n` — the chunk size. Pass `1` for the default `--collate` behaviour.
///   Must be ≥ 1.
///
/// # Errors
///
/// Returns [`Error::Unsupported`] when `n` is 0, since a chunk size of zero
/// would produce an infinite loop without consuming any pages.
///
/// # Example
///
/// ```no_run
/// use flpdf::{page_combine::CombinedPlan, page_collate::collate, page_range::PageRange, Pdf};
/// use std::io::{BufReader, Cursor};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// // (build plan from real PDFs here)
/// # let bytes_a: Vec<u8> = vec![];
/// # let bytes_b: Vec<u8> = vec![];
/// # let mut pdf_a = Pdf::open(Cursor::new(bytes_a))?;
/// # let mut pdf_b = Pdf::open(Cursor::new(bytes_b))?;
/// let plan = CombinedPlan::build(vec![
///     (&mut pdf_a, PageRange::parse("")?),
///     (&mut pdf_b, PageRange::parse("")?),
/// ])?;
/// let pages = collate(&plan, 1)?;
/// for p in &pages {
///     println!("source {} page {}", p.source_index, p.page.index_1based);
/// }
/// # Ok(())
/// # }
/// ```
pub fn collate(plan: &CombinedPlan, n: usize) -> Result<Vec<CombinedPage>> {
    if n == 0 {
        return Err(Error::Unsupported(
            "--collate chunk size must be >= 1; got 0".into(),
        ));
    }

    // Build per-input cursor vectors: each entry is (source_index, pages_slice_index).
    let inputs: Vec<(usize, &[crate::page_plan::SelectedPage])> = plan
        .per_input_plans()
        .iter()
        .enumerate()
        .map(|(src, pp)| (src, pp.pages()))
        .collect();

    let total = plan.total_page_count();
    let mut result = Vec::with_capacity(total);

    // Offset into each input's page list.
    let mut cursors: Vec<usize> = vec![0; inputs.len()];

    loop {
        let mut any_emitted = false;

        for (input_idx, &(source_index, pages)) in inputs.iter().enumerate() {
            let cursor = cursors[input_idx];
            if cursor >= pages.len() {
                // This input is exhausted; skip it.
                continue;
            }

            let end = (cursor + n).min(pages.len());
            for page in &pages[cursor..end] {
                result.push(CombinedPage {
                    source_index,
                    page: page.clone(),
                });
            }
            cursors[input_idx] = end;
            any_emitted = true;
        }

        if !any_emitted {
            // All inputs exhausted.
            break;
        }
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page_combine::{CombinedPage, CombinedPlan};
    use crate::page_plan::SelectedPage;
    use crate::page_range::PageRange;
    use crate::reader::Pdf;
    use crate::ObjectRef;
    use std::io::Cursor;

    // -----------------------------------------------------------------------
    // In-memory PDF builder (matches page_combine::tests layout)
    //
    // Object layout: 1=Catalog, 2=Pages, 3..=(2+N)=Page objects.
    // Page i (1-based) → ObjectRef { id: 2+i, gen: 0 }.
    // -----------------------------------------------------------------------

    fn build_n_page_pdf(n: u32) -> Vec<u8> {
        assert!(n >= 1);
        let mut pdf: Vec<u8> = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        let kids: String = (3u32..=2 + n).map(|i| format!("{i} 0 R ")).collect();
        let pages_obj = format!("2 0 obj\n<< /Type /Pages /Kids [{kids}] /Count {n} >>\nendobj\n");
        pdf.extend_from_slice(pages_obj.as_bytes());

        let mut page_offsets: Vec<u64> = Vec::new();
        for i in 3u32..=2 + n {
            let off = pdf.len() as u64;
            page_offsets.push(off);
            let page_obj = format!(
                "{i} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n"
            );
            pdf.extend_from_slice(page_obj.as_bytes());
        }

        let xref_start = pdf.len() as u64;
        let total = 2 + n as usize + 1;
        let mut xref = format!("xref\n0 {total}\n0000000000 65535 f \n");
        xref.push_str(&format!("{:010} 00000 n \n", off1));
        xref.push_str(&format!("{:010} 00000 n \n", off2));
        for off in &page_offsets {
            xref.push_str(&format!("{:010} 00000 n \n", off));
        }
        pdf.extend_from_slice(xref.as_bytes());

        let trailer =
            format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    fn open(bytes: Vec<u8>) -> Pdf<Cursor<Vec<u8>>> {
        Pdf::open(Cursor::new(bytes)).expect("PDF should parse")
    }

    fn combined_plan(a_pages: u32, a_range: &str, b_pages: u32, b_range: &str) -> CombinedPlan {
        let mut pdf_a = open(build_n_page_pdf(a_pages));
        let mut pdf_b = open(build_n_page_pdf(b_pages));
        let ra = PageRange::parse(a_range).unwrap();
        let rb = PageRange::parse(b_range).unwrap();
        CombinedPlan::build(vec![(&mut pdf_a, ra), (&mut pdf_b, rb)]).unwrap()
    }

    fn page_ref_at(source_pages: u32, index_1based: u32) -> ObjectRef {
        // In build_n_page_pdf, page i (1-based) is object (2+i).
        let _ = source_pages; // page_count not needed for ref calc in this layout
        ObjectRef::new(2 + index_1based, 0)
    }

    fn cp(source_index: usize, index_1based: u32) -> CombinedPage {
        CombinedPage {
            source_index,
            page: SelectedPage {
                index_1based,
                page_ref: page_ref_at(0, index_1based),
            },
        }
    }

    // -----------------------------------------------------------------------
    // Acceptance test 1: two even-length inputs interleave 1-1-1-1
    //
    // Observed with qpdf 11.9.0:
    //   N=1, A=[A1,A2], B=[B1,B2]  → A1,B1,A2,B2
    // -----------------------------------------------------------------------
    #[test]
    fn even_length_two_inputs_n1_interleaves() {
        let plan = combined_plan(2, "", 2, "");
        let pages = collate(&plan, 1).unwrap();

        assert_eq!(pages.len(), 4);
        // source_index pattern: 0,1,0,1
        let sources: Vec<usize> = pages.iter().map(|p| p.source_index).collect();
        assert_eq!(sources, vec![0, 1, 0, 1]);
        // page index pattern: A1,B1,A2,B2
        let indices: Vec<u32> = pages.iter().map(|p| p.page.index_1based).collect();
        assert_eq!(indices, vec![1, 1, 2, 2]);
    }

    // -----------------------------------------------------------------------
    // Acceptance test 2: uneven inputs — shorter exhausted, longer continues
    //
    // Observed with qpdf 11.9.0:
    //   N=1, A=[A1,A2,A3], B=[B1,B2,B3,B4,B5]
    //   → A1,B1,A2,B2,A3,B3,B4,B5
    // -----------------------------------------------------------------------
    #[test]
    fn uneven_inputs_n1_shorter_exhausted_then_longer_continues() {
        let plan = combined_plan(3, "", 5, "");
        let pages = collate(&plan, 1).unwrap();

        assert_eq!(pages.len(), 8);
        let sources: Vec<usize> = pages.iter().map(|p| p.source_index).collect();
        // A1,B1,A2,B2,A3,B3 → then B4,B5 from B only
        assert_eq!(sources, vec![0, 1, 0, 1, 0, 1, 1, 1]);
        let indices: Vec<u32> = pages.iter().map(|p| p.page.index_1based).collect();
        assert_eq!(indices, vec![1, 1, 2, 2, 3, 3, 4, 5]);
    }

    // -----------------------------------------------------------------------
    // Acceptance test 3: --collate=2 yields 1,2 / 1,2 alternation
    //
    // Observed with qpdf 11.9.0:
    //   N=2, A=[A1,A2,A3], B=[B1,B2,B3,B4,B5]
    //   → A1,A2,B1,B2,A3,B3,B4,B5
    // -----------------------------------------------------------------------
    #[test]
    fn collate_n2_uneven_inputs() {
        let plan = combined_plan(3, "", 5, "");
        let pages = collate(&plan, 2).unwrap();

        assert_eq!(pages.len(), 8);
        let sources: Vec<usize> = pages.iter().map(|p| p.source_index).collect();
        // A1,A2 | B1,B2 | A3 | B3,B4,B5
        assert_eq!(sources, vec![0, 0, 1, 1, 0, 1, 1, 1]);
        let indices: Vec<u32> = pages.iter().map(|p| p.page.index_1based).collect();
        assert_eq!(indices, vec![1, 2, 1, 2, 3, 3, 4, 5]);
    }

    // -----------------------------------------------------------------------
    // N == len(A): A is exhausted in the first round
    //
    // Observed with qpdf 11.9.0:
    //   N=3, A=[A1,A2,A3], B=[B1,B2,B3,B4,B5]
    //   → A1,A2,A3,B1,B2,B3,B4,B5
    // -----------------------------------------------------------------------
    #[test]
    fn collate_n_equals_shorter_input_length() {
        let plan = combined_plan(3, "", 5, "");
        let pages = collate(&plan, 3).unwrap();

        assert_eq!(pages.len(), 8);
        let sources: Vec<usize> = pages.iter().map(|p| p.source_index).collect();
        assert_eq!(sources, vec![0, 0, 0, 1, 1, 1, 1, 1]);
        let indices: Vec<u32> = pages.iter().map(|p| p.page.index_1based).collect();
        assert_eq!(indices, vec![1, 2, 3, 1, 2, 3, 4, 5]);
    }

    // -----------------------------------------------------------------------
    // N > all input lengths: degenerates to concatenation
    //
    // Observed with qpdf 11.9.0:
    //   N=10, A=[A1,A2,A3], B=[B1,B2,B3,B4,B5]
    //   → A1,A2,A3,B1,B2,B3,B4,B5
    // -----------------------------------------------------------------------
    #[test]
    fn collate_n_exceeds_all_input_lengths_is_concat() {
        let plan = combined_plan(3, "", 5, "");
        let pages = collate(&plan, 100).unwrap();

        assert_eq!(pages.len(), 8);
        let sources: Vec<usize> = pages.iter().map(|p| p.source_index).collect();
        assert_eq!(sources, vec![0, 0, 0, 1, 1, 1, 1, 1]);
    }

    // -----------------------------------------------------------------------
    // Single input: result equals the flat page list (same as concatenation)
    //
    // Observed with qpdf 11.9.0:
    //   N=1, single input A=[A1,A2]  → A1,A2
    // -----------------------------------------------------------------------
    #[test]
    fn single_input_same_as_flat_pages() {
        let mut pdf = open(build_n_page_pdf(3));
        let range = PageRange::parse("").unwrap();
        let plan = CombinedPlan::build(vec![(&mut pdf, range)]).unwrap();

        let pages = collate(&plan, 1).unwrap();

        assert_eq!(pages.len(), 3);
        assert_eq!(pages, plan.flat_pages());
    }

    // -----------------------------------------------------------------------
    // Three inputs N=1: C exhausts first, then A, then B continues alone
    //
    // Observed with qpdf 11.9.0:
    //   N=1, A=[A1,A2,A3], B=[B1,B2,B3,B4,B5], C=[C1,C2]
    //   → A1,B1,C1,A2,B2,C2,A3,B3,B4,B5
    // -----------------------------------------------------------------------
    #[test]
    fn three_inputs_n1_shorter_exhausted_in_order() {
        let mut pdf_a = open(build_n_page_pdf(3));
        let mut pdf_b = open(build_n_page_pdf(5));
        let mut pdf_c = open(build_n_page_pdf(2));
        let plan = CombinedPlan::build(vec![
            (&mut pdf_a, PageRange::parse("").unwrap()),
            (&mut pdf_b, PageRange::parse("").unwrap()),
            (&mut pdf_c, PageRange::parse("").unwrap()),
        ])
        .unwrap();

        let pages = collate(&plan, 1).unwrap();

        assert_eq!(pages.len(), 10);
        let sources: Vec<usize> = pages.iter().map(|p| p.source_index).collect();
        // A1,B1,C1 | A2,B2,C2 | A3,B3 | B4 | B5
        assert_eq!(sources, vec![0, 1, 2, 0, 1, 2, 0, 1, 1, 1]);
        let indices: Vec<u32> = pages.iter().map(|p| p.page.index_1based).collect();
        assert_eq!(indices, vec![1, 1, 1, 2, 2, 2, 3, 3, 4, 5]);
    }

    // -----------------------------------------------------------------------
    // Three inputs N=2
    //
    // Observed with qpdf 11.9.0:
    //   N=2, A=[A1,A2,A3], B=[B1,B2,B3,B4,B5], C=[C1,C2]
    //   → A1,A2,B1,B2,C1,C2,A3,B3,B4,B5
    // -----------------------------------------------------------------------
    #[test]
    fn three_inputs_n2() {
        let mut pdf_a = open(build_n_page_pdf(3));
        let mut pdf_b = open(build_n_page_pdf(5));
        let mut pdf_c = open(build_n_page_pdf(2));
        let plan = CombinedPlan::build(vec![
            (&mut pdf_a, PageRange::parse("").unwrap()),
            (&mut pdf_b, PageRange::parse("").unwrap()),
            (&mut pdf_c, PageRange::parse("").unwrap()),
        ])
        .unwrap();

        let pages = collate(&plan, 2).unwrap();

        assert_eq!(pages.len(), 10);
        let sources: Vec<usize> = pages.iter().map(|p| p.source_index).collect();
        // A1,A2 | B1,B2 | C1,C2 | A3 | B3,B4,B5
        assert_eq!(sources, vec![0, 0, 1, 1, 2, 2, 0, 1, 1, 1]);
        let indices: Vec<u32> = pages.iter().map(|p| p.page.index_1based).collect();
        assert_eq!(indices, vec![1, 2, 1, 2, 1, 2, 3, 3, 4, 5]);
    }

    // -----------------------------------------------------------------------
    // N=0 is an error
    // -----------------------------------------------------------------------
    #[test]
    fn chunk_size_zero_is_error() {
        let plan = combined_plan(2, "", 2, "");
        let err = collate(&plan, 0).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("chunk size") || msg.contains("collate"),
            "expected actionable error message, got: {msg}"
        );
    }

    // -----------------------------------------------------------------------
    // Input with page subset (range != all pages)
    // -----------------------------------------------------------------------
    #[test]
    fn range_subset_pages_interleaved_correctly() {
        // A selects pages 1,3 from a 4-page doc; B selects all 2 pages.
        let mut pdf_a = open(build_n_page_pdf(4));
        let mut pdf_b = open(build_n_page_pdf(2));
        let plan = CombinedPlan::build(vec![
            (&mut pdf_a, PageRange::parse("1,3").unwrap()),
            (&mut pdf_b, PageRange::parse("").unwrap()),
        ])
        .unwrap();

        let pages = collate(&plan, 1).unwrap();

        // A has 2 selected pages, B has 2 selected pages → interleave 1-1-1-1
        assert_eq!(pages.len(), 4);
        let sources: Vec<usize> = pages.iter().map(|p| p.source_index).collect();
        assert_eq!(sources, vec![0, 1, 0, 1]);
        let indices: Vec<u32> = pages.iter().map(|p| p.page.index_1based).collect();
        // A selected 1,3; B selected 1,2
        assert_eq!(indices, vec![1, 1, 3, 2]);
    }

    // -----------------------------------------------------------------------
    // page_ref is preserved correctly
    // -----------------------------------------------------------------------
    #[test]
    fn page_refs_preserved_from_per_input_plans() {
        let plan = combined_plan(3, "", 2, "");
        let pages = collate(&plan, 1).unwrap();

        // Verify the first page from input 0 matches per_input_plans()[0].pages()[0]
        let expected_ref = plan.per_input_plans()[0].pages()[0].page_ref;
        assert_eq!(pages[0].page.page_ref, expected_ref);
        // Verify the first page from input 1 matches per_input_plans()[1].pages()[0]
        let expected_ref_b = plan.per_input_plans()[1].pages()[0].page_ref;
        assert_eq!(pages[1].page.page_ref, expected_ref_b);
    }

    // -----------------------------------------------------------------------
    // Empty page sequence within a multi-input plan (0-page selection via range)
    //
    // A degenerate input with 0 selected pages must be silently skipped.
    // (PageRange::parse("") on a non-empty doc selects all; we test with
    // a subset range that results in the same structural path through collate.)
    // Actually: an empty range selects all pages, so we test the 1-of-2 empty
    // scenario by constructing a CombinedPlan with a single-page doc for B
    // and then checking collate skips B gracefully when B is already exhausted
    // after round 1.
    // -----------------------------------------------------------------------
    #[test]
    fn one_input_longer_by_far_continues_without_padding() {
        // A = 1 page, B = 4 pages; N=1
        // Expected: A1, B1, B2, B3, B4
        let plan = combined_plan(1, "", 4, "");
        let pages = collate(&plan, 1).unwrap();

        assert_eq!(pages.len(), 5);
        let sources: Vec<usize> = pages.iter().map(|p| p.source_index).collect();
        assert_eq!(sources, vec![0, 1, 1, 1, 1]);
        let indices: Vec<u32> = pages.iter().map(|p| p.page.index_1based).collect();
        assert_eq!(indices, vec![1, 1, 2, 3, 4]);
    }

    // -----------------------------------------------------------------------
    // result pages total equals plan.total_page_count()
    // -----------------------------------------------------------------------
    #[test]
    fn collate_preserves_total_page_count() {
        let plan = combined_plan(3, "", 5, "");
        let pages_n1 = collate(&plan, 1).unwrap();
        let pages_n2 = collate(&plan, 2).unwrap();
        let pages_n3 = collate(&plan, 3).unwrap();

        assert_eq!(pages_n1.len(), plan.total_page_count());
        assert_eq!(pages_n2.len(), plan.total_page_count());
        assert_eq!(pages_n3.len(), plan.total_page_count());
    }

    // -----------------------------------------------------------------------
    // `cp` helper self-test: verify expected CombinedPage values
    // -----------------------------------------------------------------------
    #[test]
    fn cp_helper_values() {
        let p = cp(0, 1);
        assert_eq!(p.source_index, 0);
        assert_eq!(p.page.index_1based, 1);
        assert_eq!(p.page.page_ref, ObjectRef::new(3, 0)); // 2+1=3
    }
}
