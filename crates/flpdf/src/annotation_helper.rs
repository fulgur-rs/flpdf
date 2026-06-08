//! Typed accessor helpers for annotation and form-field objects, mirroring
//! `QPDFAnnotationObjectHelper` and `QPDFFormFieldObjectHelper`.
//!
//! [`AnnotationObjectHelper`] wraps an annotation [`ObjectRef`] together with
//! a `&mut Pdf<R>` and exposes typed, panic-free read-only accessors for the
//! common annotation attributes.
//!
//! [`FormFieldObjectHelper`] wraps a form-field (or widget-annotation) dictionary
//! and provides typed accessors for the four inheritable form-field attributes
//! (`/FT`, `/V`, `/DV`, `/Ff`), resolving them through the field tree's
//! `/Parent` chain when absent on the widget itself.
//!
//! Both helpers are intentionally **read-only** and **thin** — they hold no
//! copied state and re-read the live document on every call.
//!
//! # Design
//!
//! - Annotation attributes (`/Subtype`, `/Rect`, `/AP`, `/A`) are **leaf-only**
//!   — they are read directly from the annotation dictionary without walking any
//!   `/Parent` chain (per ISO 32000-1 §12.5, these keys are not inheritable).
//! - Form-field attributes (`/FT`, `/V`, `/DV`, `/Ff`) are **inheritable** in
//!   the field tree (ISO 32000-1 §12.7.3.1): the helper first checks the field
//!   object itself, then walks `/Parent` references until it finds a value or
//!   exhausts the chain. A depth limit and cycle guard prevent runaway iteration
//!   on malformed documents.
//! - String and name values are returned as raw bytes (`Vec<u8>`) without any
//!   text-string decoding.
//! - `/Rect` reuses [`PageBox`] from [`crate::page_object_helper`].
//!
//! # Examples
//!
//! ## Inspect a highlight annotation
//!
//! ```no_run
//! use std::fs::File;
//! use std::io::BufReader;
//! use flpdf::{pages, Pdf, PageObjectHelper, AnnotationObjectHelper};
//!
//! let mut pdf = Pdf::open(BufReader::new(File::open("annotated.pdf")?))?;
//! let page_refs = pages::page_refs(&mut pdf)?;
//! if let Some(&page_ref) = page_refs.first() {
//!     let mut page_helper = PageObjectHelper::new(page_ref, &mut pdf);
//!     let annot_refs = page_helper.get_annotations()?;
//!     drop(page_helper);
//!     for annot_ref in annot_refs {
//!         let mut annot = AnnotationObjectHelper::new(annot_ref, &mut pdf);
//!         if let Some(subtype) = annot.subtype()? {
//!             println!("annotation subtype: {}", String::from_utf8_lossy(&subtype));
//!         }
//!         if let Some(rect) = annot.rect()? {
//!             println!("rect: [{} {} {} {}]", rect.llx, rect.lly, rect.urx, rect.ury);
//!         }
//!     }
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! ## Read an inherited field type from a widget
//!
//! ```no_run
//! use std::fs::File;
//! use std::io::BufReader;
//! use flpdf::{ObjectRef, Pdf, FormFieldObjectHelper};
//!
//! let mut pdf = Pdf::open(BufReader::new(File::open("form.pdf")?))?;
//! // Assume widget_ref is a widget annotation whose /FT lives on a parent field.
//! let widget_ref = ObjectRef::new(10, 0);
//! let mut field = FormFieldObjectHelper::new(widget_ref, &mut pdf);
//! if let Some(ft) = field.field_type()? {
//!     println!("field type: {}", String::from_utf8_lossy(&ft));
//! }
//! if let Some(flags) = field.field_flags()? {
//!     println!("Ff = 0x{:08X}", flags as u32);
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use crate::page_object_helper::PageBox;
use crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH;
use crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result};
use std::collections::BTreeSet;
use std::io::{Read, Seek};

// ---------------------------------------------------------------------------
// AnnotationObjectHelper
// ---------------------------------------------------------------------------

