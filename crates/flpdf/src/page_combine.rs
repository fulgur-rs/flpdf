//! Multi-input page list combiner.
//!
//! [`CombinedPlan`] opens multiple input documents (each with an optional
//! password), applies each input's [`PageRange`], and concatenates the
//! resulting [`PagePlan`]s in input order — matching qpdf's `--pages` semantics.
//!
//! # Ownership model
//!
//! The low-level entry point, [`CombinedPlan::build`], accepts a slice of
//! already-opened `(Pdf<R>, PageRange)` pairs so that:
//!
//! - Tests can use in-memory readers (as in `page_plan.rs`'s tests).
//! - No concrete `R` is fixed at the API boundary — callers supply whatever
//!   reader they have.
//!
//! The path-based convenience helper [`CombinedPlan::from_specs`] takes a
//! [`Vec<InputSpec>`], opens every file with the right password, and calls
//! [`CombinedPlan::build`] internally.
//!
//! # qpdf semantics
//!
//! - Concatenation order equals input order; within each input the range
//!   resolution order is preserved.
//! - The **first input is the base**: its catalog and Info dictionary are the
//!   ones that downstream rewrite layers must preserve. [`CombinedPlan::base_index`]
//!   always returns `0`; the field is exposed explicitly so callers are not
//!   relying on an implicit convention.
//!
//! # Example (low-level)
//!
//! ```no_run
//! use std::fs::File;
//! use std::io::BufReader;
//! use flpdf::{page_combine::CombinedPlan, page_range::PageRange, Pdf};
//!
//! let mut a = Pdf::open(BufReader::new(File::open("a.pdf")?))?;
//! let mut b = Pdf::open(BufReader::new(File::open("b.pdf")?))?;
//! let range_a = PageRange::parse("1-5")?;
//! let range_b = PageRange::parse("")?;
//!
//! let plan = CombinedPlan::build(vec![
//!     (&mut a, range_a),
//!     (&mut b, range_b),
//! ])?;
//!
//! println!("base input index: {}", plan.base_index()); // 0
//! for entry in plan.flat_pages() {
//!     println!("  input {}, page {}", entry.source_index, entry.page.index_1based);
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use crate::{Error, PagePlan, PageRange, Pdf, PdfOpenOptions, Result};
use std::io::{BufReader, Read, Seek};
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Specification for one input document to the combiner.
///
/// Used by the convenience path-based constructor [`CombinedPlan::from_specs`].
#[derive(Debug, Clone)]
pub struct InputSpec {
    /// Path to the PDF file on disk.
    pub path: PathBuf,
    /// Optional password to decrypt the file.
    /// The bytes are passed verbatim to [`PdfOpenOptions::password`].
    pub password: Option<String>,
    /// Which pages to include from this input.
    pub range: PageRange,
}

impl InputSpec {
    /// Convenience constructor.
    pub fn new(path: impl Into<PathBuf>, password: Option<String>, range: PageRange) -> Self {
        Self {
            path: path.into(),
            password,
            range,
        }
    }
}

/// An entry in the combined flat page list, carrying the per-input source index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CombinedPage {
    /// 0-based index into the original input slice (identifies which `Pdf` the
    /// page belongs to).
    pub source_index: usize,
    /// The selected page from that input.
    pub page: crate::page_plan::SelectedPage,
}

/// Concatenated page selection from multiple input documents.
///
/// Constructed via [`CombinedPlan::build`] (low-level, pre-opened readers) or
/// [`CombinedPlan::from_specs`] (high-level, path + password + range).
///
/// The plan stores per-input [`PagePlan`]s so that downstream layers such as
/// collation (8.6) and page-tree rebuild (8.8) can access them individually.
#[derive(Debug, Clone)]
pub struct CombinedPlan {
    /// Per-input plans, in input order.
    per_input: Vec<PagePlan>,
    /// Flat concatenated view: `(source_index, SelectedPage)` pairs.
    flat: Vec<CombinedPage>,
}

impl CombinedPlan {
    // -----------------------------------------------------------------------
    // Constructors
    // -----------------------------------------------------------------------

