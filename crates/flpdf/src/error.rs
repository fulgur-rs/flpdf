//! qpdf correspondence: QPDFExc.cc and QPDFSystemError.cc concepts combined with flpdf-specific errors; public APIs are incomplete.
use crate::encryption::primitives::PrimitiveError;
use thiserror::Error;

/// Crate-wide [`std::result::Result`] specialization.
pub type Result<T> = std::result::Result<T, Error>;

/// A qpdf `QPDFUsage`-class error raised by the job/configuration boundary.
///
/// Usage failures are distinct from PDF capability and I/O failures: qpdf
/// catches `QPDFUsage` separately and sends it through its CLI usage/help
/// exit path before any input or output file is touched.
#[derive(Debug, thiserror::Error)]
#[error("{}", usage_what(.message))]
pub struct UsageError {
    message: Vec<u8>,
}

impl UsageError {
    /// Construct a usage error with the given qpdf-compatible message.
    pub fn new(message: impl AsRef<[u8]>) -> Self {
        Self {
            message: message.as_ref().to_vec(),
        }
    }

    /// Return qpdf's observable `what()` bytes, ending at the first NUL.
    pub fn what_bytes(&self) -> &[u8] {
        &self.message[..self
            .message
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(self.message.len())]
    }
}

fn usage_what(message: &[u8]) -> String {
    String::from_utf8_lossy(
        &message[..message
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(message.len())],
    )
    .into_owned()
}

/// The qpdf error-code family carried by [`QpdfExc`].
///
/// These values mirror `qpdf_error_code_e` in `include/qpdf/Constants.h:84-95`.
/// The code is deliberately separate from the rendered message: qpdf exposes
/// both, and callers must not parse `what()` to recover the classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum QpdfErrorCode {
    /// No error.
    Success = 0,
    /// Logic or programming error (`qpdf_e_internal`).
    Internal,
    /// I/O or other runtime error (`qpdf_e_system`).
    System,
    /// Unsupported PDF feature (`qpdf_e_unsupported`).
    Unsupported,
    /// Incorrect password (`qpdf_e_password`).
    Password,
    /// Malformed or damaged PDF (`qpdf_e_damaged_pdf`).
    DamagedPdf,
    /// Erroneous or unsupported page structure (`qpdf_e_pages`).
    Pages,
    /// Object type or bounds error (`qpdf_e_object`).
    Object,
    /// JSON error (`qpdf_e_json`).
    Json,
    /// Linearization warning (`qpdf_e_linearization`).
    Linearization,
}

/// Structured counterpart of qpdf's public `QPDFExc` exception.
///
/// qpdf stores the error code, source filename, object description, signed
/// file position, and detail as independent values, while `what()` is a
/// derived diagnostic string (`include/qpdf/QPDFExc.hh:29-77`,
/// `libqpdf/QPDFExc.cc:3-51`). Keep each field as bytes because qpdf's
/// `std::string` accepts arbitrary bytes, including embedded NULs.
///
/// [`Self::what_bytes`] intentionally models the observable C-string returned
/// by `std::runtime_error::what()`: it ends at the first NUL. The getters retain
/// the complete original fields, so this distinction is not lost. [`std::fmt::Display`]
/// is only a lossy UTF-8 projection of those observable bytes and must not be
/// used as a replacement for byte-oriented qpdf logging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QpdfExc {
    error_code: QpdfErrorCode,
    filename: Vec<u8>,
    object: Vec<u8>,
    offset: i64,
    message: Vec<u8>,
    what: Vec<u8>,
}

impl QpdfExc {
    /// Construct a qpdf-shaped exception from its independent raw fields.
    pub fn new(
        error_code: QpdfErrorCode,
        filename: impl AsRef<[u8]>,
        object: impl AsRef<[u8]>,
        offset: i64,
        message: impl AsRef<[u8]>,
    ) -> Self {
        let filename = filename.as_ref().to_vec();
        let object = object.as_ref().to_vec();
        let message = message.as_ref().to_vec();
        let what = Self::create_what(&filename, &object, offset, &message);
        Self {
            error_code,
            filename,
            object,
            offset,
            message,
            what,
        }
    }

