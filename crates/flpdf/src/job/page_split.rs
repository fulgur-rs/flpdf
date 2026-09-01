//! qpdf correspondence: `QPDFJob::doSplitPages` (`QPDFJob.cc:2940-3027`).
//!
//! The job owns split output lifecycle: every chunk starts from qpdf's
//! `emptyPDF()`, receives pages through the page-document helper, fixes copied
//! form annotations, reconstructs chunk-local labels, and is written as a
//! separate output file. The output-path naming helpers below are also
//! qpdf-private (inlined in `doSplitPages`, not a separate qpdf function), so
//! they stay `fn`-private here rather than a separate public module.
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

use super::QPDFJob;
use crate::{
    Error, PageDocumentHelper, PageInput, PageObjectHelper, Pdf, PdfWriter, Result,
    WriterConfiguration,
};
use std::path::{Path, PathBuf};

/// Configuration for [`QPDFJob::split_pages`].
#[derive(Clone, Debug)]
pub struct SplitPageOptions {
    /// Number of source pages per output chunk.
    pub chunk_size: usize,
    /// Signed qpdf job value, retained until `doSplitPages` reaches its
    /// `QIntC::to_size` conversion. This is only populated by the job-JSON
    /// boundary; the public split helper continues to accept a valid usize.
    qpdf_chunk_size: Option<i32>,
    /// qpdf output filename pattern or literal output template.
    pub output_template: PathBuf,
    /// Original input path, when available, for qpdf's same-file guard.
    pub input_path: Option<PathBuf>,
    /// Apply qpdf's deterministic-ID policy to every chunk.
    pub deterministic_id: bool,
    /// Reapply the effective qpdf writer settings to every chunk.
    pub writer_configuration: WriterConfiguration,
    /// Report each chunk's real output path as it is written.
    pub verbose: bool,
}

impl SplitPageOptions {
    /// Construct split options without an input-path identity check.
    #[must_use]
    pub fn new(chunk_size: usize, output_template: impl Into<PathBuf>) -> Self {
        Self {
            chunk_size,
            qpdf_chunk_size: None,
            output_template: output_template.into(),
            input_path: None,
            deterministic_id: false,
            writer_configuration: WriterConfiguration::default(),
            verbose: false,
        }
    }

    /// Retain qpdf's signed `splitPages` value until the split loop performs
    /// its signed-to-unsigned conversion (`libqpdf/QPDFJob.cc:2970`).
    #[must_use]
    pub(crate) fn with_qpdf_chunk_size(mut self, chunk_size: i32) -> Self {
        self.qpdf_chunk_size = Some(chunk_size);
        self
    }

    /// Attach the original input path used by qpdf's overwrite guard.
    #[must_use]
    pub fn with_input_path(mut self, input_path: impl Into<PathBuf>) -> Self {
        self.input_path = Some(input_path.into());
        self
    }

    /// Apply deterministic IDs to all split outputs.
    #[must_use]
    pub fn with_deterministic_id(mut self, deterministic_id: bool) -> Self {
        self.deterministic_id = deterministic_id;
        self
    }

    /// Reapply this writer configuration to every fresh output chunk.
    #[must_use]
    pub fn with_writer_configuration(mut self, configuration: WriterConfiguration) -> Self {
        self.writer_configuration = configuration;
        self
    }

    /// Report each chunk's real output path as it is written.
    #[must_use]
    pub fn with_verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self
    }
}

fn qpdf_split_page_size(value: i32) -> Result<usize> {
    usize::try_from(value).map_err(|_| {
        Error::System(format!(
            "integer out of range converting {value} from a {}-byte signed type to a {}-byte unsigned type",
            std::mem::size_of::<i32>(),
            std::mem::size_of::<usize>(),
        ))
    })
}

