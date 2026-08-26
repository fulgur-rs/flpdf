//! qpdf correspondence: `QPDFJob` shared state and completion boundary.
//!
//! This module owns the state that qpdf keeps on `QPDFJob` itself rather than
//! on an individual CLI route: the message prefix, logger, progress callback,
//! warning aggregation, and the single warning-completion summary. JSON and
//! ordinary page-inspection dispatch are layered on top of this state; write,
//! page-transform, and remaining inspection consumers are later job slices.

use super::json::{write_json, JsonJobError, JsonJobOptions, JsonJobOutput};
use crate::pipeline::{Pipeline, PipelineHandle, PipelineResult};
use crate::{
    Error, ObjectStreamMode, Pdf, PdfOpenOptions, PdfWriter, QPDFLogger, Result, Severity,
    UsageError, WriterConfiguration,
};
use std::cell::RefCell;
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek};
use std::path::{Path, PathBuf};
use std::rc::Rc;

type ProgressHandler = Box<dyn FnMut(u8) -> Result<()> + 'static>;
type SharedProgressHandler = Rc<RefCell<ProgressHandler>>;

struct JobOutputPipeline(PipelineHandle);

impl Pipeline for JobOutputPipeline {
    fn identifier(&self) -> &str {
        "qpdf job output"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.0.write(data)
    }

    fn finish(&mut self) -> PipelineResult<()> {
        self.0.finish()
    }
}

/// Portable writer/input state populated by the qpdf job argv/JSON boundary.
///
/// This is deliberately smaller than the CLI's clap model. It owns the
/// settings exercised by `qpdf/qpdfjob-ctest.c`; full command-line transform
/// dispatch remains in the operation-specific job slices.
#[derive(Debug, Clone, Default)]
struct JobConfiguration {
    input_file: Option<PathBuf>,
    output_file: Option<PathBuf>,
    password: Vec<u8>,
    check: bool,
    require_output: bool,
    progress: bool,
    writer: WriterConfiguration,
}

