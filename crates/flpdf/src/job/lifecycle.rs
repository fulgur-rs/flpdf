//! qpdf correspondence: `QPDFJob` shared state and completion boundary.
//!
//! This module owns the state that qpdf keeps on `QPDFJob` itself rather than
//! on an individual CLI route: the message prefix, logger, progress callback,
//! warning aggregation, and the single warning-completion summary. JSON and
//! ordinary page-inspection dispatch are layered on top of this state; write,
//! page-transform, and remaining inspection consumers are later job slices.

use super::json::{write_json, JsonJobError, JsonJobOptions, JsonJobOutput};
use crate::{Pdf, PdfOpenOptions, PdfWriter, QPDFLogger, Result, Severity};
use std::cell::RefCell;
use std::io::{Cursor, Read, Seek};
use std::rc::Rc;

type ProgressHandler = Box<dyn FnMut(u8) + 'static>;
type SharedProgressHandler = Rc<RefCell<ProgressHandler>>;

/// qpdf-compatible status returned by a completed job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum JobExitCode {
    /// No warning was recorded, or warnings were explicitly configured to be
    /// exit-zero.
    Success = 0,
    /// Warnings were recorded and the job was not configured to suppress the
    /// warning exit status.
    Warning = 3,
}

impl JobExitCode {
    /// Return the process status value used by qpdf.
    #[must_use]
    pub const fn as_i32(self) -> i32 {
        self as i32
    }
}

/// Shared qpdf-shaped state for the production job lifecycle.
///
/// qpdf's `QPDFJob` keeps this state across setup, document creation, and
/// output/inspection. The operation-specific stages are intentionally not
/// duplicated here; they consume this one state object so warning summaries
/// and progress callbacks cannot diverge between CLI and library consumers.
pub struct QPDFJob {
    logger: QPDFLogger,
    message_prefix: String,
    warnings: bool,
    suppress_warnings: bool,
    warnings_exit_zero: bool,
    progress_handler: Option<SharedProgressHandler>,
}

impl Default for QPDFJob {
    fn default() -> Self {
        Self::new()
    }
}

impl QPDFJob {
    /// Construct a job with qpdf's default message prefix and logger.
    ///
    /// Corresponds to `QPDFJob::QPDFJob` (`libqpdf/QPDFJob.cc:290-293`), whose
    /// `Members` default-constructs the shared logger
    /// (`libqpdf/QPDFJob.cc:286-289`); the remaining field defaults are the
    /// `Members` in-class initializers in qpdf 11.9.0
    /// (`include/qpdf/QPDFJob.hh:588-601`).
    #[must_use]
    pub fn new() -> Self {
        Self {
            logger: QPDFLogger::default_logger(),
            message_prefix: "qpdf".to_owned(),
            warnings: false,
            suppress_warnings: false,
            warnings_exit_zero: false,
            progress_handler: None,
        }
    }

    /// Return the logger shared by this job and documents it creates.
    #[must_use]
    pub fn logger(&self) -> QPDFLogger {
        self.logger.clone()
    }

    /// Replace the logger used for subsequent job and document output.
    pub fn set_logger(&mut self, logger: QPDFLogger) {
        self.logger = logger;
    }

    /// Set the prefix used for job-generated diagnostics.
    ///
    /// Mirrors `QPDFJob::setMessagePrefix` (`QPDFJob.cc:303-307`).
    pub fn set_message_prefix(&mut self, message_prefix: impl Into<String>) {
        self.message_prefix = message_prefix.into();
    }

    /// Return the current diagnostic prefix.
    #[must_use]
    pub fn message_prefix(&self) -> &str {
        &self.message_prefix
    }

    /// Register qpdf's progress callback for writers configured by this job.
    ///
    /// The callback is shared rather than moved into one writer so the same
    /// job can configure multiple output stages while retaining one callback
    /// registration. The writer owns the qpdf event accounting and invokes
    /// this callback only after its internal borrow is released.
    pub fn register_progress_reporter<F>(&mut self, reporter: F)
    where
        F: FnMut(u8) + 'static,
    {
        self.progress_handler = Some(Rc::new(RefCell::new(Box::new(reporter))));
    }

