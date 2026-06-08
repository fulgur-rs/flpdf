//! High-level page-document helper, mirroring qpdf's `QPDFPageDocumentHelper`.
//!
//! [`PageDocumentHelper`] wraps a `&mut Pdf<R>` and exposes an ergonomic API for
//! traversing and mutating a document's page list without requiring callers to
//! interact with raw [`ObjectRef`]s or the underlying page-tree internals.
//!
//! # Design
//!
//! The helper is a thin borrowing wrapper — it holds **no copied state**.  Every
//! method re-derives the page list from the live document via the existing
//! infrastructure:
//!
//! - [`pages`](PageDocumentHelper::pages) / [`iter`](PageDocumentHelper::iter)
//!   / [`get`](PageDocumentHelper::get) delegate to [`crate::pages::page_refs`].
//! - [`rotate`](PageDocumentHelper::rotate) builds a [`RotateOp`] and calls
//!   [`crate::page_rotate::apply_rotate_to_pages`].
//! - [`insert`](PageDocumentHelper::insert) / [`remove`](PageDocumentHelper::remove)
//!   splice the ordered page list and call
//!   [`crate::page_tree_rebuild::rebuild_page_tree`].
//!
//! # Examples
//!
//! ## Traverse pages
//!
//! ```no_run
//! use std::fs::File;
//! use std::io::BufReader;
//! use flpdf::{Pdf, PageDocumentHelper};
//!
//! let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
//! let mut helper = PageDocumentHelper::new(&mut pdf);
//! let all_pages = helper.pages()?;
//! println!("{} pages", all_pages.len());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! ## Rotate a range of pages
//!
//! ```no_run
//! use std::fs::File;
//! use std::io::BufReader;
//! use flpdf::{Pdf, PageDocumentHelper, PageRange, RotateMode, write_pdf};
//!
//! let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
//! let mut helper = PageDocumentHelper::new(&mut pdf);
//!
//! // Rotate pages 1 and 3 by +90° (additive).
//! let range = PageRange::parse("1,3")?;
//! helper.rotate(&range, 90, RotateMode::Add)?;
//! drop(helper);
//!
//! let mut out = File::create("output.pdf")?;
//! write_pdf(&mut pdf, &mut out)?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! ## Remove a page
//!
//! ```no_run
//! use std::fs::File;
//! use std::io::BufReader;
//! use flpdf::{Pdf, PageDocumentHelper, write_pdf};
//!
//! let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
//! let mut helper = PageDocumentHelper::new(&mut pdf);
//!
//! // Remove the second page (0-based index 1).
//! helper.remove(1)?;
//! drop(helper);
//!
//! let mut out = File::create("output.pdf")?;
//! write_pdf(&mut pdf, &mut out)?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! ## Insert a page
//!
//! ```no_run
//! use std::fs::File;
//! use std::io::BufReader;
//! use flpdf::{Pdf, PageDocumentHelper, ObjectRef, write_pdf};
//!
//! // Assume `page_ref` is a valid /Page ObjectRef already in the document.
//! let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
//! let page_ref: ObjectRef = {
//!     let mut h = PageDocumentHelper::new(&mut pdf);
//!     *h.pages()?.first().expect("at least one page")
//! };
//!
//! let mut helper = PageDocumentHelper::new(&mut pdf);
//! // Insert the page as a duplicate at index 0 (prepend).
//! helper.insert(0, page_ref)?;
//! drop(helper);
//!
//! let mut out = File::create("output.pdf")?;
//! write_pdf(&mut pdf, &mut out)?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use crate::page_rotate::{apply_rotate_to_pages, RotateMode, RotateOp};
use crate::page_tree_rebuild::{rebuild_page_tree, RebuildResult};
use crate::pages::page_refs;
use crate::{Error, ObjectRef, PageRange, Pdf, Result};
use std::io::{Read, Seek};

/// High-level page-document helper.
///
/// Construct with [`PageDocumentHelper::new`], then use the provided methods to
/// traverse or mutate the document's page list.  All operations are delegated to
/// the underlying `Pdf<R>` infrastructure; no page-tree state is cached inside
/// this struct.
///
/// For a runnable walkthrough see `examples/reorder_pages.rs`.
pub struct PageDocumentHelper<'a, R: Read + Seek> {
    pdf: &'a mut Pdf<R>,
}

