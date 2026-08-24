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
#[error("{message}")]
pub struct UsageError {
    message: String,
}

impl UsageError {
    /// Construct a usage error with the given qpdf-compatible message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

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

    #[error("encrypted PDF: {0}")]
    Encrypted(#[from] EncryptedError),

    #[error("{0}")]
    Internal(String),

    #[error("{0}")]
    System(String),

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
