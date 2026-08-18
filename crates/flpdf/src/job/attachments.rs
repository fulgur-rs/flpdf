//! qpdf correspondence: `QPDFJob::doListAttachments` and `QPDFJob::doShowAttachment` (`libqpdf/QPDFJob.cc:876-927`).

use super::lifecycle::{JobExitCode, QPDFJob};
use crate::attachment_list::format_attachment_list_with_sink;
use crate::filespec_helper::extract_attachment;
use crate::{Error, Pdf, Result};
use std::io::{Read, Seek};

impl QPDFJob {
    /// List embedded files through the shared qpdf info pipeline.
    ///
    /// `QPDFJob::doListAttachments` owns the output and warning lifecycle,
    /// while `QPDFEmbeddedFileDocumentHelper` and the FileSpec/EF helpers own
    /// name-tree traversal and metadata projection. The existing
    /// [`format_attachment_list_with_sink`] implementation remains the one
    /// attachment traversal route.
    pub fn list_attachments<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        verbose: bool,
    ) -> Result<JobExitCode> {
        let logger = self.logger();
        let input_name = self.input_name().to_owned();
        self.inspect(pdf, |pdf| {
            let listing = format_attachment_list_with_sink(pdf, verbose, |data| logger.info(data))?;
            if listing.is_none() {
                logger.info(format!("{input_name} has no embedded files\n"))?;
            }
            Ok(())
        })
    }

    /// Show one embedded file through the shared qpdf save pipeline.
    ///
    /// The attachment is resolved before `save_to_standard_output`, matching
    /// qpdf's `doShowAttachment` order: a missing key is a fatal inspection
    /// error and does not claim the standard-output save route
    /// (`libqpdf/QPDFJob.cc:916-927`).
    pub fn show_attachment<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        key: &[u8],
    ) -> Result<JobExitCode> {
        let logger = self.logger();
        self.inspect(pdf, |pdf| {
            let bytes = extract_attachment(pdf, key)?;
            logger.save_to_standard_output(true)?;
            let save = logger.get_save()?;
            save.write(&bytes).map_err(Error::from)?;
            save.finish().map_err(Error::from)?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::{JobExitCode, QPDFJob};
    use crate::pipeline::{Pipeline, PipelineError, PipelineHandle, PipelineResult};
    use crate::{PdfOpenOptions, QPDFLogger};
    use std::io::Cursor;
    use std::sync::{Arc, Mutex};

    struct Capture {
        bytes: Arc<Mutex<Vec<u8>>>,
    }

    type CaptureBytes = Arc<Mutex<Vec<u8>>>;

    impl Pipeline for Capture {
        fn identifier(&self) -> &str {
            "attachment test capture"
        }

        fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
            self.bytes
                .lock()
                .map_err(|_| PipelineError::runtime("capture mutex poisoned"))?
                .extend_from_slice(data);
            Ok(())
        }

        fn finish(&mut self) -> PipelineResult<()> {
            Ok(())
        }
    }

    #[test]
    fn capture_pipeline_exposes_its_identifier() {
        let capture = Capture {
            bytes: Arc::new(Mutex::new(Vec::new())),
        };
        assert_eq!(Pipeline::identifier(&capture), "attachment test capture");
    }

    fn job_with_captures() -> (QPDFJob, CaptureBytes, CaptureBytes) {
        let info = Arc::new(Mutex::new(Vec::new()));
        let save = Arc::new(Mutex::new(Vec::new()));
        let logger = QPDFLogger::create();
        logger.set_output_streams(
            Some(PipelineHandle::new(Capture {
                bytes: Arc::clone(&info),
            })),
            None,
        );
        logger
            .set_save(
                Some(PipelineHandle::new(Capture {
                    bytes: Arc::clone(&save),
                })),
                false,
            )
            .expect("capture save sink");
        let mut job = QPDFJob::new();
        job.set_logger(logger);
        (job, info, save)
    }

    #[test]
    fn list_attachments_owns_no_embedded_files_message_and_completion() {
        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/minimal.pdf"
        ))
        .to_vec();
        let (mut job, info, _) = job_with_captures();
        let mut pdf = job
            .open(Cursor::new(bytes), "minimal.pdf", PdfOpenOptions::default())
            .expect("open fixture");

        let status = job
            .list_attachments(&mut pdf, false)
            .expect("list attachments");

        assert_eq!(status, JobExitCode::Success);
        assert_eq!(
            *info.lock().expect("info capture"),
            b"minimal.pdf has no embedded files\n"
        );
    }

    #[test]
    fn show_attachment_does_not_use_save_sink_when_key_is_missing() {
        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/minimal.pdf"
        ));
        let (mut job, _, save) = job_with_captures();
        let mut pdf = job
            .open(
                Cursor::new(bytes.as_slice()),
                "minimal.pdf",
                PdfOpenOptions::default(),
            )
            .expect("open fixture");

        let error = job
            .show_attachment(&mut pdf, b"missing")
            .expect_err("missing attachment must fail");

        assert!(error.to_string().contains("not found"));
        assert!(save.lock().expect("save capture").is_empty());
    }

    #[test]
    fn list_attachments_writes_qpdf_header_to_job_info_pipeline() {
        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/compat/attachment-two-page.pdf"
        ));
        let (mut job, info, _) = job_with_captures();
        let mut pdf = job
            .open(
                Cursor::new(bytes.to_vec()),
                "attachment-two-page.pdf",
                PdfOpenOptions::default(),
            )
            .expect("open fixture");

        assert_eq!(
            job.list_attachments(&mut pdf, false)
                .expect("list attachments"),
            JobExitCode::Success
        );
        assert_eq!(
            *info.lock().expect("info capture"),
            b"attachment.txt -> 8,0\n"
        );
    }

    #[test]
    fn show_attachment_writes_decoded_bytes_to_job_save_pipeline() {
        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/compat/attachment-two-page.pdf"
        ));
        let (mut job, _, save) = job_with_captures();
        let mut pdf = job
            .open(
                Cursor::new(bytes.to_vec()),
                "attachment-two-page.pdf",
                PdfOpenOptions::default(),
            )
            .expect("open fixture");

        assert_eq!(
            job.show_attachment(&mut pdf, b"attachment.txt")
                .expect("show attachment"),
            JobExitCode::Success
        );
        assert_eq!(
            *save.lock().expect("save capture"),
            b"This is a small text attachment for PDF fixture testing.\nGenerated by flpdf test corpus setup.\n"
        );
    }
}
