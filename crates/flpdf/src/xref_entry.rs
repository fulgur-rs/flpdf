//! Mirrors qpdf 11.9.0 `libqpdf/QPDFXRefEntry.cc`.

/// A PDF cross-reference entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XrefEntry {
    /// A free object entry pointing to the next free object number.
    Free { next: u32 },
    /// An uncompressed object entry at a byte offset in the PDF.
    Uncompressed { offset: u64 },
    /// An object stored at an index in an object stream.
    Compressed { stream: u32, index: u32 },
}

#[cfg(test)]
mod tests {
    use super::XrefEntry;

    #[test]
    fn variants_represent_all_pdf_xref_entry_kinds() {
        assert_eq!(XrefEntry::Free { next: 7 }, XrefEntry::Free { next: 7 });
        assert_eq!(
            XrefEntry::Uncompressed { offset: 42 },
            XrefEntry::Uncompressed { offset: 42 }
        );
        assert_eq!(
            XrefEntry::Compressed {
                stream: 12,
                index: 3,
            },
            XrefEntry::Compressed {
                stream: 12,
                index: 3,
            }
        );
    }

    #[test]
    fn entry_is_copy() {
        fn assert_copy<T: Copy>() {}
        assert_copy::<XrefEntry>();
    }
}
