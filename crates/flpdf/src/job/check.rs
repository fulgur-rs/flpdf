//! qpdf correspondence: `QPDFJob::doCheck` and `QPDFJob::doInspection` (`libqpdf/QPDFJob.cc:745-803,1646-1693`).
//!
//! The document-check consumer owns the full read-only traversal performed by
//! qpdf. The CLI only selects this operation; it does not own a second check
//! report or warning-completion path.

use super::lifecycle::{JobExitCode, QPDFJob};
use crate::content_stream::{ObjectHandleParserCallbacks, ParseControl};
use crate::linearization::{
    check_linearization_parameters, check_linearization_warnings, LinearizationCheckError,
    LinearizationParameterCheck,
};
use crate::pipeline::Discard;
use crate::{DecodeLevel, PageDocumentHelper, PageObjectHelper, Pdf, PdfWriter};
use crate::{EncryptionInfo, ObjectHandle, QPDFLogger, Result, Severity};
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
    /// Run qpdf's standalone linearization inspection on the already-open
    /// document and complete the shared warning/status boundary.
    ///
    /// This is the QPDFJob::doInspection branch for
    /// --check-linearization (libqpdf/QPDFJob.cc:1646-1674). qpdf first
    /// asks QPDF::isLinearized, then calls QPDF::checkLinearization on the
    /// same document. Linearization damage is accumulated as a warning by the
    /// document checker; it is not a CLI exception or a second file open.
    pub fn check_linearization<R: Read + Seek + 'static>(
        &mut self,
        pdf: &mut Pdf<R>,
    ) -> Result<JobExitCode> {
        self.check_linearization_report(pdf)?;
        self.complete(false)
    }

    /// Emit the linearization report without completing the enclosing job.
    ///
    /// The shared inspection dispatcher uses this report-only form to preserve
    /// qpdf's ordered `doInspection` branches (`QPDFJob.cc:1652-1667`) and emit
    /// one completion summary after all selected consumers finish.
    pub(crate) fn check_linearization_report<R: Read + Seek + 'static>(
        &mut self,
        pdf: &mut Pdf<R>,
    ) -> Result<()> {
        let logger = self.logger();
        let input_name = self.input_name().to_owned();
        pdf.set_logger(logger.clone());

        if !pdf.is_linearized()? {
            logger.info(format!("{input_name} is not linearized\n"))?;
            self.record_document_warnings(pdf);
            return Ok(());
        }

        let linearization_warnings = emit_linearization_check_for_document_with_suppression(
            pdf,
            &logger,
            &input_name,
            self.warnings_suppressed(),
        )?; // cov:ignore: closing line of a multi-line suppress_warnings call/block; llvm-cov misattributes the hit count to the previous line, not an untested branch

        if linearization_warnings {
            self.record_warnings();
        } else {
            logger.info(format!("{input_name}: no linearization errors\n"))?;
        }
        self.record_document_warnings(pdf);
        Ok(())
    }

    /// Replay qpdf repair diagnostics retained by a failed permissive open.
    ///
    /// `QPDF::processFile` emits these warnings before returning its terminal
    /// open error (`libqpdf/QPDF.cc:1550-1595`). The resolver retains them in
    /// [`crate::Error::OpenFailure`] when the caller suppresses live delivery;
    /// this job-owned adapter restores the same logger boundary for `--check`
    /// without making a second parse of the input.
    pub fn report_open_failure(&self, error: &crate::Error) -> Result<()> {
        if self.warnings_suppressed() {
            return Ok(());
        }
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
        self.run_check_report(pdf)?;
        Ok(self.complete(false)?)
    }

    /// Run the full check report without completing the enclosing job.
    ///
    /// qpdf's `doCheck` may be followed by other `doInspection` consumers on
    /// success, so job JSON uses this report-only form before its final shared
    /// completion (`QPDFJob.cc:1649-1655`).
    pub(crate) fn run_check_report<R: Read + Seek + 'static>(
        &mut self,
        pdf: &mut Pdf<R>,
    ) -> std::result::Result<(), CheckError> {
        let logger = self.logger();
        let input_name = self.input_name().to_owned();
        let message_prefix = self.message_prefix().to_owned();

        // The top-level `--check` route suppresses document warnings while
        // opening so the report can replay them after its banner. Job JSON
        // opens with the job logger live, so replay only when the document's
        // own warning delivery is suppressed.
        let replay_diagnostics = pdf.suppress_warnings();
        pdf.set_logger(logger.clone());
        let outcome = check_document_with_suppression(
            pdf,
            &logger,
            &message_prefix,
            &input_name,
            self.warnings_suppressed(),
            self.show_encryption_key(),
            replay_diagnostics,
        )?;
        self.record_document_warnings(pdf);
        if outcome.warnings {
            self.record_warnings();
        }
        Ok(())
    }

    /// Emit qpdf's `QPDFJob::showEncryption` report for an already-open
    /// document.
    ///
    /// The renderer is owned by this job layer so `--check` and the CLI's
    /// standalone `--show-encryption` path cannot drift in permission or
    /// encryption-method semantics. `password_is_hex_key` only controls the
    /// read-only inspection prefix used when a partial open retained an
    /// encrypted document after a failed ordinary password attempt.
    pub fn show_encryption<R: Read + Seek>(
        &self,
        pdf: &mut Pdf<R>,
        password_is_hex_key: bool,
    ) -> Result<()> {
        let logger = self.logger();
        emit_encryption_report(
            pdf,
            &logger,
            password_is_hex_key,
            self.show_encryption_key(),
        )
    }
}

/// Run the canonical qpdf-shaped document check for crate-internal tests.
///
/// This keeps test-only validity assertions on QPDFJob::check rather than
/// reviving the removed top-level report API.
#[cfg(test)]
pub(crate) fn check_bytes_for_test(bytes: Vec<u8>) -> std::result::Result<JobExitCode, CheckError> {
    let mut job = QPDFJob::new();
    let mut pdf = job
        .open(
            std::io::Cursor::new(bytes),
            "test.pdf",
            crate::PdfOpenOptions {
                repair: true,
                ..crate::PdfOpenOptions::default()
            },
        )
        .map_err(CheckError::Operation)?;
    job.check(&mut pdf)
}

#[cfg(test)]
fn check_document<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    logger: &QPDFLogger,
    message_prefix: &str,
    input_name: &str,
) -> std::result::Result<CheckOutcome, CheckError> {
    check_document_with_suppression(pdf, logger, message_prefix, input_name, false, false, true)
}