/// Typed read-only accessor helper for a PDF annotation dictionary.
///
/// Construct with [`AnnotationObjectHelper::new`], passing the [`ObjectRef`]
/// of any annotation dictionary (e.g. one retrieved from
/// [`crate::PageObjectHelper::get_annotations`]) and a mutable borrow of the
/// open document.
///
/// All accessors are **leaf-only**: they read only the annotation dictionary
/// itself, consistent with ISO 32000-1 §12.5 which specifies that annotation
/// attributes are not inheritable.
pub struct AnnotationObjectHelper<'a, R: Read + Seek> {
    annot_ref: ObjectRef,
    pdf: &'a mut Pdf<R>,
}

impl<'a, R: Read + Seek> AnnotationObjectHelper<'a, R> {
    /// Construct a new helper for the annotation at `annot_ref`.
    ///
    /// The constructor does not resolve the object; errors are surfaced by the
    /// individual accessor methods.
    pub fn new(annot_ref: ObjectRef, pdf: &'a mut Pdf<R>) -> Self {
        Self { annot_ref, pdf }
    }

    /// Resolve the annotation dictionary.
    fn resolve_dict(&mut self) -> Result<Dictionary> {
        match self.pdf.resolve_borrowed(self.annot_ref)? {
            Object::Dictionary(d) => Ok(d.clone()),
            _ => Err(Error::Unsupported(format!(
                "annotation object {} is not a dictionary",
                self.annot_ref
            ))),
        }
    }

    // -----------------------------------------------------------------------
    // subtype — /Subtype (Name, leaf-only)
    // -----------------------------------------------------------------------

    /// Return the annotation subtype (`/Subtype`) as raw name bytes.
    ///
    /// Common values include `b"Text"`, `b"Link"`, `b"Highlight"`,
    /// `b"Widget"`, etc. (ISO 32000-1 Table 169).
    ///
    /// Returns `Ok(None)` when `/Subtype` is absent or not a name.
    ///
    /// # Errors
    ///
    /// Propagates any error from resolving the annotation object.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use flpdf::{AnnotationObjectHelper, ObjectRef, Pdf};
    /// use std::fs::File;
    /// use std::io::BufReader;
    ///
    /// let mut pdf = Pdf::open(BufReader::new(File::open("a.pdf")?))?;
    /// let mut annot = AnnotationObjectHelper::new(ObjectRef::new(5, 0), &mut pdf);
    /// if let Some(subtype) = annot.subtype()? {
    ///     println!("subtype: {}", String::from_utf8_lossy(&subtype));
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn subtype(&mut self) -> Result<Option<Vec<u8>>> {
        let dict = self.resolve_dict()?;
        Ok(match dict.get("Subtype") {
            Some(Object::Name(bytes)) => Some(bytes.clone()),
            _ => None,
        })
    }

    // -----------------------------------------------------------------------
    // rect — /Rect (4-element array, leaf-only)
    // -----------------------------------------------------------------------

