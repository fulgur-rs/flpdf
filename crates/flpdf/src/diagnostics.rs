//! Diagnostic primitives shared by the parser, writer, and `check` module.

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
}