/// qpdf-compatible status returned by a completed job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum JobExitCode {
    /// No warning was recorded, or warnings were explicitly configured to be
    /// exit-zero.
    Success = 0,
    /// The job could not create or write its requested document.
    Error = 2,
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
    input_name: String,
    message_prefix: String,
    warnings: bool,
    suppress_warnings: bool,
    warnings_exit_zero: bool,
    progress_handler: Option<SharedProgressHandler>,
    configuration: JobConfiguration,
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
            input_name: String::new(),
            message_prefix: "qpdf".to_owned(),
            warnings: false,
            suppress_warnings: false,
            warnings_exit_zero: false,
            progress_handler: None,
            configuration: JobConfiguration::default(),
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

    /// Set the qpdf input name used by inspection diagnostics.
    ///
    /// This corresponds to the `QPDFJob` input filename retained by
    /// `QPDFJob::doListAttachments` for its no-embedded-files branch
    /// (`libqpdf/QPDFJob.cc:909`). `open` and `create_from_json` set it
    /// automatically; `update_from_json` keeps the primary input name while
    /// using its own source name only for the update source. Callers that open
    /// a document outside this job may set it explicitly before an inspection.
    pub fn set_input_name(&mut self, input_name: impl Into<String>) {
        self.input_name = input_name.into();
    }

    /// Return the input name retained by this job.
    #[must_use]
    pub fn input_name(&self) -> &str {
        &self.input_name
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
    /// this callback only after its internal borrow is released. A callback
    /// error aborts the active writer, matching qpdf's exception propagation
    /// from QPDFWriter::indicateProgress.
    pub fn register_progress_reporter<F>(&mut self, reporter: F)
    where
        F: FnMut(u8) -> Result<()> + 'static,
    {
        self.progress_handler = Some(Rc::new(RefCell::new(Box::new(reporter))));
    }

    /// Attach the registered progress reporter to one qpdf-shaped writer.
    pub fn configure_writer_progress<R>(&self, writer: &mut PdfWriter<'_, R>)
    where
        R: Read + Seek + 'static,
    {
        // QPDFJob::setWriterOptions uses the custom handler when one was
        // registered and otherwise constructs this default reporter from the
        // job logger (`libqpdf/QPDFJob.cc:2926-2935`).
        let reporter = match self.progress_handler.as_ref() {
            Some(reporter) => Rc::clone(reporter),
            None if self.configuration.progress => {
                let logger = self.logger.clone();
                let prefix = self.message_prefix.clone();
                let output_name = self
                    .configuration
                    .output_file
                    .as_deref()
                    .filter(|path| *path != Path::new("-"))
                    .map_or_else(
                        || "standard output".to_owned(),
                        |path| path.display().to_string(),
                    );
                let callback: ProgressHandler = Box::new(move |percent| {
                    logger.info(format!(
                        "{prefix}: {output_name}: write progress: {percent}%\n"
                    ))
                });
                Rc::new(RefCell::new(callback))
            }
            None => return,
        };
        writer
            .register_progress_reporter(Box::new(move |percent| (reporter.borrow_mut())(percent)));
    }

    /// Initialize the portable qpdf-job argument surface used by qtest.
    ///
    /// This mirrors `QPDFJob::initializeFromArgv` for the arguments owned by
    /// `qpdfjob-ctest.c`: one input, one output, deterministic/static IDs,
    /// object-stream mode, password, decrypt, and check. The full CLI parser
    /// remains outside this production library boundary.
    pub fn initialize_from_argv(&mut self, argv: &[String]) -> Result<()> {
        let mut configuration = JobConfiguration::default();
        let mut positionals = Vec::new();
        let mut parse_options = true;

        for argument in argv.iter().skip(1) {
            if parse_options && argument == "--" {
                parse_options = false;
                continue;
            }
            if parse_options && argument.starts_with("--") {
                match argument.as_str() {
                    "--deterministic-id" => configuration.writer.set_deterministic_id(true),
                    "--static-id" => configuration.writer.set_static_id(true),
                    "--decrypt" => {
                        configuration.writer.set_preserve_encryption(false);
                    }
                    "--progress" => configuration.progress = true,
                    "--check" => configuration.check = true,
                    _ if argument.starts_with("--password=") => {
                        configuration.password = argument.as_bytes()[11..].to_vec();
                    }
                    _ if argument.starts_with("--object-streams=") => {
                        configuration
                            .writer
                            .set_object_stream_mode(parse_object_stream_mode(&argument[17..])?);
                    }
                    _ => {
                        return Err(
                            UsageError::new(format!("unrecognized argument {argument}")).into()
                        );
                    }
                }
            } else if parse_options && argument.starts_with('-') {
                return Err(UsageError::new(format!("unrecognized argument {argument}")).into());
            } else {
                positionals.push(argument.clone());
            }
        }

        if positionals.len() > 2 {
            return Err(UsageError::new(format!("unknown argument {}", positionals[2])).into());
        }
        configuration.input_file = positionals.first().map(PathBuf::from);
        configuration.output_file = positionals.get(1).map(PathBuf::from);
        if configuration.input_file.is_none() && !configuration.check {
            return Err(UsageError::new("an input file name is required").into());
        }

        self.configuration = configuration;
        self.input_name = self
            .configuration
            .input_file
            .as_ref()
            .map_or_else(String::new, |path| path.display().to_string());
        self.warnings = false;
        Ok(())
    }

    /// Initialize the portable qpdf-job JSON surface used by qtest.
    ///
    /// This implements a subset of qpdf's `--job-json-file` schema:
    /// `inputFile`, `outputFile`, `password`, `staticId`, `deterministicId`,
    /// `decrypt`, `objectStreams`, and `progress`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unsupported`] if `json` does not parse as a
    /// dictionary, if `inputFile` is missing or empty, if `outputFile` is
    /// missing, or if the dictionary contains a top-level key outside the
    /// subset above (qpdf's full job JSON schema key, but not yet
    /// implemented by this crate).
    pub fn initialize_from_json(&mut self, json: &str) -> Result<()> {
        self.initialize_from_json_with_partial(json, false)
    }

    /// Initialize a job JSON document for the CLI's partial job-json-file
    /// route. Configuration checks that require command-line input/output
    /// values are deferred to [`QPDFJob::run`], matching qpdf's
    /// `QPDFJob::Config::jobJsonFile` call to `initializeFromJson(..., true)`
    /// (`libqpdf/QPDFJob_config.cc:774-784`).
    pub fn initialize_from_json_partial(&mut self, json: &str) -> Result<()> {
        self.initialize_from_json_with_partial(json, true)
    }

    fn initialize_from_json_with_partial(&mut self, json: &str, partial: bool) -> Result<()> {
        // The qpdf C API sets this prefix before parsing JSON
        // (`libqpdf/qpdfjob-c.cc:79-87`), so initialization and run-time
        // configuration errors share the same observable source name.
        self.set_message_prefix("qpdfjob json");
        let value = crate::json::Json::parse(json.as_bytes())
            .map_err(|error| Error::parse(0, format!("qpdfjob JSON: {error}")))?;
        if !value.is_dictionary() {
            return Err(Error::Unsupported(
                "qpdfjob JSON must contain a dictionary".to_owned(),
            ));
        }
        // qpdf validates the entire job JSON against `JOB_SCHEMA`
        // (`libqpdf/QPDFJob_json.cc:615-624`) before handling any key, so an
        // unrecognized key is a loud schema error rather than a silently
        // ignored option. `JOB_SCHEMA` covers the full `--job-json-file`
        // surface (`libqpdf/qpdf/auto_job_schema.hh`); this crate implements
        // only the subset below, so reject anything else here rather than
        // let it fall through to `job.run()` unapplied.
        const SUPPORTED_TOP_LEVEL_KEYS: &[&[u8]] = &[
            b"inputFile",
            b"outputFile",
            b"password",
            b"staticId",
            b"deterministicId",
            b"decrypt",
            b"objectStreams",
            b"progress",
        ];
        let mut unsupported_key = None;
        let mut output_file_key_present = false;
        value.for_each_dict_item(|key, _item| {
            if key == b"outputFile" {
                output_file_key_present = true;
            }
            if unsupported_key.is_none() && !SUPPORTED_TOP_LEVEL_KEYS.contains(&key) {
                unsupported_key = Some(key.to_vec());
            }
        });
        if let Some(key) = unsupported_key {
            return Err(Error::Unsupported(format!(
                "qpdfjob JSON key \"{}\" is not yet implemented",
                String::from_utf8_lossy(&key)
            )));
        }

        let input = value.get_dict_item(b"inputFile").get_string();
        let output = value.get_dict_item(b"outputFile").get_string();
        // qpdf's JSONHandler dispatches `outputFile` to a string-only handler
        // (`QPDFJob_json.cc:262-265`'s `setupOutputFile` -> `addParameter` ->
        // `addStringHandler`); a present value of any other JSON type falls
        // through every type check in `JSONHandler::handle` and is rejected
        // with a usage error (`libqpdf/JSONHandler.cc:186`), regardless of
        // partial-initialization mode. Only a genuinely *absent* key reaches
        // `partial`'s deferred-output allowance below.
        if output_file_key_present && output.is_none() {
            return Err(Error::Unsupported(
                "qpdfjob JSON key \"outputFile\" must be a string".to_owned(),
            ));
        }
        let Some(input) = input.filter(|value| !value.is_empty()) else {
            return Err(Error::Unsupported(
                "qpdfjob JSON requires inputFile".to_owned(),
            ));
        };
        let mut configuration = JobConfiguration {
            input_file: Some(PathBuf::from(String::from_utf8_lossy(&input).into_owned())),
            output_file: output
                .filter(|value| !value.is_empty())
                .map(|value| PathBuf::from(String::from_utf8_lossy(&value).into_owned())),
            password: value
                .get_dict_item(b"password")
                .get_string()
                .unwrap_or_default(),
            require_output: true,
            ..JobConfiguration::default()
        };
        if json_flag(&value, b"staticId") {
            configuration.writer.set_static_id(true);
        }
        if json_flag(&value, b"deterministicId") {
            configuration.writer.set_deterministic_id(true);
        }
        if json_flag(&value, b"decrypt") {
            configuration.writer.set_preserve_encryption(false);
        }
        configuration.progress = json_flag(&value, b"progress");
        if let Some(mode) = value.get_dict_item(b"objectStreams").get_string() {
            configuration
                .writer
                .set_object_stream_mode(parse_object_stream_mode(&String::from_utf8_lossy(&mode))?);
        } // cov:ignore: the successful objectStreams branch is covered; llvm-cov attributes this closing span separately

        self.configuration = configuration;
        self.input_name = self
            .configuration
            .input_file
            .as_ref()
            .map_or_else(String::new, |path| path.display().to_string());
        self.warnings = false;
        if !partial {
            self.check_configuration()?;
        }
        Ok(())
    }

    /// Create the configured input document, returning `None` after qpdf-style
    /// error reporting for a missing or malformed input.
    pub fn create_qpdf(&mut self) -> Result<Option<Pdf<BufReader<File>>>> {
        match self.check_configuration() {
            Ok(()) => {}
            Err(error @ Error::Usage(_)) => return Err(error),
            Err(error) => {
                self.report_job_error(&error)?;
                return Ok(None);
            }
        }
        let Some(input) = self.configuration.input_file.clone() else {
            let error = Error::Unsupported("qpdfjob input file is not configured".to_owned());
            self.report_job_error(&error)?;
            return Ok(None);
        };
        let file = match File::open(&input) {
            Ok(file) => file,
            Err(error) => {
                let error = Error::file_io("open", input.clone(), error);
                self.report_job_error(&error)?;
                return Ok(None);
            }
        };
        match self.open(
            BufReader::new(file),
            input.display().to_string(),
            PdfOpenOptions {
                password: self.configuration.password.clone(),
                ..PdfOpenOptions::default()
            },
        ) {
            Ok(pdf) => Ok(Some(pdf)),
            Err(error) => {
                self.report_job_error(&error)?;
                Ok(None)
            }
        }
    }

    /// Write a created document through the configured qpdf writer and
    /// complete the shared warning/status boundary.
    pub fn write_qpdf<R>(&mut self, pdf: &mut Pdf<R>) -> Result<JobExitCode>
    where
        R: Read + Seek + 'static,
    {
        let Some(output) = self.configuration.output_file.clone() else {
            return Ok(JobExitCode::Error);
        };
        let writer_configuration = self.configuration.writer.clone();
        let progress_requested = self.configuration.progress;
        let write_result = (|| {
            let mut writer = PdfWriter::new(pdf);
            writer_configuration.apply_to(&mut writer);
            if progress_requested {
                self.configure_writer_progress(&mut writer);
            }
            if output == Path::new("-") {
                self.logger.save_to_standard_output(true)?;
                writer.set_output_pipeline(JobOutputPipeline(self.logger.get_save()?))?;
            } else {
                writer.set_output_file(&output)?;
            }
            writer.write()
        })();
        match write_result {
            Ok(()) => {
                self.record_document_warnings(pdf);
                self.complete(true)
            }
            Err(error) => {
                self.report_job_error(&error)?;
                Ok(JobExitCode::Error)
            }
        }
    }

    /// Run the configured create/write or check lifecycle.
    pub fn run(&mut self) -> Result<JobExitCode> {
        let Some(mut pdf) = self.create_qpdf()? else {
            return Ok(JobExitCode::Error);
        };
        if self.configuration.check || self.configuration.output_file.is_none() {
            let check_result = self.check(&mut pdf);
            return self.map_check_result(check_result);
        }
        self.write_qpdf(&mut pdf)
    }

    fn map_check_result(
        &self,
        result: std::result::Result<JobExitCode, super::check::CheckError>,
    ) -> Result<JobExitCode> {
        match result {
            Ok(status) => Ok(status),
            Err(super::check::CheckError::ErrorsDetected) => Ok(JobExitCode::Error),
            Err(super::check::CheckError::Operation(error)) => {
                self.report_job_error(&error)?;
                Ok(JobExitCode::Error)
            }
        }
    }

    /// Apply qpdf's pre-open output destination and file-identity checks.
    ///
    /// This is the portable subset of `QPDFJob::checkConfiguration`
    /// (`libqpdf/QPDFJob.cc:567-631`): stdout is reserved before the input is
    /// opened, and `QUtil::same_file` rejects destructive aliases before the
    /// writer can truncate them.
    fn check_configuration(&self) -> Result<()> {
        if self.configuration.require_output && self.configuration.output_file.is_none() {
            return Err(UsageError::new(
                "an output file name is required; use - for standard output",
            )
            .into());
        }
        if self.configuration.output_file.as_deref() == Some(Path::new("-")) {
            self.logger.save_to_standard_output(true)?;
        }
        if let (Some(input), Some(output)) = (
            self.configuration.input_file.as_deref(),
            self.configuration.output_file.as_deref(),
        ) {
            if crate::qutil::same_file(input, output) {
                return Err(UsageError::new(
                    "input file and output file are the same; use --replace-input to intentionally overwrite the input",
                )
                .into());
            }
        }
        Ok(())
    }

    fn report_job_error(&self, error: &Error) -> Result<()> {
        // qpdf's C wrapper streams the prefix, separator, message, and final
        // newline separately (`qpdfjob-c.cc:32-39`). Keeping those writes
        // separate preserves custom-pipeline boundaries as well as bytes.
        let pipeline = self.logger.get_error()?;
        pipeline
            .write(self.message_prefix.as_bytes())
            .map_err(Error::from)?;
        pipeline.write(b": ").map_err(Error::from)?;
        pipeline
            .write(Self::job_error_message(error).as_bytes())
            .map_err(Error::from)?;
        pipeline.write(b"\n").map_err(Error::from)
    }

    fn job_error_message(error: &Error) -> String {
        match error {
            Error::FileIo {
                operation,
                path,
                source,
            } => {
                let source = source.to_string();
                let source = source
                    .split_once(" (os error ")
                    .map_or(source.as_str(), |(message, _)| message);
                format!("{operation} {}: {source}", path.display())
            }
            _ => error.to_string(),
        }
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
        let input_name = input_name.into();
        self.input_name = input_name.clone();
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
        let input_name = input_name.into();
        self.input_name = input_name.clone();
        options.logger = Some(self.logger.clone());
        options.description = input_name;
        let mut pdf = Pdf::open_with_options(source, options)?;
        // qpdf's createQPDF calls getVersionAsPDFVersion immediately after
        // processFile; that path enters getExtensionLevel and therefore
        // QPDF::getRoot before any job operation emits output
        // (libqpdf/QPDFJob.cc:429-480,1696-1716; QPDF.cc:2306-2368).
        pdf.root_handle()?;
        self.record_document_warnings(&pdf);
        Ok(pdf)
    }

    /// Open a file-backed document for qpdf's read-only encryption inspection
    /// path. This is the Rust counterpart of `QPDFJob::createQPDF` retaining
    /// the partially initialized document after `qpdf_e_password` so
    /// `showEncryption` can report the parsed parameters.
    pub fn open_for_encryption_inspection<R>(
        &mut self,
        source: R,
        input_name: impl Into<String>,
        mut options: PdfOpenOptions,
    ) -> Result<Pdf<R>>
    where
        R: Read + Seek,
    {
        let input_name = input_name.into();
        self.input_name = input_name.clone();
        options.logger = Some(self.logger.clone());
        options.description = input_name;
        let mut pdf = Pdf::open_for_encryption_inspection(source, options)?;
        // `--password-is-hex-key` (raw key) authentication intentionally
        // leaves both user/owner password-match flags false on success --
        // it bypasses password derivation entirely -- so those flags alone
        // cannot distinguish a genuinely failed open from a successful
        // raw-key one. `encryption_file_key()` is populated only on
        // successful authentication (any mode); `is_encrypted()` separately
        // distinguishes a plaintext document (no /Encrypt at all, so this
        // must be `false` regardless of key presence) from a genuinely
        // failed encrypted one.
        let authentication_failed = pdf.is_encrypted() && pdf.encryption_file_key().is_none();
        // qpdf's createQPDF returns from its password-error catch before the
        // ordinary post-open root walk. Do not resolve an encrypted root in
        // the same partial state; successful/plaintext opens keep the normal
        // QPDFJob root initialization and warning boundary.
        if !authentication_failed {
            pdf.root_handle()?;
        }
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
        let input_name = input_name.into();
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
    /// lifecycle boundary (`QPDFJob.cc:484-563`). The warning-summary
    /// destination is derived from [`JsonJobOutput`] itself, so callers cannot
    /// provide a destination and an inconsistent `creates_output` flag.
    pub fn write_json<R>(
        &mut self,
        pdf: &mut Pdf<R>,
        options: JsonJobOptions<'_>,
        output: JsonJobOutput<'_>,
    ) -> std::result::Result<JobExitCode, JsonJobError>
    where
        R: Read + Seek,
    {
        let creates_output = matches!(&output, JsonJobOutput::File { .. });
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

    /// Return whether warning delivery is suppressed for job-owned documents.
    pub(crate) fn warnings_suppressed(&self) -> bool {
        self.suppress_warnings
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

fn parse_object_stream_mode(value: &str) -> Result<ObjectStreamMode> {
    match value {
        "preserve" => Ok(ObjectStreamMode::Preserve),
        "disable" => Ok(ObjectStreamMode::Disable),
        "generate" => Ok(ObjectStreamMode::Generate),
        other => Err(Error::Unsupported(format!(
            "qpdfjob: invalid objectStreams value {other}"
        ))),
    }
}

fn json_flag(value: &crate::json::Json, key: &[u8]) -> bool {
    let value = value.get_dict_item(key);
    value.get_bool().unwrap_or(false) || value.get_string().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Error, PdfOpenOptions};
    use std::io::Cursor;

    fn trailer_root_pdf(root: &str) -> Vec<u8> {
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let xref_start = bytes.len();
        bytes.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 1 /Root {root} >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        bytes
    }

    #[test]
    fn open_rejects_a_dangling_root_before_returning_a_job_document() {
        let mut job = QPDFJob::new();

        assert!(matches!(
            job.open(
                Cursor::new(trailer_root_pdf("99 0 R")),
                "dangling-root.pdf",
                PdfOpenOptions::default(),
            ),
            Err(Error::System(message)) if message == "unable to find /Root dictionary"
        ));
    }

    #[test]
    fn open_rejects_a_non_dictionary_root_before_returning_a_job_document() {
        let mut job = QPDFJob::new();

        assert!(matches!(
            job.open(
                Cursor::new(trailer_root_pdf("42")),
                "wrong-type-root.pdf",
                PdfOpenOptions::default(),
            ),
            Err(Error::System(message)) if message == "unable to find /Root dictionary"
        ));
    }

    #[test]
    fn open_accepts_a_direct_dictionary_root() {
        let mut job = QPDFJob::new();

        assert!(job
            .open(
                Cursor::new(trailer_root_pdf("<< /Type /Catalog >>")),
                "direct-root.pdf",
                PdfOpenOptions::default(),
            )
            .is_ok());
    }

    /// `--password-is-hex-key` authentication intentionally leaves both
    /// user/owner password-match flags false even on success (it bypasses
    /// password derivation entirely). `open_for_encryption_inspection` must
    /// still run the normal post-open root walk for a successful raw-key
    /// open, the same way it does for a successful password-based one --
    /// distinguishing genuine `BadPassword` failure from raw-key success by
    /// whether a file key was installed, not by the password-match flags.
    #[test]
    fn open_for_encryption_inspection_walks_the_root_after_successful_raw_key_auth() {
        let mut source = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/encrypted/v5-aes-256-r6.pdf"),
        )
        .expect("committed encrypted fixture");
        // Corrupt the trailer's /Root reference to a dangling object number
        // (the fixture's /Size is 4, so object 9 cannot exist) -- same
        // length substitution, no xref/offset shift needed. This is the
        // only way to make the raw-key-vs-password-flags distinction this
        // predicate exists to draw actually observable from outside.
        let needle = b"/Root 1 0 R";
        let at = source
            .windows(needle.len())
            .position(|window| window == needle)
            .expect("fixture has a /Root 1 0 R trailer reference");
        source[at..at + needle.len()].copy_from_slice(b"/Root 9 0 R");

        // Known raw file key for this committed fixture (verified with
        // `qpdf --show-encryption-key --password=… tests/fixtures/encrypted/
        // v5-aes-256-r6.pdf`, matching the constant already established in
        // `crates/flpdf-cli/tests/cli_password_hex_key_tests.rs`).
        let hex_key = b"fc459408a5282b7c59daa5162f860e82315679cc04942ef57993bfd287f30290".to_vec();

        let mut job = QPDFJob::new();
        let result = job.open_for_encryption_inspection(
            Cursor::new(source),
            "dangling-root-hex-key.pdf",
            PdfOpenOptions {
                password: hex_key,
                password_is_hex_key: true,
                ..PdfOpenOptions::default()
            },
        );

        match result {
            Err(Error::System(message)) => assert_eq!(message, "unable to find /Root dictionary"),
            // cov:ignore-start: diagnostic panic arms reachable only if this
            // regression test itself starts failing in an unexpected shape;
            // the passing-suite path always takes the arm above.
            Err(other) => {
                panic!("expected the dangling-/Root error, got a different error: {other}")
            }
            Ok(_) => panic!(
                "a successful raw-key open must still surface a dangling /Root, \
                 proving the root walk ran rather than being skipped as a \
                 (mis-detected) authentication failure"
            ),
            // cov:ignore-end
        }
    }

    #[test]
    fn check_result_mapping_preserves_success_and_maps_both_errors() {
        let job = QPDFJob::new();
        assert_eq!(
            job.map_check_result(Ok(JobExitCode::Success)).unwrap(),
            JobExitCode::Success
        );
        assert_eq!(
            job.map_check_result(Err(super::super::check::CheckError::ErrorsDetected))
                .unwrap(),
            JobExitCode::Error
        );
        assert_eq!(
            job.map_check_result(Err(super::super::check::CheckError::Operation(
                Error::Internal("operation failed".to_owned()),
            )))
            .unwrap(),
            JobExitCode::Error
        );
    }

    #[test]
    fn job_output_pipeline_exposes_its_qpdf_identifier() {
        let pipeline = JobOutputPipeline(PipelineHandle::new(crate::pipeline::Discard));

        assert_eq!(pipeline.identifier(), "qpdf job output");
    }
}