    /// Return qpdf's independent error code.
    pub fn get_error_code(&self) -> QpdfErrorCode {
        self.error_code
    }

    /// Return the complete original filename bytes.
    pub fn get_filename(&self) -> &[u8] {
        &self.filename
    }

    /// Return the complete original object-description bytes.
    pub fn get_object(&self) -> &[u8] {
        &self.object
    }

    /// Return qpdf's signed file position.
    pub fn get_file_position(&self) -> i64 {
        self.offset
    }

    /// Return the complete original detail-message bytes.
    pub fn get_message_detail(&self) -> &[u8] {
        &self.message
    }

    /// Return the observable `what()` bytes, truncated at the first NUL.
    pub fn what_bytes(&self) -> &[u8] {
        let end = self
            .what
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(self.what.len());
        &self.what[..end]
    }

    fn create_what(filename: &[u8], object: &[u8], offset: i64, message: &[u8]) -> Vec<u8> {
        let mut result = Vec::new();
        if !filename.is_empty() {
            result.extend_from_slice(filename);
        }
        if !(object.is_empty() && offset == 0) {
            if !filename.is_empty() {
                result.extend_from_slice(b" (");
            }
            if !object.is_empty() {
                result.extend_from_slice(object);
                if offset > 0 {
                    result.extend_from_slice(b", ");
                }
            }
            if offset > 0 {
                result.extend_from_slice(b"offset ");
                result.extend_from_slice(offset.to_string().as_bytes());
            }
            if !filename.is_empty() {
                result.push(b')');
            }
        }
        if !result.is_empty() {
            result.extend_from_slice(b": ");
        }
        result.extend_from_slice(message);
        result
    }
}

impl std::fmt::Display for QpdfExc {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&String::from_utf8_lossy(self.what_bytes()))
    }
}

impl std::error::Error for QpdfExc {}