    /// Build a combined plan from pre-opened `(Pdf, PageRange)` pairs.
    ///
    /// Inputs are consumed in order; the resulting pages are concatenated in
    /// the same order. The first input is the base (index 0).
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] when `inputs` is empty.
    /// - Any error from [`PagePlan::build`] — notably range-out-of-bounds or
    ///   missing pages — attributed to the relevant input by index.
    pub fn build<R: Read + Seek>(inputs: Vec<(&mut Pdf<R>, PageRange)>) -> Result<Self> {
        if inputs.is_empty() {
            return Err(Error::Unsupported(
                "at least one input document is required; got an empty input list".into(),
            ));
        }

        let mut per_input = Vec::with_capacity(inputs.len());
        let mut flat = Vec::new();

        for (source_index, (pdf, range)) in inputs.into_iter().enumerate() {
            let plan = PagePlan::build(pdf, &range)
                .map_err(|e| Error::Unsupported(format!("input {source_index}: {e}")))?;

            for page in plan.pages() {
                flat.push(CombinedPage {
                    source_index,
                    page: page.clone(),
                });
            }

            per_input.push(plan);
        }

        Ok(Self { per_input, flat })
    }

    /// Build a combined plan from a list of [`InputSpec`]s.
    ///
    /// Each spec is opened with [`Pdf::open_with_options`] using the supplied
    /// password (if any). Bad passwords produce [`Error::Encrypted`] with an
    /// actionable message.
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] when `specs` is empty.
    /// - [`Error::Io`] when a file cannot be opened.
    /// - [`Error::Encrypted`] when a password is wrong or encryption is
    ///   unsupported.
    /// - Any error from [`PagePlan::build`].
    pub fn from_specs(specs: Vec<InputSpec>) -> Result<Self> {
        if specs.is_empty() {
            return Err(Error::Unsupported(
                "at least one input document is required; got an empty input list".into(),
            ));
        }

        // Open all files first, accumulating them so their lifetimes last until
        // we're done building the plan.
        let mut opened: Vec<(Pdf<BufReader<std::fs::File>>, PageRange)> =
            Vec::with_capacity(specs.len());

        for (i, spec) in specs.iter().enumerate() {
            let file = std::fs::File::open(&spec.path).map_err(|e| {
                // Preserve Error::Io so callers can distinguish file-not-found from
                // encryption errors and parse failures.
                Error::Io(std::io::Error::new(
                    e.kind(),
                    format!("input {i}: cannot open '{}': {e}", spec.path.display()),
                ))
            })?;
            let reader = BufReader::new(file);
            let opts = PdfOpenOptions {
                password: spec
                    .password
                    .as_deref()
                    .map(|s| s.as_bytes().to_vec())
                    .unwrap_or_default(),
                ..PdfOpenOptions::default()
            };
            // Preserve Error::Encrypted so CLI layers can pattern-match on it
            // for exit-code decisions (e.g. bad password vs. unsupported feature).
            // Only Unsupported/Parse are promoted to Unsupported with input context.
            let pdf = Pdf::open_with_options(reader, opts).map_err(|e| match e {
                Error::Encrypted(_) => e,
                Error::Io(_) => e,
                other => {
                    Error::Unsupported(format!("input {i} ('{}'): {other}", spec.path.display()))
                }
            })?;
            opened.push((pdf, spec.range.clone()));
        }

        // Now build per-input plans.
        let mut per_input = Vec::with_capacity(opened.len());
        let mut flat = Vec::new();

        for (source_index, (ref mut pdf, ref range)) in opened.iter_mut().enumerate() {
            let plan = PagePlan::build(pdf, range)
                .map_err(|e| Error::Unsupported(format!("input {source_index}: {e}")))?;

            for page in plan.pages() {
                flat.push(CombinedPage {
                    source_index,
                    page: page.clone(),
                });
            }

            per_input.push(plan);
        }

        Ok(Self { per_input, flat })
    }

    // -----------------------------------------------------------------------
    // Accessors
    // -----------------------------------------------------------------------

    /// Index of the base input whose catalog and Info dictionary downstream
    /// rewrite layers should preserve. Always `0`.
    pub fn base_index(&self) -> usize {
        0
    }

    /// Per-input page plans, in input order.
    ///
    /// The caller can index into this slice with a `source_index` value from
    /// [`CombinedPage`] to retrieve the matching [`PagePlan`].
    pub fn per_input_plans(&self) -> &[PagePlan] {
        &self.per_input
    }

    /// Flat concatenated view: one entry per selected page in combined order.
    ///
    /// Each entry carries `source_index` so that downstream layers can
    /// determine which input `Pdf` a page belongs to.
    pub fn flat_pages(&self) -> &[CombinedPage] {
        &self.flat
    }

    /// Total number of selected pages across all inputs.
    pub fn total_page_count(&self) -> usize {
        self.flat.len()
    }

    /// Number of input documents in this plan.
    pub fn input_count(&self) -> usize {
        self.per_input.len()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::Pdf;
    use crate::ObjectRef;
    use std::io::Cursor;

    // -----------------------------------------------------------------------
    // In-memory PDF builder (duplicated from page_plan::tests for isolation)
    // -----------------------------------------------------------------------

    /// Build a minimal N-page PDF in memory.
    ///
    /// Object layout:
    ///   1 0 R  Catalog
    ///   2 0 R  Pages (root, Kids = [3 0 R, 4 0 R, ...])
    ///   3 0 R  Page 1
    ///   4 0 R  Page 2
    ///   …
    fn build_n_page_pdf(n: u32) -> Vec<u8> {
        assert!(n >= 1, "need at least 1 page");

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

    // -----------------------------------------------------------------------
    // CombinedPlan::build — normal cases
    // -----------------------------------------------------------------------

    #[test]
    fn single_input_all_pages() {
        let mut pdf = open(build_n_page_pdf(3));
        let range = PageRange::parse("").unwrap();
        let plan = CombinedPlan::build(vec![(&mut pdf, range)]).unwrap();

        assert_eq!(plan.input_count(), 1);
        assert_eq!(plan.total_page_count(), 3);
        assert_eq!(plan.base_index(), 0);

        let flat = plan.flat_pages();
        assert_eq!(flat[0].source_index, 0);
        assert_eq!(flat[0].page.index_1based, 1);
        assert_eq!(flat[1].page.index_1based, 2);
        assert_eq!(flat[2].page.index_1based, 3);
    }

    #[test]
    fn two_inputs_concatenate_in_order() {
        let mut pdf_a = open(build_n_page_pdf(3)); // pages: obj 3,4,5
        let mut pdf_b = open(build_n_page_pdf(2)); // pages: obj 3,4

        let range_a = PageRange::parse("1-3").unwrap();
        let range_b = PageRange::parse("1-2").unwrap();
        let plan = CombinedPlan::build(vec![(&mut pdf_a, range_a), (&mut pdf_b, range_b)]).unwrap();

        assert_eq!(plan.input_count(), 2);
        assert_eq!(plan.total_page_count(), 5);

        let flat = plan.flat_pages();
        // First 3 entries belong to input 0
        assert_eq!(flat[0].source_index, 0);
        assert_eq!(flat[1].source_index, 0);
        assert_eq!(flat[2].source_index, 0);
        // Last 2 entries belong to input 1
        assert_eq!(flat[3].source_index, 1);
        assert_eq!(flat[4].source_index, 1);
    }

    #[test]
    fn per_input_plans_are_accessible() {
        let mut pdf_a = open(build_n_page_pdf(4));
        let mut pdf_b = open(build_n_page_pdf(2));

        let range_a = PageRange::parse("2,4").unwrap();
        let range_b = PageRange::parse("").unwrap();
        let plan = CombinedPlan::build(vec![(&mut pdf_a, range_a), (&mut pdf_b, range_b)]).unwrap();

        let plans = plan.per_input_plans();
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].len(), 2); // pages 2,4 from input A
        assert_eq!(plans[1].len(), 2); // all 2 pages from input B
        assert_eq!(plans[0].pages()[0].index_1based, 2);
        assert_eq!(plans[0].pages()[1].index_1based, 4);
    }

    #[test]
    fn base_index_is_always_zero() {
        let mut pdf_a = open(build_n_page_pdf(2));
        let mut pdf_b = open(build_n_page_pdf(3));
        let plan = CombinedPlan::build(vec![
            (&mut pdf_a, PageRange::parse("").unwrap()),
            (&mut pdf_b, PageRange::parse("").unwrap()),
        ])
        .unwrap();
        assert_eq!(plan.base_index(), 0);
    }

    #[test]
    fn single_input_with_range_subset() {
        // Input has 5 pages, range selects "2,4"
        let mut pdf = open(build_n_page_pdf(5));
        let range = PageRange::parse("2,4").unwrap();
        let plan = CombinedPlan::build(vec![(&mut pdf, range)]).unwrap();

        assert_eq!(plan.total_page_count(), 2);
        let flat = plan.flat_pages();
        assert_eq!(flat[0].page.index_1based, 2);
        // page 2 → object 4 (3+1)
        assert_eq!(flat[0].page.page_ref, ObjectRef::new(4, 0));
        assert_eq!(flat[1].page.index_1based, 4);
        assert_eq!(flat[1].page.page_ref, ObjectRef::new(6, 0));
    }

    #[test]
    fn three_inputs_concatenation_order() {
        let mut pdf_a = open(build_n_page_pdf(2));
        let mut pdf_b = open(build_n_page_pdf(3));
        let mut pdf_c = open(build_n_page_pdf(1));

        let plan = CombinedPlan::build(vec![
            (&mut pdf_a, PageRange::parse("").unwrap()),
            (&mut pdf_b, PageRange::parse("1,3").unwrap()),
            (&mut pdf_c, PageRange::parse("").unwrap()),
        ])
        .unwrap();

        // A: 2 pages, B: 2 pages (1,3), C: 1 page
        assert_eq!(plan.total_page_count(), 5);
        let flat = plan.flat_pages();

        // First 2 from A
        assert!(flat[0..2].iter().all(|p| p.source_index == 0));
        // Next 2 from B
        assert!(flat[2..4].iter().all(|p| p.source_index == 1));
        // Last 1 from C
        assert_eq!(flat[4].source_index, 2);

        // B's pages: index 1 and 3
        assert_eq!(flat[2].page.index_1based, 1);
        assert_eq!(flat[3].page.index_1based, 3);
    }

    #[test]
    fn flat_pages_match_per_input_plans() {
        let mut pdf_a = open(build_n_page_pdf(3));
        let mut pdf_b = open(build_n_page_pdf(2));

        let plan = CombinedPlan::build(vec![
            (&mut pdf_a, PageRange::parse("1,3").unwrap()),
            (&mut pdf_b, PageRange::parse("2").unwrap()),
        ])
        .unwrap();

        let flat = plan.flat_pages();
        let plans = plan.per_input_plans();

        // flat[0] and flat[1] should match per_input[0]
        assert_eq!(flat[0].page, plans[0].pages()[0]);
        assert_eq!(flat[1].page, plans[0].pages()[1]);
        // flat[2] should match per_input[1]
        assert_eq!(flat[2].page, plans[1].pages()[0]);
    }

    // -----------------------------------------------------------------------
    // CombinedPlan::build — error cases
    // -----------------------------------------------------------------------

    #[test]
    fn empty_inputs_is_error() {
        // CombinedPlan::build needs a concrete R type — use Cursor<Vec<u8>>.
        let inputs: Vec<(&mut Pdf<Cursor<Vec<u8>>>, PageRange)> = vec![];
        let err = CombinedPlan::build(inputs).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("at least one input"),
            "expected actionable message, got: {msg}"
        );
    }

    #[test]
    fn out_of_range_page_in_second_input_is_error() {
        let mut pdf_a = open(build_n_page_pdf(3));
        let mut pdf_b = open(build_n_page_pdf(2));

        let err = CombinedPlan::build(vec![
            (&mut pdf_a, PageRange::parse("1").unwrap()),
            (&mut pdf_b, PageRange::parse("5").unwrap()), // 5 > 2
        ])
        .unwrap_err();

        let msg = err.to_string();
        assert!(
            msg.contains("input 1"),
            "expected error attributed to input 1, got: {msg}"
        );
        assert!(
            msg.contains("out of range"),
            "expected 'out of range' in error message, got: {msg}"
        );
    }

    #[test]
    fn out_of_range_page_in_first_input_is_error() {
        let mut pdf_a = open(build_n_page_pdf(2));

        let err = CombinedPlan::build(vec![
            (&mut pdf_a, PageRange::parse("10").unwrap()), // 10 > 2
        ])
        .unwrap_err();

        let msg = err.to_string();
        assert!(
            msg.contains("input 0"),
            "expected error attributed to input 0, got: {msg}"
        );
    }

    // -----------------------------------------------------------------------
    // Password forwarding test (via PdfOpenOptions — no real encrypted file)
    // -----------------------------------------------------------------------

    #[test]
    fn input_spec_password_is_forwarded_to_open_options() {
        // Verify that InputSpec::new correctly stores the password field,
        // so it will be passed to PdfOpenOptions when from_specs is called.
        let spec = InputSpec::new(
            "test.pdf",
            Some("secret".into()),
            PageRange::parse("").unwrap(),
        );
        assert_eq!(spec.password, Some("secret".to_string()));

        // Also verify no-password case
        let spec_no_pw = InputSpec::new("test.pdf", None, PageRange::parse("").unwrap());
        assert_eq!(spec_no_pw.password, None);
    }

    #[test]
    fn from_specs_nonexistent_file_is_actionable_error() {
        let spec = InputSpec::new(
            "/nonexistent/path/to/file.pdf",
            None,
            PageRange::parse("").unwrap(),
        );
        let err = CombinedPlan::from_specs(vec![spec]).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("input 0"),
            "expected error attributed to input 0, got: {msg}"
        );
    }

    #[test]
    fn from_specs_empty_is_error() {
        let err = CombinedPlan::from_specs(vec![]).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("at least one input"),
            "expected actionable message, got: {msg}"
        );
    }

    #[test]
    fn from_specs_nonexistent_file_preserves_io_error_variant() {
        let spec = InputSpec::new(
            "/nonexistent/path/to/file.pdf",
            None,
            PageRange::parse("").unwrap(),
        );
        let err = CombinedPlan::from_specs(vec![spec]).unwrap_err();
        // Error::Io must be preserved so CLI layers can distinguish IO failures.
        assert!(
            matches!(err, Error::Io(_)),
            "expected Error::Io, got: {err:?}"
        );
    }

    // -----------------------------------------------------------------------
    // from_specs — happy path (write a real file to disk and read it back)
    // -----------------------------------------------------------------------

    #[test]
    fn from_specs_two_files_concatenate_in_order() {
        use std::io::Write;

        let dir = std::env::temp_dir();
        let path_a = dir.join("flpdf_test_combine_a.pdf");
        let path_b = dir.join("flpdf_test_combine_b.pdf");

        // Write 3-page and 2-page PDFs to disk.
        std::fs::File::create(&path_a)
            .unwrap()
            .write_all(&build_n_page_pdf(3))
            .unwrap();
        std::fs::File::create(&path_b)
            .unwrap()
            .write_all(&build_n_page_pdf(2))
            .unwrap();

        let specs = vec![
            InputSpec::new(&path_a, None, PageRange::parse("1,3").unwrap()),
            InputSpec::new(&path_b, None, PageRange::parse("").unwrap()),
        ];
        let plan = CombinedPlan::from_specs(specs).unwrap();

        // A selected pages 1,3 (2 pages) + B all pages (2 pages)
        assert_eq!(plan.input_count(), 2);
        assert_eq!(plan.total_page_count(), 4);
        assert_eq!(plan.base_index(), 0);

        let flat = plan.flat_pages();
        assert_eq!(flat[0].source_index, 0);
        assert_eq!(flat[0].page.index_1based, 1);
        assert_eq!(flat[1].source_index, 0);
        assert_eq!(flat[1].page.index_1based, 3);
        assert_eq!(flat[2].source_index, 1);
        assert_eq!(flat[2].page.index_1based, 1);
        assert_eq!(flat[3].source_index, 1);
        assert_eq!(flat[3].page.index_1based, 2);

        // Clean up temp files.
        let _ = std::fs::remove_file(&path_a);
        let _ = std::fs::remove_file(&path_b);
    }
}