impl QPDFJob {
    /// Execute qpdf's fresh-document split-pages job.
    ///
    /// `source`'s suppress-warnings setting is only overridden for the
    /// duration of this call: the value in effect when this method was
    /// entered is restored before returning, on every path (success or
    /// error), so a `source` reused across multiple jobs is never left with
    /// a stale suppression state from an earlier call.
    pub fn split_pages<R: std::io::Read + std::io::Seek + 'static>(
        &mut self,
        source: &mut Pdf<R>,
        options: SplitPageOptions,
    ) -> Result<Vec<PathBuf>> {
        let original_suppress_warnings = source.suppress_warnings();
        // QPDFJob owns the logger and warning state for every operation. The
        // split source may have been opened outside QPDFJob (the CLI opens an
        // intermediate rewrite), so install the job logger before any page or
        // AcroForm traversal can emit a lazy warning. A source opened with
        // `PdfOpenOptions { suppress_warnings: true, .. }` independently of
        // this job keeps that suppression rather than being un-suppressed by
        // this job's own (possibly unconfigured) default.
        source.set_logger(self.logger());
        source.set_suppress_warnings(self.warnings_suppressed() || original_suppress_warnings);
        let result = self.split_pages_with_suppression_installed(source, options);
        source.set_suppress_warnings(original_suppress_warnings);
        result
    }

    fn split_pages_with_suppression_installed<R: std::io::Read + std::io::Seek + 'static>(
        &mut self,
        source: &mut Pdf<R>,
        options: SplitPageOptions,
    ) -> Result<Vec<PathBuf>> {
        if options.qpdf_chunk_size.is_none() && options.chunk_size == 0 {
            return Err(Error::Unsupported(
                "split_pages: chunk_size must be >= 1".to_owned(),
            ));
        }
        let pages = PageDocumentHelper::new(source).get_all_pages()?;
        let chunk_size = options
            .qpdf_chunk_size
            .map_or(Ok(options.chunk_size), qpdf_split_page_size)?;
        // The public constructor rejects zero before this point. The
        // qpdf-specific setter is called only for a non-zero truthy job
        // value, matching qpdf's `if (m->split_pages)` dispatch.
        debug_assert_ne!(chunk_size, 0);

        // qpdf's doSplitPages has no page-count guard: `for (i = 0; i <
        // num_pages; i += m->split_pages)` trivially iterates zero times
        // when num_pages is 0, writing no chunks (confirmed live:
        // `qpdf --empty --split-pages=1 out-%d.pdf` exits 0 with no output
        // files, as does a --collate=0-produced empty page selection).
        let page_count = pages.len();
        // cov:ignore-start: a supported process cannot allocate more than
        // u32::MAX page references for one parsed PDF.
        let width = digit_width(u32::try_from(page_count).map_err(|_| {
            Error::Unsupported("split_pages: page count exceeds qpdf's naming range".to_owned())
        })?);
        // cov:ignore-end
        let has_page_labels = source.page_labels().has_page_labels()?;
        let has_acro_form = source.acroform()?.has_acro_form()?;
        let source_version =
            crate::parse_pdf_version(source.version()).map(|version| version.get_version().0);
        let source_extension_level = source.adobe_extension_level()?.unwrap_or(0);
        let mut written = Vec::new();

        for chunk_start in (0..page_count).step_by(chunk_size) {
            let chunk_end = chunk_start.saturating_add(chunk_size).min(page_count);
            // cov:ignore-start: page_count is bounded by the allocation above,
            // so these conversions cannot fail on supported targets.
            let first_page = u32::try_from(chunk_start + 1).map_err(|_| {
                Error::Unsupported("split_pages: page number exceeds qpdf's naming range".into())
            })?;
            let last_page = u32::try_from(chunk_end).map_err(|_| {
                Error::Unsupported("split_pages: page number exceeds qpdf's naming range".into())
            })?;
            // cov:ignore-end
            let output_path = chunk_output_path(
                &options.output_template,
                first_page,
                last_page,
                width,
                chunk_size,
            );
            if let Some(input_path) = options.input_path.as_deref() {
                if same_file_if_existing(input_path, &output_path)? {
                    return Err(Error::Unsupported(format!(
                        "split pages would overwrite input file with {}",
                        output_path.display()
                    )));
                }
            }

            let mut output = Pdf::empty()?;
            for &page_ref in &pages[chunk_start..chunk_end] {
                let rebuild = PageDocumentHelper::new(&mut output)
                    .add_page(PageInput::foreign(source, page_ref), false)?;
                let new_page = rebuild
                    .new_kids
                    .last()
                    .copied()
                    .ok_or(Error::Missing("split output page"))?;

                // qpdf's doSplitPages calls fixCopiedAnnotations after every
                // foreign page copy when the source has an AcroForm. The
                // canonical PageObjectHelper facade owns the corresponding
                // field-tree copy and annotation transform.
                if has_acro_form {
                    let source_page = source.get_object_handle(page_ref);
                    PageObjectHelper::new(new_page, &mut output)
                        .fix_copied_annotations_from(source_page, source)?; // cov:ignore: malformed foreign annotation errors are covered by the canonical PageObjectHelper tests
                }
            }

            if has_page_labels {
                // cov:ignore-start: chunk indices are usize values bounded by
                // the parsed page allocation and therefore fit in i64.
                let start = i64::try_from(chunk_start).map_err(|_| {
                    Error::Unsupported("split_pages: label start exceeds i64".to_owned())
                })?;
                let end = i64::try_from(chunk_end - 1).map_err(|_| {
                    Error::Unsupported("split_pages: label end exceeds i64".to_owned())
                })?;
                // cov:ignore-end
                let entries = source
                    .page_labels()
                    .labels_for_page_range(start, end, 0)?
                    .into_iter()
                    .map(|(output_index, label)| {
                        // cov:ignore-start: labels_for_page_range returns only
                        // indices within the requested non-negative range.
                        let source_index = start.checked_add(output_index).ok_or_else(|| {
                            Error::Unsupported("split_pages: label index overflow".to_owned())
                        })?;
                        // cov:ignore-end
                        let prefix_present =
                            source.page_labels().label_prefix_is_present(source_index)?;
                        Ok((output_index, label, prefix_present))
                    })
                    .collect::<Result<Vec<_>>>()?;
                output
                    .page_labels()
                    .write_reconstructed_labels_with_prefix_presence(&entries)?;
            }

            let mut writer = PdfWriter::new(&mut output);
            options.writer_configuration.apply_to(&mut writer);
            if let Some(version) = source_version.as_deref() {
                writer.set_minimum_pdf_version(version, source_extension_level);
            }
            if options.deterministic_id {
                writer.set_deterministic_id(true);
            }
            self.configure_writer_progress(&mut writer);
            writer.set_output_file(&output_path)?;
            writer.write()?;
            // qpdf reports each chunk from inside this per-chunk loop
            // (`libqpdf/QPDFJob.cc:3019-3021`), immediately after that
            // chunk's write succeeds, so a later chunk's failure still
            // leaves the reports for every chunk written before it. Confirmed
            // live: `qpdf --verbose --split-pages=1` with a later chunk's
            // destination pre-occupied by a directory still prints "wrote
            // file" for the earlier, successfully written chunks before
            // failing.
            if options.verbose {
                let message = format!(
                    "{}: wrote file {}\n",
                    self.message_prefix(),
                    output_path.display()
                );
                self.logger().info(message)?;
            }
            written.push(output_path);
        }

        self.record_document_warnings(source);
        Ok(written)
    }
}

