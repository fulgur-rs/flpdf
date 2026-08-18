//! qpdf correspondence: `QPDFJob::doSplitPages` (`QPDFJob.cc:2940-3027`).
//!
//! The job owns split output lifecycle: every chunk starts from qpdf's
//! `emptyPDF()`, receives pages through the page-document helper, fixes copied
//! form annotations, reconstructs chunk-local labels, and is written as a
//! separate output file.

use super::QPDFJob;
use crate::page_split::{chunk_output_path, digit_width};
use crate::{
    Error, Matrix, PageDocumentHelper, PageInput, PageObjectHelper, Pdf, PdfWriter, Result,
};
use std::path::{Path, PathBuf};

/// Configuration for [`QPDFJob::split_pages`].
#[derive(Clone, Debug)]
pub struct SplitPageOptions {
    /// Number of source pages per output chunk.
    pub chunk_size: usize,
    /// qpdf output filename pattern or literal output template.
    pub output_template: PathBuf,
    /// Original input path, when available, for qpdf's same-file guard.
    pub input_path: Option<PathBuf>,
    /// Apply qpdf's deterministic-ID policy to every chunk.
    pub deterministic_id: bool,
    /// Apply qpdf's static-ID policy to every chunk.
    pub static_id: bool,
}

impl SplitPageOptions {
    /// Construct split options without an input-path identity check.
    #[must_use]
    pub fn new(chunk_size: usize, output_template: impl Into<PathBuf>) -> Self {
        Self {
            chunk_size,
            output_template: output_template.into(),
            input_path: None,
            deterministic_id: false,
            static_id: false,
        }
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

    /// Apply a static ID to all split outputs.
    #[must_use]
    pub fn with_static_id(mut self, static_id: bool) -> Self {
        self.static_id = static_id;
        self
    }
}

impl QPDFJob {
    /// Execute qpdf's fresh-document split-pages job.
    pub fn split_pages<R: std::io::Read + std::io::Seek + 'static>(
        &mut self,
        source: &mut Pdf<R>,
        options: SplitPageOptions,
    ) -> Result<Vec<PathBuf>> {
        if options.chunk_size == 0 {
            return Err(Error::Unsupported(
                "split_pages: chunk_size must be >= 1".to_owned(),
            ));
        }

        let pages = PageDocumentHelper::new(source).get_all_pages()?;
        if pages.is_empty() {
            return Err(Error::Missing("document has no pages"));
        }
        let page_count = pages.len();
        // cov:ignore-start: a supported process cannot allocate more than
        // u32::MAX page references for one parsed PDF.
        let width = digit_width(u32::try_from(page_count).map_err(|_| {
            Error::Unsupported("split_pages: page count exceeds qpdf's naming range".to_owned())
        })?);
        // cov:ignore-end
        let has_page_labels = source.page_labels().has_page_labels()?;
        let has_acro_form = source.acroform().has_acro_form()?;
        let mut written = Vec::new();

        for chunk_start in (0..page_count).step_by(options.chunk_size) {
            let chunk_end = chunk_start
                .saturating_add(options.chunk_size)
                .min(page_count);
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
                options.chunk_size,
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
                    PageObjectHelper::new(new_page, &mut output).copy_annotations_from(
                        source_page,
                        Matrix::default(),
                        source,
                    )?; // cov:ignore: malformed foreign annotation errors are covered by the canonical PageObjectHelper tests
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
            if options.deterministic_id {
                writer.set_deterministic_id(true);
            }
            if options.static_id {
                writer.set_static_id(true);
            }
            self.configure_writer_progress(&mut writer);
            writer.set_output_file(&output_path)?;
            writer.write()?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Object, Pdf};
    use std::io::Cursor;

    fn open_fixture(name: &str) -> Pdf<Cursor<Vec<u8>>> {
        let bytes: &[u8] = match name {
            "direct-outlines.pdf" => {
                include_bytes!("../../../../tests/fixtures/json-diff/direct-outlines.pdf")
            }
            "three-page.pdf" => include_bytes!("../../../../tests/fixtures/compat/three-page.pdf"),
            "objstm-lin-acroform-widget-page1-page2.pdf" => include_bytes!(
                "../../../../tests/fixtures/compat/objstm-lin-acroform-widget-page1-page2.pdf"
            ),
            _ => panic!("fixture is not registered: {name}"),
        };
        Pdf::open_mem_owned(bytes.to_vec()).expect("fixture must parse")
    }

    fn catalog(pdf: &mut Pdf<Cursor<Vec<u8>>>) -> crate::Dictionary {
        let root = pdf.root_ref().expect("fixture has /Root");
        pdf.resolve(root)
            .expect("catalog resolves")
            .into_dict()
            .expect("catalog is a dictionary")
    }

    #[test]
    fn split_page_options_builder_keeps_qpdf_job_inputs() {
        let options = SplitPageOptions::new(2, "out-%d.pdf")
            .with_input_path("input.pdf")
            .with_deterministic_id(true)
            .with_static_id(true);
        assert_eq!(options.chunk_size, 2);
        assert_eq!(options.output_template, PathBuf::from("out-%d.pdf"));
        assert_eq!(options.input_path, Some(PathBuf::from("input.pdf")));
        assert!(options.deterministic_id);
        assert!(options.static_id);
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
            let options =
                SplitPageOptions::new(1, temp.path().join("chunk-%d.pdf")).with_static_id(true);
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
        assert!(catalog.get("Outlines").is_none());
        assert!(catalog.get("PageMode").is_none());

        let Object::Dictionary(page_labels) = catalog
            .get("PageLabels")
            .expect("chunk should retain page labels")
        else {
            panic!("PageLabels must be a dictionary"); // cov:ignore: qpdf fixture shape is asserted by this test
        };
        let Object::Array(nums) = page_labels.get("Nums").expect("labels have /Nums") else {
            panic!("PageLabels /Nums must be an array"); // cov:ignore: qpdf fixture shape is asserted by this test
        };
        let Object::Dictionary(label) = nums.get(1).expect("first label dictionary") else {
            panic!("first label must be a dictionary"); // cov:ignore: qpdf fixture shape is asserted by this test
        };
        assert_eq!(label.get("P"), Some(&Object::String(Vec::new())));
    }

    #[test]
    fn split_pages_rejects_an_empty_source_document() {
        let mut source = Pdf::empty().expect("empty PDF should parse");
        let temp = tempfile::tempdir().expect("temporary directory");
        let options = SplitPageOptions::new(1, temp.path().join("out.pdf"));
        let mut job = super::super::QPDFJob::new();

        let error = job
            .split_pages(&mut source, options)
            .expect_err("a document without pages must be rejected");
        assert!(error.to_string().contains("document has no pages"));
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
        assert!(!first.acroform().has_acro_form().unwrap());
        assert!(second.acroform().has_acro_form().unwrap());
        assert_eq!(second.acroform().fields().unwrap().len(), 1);
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
}
