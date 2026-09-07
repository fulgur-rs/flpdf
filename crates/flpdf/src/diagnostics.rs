//! qpdf correspondence: `QPDF::Members::warnings` represented as Rust values.
//!
//! qpdf stores warnings as a single ordered `std::vector<QPDFExc>` and copies
//! each exception into that vector before optional logger delivery
//! (`include/qpdf/QPDF.hh:261-273`, `libqpdf/QPDF.cc:487-504`). The collection
//! therefore stores [`crate::QpdfExc`] directly. Fatal operation results belong
//! to [`crate::Error`] or a job-specific error, not to this warning collection.

/// The document-owned, append-only qpdf warning collection.
///
/// Every entry retains qpdf's independent error code, source filename, object
/// description, signed file position, and detail bytes. Callers that need the
/// logger-visible representation must use [`crate::QpdfExc::what_bytes`]; it
/// intentionally stops at the first NUL just like C++ `what()` consumers.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Diagnostics {
    entries: Vec<crate::QpdfExc>,
}

impl Diagnostics {
    /// Append a warning while preserving qpdf's emission order.
    pub fn push(&mut self, warning: crate::QpdfExc) {
        self.entries.push(warning);
    }

    /// Return all warnings in insertion order.
    pub fn entries(&self) -> &[crate::QpdfExc] {
        &self.entries
    }

    /// Return the number of collected warnings.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Return whether no warnings have been collected.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{QpdfErrorCode, QpdfExc};

    #[test]
    fn qpdf_warning_collection_preserves_structured_exception() {
        let warning = QpdfExc::new(
            QpdfErrorCode::DamagedPdf,
            b"file-\xff.pdf",
            b"object 4 0",
            -1,
            b"detail-\xfe",
        );
        let mut diags = Diagnostics::default();
        diags.push(warning.clone());

        let entries = diags.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0], warning);
        assert_eq!(entries[0].get_filename(), b"file-\xff.pdf");
        assert_eq!(entries[0].get_object(), b"object 4 0");
        assert_eq!(entries[0].get_file_position(), -1);
        assert_eq!(entries[0].get_message_detail(), b"detail-\xfe");
        assert_eq!(
            entries[0].what_bytes(),
            b"file-\xff.pdf (object 4 0): detail-\xfe"
        );
    }

    #[test]
    fn qpdf_warning_collection_clone_preserves_order_and_raw_what() {
        let mut diags = Diagnostics::default();
        diags.push(QpdfExc::new(
            QpdfErrorCode::Object,
            b"",
            b"object 1 0",
            0,
            b"first",
        ));
        diags.push(QpdfExc::new(
            QpdfErrorCode::System,
            b"source-\xff",
            b"",
            7,
            b"second\0after-nul",
        ));

        let cloned = diags.clone();
        assert_eq!(cloned.entries().len(), 2);
        assert_eq!(cloned.entries()[0].what_bytes(), b"object 1 0: first");
        assert_eq!(
            cloned.entries()[1].get_message_detail(),
            b"second\0after-nul"
        );
        assert_eq!(
            cloned.entries()[1].what_bytes(),
            b"source-\xff (offset 7): second"
        );
    }

    #[test]
    fn warning_collection_length_and_empty_state_follow_pushes() {
        let mut diagnostics = Diagnostics::default();
        assert!(diagnostics.is_empty());
        assert_eq!(diagnostics.len(), 0);
        diagnostics.push(QpdfExc::new(QpdfErrorCode::Object, b"", b"", 0, b"warning"));
        assert!(!diagnostics.is_empty());
        assert_eq!(diagnostics.len(), 1);
    }
}
