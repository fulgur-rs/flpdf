//! qpdf correspondence: `QPDFJob::doCheck` and `QPDFJob::doInspection` (`libqpdf/QPDFJob.cc:745-803,1646-1693`).
//!
//! The document-check consumer owns the full read-only traversal performed by
//! qpdf. The CLI only selects this operation; it does not own a second check
//! report or warning-completion path.

use super::lifecycle::{JobExitCode, QPDFJob};
use crate::content_stream::{ObjectHandleParserCallbacks, ParseControl};
use crate::linearization::{check_linearization, LinearizationCheckError};
use crate::pipeline::Discard;
use crate::{DecodeLevel, PageDocumentHelper, PageObjectHelper, Pdf, PdfWriter};
use crate::{ObjectHandle, QPDFLogger, Result, Severity};
use std::fmt;
use std::io::{Read, Seek};

/// Failure returned by the qpdf-shaped document-check consumer.
#[derive(Debug)]
pub enum CheckError {
    /// The document opened, but qpdf's full check traversal found an error.
    ErrorsDetected,
    /// The check could not complete because the logger or document operation
    /// itself failed.
    Operation(crate::Error),
}

impl fmt::Display for CheckError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ErrorsDetected => formatter.write_str("errors detected"),
            Self::Operation(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for CheckError {}

impl From<crate::Error> for CheckError {
    fn from(error: crate::Error) -> Self {
        Self::Operation(error)
    }
}

struct CheckOutcome {
    warnings: bool,
}

/// A qpdf `DiscardContents` equivalent for the canonical ObjectHandle parser.
struct DiscardContents;

impl ObjectHandleParserCallbacks for DiscardContents {
    fn handle_object(
        &mut self,
        _object: ObjectHandle,
        _offset: usize,
        _length: usize,
    ) -> Result<ParseControl> {
        Ok(ParseControl::Continue)
    }

    fn handle_eof(&mut self) -> Result<()> {
        Ok(())
    }
}

impl QPDFJob {
    /// Replay qpdf repair diagnostics retained by a failed permissive open.
    ///
    /// `QPDF::processFile` emits these warnings before returning its terminal
    /// open error (`libqpdf/QPDF.cc:1550-1595`). The resolver retains them in
    /// [`crate::Error::OpenFailure`] when the caller suppresses live delivery;
    /// this job-owned adapter restores the same logger boundary for `--check`
    /// without making a second parse of the input.
    pub fn report_open_failure(&self, error: &crate::Error) -> Result<()> {
        let Some((_, diagnostics)) = error.open_failure() else {
            return Ok(());
        };
        let logger = self.logger();
        let message_prefix = self.message_prefix().to_owned();
        let input_name = self.input_name().to_owned();
        emit_diagnostics(diagnostics, 0, &logger, &message_prefix, &input_name)?;
        Ok(())
    }

    /// Run qpdf's document-check consumer and complete the shared inspection
    /// warning/exit-status boundary.
    pub fn check<R: Read + Seek + 'static>(
        &mut self,
        pdf: &mut Pdf<R>,
    ) -> std::result::Result<JobExitCode, CheckError> {
        let logger = self.logger();
        let input_name = self.input_name().to_owned();
        let message_prefix = self.message_prefix().to_owned();

        pdf.set_logger(logger.clone());
        let outcome = check_document(pdf, &logger, &message_prefix, &input_name)?;
        self.record_document_warnings(pdf);
        if outcome.warnings {
            self.record_warnings();
        }
        Ok(self.complete(false)?)
    }
}