fn check_document_with_suppression<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    logger: &QPDFLogger,
    message_prefix: &str,
    input_name: &str,
    suppress_warnings: bool,
    show_encryption_key: bool,
    replay_diagnostics: bool,
) -> std::result::Result<CheckOutcome, CheckError> {
    let mut warnings = false;
    let mut diagnostics_seen = 0;

    // qpdf's `JobSetter::setCheckMode` is applied before the first
    // `QPDF::getRoot` in `doCheck` (`QPDFJob.cc:745-752`). Keep the flag on
    // the document so the root accessor repairs an invalid Catalog type for
    // every subsequent inspection branch on this same document.
    pdf.set_check_mode(true);

    // qpdf's QPDF::getRoot reads trailer /Root through getKey and accepts
    // either a direct or indirect Catalog, rejecting only a missing,
    // dangling, or non-dictionary value (libqpdf/QPDF.cc:2329-2367).
    let root_diagnostics_seen = diagnostic_count(pdf);
    if let Err(error) = pdf.root_handle() {
        return Err(map_check_error(
            logger,
            message_prefix,
            input_name,
            error,
            logger_failure_since(pdf, root_diagnostics_seen),
        ));
    }

    logger.info(format!("checking {input_name}\n"))?;
    let extension_diagnostics_seen = diagnostic_count(pdf);
    let extension_level = match pdf.adobe_extension_level() {
        Ok(level) => level,
        Err(error) => {
            return Err(map_check_error(
                logger,
                message_prefix,
                input_name,
                error,
                logger_failure_since(pdf, extension_diagnostics_seen),
            ));
        }
    };
    match extension_level {
        Some(level) if level > 0 => {
            let version = format!("PDF Version: {} extension level {level}\n", pdf.version());
            logger.info(version)?;
        }
        Some(_) | None => logger.info(format!("PDF Version: {}\n", pdf.version()))?,
    }
    // `check()` only ever renders this report for a document that already
    // opened successfully, so the password-derived `user_password_matched`/
    // `owner_password_matched` flags can legitimately both be false (a
    // hex-key open never sets them) without the document being the product
    // of a failed password attempt. qpdf's `doCheck` calls `showEncryption`
    // directly (`QPDFJob.cc:744-765`) and that function never emits
    // "Incorrect password supplied" in any branch (`QPDFJob.cc:700-742`) —
    // the message exists only in `createQPDF()`'s `qpdf_e_password` catch
    // block for the standalone `--show-encryption` mode
    // (`QPDFJob.cc:428-448`), a path `--check` never reaches. Always
    // suppress it here.
    emit_encryption_report(pdf, logger, true, show_encryption_key)?;

    let linearized_diagnostics_seen = diagnostic_count(pdf);
    let linearized = match pdf.is_linearized() {
        Ok(value) => value,
        Err(error) => {
            return Err(map_check_error(
                logger,
                message_prefix,
                input_name,
                error,
                logger_failure_since(pdf, linearized_diagnostics_seen),
            ));
        }
    };
    if linearized {
        logger.info("File is linearized\n")?;
        warnings |= emit_linearization_check_for_document_with_suppression(
            pdf,
            logger,
            input_name,
            suppress_warnings,
        )?; // cov:ignore: closing line of a multi-line suppress_warnings call/block; llvm-cov misattributes the hit count to the previous line, not an untested branch
    } else {
        logger.info("File is not linearized\n")?;
    }

    let (new_warnings, new_errors) = inspect_new_diagnostics(
        pdf,
        diagnostics_seen,
        logger,
        message_prefix,
        input_name,
        suppress_warnings,
        replay_diagnostics,
    )?; // cov:ignore: closing line of a multi-line suppress_warnings call/block; llvm-cov misattributes the hit count to the previous line, not an untested branch
    warnings |= new_warnings;
    diagnostics_seen = pdf.repair_diagnostics().entries().len();
    if new_errors {
        return Err(CheckError::ErrorsDetected); // cov:ignore: Pdf repair diagnostics are warning-severity; retain this defensive boundary.
    }

    let writer_diagnostics_seen = diagnostic_count(pdf);
    let writer_result = (|| -> Result<()> {
        let mut writer = PdfWriter::new(pdf);
        writer.set_output_pipeline(Discard)?;
        writer.set_decode_level(DecodeLevel::All);
        writer.write()
    })();
    if let Err(error) = writer_result {
        return Err(map_check_error(
            logger,
            message_prefix,
            input_name,
            error,
            logger_failure_since(pdf, writer_diagnostics_seen),
        ));
    }

    let (new_warnings, new_errors) = inspect_new_diagnostics(
        pdf,
        diagnostics_seen,
        logger,
        message_prefix,
        input_name,
        suppress_warnings,
        replay_diagnostics,
    )?; // cov:ignore: closing line of a multi-line suppress_warnings call/block; llvm-cov misattributes the hit count to the previous line, not an untested branch
    warnings |= new_warnings;
    diagnostics_seen = pdf.repair_diagnostics().entries().len();
    if new_errors {
        return Err(CheckError::ErrorsDetected); // cov:ignore: Pdf repair diagnostics are warning-severity; retain this defensive boundary.
    }

    let page_tree_diagnostics_seen = diagnostic_count(pdf);
    let pages_result = PageDocumentHelper::new(pdf).get_all_pages();
    let pages = match pages_result {
        Ok(pages) => pages,
        // cov:ignore-start: check's qpdf-shaped discard writer above runs the
        // same page-tree preparation with DecodeLevel::All before this second
        // page-list read; a failure in this defensive repeat is therefore
        // unreachable for a live document state.
        Err(error) => {
            return Err(map_page_tree_error(
                logger,
                message_prefix,
                input_name,
                error,
                logger_failure_since(pdf, page_tree_diagnostics_seen),
            ));
        } // cov:ignore-end
    };
    let mut page_errors = false;
    for (index, page_ref) in pages.into_iter().enumerate() {
        let page_diagnostics_seen = diagnostic_count(pdf);
        let page_result = {
            let mut page = PageObjectHelper::new(page_ref, pdf);
            let mut discard_contents = DiscardContents;
            page.parse_page_contents(&mut discard_contents)
        };
        if let Err(error) = page_result {
            if logger_failure_since(pdf, page_diagnostics_seen) && is_logger_error(&error) {
                return Err(CheckError::Operation(error));
            }
            page_errors = true;
            logger.error(format!("ERROR: page {}: {error}\n", index + 1))?;
        }
    }
    if page_errors {
        return Err(CheckError::ErrorsDetected);
    }

    let (new_warnings, new_errors) = inspect_new_diagnostics(
        pdf,
        diagnostics_seen,
        logger,
        message_prefix,
        input_name,
        suppress_warnings,
        replay_diagnostics,
    )?; // cov:ignore: closing line of a multi-line suppress_warnings call/block; llvm-cov misattributes the hit count to the previous line, not an untested branch
    warnings |= new_warnings;
    if new_errors {
        return Err(CheckError::ErrorsDetected); // cov:ignore: Pdf repair diagnostics are warning-severity; retain this defensive boundary.
    }

    if !warnings {
        logger.info(
            "No syntax or stream encoding errors found; the file may still contain\nerrors that qpdf cannot detect\n",
        )?; // cov:ignore: closing line of a multi-line suppress_warnings call/block; llvm-cov misattributes the hit count to the previous line, not an untested branch
    }

    Ok(CheckOutcome { warnings })
}

fn inspect_new_diagnostics<R: Read + Seek>(
    pdf: &Pdf<R>,
    seen: usize,
    logger: &QPDFLogger,
    message_prefix: &str,
    input_name: &str,
    suppress_warnings: bool,
    replay_diagnostics: bool,
) -> std::result::Result<(bool, bool), CheckError> {
    if replay_diagnostics {
        return emit_new_diagnostics_with_suppression(
            pdf,
            seen,
            logger,
            message_prefix,
            input_name,
            suppress_warnings,
        );
    }

    let diagnostics = pdf.repair_diagnostics();
    let mut new_diagnostics = diagnostics.entries().iter().skip(seen);
    let warnings = new_diagnostics
        .clone()
        .any(|diagnostic| matches!(diagnostic.severity, Severity::Warning));
    let errors = new_diagnostics.any(|diagnostic| matches!(diagnostic.severity, Severity::Error));
    Ok((warnings, errors))
}

fn emit_encryption_report<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    logger: &QPDFLogger,
    suppress_password_mismatch_notice: bool,
    show_encryption_key: bool,
) -> Result<()> {
    let report =
        render_encryption_report(pdf, suppress_password_mismatch_notice, show_encryption_key)?;
    logger.info(report)
}

/// Build the byte-exact report emitted by qpdf's `QPDFJob::showEncryption`.
///
/// This keeps the report in a byte buffer because qpdf writes the recovered
/// user password as an arbitrary byte string, not as UTF-8 text. The
/// revision-dependent permission projection follows
/// `libqpdf/QPDF_encryption.cc`'s `allow*` methods.
///
/// `suppress_password_mismatch_notice` covers every caller where the
/// "Incorrect password supplied" line must never appear even though the
/// matched-password flags are both false: a hex-key open (the caller passes
/// `true` for `password_is_hex_key`) never sets either flag on success, and
/// `check()`'s report (the caller passes a hard-coded `true`) can only ever
/// run against a document that already opened successfully, so the failed-
/// password state this line describes is unreachable there regardless of
/// how the document was authenticated.
fn render_encryption_report<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    suppress_password_mismatch_notice: bool,
    show_encryption_key: bool,
) -> Result<Vec<u8>> {
    let Some(info) = pdf.encryption_info()? else {
        return Ok(b"File is not encrypted\n".to_vec());
    };

    let mut output = Vec::new();
    if !info.user_password_matched
        && !info.owner_password_matched
        && !suppress_password_mismatch_notice
    {
        output.extend_from_slice(b"Incorrect password supplied\n");
    }

    output.extend_from_slice(format!("R = {}\n", info.r).as_bytes());
    output.extend_from_slice(format!("P = {}\n", info.permissions.raw()).as_bytes());
    output.extend_from_slice(b"User password = ");
    output.extend_from_slice(&info.user_password);
    output.push(b'\n');
    if show_encryption_key {
        let key = pdf.encryption_file_key().unwrap_or_default();
        output.extend_from_slice(b"Encryption key = ");
        output.extend_from_slice(hex::encode(key).as_bytes());
        output.push(b'\n');
    }
    if info.owner_password_matched {
        output.extend_from_slice(b"Supplied password is owner password\n");
    }
    if info.user_password_matched {
        output.extend_from_slice(b"Supplied password is user password\n");
    }

    let permissions = permission_report(&info);
    for (label, allowed) in [
        ("extract for accessibility", permissions.accessibility),
        ("extract for any purpose", permissions.extract_all),
        ("print low resolution", permissions.print_low),
        ("print high resolution", permissions.print_high),
        ("modify document assembly", permissions.modify_assembly),
        ("modify forms", permissions.modify_form),
        ("modify annotations", permissions.modify_annotation),
        ("modify other", permissions.modify_other),
        ("modify anything", permissions.modify_all),
    ] {
        output.extend_from_slice(format!("{label}: {}\n", show_bool(allowed)).as_bytes());
    }

    if info.v >= 4 {
        output.extend_from_slice(
            format!("stream encryption method: {}\n", info.stream_method).as_bytes(),
        );
        output.extend_from_slice(
            format!("string encryption method: {}\n", info.string_method).as_bytes(),
        );
        output
            .extend_from_slice(format!("file encryption method: {}\n", info.eff_method).as_bytes());
    }
    Ok(output)
}

