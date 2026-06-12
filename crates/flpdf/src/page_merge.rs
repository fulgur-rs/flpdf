//! Multi-document page merge (qpdf `--pages` parity).
//!
//! [`merge_documents`] copies selected pages from N source documents into one
//! fresh target. `inputs[0]` is the primary: its document-level information
//! (outlines, named destinations, AcroForm `/DR` `/DA`) is inherited; later
//! inputs contribute pages and form fields only. Shared resources within one
//! input are de-duplicated; form-field name collisions are resolved by qpdf's
//! `<name>+<N>` renaming rule.

use crate::{Pdf, Result};
use std::io::{Cursor, Read, Seek};

/// One merge input: an opened source document and the 0-based page indices to
/// take from it (arbitrary order, duplicates allowed).
pub struct MergeInput<'a, R: Read + Seek> {
    /// The opened source document.
    pub source: &'a mut Pdf<R>,
    /// 0-based page indices to copy, in output order.
    pub pages: Vec<usize>,
}

/// Merge selected pages from N sources into one fresh document.
///
/// # Errors
///
/// Returns [`Err`] if a source document cannot be read or a requested page
/// index is out of range for its input.
///
/// # Panics
///
/// Currently panics unconditionally: the merge logic is not yet implemented.
pub fn merge_documents<R: Read + Seek>(
    inputs: &mut [MergeInput<'_, R>],
) -> Result<Pdf<Cursor<Vec<u8>>>> {
    let _ = inputs;
    unimplemented!("merge_documents is implemented across the plan")
}
