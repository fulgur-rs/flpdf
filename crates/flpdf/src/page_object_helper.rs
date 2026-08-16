//! qpdf correspondence: QPDFPageObjectHelper.cc responsibilities shared with page form, resource, flatten, and overlay modules.
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
//! - [`content_stream_objects`](PageObjectHelper::content_stream_objects) —
//!   decode via the existing stream filter pipeline, then parse into
//!   qpdf-shaped [`Object`] events.
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
//!     let objects = helper.content_stream_objects()?;
//!     println!("{} content-stream objects on page 1", objects.len());
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

use crate::content_stream::{parse_content_stream_data, ParseControl, ParserCallbacks};
use crate::object_handle::{ObjectHandle, ObjectHandleIdentity};
use crate::page_rotate::resolve_inherited_rotate;
use crate::pages::{resolve_inherited_resources, DEFAULT_MAX_PAGE_TREE_DEPTH};
use crate::ref_chain::resolve_ref_chain;
use crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result};
use std::collections::HashSet;
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
pub struct PageObjectHelper<'a, R: Read + Seek + 'static> {
    page_ref: ObjectRef,
    pdf: &'a mut Pdf<R>,
}

#[derive(Default)]
struct ObjectRecordingCallbacks {
    objects: Vec<Object>,
}

impl ParserCallbacks for ObjectRecordingCallbacks {
    fn handle_object(
        &mut self,
        object: Object,
        _offset: usize,
        _length: usize,
    ) -> Result<ParseControl> {
        self.objects.push(object);
        Ok(ParseControl::Continue)
    }

    fn handle_eof(&mut self) -> Result<()> {
        Ok(())
    }
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

    /// Return the live canonical handle for this page after validating its
    /// `/Type`. This is the page-helper equivalent of qpdf's
    /// `QPDFPageObjectHelper` construction over a `QPDFObjectHandle`: all
    /// subsequent page attributes and mutations operate on the resolver-backed
    /// object graph rather than on a legacy `Object` snapshot.
    fn resolved_page_handle(&mut self) -> Result<ObjectHandle> {
        let page = self.pdf.get_object_handle(self.page_ref);
        self.pdf.resolve_object_handle(&page)?;
        if page.as_dictionary().is_none() {
            return Err(Error::Unsupported(format!(
                "object {} is not a dictionary, cannot use as a page",
                self.page_ref
            )));
        }

        let page_type = page.try_get_key(b"/Type")?;
        self.pdf.resolve_object_handle(&page_type)?;
        match page_type.as_name() {
            Some(name) if name.as_slice() == b"Page" => Ok(page),
            Some(name) => Err(Error::Unsupported(format!(
                "object {} has /Type /{}, expected /Page",
                self.page_ref,
                String::from_utf8_lossy(&name)
            ))),
            None if page.has_key(b"/Type") => Err(Error::Unsupported(format!(
                "object {} has a non-name /Type entry",
                self.page_ref
            ))),
            None => Err(Error::Unsupported(format!(
                "object {} has no /Type entry",
                self.page_ref
            ))),
        }
    }

    /// Return a live page attribute, applying qpdf's page-tree inheritance
    /// rules for `/MediaBox`, `/CropBox`, `/Resources`, and `/Rotate`.
    ///
    /// When `copy_if_shared` is true, an inherited or indirect value is
    /// shallow-copied into the page dictionary before it is returned. The
    /// returned handle is therefore the value that a caller may mutate without
    /// changing the shared source attribute, matching
    /// `QPDFPageObjectHelper::getAttribute` (`libqpdf/QPDFPageObjectHelper.cc:224-260`).
    /// Missing and null attributes return a direct null handle.
    pub fn get_attribute(&mut self, key: &[u8], copy_if_shared: bool) -> Result<ObjectHandle> {
        let page = self.resolved_page_handle()?;
        let inheritable = matches!(key, b"/MediaBox" | b"/CropBox" | b"/Resources" | b"/Rotate");
        let mut result = self
            .pdf
            .resolve_object_handle_to_terminal(&page.try_get_key(key)?)?;
        let mut inherited = false;

        if result.is_null() && inheritable {
            let mut node = page.clone();
            let mut seen: HashSet<ObjectHandleIdentity> = HashSet::new();
            let mut depth = 0usize;
            loop {
                if depth >= DEFAULT_MAX_PAGE_TREE_DEPTH {
                    return Err(Error::Unsupported(format!(
                        "page tree depth exceeds maximum of {DEFAULT_MAX_PAGE_TREE_DEPTH} at {}",
                        self.page_ref
                    )));
                }
                if !seen.insert(node.identity_key()) {
                    break;
                }

                let parent = self
                    .pdf
                    .resolve_object_handle_to_terminal(&node.try_get_key(b"/Parent")?)?;
                if parent.is_null() {
                    break;
                }
                node = parent;
                depth += 1;

                let candidate = self
                    .pdf
                    .resolve_object_handle_to_terminal(&node.try_get_key(key)?)?;
                if !candidate.is_null() {
                    result = candidate;
                    inherited = true;
                    break;
                }
            }
        }

        if copy_if_shared && (inherited || result.is_indirect()) {
            let copy = result.shallow_copy()?;
            page.replace_key(key, copy.clone())?;
            self.pdf.mark_object_handle_dirty(&page)?;
            result = copy;
        }
        Ok(result)
    }