/// Return whether two existing paths identify the same filesystem object.
///
/// qpdf checks this before each split output is opened. Missing output paths
/// are safe and return `false`; other metadata failures retain the path
/// context instead of silently allowing a destructive write.
fn same_file_if_existing(input: &Path, output: &Path) -> Result<bool> {
    let output_metadata = match std::fs::metadata(output) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(Error::file_io("inspect split output", output, error)),
    };
    let input_metadata = match std::fs::metadata(input) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(Error::file_io("inspect split input", input, error)),
    };

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(input_metadata.dev() == output_metadata.dev()
            && input_metadata.ino() == output_metadata.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = (input_metadata, output_metadata);
        Ok(std::fs::canonicalize(input)? == std::fs::canonicalize(output)?)
    }
}

// ---------------------------------------------------------------------------
// Output naming helpers (qpdf: inlined in `doSplitPages`)
// ---------------------------------------------------------------------------

/// Compute the output path for one chunk, honoring qpdf 11.9.0's
/// `chunk_size`-dependent naming:
///
/// - `chunk_size == 1`: single-number suffix `{stem}-{page}{ext}` (every
///   chunk is exactly one page, e.g. `out-1.pdf`).
/// - `chunk_size >= 2`: range suffix `{stem}-{lo}-{hi}{ext}` via
///   [`split_output_path`], retained even for a trailing single-page chunk
///   (e.g. 3 pages `--split-pages=2` → `out-1-2.pdf`, `out-3-3.pdf`).
fn chunk_output_path(
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

/// Compute the **range-form** output path (`{stem}-{first}-{last}{ext}`) for a
/// chunk given the template and 1-based page numbers.
///
/// This is a pure function suitable for unit testing independent of PDF I/O.
/// It is the naming used by qpdf 11.9.0 for `--split-pages >= 2`. The
/// `--split-pages=1` single-number form is produced by [`chunk_output_path`],
/// so this helper keeps a stable signature and always emits the range form.
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
fn split_output_path(template: &Path, first_page: u32, last_page: u32, width: usize) -> PathBuf {
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
fn digit_width(n: u32) -> usize {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::page_refs;
    use crate::pipeline::{Pipeline, PipelineHandle, PipelineResult};
    use crate::{ObjectHandle, Pdf, PdfOpenOptions, QPDFLogger};
    use std::io::Cursor;
    use std::sync::{Arc, Mutex};

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
        assert_eq!(
            digit_width(0),
            1,
            "edge case: won't occur for real page counts"
        );
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

    fn open_fixture(name: &str) -> Pdf<Cursor<Vec<u8>>> {
        let bytes: &[u8] = match name {
            "direct-outlines.pdf" => {
                include_bytes!("../../../../tests/fixtures/json-diff/direct-outlines.pdf")
            }
            "three-page.pdf" => include_bytes!("../../../../tests/fixtures/compat/three-page.pdf"),
            "objstm-lin-acroform-widget-page1-page2.pdf" => include_bytes!(
                "../../../../tests/fixtures/compat/objstm-lin-acroform-widget-page1-page2.pdf"
            ),
            "acroform-sig-orphan-widget.pdf" => {
                include_bytes!("../../../../tests/fixtures/compat/acroform-sig-orphan-widget.pdf")
            }
            _ => panic!("fixture is not registered: {name}"),
        };
        Pdf::open_mem_owned(bytes.to_vec()).expect("fixture must parse")
    }

    fn catalog(pdf: &mut Pdf<Cursor<Vec<u8>>>) -> ObjectHandle {
        pdf.root_handle().expect("catalog resolves")
    }

    fn first_page_annotation_count(pdf: &mut Pdf<Cursor<Vec<u8>>>) -> usize {
        let page_ref = page_refs(pdf)
            .expect("page references resolve")
            .into_iter()
            .next()
            .expect("fixture has a page");
        let page = pdf.get_object_handle(page_ref);
        pdf.resolve(&page).expect("page resolves");
        if !page.has_key(b"/Annots") {
            return 0;
        }
        let annots = page.get_key(b"/Annots");
        pdf.resolve(&annots).expect("/Annots resolves");
        annots
            .as_array()
            .map(|annots| annots.len())
            .unwrap_or_else(|| panic!("/Annots is not an array")) // cov:ignore: fixture /Annots is an array
    }

    #[test]
    fn split_page_options_builder_keeps_qpdf_job_inputs() {
        let options = SplitPageOptions::new(2, "out-%d.pdf")
            .with_input_path("input.pdf")
            .with_deterministic_id(true);
        assert_eq!(options.chunk_size, 2);
        assert_eq!(options.output_template, PathBuf::from("out-%d.pdf"));
        assert_eq!(options.input_path, Some(PathBuf::from("input.pdf")));
        assert!(options.deterministic_id);
    }

    #[test]
    fn split_pages_with_static_id_produces_identical_chunk_ids_across_runs() {
        // QPDFJob::setWriterOptions applies static_id to every chunk writer
        // the same way it applies deterministic_id (QPDFJob.cc:2879-2883),
        // called once per chunk from doSplitPages (QPDFJob.cc:3021-3022).
        let run = || {
            let mut source = open_fixture("three-page.pdf");
            let temp = tempfile::tempdir().expect("tempdir");
            let mut job = QPDFJob::new();
            let mut configuration = WriterConfiguration::default();
            configuration.set_static_id(true);
            let options = SplitPageOptions::new(1, temp.path().join("chunk-%d.pdf"))
                .with_writer_configuration(configuration);
            let written = job
                .split_pages(&mut source, options)
                .expect("split succeeds");
            written
                .iter()
                .map(|path| std::fs::read(path).expect("chunk readable"))
                .collect::<Vec<_>>()
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn split_pages_records_an_orphan_widget_warning_once() {
        let mut source = open_fixture("acroform-sig-orphan-widget.pdf");
        let temp = tempfile::tempdir().expect("tempdir");
        let mut job = QPDFJob::new();
        let written = job
            .split_pages(
                &mut source,
                SplitPageOptions::new(1, temp.path().join("out.pdf")),
            )
            .expect("split job should succeed with a recoverable warning");

        assert_eq!(written.len(), 1);
        assert!(job.has_warnings());
        assert_eq!(
            source
                .repair_diagnostics()
                .entries()
                .iter()
                .filter(|entry| {
                    entry
                        .message
                        .contains("this widget annotation is not reachable from /AcroForm")
                })
                .count(),
            1
        );
    }

    struct RecordingWarningSink(Arc<Mutex<Vec<u8>>>);

    impl Pipeline for RecordingWarningSink {
        // cov:ignore-start: QPDFLogger::warn only ever calls
        // PipelineHandle::write, never identifier (used only for a
        // misconfigured-pipeline error context this test never triggers).
        fn identifier(&self) -> &str {
            "split_pages warning recording sink"
        }
        // cov:ignore-end

        fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
            self.0.lock().unwrap().extend_from_slice(data);
            Ok(())
        }

        // cov:ignore-start: QPDFLogger::warn only ever calls
        // PipelineHandle::write, never finish.
        fn finish(&mut self) -> PipelineResult<()> {
            Ok(())
        }
        // cov:ignore-end
    }

    #[test]
    fn split_pages_retains_a_source_configured_suppress_warnings_setting() {
        // A source opened independently of the job (e.g. the CLI's
        // intermediate rewrite) with `PdfOpenOptions { suppress_warnings:
        // true, .. }` must stay suppressed even when the job itself never
        // called `set_suppress_warnings`; split_pages must not silently
        // un-suppress it by overwriting with the job's own unconfigured
        // default. `split_pages` also installs the job's own logger onto
        // `source` before any traversal runs (so both must observe the same
        // sink), which is why the recording sink is attached to the job, not
        // to the source's own (about-to-be-replaced) open-time logger.
        let bytes =
            include_bytes!("../../../../tests/fixtures/compat/acroform-sig-orphan-widget.pdf")
                .to_vec();
        let mut source = Pdf::open_mem_owned_with_options(
            bytes,
            PdfOpenOptions {
                suppress_warnings: true,
                ..PdfOpenOptions::default()
            },
        )
        .expect("fixture should parse");

        let recorded = Arc::new(Mutex::new(Vec::new()));
        let logger = QPDFLogger::create();
        logger.set_warn(Some(PipelineHandle::new(RecordingWarningSink(Arc::clone(
            &recorded,
        )))));
        let temp = tempfile::tempdir().expect("tempdir");
        let mut job = QPDFJob::new();
        job.set_logger(logger);
        job.split_pages(
            &mut source,
            SplitPageOptions::new(1, temp.path().join("out.pdf")),
        )
        .expect("split job should succeed with a recoverable, suppressed warning");

        let delivered = String::from_utf8_lossy(&recorded.lock().unwrap()).into_owned();
        assert!(
            delivered.is_empty(),
            "a source opened with suppress_warnings:true must stay suppressed \
             through split_pages: {delivered:?}"
        );
    }

    #[test]
    fn split_pages_delivers_warnings_from_an_unsuppressed_source() {
        // Companion to the suppressed case above: a source opened WITHOUT
        // suppress_warnings must still have its orphan-widget warning
        // delivered to the job's logger, proving the recording sink itself
        // observes real warning traffic (not merely absence for an
        // unrelated reason).
        let bytes =
            include_bytes!("../../../../tests/fixtures/compat/acroform-sig-orphan-widget.pdf")
                .to_vec();
        let mut source = Pdf::open_mem_owned(bytes).expect("fixture should parse");

        let recorded = Arc::new(Mutex::new(Vec::new()));
        let logger = QPDFLogger::create();
        logger.set_warn(Some(PipelineHandle::new(RecordingWarningSink(Arc::clone(
            &recorded,
        )))));
        let temp = tempfile::tempdir().expect("tempdir");
        let mut job = QPDFJob::new();
        job.set_logger(logger);
        job.split_pages(
            &mut source,
            SplitPageOptions::new(1, temp.path().join("out.pdf")),
        )
        .expect("split job should succeed with a recoverable warning");

        assert!(
            String::from_utf8_lossy(&recorded.lock().unwrap())
                .contains("this widget annotation is not reachable from /AcroForm"),
            "an unsuppressed source's orphan-widget warning must reach the job's logger"
        );
    }

    #[test]
    fn split_pages_uses_a_fresh_catalog_and_preserves_explicit_empty_prefix() {
        let mut source = open_fixture("direct-outlines.pdf");
        let temp = tempfile::tempdir().expect("temporary directory");
        let options = SplitPageOptions::new(1, temp.path().join("out.pdf"));
        let mut job = super::super::QPDFJob::new();

        let written = job
            .split_pages(&mut source, options)
            .expect("split job should succeed");
        assert_eq!(written.len(), 30);

        let mut chunk =
            Pdf::open_mem_owned(std::fs::read(&written[0]).expect("first chunk should be written"))
                .expect("chunk should parse");
        let catalog = catalog(&mut chunk);
        assert!(!catalog.has_key(b"/Outlines"));
        assert!(!catalog.has_key(b"/PageMode"));

        let page_labels = catalog.get_key(b"/PageLabels");
        chunk.resolve(&page_labels).expect("PageLabels resolves");
        let nums = page_labels.get_key(b"/Nums");
        chunk.resolve(&nums).expect("PageLabels /Nums resolves");
        let labels = nums.as_array().expect("PageLabels /Nums must be an array");
        let label = labels.get(1).expect("first label dictionary");
        chunk.resolve(label).expect("label dictionary resolves");
        assert_eq!(label.get_key(b"/P").as_string(), Some(Vec::new()));
    }

    #[test]
    fn split_pages_on_an_empty_source_document_writes_no_chunks() {
        // qpdf's doSplitPages has no page-count guard (confirmed live:
        // `qpdf --empty --split-pages=1 out-%d.pdf` exits 0 with no output
        // files); split_pages must match rather than reject the input.
        let mut source = Pdf::empty().expect("empty PDF should parse");
        let temp = tempfile::tempdir().expect("temporary directory");
        let options = SplitPageOptions::new(1, temp.path().join("out.pdf"));
        let mut job = super::super::QPDFJob::new();

        let written = job
            .split_pages(&mut source, options)
            .expect("a document without pages is a no-op, not an error");
        assert!(written.is_empty());
    }

    #[test]
    #[should_panic(expected = "fixture is not registered")]
    fn fixture_helper_rejects_unknown_fixture_names() {
        let _ = open_fixture("unknown");
    }

    #[test]
    fn same_file_guard_reports_metadata_errors_and_missing_input() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let existing_output = temp.path().join("existing-output.pdf");
        std::fs::write(&existing_output, b"output").expect("output should exist");

        #[cfg(unix)]
        {
            let file_parent = temp.path().join("file-parent");
            std::fs::write(&file_parent, b"not a directory").expect("parent file should exist");

            let output_error = same_file_if_existing(&file_parent, &file_parent.join("output.pdf"))
                .expect_err("metadata below a file must fail");
            assert!(output_error.to_string().contains("inspect split output"));

            let input_error =
                same_file_if_existing(&file_parent.join("input.pdf"), &existing_output)
                    .expect_err("input metadata below a file must fail");
            assert!(input_error.to_string().contains("inspect split input"));
        }

        #[cfg(not(unix))]
        assert!(same_file_if_existing(&existing_output, &existing_output)
            .expect("canonicalize should inspect an existing Windows path"));

        let missing_input = temp.path().join("missing-input.pdf");
        assert!(!same_file_if_existing(&missing_input, &existing_output)
            .expect("a missing input is not an overwrite"));
    }

    #[test]
    fn split_pages_keeps_only_fields_reached_by_the_chunk() {
        let mut source = open_fixture("objstm-lin-acroform-widget-page1-page2.pdf");
        let temp = tempfile::tempdir().expect("temporary directory");
        let options = SplitPageOptions::new(1, temp.path().join("out.pdf"));
        let mut job = super::super::QPDFJob::new();

        let written = job
            .split_pages(&mut source, options)
            .expect("split job should succeed");
        assert_eq!(written.len(), 3);

        let mut first = Pdf::open_mem_owned(std::fs::read(&written[0]).unwrap()).unwrap();
        let mut second = Pdf::open_mem_owned(std::fs::read(&written[1]).unwrap()).unwrap();
        let mut third = Pdf::open_mem_owned(std::fs::read(&written[2]).unwrap()).unwrap();
        assert!(!first.acroform().unwrap().has_acro_form().unwrap());
        assert!(second.acroform().unwrap().has_acro_form().unwrap());
        assert_eq!(second.acroform().unwrap().fields().unwrap().len(), 1);
        assert_eq!(first_page_annotation_count(&mut first), 0);
        assert_eq!(first_page_annotation_count(&mut second), 1);
        assert_eq!(first_page_annotation_count(&mut third), 1);
    }

    #[test]
    fn split_pages_preserves_source_pdf_version_on_each_chunk() {
        let mut source = open_fixture("objstm-lin-acroform-widget-page1-page2.pdf");
        assert_eq!(source.version(), "1.5");
        let temp = tempfile::tempdir().expect("temporary directory");
        let options = SplitPageOptions::new(1, temp.path().join("out.pdf"));
        let mut job = super::super::QPDFJob::new();

        let written = job
            .split_pages(&mut source, options)
            .expect("split job should succeed");

        for path in written {
            let bytes = std::fs::read(path).expect("chunk should be readable");
            assert!(bytes.starts_with(b"%PDF-1.5\n"));
        }
    }

    #[test]
    fn split_pages_rejects_a_chunk_that_would_overwrite_the_input() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let input = temp.path().join("out-1.pdf");
        std::fs::write(
            &input,
            include_bytes!("../../../../tests/fixtures/compat/one-page.pdf"),
        )
        .expect("input should be created");
        let mut source = Pdf::open_mem_owned(std::fs::read(&input).unwrap()).unwrap();
        let options = SplitPageOptions::new(1, temp.path().join("out.pdf")).with_input_path(&input);
        let mut job = super::super::QPDFJob::new();

        let error = job
            .split_pages(&mut source, options)
            .expect_err("split must not overwrite its input");
        assert!(error.to_string().contains("overwrite input"));
    }

    #[test]
    fn split_pages_applies_qpdf_percent_d_to_the_first_placeholder() {
        let mut source = open_fixture("three-page.pdf");
        let temp = tempfile::tempdir().expect("temporary directory");
        let options = SplitPageOptions::new(2, temp.path().join("literal-%d-tail-%d.pdf"));
        let mut job = super::super::QPDFJob::new();

        let written = job
            .split_pages(&mut source, options)
            .expect("split job should succeed");
        assert_eq!(
            written,
            vec![
                temp.path().join("literal-1-2-tail-%d.pdf"),
                temp.path().join("literal-3-3-tail-%d.pdf"),
            ]
        );
    }

    // -----------------------------------------------------------------------
    // Naming, page-label, and error-propagation regression tests (moved from
    // crate::page_split's removed `split_pages` compatibility facade)
    // -----------------------------------------------------------------------

    fn split(
        src: Vec<u8>,
        chunk_size: usize,
        output_template: &Path,
        deterministic_id: bool,
    ) -> Result<Vec<PathBuf>> {
        let mut pdf = Pdf::open_mem_owned(src)?;
        super::super::QPDFJob::new().split_pages(
            &mut pdf,
            SplitPageOptions::new(chunk_size, output_template)
                .with_deterministic_id(deterministic_id),
        )
    }

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
    /// same shape [`crate::page_label_document_helper::PageLabelDocumentHelper::write_reconstructed_labels`]
    /// itself installs).
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

    /// Read the catalog's `/PageLabels /Nums` entries as `(index, label handle)`
    /// pairs, for asserting the exact reconstructed shape of a chunk's labels.
    fn read_nums(bytes: &[u8]) -> Vec<(i64, ObjectHandle)> {
        let mut pdf = Pdf::open(Cursor::new(bytes.to_vec())).expect("should parse");
        let catalog = pdf.root_handle().expect("/Root");
        let page_labels = catalog.get_key(b"/PageLabels");
        pdf.resolve(&page_labels).expect("resolve PageLabels");
        let nums = page_labels.get_key(b"/Nums");
        pdf.resolve(&nums).expect("resolve /Nums");
        nums.as_array()
            .expect("/Nums must be a direct array")
            .chunks_exact(2)
            .map(|pair| {
                let idx = pair[0]
                    .as_integer()
                    .expect("expected an integer label index");
                (idx, pair[1].clone())
            })
            .collect()
    }

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

        split(src, 1, &template, false).expect("split should succeed");

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

        split(src, 2, &template, false).expect("split should succeed");

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

        split(src, 100, &template, false).expect("split should succeed");

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

        split(src, 2, &template, false).expect("split should succeed");

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
        // `split` takes ownership, so the second run needs its own copy. The
        // clone is this test's, not the job's: a single run copies nothing.
        split(src.clone(), 1, &d1.path().join("out.pdf"), true).expect("split should succeed");
        split(src, 1, &d2.path().join("out.pdf"), true).expect("split should succeed");

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
        // job writer and must propagate out to the caller.
        let src = build_n_page_pdf(2);
        let tmpdir = tempfile::tempdir().expect("tmpdir");
        let bad = tmpdir.path().join("no_such_subdir").join("out.pdf");
        let result = split(src, 1, &bad, false);
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
        let result = split(src, 0, &template, false);
        assert!(result.is_err(), "chunk_size=0 should return an error");
    }

    #[test]
    fn split_pages_does_not_leave_suppression_sticky_across_jobs() {
        // A job's own suppress_warnings setting must only apply for the
        // duration of its own split_pages call, even when that call errors
        // out (e.g. chunk_size=0) before completing: it must not leak into
        // a later, unrelated job's call on the same reused source.
        let mut source = open_fixture("acroform-sig-orphan-widget.pdf");
        let temp = tempfile::tempdir().expect("tempdir");

        let mut suppressing_job = QPDFJob::new();
        suppressing_job.set_suppress_warnings(true);
        let error = suppressing_job
            .split_pages(
                &mut source,
                SplitPageOptions::new(0, temp.path().join("a.pdf")),
            )
            .expect_err("chunk_size=0 must still error");
        assert!(matches!(error, Error::Unsupported(_)));

        let recorded = Arc::new(Mutex::new(Vec::new()));
        let logger = QPDFLogger::create();
        logger.set_warn(Some(PipelineHandle::new(RecordingWarningSink(Arc::clone(
            &recorded,
        )))));
        let mut plain_job = QPDFJob::new();
        plain_job.set_logger(logger);
        plain_job
            .split_pages(
                &mut source,
                SplitPageOptions::new(1, temp.path().join("b.pdf")),
            )
            .expect("split job should succeed with a recoverable warning");

        assert!(
            String::from_utf8_lossy(&recorded.lock().unwrap())
                .contains("this widget annotation is not reachable from /AcroForm"),
            "a later unsuppressed job must still observe the source's warning, not inherit \
             suppression left over from an earlier, unrelated job's failed call"
        );
    }

    #[test]
    fn split_pages_reconstructs_page_labels_per_chunk() {
        // 5-page source: roman lowercase from page 0, decimal (restart at 1)
        // from page 3. split=2 → chunks [0,1], [2,3], [4,4]. Expected shape
        // verified byte-for-byte against qpdf 11.9.0 `--split-pages=2`.
        let src = build_n_page_pdf_with_pagelabels(5, "[0 << /S /r >> 3 << /S /D /St 1 >>]");
        let tmpdir = tempfile::tempdir().expect("tmpdir");
        let template = tmpdir.path().join("out.pdf");

        split(src, 2, &template, false).expect("split should succeed");

        let chunk1 = std::fs::read(tmpdir.path().join("out-1-2.pdf")).unwrap();
        let chunk2 = std::fs::read(tmpdir.path().join("out-3-4.pdf")).unwrap();
        let chunk3 = std::fs::read(tmpdir.path().join("out-5-5.pdf")).unwrap();

        let s = |name: &str| name.as_bytes().to_vec();

        let nums1 = read_nums(&chunk1);
        assert_eq!(nums1.len(), 1);
        assert_eq!(nums1[0].0, 0);
        assert_eq!(nums1[0].1.get_key(b"/S").as_name(), Some(s("r")));
        assert_eq!(nums1[0].1.get_key(b"/St").as_integer(), Some(1));

        let nums2 = read_nums(&chunk2);
        assert_eq!(nums2.len(), 2, "roman continuation + decimal restart");
        assert_eq!(nums2[0].0, 0);
        assert_eq!(nums2[0].1.get_key(b"/S").as_name(), Some(s("r")));
        assert_eq!(nums2[0].1.get_key(b"/St").as_integer(), Some(3));
        assert_eq!(nums2[1].0, 1);
        assert_eq!(nums2[1].1.get_key(b"/S").as_name(), Some(s("D")));
        assert_eq!(nums2[1].1.get_key(b"/St").as_integer(), Some(1));

        let nums3 = read_nums(&chunk3);
        assert_eq!(nums3.len(), 1);
        assert_eq!(nums3[0].0, 0);
        assert_eq!(nums3[0].1.get_key(b"/S").as_name(), Some(s("D")));
        assert_eq!(nums3[0].1.get_key(b"/St").as_integer(), Some(2));
    }

    #[test]
    fn split_pages_without_page_labels_omits_pagelabels_key() {
        let src = build_n_page_pdf(3);
        let tmpdir = tempfile::tempdir().expect("tmpdir");
        let template = tmpdir.path().join("out.pdf");
        split(src, 1, &template, false).expect("split should succeed");

        let chunk = std::fs::read(tmpdir.path().join("out-1.pdf")).unwrap();
        let mut pdf = Pdf::open(Cursor::new(chunk)).expect("should parse");
        let catalog = pdf.root_handle().unwrap();
        assert!(
            !catalog.has_key(b"/PageLabels"),
            "a source with no /PageLabels must not gain one"
        );
    }
}
