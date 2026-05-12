//! Diagnostic primitives shared by the parser, writer, and `check` module.
//!
//! The [`Diagnostics::push_encrypted`] helper maps an [`crate::error::EncryptedError`]
//! to a structured [`Diagnostic`] with a `[<code>]` prefix in the message.

/// Severity of a [`Diagnostic`].
///
/// Only `Error` flips [`crate::CheckReport::valid`] to `false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Warning,
    Error,
}

/// A single message produced while parsing or validating a document.
///
/// `offset` is the byte offset in the source file when known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub offset: Option<u64>,
}

impl Diagnostic {
    /// Construct a warning diagnostic.
    pub fn warning(message: impl Into<String>, offset: Option<u64>) -> Self {
        Self {
            severity: Severity::Warning,
            message: message.into(),
            offset,
        }
    }

    /// Construct an error diagnostic.
    pub fn error(message: impl Into<String>, offset: Option<u64>) -> Self {
        Self {
            severity: Severity::Error,
            message: message.into(),
            offset,
        }
    }
}

/// Append-only collection of [`Diagnostic`]s.
#[derive(Debug, Default, Clone)]
pub struct Diagnostics {
    entries: Vec<Diagnostic>,
}

impl Diagnostics {
    /// Push a new diagnostic onto the collection.
    pub fn push(&mut self, diagnostic: Diagnostic) {
        self.entries.push(diagnostic);
    }

    /// All diagnostics in insertion order.
    pub fn entries(&self) -> &[Diagnostic] {
        &self.entries
    }

    /// `true` when at least one diagnostic has [`Severity::Error`].
    pub fn has_errors(&self) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.severity == Severity::Error)
    }

    /// Push an error-severity diagnostic derived from an [`crate::error::EncryptedError`].
    ///
    /// The message is formatted as `[<code>] <display>`, e.g.
    /// `[encrypted.bad-password] incorrect password`.  This makes the
    /// machine-readable code available to log processors without requiring a
    /// separate field in [`Diagnostic`].
    pub fn push_encrypted(&mut self, e: &crate::error::EncryptedError) {
        let message = format!("[{}] {}", e.code(), e);
        self.push(Diagnostic::error(message, None));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::EncryptedError;

    #[test]
    fn push_encrypted_bad_password_round_trip() {
        let mut diags = Diagnostics::default();
        let e = EncryptedError::BadPassword;
        diags.push_encrypted(&e);

        let entries = diags.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].severity, Severity::Error);
        assert_eq!(
            entries[0].message,
            "[encrypted.bad-password] incorrect password"
        );
        assert!(diags.has_errors());
    }

    #[test]
    fn push_encrypted_weak_crypto_round_trip() {
        let mut diags = Diagnostics::default();
        let e = EncryptedError::WeakCryptoNotAllowed;
        diags.push_encrypted(&e);

        let entries = diags.entries();
        assert_eq!(entries[0].severity, Severity::Error);
        assert!(entries[0]
            .message
            .starts_with("[encrypted.weak-crypto-not-allowed]"));
    }

    #[test]
    fn push_encrypted_malformed_round_trip() {
        let mut diags = Diagnostics::default();
        let e = EncryptedError::Malformed {
            reason: "missing /O entry".into(),
        };
        diags.push_encrypted(&e);

        let entries = diags.entries();
        assert_eq!(
            entries[0].message,
            "[encrypted.malformed] malformed /Encrypt dictionary: missing /O entry"
        );
    }
}
