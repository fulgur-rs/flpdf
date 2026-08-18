//! qpdf correspondence: `QPDFJob::doSplitPages` output naming helpers.
//!
//! The split lifecycle lives in [`crate::job::QPDFJob::split_pages`]. This
//! module retains the pure qpdf 11.9.0 filename and page-count helpers plus a
//! thin bytes-based compatibility facade for existing library callers.
//!
//! # Naming convention
//!
//! The output path naming follows qpdf 11.9.0's convention, confirmed by
//! running `qpdf --split-pages=2 <in>.pdf <out>.pdf` with various page counts:
//!
//! ```text
//! # 3-page source, split=1 → 3 chunks (observed: out-1.pdf, out-2.pdf, out-3.pdf)
//! # 5-page source, split=2 → 3 chunks (observed: _out-1-2.pdf, _out-3-4.pdf, _out-5-5.pdf)
//! # 11-page source, split=2 → 6 chunks (observed: _out11-01-02.pdf, ..., _out11-11-11.pdf)
//! # 100-page source, split=10 → 10 chunks (observed: _out100-001-010.pdf, ...)
//! ```
//!
//! The pattern is `{stem}-{first}-{last}{ext}` for `--split-pages >= 2`, and
//! `{stem}-{page}{ext}` for `--split-pages=1` (qpdf 11.9.0 emits a single
//! page number per chunk when every chunk is one page; the range form is
//! retained for `>= 2`, including a trailing single-page chunk). In both
//! forms:
//! - The separator between stem and page number(s) is `-`.
//! - Page numbers are 1-based positions in the **source document**.
//! - Each number is zero-padded to the digit-width of the **source page
//!   count** (NOT the chunk count).  For 1–9 pages: no padding (width=1);
//!   for 10–99 pages: width=2; for 100–999 pages: width=3, etc.
//! - The extension (including the `.`) comes after the page number(s). If
//!   the template has no extension, no `.` is added.
//! - The split is at the **last** `.` in the filename portion of the path
//!   (confirmed with `two.dots.pdf` → `two.dots-1-2.pdf`).
//!
//! The source page copy, inherited-attribute materialization, AcroForm fix,
//! chunk-local labels, and writer lifecycle are deliberately owned by the job
//! module so every caller reaches one canonical route.

use crate::{Pdf, Result};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Compatibility facade for qpdf's fresh-document split job.
///
/// The bytes are opened once and delegated to [`crate::job::QPDFJob::split_pages`]
/// so the canonical foreign-page, AcroForm, PageLabels, and writer lifecycle
/// is shared with the CLI. New callers that know the input path should use the
/// job API directly to enable qpdf's same-file guard.
///
/// # Arguments
///
/// - `src_bytes`: Raw bytes of the source PDF, taken by value and moved into
///   the owned in-memory source document.
/// - `chunk_size`: Number of pages per chunk (`N` in `--split-pages=N`). Must
///   be ≥ 1. Values ≥ total page count emit a single file containing all pages.
/// - `output_template`: Output path template. The page suffix is inserted
///   immediately before the last `.` in the filename (or at the end if there
///   is no `.`): `-{first}-{last}` for `chunk_size >= 2` (e.g. `output.pdf`
///   → `output-1-2.pdf`), or `-{page}` for `chunk_size == 1` (e.g.
///   `output.pdf` → `output-1.pdf`), matching qpdf 11.9.0.
///
/// When `deterministic_id` is set, each chunk uses a content-derived
/// deterministic `/ID` (mirroring `qpdf --split-pages --deterministic-id`,
/// which applies the deterministic-ID policy to every split output).
///
/// Returns the list of chunk output paths in write order. Callers that want to
/// mirror qpdf's `--verbose --split-pages` `wrote file <chunk>` emission can
/// iterate this list; qpdf 11.9.0 emits one such line per chunk inside its
/// split loop (`libqpdf/QPDFJob.cc doSplitPages`).
///
/// # Errors
///
/// - [`crate::Error::Missing`] if the source PDF has no `/Root` or no pages.
/// - [`crate::Error::Io`] on any file-system error writing the output files.
/// - Other [`crate::Error`] variants on structural PDF problems.
pub fn split_pages(
    src_bytes: Vec<u8>,
    chunk_size: usize,
    output_template: &Path,
    deterministic_id: bool,
) -> Result<Vec<PathBuf>> {
    let mut pdf = Pdf::open_mem_owned(src_bytes)?;
    let mut job = crate::job::QPDFJob::new();
    job.split_pages(
        &mut pdf,
        crate::job::SplitPageOptions::new(chunk_size, output_template)
            .with_deterministic_id(deterministic_id),
    )
}

