use crate::security::primitives::PrimitiveError;
use thiserror::Error;

/// Crate-wide [`std::result::Result`] specialization.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors produced by the public APIs of `flpdf`.
///
/// I/O failures bubble up via [`Error::Io`]. Structural problems (malformed tokens,
/// unexpected types, depth limits, oversized fields) use [`Error::Parse`] or
/// [`Error::Unsupported`]. [`Error::Missing`] is reserved for required dictionary
/// entries that the spec mandates, e.g. `/Root` on the trailer.
/// [`Error::Encrypted`] covers all encryption-related failures; its subkind is
/// carried by [`EncryptedError`].
#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("parse error at byte {offset}: {message}")]
    Parse { offset: usize, message: String },

    #[error("unsupported PDF feature: {0}")]
    Unsupported(String),

    #[error("missing required PDF entry: {0}")]
    Missing(&'static str),

    #[error("encrypted PDF: {0}")]
    Encrypted(#[from] EncryptedError),

    #[error("signed PDF: {message}")]
    Signed {
        /// Signature field names that make a destructive rewrite unsafe.
        fields: Vec<String>,
        /// Human-readable diagnostic suitable for CLI output.
        message: String,
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

    /// Rebase a relative [`Error::Parse`] offset onto an absolute position.
    ///
    /// When an error is produced while parsing a sub-slice that begins at
    /// `base` within a larger buffer, its `offset` is relative to that slice.
    /// This shifts it back to an absolute offset (`base + offset`) so
    /// diagnostics point at the true byte position. Non-[`Error::Parse`]
    /// variants are returned unchanged.
    pub(crate) fn rebase_offset(self, base: usize) -> Self {
        match self {
            Self::Parse { offset, message } => Self::Parse {
                offset: base + offset,
                message,
            },
            other => other,
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

    /// The document is encrypted with RC4, which is a weak cipher. The caller
    /// must pass `--allow-weak-crypto` to permit processing.
    #[error("encryption uses weak crypto (RC4); pass --allow-weak-crypto to permit")]
    WeakCryptoNotAllowed,
}

/// Bridge from the low-level `PrimitiveError` to [`Error::Encrypted`].
///
/// This allows `?`-propagation from `security::primitives` functions directly
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
            Self::WeakCryptoNotAllowed => "encrypted.weak-crypto-not-allowed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::primitives::PrimitiveError;

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
    fn encrypted_error_display_weak_crypto() {
        let e = EncryptedError::WeakCryptoNotAllowed;
        assert_eq!(
            e.to_string(),
            "encryption uses weak crypto (RC4); pass --allow-weak-crypto to permit"
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
        assert_eq!(
            EncryptedError::WeakCryptoNotAllowed.code(),
            "encrypted.weak-crypto-not-allowed"
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

    #[test]
    fn primitive_error_padding_maps_to_encrypted_malformed() {
        let e: Error = PrimitiveError::PaddingError.into();
        assert!(
            e.to_string()
                .contains("malformed /Encrypt dictionary: primitive: invalid PKCS#7 padding"),
            "unexpected display: {e}"
        );
    }
}
