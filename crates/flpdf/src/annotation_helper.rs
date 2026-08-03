//! qpdf correspondence: `QPDFAnnotationObjectHelper.cc`.
//! Typed accessor helpers for annotation objects.
//!
//! [`AnnotationObjectHelper`] wraps an annotation [`ObjectRef`] together with
//! a `&mut Pdf<R>` and exposes typed, panic-free read-only accessors for the
//! common annotation attributes.
//!
//! The helper is intentionally **read-only** and **thin** — it holds no copied
//! state and re-reads the live document on every call.
//!
//! # Design
//!
//! - Annotation attributes (`/Subtype`, `/Rect`, `/AP`, `/A`) are **leaf-only**
//!   — they are read directly from the annotation dictionary without walking any
//!   `/Parent` chain (per ISO 32000-1 §12.5, these keys are not inheritable).
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
use crate::page_object_helper::PageBox;
use crate::ref_chain::resolve_ref_chain;
use crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result};
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
pub struct AnnotationObjectHelper<'a, R: Read + Seek + 'static> {
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
        Ok(match dict.get("Subtype").cloned() {
            Some(value) => match resolve_ref_chain(self.pdf, &value)?.0 {
                Object::Name(bytes) => Some(bytes),
                _ => None,
            },
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
            Object::Real(r) | Object::RealLiteral { value: r, .. } => *r,
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
