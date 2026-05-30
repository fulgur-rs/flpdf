//! Per-page typed accessor helper, mirroring qpdf's `QPDFPageObjectHelper`.
//!
//! [`PageObjectHelper`] wraps a single leaf `/Page` [`ObjectRef`] together with
//! a `&mut Pdf<R>` and exposes ergonomic, typed accessors for the most common
//! per-page attributes. All operations are delegated to the underlying
//! infrastructure — no page-dictionary state is copied or cached inside this
//! struct.
//!
//! # Design
//!
//! The helper is intentionally thin. It re-reads the live document on every
//! call so that mutations applied through other helpers remain visible
//! immediately.
//!
//! - [`content_streams`](PageObjectHelper::content_streams) — decode via the
//!   existing stream filter pipeline, then tokenize with
//!   [`ContentStreamParser`].
//! - [`resources`](PageObjectHelper::resources) — delegates to
//!   [`crate::pages::resolve_inherited_resources`] (walks `/Parent` chain).
//! - [`rotate`](PageObjectHelper::rotate) — **getter** that delegates to
//!   [`crate::page_rotate::resolve_inherited_rotate`].
//! - [`get_annotations`](PageObjectHelper::get_annotations) — reads the leaf's
//!   `/Annots` array (not inheritable per PDF spec).
//! - [`media_box`](PageObjectHelper::media_box) — inheritable; walks `/Parent`
//!   chain.
//! - [`crop_box`](PageObjectHelper::crop_box) — inheritable; falls back to
//!   `media_box()` when absent.
//! - [`bleed_box`](PageObjectHelper::bleed_box) /
//!   [`trim_box`](PageObjectHelper::trim_box) /
//!   [`art_box`](PageObjectHelper::art_box) — leaf-only; fall back to
//!   `crop_box()` when absent.
//!
//! # Examples
//!
//! ## Inspect content-stream tokens
//!
//! ```no_run
//! use std::fs::File;
//! use std::io::BufReader;
//! use flpdf::{pages, Pdf, PageObjectHelper};
//!
//! let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
//! let page_refs = pages::page_refs(&mut pdf)?;
//! if let Some(&page_ref) = page_refs.first() {
//!     let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
//!     let tokens = helper.content_streams()?;
//!     println!("{} content-stream tokens on page 1", tokens.len());
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! ## Read the effective media box
//!
//! ```no_run
//! use std::fs::File;
//! use std::io::BufReader;
//! use flpdf::{pages, Pdf, PageObjectHelper};
//!
//! let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
//! let page_refs = pages::page_refs(&mut pdf)?;
//! if let Some(&page_ref) = page_refs.first() {
//!     let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
//!     if let Some(mb) = helper.media_box()? {
//!         println!("MediaBox: {:?}", mb);
//!     }
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! ## Get page resources
//!
//! ```no_run
//! use std::fs::File;
//! use std::io::BufReader;
//! use flpdf::{pages, Pdf, PageObjectHelper};
//!
//! let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
//! let page_refs = pages::page_refs(&mut pdf)?;
//! if let Some(&page_ref) = page_refs.first() {
//!     let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
//!     if let Some(res) = helper.resources()? {
//!         let has_font = res.get("Font").is_some();
//!         println!("page has fonts: {has_font}");
//!     }
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! ## Read effective rotation (getter, not mutating)
//!
//! ```no_run
//! use std::fs::File;
//! use std::io::BufReader;
//! use flpdf::{pages, Pdf, PageObjectHelper};
//!
//! let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
//! let page_refs = pages::page_refs(&mut pdf)?;
//! if let Some(&page_ref) = page_refs.first() {
//!     let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
//!     let degrees = helper.rotate()?;
//!     println!("page rotation: {degrees}°");
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! ## List annotation references
//!
//! ```no_run
//! use std::fs::File;
//! use std::io::BufReader;
//! use flpdf::{pages, Pdf, PageObjectHelper};
//!
//! let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
//! let page_refs = pages::page_refs(&mut pdf)?;
//! if let Some(&page_ref) = page_refs.first() {
//!     let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
//!     let annots = helper.get_annotations()?;
//!     println!("{} annotations on page 1", annots.len());
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use crate::content_stream::{ContentStreamParser, ContentToken};
use crate::page_rotate::resolve_inherited_rotate;
use crate::pages::{resolve_inherited_resources, DEFAULT_MAX_PAGE_TREE_DEPTH};
use crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result};
use std::collections::BTreeSet;
use std::io::{Read, Seek};