    /// Return the effective `/MediaBox` handle.
    pub fn get_media_box(&mut self, copy_if_shared: bool) -> Result<ObjectHandle> {
        self.get_attribute(b"/MediaBox", copy_if_shared)
    }

    /// Return the effective `/CropBox` handle, falling back to `/MediaBox`.
    pub fn get_crop_box(
        &mut self,
        copy_if_shared: bool,
        copy_if_fallback: bool,
    ) -> Result<ObjectHandle> {
        let result = self.get_attribute(b"/CropBox", copy_if_shared)?;
        if !result.is_null() {
            return Ok(result);
        }
        let fallback = self.get_media_box(copy_if_shared)?;
        self.apply_fallback(b"/CropBox", fallback, copy_if_fallback)
    }

    /// Return the effective `/BleedBox` handle, falling back to `/CropBox`.
    pub fn get_bleed_box(
        &mut self,
        copy_if_shared: bool,
        copy_if_fallback: bool,
    ) -> Result<ObjectHandle> {
        let result = self.get_attribute(b"/BleedBox", copy_if_shared)?;
        if !result.is_null() {
            return Ok(result);
        }
        let fallback = self.get_crop_box(copy_if_shared, copy_if_fallback)?;
        self.apply_fallback(b"/BleedBox", fallback, copy_if_fallback)
    }

    /// Return the effective `/TrimBox` handle, falling back to `/CropBox`.
    pub fn get_trim_box(
        &mut self,
        copy_if_shared: bool,
        copy_if_fallback: bool,
    ) -> Result<ObjectHandle> {
        let result = self.get_attribute(b"/TrimBox", copy_if_shared)?;
        if !result.is_null() {
            return Ok(result);
        }
        let fallback = self.get_crop_box(copy_if_shared, copy_if_fallback)?;
        self.apply_fallback(b"/TrimBox", fallback, copy_if_fallback)
    }

    /// Return the effective `/ArtBox` handle, falling back to `/CropBox`.
    pub fn get_art_box(
        &mut self,
        copy_if_shared: bool,
        copy_if_fallback: bool,
    ) -> Result<ObjectHandle> {
        let result = self.get_attribute(b"/ArtBox", copy_if_shared)?;
        if !result.is_null() {
            return Ok(result);
        }
        let fallback = self.get_crop_box(copy_if_shared, copy_if_fallback)?;
        self.apply_fallback(b"/ArtBox", fallback, copy_if_fallback)
    }

    fn apply_fallback(
        &mut self,
        key: &[u8],
        fallback: ObjectHandle,
        copy_if_fallback: bool,
    ) -> Result<ObjectHandle> {
        if !copy_if_fallback || fallback.is_null() {
            return Ok(fallback);
        }
        let page = self.resolved_page_handle()?;
        let copy = fallback.shallow_copy()?;
        page.replace_key(key, copy.clone())?;
        self.pdf.mark_object_handle_dirty(&page)?;
        Ok(copy)
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
    // content_stream_objects
    // -----------------------------------------------------------------------

    /// Return the qpdf-shaped content object events for this page.
    ///
    /// Aggregates the page's `/Contents` entry (single stream or array), decodes
    /// each stream through its filter pipeline (same as
    /// [`crate::pages::page_content_bytes`]), then parses the concatenated bytes
    /// through [`parse_content_stream_data`].
    ///
    /// Returns an empty `Vec` when the page has no `/Contents`.
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] when `page_ref` does not resolve to a
    ///   `/Type /Page` dictionary, or when a `/Contents` element is not a stream.
    /// - Any error from [`crate::pages::page_content_bytes`] or
    ///   [`parse_content_stream_data`].
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
    ///     let objects = helper.content_stream_objects()?;
    ///     println!("{} objects", objects.len());
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn content_stream_objects(&mut self) -> Result<Vec<Object>> {
        self.ensure_leaf_page()?;
        let raw = crate::pages::page_content_bytes(self.pdf, self.page_ref)?;
        let mut callbacks = ObjectRecordingCallbacks::default();
        parse_content_stream_data(&raw, &mut callbacks)?;
        Ok(callbacks.objects)
    }