impl<'a, R: Read + Seek> PageDocumentHelper<'a, R> {
    /// Create a new helper borrowing `pdf` mutably.
    pub fn new(pdf: &'a mut Pdf<R>) -> Self {
        Self { pdf }
    }

    /// Return all leaf page `ObjectRef`s in document order.
    ///
    /// This is the primary accessor. Every other method that needs the full
    /// page list calls this internally.
    ///
    /// # Errors
    ///
    /// Propagates errors from [`crate::pages::page_refs`] (e.g. missing catalog,
    /// page-tree depth exceeded).
    pub fn pages(&mut self) -> Result<Vec<ObjectRef>> {
        page_refs(self.pdf)
    }

    /// Return an iterator over all leaf page `ObjectRef`s in document order.
    ///
    /// The iterator owns its elements (collected into a `Vec` first) to avoid
    /// holding a borrow on `self` across the iteration.
    ///
    /// # Errors
    ///
    /// Same as [`Self::pages`].
    pub fn iter(&mut self) -> Result<std::vec::IntoIter<ObjectRef>> {
        Ok(self.pages()?.into_iter())
    }

    /// Return the page `ObjectRef` at 0-based `idx`, or `Ok(None)` if `idx` is
    /// out of bounds.
    ///
    /// # Errors
    ///
    /// Same as [`Self::pages`].
    pub fn get(&mut self, idx: usize) -> Result<Option<ObjectRef>> {
        Ok(self.pages()?.get(idx).copied())
    }

    /// Insert `page` at 0-based position `idx`, shifting existing pages to the
    /// right.
    ///
    /// `idx == 0` prepends; `idx == page_count` appends.  `page` must already
    /// exist in the document as a valid `/Page` dictionary — [`rebuild_page_tree`]
    /// will return an error otherwise.
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] when `idx > page_count`.
    /// - Any error from [`rebuild_page_tree`] (e.g. `page` is not a `/Page` dict).
    pub fn insert(&mut self, idx: usize, page: ObjectRef) -> Result<RebuildResult> {
        let mut refs = page_refs(self.pdf)?;
        if idx > refs.len() {
            return Err(Error::Unsupported(format!(
                "insert index {idx} is out of bounds (page count {})",
                refs.len()
            )));
        }
        refs.insert(idx, page);
        rebuild_page_tree(self.pdf, &refs)
    }

    /// Remove the page at 0-based position `idx`.
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] when `idx >= page_count`.
    /// - [`Error::Missing`] when removing the last remaining page (the result
    ///   would be an empty document, which is not allowed by [`rebuild_page_tree`]).
    pub fn remove(&mut self, idx: usize) -> Result<RebuildResult> {
        let mut refs = page_refs(self.pdf)?;
        if idx >= refs.len() {
            return Err(Error::Unsupported(format!(
                "remove index {idx} is out of bounds (page count {})",
                refs.len()
            )));
        }
        refs.remove(idx);
        if refs.is_empty() {
            return Err(Error::Missing(
                "cannot remove the only remaining page: result would be an empty document",
            ));
        }
        rebuild_page_tree(self.pdf, &refs)
    }

    /// Apply a rotation to the pages selected by `range`.
    ///
    /// `degrees` and `mode` are forwarded to [`RotateOp`] and
    /// [`apply_rotate_to_pages`].  The rotation is materialized explicitly on
    /// every selected leaf page (inherited `/Rotate` is resolved first and then
    /// written explicitly on the leaf).
    ///
    /// An all-pages [`PageRange`] (constructed from an empty string) rotates
    /// every page in document order.
    ///
    /// # Errors
    ///
    /// - Any error from [`crate::pages::page_refs`].
    /// - [`Error::Parse`] / range resolution errors when `range` refers to
    ///   page numbers beyond the document's page count.
    /// - Any error from [`apply_rotate_to_pages`].
    pub fn rotate(&mut self, range: &PageRange, degrees: i32, mode: RotateMode) -> Result<()> {
        let all_refs = page_refs(self.pdf)?;
        let page_count = all_refs.len() as u32;

        // Resolve the range to 1-based page numbers, then convert to ObjectRefs.
        let page_numbers = range.resolve(page_count)?;
        let selected: Vec<ObjectRef> = page_numbers
            .into_iter()
            .filter_map(|n| all_refs.get((n - 1) as usize).copied())
            .collect();

        let op = RotateOp { mode, degrees };
        apply_rotate_to_pages(self.pdf, &selected, &op)
    }
}
