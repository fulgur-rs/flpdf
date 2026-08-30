//! qpdf correspondence: QPDFLogger.cc diagnostic routing represented as Rust values.
//! Diagnostic primitives shared by the parser, writer, and job check module.
//!
//! The [`Diagnostics::push_encrypted`] helper maps an [`crate::error::EncryptedError`]
//! to a structured [`Diagnostic`] with a `[<code>]` prefix in the message.

/// Severity of a [`Diagnostic`].
///
/// Job check errors are reported through [`crate::job::CheckError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Warning,
    Error,
}

/// Identifies whether a warning already carries the qpdf exception location
/// assembled by its owning object/document boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticOrigin {
    /// A parser, xref, or validation warning whose filename/offset belongs to
    /// the caller that renders the diagnostic.
    Input,
    /// A `QPDFObjectHandle` warning whose qpdf object description is already
    /// part of [`Diagnostic::message`].
    Object,
}

/// A single message produced while parsing or validating a document.
///
/// `offset` is the byte offset in the source file when known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub offset: Option<u64>,
    pub origin: DiagnosticOrigin,
}

impl Diagnostic {
    /// Construct a warning diagnostic.
    pub fn warning(message: impl Into<String>, offset: Option<u64>) -> Self {
        Self {
            severity: Severity::Warning,
            message: message.into(),
            offset,
            origin: DiagnosticOrigin::Input,
        }
    }

    /// Construct an error diagnostic.
    pub fn error(message: impl Into<String>, offset: Option<u64>) -> Self {
        Self {
            severity: Severity::Error,
            message: message.into(),
            offset,
            origin: DiagnosticOrigin::Input,
        }
    }

    /// Construct a warning whose message already contains qpdf's object
    /// description and therefore must not receive a second input filename at
    /// the output boundary.
    pub fn object_warning(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            message: message.into(),
            offset: None,
            origin: DiagnosticOrigin::Object,
        }
    }

    /// Whether this diagnostic carries a qpdf object-level exception
    /// location in its message.
    pub fn is_object_warning(&self) -> bool {
        self.origin == DiagnosticOrigin::Object
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

    #[test]
    fn object_warning_retains_qpdf_exception_location_kind() {
        let diagnostic = Diagnostic::object_warning("test array: warning");

        assert!(diagnostic.is_object_warning());
        assert_eq!(diagnostic.message, "test array: warning");
        assert_eq!(diagnostic.offset, None);
    }
}