    /// Return the annotation rectangle (`/Rect`) as a [`PageBox`].
    ///
    /// The four numbers are `[llx, lly, urx, ury]` in default user-space units
    /// (ISO 32000-1 §12.5.4). Both [`Object::Integer`] and [`Object::Real`]
    /// elements are accepted and coerced to `f64`.
    ///
    /// Returns `Ok(None)` when `/Rect` is absent.
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] when `/Rect` is present but is not a 4-element
    ///   numeric array.
    /// - Any error from resolving the annotation object.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use flpdf::{AnnotationObjectHelper, ObjectRef, Pdf};
    /// use std::fs::File;
    /// use std::io::BufReader;
    ///
    /// let mut pdf = Pdf::open(BufReader::new(File::open("a.pdf")?))?;
    /// let mut annot = AnnotationObjectHelper::new(ObjectRef::new(5, 0), &mut pdf);
    /// if let Some(r) = annot.rect()? {
    ///     println!("[{} {} {} {}]", r.llx, r.lly, r.urx, r.ury);
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn rect(&mut self) -> Result<Option<PageBox>> {
        let dict = self.resolve_dict()?;
        let val = match dict.get("Rect").cloned() {
            None | Some(Object::Null) => return Ok(None),
            Some(v) => v,
        };
        let arr = resolve_to_array(val, self.pdf, self.annot_ref, "Rect")?;
        parse_rect_array(&arr, b"Rect").map(Some)
    }

    // -----------------------------------------------------------------------
    // appearance — /AP (Dictionary or Reference → Dictionary, leaf-only)
    // -----------------------------------------------------------------------

    /// Return the annotation appearance dictionary (`/AP`).
    ///
    /// `/AP` contains the appearance streams keyed by `/N` (normal), `/R`
    /// (rollover), and `/D` (down) (ISO 32000-1 §12.5.5). The dictionary is
    /// returned as-is; individual appearance streams must be fetched separately.
    ///
    /// An indirect `/AP` reference is resolved automatically.
    ///
    /// Returns `Ok(None)` when `/AP` is absent or null.
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] when `/AP` resolves to a non-dictionary.
    /// - Any error from [`Pdf::resolve`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use flpdf::{AnnotationObjectHelper, ObjectRef, Pdf};
    /// use std::fs::File;
    /// use std::io::BufReader;
    ///
    /// let mut pdf = Pdf::open(BufReader::new(File::open("a.pdf")?))?;
    /// let mut annot = AnnotationObjectHelper::new(ObjectRef::new(5, 0), &mut pdf);
    /// if let Some(ap) = annot.appearance()? {
    ///     let has_normal = ap.get("N").is_some();
    ///     println!("has normal appearance: {has_normal}");
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn appearance(&mut self) -> Result<Option<Dictionary>> {
        let dict = self.resolve_dict()?;
        resolve_optional_dict(dict.get("AP").cloned(), self.pdf, self.annot_ref, "AP")
    }

    // -----------------------------------------------------------------------
    // action — /A (Dictionary or Reference → Dictionary, leaf-only)
    // -----------------------------------------------------------------------

    /// Return the annotation action dictionary (`/A`).
    ///
    /// The returned dictionary contains at minimum `/S` (action subtype, e.g.
    /// `b"URI"`, `b"GoTo"`) plus action-specific keys (ISO 32000-1 §12.6).
    ///
    /// An indirect `/A` reference is resolved automatically.
    ///
    /// Returns `Ok(None)` when `/A` is absent or null.
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] when `/A` resolves to a non-dictionary.
    /// - Any error from [`Pdf::resolve`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use flpdf::{AnnotationObjectHelper, ObjectRef, Pdf};
    /// use std::fs::File;
    /// use std::io::BufReader;
    ///
    /// let mut pdf = Pdf::open(BufReader::new(File::open("a.pdf")?))?;
    /// let mut annot = AnnotationObjectHelper::new(ObjectRef::new(5, 0), &mut pdf);
    /// if let Some(action) = annot.action()? {
    ///     if let Some(Object::Name(s)) = action.get("S") {
    ///         println!("action subtype: {}", String::from_utf8_lossy(s));
    ///     }
    /// }
    /// # use flpdf::Object;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn action(&mut self) -> Result<Option<Dictionary>> {
        let dict = self.resolve_dict()?;
        resolve_optional_dict(dict.get("A").cloned(), self.pdf, self.annot_ref, "A")
    }
}

// ---------------------------------------------------------------------------
// FormFieldObjectHelper
// ---------------------------------------------------------------------------

/// Typed read-only accessor helper for a PDF AcroForm field or widget
/// annotation dictionary.
///
/// Construct with [`FormFieldObjectHelper::new`], passing the [`ObjectRef`]
/// of a field or widget-annotation dictionary and a mutable borrow of the
/// open document.
///
/// The four form-field attributes (`/FT`, `/V`, `/DV`, `/Ff`) are
/// **inheritable** in the field tree (ISO 32000-1 §12.7.3.1): when absent on
/// the field itself, the helper walks the `/Parent` chain until a value is
/// found or the chain is exhausted. A cycle guard and depth limit prevent
/// infinite loops on malformed documents.
///
/// Note: a widget annotation and a terminal form field may be represented by
/// the same dictionary. In that case the same [`ObjectRef`] can be wrapped by
/// both [`AnnotationObjectHelper`] and [`FormFieldObjectHelper`].
pub struct FormFieldObjectHelper<'a, R: Read + Seek> {
    field_ref: ObjectRef,
    pdf: &'a mut Pdf<R>,
}