struct PermissionReport {
    accessibility: bool,
    extract_all: bool,
    print_low: bool,
    print_high: bool,
    modify_assembly: bool,
    modify_form: bool,
    modify_annotation: bool,
    modify_other: bool,
    modify_all: bool,
}

fn permission_report(info: &EncryptionInfo) -> PermissionReport {
    let raw = info.permissions.raw() as u32;
    let bit = |number: u32| raw & (1u32 << (number - 1)) != 0;
    let print_low = bit(3);
    let modify_assembly = if info.r < 3 { bit(4) } else { bit(11) };
    let modify_form = if info.r < 3 { bit(6) } else { bit(9) };
    let modify_annotation = bit(6);
    let modify_other = bit(4);

    PermissionReport {
        accessibility: if info.r < 3 { bit(5) } else { bit(10) },
        extract_all: bit(5),
        print_low,
        print_high: print_low && (info.r < 3 || bit(12)),
        modify_assembly,
        modify_form,
        modify_annotation,
        modify_other,
        modify_all: modify_annotation
            && modify_other
            && (info.r < 3 || (modify_form && modify_assembly)),
    }
}

fn show_bool(value: bool) -> &'static str {
    if value {
        "allowed"
    } else {
        "not allowed"
    }
}

fn linearization_parameter_error_message(input_name: &str, message: &str, offset: u64) -> String {
    for object in ["linearization dictionary", "linearization hint table"] {
        let prefix = format!("{object}: ");
        if let Some(detail) = message.strip_prefix(&prefix) {
            return format!("{input_name} ({object}, offset {offset}): {detail}");
        }
    }
    format!("linearization check failed: {message}")
}

fn linearization_parameter_offset<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    message: &str,
) -> Result<u64> {
    if message.starts_with("linearization dictionary: ") {
        if let Some(candidate) = pdf.linearization_candidate_ref()? {
            let parsed_offset = pdf.get_object_handle(candidate).get_parsed_offset();
            // cov:ignore-start: parser-created linearization candidates always
            // carry a non-negative parsed dictionary offset; this fallback is
            // defensive for synthetic handles.
            if parsed_offset >= 0 {
                return Ok(parsed_offset as u64);
            }
            // cov:ignore-end
        } // cov:ignore: candidate is guaranteed for a linearization parameter error
    }
    Ok(pdf.source_last_offset())
}

/// Run qpdf's linearization-data loading and warning-producing check once for
/// either the generic document check or standalone --check-linearization.
///
/// The parameter preflight is required before the deep checker: qpdf's
/// readLinearizationData accepts an integer /O without dereferencing the
/// referenced object, so a mismatching /O is a soft warning even when the
/// referenced object is not a Page.
fn emit_linearization_check_for_document_with_suppression<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    logger: &QPDFLogger,
    input_name: &str,
    suppress_warnings: bool,
) -> Result<bool> {
    let mut warnings = false;
    let source_bytes = match pdf.source_bytes() {
        Ok(bytes) => bytes,
        Err(error) => {
            warnings = true;
            let message = format!("error encountered while checking linearization data: {error}");
            if !suppress_warnings {
                emit_warning(logger, input_name, message)?;
            } // cov:ignore: closing line of a multi-line suppress_warnings call/block; llvm-cov misattributes the hit count to the previous line, not an untested branch
            Vec::new()
        }
    };

    if source_bytes.is_empty() {
        return Ok(warnings);
    }

    let diagnostics_seen = diagnostic_count(pdf);
    match check_linearization_parameters(pdf) {
        Ok(LinearizationParameterCheck::Clean) => {
            warnings |= emit_linearization_check_warnings_with_suppression(
                pdf,
                &source_bytes,
                logger,
                input_name,
                false,
                suppress_warnings,
            )?; // cov:ignore: closing line of a multi-line suppress_warnings call/block; llvm-cov misattributes the hit count to the previous line, not an untested branch
        }
        Ok(LinearizationParameterCheck::Warning(message)) => {
            warnings = true;
            if !suppress_warnings {
                emit_warning(logger, input_name, message)?;
            } // cov:ignore: closing line of a multi-line suppress_warnings call/block; llvm-cov misattributes the hit count to the previous line, not an untested branch
            warnings |= emit_linearization_check_warnings_with_suppression(
                pdf,
                &source_bytes,
                logger,
                input_name,
                true,
                suppress_warnings,
            )?; // cov:ignore: closing line of a multi-line suppress_warnings call/block; llvm-cov misattributes the hit count to the previous line, not an untested branch
        }
        Ok(LinearizationParameterCheck::Error(message)) => {
            warnings = true;
            let message = format!(
                "error encountered while checking linearization data: {}",
                linearization_parameter_error_message(
                    input_name,
                    message,
                    linearization_parameter_offset(pdf, message)?,
                )
            );
            if !suppress_warnings {
                emit_warning(logger, input_name, message)?;
            } // cov:ignore: closing line of a multi-line suppress_warnings call/block; llvm-cov misattributes the hit count to the previous line, not an untested branch
        }
        Err(error) if logger_failure_since(pdf, diagnostics_seen) && is_logger_error(&error) => {
            return Err(error);
        }
        Err(error) => {
            warnings = true;
            let message = format!("error encountered while checking linearization data: {error}");
            if !suppress_warnings {
                emit_warning(logger, input_name, message)?;
            } // cov:ignore: closing line of a multi-line suppress_warnings call/block; llvm-cov misattributes the hit count to the previous line, not an untested branch
        }
    }

    Ok(warnings)
}

fn emit_linearization_check_warnings_with_suppression<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    source_bytes: &[u8],
    logger: &QPDFLogger,
    input_name: &str,
    skip_first_page_warning: bool,
    suppress_warnings: bool,
) -> Result<bool> {
    let diagnostics_seen = diagnostic_count(pdf);
    match check_linearization_warnings(pdf, source_bytes, skip_first_page_warning) {
        Ok(messages) => {
            let has_warnings = !messages.is_empty();
            for message in messages {
                if !suppress_warnings {
                    emit_warning(logger, input_name, message)?;
                } // cov:ignore: closing line of a multi-line suppress_warnings call/block; llvm-cov misattributes the hit count to the previous line, not an untested branch
            }
            Ok(has_warnings)
        }
        Err(LinearizationCheckError::NotLinearized) => Ok(false), // cov:ignore: check_document accepts only a linearized candidate before this helper
        Err(LinearizationCheckError::InvalidParam { message }) => {
            let message = format!(
                "error encountered while checking linearization data: {}",
                linearization_parameter_error_message(
                    input_name,
                    &message,
                    linearization_parameter_offset(pdf, &message)?
                )
            );
            if !suppress_warnings {
                emit_warning(logger, input_name, message)?;
            } // cov:ignore: closing line of a multi-line suppress_warnings call/block; llvm-cov misattributes the hit count to the previous line, not an untested branch
            Ok(true)
        }
        Err(error) => {
            let error_message = error.to_string();
            if let Some(error) = take_logger_failure(error, pdf, diagnostics_seen) {
                return Err(error);
            }
            let message =
                format!("error encountered while checking linearization data: {error_message}");
            if !suppress_warnings {
                emit_warning(logger, input_name, message)?;
            }
            Ok(true)
        }
    }
}

fn map_page_tree_error(
    logger: &QPDFLogger,
    message_prefix: &str,
    input_name: &str,
    error: crate::Error,
    logger_failure: bool,
) -> CheckError {
    map_check_error(logger, message_prefix, input_name, error, logger_failure)
}

fn is_logger_error(error: &crate::Error) -> bool {
    matches!(error, crate::Error::Internal(_) | crate::Error::System(_))
}

fn diagnostic_count<R: Read + Seek>(pdf: &Pdf<R>) -> usize {
    pdf.repair_diagnostics().entries().len()
}

fn logger_failure_since<R: Read + Seek>(pdf: &Pdf<R>, seen: usize) -> bool {
    !pdf.suppress_warnings() && diagnostic_count(pdf) > seen
}

fn take_logger_failure<R: Read + Seek>(
    error: LinearizationCheckError,
    pdf: &Pdf<R>,
    diagnostics_seen: usize,
) -> Option<crate::Error> {
    let LinearizationCheckError::Io(error) = error else {
        return None;
    };
    let Ok(error) = error.downcast::<crate::Error>() else {
        return None;
    };
    (logger_failure_since(pdf, diagnostics_seen) && is_logger_error(&error)).then_some(*error)
}

fn map_check_error(
    logger: &QPDFLogger,
    message_prefix: &str,
    input_name: &str,
    error: crate::Error,
    logger_failure: bool,
) -> CheckError {
    if logger_failure && is_logger_error(&error) {
        CheckError::Operation(error)
    } else {
        match emit_error(logger, message_prefix, input_name, &error) {
            Ok(()) => CheckError::ErrorsDetected,
            Err(delivery_error) => CheckError::Operation(delivery_error),
        }
    }
}