// ---------------------------------------------------------------------------
// PageBox — a typed rectangle
// ---------------------------------------------------------------------------

/// An axis-aligned rectangle expressed as `[llx, lly, urx, ury]` in user-space
/// units, corresponding to a PDF rectangle array `[x1 y1 x2 y2]`.
///
/// PDF allows any combination of [`Object::Integer`] and [`Object::Real`]
/// elements; both are coerced to `f64`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageBox {
    /// Left x coordinate (lower-left x).
    pub llx: f64,
    /// Bottom y coordinate (lower-left y).
    pub lly: f64,
    /// Right x coordinate (upper-right x).
    pub urx: f64,
    /// Top y coordinate (upper-right y).
    pub ury: f64,
}

impl PageBox {
    /// Construct a `PageBox` from its four corner coordinates.
    pub fn new(llx: f64, lly: f64, urx: f64, ury: f64) -> Self {
        Self { llx, lly, urx, ury }
    }
}

// ---------------------------------------------------------------------------
// PageObjectHelper
// ---------------------------------------------------------------------------

/// Per-page typed accessor helper.
///
/// Construct with [`PageObjectHelper::new`], then use the provided methods to
/// inspect the page's content streams, resources, rotation, annotations, and
/// bounding boxes. All operations are delegated to the underlying `Pdf<R>`
/// infrastructure; no state is cached inside this struct.
pub struct PageObjectHelper<'a, R: Read + Seek> {
    page_ref: ObjectRef,
    pdf: &'a mut Pdf<R>,
}

impl<'a, R: Read + Seek> PageObjectHelper<'a, R> {
    /// Create a new helper for `page_ref` borrowing `pdf` mutably.
    ///
    /// `page_ref` should be the `ObjectRef` of a leaf `/Page` dictionary.
    /// The helper does not validate this at construction time — methods will
    /// propagate errors when given a non-`/Page` reference.
    pub fn new(page_ref: ObjectRef, pdf: &'a mut Pdf<R>) -> Self {
        Self { page_ref, pdf }
    }

    /// Verify `page_ref` resolves to a leaf `/Type /Page` dictionary.
    ///
    /// Guards the public accessors so a `/Pages` tree node (or any other
    /// dictionary) cannot be misread as a page and return plausible but
    /// incorrect inherited/default metadata.
    fn ensure_leaf_page(&mut self) -> Result<()> {
        let obj = self.pdf.resolve_borrowed(self.page_ref)?;
        match obj {
            Object::Dictionary(ref d) if matches!(d.get("Type"), Some(Object::Name(n)) if n == b"Page") => {
                Ok(())
            }
            _ => Err(Error::Unsupported(format!(
                "object {} is not a /Type /Page dictionary",
                self.page_ref
            ))),
        }
    }

    // -----------------------------------------------------------------------
    // content_streams
    // -----------------------------------------------------------------------