/// Compute the **range-form** output path (`{stem}-{first}-{last}{ext}`) for a
/// chunk given the template and 1-based page numbers.
///
/// This is a pure function suitable for unit testing independent of PDF I/O.
/// It is the naming used by qpdf 11.9.0 for `--split-pages >= 2`. The
/// `--split-pages=1` single-number form is produced by [`split_pages`] via
/// the internal `chunk_output_path` helper, so this public helper keeps a
/// stable signature and always emits the range form.
///
/// # Naming rule (observed with qpdf 11.9.0)
///
/// - Split at the *last* `.` in the filename component.
/// - If no `.` exists, append the page range suffix directly (no extension).
/// - Zero-pad `first` and `last` to `width` digits (width = digit count of
///   source page count, not chunk count).
///
/// A leading-dot ("hidden file") template such as `.pdf` is intentionally
/// treated the same as any other: the last `.` is at index 0, so the stem is
/// empty and the result is `-1-2.pdf`. This is **not** a bug — it matches
/// qpdf 11.9.0 exactly: `qpdf --split-pages=2 in.pdf /tmp/.pdf` writes
/// `/tmp/-1-2.pdf` and `/tmp/-3-3.pdf`. Do not special-case `dot_pos == 0`
/// to produce `.pdf-1-2`; that would diverge from qpdf.
///
/// ```
/// # use std::path::{Path, PathBuf};
/// # use flpdf::page_split::split_output_path;
/// // 5-page source → width=1
/// assert_eq!(
///     split_output_path(Path::new("out.pdf"), 1, 2, 1),
///     PathBuf::from("out-1-2.pdf"),
/// );
/// // 11-page source → width=2
/// assert_eq!(
///     split_output_path(Path::new("out.pdf"), 11, 11, 2),
///     PathBuf::from("out-11-11.pdf"),
/// );
/// // No extension
/// assert_eq!(
///     split_output_path(Path::new("out"), 1, 2, 1),
///     PathBuf::from("out-1-2"),
/// );
/// // Multiple dots: split at last dot
/// assert_eq!(
///     split_output_path(Path::new("two.dots.pdf"), 1, 2, 1),
///     PathBuf::from("two.dots-1-2.pdf"),
/// );
/// // Leading-dot template: qpdf 11.9.0-verified — empty stem, ".pdf" ext
/// assert_eq!(
///     split_output_path(Path::new(".pdf"), 1, 2, 1),
///     PathBuf::from("-1-2.pdf"),
/// );
/// ```
pub fn split_output_path(
    template: &Path,
    first_page: u32,
    last_page: u32,
    width: usize,
) -> PathBuf {
    let range = format!(
        "{:0>width$}-{:0>width$}",
        first_page,
        last_page,
        width = width
    );
    if let Some(path) = replace_first_percent_d(template, &range) {
        return path;
    }
    let (parent, stem, ext) = split_template(template);

    let new_filename = format!("{stem}-{range}{ext}");

    join_parent(parent, new_filename)
}

