//! qpdf correspondence: QPDF.cc document-construction entry points (`emptyPDF()` first; `processFile`/`processMemoryFile` remain in `reader.rs` pending the broader split — see `docs/plans/2026-08-07-reader-rs-pdf-engine-resolve-split-design.md`).

use crate::Pdf;
use std::io::Cursor;

// Mirrors qpdf's `EMPTY_PDF` (`libqpdf/QPDF.cc:34-51`) byte for byte: PDF
// 1.3, a Catalog (object 1) pointing at an empty Pages tree (object 2), and
// a classic xref table whose offsets match this exact literal.
const EMPTY_PDF_BYTES: &[u8] = concat!(
    "%PDF-1.3\n",
    "1 0 obj\n",
    "<< /Type /Catalog /Pages 2 0 R >>\n",
    "endobj\n",
    "2 0 obj\n",
    "<< /Type /Pages /Kids [] /Count 0 >>\n",
    "endobj\n",
    "xref\n",
    "0 3\n",
    "0000000000 65535 f \n",
    "0000000009 00000 n \n",
    "0000000058 00000 n \n",
    "trailer << /Size 3 /Root 1 0 R >>\n",
    "startxref\n",
    "110\n",
    "%%EOF\n",
)
.as_bytes();

impl Pdf<Cursor<Vec<u8>>> {
    // CLAUDE.md deviation class (B): qpdf's `QPDF::emptyPDF()` is a `void`
    // method that lazily initializes an already-constructed `QPDF` (the
    // C++ type has a default-constructed, not-yet-loaded state). flpdf's
    // `Pdf` has no such state; every `open*` returns a ready-to-use
    // `Result<Self>`. `empty()` is the static-factory counterpart of that
    // mutator — same fixed bytes, same `processMemoryFile`-equivalent
    // parse path (`open_mem_owned`), only the "already-constructed
    // instance to mutate" scaffolding is replaced by a factory return.
    // Recorded in docs/qpdf-correspondence.md (QPDFPageDocumentHelper.cc
    // row, §7).
    /// Open a canonical minimal PDF: a `Catalog` (object 1) pointing at an
    /// empty `Pages` tree (object 2, zero pages), read through the normal
    /// parser and object cache like any other document.
    ///
    /// Mirrors qpdf's `QPDF::emptyPDF()` (`libqpdf/QPDF.cc:34-51,290-293`):
    /// the same fixed bytes, opened the same way [`Pdf::open_mem_owned`]
    /// opens any in-memory PDF. Objects can be added and the trailer or
    /// catalog mutated exactly as with any other opened document.
    ///
    /// # Errors
    ///
    /// Propagates any error from [`Pdf::open_mem_owned`]; the fixed bytes
    /// are well-formed, so in practice this only surfaces allocator or
    /// similar infrastructure failures.
    ///
    /// # Examples
    ///
    /// ```
    /// use flpdf::Pdf;
    ///
    /// let pdf = Pdf::empty()?;
    /// assert_eq!(pdf.version(), "1.3");
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn empty() -> crate::Result<Self> {
        Self::open_mem_owned(EMPTY_PDF_BYTES.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Object, ObjectRef};

    #[test]
    fn empty_returns_canonical_minimal_document() {
        let mut pdf = Pdf::empty().expect("Pdf::empty must succeed");
        assert_eq!(pdf.version(), "1.3");

        let root_ref = pdf.root_ref().expect("root ref");
        assert_eq!(root_ref, ObjectRef::new(1, 0));
        let catalog = pdf.resolve(root_ref).unwrap();
        let catalog_dict = catalog.as_dict().unwrap();
        assert_eq!(
            catalog_dict.get("Type").unwrap().as_name(),
            Some(&b"Catalog"[..])
        );
        let pages_ref = catalog_dict
            .get("Pages")
            .and_then(Object::as_ref_id)
            .expect("/Pages must be a reference");
        assert_eq!(pages_ref, ObjectRef::new(2, 0));

        let pages = pdf.resolve(pages_ref).unwrap();
        let pages_dict = pages.as_dict().unwrap();
        assert_eq!(
            pages_dict.get("Type").unwrap().as_name(),
            Some(&b"Pages"[..])
        );
        assert_eq!(pages_dict.get("Kids"), Some(&Object::Array(vec![])));
        assert_eq!(pages_dict.get("Count"), Some(&Object::Integer(0)));

        assert_eq!(pdf.trailer().get("Size"), Some(&Object::Integer(3)));
        assert_eq!(
            pdf.trailer().get("Root"),
            Some(&Object::Reference(root_ref))
        );
    }

    #[test]
    fn empty_generations_are_independent_with_stable_root_and_pages_identity() {
        let mut a = Pdf::empty().unwrap();
        let mut b = Pdf::empty().unwrap();

        let root_ref = ObjectRef::new(1, 0);
        let pages_ref = ObjectRef::new(2, 0);
        assert_eq!(a.root_ref(), Some(root_ref));
        assert_eq!(b.root_ref(), Some(root_ref));

        // Mutating `a`'s Pages dict must not leak into `b`: each call to
        // `Pdf::empty()` owns its own bytes and cache.
        let mut pages_dict = a.resolve(pages_ref).unwrap().as_dict().unwrap().clone();
        pages_dict.insert("Count", Object::Integer(7));
        a.set_object(pages_ref, Object::Dictionary(pages_dict));

        assert_eq!(
            a.resolve(pages_ref)
                .unwrap()
                .as_dict()
                .unwrap()
                .get("Count"),
            Some(&Object::Integer(7))
        );
        assert_eq!(
            b.resolve(pages_ref)
                .unwrap()
                .as_dict()
                .unwrap()
                .get("Count"),
            Some(&Object::Integer(0))
        );
    }
}