#[cfg(test)]
fn emit_new_diagnostics<R: Read + Seek>(
    pdf: &Pdf<R>,
    seen: usize,
    logger: &QPDFLogger,
    message_prefix: &str,
    input_name: &str,
) -> std::result::Result<(bool, bool), CheckError> {
    emit_new_diagnostics_with_suppression(pdf, seen, logger, message_prefix, input_name, false)
}

fn emit_new_diagnostics_with_suppression<R: Read + Seek>(
    pdf: &Pdf<R>,
    seen: usize,
    logger: &QPDFLogger,
    message_prefix: &str,
    input_name: &str,
    suppress_warnings: bool,
) -> std::result::Result<(bool, bool), CheckError> {
    let diagnostics = pdf.repair_diagnostics();
    let result = emit_diagnostics_with_suppression(
        &diagnostics,
        seen,
        logger,
        message_prefix,
        input_name,
        suppress_warnings,
    )?;
    Ok(result)
}

fn emit_diagnostics(
    diagnostics: &crate::Diagnostics,
    seen: usize,
    logger: &QPDFLogger,
    message_prefix: &str,
    input_name: &str,
) -> Result<(bool, bool)> {
    emit_diagnostics_with_suppression(diagnostics, seen, logger, message_prefix, input_name, false)
}

