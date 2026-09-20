//! Page-selection plan for a single document.
//!
//! qpdf correspondence: QPDFJob.cc handlePageSpecs single-document selection planning.
//!
//! Given a [`Pdf`] and a [`PageRange`], [`PagePlan::build`] resolves the range
//! to a concrete, ordered list of [`SelectedPage`] entries — one entry per page
//! that should be included in the output.
//!
//! This module is the foundation shared by the extraction, split, rotate, and
//! page-tree-rebuild layers. It does **not** perform
//! any rewriting; it only computes what pages are selected and in which order.
//!
//! The plan is consumed by the internal Job page-selection, split, and
//! page-tree-rebuild routes. Public callers should use the corresponding
//! `QPDFJobConfig` page-spec methods or higher-level page operations.

use crate::pages::page_refs;
use crate::{Error, ObjectRef, PageRange, Pdf, Result};
use std::io::{Read, Seek};

// ---------------------------------------------------------------------------
// Job-internal types
// ---------------------------------------------------------------------------

/// A single page in a selection plan.
///
/// `index_1based` is the 1-based position in the **source document** (i.e.
/// the original page number), not the position within the selection. The
/// pair `(index_1based, page_ref)` carries enough information for all
/// downstream layers:
///
/// - `page_ref` is used by page-tree rebuild and content extraction.
/// - `index_1based` is used by split-pages for output file naming and
///   by the rotate layer for per-page diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SelectedPage {
    /// 1-based page number in the source document.
    pub(crate) index_1based: u32,
    /// Indirect reference to the `/Page` object.
    pub(crate) page_ref: ObjectRef,
}

/// An ordered selection of pages from a single document.
///
/// Constructed via [`PagePlan::build`] from a [`PageRange`]. The ordering is
/// deterministic: it reflects the input
/// expression order, including duplicates preserved by [`PageRange::resolve`].
///
/// The selected page refs it produces feed directly into
/// [`rebuild_page_tree`](crate::pages::tree_rebuild::rebuild_page_tree), which
/// rewrites the document's `/Pages` tree to contain exactly those pages. For an
/// end-to-end extraction walkthrough see the runnable `examples/extract_pages.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PagePlan {
    /// Number of pages in the source document.
    pub(crate) page_count: u32,
    /// The selected pages, in selection order.
    pages: Vec<SelectedPage>,
}

impl PagePlan {
    /// Build a plan from a [`PageRange`] expression.
    ///
    /// Enumerates the document's pages via [`crate::pages::page_refs`], resolves the
    /// range against the page count, and maps each resolved 1-based number to
    /// the corresponding [`ObjectRef`].
    ///
    /// Use [`PageRange::all`] to select all pages in document order.
    ///
    /// # Errors
    ///
    /// - [`Error::Missing`] when the document has no pages (empty `/Pages` tree).
    /// - Any qpdf-compatible numeric-range error returned by
    ///   [`PageRange::resolve`] when a page number or `rN` endpoint is out of
    ///   range.
    /// - Any I/O or structural error from resolving the page tree.
    pub(crate) fn build<R: Read + Seek>(pdf: &mut Pdf<R>, range: &PageRange) -> Result<Self> {
        let all_refs = page_refs(pdf)?;
        let page_count = u32::try_from(all_refs.len()).map_err(|_| {
            Error::Unsupported(format!(
                "document has {} pages, which exceeds the maximum supported page count",
                all_refs.len()
            ))
        })?;

        if page_count == 0 {
            // qpdf-deviation: qpdf 11.9.0 does not guard this case in
            // QUtil::parse_numrange's caller, so an empty page vector reaches
            // std::vector::at and leaks libstdc++'s
            // "vector::_M_range_check: __n (which is 0) >= this->size()
            // (which is 0)" -- a toolchain implementation detail with no
            // qpdf semantic contract to reproduce in Rust (same reasoning as
            // job/argv.rs::job_json_file_open_error's libstdc++ leak note).
            // Surface flpdf's own actionable diagnostic instead.
            return Err(Error::Missing("document has no pages"));
        }

        let indices = range.resolve(page_count)?;
        let pages = indices
            .into_iter()
            .map(|n| {
                // `n` is guaranteed in-bounds by PageRange::resolve.
                SelectedPage {
                    index_1based: n,
                    page_ref: all_refs[(n - 1) as usize],
                }
            })
            .collect();

        Ok(Self { page_count, pages })
    }