    /// Return the page's `/Contents` as canonical stream handles.
    ///
    /// This is the direct `QPDFPageObjectHelper::getPageContents` route
    /// (`libqpdf/QPDFPageObjectHelper.cc:439-442`) and deliberately preserves
    /// each stream's identity and lazy provider instead of decoding it into a
    /// byte buffer or legacy [`Object`] value.
    pub fn get_page_contents(&mut self) -> Result<Vec<ObjectHandle>> {
        let page = self.resolved_page_handle()?;
        page.get_page_contents()
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
    /// use [`crate::page_rotate::apply_rotate_to_pages`].
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
                // /Annots may be stored behind a holder chain (ref -> ref ->
                // array); follow the chain to its terminal rather than a single
                // hop, then move the owned array out.
                let (terminal, _) = resolve_ref_chain(self.pdf, &Object::Reference(r))?;
                match terminal {
                    Object::Array(arr) => arr,
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
        let value = self.get_media_box(false)?;
        self.page_box_from_handle(b"/MediaBox", value)
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
        let value = self.get_crop_box(false, false)?;
        self.page_box_from_handle(b"/CropBox", value)
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
        let value = self.get_bleed_box(false, false)?;
        self.page_box_from_handle(b"/BleedBox", value)
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
        let value = self.get_trim_box(false, false)?;
        self.page_box_from_handle(b"/TrimBox", value)
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
        let value = self.get_art_box(false, false)?;
        self.page_box_from_handle(b"/ArtBox", value)
    }

    fn page_box_from_handle(&mut self, key: &[u8], value: ObjectHandle) -> Result<Option<PageBox>> {
        if value.is_null() {
            return Ok(None);
        }
        self.pdf.resolve_object_handle(&value)?;
        let Some(items) = value.as_array() else {
            return Err(Error::Unsupported(format!(
                "{} on page {} does not resolve to an array",
                String::from_utf8_lossy(key),
                self.page_ref
            )));
        };
        if items.len() < 4 {
            return Err(Error::Unsupported(format!(
                "{} rectangle array has {} elements, expected 4",
                String::from_utf8_lossy(key),
                items.len()
            )));
        }
        let mut coords = [0.0f64; 4];
        for (index, item) in items.into_iter().take(4).enumerate() {
            self.pdf.resolve_object_handle(&item)?;
            coords[index] = item
                .as_integer()
                .map(|value| value as f64)
                .or_else(|| item.as_real())
                .ok_or_else(|| {
                    Error::Unsupported(format!(
                        "{} rectangle element {index} has type {} (expected number)",
                        String::from_utf8_lossy(key),
                        item.type_name()
                    ))
                })?;
        }
        Ok(Some(PageBox::new(
            coords[0], coords[1], coords[2], coords[3],
        )))
    }
}

// ---------------------------------------------------------------------------
// Private free functions
// ---------------------------------------------------------------------------

fn object_type_name(obj: &Object) -> &'static str {
    match obj {
        Object::Null => "null",
        Object::Boolean(_) => "boolean",
        Object::Integer(_) => "integer",
        Object::Real(_) | Object::RealLiteral { .. } => "real",
        Object::Name(_) => "name",
        Object::String(_) => "string",
        Object::Operator(_) => "operator",
        Object::InlineImage(_) => "inline-image",
        Object::Array(_) => "array",
        Object::Dictionary(_) => "dictionary",
        Object::Stream(_) => "stream",
        Object::Reference(_) => "reference",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `object_type_name` collapses both real variants to `"real"` — both
    /// forms should produce the same diagnostic string.
    #[test]
    fn object_type_name_collapses_real_and_real_literal() {
        assert_eq!(object_type_name(&Object::Real(1.5)), "real");
        assert_eq!(
            object_type_name(&Object::RealLiteral {
                value: 1.5,
                literal: b"1.5".to_vec(),
            }),
            "real"
        );
    }

    #[test]
    fn object_type_name_labels_content_only_values() {
        assert_eq!(
            object_type_name(&Object::Operator(b"q".to_vec())),
            "operator"
        );
        assert_eq!(
            object_type_name(&Object::InlineImage(b"data".to_vec())),
            "inline-image"
        );
    }
}