/// Split a template path into `(parent, stem, ext)` where `ext` includes the
/// leading `.` (empty when the filename has no `.`). Shared by
/// [`split_output_path`] and [`chunk_output_path`].
fn split_template(template: &Path) -> (&Path, String, String) {
    let parent = template.parent().unwrap_or(Path::new(""));
    let filename = template
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    let (stem, ext) = match filename.rfind('.') {
        Some(dot_pos) => {
            let (s, e) = filename.split_at(dot_pos);
            (s.to_string(), e.to_string()) // e includes the leading '.'
        }
        None => (filename.clone(), String::new()),
    };
    (parent, stem, ext)
}

/// Join `filename` onto `parent`, returning a bare `PathBuf` when there is no
/// parent component.
fn join_parent(parent: &Path, filename: String) -> PathBuf {
    if parent == Path::new("") {
        PathBuf::from(filename)
    } else {
        parent.join(filename)
    }
}

/// Compute the output path for one chunk, honoring qpdf 11.9.0's
/// `chunk_size`-dependent naming:
///
/// - `chunk_size == 1`: single-number suffix `{stem}-{page}{ext}` (every
///   chunk is exactly one page, e.g. `out-1.pdf`).
/// - `chunk_size >= 2`: range suffix `{stem}-{lo}-{hi}{ext}` via
///   [`split_output_path`], retained even for a trailing single-page chunk
///   (e.g. 3 pages `--split-pages=2` → `out-1-2.pdf`, `out-3-3.pdf`).
///
/// Internal: keeps the public [`split_output_path`] signature stable while
/// adding the single-number form needed by `--split-pages=1`.
pub(crate) fn chunk_output_path(
    template: &Path,
    first_page: u32,
    last_page: u32,
    width: usize,
    chunk_size: usize,
) -> PathBuf {
    let suffix = if chunk_size == 1 {
        format!("{first_page:0>width$}", width = width)
    } else {
        format!("{first_page:0>width$}-{last_page:0>width$}", width = width)
    };
    if let Some(path) = replace_first_percent_d(template, &suffix) {
        return path;
    }
    if chunk_size != 1 {
        return split_output_path(template, first_page, last_page, width);
    }
    let (parent, stem, ext) = split_template(template);
    let new_filename = format!("{stem}-{first_page:0>width$}{ext}", width = width);
    join_parent(parent, new_filename)
}

/// Replace qpdf's first output-template `%d` with the page number or range.
///
/// qpdf searches the complete output path, not only the filename, and leaves
/// later `%d` sequences literal. Returning `None` keeps the ordinary
/// stem/extension naming path allocation-free for templates without a
/// placeholder.
fn replace_first_percent_d(template: &Path, replacement: &str) -> Option<PathBuf> {
    let template = template.to_string_lossy();
    let position = template.find("%d")?;
    let mut output = String::with_capacity(template.len() + replacement.len());
    output.push_str(&template[..position]);
    output.push_str(replacement);
    output.push_str(&template[position + 2..]);
    Some(PathBuf::from(output))
}