    /// Attach the registered progress reporter to one qpdf-shaped writer.
    pub fn configure_writer_progress<R>(&self, writer: &mut PdfWriter<'_, R>)
    where
        R: Read + Seek + 'static,
    {
        let Some(reporter) = self.progress_handler.as_ref() else {
            return;
        };
        let reporter = Rc::clone(reporter);
        writer.register_progress_reporter(Box::new(move |percent| {
            (reporter.borrow_mut())(percent);
        }));
    }

    /// Create a complete JSON-input document with this job's logger already
    /// installed.
    ///
    /// qpdf creates the rootless document and imports JSON before any later
    /// transformations (`QPDFJob.cc:429-482`, `QPDFJob.cc:1708`). Installing
    /// the logger in the seed options preserves import-time warning routing;
    /// replacing it after `Pdf::create_from_json` would lose that boundary.
    pub fn create_from_json<S>(
        &mut self,
        source: S,
        input_name: impl Into<String>,
    ) -> Result<Pdf<Cursor<Vec<u8>>>>
    where
        S: Read + Seek + 'static,
    {
        let pdf = Pdf::create_from_json_with_options(
            source,
            input_name,
            PdfOpenOptions {
                logger: Some(self.logger.clone()),
                ..PdfOpenOptions::default()
            },
        )?;
        self.record_document_warnings(&pdf);
        Ok(pdf)
    }

    /// Open a file-backed document with this job's logger installed before
    /// parsing begins.
    ///
    /// This is the ordinary-input half of qpdf's `createQPDF` boundary:
    /// `QPDFJob` applies its document options before `processFile` can emit
    /// repair diagnostics (`QPDFJob.cc:429-462`). The caller supplies policy
    /// options such as repair, weak-crypto allowance, and warning suppression;
    /// the job owns the logger and qpdf-shaped input description.
    pub fn open<R>(
        &mut self,
        source: R,
        input_name: impl Into<String>,
        mut options: PdfOpenOptions,
    ) -> Result<Pdf<R>>
    where
        R: Read + Seek,
    {
        options.logger = Some(self.logger.clone());
        options.description = input_name.into();
        let pdf = Pdf::open_with_options(source, options)?;
        self.record_document_warnings(&pdf);
        Ok(pdf)
    }

    /// Apply a partial JSON update before the job's output or inspection
    /// stage, matching qpdf's update-before-transform order.
    pub fn update_from_json<R, S>(
        &mut self,
        pdf: &mut Pdf<R>,
        source: S,
        input_name: impl Into<String>,
    ) -> Result<()>
    where
        R: Read + Seek,
        S: Read + Seek + 'static,
    {
        pdf.set_logger(self.logger.clone());
        pdf.update_from_json(source, input_name)?;
        self.record_document_warnings(pdf);
        Ok(())
    }

    /// Run one read-only consumer and complete the shared warning/status
    /// boundary after it has finished.
    ///
    /// This mirrors `QPDFJob::writeQPDF` selecting `doInspection` when no
    /// output is created (`QPDFJob.cc:484-516,1646-1693`). The callback owns
    /// the inspection-specific output; the job owns logger identity, lazy
    /// warning collection, and the final status.
    pub fn inspect<R, F, E>(
        &mut self,
        pdf: &mut Pdf<R>,
        inspection: F,
    ) -> std::result::Result<JobExitCode, E>
    where
        R: Read + Seek,
        F: FnOnce(&mut Pdf<R>) -> std::result::Result<(), E>,
        E: From<crate::Error>,
    {
        pdf.set_logger(self.logger.clone());
        inspection(pdf)?;
        self.record_document_warnings(pdf);
        Ok(self.complete(false)?)
    }

    /// Serialize one already-created document and then complete the shared
    /// warning/exit-status boundary.
    ///
    /// JSON construction and stream side-file handling remain owned by the
    /// existing serializer; this method owns only the qpdf `writeQPDF`
    /// lifecycle boundary (`QPDFJob.cc:484-563`).
    pub fn write_json<R>(
        &mut self,
        pdf: &mut Pdf<R>,
        options: JsonJobOptions<'_>,
        output: JsonJobOutput<'_>,
        creates_output: bool,
    ) -> std::result::Result<JobExitCode, JsonJobError>
    where
        R: Read + Seek,
    {
        pdf.set_logger(self.logger.clone());
        write_json(pdf, options, output)?;
        self.record_document_warnings(pdf);
        Ok(self.complete(creates_output)?)
    }

    /// Record that a stage observed one or more qpdf warnings.
    pub fn record_warnings(&mut self) {
        self.warnings = true;
    }

    /// Record warnings from a parsed document's diagnostic collection.
    pub fn record_document_warnings<R>(&mut self, pdf: &Pdf<R>)
    where
        R: Read + Seek,
    {
        if pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|entry| entry.severity == Severity::Warning)
        {
            self.record_warnings();
        }
    }

    /// Return whether any stage has recorded a warning.
    #[must_use]
    pub fn has_warnings(&self) -> bool {
        self.warnings
    }

    /// Suppress the warning completion message while retaining diagnostics.
    pub fn set_suppress_warnings(&mut self, value: bool) {
        self.suppress_warnings = value;
    }

    /// Configure qpdf's `warnings-exit-0` behavior.
    pub fn set_warnings_exit_zero(&mut self, value: bool) {
        self.warnings_exit_zero = value;
    }

    /// Complete the shared warning boundary after output or inspection.
    ///
    /// This mirrors `QPDFJob::writeQPDF` and `getExitCode`: all operation
    /// output must be completed by the caller before this method is invoked;
    /// this method emits at most the one qpdf-shaped summary and returns the
    /// corresponding status (`QPDFJob.cc:484-563`).
    pub fn complete(&self, creates_output: bool) -> Result<JobExitCode> {
        if self.warnings && !self.suppress_warnings {
            let suffix = if creates_output {
                "; resulting file may have some problems"
            } else {
                ""
            };
            self.logger.warn(format!(
                "{}: operation succeeded with warnings{suffix}\n",
                self.message_prefix
            ))?;
        }

        if self.warnings && !self.warnings_exit_zero {
            Ok(JobExitCode::Warning)
        } else {
            Ok(JobExitCode::Success)
        }
    }
}