fn emit_diagnostics_with_suppression(
    diagnostics: &crate::Diagnostics,
    seen: usize,
    logger: &QPDFLogger,
    message_prefix: &str,
    input_name: &str,
    suppress_warnings: bool,
) -> Result<(bool, bool)> {
    let mut warnings = false;
    let mut errors = false;
    for diagnostic in diagnostics.entries().iter().skip(seen) {
        match diagnostic.severity {
            Severity::Warning => {
                warnings = true;
                if suppress_warnings {
                    continue;
                }
                if is_contextless_object_warning(&diagnostic.message) {
                    logger.warn(format!("WARNING: {}\n", diagnostic.message))?;
                    continue;
                }
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

/// qpdf's ObjectHandle::objectWarning uses an empty filename and zero offset
/// in its QPDFExc, so its description is already the complete warning prefix
/// (`libqpdf/QPDFObjectHandle.cc:2203-2212`, `libqpdf/QPDFExc.cc:19-49`).
fn is_contextless_object_warning(message: &str) -> bool {
    ["object ", "page object ", "content stream object "]
        .iter()
        .any(|prefix| message.starts_with(prefix))
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
    use crate::{Diagnostic, Diagnostics, Error, ObjectRef, PdfOpenOptions, QPDFLogger};
    use std::io::{self, Cursor, Read, Seek, SeekFrom};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    // cov:ignore-start: test-only logger capture pipeline is not production code
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
    // cov:ignore-end

    // cov:ignore-start: test-only failing logger pipeline is not production code
    struct FailingCapture;

    impl Pipeline for FailingCapture {
        fn identifier(&self) -> &str {
            "check test failing capture"
        }

        fn write(&mut self, _data: &[u8]) -> PipelineResult<()> {
            Err(crate::pipeline::PipelineError::runtime("logger failure"))
        }

        fn finish(&mut self) -> PipelineResult<()> {
            Ok(())
        }
    }
    // cov:ignore-end

    struct ToggleReader {
        reader: Cursor<Vec<u8>>,
        fail: Arc<AtomicBool>,
    }

    impl Read for ToggleReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.fail.load(Ordering::Relaxed) {
                return Err(io::Error::other("test reader failure"));
            }
            self.reader.read(buffer)
        }
    }

    impl Seek for ToggleReader {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            if self.fail.load(Ordering::Relaxed) {
                return Err(io::Error::other("test reader failure"));
            }
            self.reader.seek(position)
        }
    }

    struct SnapshotFailReader {
        reader: Cursor<Vec<u8>>,
        armed: Arc<AtomicBool>,
        snapshot: Arc<AtomicBool>,
        start_zero_seeks: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Read for SnapshotFailReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.snapshot.load(Ordering::Relaxed) {
                return Err(io::Error::other("linearization snapshot failure"));
            }
            self.reader.read(buffer)
        }
    }

    impl Seek for SnapshotFailReader {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            if self.armed.load(Ordering::Relaxed) && position == SeekFrom::Start(0) {
                let previous = self.start_zero_seeks.fetch_add(1, Ordering::Relaxed);
                if previous >= 1 {
                    self.snapshot.store(true, Ordering::Relaxed);
                }
            }
            self.reader.seek(position)
        }
    }

    fn logger_with_capture(bytes: Arc<Mutex<Vec<u8>>>) -> QPDFLogger {
        let logger = QPDFLogger::create();
        logger.set_output_streams(
            Some(PipelineHandle::new(Capture {
                bytes: Arc::clone(&bytes),
            })),
            Some(PipelineHandle::new(Capture { bytes })),
        );
        logger
    }

    fn missing_root_pdf_bytes() -> Vec<u8> {
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let off1 = pdf.len();
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = pdf.len();
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");
        let xref_start = pdf.len();
        pdf.extend_from_slice(
            format!("xref\n0 3\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n")
                .as_bytes(),
        );
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 3 >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
        );
        pdf
    }

    fn extension_level_pdf_bytes() -> Vec<u8> {
        let mut pdf = b"%PDF-1.7\n".to_vec();
        let off1 = pdf.len();
        pdf.extend_from_slice(
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Extensions << /ADBE << /BaseVersion /1.7 /ExtensionLevel 8 >> >> >>\nendobj\n",
        );
        let off2 = pdf.len();
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let off3 = pdf.len();
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let xref_start = pdf.len();
        pdf.extend_from_slice(
            format!(
                "xref\n0 4\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n"
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    fn page_tree_warning_pdf_bytes() -> Vec<u8> {
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let off1 = pdf.len();
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = pdf.len();
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Parent 3 0 R /Kids [] /Count 0 >>\nendobj\n",
        );
        let off3 = pdf.len();
        pdf.extend_from_slice(b"3 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");
        let xref_start = pdf.len();
        pdf.extend_from_slice(
            format!(
                "xref\n0 4\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n"
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    fn linearization_candidate_warning_pdf_bytes() -> Vec<u8> {
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let off1 = pdf.len();
        // Deliberately omit `endobj`: resolving the candidate is a qpdf
        // recovery warning, and the test injects a failure in its warning
        // sink. The next object gives the parser a concrete boundary.
        pdf.extend_from_slice(b"1 0 obj\n<< /Linearized 1 >>\n");
        let off2 = pdf.len();
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Catalog /Pages 3 0 R >>\nendobj\n");
        let off3 = pdf.len();
        pdf.extend_from_slice(b"3 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");
        let xref_start = pdf.len();
        pdf.extend_from_slice(
            format!(
                "xref\n0 4\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n"
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 2 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    fn lazy_extension_warning_pdf_bytes() -> Vec<u8> {
        let mut pdf = b"%PDF-1.7\n".to_vec();
        let off1 = pdf.len();
        pdf.extend_from_slice(
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Extensions 4 0 R >>\nendobj\n",
        );
        let off2 = pdf.len();
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let off3 = pdf.len();
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let off4 = pdf.len();
        // The Catalog is valid; resolving this indirect extension dictionary
        // later emits the expected-endobj recovery warning.
        pdf.extend_from_slice(b"4 0 obj\n<< /ADBE << /BaseVersion /1.7 /ExtensionLevel 8 >> >>\n");
        let xref_start = pdf.len();
        pdf.extend_from_slice(
            format!(
                "xref\n0 5\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n{off4:010} 00000 n \n"
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    fn single_page_content_pdf_bytes(content: &[u8]) -> Vec<u8> {
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let off1 = pdf.len();
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = pdf.len();
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let off3 = pdf.len();
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n",
        );
        let off4 = pdf.len();
        pdf.extend_from_slice(
            format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len()).as_bytes(),
        );
        pdf.extend_from_slice(content);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
        let xref_start = pdf.len();
        pdf.extend_from_slice(
            format!(
                "xref\n0 5\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n{off4:010} 00000 n \n"
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    fn corrupt_content_stream_pdf_bytes() -> Vec<u8> {
        let payload = b"this is not valid zlib data at all";
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let off1 = pdf.len();
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = pdf.len();
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let off3 = pdf.len();
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n",
        );
        let off4 = pdf.len();
        pdf.extend_from_slice(
            format!(
                "4 0 obj\n<< /Filter /FlateDecode /Length {} >>\nstream\n",
                payload.len()
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(payload);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
        let xref_start = pdf.len();
        pdf.extend_from_slice(
            format!(
                "xref\n0 5\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n{off4:010} 00000 n \n"
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn check_error_and_diagnostic_helpers_keep_qpdf_locations() {
        assert_eq!(CheckError::ErrorsDetected.to_string(), "errors detected");
        let operation: CheckError = Error::Internal("operation failed".to_owned()).into();
        assert_eq!(operation.to_string(), "operation failed");

        let output = Arc::new(Mutex::new(Vec::new()));
        let logger = logger_with_capture(Arc::clone(&output));
        let mut diagnostics = Diagnostics::default();
        diagnostics.push(Diagnostic::warning(
            "(object 5 0, offset 232): expected endobj",
            Some(232),
        ));
        diagnostics.push(Diagnostic::warning(
            "(trailer, offset 190): duplicated key",
            Some(190),
        ));
        diagnostics.push(Diagnostic::warning("xref warning", Some(12)));
        diagnostics.push(Diagnostic::warning("warning without offset", None));
        diagnostics.push(Diagnostic::warning(
            "page object 3 0:  object is supposed to be a stream or an array of streams but is neither",
            None,
        ));
        diagnostics.push(Diagnostic::error("bad xref", Some(13)));

        let (warnings, errors) = emit_diagnostics(&diagnostics, 0, &logger, "qpdf", "input.pdf")
            .expect("diagnostics should be delivered");
        assert!(warnings);
        assert!(errors);
        emit_warning(&logger, "input.pdf", "linearization warning").unwrap();
        emit_error(
            &logger,
            "qpdf",
            "input.pdf",
            &Error::Internal("fatal".to_owned()),
        )
        .unwrap();

        let output = String::from_utf8(output.lock().expect("capture output").clone()).unwrap();
        assert!(output.contains("WARNING: input.pdf (offset 12): xref warning\n"));
        assert!(output.contains("WARNING: input.pdf (object 5 0, offset 232): expected endobj\n"));
        assert!(output.contains("qpdf: input.pdf (offset 13): bad xref\n"));
        assert!(output.contains("WARNING: input.pdf: linearization warning\n"));
        assert!(output.contains(
            "WARNING: page object 3 0:  object is supposed to be a stream or an array of streams but is neither\n"
        ));
        assert!(!output
            .contains("WARNING: input.pdf: page object 3 0: object is supposed to be a stream"));
        assert!(output.contains("qpdf: input.pdf: fatal\n"));
    }

    #[test]
    fn document_check_reports_rootless_input_before_banner() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let logger = logger_with_capture(Arc::clone(&output));
        let mut pdf =
            Pdf::open(Cursor::new(missing_root_pdf_bytes())).expect("fixture should open");

        let result = check_document(&mut pdf, &logger, "qpdf", "rootless.pdf");

        assert!(matches!(result, Err(CheckError::ErrorsDetected)));
        let output = String::from_utf8(output.lock().expect("capture output").clone()).unwrap();
        assert_eq!(
            output,
            "qpdf: rootless.pdf: unable to find /Root dictionary\n"
        );
    }

    #[test]
    fn document_check_accepts_direct_root_fixture_like_qpdf() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let logger = logger_with_capture(Arc::clone(&output));
        let mut pdf = Pdf::open(Cursor::new(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/compat/direct-root-adbe.pdf"
        ))))
        .expect("direct-root fixture should open");

        let outcome = check_document(&mut pdf, &logger, "qpdf", "direct-root-adbe.pdf")
            .expect("direct-root fixture should check cleanly");

        assert!(!outcome.warnings);
        let output = String::from_utf8(output.lock().expect("capture output").clone()).unwrap();
        assert_eq!(
            output,
            concat!(
                "checking direct-root-adbe.pdf\n",
                "PDF Version: 1.7 extension level 8\n",
                "File is not encrypted\n",
                "File is not linearized\n",
                "No syntax or stream encoding errors found; the file may still contain\n",
                "errors that qpdf cannot detect\n",
            )
        );
    }

    #[test]
    fn document_check_repairs_invalid_catalog_type_on_the_live_root() {
        let mut bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/compat/one-page.pdf"
        ))
        .to_vec();
        let marker = b"/Type /Catalog";
        let start = bytes
            .windows(marker.len())
            .position(|window| window == marker)
            .expect("one-page fixture has a Catalog type");
        bytes[start..start + marker.len()].copy_from_slice(b"/Type /Catxxxx");

        let output = Arc::new(Mutex::new(Vec::new()));
        let logger = logger_with_capture(Arc::clone(&output));
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("invalid Catalog type should open");

        let outcome = check_document(&mut pdf, &logger, "qpdf", "invalid-catalog.pdf")
            .expect("invalid Catalog type is a check warning");
        assert!(outcome.warnings);
        let root = pdf.root_handle().expect("check mode keeps a live Catalog");
        assert!(root
            .try_get_key(b"/Type")
            .expect("Catalog type lookup")
            .try_is_name_and_equals(b"Catalog")
            .expect("Catalog type inspection"));
        let output = String::from_utf8(output.lock().expect("capture output").clone()).unwrap();
        assert_eq!(
            output
                .matches("catalog /Type entry missing or invalid")
                .count(),
            1
        );
    }

    #[test]
    fn document_check_reports_extension_level_and_linearization_warning() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let logger = logger_with_capture(Arc::clone(&output));
        let mut pdf =
            Pdf::open(Cursor::new(extension_level_pdf_bytes())).expect("fixture should open");
        let outcome = check_document(&mut pdf, &logger, "qpdf", "extension.pdf")
            .expect("extension fixture should check cleanly");
        assert!(!outcome.warnings);
        let output = String::from_utf8(output.lock().expect("capture output").clone()).unwrap();
        assert!(output.contains("PDF Version: 1.7 extension level 8\n"));

        let output = Arc::new(Mutex::new(Vec::new()));
        let logger = logger_with_capture(Arc::clone(&output));
        let mut linearized = Pdf::open(Cursor::new(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/compat/linearized-one-page.pdf"
        ))))
        .expect("linearized fixture should open");
        let candidate = linearized
            .linearization_candidate_ref()
            .expect("candidate probe should work")
            .expect("fixture should have a linearization object");
        let candidate_handle = linearized.get_object_handle(candidate);
        candidate_handle
            .try_dereference()
            .expect("candidate should resolve");
        candidate_handle
            .replace_key(b"/O", ObjectHandle::integer(999))
            .expect("candidate should be mutable");

        let outcome = check_document(&mut linearized, &logger, "qpdf", "linearized.pdf")
            .expect("linearization errors are warnings");
        assert!(outcome.warnings);
        let output = String::from_utf8(output.lock().expect("capture output").clone()).unwrap();
        assert!(output.contains("WARNING: linearized.pdf: first page object (/O) mismatch\n"));
    }

    #[test]
    fn document_check_omits_zero_adobe_extension_level_like_qpdf() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let logger = logger_with_capture(Arc::clone(&output));
        let mut pdf = Pdf::open(Cursor::new(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/compat/linearize-indirect-extensions.pdf"
        ))))
        .expect("zero extension-level fixture should open");

        let outcome = check_document(
            &mut pdf,
            &logger,
            "qpdf",
            "linearize-indirect-extensions.pdf",
        )
        .expect("zero extension-level fixture should check cleanly");

        assert!(!outcome.warnings);
        let output = String::from_utf8(output.lock().expect("capture output").clone()).unwrap();
        assert_eq!(
            output,
            concat!(
                "checking linearize-indirect-extensions.pdf\n",
                "PDF Version: 1.4\n",
                "File is not encrypted\n",
                "File is not linearized\n",
                "No syntax or stream encoding errors found; the file may still contain\n",
                "errors that qpdf cannot detect\n",
            )
        );
    }

    fn check_linearized_candidate_warning(key: &[u8], value: ObjectHandle) -> String {
        let output = Arc::new(Mutex::new(Vec::new()));
        let logger = logger_with_capture(Arc::clone(&output));
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/compat/linearized-one-page.pdf"
            ))
            .to_vec(),
        ))
        .expect("linearized fixture should open");
        let candidate = pdf
            .linearization_candidate_ref()
            .expect("candidate probe should work")
            .expect("fixture should have a linearization object");
        let candidate_handle = pdf.get_object_handle(candidate);
        candidate_handle
            .try_dereference()
            .expect("candidate should resolve");
        candidate_handle
            .replace_key(key, value)
            .expect("candidate should be mutable");

        let outcome = check_document(&mut pdf, &logger, "qpdf", "linearized.pdf")
            .expect("linearization errors are warnings");
        assert!(outcome.warnings);
        let captured = output.lock().expect("capture output").clone();
        String::from_utf8(captured).unwrap()
    }

    #[test]
    fn document_check_uses_qpdf_first_page_object_warning() {
        let output = check_linearized_candidate_warning(b"/O", ObjectHandle::integer(7));

        assert!(output.contains("WARNING: linearized.pdf: first page object (/O) mismatch\n"));
        assert!(!output.contains("linearization check failed:"));
    }

    #[test]
    fn document_check_continues_after_o_warning_and_reports_t_warning() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let logger = logger_with_capture(Arc::clone(&output));
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/compat/linearized-one-page.pdf"
            ))
            .to_vec(),
        ))
        .expect("linearized fixture should open");
        let candidate = pdf
            .linearization_candidate_ref()
            .expect("candidate probe should work")
            .expect("fixture should have a linearization object");
        let candidate_handle = pdf.get_object_handle(candidate);
        candidate_handle
            .try_dereference()
            .expect("candidate should resolve");
        candidate_handle
            .replace_key(b"/O", ObjectHandle::integer(7))
            .expect("candidate should be mutable");
        candidate_handle
            .replace_key(b"/T", ObjectHandle::integer(1522))
            .expect("candidate should be mutable");

        let outcome = check_document(&mut pdf, &logger, "qpdf", "linearized.pdf")
            .expect("linearization mismatches are warnings");
        assert!(outcome.warnings);
        let output = String::from_utf8(output.lock().expect("capture output").clone()).unwrap();
        assert!(output.contains("WARNING: linearized.pdf: first page object (/O) mismatch\n"));
        assert!(
            output.contains("WARNING: linearized.pdf: space before first xref item (/T) mismatch"),
            "{output}"
        );
    }

    #[test]
    fn document_check_reports_qpdf_offset_for_n_mismatch() {
        let output = check_linearized_candidate_warning(b"/N", ObjectHandle::integer(2));

        assert!(output.contains(
            "WARNING: linearized.pdf: error encountered while checking linearization data: "
        ));
        assert!(output.contains(
            "linearized.pdf (linearization hint table, offset 908): /N does not match number of pages"
        ), "{output}");
    }

    #[test]
    fn document_check_reports_qpdf_offset_for_linearization_dictionary_type() {
        let output = check_linearized_candidate_warning(b"/P", ObjectHandle::name(b"Bad".to_vec()));

        assert!(output.contains(
            "linearized.pdf (linearization dictionary, offset 23): some keys in linearization dictionary are of the wrong type"
        ), "{output}");
    }

    #[test]
    fn document_check_uses_qpdf_page_count_warning() {
        let output = check_linearized_candidate_warning(b"/N", ObjectHandle::integer(2));

        assert!(output.contains(
            "WARNING: linearized.pdf: error encountered while checking linearization data: \
             linearized.pdf (linearization hint table, offset 908): /N does not match number of pages\n"
        ));
        assert!(!output.contains("linearization check failed:"));
    }

    #[test]
    fn document_check_uses_qpdf_linearization_dictionary_type_warning() {
        let output = check_linearized_candidate_warning(b"/P", ObjectHandle::name(b"Bad".to_vec()));

        assert!(output.contains(
            "WARNING: linearized.pdf: error encountered while checking linearization data: \
             linearized.pdf (linearization dictionary, offset 23): some keys in linearization dictionary are of the wrong type\n"
        ));
        assert!(!output.contains("/P is present but is neither an integer nor null"));
    }

    #[test]
    fn document_check_accepts_integer_linearization_p() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let logger = logger_with_capture(Arc::clone(&output));
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/compat/linearized-one-page.pdf"
            ))
            .to_vec(),
        ))
        .expect("linearized fixture should open");
        let candidate = pdf
            .linearization_candidate_ref()
            .expect("candidate probe should work")
            .expect("fixture should have a linearization object");
        let candidate_handle = pdf.get_object_handle(candidate);
        candidate_handle
            .try_dereference()
            .expect("candidate should resolve");
        candidate_handle
            .replace_key(b"/P", ObjectHandle::integer(-1))
            .expect("candidate should be mutable");

        let outcome = check_document(&mut pdf, &logger, "qpdf", "linearized.pdf")
            .expect("integer /P should be accepted");
        assert!(!outcome.warnings);
        let captured = output.lock().expect("capture output").clone();
        let output = String::from_utf8(captured).unwrap();
        assert!(!output.contains("linearization data"));
    }

    #[test]
    fn document_check_keeps_strict_linearization_warnings_after_parameter_preflight() {
        let output = check_linearized_candidate_warning(
            b"/H",
            ObjectHandle::array(vec![ObjectHandle::integer(601)]),
        );

        assert!(
            output.contains(
                "WARNING: linearized.pdf: error encountered while checking linearization data: \
                 linearization check failed: /H has the wrong number of items (expected 2 or 4, got 1)\n"
            ),
            "{output}"
        );
    }

    #[test]
    fn document_check_downgrades_linearization_parameter_probe_failure() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let logger = logger_with_capture(Arc::clone(&output));
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/compat/linearized-one-page.pdf"
            ))
            .to_vec(),
        ))
        .expect("linearized fixture should open");

        let root = pdf.get_object_handle(pdf.root_ref().expect("root should exist"));
        root.try_dereference().expect("root should resolve");
        let pages_ref = root
            .try_get_key(b"/Pages")
            .expect("pages key should resolve")
            .object_ref()
            .expect("pages should be indirect");
        let pages = pdf.get_object_handle(pages_ref);
        pages.try_dereference().expect("pages should resolve");
        pages
            .replace_key(b"/Kids", ObjectHandle::array(vec![pages.clone()]))
            .expect("pages should be mutable");

        assert!(check_linearization_parameters(&mut pdf).is_err());

        let _ = check_document(&mut pdf, &logger, "qpdf", "linearized.pdf");
        let output = String::from_utf8(output.lock().expect("capture output").clone()).unwrap();
        assert!(output.contains(
            "WARNING: linearized.pdf: error encountered while checking linearization data:"
        ));
    }

    #[test]
    fn linearization_parameter_preflight_accepts_a_non_linearized_document() {
        let mut bytes = b"%PDF-1.4\n".to_vec();
        bytes.extend(std::iter::repeat_n(b' ', 1024));
        let object_offset = bytes.len();
        bytes.extend_from_slice(b"1 0 obj\n1\nendobj\n");
        let xref_offset = bytes.len();
        bytes.extend_from_slice(
            format!("xref\n0 2\n0000000000 65535 f \n{object_offset:010} 00000 n \n").as_bytes(),
        );
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 2 >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("fixture should open");

        assert_eq!(
            check_linearization_parameters(&mut pdf).expect("preflight should complete"),
            LinearizationParameterCheck::Clean
        );
    }

    #[test]
    fn linearization_parameter_preflight_accepts_a_non_dictionary_candidate() {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/compat/linearized-one-page.pdf"
            ))
            .to_vec(),
        ))
        .expect("linearized fixture should open");
        let candidate = pdf
            .linearization_candidate_ref()
            .expect("candidate probe should work")
            .expect("fixture should have a linearization object");
        pdf.replace_object(candidate, ObjectHandle::integer(1))
            .expect("candidate should be replaceable");

        assert_eq!(
            check_linearization_parameters(&mut pdf).expect("preflight should complete"),
            LinearizationParameterCheck::Clean
        );
    }

    #[test]
    fn linearization_parameter_preflight_accepts_an_empty_page_tree() {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/compat/linearized-one-page.pdf"
            ))
            .to_vec(),
        ))
        .expect("linearized fixture should open");
        let candidate = pdf
            .linearization_candidate_ref()
            .expect("candidate probe should work")
            .expect("fixture should have a linearization object");
        let candidate_handle = pdf.get_object_handle(candidate);
        candidate_handle
            .try_dereference()
            .expect("candidate should resolve");
        candidate_handle
            .replace_key(b"/N", ObjectHandle::integer(0))
            .expect("candidate should be mutable");

        let root = pdf.get_object_handle(pdf.root_ref().expect("root should exist"));
        let pages_value = root
            .try_get_key(b"/Pages")
            .expect("pages key should resolve");
        let pages_ref = pages_value
            .object_ref()
            .or_else(|| pages_value.object_ref())
            .expect("pages should be indirect");
        let pages = pdf.get_object_handle(pages_ref);
        pages.try_dereference().expect("pages should resolve");
        pages
            .replace_key(b"/Kids", ObjectHandle::array(Vec::new()))
            .expect("pages should be mutable");
        pages
            .replace_key(b"/Count", ObjectHandle::integer(0))
            .expect("pages should be mutable");

        assert_eq!(
            check_linearization_parameters(&mut pdf).expect("preflight should complete"),
            LinearizationParameterCheck::Clean
        );
    }

    #[test]
    fn document_check_turns_linearization_source_read_failure_into_a_warning() {
        let armed = Arc::new(AtomicBool::new(false));
        let snapshot = Arc::new(AtomicBool::new(false));
        let start_zero_seeks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut pdf = Pdf::open(SnapshotFailReader {
            reader: Cursor::new(
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../tests/fixtures/compat/linearized-one-page.pdf"
                ))
                .to_vec(),
            ),
            armed: Arc::clone(&armed),
            snapshot: Arc::clone(&snapshot),
            start_zero_seeks: Arc::clone(&start_zero_seeks),
        })
        .expect("linearized fixture should open");
        armed.store(true, Ordering::Relaxed);
        let output = Arc::new(Mutex::new(Vec::new()));
        let logger = logger_with_capture(Arc::clone(&output));

        let result = check_document(&mut pdf, &logger, "qpdf", "snapshot-failure.pdf");
        assert!(matches!(result, Err(CheckError::ErrorsDetected)));
        let output = String::from_utf8(output.lock().expect("capture output").clone()).unwrap();
        assert!(output.contains(
            "WARNING: snapshot-failure.pdf: error encountered while checking linearization data:"
        ));
    }

    #[test]
    fn document_check_maps_reader_writer_and_page_failures() {
        let fail = Arc::new(AtomicBool::new(false));
        let mut pdf = Pdf::open(ToggleReader {
            reader: Cursor::new(
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../tests/fixtures/minimal.pdf"
                ))
                .to_vec(),
            ),
            fail: Arc::clone(&fail),
        })
        .expect("fixture should open");
        fail.store(true, Ordering::Relaxed);
        let mut read_probe = ToggleReader {
            reader: Cursor::new(Vec::new()),
            fail: Arc::clone(&fail),
        };
        assert!(read_probe.read(&mut [0u8; 1]).is_err());
        let output = Arc::new(Mutex::new(Vec::new()));
        let logger = logger_with_capture(Arc::clone(&output));
        assert!(matches!(
            check_document(&mut pdf, &logger, "qpdf", "reader-failure.pdf"),
            Err(CheckError::ErrorsDetected)
        ));

        let output = Arc::new(Mutex::new(Vec::new()));
        let logger = logger_with_capture(Arc::clone(&output));
        let mut writer_pdf = Pdf::open(Cursor::new(corrupt_content_stream_pdf_bytes()))
            .expect("corrupt stream fixture should open");
        assert!(matches!(
            check_document(&mut writer_pdf, &logger, "qpdf", "writer-failure.pdf"),
            Err(CheckError::ErrorsDetected)
        ));

        let output = Arc::new(Mutex::new(Vec::new()));
        let logger = logger_with_capture(Arc::clone(&output));
        let mut page_tree_pdf =
            Pdf::open(Cursor::new(page_tree_warning_pdf_bytes())).expect("fixture should open");
        let failing_pdf_logger = QPDFLogger::create();
        failing_pdf_logger.set_output_streams(None, Some(PipelineHandle::new(FailingCapture)));
        page_tree_pdf.set_logger(failing_pdf_logger);
        assert!(matches!(
            check_document(&mut page_tree_pdf, &logger, "qpdf", "page-tree-failure.pdf"),
            Err(CheckError::Operation(Error::System(message))) if message == "logger failure"
        ));

        let output = Arc::new(Mutex::new(Vec::new()));
        let logger = logger_with_capture(Arc::clone(&output));
        let mut page_pdf = Pdf::open(Cursor::new(single_page_content_pdf_bytes(b"<< /A 1")))
            .expect("malformed content fixture should open");
        assert!(matches!(
            check_document(&mut page_pdf, &logger, "qpdf", "page-failure.pdf"),
            Err(CheckError::ErrorsDetected)
        ));
        let output = String::from_utf8(output.lock().expect("capture output").clone()).unwrap();
        assert!(output.contains("ERROR: page 1:"));

        let mapped = map_page_tree_error(
            &logger,
            "qpdf",
            "page-tree-failure.pdf",
            Error::Internal("page tree failure".to_owned()),
            false,
        );
        assert!(matches!(mapped, CheckError::ErrorsDetected));
        let failing_logger = QPDFLogger::create();
        failing_logger.set_output_streams(None, Some(PipelineHandle::new(FailingCapture)));
        let mapped = map_page_tree_error(
            &failing_logger,
            "qpdf",
            "page-tree-failure.pdf",
            Error::Internal("page tree failure".to_owned()),
            true,
        );
        assert!(matches!(
            mapped,
            CheckError::Operation(Error::Internal(message)) if message == "page tree failure"
        ));
    }

    #[test]
    fn diagnostic_delivery_errors_cross_the_job_check_boundary() {
        let pdf = Pdf::open(Cursor::new(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/test_driver/missing_startxref.pdf"
        ))))
        .expect("warning fixture should open");
        assert!(!pdf.repair_diagnostics().entries().is_empty());
        let logger = QPDFLogger::create();
        logger.set_output_streams(None, Some(PipelineHandle::new(FailingCapture)));

        let result = emit_new_diagnostics(&pdf, 0, &logger, "qpdf", "broken.pdf");

        assert!(matches!(
            result,
            Err(CheckError::Operation(Error::System(message))) if message == "logger failure"
        ));
    }

    #[test]
    fn document_check_propagates_a_page_tree_warning_logger_failure() {
        let mut pdf = Pdf::open(Cursor::new(page_tree_warning_pdf_bytes()))
            .expect("page-tree fixture should open");
        let document_logger = QPDFLogger::create();
        document_logger.set_warn(Some(PipelineHandle::new(
            crate::pipeline::test_support::NthWriteFailure::new(1),
        )));
        pdf.set_logger(document_logger);

        let report_output = Arc::new(Mutex::new(Vec::new()));
        let report_logger = logger_with_capture(Arc::clone(&report_output));
        let result = check_document(&mut pdf, &report_logger, "qpdf", "page-tree.pdf");

        assert!(matches!(
            &result,
            Err(CheckError::Operation(Error::System(message)))
                if message == "sink write failure 1"
        ));
    }

    #[test]
    fn document_check_propagates_a_lazy_extension_logger_failure() {
        let mut pdf = Pdf::open(Cursor::new(lazy_extension_warning_pdf_bytes()))
            .expect("lazy extension fixture should open");
        let document_logger = QPDFLogger::create();
        document_logger.set_warn(Some(PipelineHandle::new(
            crate::pipeline::test_support::NthWriteFailure::new(1),
        )));
        pdf.set_logger(document_logger);

        let report_output = Arc::new(Mutex::new(Vec::new()));
        let report_logger = logger_with_capture(Arc::clone(&report_output));
        let result = check_document(&mut pdf, &report_logger, "qpdf", "extension.pdf");

        assert!(matches!(
            &result,
            Err(CheckError::Operation(Error::System(message)))
                if message == "sink write failure 1"
        ));
    }

    #[test]
    fn document_check_reports_a_linearization_probe_operation_failure() {
        let failure = Arc::new(AtomicBool::new(false));
        let mut pdf = Pdf::open(ToggleReader {
            reader: Cursor::new(
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../tests/fixtures/compat/linearized-one-page.pdf"
                ))
                .to_vec(),
            ),
            fail: Arc::clone(&failure),
        })
        .expect("linearized fixture should open");
        pdf.root_handle()
            .expect("Catalog should resolve before failure");
        failure.store(true, Ordering::Relaxed);

        let output = Arc::new(Mutex::new(Vec::new()));
        let logger = logger_with_capture(Arc::clone(&output));
        let result = check_document(&mut pdf, &logger, "qpdf", "probe-failure.pdf");

        assert!(matches!(result, Err(CheckError::ErrorsDetected)));
        let output = String::from_utf8(output.lock().expect("capture output").clone()).unwrap();
        assert!(output.contains("probe-failure.pdf: I/O error: test reader failure"));
    }

    #[test]
    fn document_check_propagates_a_page_content_warning_logger_failure() {
        let mut pdf = Pdf::open(Cursor::new(single_page_content_pdf_bytes(b"q\nQ\n")))
            .expect("content fixture should open");
        let page = pdf.get_object_handle(ObjectRef::new(3, 0));
        page.try_dereference().expect("page should resolve");
        page.replace_key(b"/Contents", ObjectHandle::integer(42))
            .expect("page should be mutable");
        pdf.mark_object_handle_dirty(&page)
            .expect("page mutation should be tracked");
        let document_logger = QPDFLogger::create();
        document_logger.set_warn(Some(PipelineHandle::new(
            crate::pipeline::test_support::NthWriteFailure::new(1),
        )));
        pdf.set_logger(document_logger);

        let report_output = Arc::new(Mutex::new(Vec::new()));
        let report_logger = logger_with_capture(Arc::clone(&report_output));
        let result = check_document(&mut pdf, &report_logger, "qpdf", "content.pdf");

        assert!(matches!(
            &result,
            Err(CheckError::Operation(Error::System(message)))
                if message == "sink write failure 1"
        ));
    }

    #[test]
    fn document_check_propagates_linearization_page_warning_logger_failure() {
        let mut pdf = Pdf::open(Cursor::new(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/compat/linearized-one-page.pdf"
        ))))
        .expect("linearized fixture should open");
        let root = pdf.root_handle().expect("Catalog should resolve");
        let pages = root
            .try_get_key(b"/Pages")
            .expect("Catalog /Pages should exist");
        let pages_ref = pages.object_ref().expect("/Pages should be indirect");
        let pages = pdf.get_object_handle(pages_ref);
        pages
            .try_dereference()
            .expect("page-tree root should resolve");
        pages
            .replace_key(b"/Type", ObjectHandle::name(b"NotPages".to_vec()))
            .expect("page-tree root should be mutable");

        let document_logger = QPDFLogger::create();
        document_logger.set_warn(Some(PipelineHandle::new(
            crate::pipeline::test_support::NthWriteFailure::new(1),
        )));
        pdf.set_logger(document_logger);

        let report_output = Arc::new(Mutex::new(Vec::new()));
        let report_logger = logger_with_capture(Arc::clone(&report_output));
        let result = check_document(&mut pdf, &report_logger, "qpdf", "linearized.pdf");

        assert!(matches!(
            &result,
            Err(CheckError::Operation(Error::System(message)))
                if message == "sink write failure 1"
        ));
    }

    #[test]
    fn is_linearized_propagates_candidate_warning_logger_failure() {
        let mut pdf = Pdf::open(Cursor::new(linearization_candidate_warning_pdf_bytes()))
            .expect("candidate-warning fixture should open");
        let logger = QPDFLogger::create();
        logger.set_warn(Some(PipelineHandle::new(
            crate::pipeline::test_support::NthWriteFailure::new(1),
        )));
        pdf.set_logger(logger);

        let result = pdf.is_linearized();

        assert!(
            matches!(
                &result,
                Err(Error::System(message)) if message == "sink write failure 1"
            ),
            "{result:?}"
        );
    }

    #[test]
    fn linearization_warning_checker_propagates_logger_failure() {
        let mut pdf = Pdf::open(Cursor::new(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/compat/linearized-one-page.pdf"
        ))))
        .expect("linearized fixture should open");
        let root = pdf.root_handle().expect("Catalog should resolve");
        let pages = root
            .try_get_key(b"/Pages")
            .expect("Catalog /Pages should exist");
        let pages_ref = pages.object_ref().expect("/Pages should be indirect");
        let pages = pdf.get_object_handle(pages_ref);
        pages
            .try_dereference()
            .expect("page-tree root should resolve");
        pages
            .replace_key(b"/Type", ObjectHandle::name(b"NotPages".to_vec()))
            .expect("page-tree root should be mutable");
        let source_bytes = pdf.source_bytes().expect("source bytes");

        let document_logger = QPDFLogger::create();
        document_logger.set_warn(Some(PipelineHandle::new(
            crate::pipeline::test_support::NthWriteFailure::new(1),
        )));
        pdf.set_logger(document_logger);

        let report_output = Arc::new(Mutex::new(Vec::new()));
        let report_logger = logger_with_capture(Arc::clone(&report_output));
        let result = emit_linearization_check_warnings_with_suppression(
            &mut pdf,
            &source_bytes,
            &report_logger,
            "linearized.pdf",
            false,
            false,
        );

        assert!(
            matches!(
                &result,
                Err(Error::System(message)) if message == "sink write failure 1"
            ),
            "{result:?}"
        );
    }

    #[test]
    fn linearization_warning_checker_downgrades_a_non_logger_operation_error() {
        let failure = Arc::new(AtomicBool::new(false));
        let mut pdf = Pdf::open(ToggleReader {
            reader: Cursor::new(
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../tests/fixtures/compat/linearized-one-page.pdf"
                ))
                .to_vec(),
            ),
            fail: Arc::clone(&failure),
        })
        .expect("linearized fixture should open");
        failure.store(true, Ordering::Relaxed);
        let source_bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/compat/linearized-one-page.pdf"
        ));
        let output = Arc::new(Mutex::new(Vec::new()));
        let logger = logger_with_capture(Arc::clone(&output));

        let result = emit_linearization_check_warnings_with_suppression(
            &mut pdf,
            source_bytes,
            &logger,
            "linearized.pdf",
            false,
            false,
        );

        assert!(matches!(result, Ok(true)));
        let output = String::from_utf8(output.lock().expect("capture output").clone()).unwrap();
        assert!(output.contains("I/O error: I/O error: test reader failure"));
    }

    #[test]
    fn operation_error_helpers_keep_non_logger_errors_reportable() {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/minimal.pdf"
            ))
            .to_vec(),
        ))
        .expect("minimal fixture should open");

        assert!(take_logger_failure(LinearizationCheckError::NotLinearized, &pdf, 0,).is_none());
        assert!(take_logger_failure(
            LinearizationCheckError::Io(Box::new(io::Error::other("disk gone"))),
            &pdf,
            0,
        )
        .is_none());
        assert!(take_logger_failure(
            LinearizationCheckError::Io(Box::new(Error::System("operation".to_owned()))),
            &pdf,
            0,
        )
        .is_none());

        let failing_logger = QPDFLogger::create();
        failing_logger.set_output_streams(None, Some(PipelineHandle::new(FailingCapture)));
        let mapped = map_check_error(
            &failing_logger,
            "qpdf",
            "input.pdf",
            Error::parse(0, "malformed"),
            false,
        );
        assert!(matches!(
            mapped,
            CheckError::Operation(Error::System(message)) if message == "logger failure"
        ));

        pdf.set_suppress_warnings(true);
        assert!(!logger_failure_since(&pdf, 0));
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
    fn encrypted_document_check_uses_qpdf_show_encryption_report() {
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
        let mut pdf = job
            .open(
                Cursor::new(include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../tests/fixtures/encrypted/v2-rc4-128-r3.pdf"
                ))),
                "encrypted.pdf",
                PdfOpenOptions {
                    password: b"user-v2".to_vec(),
                    ..PdfOpenOptions::default()
                },
            )
            .expect("encrypted fixture should open");

        let status = job.check(&mut pdf).expect("check should succeed");

        assert_eq!(status, JobExitCode::Success);
        assert_eq!(
            *info.lock().expect("info capture"),
            concat!(
                "checking encrypted.pdf\n",
                "PDF Version: 1.7\n",
                "R = 3\n",
                "P = -4\n",
                "User password = user-v2\n",
                "Supplied password is user password\n",
                "extract for accessibility: allowed\n",
                "extract for any purpose: allowed\n",
                "print low resolution: allowed\n",
                "print high resolution: allowed\n",
                "modify document assembly: allowed\n",
                "modify forms: allowed\n",
                "modify annotations: allowed\n",
                "modify other: allowed\n",
                "modify anything: allowed\n",
                "File is not linearized\n",
                "No syntax or stream encoding errors found; the file may still contain\n",
                "errors that qpdf cannot detect\n",
            )
            .as_bytes()
        );
    }

    #[test]
    fn hex_key_opened_document_check_never_reports_incorrect_password() {
        // Regression test: a hex-key open never sets `user_password_matched`
        // or `owner_password_matched`, which used to make check()'s
        // encryption report wrongly print "Incorrect password supplied" for
        // a successfully-authenticated document. qpdf's `doCheck` calls
        // `showEncryption` directly (`QPDFJob.cc:744-765`) and that function
        // has no branch that emits this line (`QPDFJob.cc:700-742`); verified
        // byte-identical against `qpdf --check --password-is-hex-key
        // --password=<key> v5-aes-256-r6.pdf` (qpdf 11.9.0).
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
        let mut pdf = job
            .open(
                Cursor::new(include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../tests/fixtures/encrypted/v5-aes-256-r6.pdf"
                ))),
                "encrypted.pdf",
                PdfOpenOptions {
                    password: b"fc459408a5282b7c59daa5162f860e82315679cc04942ef57993bfd287f30290"
                        .to_vec(),
                    password_is_hex_key: true,
                    ..PdfOpenOptions::default()
                },
            )
            .expect("hex-key open should succeed");

        let status = job.check(&mut pdf).expect("check should succeed");

        assert_eq!(status, JobExitCode::Success);
        assert_eq!(
            *info.lock().expect("info capture"),
            concat!(
                "checking encrypted.pdf\n",
                "PDF Version: 1.7 extension level 8\n",
                "R = 6\n",
                "P = -4\n",
                "User password = \n",
                "extract for accessibility: allowed\n",
                "extract for any purpose: allowed\n",
                "print low resolution: allowed\n",
                "print high resolution: allowed\n",
                "modify document assembly: allowed\n",
                "modify forms: allowed\n",
                "modify annotations: allowed\n",
                "modify other: allowed\n",
                "modify anything: allowed\n",
                "stream encryption method: AESv3\n",
                "string encryption method: AESv3\n",
                "file encryption method: AESv3\n",
                "File is not linearized\n",
                "No syntax or stream encoding errors found; the file may still contain\n",
                "errors that qpdf cannot detect\n",
            )
            .as_bytes()
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
            Ok(_) => panic!("fixture must fail during repair"), // cov:ignore: the repair-failure fixture must take the Err branch
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

    #[test]
    fn failed_open_warnings_are_silent_under_no_warn() {
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
        job.set_suppress_warnings(true);
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
            Ok(_) => panic!("fixture must fail during repair"), // cov:ignore: the repair-failure fixture must take the Err branch
            Err(error) => error,
        };

        job.report_open_failure(&error)
            .expect("suppressed replay should still succeed");

        assert!(
            warnings.lock().expect("warning capture").is_empty(),
            "--no-warn must suppress open-failure repair diagnostics entirely"
        );
    }
}