fn check_document<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    logger: &QPDFLogger,
    message_prefix: &str,
    input_name: &str,
) -> std::result::Result<CheckOutcome, CheckError> {
    let mut warnings = false;
    let mut diagnostics_seen = 0;

    if pdf.root_ref().is_none() {
        emit_error(
            logger,
            message_prefix,
            input_name,
            &"unable to find /Root dictionary",
        )?;
        return Err(CheckError::ErrorsDetected);
    }

    logger.info(format!("checking {input_name}\n"))?;
    let extension_level = pdf.adobe_extension_level();
    match extension_level {
        Some(level) => logger.info(format!(
            "PDF Version: {} extension level {level}\n",
            pdf.version()
        ))?,
        None => logger.info(format!("PDF Version: {}\n", pdf.version()))?,
    }
    logger.info(if pdf.is_encrypted() {
        "File is encrypted\n"
    } else {
        "File is not encrypted\n"
    })?;

    let linearized = match pdf.is_linearized() {
        Ok(value) => value,
        Err(error) => {
            emit_error(logger, message_prefix, input_name, &error)?;
            return Err(CheckError::ErrorsDetected);
        }
    };
    if linearized {
        logger.info("File is linearized\n")?;
        let source_bytes = match pdf.source_bytes() {
            Ok(bytes) => bytes,
            Err(error) => {
                warnings = true;
                emit_warning(
                    logger,
                    input_name,
                    format!("error encountered while checking linearization data: {error}"),
                )?;
                Vec::new()
            }
        };
        if !source_bytes.is_empty() {
            match check_linearization(pdf, &source_bytes) {
                Ok(()) | Err(LinearizationCheckError::NotLinearized) => {}
                Err(error) => {
                    warnings = true;
                    emit_warning(
                        logger,
                        input_name,
                        format!("error encountered while checking linearization data: {error}"),
                    )?;
                }
            }
        }
    } else {
        logger.info("File is not linearized\n")?;
    }

    let (new_warnings, new_errors) =
        emit_new_diagnostics(pdf, diagnostics_seen, logger, message_prefix, input_name)?;
    warnings |= new_warnings;
    diagnostics_seen = pdf.repair_diagnostics().entries().len();
    if new_errors {
        return Err(CheckError::ErrorsDetected);
    }

    let writer_result = (|| -> Result<()> {
        let mut writer = PdfWriter::new(pdf);
        writer.set_output_pipeline(Discard)?;
        writer.set_decode_level(DecodeLevel::All);
        writer.write()
    })();
    if let Err(error) = writer_result {
        emit_error(logger, message_prefix, input_name, &error)?;
        return Err(CheckError::ErrorsDetected);
    }

    let (new_warnings, new_errors) =
        emit_new_diagnostics(pdf, diagnostics_seen, logger, message_prefix, input_name)?;
    warnings |= new_warnings;
    diagnostics_seen = pdf.repair_diagnostics().entries().len();
    if new_errors {
        return Err(CheckError::ErrorsDetected);
    }

    let pages = PageDocumentHelper::new(pdf)
        .get_all_pages()
        .map_err(|error| {
            emit_error(logger, message_prefix, input_name, &error)
                .map(|_| CheckError::ErrorsDetected)
                .unwrap_or(CheckError::Operation(error))
        })?;
    let mut page_errors = false;
    for (index, page_ref) in pages.into_iter().enumerate() {
        let mut page = PageObjectHelper::new(page_ref, pdf);
        let mut discard_contents = DiscardContents;
        if let Err(error) = page.parse_page_contents(&mut discard_contents) {
            page_errors = true;
            logger.error(format!("ERROR: page {}: {error}\n", index + 1))?;
        }
    }
    if page_errors {
        return Err(CheckError::ErrorsDetected);
    }

    let (new_warnings, new_errors) =
        emit_new_diagnostics(pdf, diagnostics_seen, logger, message_prefix, input_name)?;
    warnings |= new_warnings;
    if new_errors {
        return Err(CheckError::ErrorsDetected);
    }

    if !warnings {
        logger.info(format!(
            "No syntax or stream encoding errors found; the file may still contain\nerrors that {message_prefix} cannot detect\n"
        ))?;
    }

    Ok(CheckOutcome { warnings })
}

fn emit_new_diagnostics<R: Read + Seek>(
    pdf: &Pdf<R>,
    seen: usize,
    logger: &QPDFLogger,
    message_prefix: &str,
    input_name: &str,
) -> std::result::Result<(bool, bool), CheckError> {
    let diagnostics = pdf.repair_diagnostics();
    Ok(emit_diagnostics(
        &diagnostics,
        seen,
        logger,
        message_prefix,
        input_name,
    )?)
}

fn emit_diagnostics(
    diagnostics: &crate::Diagnostics,
    seen: usize,
    logger: &QPDFLogger,
    message_prefix: &str,
    input_name: &str,
) -> Result<(bool, bool)> {
    let mut warnings = false;
    let mut errors = false;
    for diagnostic in diagnostics.entries().iter().skip(seen) {
        match diagnostic.severity {
            Severity::Warning => {
                warnings = true;
                let location = diagnostic_location(input_name, diagnostic);
                let separator = if diagnostic.message.starts_with("(object ")
                    || diagnostic.message.starts_with("(trailer,")
                {
                    " "
                } else {
                    ": "
                };
                logger.warn(format!(
                    "WARNING: {location}{separator}{}\n",
                    diagnostic.message
                ))?;
            }
            Severity::Error => {
                errors = true;
                emit_error_diagnostic(logger, message_prefix, input_name, diagnostic)?;
            }
        }
    }
    Ok((warnings, errors))
}