/// Return the number of decimal digits needed to represent `n`.
///
/// - `n = 0` → 1 (edge case, won't occur for real page counts ≥ 1)
/// - `n = 1..=9` → 1
/// - `n = 10..=99` → 2
/// - `n = 100..=999` → 3
/// - etc.
///
/// This determines the zero-pad width: qpdf pads page numbers to the number
/// of digits of the **source page count**, not the chunk count.
///
/// Empirical evidence (qpdf 11.9.0):
/// - 5 pages, split=2 → chunks named `1-2`, `3-4`, `5-5` (width=1, no padding)
/// - 11 pages, split=2 → `01-02`, …, `11-11` (width=2)
/// - 100 pages, split=10 → `001-010`, …, `091-100` (width=3)
pub fn digit_width(n: u32) -> usize {
    if n == 0 {
        return 1;
    }
    let mut w = 0;
    let mut v = n;
    while v > 0 {
        w += 1;
        v /= 10;
    }
    w
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::page_refs;
    use crate::{Object, Pdf};
    use std::io::Cursor;
    use std::sync::Arc;

    // -----------------------------------------------------------------------
    // Pure-function unit tests: naming
    // -----------------------------------------------------------------------

    #[test]
    fn chunk_output_path_chunk_size_1_single_number() {
        // --split-pages=1: qpdf 11.9.0 names each single-page chunk with a
        // single page number (out-1.pdf), NOT the range form (out-1-1.pdf).
        assert_eq!(
            chunk_output_path(Path::new("out.pdf"), 1, 1, 1, 1),
            PathBuf::from("out-1.pdf"),
        );
        assert_eq!(
            chunk_output_path(Path::new("out.pdf"), 3, 3, 1, 1),
            PathBuf::from("out-3.pdf"),
        );
    }

    #[test]
    fn chunk_output_path_replaces_only_the_first_percent_d() {
        assert_eq!(
            chunk_output_path(Path::new("literal-%d-tail.pdf"), 1, 1, 1, 1),
            PathBuf::from("literal-1-tail.pdf"),
        );
        assert_eq!(
            chunk_output_path(Path::new("literal-%d-%d.pdf"), 1, 2, 1, 2),
            PathBuf::from("literal-1-2-%d.pdf"),
        );
    }

    #[test]
    fn chunk_output_path_chunk_size_1_single_number_zero_padded() {
        // Single-number form is still zero-padded to `width`.
        assert_eq!(
            chunk_output_path(Path::new("out.pdf"), 7, 7, 2, 1),
            PathBuf::from("out-07.pdf"),
        );
    }

    #[test]
    fn chunk_output_path_chunk_size_1_single_number_no_extension() {
        assert_eq!(
            chunk_output_path(Path::new("out"), 2, 2, 1, 1),
            PathBuf::from("out-2"),
        );
    }

    #[test]
    fn chunk_output_path_chunk_size_ge_2_delegates_to_range_form() {
        // chunk_size >= 2 keeps the range form, including a trailing
        // single-page chunk (out-3-3.pdf) — must match split_output_path.
        assert_eq!(
            chunk_output_path(Path::new("out.pdf"), 1, 2, 1, 2),
            split_output_path(Path::new("out.pdf"), 1, 2, 1),
        );
        assert_eq!(
            chunk_output_path(Path::new("out.pdf"), 3, 3, 1, 2),
            PathBuf::from("out-3-3.pdf"),
        );
    }

    #[test]
    fn split_output_path_basic_pdf_extension() {
        // 5-page source (width=1): out-1-2.pdf
        assert_eq!(
            split_output_path(Path::new("out.pdf"), 1, 2, 1),
            PathBuf::from("out-1-2.pdf"),
        );
    }

    #[test]
    fn split_output_path_replaces_only_the_first_percent_d() {
        assert_eq!(
            split_output_path(Path::new("literal-%d-tail-%d.pdf"), 1, 2, 1),
            PathBuf::from("literal-1-2-tail-%d.pdf"),
        );
    }

    #[test]
    fn split_output_path_last_chunk_same_page() {
        // Final chunk where first == last (range form retained).
        assert_eq!(
            split_output_path(Path::new("out.pdf"), 5, 5, 1),
            PathBuf::from("out-5-5.pdf"),
        );
    }

    #[test]
    fn split_output_path_zero_padded_width2() {
        // 11-page source (width=2): first chunk 1-2 → 01-02
        assert_eq!(
            split_output_path(Path::new("out.pdf"), 1, 2, 2),
            PathBuf::from("out-01-02.pdf"),
        );
        // Last chunk 11-11
        assert_eq!(
            split_output_path(Path::new("out.pdf"), 11, 11, 2),
            PathBuf::from("out-11-11.pdf"),
        );
    }

    #[test]
    fn split_output_path_zero_padded_width3() {
        // 100-page source (width=3): first chunk 1-10 → 001-010
        assert_eq!(
            split_output_path(Path::new("out.pdf"), 1, 10, 3),
            PathBuf::from("out-001-010.pdf"),
        );
        assert_eq!(
            split_output_path(Path::new("out.pdf"), 91, 100, 3),
            PathBuf::from("out-091-100.pdf"),
        );
    }

    #[test]
    fn split_output_path_no_extension() {
        // No `.` in filename → suffix appended without extension
        assert_eq!(
            split_output_path(Path::new("out"), 1, 2, 1),
            PathBuf::from("out-1-2"),
        );
    }

    #[test]
    fn split_output_path_multiple_dots_splits_at_last() {
        // "two.dots.pdf" → split at last `.` → "two.dots-1-2.pdf"
        assert_eq!(
            split_output_path(Path::new("two.dots.pdf"), 1, 2, 1),
            PathBuf::from("two.dots-1-2.pdf"),
        );
    }

    #[test]
    fn split_output_path_leading_dot_template_matches_qpdf() {
        // qpdf 11.9.0-verified: `qpdf --split-pages=2 in.pdf /tmp/.pdf`
        // writes /tmp/-1-2.pdf (empty stem, ".pdf" treated as extension).
        // Must NOT special-case dot_pos==0 into ".pdf-1-2".
        assert_eq!(
            split_output_path(Path::new(".pdf"), 1, 2, 1),
            PathBuf::from("-1-2.pdf"),
        );
        assert_eq!(
            split_output_path(Path::new("/tmp/.pdf"), 3, 3, 1),
            PathBuf::from("/tmp/-3-3.pdf"),
        );
    }

    #[test]
    fn split_output_path_preserves_parent_directory() {
        assert_eq!(
            split_output_path(Path::new("/tmp/out.pdf"), 3, 4, 1),
            PathBuf::from("/tmp/out-3-4.pdf"),
        );
    }

    // -----------------------------------------------------------------------
    // Pure-function unit tests: digit_width
    // -----------------------------------------------------------------------

    #[test]
    fn digit_width_small_values() {
        assert_eq!(digit_width(1), 1);
        assert_eq!(digit_width(9), 1);
        assert_eq!(digit_width(10), 2);
        assert_eq!(digit_width(99), 2);
        assert_eq!(digit_width(100), 3);
        assert_eq!(digit_width(999), 3);
        assert_eq!(digit_width(1000), 4);
    }

    #[test]
    fn digit_width_matches_qpdf_observations() {
        // Empirically observed with qpdf 11.9.0:
        // - 5-page source → width 1 (no zero-padding)
        assert_eq!(digit_width(5), 1);
        // - 11-page source → width 2
        assert_eq!(digit_width(11), 2);
        // - 100-page source → width 3
        assert_eq!(digit_width(100), 3);
    }

    // -----------------------------------------------------------------------
    // Minimal PDF builder (reused from page_plan tests)
    // -----------------------------------------------------------------------

    /// Build a minimal valid N-page PDF in memory.
    ///
    /// Object layout:
    ///   1 0 R  Catalog
    ///   2 0 R  Pages root (Kids = [3 0 R, 4 0 R, …])
    ///   3 0 R  Page 1
    ///   4 0 R  Page 2
    ///   …
    fn build_n_page_pdf(n: u32) -> Vec<u8> {
        assert!(n >= 1);
        let mut pdf: Vec<u8> = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        let kids: String = (3u32..=2 + n).map(|i| format!("{i} 0 R ")).collect();
        pdf.extend_from_slice(
            format!("2 0 obj\n<< /Type /Pages /Kids [{kids}] /Count {n} >>\nendobj\n").as_bytes(),
        );

        let mut page_offsets: Vec<u64> = Vec::new();
        for i in 3u32..=2 + n {
            let off = pdf.len() as u64;
            page_offsets.push(off);
            pdf.extend_from_slice(
                format!(
                    "{i} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n"
                )
                .as_bytes(),
            );
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

    /// Like [`build_n_page_pdf`] but the catalog also carries
    /// `/PageLabels << /Nums {nums_body} >>` (an inline, direct dict — the
    /// same shape [`super::write_reconstructed_labels`] itself installs).
    fn build_n_page_pdf_with_pagelabels(n: u32, nums_body: &str) -> Vec<u8> {
        assert!(n >= 1);
        let mut pdf: Vec<u8> = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(
            format!(
                "1 0 obj\n<< /Type /Catalog /Pages 2 0 R /PageLabels << /Nums {nums_body} >> >>\nendobj\n"
            )
            .as_bytes(),
        );

        let off2 = pdf.len() as u64;
        let kids: String = (3u32..=2 + n).map(|i| format!("{i} 0 R ")).collect();
        pdf.extend_from_slice(
            format!("2 0 obj\n<< /Type /Pages /Kids [{kids}] /Count {n} >>\nendobj\n").as_bytes(),
        );

        let mut page_offsets: Vec<u64> = Vec::new();
        for i in 3u32..=2 + n {
            let off = pdf.len() as u64;
            page_offsets.push(off);
            pdf.extend_from_slice(
                format!(
                    "{i} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n"
                )
                .as_bytes(),
            );
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

    /// Read the catalog's `/PageLabels /Nums` entries as `(index, Dictionary)`
    /// pairs, for asserting the exact reconstructed shape of a chunk's labels.
    fn read_nums(bytes: &[u8]) -> Vec<(i64, crate::Dictionary)> {
        let mut pdf = Pdf::open(Cursor::new(bytes.to_vec())).expect("should parse");
        let catalog_ref = pdf.root_ref().expect("/Root");
        let catalog = pdf
            .resolve(catalog_ref)
            .expect("resolve catalog")
            .into_dict()
            .expect("catalog is a dict");
        let Some(Object::Dictionary(page_labels)) = catalog.get("PageLabels") else {
            panic!("/PageLabels must be a direct dictionary, got {catalog:?}"); // cov:ignore: defensive — split_pages always installs a direct dict when labels exist
        };
        let Some(Object::Array(nums)) = page_labels.get("Nums") else {
            panic!("/Nums must be a direct array"); // cov:ignore: defensive — write_reconstructed_labels always installs a direct array
        };
        nums.chunks_exact(2)
            .map(|pair| {
                let idx = match &pair[0] {
                    Object::Integer(n) => *n,
                    other => panic!("expected an integer index, got {other:?}"), // cov:ignore: defensive — write_reconstructed_labels always emits an integer index
                };
                let dict = match &pair[1] {
                    Object::Dictionary(d) => d.clone(),
                    other => panic!("expected a label dictionary, got {other:?}"), // cov:ignore: defensive — write_reconstructed_labels always emits a label dictionary
                };
                (idx, dict)
            })
            .collect()
    }

    // -----------------------------------------------------------------------
    // Integration tests: split_pages
    // -----------------------------------------------------------------------

    /// Open a PDF from bytes and return the page count.
    fn page_count_of(bytes: &[u8]) -> usize {
        let mut pdf = Pdf::open_mem(Arc::from(bytes)).expect("should parse");
        page_refs(&mut pdf).expect("should get page refs").len()
    }

    #[test]
    fn split_pages_single_chunk_size_one() {
        let src = build_n_page_pdf(3);
        let tmpdir = tempfile::tempdir().expect("tmpdir");
        let template = tmpdir.path().join("out.pdf");

        split_pages(src, 1, &template, false).expect("split should succeed");

        // qpdf 11.9.0: --split-pages=1 → single-number suffix (out-N.pdf),
        // not the range form out-N-N.pdf (flpdf-s5e).
        let out1 = tmpdir.path().join("out-1.pdf");
        let out2 = tmpdir.path().join("out-2.pdf");
        let out3 = tmpdir.path().join("out-3.pdf");

        assert!(out1.exists(), "chunk 1 should exist");
        assert!(out2.exists(), "chunk 2 should exist");
        assert!(out3.exists(), "chunk 3 should exist");

        assert_eq!(page_count_of(&std::fs::read(&out1).unwrap()), 1);
        assert_eq!(page_count_of(&std::fs::read(&out2).unwrap()), 1);
        assert_eq!(page_count_of(&std::fs::read(&out3).unwrap()), 1);
    }

    #[test]
    fn split_pages_chunk_size_two_with_remainder() {
        // 5 pages, split=2 → chunks [1-2], [3-4], [5-5]
        let src = build_n_page_pdf(5);
        let tmpdir = tempfile::tempdir().expect("tmpdir");
        let template = tmpdir.path().join("out.pdf");

        split_pages(src, 2, &template, false).expect("split should succeed");

        let out12 = tmpdir.path().join("out-1-2.pdf");
        let out34 = tmpdir.path().join("out-3-4.pdf");
        let out55 = tmpdir.path().join("out-5-5.pdf");

        assert!(out12.exists(), "chunk 1-2 should exist: {:?}", out12);
        assert!(out34.exists(), "chunk 3-4 should exist: {:?}", out34);
        assert!(out55.exists(), "chunk 5-5 should exist: {:?}", out55);

        assert_eq!(page_count_of(&std::fs::read(&out12).unwrap()), 2);
        assert_eq!(page_count_of(&std::fs::read(&out34).unwrap()), 2);
        assert_eq!(page_count_of(&std::fs::read(&out55).unwrap()), 1);
    }

    #[test]
    fn split_pages_large_n_emits_one_file() {
        // N >= page count → single output file with all pages
        let src = build_n_page_pdf(3);
        let tmpdir = tempfile::tempdir().expect("tmpdir");
        let template = tmpdir.path().join("out.pdf");

        split_pages(src, 100, &template, false).expect("split should succeed");

        let out = tmpdir.path().join("out-1-3.pdf");
        assert!(out.exists(), "single chunk file should exist: {:?}", out);
        assert_eq!(page_count_of(&std::fs::read(&out).unwrap()), 3);
    }

    #[test]
    fn split_pages_naming_zero_padded_for_10plus_pages() {
        // 11 pages, split=2 → files with zero-padded 2-digit numbers
        let src = build_n_page_pdf(11);
        let tmpdir = tempfile::tempdir().expect("tmpdir");
        let template = tmpdir.path().join("out.pdf");

        split_pages(src, 2, &template, false).expect("split should succeed");

        // First chunk: out-01-02.pdf
        let first = tmpdir.path().join("out-01-02.pdf");
        assert!(
            first.exists(),
            "zero-padded first chunk should exist: {:?}",
            first
        );

        // Last chunk: out-11-11.pdf
        let last = tmpdir.path().join("out-11-11.pdf");
        assert!(
            last.exists(),
            "zero-padded last chunk should exist: {:?}",
            last
        );
    }

    #[test]
    fn split_pages_deterministic_id_produces_stable_chunks() {
        // With deterministic_id, each chunk is a full rewrite with a
        // content-derived /ID, so two runs over the same input are byte-stable.
        let src = build_n_page_pdf(3);
        let d1 = tempfile::tempdir().expect("tmpdir");
        let d2 = tempfile::tempdir().expect("tmpdir");
        // `split_pages` takes ownership, so the second run needs its own copy.
        // The clone is this test's, not the function's: a single run copies
        // nothing.
        split_pages(src.clone(), 1, &d1.path().join("out.pdf"), true)
            .expect("split should succeed");
        split_pages(src, 1, &d2.path().join("out.pdf"), true).expect("split should succeed");

        let c1 = std::fs::read(d1.path().join("out-1.pdf")).unwrap();
        let c2 = std::fs::read(d2.path().join("out-1.pdf")).unwrap();
        assert_eq!(
            c1, c2,
            "deterministic-id chunks must be byte-stable across runs"
        );
        assert!(
            c1.windows(3).any(|w| w == b"/ID"),
            "a deterministic-id chunk must carry an /ID"
        );
    }

    #[test]
    fn split_pages_propagates_chunk_write_error() {
        // A chunk write into a non-existent directory fails at the canonical
        // job writer and must propagate out of the compatibility facade.
        let src = build_n_page_pdf(2);
        let tmpdir = tempfile::tempdir().expect("tmpdir");
        let bad = tmpdir.path().join("no_such_subdir").join("out.pdf");
        let result = split_pages(src, 1, &bad, false);
        assert!(
            result.is_err(),
            "a chunk write failure must propagate out of split_pages"
        );
    }

    #[test]
    fn split_pages_chunk_size_zero_is_error() {
        let src = build_n_page_pdf(3);
        let tmpdir = tempfile::tempdir().expect("tmpdir");
        let template = tmpdir.path().join("out.pdf");
        let result = split_pages(src, 0, &template, false);
        assert!(result.is_err(), "chunk_size=0 should return an error");
    }

    #[test]
    fn split_pages_reconstructs_page_labels_per_chunk() {
        // 5-page source: roman lowercase from page 0, decimal (restart at 1)
        // from page 3. split=2 → chunks [0,1], [2,3], [4,4]. Expected shape
        // verified byte-for-byte against qpdf 11.9.0 `--split-pages=2`.
        let src = build_n_page_pdf_with_pagelabels(5, "[0 << /S /r >> 3 << /S /D /St 1 >>]");
        let tmpdir = tempfile::tempdir().expect("tmpdir");
        let template = tmpdir.path().join("out.pdf");

        split_pages(src, 2, &template, false).expect("split should succeed");

        let chunk1 = std::fs::read(tmpdir.path().join("out-1-2.pdf")).unwrap();
        let chunk2 = std::fs::read(tmpdir.path().join("out-3-4.pdf")).unwrap();
        let chunk3 = std::fs::read(tmpdir.path().join("out-5-5.pdf")).unwrap();

        let s = |name: &str| Object::Name(name.as_bytes().to_vec());

        let nums1 = read_nums(&chunk1);
        assert_eq!(nums1.len(), 1);
        assert_eq!(nums1[0].0, 0);
        assert_eq!(nums1[0].1.get("S"), Some(&s("r")));
        assert_eq!(nums1[0].1.get("St"), Some(&Object::Integer(1)));

        let nums2 = read_nums(&chunk2);
        assert_eq!(nums2.len(), 2, "roman continuation + decimal restart");
        assert_eq!(nums2[0].0, 0);
        assert_eq!(nums2[0].1.get("S"), Some(&s("r")));
        assert_eq!(nums2[0].1.get("St"), Some(&Object::Integer(3)));
        assert_eq!(nums2[1].0, 1);
        assert_eq!(nums2[1].1.get("S"), Some(&s("D")));
        assert_eq!(nums2[1].1.get("St"), Some(&Object::Integer(1)));

        let nums3 = read_nums(&chunk3);
        assert_eq!(nums3.len(), 1);
        assert_eq!(nums3[0].0, 0);
        assert_eq!(nums3[0].1.get("S"), Some(&s("D")));
        assert_eq!(nums3[0].1.get("St"), Some(&Object::Integer(2)));
    }

    #[test]
    fn split_pages_without_page_labels_omits_pagelabels_key() {
        let src = build_n_page_pdf(3);
        let tmpdir = tempfile::tempdir().expect("tmpdir");
        let template = tmpdir.path().join("out.pdf");
        split_pages(src, 1, &template, false).expect("split should succeed");

        let chunk = std::fs::read(tmpdir.path().join("out-1.pdf")).unwrap();
        let mut pdf = Pdf::open(Cursor::new(chunk)).expect("should parse");
        let catalog_ref = pdf.root_ref().unwrap();
        let catalog = pdf
            .resolve(catalog_ref)
            .unwrap()
            .into_dict()
            .expect("catalog is a dict");
        assert!(
            catalog.get("PageLabels").is_none(),
            "a source with no /PageLabels must not gain one"
        );
    }
}