impl<'a, R: Read + Seek> FormFieldObjectHelper<'a, R> {
    /// Construct a new helper for the form field at `field_ref`.
    ///
    /// The constructor does not resolve the object; errors are surfaced by the
    /// individual accessor methods.
    pub fn new(field_ref: ObjectRef, pdf: &'a mut Pdf<R>) -> Self {
        Self { field_ref, pdf }
    }

    // -----------------------------------------------------------------------
    // field_type — /FT (inheritable Name)
    // -----------------------------------------------------------------------

    /// Return the field type (`/FT`) as raw name bytes.
    ///
    /// Common values are `b"Tx"` (text), `b"Btn"` (button), `b"Ch"` (choice),
    /// `b"Sig"` (signature) (ISO 32000-1 §12.7.3.1).
    ///
    /// `/FT` is inheritable: when absent on the field itself, the `/Parent`
    /// chain is walked until a value is found.
    ///
    /// Returns `Ok(None)` when no node in the chain carries `/FT`.
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] if the field-tree depth limit is exceeded.
    /// - Any error from [`Pdf::resolve`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use flpdf::{FormFieldObjectHelper, ObjectRef, Pdf};
    /// use std::fs::File;
    /// use std::io::BufReader;
    ///
    /// let mut pdf = Pdf::open(BufReader::new(File::open("form.pdf")?))?;
    /// let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    /// if let Some(ft) = field.field_type()? {
    ///     println!("field type: {}", String::from_utf8_lossy(&ft));
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn field_type(&mut self) -> Result<Option<Vec<u8>>> {
        self.resolve_inherited_name(b"FT")
    }

    // -----------------------------------------------------------------------
    // field_value — /V (inheritable, any Object)
    // -----------------------------------------------------------------------

    /// Return the field value (`/V`).
    ///
    /// The value type depends on the field type: a string for text fields,
    /// a name for check boxes and radio buttons, an array for list boxes, etc.
    /// (ISO 32000-1 §12.7.3.1). The raw [`Object`] is returned without any
    /// text-string decoding.
    ///
    /// `/V` is inheritable: when absent on the field itself, the `/Parent`
    /// chain is walked until a value is found.
    ///
    /// Returns `Ok(None)` when no node in the chain carries `/V`.
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] if the field-tree depth limit is exceeded.
    /// - Any error from [`Pdf::resolve`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use flpdf::{FormFieldObjectHelper, Object, ObjectRef, Pdf};
    /// use std::fs::File;
    /// use std::io::BufReader;
    ///
    /// let mut pdf = Pdf::open(BufReader::new(File::open("form.pdf")?))?;
    /// let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    /// match field.field_value()? {
    ///     Some(Object::String(bytes)) => println!("value: {}", String::from_utf8_lossy(&bytes)),
    ///     Some(Object::Name(bytes))   => println!("value: /{}", String::from_utf8_lossy(&bytes)),
    ///     Some(other)                 => println!("value type: {other:?}"),
    ///     None                        => println!("no value"),
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn field_value(&mut self) -> Result<Option<Object>> {
        self.resolve_inherited_object(b"V")
    }

    // -----------------------------------------------------------------------
    // field_default_value — /DV (inheritable, any Object)
    // -----------------------------------------------------------------------

    /// Return the field default value (`/DV`).
    ///
    /// Same typing rules as [`field_value`](FormFieldObjectHelper::field_value).
    ///
    /// `/DV` is inheritable: when absent on the field itself, the `/Parent`
    /// chain is walked until a value is found.
    ///
    /// Returns `Ok(None)` when no node in the chain carries `/DV`.
    ///
    /// # Errors
    ///
    /// Same as [`field_value`](FormFieldObjectHelper::field_value).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use flpdf::{FormFieldObjectHelper, Object, ObjectRef, Pdf};
    /// use std::fs::File;
    /// use std::io::BufReader;
    ///
    /// let mut pdf = Pdf::open(BufReader::new(File::open("form.pdf")?))?;
    /// let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    /// if let Some(dv) = field.field_default_value()? {
    ///     println!("default: {dv:?}");
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn field_default_value(&mut self) -> Result<Option<Object>> {
        self.resolve_inherited_object(b"DV")
    }

    // -----------------------------------------------------------------------
    // field_flags — /Ff (inheritable Integer bit-field)
    // -----------------------------------------------------------------------

    /// Return the field flags (`/Ff`) as a 64-bit integer bit-field.
    ///
    /// `/Ff` is a 32-bit unsigned integer in the PDF spec (ISO 32000-1 Table
    /// 221) but is stored as [`Object::Integer`] (`i64`) in the object model.
    /// Callers that need the unsigned view may cast with `as u32`.
    ///
    /// `/Ff` is inheritable: when absent on the field itself, the `/Parent`
    /// chain is walked until a value is found.
    ///
    /// Returns `Ok(None)` when no node in the chain carries `/Ff`.
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] if the field-tree depth limit is exceeded.
    /// - Any error from [`Pdf::resolve`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use flpdf::{FormFieldObjectHelper, ObjectRef, Pdf};
    /// use std::fs::File;
    /// use std::io::BufReader;
    ///
    /// let mut pdf = Pdf::open(BufReader::new(File::open("form.pdf")?))?;
    /// let mut field = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
    /// if let Some(ff) = field.field_flags()? {
    ///     // Bit 1 (0-indexed): ReadOnly
    ///     let read_only = (ff as u32) & 1 != 0;
    ///     println!("read-only: {read_only}");
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn field_flags(&mut self) -> Result<Option<i64>> {
        self.resolve_inherited_integer(b"Ff")
    }

    // -----------------------------------------------------------------------
    // Private: walk /Parent chain for a Name value
    // -----------------------------------------------------------------------

    fn resolve_inherited_name(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let mut seen: BTreeSet<ObjectRef> = BTreeSet::new();
        let mut current = self.field_ref;
        let mut depth: usize = 0;

        loop {
            if depth >= DEFAULT_MAX_PAGE_TREE_DEPTH {
                return Err(Error::Unsupported(format!(
                    "field tree depth exceeds maximum of {} at {}",
                    DEFAULT_MAX_PAGE_TREE_DEPTH, current
                )));
            }

            if !seen.insert(current) {
                return Ok(None); // cycle detected
            }

            let node_obj = self.pdf.resolve_borrowed(current)?;
            let Some(dict) = node_obj.as_dict() else {
                return Err(Error::Unsupported(format!(
                    "field tree node {current} is not a dictionary"
                )));
            };

            if let Some(val) = dict.get(key).cloned() {
                match val {
                    Object::Null => {} // treat as absent per §7.3.9
                    Object::Name(bytes) => return Ok(Some(bytes)),
                    _ => {} // unexpected type: skip silently, do not error
                }
            }

            // Climb to /Parent.
            match dict.get("Parent").cloned() {
                Some(Object::Reference(r)) => {
                    current = r;
                    depth += 1;
                }
                _ => return Ok(None),
            }
        }
    }

    // -----------------------------------------------------------------------
    // Private: walk /Parent chain for an arbitrary Object value
    // -----------------------------------------------------------------------

    fn resolve_inherited_object(&mut self, key: &[u8]) -> Result<Option<Object>> {
        let mut seen: BTreeSet<ObjectRef> = BTreeSet::new();
        let mut current = self.field_ref;
        let mut depth: usize = 0;

        loop {
            if depth >= DEFAULT_MAX_PAGE_TREE_DEPTH {
                return Err(Error::Unsupported(format!(
                    "field tree depth exceeds maximum of {} at {}",
                    DEFAULT_MAX_PAGE_TREE_DEPTH, current
                )));
            }

            if !seen.insert(current) {
                return Ok(None);
            }

            let node_obj = self.pdf.resolve_borrowed(current)?;
            let Some(dict) = node_obj.as_dict() else {
                return Err(Error::Unsupported(format!(
                    "field tree node {current} is not a dictionary"
                )));
            };

            // Clone out the matched value and the /Parent link before any
            // mutable resolve, releasing the borrow on `node_obj`/`self.pdf`.
            let found = dict.get(key).cloned();
            let parent = dict.get("Parent").cloned();

            if let Some(val) = found {
                match val {
                    Object::Null => {} // absent per §7.3.9
                    // The value may be stored as an indirect reference; resolve
                    // one level so callers receive the materialized object, not
                    // a bare Reference (matches AcroFormDocumentHelper's
                    // deref_leaf, keeping the two read paths consistent). A
                    // reference resolving to Null is treated as absent, so we
                    // keep climbing the /Parent chain.
                    Object::Reference(r) => match self.pdf.resolve(r)? {
                        Object::Null => {}
                        resolved => return Ok(Some(resolved)),
                    },
                    other => return Ok(Some(other)),
                }
            }

            match parent {
                Some(Object::Reference(r)) => {
                    current = r;
                    depth += 1;
                }
                _ => return Ok(None),
            }
        }
    }

    // -----------------------------------------------------------------------
    // Private: walk /Parent chain for an Integer value
    // -----------------------------------------------------------------------

    fn resolve_inherited_integer(&mut self, key: &[u8]) -> Result<Option<i64>> {
        let mut seen: BTreeSet<ObjectRef> = BTreeSet::new();
        let mut current = self.field_ref;
        let mut depth: usize = 0;

        loop {
            if depth >= DEFAULT_MAX_PAGE_TREE_DEPTH {
                return Err(Error::Unsupported(format!(
                    "field tree depth exceeds maximum of {} at {}",
                    DEFAULT_MAX_PAGE_TREE_DEPTH, current
                )));
            }

            if !seen.insert(current) {
                return Ok(None);
            }

            let node_obj = self.pdf.resolve_borrowed(current)?;
            let Some(dict) = node_obj.as_dict() else {
                return Err(Error::Unsupported(format!(
                    "field tree node {current} is not a dictionary"
                )));
            };

            if let Some(val) = dict.get(key).cloned() {
                match val {
                    Object::Null => {}
                    Object::Integer(n) => return Ok(Some(n)),
                    _ => {} // wrong type: skip
                }
            }

            match dict.get("Parent").cloned() {
                Some(Object::Reference(r)) => {
                    current = r;
                    depth += 1;
                }
                _ => return Ok(None),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Private free functions
// ---------------------------------------------------------------------------

/// Resolve `val` to an `Array`, following at most one level of indirection.
fn resolve_to_array<R: Read + Seek>(
    val: Object,
    pdf: &mut Pdf<R>,
    origin: ObjectRef,
    key: &str,
) -> Result<Vec<Object>> {
    match val {
        Object::Array(arr) => Ok(arr),
        Object::Reference(r) => match pdf.resolve_borrowed(r)? {
            Object::Array(arr) => Ok(arr.clone()),
            _ => Err(Error::Unsupported(format!(
                "/{key} reference {r} on object {origin} does not resolve to an array"
            ))),
        },
        _ => Err(Error::Unsupported(format!(
            "/{key} on object {origin} has unexpected type"
        ))),
    }
}

/// Resolve an optional dictionary value, handling indirection.
///
/// Returns `Ok(None)` when `val` is `None` or `Some(Null)`.
fn resolve_optional_dict<R: Read + Seek>(
    val: Option<Object>,
    pdf: &mut Pdf<R>,
    origin: ObjectRef,
    key: &str,
) -> Result<Option<Dictionary>> {
    match val {
        None | Some(Object::Null) => Ok(None),
        Some(Object::Dictionary(d)) => Ok(Some(d)),
        Some(Object::Reference(r)) => match pdf.resolve_borrowed(r)? {
            Object::Dictionary(d) => Ok(Some(d.clone())),
            Object::Null => Ok(None),
            _ => Err(Error::Unsupported(format!(
                "/{key} reference {r} on object {origin} does not resolve to a dictionary"
            ))),
        },
        Some(_) => Err(Error::Unsupported(format!(
            "/{key} on object {origin} has unexpected type"
        ))),
    }
}

/// Parse a 4-element PDF rectangle array into a [`PageBox`].
///
/// Mirrors `page_object_helper::parse_rect_array` — kept private here to
/// avoid coupling across modules.
fn parse_rect_array(arr: &[Object], key: &[u8]) -> Result<PageBox> {
    if arr.len() != 4 {
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
                    "/{} rectangle element {i} has unexpected type {:?}",
                    String::from_utf8_lossy(key),
                    std::mem::discriminant(other)
                )));
            }
        };
    }
    Ok(PageBox::new(coords[0], coords[1], coords[2], coords[3]))
}