fn emit_warning(logger: &QPDFLogger, input_name: &str, message: impl AsRef<str>) -> Result<()> {
    let message = message.as_ref();
    logger.warn(format!("WARNING: {input_name}: {message}\n"))
}

fn diagnostic_location(input_name: &str, diagnostic: &crate::Diagnostic) -> String {
    if diagnostic.message.starts_with("(object ") || diagnostic.message.starts_with("(trailer,") {
        input_name.to_owned()
    } else {
        match diagnostic.offset {
            Some(offset) => format!("{input_name} (offset {offset})"),
            None => input_name.to_owned(),
        }
    }
}

fn emit_error_diagnostic(
    logger: &QPDFLogger,
    message_prefix: &str,
    input_name: &str,
    diagnostic: &crate::Diagnostic,
) -> Result<()> {
    logger.error(format!(
        "{message_prefix}: {}: {}\n",
        diagnostic_location(input_name, diagnostic),
        diagnostic.message
    ))
}

fn emit_error(
    logger: &QPDFLogger,
    message_prefix: &str,
    input_name: &str,
    error: &impl fmt::Display,
) -> Result<()> {
    logger.error(format!("{message_prefix}: {input_name}: {error}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::{Pipeline, PipelineHandle, PipelineResult};
    use crate::{PdfOpenOptions, QPDFLogger};
    use std::io::Cursor;
    use std::sync::{Arc, Mutex};

    struct Capture {
        bytes: Arc<Mutex<Vec<u8>>>,
    }

    impl Pipeline for Capture {
        fn identifier(&self) -> &str {
            "check test capture"
        }

        fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
            self.bytes
                .lock()
                .expect("capture mutex")
                .extend_from_slice(data);
            Ok(())
        }

        fn finish(&mut self) -> PipelineResult<()> {
            Ok(())
        }
    }

    #[test]
    fn clean_document_check_uses_the_shared_job_info_pipeline() {
        let info = Arc::new(Mutex::new(Vec::new()));
        let logger = QPDFLogger::create();
        logger.set_output_streams(
            Some(PipelineHandle::new(Capture {
                bytes: Arc::clone(&info),
            })),
            None,
        );
        let mut job = QPDFJob::new();
        job.set_logger(logger);
        job.set_message_prefix("qpdf");
        let mut pdf = job
            .open(
                Cursor::new(include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../tests/fixtures/minimal.pdf"
                ))),
                "minimal.pdf",
                PdfOpenOptions::default(),
            )
            .expect("open fixture");

        let status = job.check(&mut pdf).expect("check should succeed");

        assert_eq!(status, JobExitCode::Success);
        assert_eq!(
            *info.lock().expect("info capture"),
            b"checking minimal.pdf\nPDF Version: 1.7\nFile is not encrypted\nFile is not linearized\nNo syntax or stream encoding errors found; the file may still contain\nerrors that qpdf cannot detect\n"
        );
    }

    #[test]
    fn check_source_bytes_excludes_the_non_pdf_prefix() {
        let document = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/minimal.pdf"
        ));
        let mut source = b"leading material\n".to_vec();
        source.extend_from_slice(document);
        let pdf = Pdf::open(Cursor::new(source)).expect("prefixed fixture should open");

        assert_eq!(
            pdf.source_bytes().expect("source bytes should be readable"),
            document.as_slice()
        );
    }

    #[test]
    fn failed_open_warnings_replay_through_the_job_logger() {
        let warnings = Arc::new(Mutex::new(Vec::new()));
        let logger = QPDFLogger::create();
        logger.set_output_streams(
            None,
            Some(PipelineHandle::new(Capture {
                bytes: Arc::clone(&warnings),
            })),
        );

        let mut job = QPDFJob::new();
        job.set_logger(logger);
        job.set_message_prefix("qpdf");
        let options = PdfOpenOptions {
            suppress_warnings: true,
            ..PdfOpenOptions::default()
        };
        let source = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/test_driver/open_repair_failure.pdf"
        ))
        .to_vec();
        let error = match job.open(Cursor::new(source), "open-repair-failure.pdf", options) {
            Ok(_) => panic!("fixture must fail during repair"),
            Err(error) => error,
        };

        job.report_open_failure(&error)
            .expect("replaying open warnings should succeed");

        assert_eq!(
            *warnings.lock().expect("warning capture"),
            b"WARNING: open-repair-failure.pdf: file is damaged\n\
WARNING: open-repair-failure.pdf: can't find startxref\n\
WARNING: open-repair-failure.pdf: Attempting to reconstruct cross-reference table\n"
        );
    }
}