    /// Build a plan from a pre-resolved slice of 1-based page numbers.
    ///
    /// This entry point is intended for callers that have already resolved a
    /// range (e.g., the multi-input combiner) or that need to
    /// specify exact page indices programmatically.
    ///
    /// Validation rules:
    /// - Every index must be ≥ 1 and ≤ the document's page count.
    /// - Out-of-range indices produce [`Error::Unsupported`] with an
    ///   actionable message.
    /// - Duplicates are **not** removed (the caller is expected to deduplicate
    ///   if that is the desired behaviour; [`PageRange::resolve`] preserves
    ///   duplicates according to qpdf, so callers needing a set must
    ///   deduplicate explicitly.
    ///
    /// An empty slice selects **all** pages in document order.
    ///
    /// # Errors
    ///
    /// - [`Error::Missing`] when the document has no pages.
    /// - [`Error::Unsupported`] when any index is 0 or exceeds the page count.
    /// - Any I/O or structural error from [`page_refs`].
    #[cfg(test)]
    pub(crate) fn from_1based_indices<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        indices: &[u32],
    ) -> Result<Self> {
        let all_refs = page_refs(pdf)?;
        let page_count = u32::try_from(all_refs.len()).map_err(|_| {
            Error::Unsupported(format!(
                "document has {} pages, which exceeds the maximum supported page count",
                all_refs.len()
            ))
        })?;

        if page_count == 0 {
            return Err(Error::Missing("document has no pages"));
        }

        if indices.is_empty() {
            // This pre-resolved API intentionally treats an empty slice as
            // all pages; PageRange::parse_numrange("") itself is qpdf's empty result.
            let pages = (1u32..=page_count)
                .map(|n| SelectedPage {
                    index_1based: n,
                    page_ref: all_refs[(n - 1) as usize],
                })
                .collect();
            return Ok(Self { page_count, pages });
        }

        let mut pages = Vec::with_capacity(indices.len());
        for &n in indices {
            if n == 0 || n > page_count {
                return Err(Error::Unsupported(format!(
                    "page index {n} is out of range (document has {page_count} page(s)); \
                     page numbers are 1-based and must not exceed the page count"
                )));
            }
            pages.push(SelectedPage {
                index_1based: n,
                page_ref: all_refs[(n - 1) as usize],
            });
        }

        Ok(Self { page_count, pages })
    }

    /// The selected pages in selection order.
    pub(crate) fn pages(&self) -> &[SelectedPage] {
        &self.pages
    }

    /// Number of pages in the source document (not the selection size).
    #[cfg(test)]
    pub(crate) fn source_page_count(&self) -> u32 {
        self.page_count
    }

    /// Number of pages in this selection.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.pages.len()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Pdf;
    use std::io::Cursor;

    // -----------------------------------------------------------------------
    // Multi-page PDF builder
    // -----------------------------------------------------------------------

    /// Build a minimal N-page PDF.
    ///
    /// Object layout:
    ///   1 0 R  Catalog
    ///   2 0 R  Pages (root, Kids = [3 0 R, 4 0 R, ...])
    ///   3 0 R  Page 1
    ///   4 0 R  Page 2
    ///   …
    ///   (2 + N) 0 R  Page N
    fn build_n_page_pdf(n: u32) -> Vec<u8> {
        assert!(n >= 1, "need at least 1 page");

        let mut pdf: Vec<u8> = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        // Catalog (object 1)
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        // Pages root (object 2)
        let off2 = pdf.len() as u64;
        let kids: String = (3u32..=2 + n).map(|i| format!("{i} 0 R ")).collect();
        let pages_obj = format!("2 0 obj\n<< /Type /Pages /Kids [{kids}] /Count {n} >>\nendobj\n");
        pdf.extend_from_slice(pages_obj.as_bytes());

        // Individual Page objects (objects 3 … 2+N)
        let mut page_offsets: Vec<u64> = Vec::new();
        for i in 3u32..=2 + n {
            let off = pdf.len() as u64;
            page_offsets.push(off);
            let page_obj = format!(
                "{i} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n"
            );
            pdf.extend_from_slice(page_obj.as_bytes());
        }

        // xref table
        let xref_start = pdf.len() as u64;
        let total = 2 + n as usize + 1; // 0..=(2+N)
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

    // -----------------------------------------------------------------------
    // build() — normal cases
    // -----------------------------------------------------------------------

    #[test]
    fn all_range_selects_all_pages() {
        let mut pdf = open(build_n_page_pdf(5));
        let range = PageRange::all();
        let plan = PagePlan::build(&mut pdf, &range).unwrap();

        assert_eq!(plan.source_page_count(), 5);
        assert_eq!(plan.len(), 5);
        for (i, entry) in plan.pages().iter().enumerate() {
            assert_eq!(entry.index_1based, (i + 1) as u32);
            // Pages are 1-indexed: object 3 = page 1, 4 = page 2, …
            assert_eq!(entry.page_ref, ObjectRef::new((i + 3) as u32, 0));
        }
    }

    #[test]
    fn explicit_range_selects_subset_in_order() {
        let mut pdf = open(build_n_page_pdf(5));
        let range = PageRange::parse_numrange("1,3,5").unwrap();
        let plan = PagePlan::build(&mut pdf, &range).unwrap();

        assert_eq!(plan.len(), 3);
        assert_eq!(
            plan.pages()[0],
            SelectedPage {
                index_1based: 1,
                page_ref: ObjectRef::new(3, 0)
            }
        );
        assert_eq!(
            plan.pages()[1],
            SelectedPage {
                index_1based: 3,
                page_ref: ObjectRef::new(5, 0)
            }
        );
        assert_eq!(
            plan.pages()[2],
            SelectedPage {
                index_1based: 5,
                page_ref: ObjectRef::new(7, 0)
            }
        );
    }

    #[test]
    fn descending_range_preserves_order() {
        let mut pdf = open(build_n_page_pdf(5));
        let range = PageRange::parse_numrange("5-1").unwrap();
        let plan = PagePlan::build(&mut pdf, &range).unwrap();

        assert_eq!(plan.len(), 5);
        // Reversed: index 5, 4, 3, 2, 1
        let indices: Vec<u32> = plan.pages().iter().map(|p| p.index_1based).collect();
        assert_eq!(indices, vec![5, 4, 3, 2, 1]);
    }

    #[test]
    fn duplicates_preserved() {
        // "1,3,1" → [1, 3, 1] (qpdf-parity: PageRange::resolve preserves
        // duplicates). Verified against qpdf 11.9.0 with
        // `qpdf --pages in 1,3,1 --` which emits 3 pages.
        let mut pdf = open(build_n_page_pdf(5));
        let range = PageRange::parse_numrange("1,3,1").unwrap();
        let plan = PagePlan::build(&mut pdf, &range).unwrap();

        assert_eq!(plan.len(), 3);
        assert_eq!(plan.pages()[0].index_1based, 1);
        assert_eq!(plan.pages()[1].index_1based, 3);
        assert_eq!(plan.pages()[2].index_1based, 1);
        // Same page_ref for both occurrences of page 1: downstream writer
        // (rebuild_page_tree / extract_pages) shallow-clones the leaf so each
        // slot gets its own /Parent, matching qpdf's duplicate-page output.
        assert_eq!(plan.pages()[0].page_ref, plan.pages()[2].page_ref);
    }

    #[test]
    fn determinism_same_plan_twice() {
        let bytes = build_n_page_pdf(5);
        let range = PageRange::parse_numrange("2,4,1").unwrap();

        let mut pdf1 = open(bytes.clone());
        let plan1 = PagePlan::build(&mut pdf1, &range).unwrap();

        let mut pdf2 = open(bytes);
        let plan2 = PagePlan::build(&mut pdf2, &range).unwrap();

        assert_eq!(plan1, plan2);
    }

    // -----------------------------------------------------------------------
    // build() — error cases
    // -----------------------------------------------------------------------

    #[test]
    fn out_of_range_page_number_is_error() {
        let mut pdf = open(build_n_page_pdf(5));
        let range = PageRange::parse_numrange("10").unwrap(); // 10 > 5
        let err = PagePlan::build(&mut pdf, &range).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("out of range"),
            "expected 'out of range' in error message, got: {msg}"
        );
    }

    #[test]
    fn out_of_range_from_end_is_error() {
        let mut pdf = open(build_n_page_pdf(3));
        let range = PageRange::parse_numrange("r10").unwrap(); // r10 on 3-page doc
        let err = PagePlan::build(&mut pdf, &range).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("out of range"),
            "expected 'out of range' in error message, got: {msg}"
        );
    }

    #[test]
    fn single_page_document() {
        let mut pdf = open(build_n_page_pdf(1));
        let range = PageRange::parse_numrange("z").unwrap(); // last = page 1
        let plan = PagePlan::build(&mut pdf, &range).unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan.pages()[0].index_1based, 1);
    }

    // -----------------------------------------------------------------------
    // from_1based_indices() — normal cases
    // -----------------------------------------------------------------------

    #[test]
    fn from_indices_empty_selects_all() {
        let mut pdf = open(build_n_page_pdf(5));
        let plan = PagePlan::from_1based_indices(&mut pdf, &[]).unwrap();
        assert_eq!(plan.len(), 5);
        let indices: Vec<u32> = plan.pages().iter().map(|p| p.index_1based).collect();
        assert_eq!(indices, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn from_indices_specific_pages() {
        let mut pdf = open(build_n_page_pdf(5));
        let plan = PagePlan::from_1based_indices(&mut pdf, &[2, 4]).unwrap();
        assert_eq!(plan.len(), 2);
        assert_eq!(plan.pages()[0].index_1based, 2);
        assert_eq!(plan.pages()[1].index_1based, 4);
    }

    // -----------------------------------------------------------------------
    // from_1based_indices() — error cases
    // -----------------------------------------------------------------------

    #[test]
    fn from_indices_zero_is_error() {
        let mut pdf = open(build_n_page_pdf(5));
        let err = PagePlan::from_1based_indices(&mut pdf, &[0]).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("out of range"),
            "expected 'out of range' in error message, got: {msg}"
        );
    }

    #[test]
    fn from_indices_exceeds_page_count_is_error() {
        let mut pdf = open(build_n_page_pdf(3));
        let err = PagePlan::from_1based_indices(&mut pdf, &[4]).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("out of range"),
            "expected 'out of range' in error message, got: {msg}"
        );
    }
}