/// Errors produced by the public APIs of `flpdf`.
///
/// Unscoped I/O failures bubble up via [`Error::Io`]. Filesystem operations
/// whose path is part of the public operation use [`Error::FileIo`] so callers
/// retain the operation, path, and source error. Structural problems (malformed
/// tokens, unexpected types, depth limits, oversized fields) use
/// [`Error::Parse`] or [`Error::Unsupported`]. [`Error::Missing`] is reserved
/// for required dictionary entries that the spec mandates, e.g. `/Root` on the
/// trailer.
/// [`Error::Encrypted`] covers all encryption-related failures; its subkind is
/// carried by [`EncryptedError`].
/// [`Error::Usage`] covers qpdf job/configuration usage failures and must be
/// routed through the CLI's usage/help exit path rather than reported as a PDF
/// error.
/// [`Error::Internal`] and [`Error::System`] mirror qpdf's public classification
/// of `std::logic_error` and `std::runtime_error`, respectively.
/// [`Error::SystemBytes`] is the byte-preserving counterpart for qpdf runtime
/// messages whose source description may contain non-UTF-8 bytes.
/// [`Error::OpenFailure`] preserves the terminal source error and accumulated
/// repair diagnostics from a failed permissive open; callers can retrieve both
/// through [`Error::open_failure`].
#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// A filesystem failure whose operation and path are known at this API
    /// boundary.
    #[error("{operation} {}: {source}", path.display())]
    FileIo {
        operation: &'static str,
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("parse error at byte {offset}: {message}")]
    Parse { offset: usize, message: String },

    #[error("unsupported PDF feature: {0}")]
    Unsupported(String),

    /// A qpdf job/configuration usage failure.
    #[error(transparent)]
    Usage(#[from] UsageError),

    #[error("missing required PDF entry: {0}")]
    Missing(&'static str),

    /// A qpdf `qpdf_e_pages` exception from an invalid or unsupported pages
    /// structure. The string is the complete `QPDFExc::what()` location and
    /// message, including the source description and page-object context.
    #[error("{0}")]
    Pages(String),

    #[error("encrypted PDF: {0}")]
    Encrypted(#[from] EncryptedError),

    #[error("{0}")]
    Internal(String),

    #[error("{0}")]
    System(String),

    /// A qpdf runtime error whose complete message must retain arbitrary
    /// source-name bytes. `Display` uses qpdf's lossy text projection for
    /// ordinary Rust callers; byte-oriented command boundaries should use
    /// [`Self::raw_message`].
    #[error("{}", String::from_utf8_lossy(.0))]
    SystemBytes(Vec<u8>),

    /// A terminal permissive-open failure with the qpdf-compatible repair
    /// warnings accumulated before reconstruction failed.
    #[error("{source}")]
    OpenFailure {
        #[source]
        source: Box<Error>,
        diagnostics: crate::Diagnostics,
    },
}

impl Error {
    /// Convenience constructor for [`Error::Parse`].
    pub fn parse(offset: usize, message: impl Into<String>) -> Self {
        Self::Parse {
            offset,
            message: message.into(),
        }
    }

    /// Preserve filesystem operation context without routing it through a
    /// pipeline-specific error type.
    pub(crate) fn file_io(
        operation: &'static str,
        path: impl Into<std::path::PathBuf>,
        source: std::io::Error,
    ) -> Self {
        Self::FileIo {
            operation,
            path: path.into(),
            source,
        }
    }

    /// Rebase a relative [`Error::Parse`] offset onto an absolute position.
    ///
    /// When an error is produced while parsing a sub-slice that begins at
    /// `base` within a larger buffer, its `offset` is relative to that slice.
    /// This shifts it back to an absolute offset (`base + offset`) so
    /// diagnostics point at the true byte position. Non-[`Error::Parse`]
    /// variants are returned unchanged.
    ///
    /// The addition saturates because `base` is not always a position inside a
    /// buffer this crate allocated: the reader's object resolution rebases
    /// onto a cross-reference entry's offset, which is whatever the input
    /// file's xref table or xref stream declared and can be any `u64`. A
    /// wrapping add would panic in a debug build on such a file; saturating
    /// pins the diagnostic at `usize::MAX` instead, which is wrong by the same
    /// amount the declared offset was.
    pub(crate) fn rebase_offset(self, base: usize) -> Self {
        match self {
            Self::Parse { offset, message } => Self::Parse {
                offset: base.saturating_add(offset),
                message,
            },
            other => other,
        }
    }

    /// Return the terminal source error and preceding repair diagnostics for a
    /// failed permissive open.
    pub fn open_failure(&self) -> Option<(&Error, &crate::Diagnostics)> {
        match self {
            Self::OpenFailure {
                source,
                diagnostics,
            } => Some((source.as_ref(), diagnostics)),
            _ => None,
        }
    }

    /// Return the complete qpdf message when this error carries raw bytes.
    ///
    /// A permissive JSON import may wrap the terminal error in
    /// [`Error::OpenFailure`], so this accessor follows that one source edge
    /// while leaving ordinary string errors unchanged.
    pub fn raw_message(&self) -> Option<&[u8]> {
        match self {
            Self::SystemBytes(message) => Some(message),
            Self::Usage(error) => Some(error.what_bytes()),
            Self::OpenFailure { source, .. } => source.raw_message(),
            _ => None,
        }
    }

    /// Attach open-time repair diagnostics to a terminal failure.
    ///
    /// This wrapper is created only after qpdf-compatible repair warnings
    /// exist. Empty diagnostics leave the original error unchanged.
    pub(crate) fn with_open_diagnostics(source: Error, diagnostics: crate::Diagnostics) -> Error {
        if diagnostics.entries().is_empty() {
            source
        } else {
            Self::OpenFailure {
                source: Box::new(source),
                diagnostics,
            }
        }
    }
}

impl From<crate::pipeline::PipelineError> for Error {
    fn from(error: crate::pipeline::PipelineError) -> Self {
        match error {
            crate::pipeline::PipelineError::Logic(message) => {
                Self::Internal(message.into_string_lossy())
            }
            crate::pipeline::PipelineError::Runtime(message) => {
                Self::System(message.into_string_lossy())
            }
        }
    }
}

/// Sub-kind of [`Error::Encrypted`], describing why an encrypted PDF could not
/// be processed.
///
/// Each variant carries enough context for the CLI to emit an actionable
/// diagnostic message. Downstream callers may pattern-match on these variants
/// to decide whether to retry with a different password, refuse processing, etc.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EncryptedError {
    /// The supplied password (or the empty default) was rejected by the security handler.
    #[error("incorrect password")]
    BadPassword,

    /// The `/Encrypt` dictionary specifies a filter or algorithm revision that
    /// `flpdf` does not support.
    #[error("unsupported encryption handler: filter={filter}, V={v}, R={r}, CFM={cfm:?}")]
    UnsupportedHandler {
        /// Value of the `/Filter` key (e.g. `"Standard"`).
        filter: String,
        /// Encryption algorithm version (`/V`).
        v: i64,
        /// Revision of the standard security handler (`/R`).
        r: i64,
        /// Crypt filter method (`/CFM`), if present.
        cfm: Option<String>,
    },

    /// The `/Encrypt` dictionary is structurally invalid or missing required
    /// entries.
    #[error("malformed /Encrypt dictionary: {reason}")]
    Malformed {
        /// Human-readable description of what is missing or invalid.
        reason: String,
    },
}

/// Bridge from the low-level `PrimitiveError` to [`Error::Encrypted`].
///
/// This allows `?`-propagation from `encryption::primitives` functions directly
/// into the public error type without exposing `PrimitiveError` in the public
/// API.
impl From<PrimitiveError> for Error {
    fn from(e: PrimitiveError) -> Self {
        Error::Encrypted(EncryptedError::Malformed {
            reason: format!("primitive: {e}"),
        })
    }
}

impl EncryptedError {
    /// A short machine-readable code suitable for use in diagnostic messages,
    /// e.g. `"encrypted.bad-password"`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::BadPassword => "encrypted.bad-password",
            Self::UnsupportedHandler { .. } => "encrypted.unsupported-handler",
            Self::Malformed { .. } => "encrypted.malformed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qpdf_exc_exposes_all_error_codes_and_preserves_raw_fields() {
        let codes = [
            QpdfErrorCode::Success,
            QpdfErrorCode::Internal,
            QpdfErrorCode::System,
            QpdfErrorCode::Unsupported,
            QpdfErrorCode::Password,
            QpdfErrorCode::DamagedPdf,
            QpdfErrorCode::Pages,
            QpdfErrorCode::Object,
            QpdfErrorCode::Json,
            QpdfErrorCode::Linearization,
        ];
        for (expected, code) in codes.into_iter().enumerate() {
            let error = QpdfExc::new(
                code,
                b"file-\xff\0tail",
                b"object-\0\xfe",
                -1,
                b"detail-\0\xfd",
            );
            assert_eq!(error.get_error_code(), code);
            assert_eq!(error.get_error_code() as u8, expected as u8);
            assert_eq!(error.get_filename(), b"file-\xff\0tail");
            assert_eq!(error.get_object(), b"object-\0\xfe");
            assert_eq!(error.get_file_position(), -1);
            assert_eq!(error.get_message_detail(), b"detail-\0\xfd");
        }
    }

    #[test]
    fn qpdf_exc_what_matches_qpdf_c_string_boundaries_and_display_projection() {
        type QpdfWhatCase<'a> = (&'a str, &'a [u8], &'a [u8], i64, &'a [u8]);
        let cases: &[QpdfWhatCase<'_>] = &[
            ("empty-empty-negative", b"", b"", -1, b"m"),
            ("empty-empty-zero", b"", b"", 0, b"m"),
            ("empty-empty-positive", b"", b"", 7, b"offset 7: m"),
            ("empty-object-negative", b"", b"o", -1, b"o: m"),
            ("empty-object-zero", b"", b"o", 0, b"o: m"),
            ("empty-object-positive", b"", b"o", 7, b"o, offset 7: m"),
            ("filename-empty-negative", b"f", b"", -1, b"f (): m"),
            ("filename-empty-zero", b"f", b"", 0, b"f: m"),
            ("filename-empty-positive", b"f", b"", 7, b"f (offset 7): m"),
            ("filename-object-negative", b"f", b"o", -1, b"f (o): m"),
            ("filename-object-zero", b"f", b"o", 0, b"f (o): m"),
            (
                "filename-object-positive",
                b"f",
                b"o",
                7,
                b"f (o, offset 7): m",
            ),
        ];
        for &(label, filename, object, offset, expected) in cases {
            let error = QpdfExc::new(QpdfErrorCode::DamagedPdf, filename, object, offset, b"m");
            assert_eq!(error.what_bytes(), expected, "{label}");
            assert_eq!(
                error.to_string(),
                String::from_utf8_lossy(expected),
                "{label}"
            );
        }

        let nul_filename = QpdfExc::new(QpdfErrorCode::DamagedPdf, b"f\0x", b"o", 7, b"m");
        assert_eq!(nul_filename.what_bytes(), b"f");
        assert_eq!(nul_filename.to_string(), "f");

        let nul_object = QpdfExc::new(QpdfErrorCode::DamagedPdf, b"f", b"o\0y", 7, b"m");
        assert_eq!(nul_object.what_bytes(), b"f (o");

        let nul_message = QpdfExc::new(QpdfErrorCode::DamagedPdf, b"f", b"o", 7, b"m\0z");
        assert_eq!(nul_message.what_bytes(), b"f (o, offset 7): m");
    }
    use crate::encryption::primitives::PrimitiveError;
    use crate::Diagnostic;
    use std::error::Error as _;

    #[test]
    fn pipeline_logic_error_maps_to_qpdf_internal_category() {
        let public: Error =
            crate::pipeline::PipelineError::logic("Pl_Buffer::getBuffer() called when not ready")
                .into();
        assert!(matches!(
            public,
            Error::Internal(ref message)
                if message == "Pl_Buffer::getBuffer() called when not ready"
        ));
    }

    #[test]
    fn pipeline_runtime_error_maps_to_qpdf_system_category() {
        let public: Error =
            crate::pipeline::PipelineError::runtime("inflate: inflate: data: corrupt stream")
                .into();
        assert!(matches!(
            public,
            Error::System(ref message)
                if message == "inflate: inflate: data: corrupt stream"
        ));
    }

    #[test]
    fn open_failure_delegates_display_and_error_source() {
        let source = Error::parse(7, "terminal repair failure");
        let mut diagnostics = crate::Diagnostics::default();
        diagnostics.push(Diagnostic::warning("repair warning", None));

        let error = Error::with_open_diagnostics(source, diagnostics);

        assert_eq!(
            error.to_string(),
            "parse error at byte 7: terminal repair failure"
        );
        assert_eq!(
            std::error::Error::source(&error)
                .expect("wrapped error source")
                .to_string(),
            "parse error at byte 7: terminal repair failure"
        );
        let (source, diagnostics) = error.open_failure().expect("open failure wrapper");
        assert_eq!(
            source.to_string(),
            "parse error at byte 7: terminal repair failure"
        );
        assert_eq!(diagnostics.entries().len(), 1);
    }

    #[test]
    fn empty_open_diagnostics_leave_the_original_error_unwrapped() {
        let error =
            Error::with_open_diagnostics(Error::parse(9, "plain failure"), Default::default());

        assert!(matches!(error, Error::Parse { offset: 9, .. }));
        assert!(error.open_failure().is_none());
        assert!(error.source().is_none());
    }

    #[test]
    fn system_bytes_preserves_raw_message_and_open_failure_source() {
        let raw = b"raw-error-\xff".to_vec();
        let error = Error::SystemBytes(raw.clone());
        assert_eq!(error.raw_message(), Some(raw.as_slice()));
        assert_eq!(error.to_string(), "raw-error-�");

        let mut diagnostics = crate::Diagnostics::default();
        diagnostics.push(Diagnostic::warning("preceding warning", None));
        let wrapped = Error::with_open_diagnostics(error, diagnostics);
        assert_eq!(wrapped.raw_message(), Some(raw.as_slice()));
    }

    #[test]
    fn ordinary_errors_have_no_raw_message() {
        assert!(Error::System("ordinary".to_owned()).raw_message().is_none());
    }

    #[test]
    fn encrypted_error_display_bad_password() {
        let e = EncryptedError::BadPassword;
        assert_eq!(e.to_string(), "incorrect password");
    }

    #[test]
    fn encrypted_error_display_unsupported_handler() {
        let e = EncryptedError::UnsupportedHandler {
            filter: "Standard".into(),
            v: 4,
            r: 6,
            cfm: Some("AESV3".into()),
        };
        assert_eq!(
            e.to_string(),
            r#"unsupported encryption handler: filter=Standard, V=4, R=6, CFM=Some("AESV3")"#
        );
    }

    #[test]
    fn encrypted_error_display_unsupported_handler_no_cfm() {
        let e = EncryptedError::UnsupportedHandler {
            filter: "Standard".into(),
            v: 1,
            r: 2,
            cfm: None,
        };
        assert_eq!(
            e.to_string(),
            "unsupported encryption handler: filter=Standard, V=1, R=2, CFM=None"
        );
    }

    #[test]
    fn encrypted_error_display_malformed() {
        let e = EncryptedError::Malformed {
            reason: "missing /O entry".into(),
        };
        assert_eq!(
            e.to_string(),
            "malformed /Encrypt dictionary: missing /O entry"
        );
    }

    #[test]
    fn error_encrypted_wraps_subkind() {
        let e: Error = EncryptedError::BadPassword.into();
        assert_eq!(e.to_string(), "encrypted PDF: incorrect password");
    }

    #[test]
    fn encrypted_error_codes() {
        assert_eq!(EncryptedError::BadPassword.code(), "encrypted.bad-password");
        assert_eq!(
            EncryptedError::UnsupportedHandler {
                filter: String::new(),
                v: 0,
                r: 0,
                cfm: None
            }
            .code(),
            "encrypted.unsupported-handler"
        );
        assert_eq!(
            EncryptedError::Malformed {
                reason: String::new()
            }
            .code(),
            "encrypted.malformed"
        );
    }

    #[test]
    fn primitive_error_invalid_length_maps_to_encrypted_malformed() {
        let e: Error = PrimitiveError::InvalidLength.into();
        match e {
            Error::Encrypted(EncryptedError::Malformed { ref reason }) => {
                assert!(
                    reason.contains("primitive"),
                    "expected 'primitive' in reason, got: {reason}"
                );
                assert!(
                    reason.contains("invalid key/IV length"),
                    "expected original message in reason, got: {reason}"
                );
            }
            other => panic!("expected Error::Encrypted(Malformed), got: {other:?}"),
        }
    }

    #[test]
    fn rebase_offset_shifts_parse_errors() {
        let rebased = Error::parse(5, "boom").rebase_offset(100);
        match rebased {
            Error::Parse { offset, message } => {
                assert_eq!(offset, 105);
                assert_eq!(message, "boom");
            }
            other => panic!("expected Error::Parse, got {other:?}"),
        }
    }

    #[test]
    fn rebase_offset_leaves_non_parse_errors_unchanged() {
        let original = Error::Unsupported("nope".into());
        let rebased = original.rebase_offset(100);
        assert!(matches!(rebased, Error::Unsupported(ref s) if s == "nope"));
    }
}
