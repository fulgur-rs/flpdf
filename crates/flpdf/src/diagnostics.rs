#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub offset: Option<u64>,
}

impl Diagnostic {
    pub fn warning(message: impl Into<String>, offset: Option<u64>) -> Self {
        Self {
            severity: Severity::Warning,
            message: message.into(),
            offset,
        }
    }

    pub fn error(message: impl Into<String>, offset: Option<u64>) -> Self {
        Self {
            severity: Severity::Error,
            message: message.into(),
            offset,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct Diagnostics {
    entries: Vec<Diagnostic>,
}

impl Diagnostics {
    pub fn push(&mut self, diagnostic: Diagnostic) {
        self.entries.push(diagnostic);
    }

    pub fn entries(&self) -> &[Diagnostic] {
        &self.entries
    }

    pub fn has_errors(&self) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.severity == Severity::Error)
    }
}