    /// Return the tokenized content stream of this page.
    ///
    /// Aggregates the page's `/Contents` entry (single stream or array), decodes
    /// each stream through its filter pipeline (same as
    /// [`crate::pages::page_content_bytes`]), then tokenizes the concatenated
    /// bytes via [`ContentStreamParser`].
    ///
    /// Returns an empty `Vec` when the page has no `/Contents`.
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] when `page_ref` does not resolve to a
    ///   `/Type /Page` dictionary, or when a `/Contents` element is not a stream.
    /// - Any error from [`crate::pages::page_content_bytes`] or
    ///   [`ContentStreamParser`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use std::fs::File;
    /// use std::io::BufReader;
    /// use flpdf::{pages, Pdf, PageObjectHelper};
    ///
    /// let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
    /// let page_refs = pages::page_refs(&mut pdf)?;
    /// if let Some(&page_ref) = page_refs.first() {
    ///     let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    ///     let tokens = helper.content_streams()?;
    ///     println!("{} tokens", tokens.len());
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn content_streams(&mut self) -> Result<Vec<ContentToken>> {
        self.ensure_leaf_page()?;
        let raw = crate::pages::page_content_bytes(self.pdf, self.page_ref)?;
        if raw.is_empty() {
            return Ok(Vec::new());
        }
        let tokens = ContentStreamParser::new(&raw).collect::<Result<Vec<_>>>()?;
        Ok(tokens)
    }

    // -----------------------------------------------------------------------
    // resources
    // -----------------------------------------------------------------------

    /// Return the effective `/Resources` dictionary for this page, walking up
    /// the `/Parent` chain until one is found.
    ///
    /// Returns `Ok(None)` when no node in the inheritance chain carries a
    /// `/Resources` entry.
    ///
    /// This delegates to [`crate::pages::resolve_inherited_resources`].
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] if the page-tree depth limit is exceeded.
    /// - Any error from [`Pdf::resolve`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use std::fs::File;
    /// use std::io::BufReader;
    /// use flpdf::{pages, Pdf, PageObjectHelper};
    ///
    /// let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
    /// let page_refs = pages::page_refs(&mut pdf)?;
    /// if let Some(&page_ref) = page_refs.first() {
    ///     let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    ///     let resources = helper.resources()?;
    ///     println!("resources present: {}", resources.is_some());
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn resources(&mut self) -> Result<Option<Dictionary>> {
        self.ensure_leaf_page()?;
        resolve_inherited_resources(self.pdf, self.page_ref)
    }

    // -----------------------------------------------------------------------
    // rotate  (GETTER — resolves inherited value, does not mutate)
    // -----------------------------------------------------------------------

    /// Return the effective `/Rotate` value for this page in degrees, resolved
    /// through the `/Parent` chain.
    ///
    /// Returns `0` (the PDF default, ISO 32000-1 §7.7.3.3 Table 30) when no
    /// node in the chain carries a `/Rotate` entry. The returned value is
    /// always normalized to one of `{0, 90, 180, 270}`.
    ///
    /// This is a **getter** — it does not mutate the document. To rotate pages,
    /// use [`crate::page_rotate::apply_rotate_to_pages`] or
    /// [`crate::PageDocumentHelper::rotate`].
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] if the page-tree depth limit is exceeded.
    /// - Any error from [`Pdf::resolve`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use std::fs::File;
    /// use std::io::BufReader;
    /// use flpdf::{pages, Pdf, PageObjectHelper};
    ///
    /// let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
    /// let page_refs = pages::page_refs(&mut pdf)?;
    /// if let Some(&page_ref) = page_refs.first() {
    ///     let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    ///     let deg = helper.rotate()?;
    ///     println!("rotation: {deg}°");
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn rotate(&mut self) -> Result<i32> {
        self.ensure_leaf_page()?;
        resolve_inherited_rotate(self.pdf, self.page_ref)
    }

    // -----------------------------------------------------------------------
    // get_annotations
    // -----------------------------------------------------------------------

    /// Return the `ObjectRef`s of all annotations on this page.
    ///
    /// Reads the leaf page's `/Annots` array. Unlike boxes and resources,
    /// `/Annots` is **not** inheritable — only the leaf page dictionary is
    /// consulted.
    ///
    /// Returns an empty `Vec` when `/Annots` is absent or empty.
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] when `page_ref` does not resolve to a
    ///   dictionary, when `/Annots` is not an array, or when an array element
    ///   is not an [`Object::Reference`].
    /// - Any error from [`Pdf::resolve`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use std::fs::File;
    /// use std::io::BufReader;
    /// use flpdf::{pages, Pdf, PageObjectHelper};
    ///
    /// let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
    /// let page_refs = pages::page_refs(&mut pdf)?;
    /// if let Some(&page_ref) = page_refs.first() {
    ///     let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    ///     let annots = helper.get_annotations()?;
    ///     for annot_ref in &annots {
    ///         println!("annotation: {annot_ref}");
    ///     }
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn get_annotations(&mut self) -> Result<Vec<ObjectRef>> {
        self.ensure_leaf_page()?;
        let page_obj = self.pdf.resolve_borrowed(self.page_ref)?;
        let Object::Dictionary(page_dict) = page_obj else {
            return Err(Error::Unsupported(format!(
                "object {} is not a dictionary, cannot read /Annots",
                self.page_ref
            )));
        };

        let annots_val = match page_dict.get("Annots").cloned() {
            None => return Ok(Vec::new()),
            Some(Object::Null) => return Ok(Vec::new()),
            Some(v) => v,
        };

        // /Annots may be a direct array or an indirect reference to an array.
        let annots_array = match annots_val {
            Object::Array(arr) => arr,
            Object::Reference(r) => {
                let resolved = self.pdf.resolve_borrowed(r)?;
                match resolved {
                    Object::Array(arr) => arr.clone(),
                    _ => {
                        return Err(Error::Unsupported(format!(
                            "/Annots reference {r} on page {} does not resolve to an array",
                            self.page_ref
                        )));
                    }
                }
            }
            other => {
                return Err(Error::Unsupported(format!(
                    "/Annots on page {} has unexpected type {}",
                    self.page_ref,
                    object_type_name(&other)
                )));
            }
        };

        let mut refs = Vec::with_capacity(annots_array.len());
        for (i, elem) in annots_array.iter().enumerate() {
            match elem {
                Object::Reference(r) => refs.push(*r),
                other => {
                    return Err(Error::Unsupported(format!(
                        "/Annots element {i} on page {} has type {} (expected reference)",
                        self.page_ref,
                        object_type_name(other)
                    )));
                }
            }
        }
        Ok(refs)
    }

    // -----------------------------------------------------------------------
    // Bounding boxes
    // -----------------------------------------------------------------------

    /// Return the effective `/MediaBox` for this page, resolving inheritance
    /// through the `/Parent` chain.
    ///
    /// Returns `Ok(None)` when no node in the chain carries a `/MediaBox`
    /// entry.
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] if the page-tree depth limit is exceeded, or
    ///   the rectangle array has fewer than 4 numeric elements.
    /// - Any error from [`Pdf::resolve`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use std::fs::File;
    /// use std::io::BufReader;
    /// use flpdf::{pages, Pdf, PageObjectHelper};
    ///
    /// let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
    /// let page_refs = pages::page_refs(&mut pdf)?;
    /// if let Some(&page_ref) = page_refs.first() {
    ///     let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    ///     if let Some(mb) = helper.media_box()? {
    ///         println!("[{} {} {} {}]", mb.llx, mb.lly, mb.urx, mb.ury);
    ///     }
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn media_box(&mut self) -> Result<Option<PageBox>> {
        self.ensure_leaf_page()?;
        self.inherited_box(b"MediaBox")
    }

    /// Return the effective `/CropBox` for this page, resolving inheritance
    /// through the `/Parent` chain.
    ///
    /// Per ISO 32000-1 §14.11.2: when `/CropBox` is absent, the default is the
    /// `/MediaBox`. Returns `Ok(None)` only when `/MediaBox` is also absent.
    ///
    /// # Errors
    ///
    /// Same as [`media_box`](PageObjectHelper::media_box).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use std::fs::File;
    /// use std::io::BufReader;
    /// use flpdf::{pages, Pdf, PageObjectHelper};
    ///
    /// let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
    /// let page_refs = pages::page_refs(&mut pdf)?;
    /// if let Some(&page_ref) = page_refs.first() {
    ///     let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    ///     if let Some(cb) = helper.crop_box()? {
    ///         println!("[{} {} {} {}]", cb.llx, cb.lly, cb.urx, cb.ury);
    ///     }
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn crop_box(&mut self) -> Result<Option<PageBox>> {
        self.ensure_leaf_page()?;
        match self.inherited_box(b"CropBox")? {
            Some(b) => Ok(Some(b)),
            None => self.media_box(),
        }
    }

    /// Return the effective `/BleedBox` for this page.
    ///
    /// Per ISO 32000-1 §14.11.2: `/BleedBox` is **not** inheritable and its
    /// default is the `/CropBox` (which itself defaults to `/MediaBox`).
    ///
    /// # Errors
    ///
    /// Same as [`crop_box`](PageObjectHelper::crop_box).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use std::fs::File;
    /// use std::io::BufReader;
    /// use flpdf::{pages, Pdf, PageObjectHelper};
    ///
    /// let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
    /// let page_refs = pages::page_refs(&mut pdf)?;
    /// if let Some(&page_ref) = page_refs.first() {
    ///     let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    ///     if let Some(bb) = helper.bleed_box()? {
    ///         println!("[{} {} {} {}]", bb.llx, bb.lly, bb.urx, bb.ury);
    ///     }
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn bleed_box(&mut self) -> Result<Option<PageBox>> {
        self.ensure_leaf_page()?;
        match self.leaf_box(b"BleedBox")? {
            Some(b) => Ok(Some(b)),
            None => self.crop_box(),
        }
    }

    /// Return the effective `/TrimBox` for this page.
    ///
    /// Per ISO 32000-1 §14.11.2: `/TrimBox` is **not** inheritable and its
    /// default is the `/CropBox` (which itself defaults to `/MediaBox`).
    ///
    /// # Errors
    ///
    /// Same as [`crop_box`](PageObjectHelper::crop_box).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use std::fs::File;
    /// use std::io::BufReader;
    /// use flpdf::{pages, Pdf, PageObjectHelper};
    ///
    /// let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
    /// let page_refs = pages::page_refs(&mut pdf)?;
    /// if let Some(&page_ref) = page_refs.first() {
    ///     let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    ///     if let Some(tb) = helper.trim_box()? {
    ///         println!("[{} {} {} {}]", tb.llx, tb.lly, tb.urx, tb.ury);
    ///     }
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn trim_box(&mut self) -> Result<Option<PageBox>> {
        self.ensure_leaf_page()?;
        match self.leaf_box(b"TrimBox")? {
            Some(b) => Ok(Some(b)),
            None => self.crop_box(),
        }
    }

    /// Return the effective `/ArtBox` for this page.
    ///
    /// Per ISO 32000-1 §14.11.2: `/ArtBox` is **not** inheritable and its
    /// default is the `/CropBox` (which itself defaults to `/MediaBox`).
    ///
    /// # Errors
    ///
    /// Same as [`crop_box`](PageObjectHelper::crop_box).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use std::fs::File;
    /// use std::io::BufReader;
    /// use flpdf::{pages, Pdf, PageObjectHelper};
    ///
    /// let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
    /// let page_refs = pages::page_refs(&mut pdf)?;
    /// if let Some(&page_ref) = page_refs.first() {
    ///     let mut helper = PageObjectHelper::new(page_ref, &mut pdf);
    ///     if let Some(ab) = helper.art_box()? {
    ///         println!("[{} {} {} {}]", ab.llx, ab.lly, ab.urx, ab.ury);
    ///     }
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn art_box(&mut self) -> Result<Option<PageBox>> {
        self.ensure_leaf_page()?;
        match self.leaf_box(b"ArtBox")? {
            Some(b) => Ok(Some(b)),
            None => self.crop_box(),
        }
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Walk the `/Parent` chain looking for a rectangle array under `key`.
    ///
    /// Mirrors the pattern of [`crate::pages::resolve_inherited_resources`]:
    /// - Per PDF §7.3.9, `Object::Null` is treated as absent.
    /// - Cycle guard via a `BTreeSet<ObjectRef>`.
    /// - Depth limited by [`DEFAULT_MAX_PAGE_TREE_DEPTH`].
    fn inherited_box(&mut self, key: &[u8]) -> Result<Option<PageBox>> {
        let mut seen: BTreeSet<ObjectRef> = BTreeSet::new();
        let mut current = self.page_ref;
        let mut depth: usize = 0;

        loop {
            if depth >= DEFAULT_MAX_PAGE_TREE_DEPTH {
                return Err(Error::Unsupported(format!(
                    "page tree depth exceeds maximum of {DEFAULT_MAX_PAGE_TREE_DEPTH} at {current}"
                )));
            }

            if !seen.insert(current) {
                // Cycle detected — stop walking.
                return Ok(None);
            }

            let node_obj = self.pdf.resolve_borrowed(current)?;
            let Object::Dictionary(dict) = node_obj else {
                return Ok(None);
            };

            let val = dict.get(key).cloned();
            let parent_val = dict.get("Parent").cloned();

            if let Some(val) = val {
                match val {
                    Object::Null => {}
                    Object::Array(arr) => {
                        return parse_rect_array(&arr, key).map(Some);
                    }
                    Object::Reference(r) => {
                        let resolved = self.pdf.resolve_borrowed(r)?;
                        match resolved {
                            Object::Null => {}
                            Object::Array(arr) => {
                                return parse_rect_array(&arr, key).map(Some);
                            }
                            _ => {
                                return Err(Error::Unsupported(format!(
                                    "/{} reference {r} on node {current} does not resolve to an array",
                                    String::from_utf8_lossy(key)
                                )));
                            }
                        }
                    }
                    _ => {
                        return Err(Error::Unsupported(format!(
                            "/{} entry on node {current} has unexpected type",
                            String::from_utf8_lossy(key)
                        )));
                    }
                }
            }

            // Not found here — climb to /Parent.
            let parent_val = match parent_val {
                Some(Object::Null) | None => return Ok(None),
                Some(v) => v,
            };

            match parent_val {
                Object::Reference(r) => {
                    current = r;
                    depth += 1;
                }
                _ => return Ok(None),
            }
        }
    }

    /// Read a bounding-box key from the **leaf page only** (not inheritable).
    ///
    /// Used for `/BleedBox`, `/TrimBox`, and `/ArtBox` which are defined only
    /// on the leaf and default to `/CropBox` per ISO 32000-1 §14.11.2.
    fn leaf_box(&mut self, key: &[u8]) -> Result<Option<PageBox>> {
        let page_obj = self.pdf.resolve_borrowed(self.page_ref)?;
        let Object::Dictionary(dict) = page_obj else {
            return Ok(None);
        };

        let val = match dict.get(key).cloned() {
            None => return Ok(None),
            Some(Object::Null) => return Ok(None),
            Some(v) => v,
        };

        match val {
            Object::Array(arr) => parse_rect_array(&arr, key).map(Some),
            Object::Reference(r) => {
                let resolved = self.pdf.resolve_borrowed(r)?;
                match resolved {
                    Object::Null => Ok(None),
                    Object::Array(arr) => parse_rect_array(&arr, key).map(Some),
                    _ => Err(Error::Unsupported(format!(
                        "/{} reference {r} on page {} does not resolve to an array",
                        String::from_utf8_lossy(key),
                        self.page_ref
                    ))),
                }
            }
            _ => Err(Error::Unsupported(format!(
                "/{} entry on page {} has unexpected type",
                String::from_utf8_lossy(key),
                self.page_ref
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// Private free functions
// ---------------------------------------------------------------------------

/// Parse a 4-element PDF rectangle array into a [`PageBox`].
///
/// Each element may be [`Object::Integer`] or [`Object::Real`]; both are
/// coerced to `f64`. Returns [`Error::Unsupported`] when the array has fewer
/// than 4 elements or contains non-numeric values.
fn parse_rect_array(arr: &[Object], key: &[u8]) -> Result<PageBox> {
    if arr.len() < 4 {
        return Err(Error::Unsupported(format!(
            "/{} rectangle array has {} elements, expected 4",
            String::from_utf8_lossy(key),
            arr.len()
        )));
    }
    let mut coords = [0f64; 4];
    for (i, elem) in arr.iter().take(4).enumerate() {
        coords[i] = match elem {
            Object::Integer(n) => *n as f64,
            Object::Real(r) => *r,
            other => {
                return Err(Error::Unsupported(format!(
                    "/{} rectangle element {i} has type {} (expected number)",
                    String::from_utf8_lossy(key),
                    object_type_name(other)
                )));
            }
        };
    }
    Ok(PageBox::new(coords[0], coords[1], coords[2], coords[3]))
}

fn object_type_name(obj: &Object) -> &'static str {
    match obj {
        Object::Null => "null",
        Object::Boolean(_) => "boolean",
        Object::Integer(_) => "integer",
        Object::Real(_) => "real",
        Object::Name(_) => "name",
        Object::String(_) => "string",
        Object::Array(_) => "array",
        Object::Dictionary(_) => "dictionary",
        Object::Stream(_) => "stream",
        Object::Reference(_) => "reference",
    }
}
