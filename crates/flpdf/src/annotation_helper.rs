//! qpdf correspondence: `QPDFAnnotationObjectHelper.cc`.
//! Typed accessor helpers for annotation objects.
//!
//! [`AnnotationObjectHelper`] wraps an annotation [`ObjectRef`] together with
//! a `&mut Pdf<R>` and exposes typed, fail-soft read-only accessors for the
//! common annotation attributes, mirroring qpdf's own transparently
//! dereferencing `QPDFObjectHandle` API on top of this crate's
//! [`ObjectHandle`], which requires an explicit resolve at every hop
//! (`Pdf::resolve_object_handle`) — see [`FormFieldObjectHelper`] for the
//! same established shape.
//!
//! # Design
//!
//! - Annotation attributes (`/Subtype`, `/Rect`, `/AP`, `/AS`, `/F`) are
//!   **leaf-only** — they are read directly from the annotation dictionary
//!   without walking any `/Parent` chain (per ISO 32000-1 §12.5, these keys
//!   are not inheritable).
//! - Accessors are **fail-soft**, matching qpdf: a missing key or a value of
//!   the wrong type yields a default (empty name, zero flags, a
//!   `(0, 0, 0, 0)` rectangle, a null handle) rather than an error. Only an
//!   I/O, parse, filter, or decryption failure while resolving an
//!   [`ObjectHandle`] surfaces as `Err`.
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
//!         let subtype = annot.get_subtype()?;
//!         println!("annotation subtype: {}", String::from_utf8_lossy(&subtype));
//!         let rect = annot.get_rect()?;
//!         println!("rect: [{} {} {} {}]", rect.llx, rect.lly, rect.urx, rect.ury);
//!     }
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use crate::object_handle::ObjectHandle;
use crate::page_object_helper::PageBox;
use crate::{ObjectRef, Pdf, Result};
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
    annot: ObjectHandle,
    pdf: &'a mut Pdf<R>,
}

impl<'a, R: Read + Seek> AnnotationObjectHelper<'a, R> {
    /// Construct a new helper for the annotation at `annot_ref`.
    ///
    /// The constructor does not resolve the object; errors are surfaced by
    /// the individual accessor methods.
    pub fn new(annot_ref: ObjectRef, pdf: &'a mut Pdf<R>) -> Self {
        let annot = pdf.get_object_handle(annot_ref);
        Self { annot, pdf }
    }

    /// Resolve `self.annot` and return the key's resolved child handle.
    fn resolved_key(&mut self, key: &[u8]) -> Result<ObjectHandle> {
        self.pdf.resolve_object_handle(&self.annot)?;
        let child = self.annot.get_key(key);
        self.pdf.resolve_object_handle(&child)?;
        Ok(child)
    }

    // -----------------------------------------------------------------------
    // get_subtype — /Subtype (Name, leaf-only)
    // -----------------------------------------------------------------------

    /// Return the annotation subtype (`/Subtype`) as raw name bytes.
    ///
    /// Common values include `b"Text"`, `b"Link"`, `b"Highlight"`,
    /// `b"Widget"`, etc. (ISO 32000-1 Table 169).
    ///
    /// Mirrors `QPDFAnnotationObjectHelper::getSubtype`
    /// (`libqpdf/QPDFAnnotationObjectHelper.cc:14-17`): returns an empty
    /// `Vec` when `/Subtype` is absent or not a name, never an error for
    /// that reason.
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
    /// let subtype = annot.get_subtype()?;
    /// println!("subtype: {}", String::from_utf8_lossy(&subtype));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn get_subtype(&mut self) -> Result<Vec<u8>> {
        Ok(self
            .resolved_key(b"/Subtype")?
            .as_name()
            .unwrap_or_default())
    }

    // -----------------------------------------------------------------------
    // get_rect — /Rect (4-element numeric array, leaf-only)
    // -----------------------------------------------------------------------

    /// Return the annotation rectangle (`/Rect`) as a [`PageBox`].
    ///
    /// The four numbers are `[llx, lly, urx, ury]` in default user-space
    /// units (ISO 32000-1 §12.5.4). Both integer and real elements are
    /// accepted and coerced to `f64`.
    ///
    /// Mirrors `QPDFObjectHandle::getArrayAsRectangle`
    /// (`libqpdf/QPDFObjectHandle.cc:817-836`), used by
    /// `QPDFAnnotationObjectHelper::getRect`: a missing `/Rect`, a
    /// non-array value, an array with a length other than 4, or a
    /// non-numeric element all yield `PageBox::new(0.0, 0.0, 0.0, 0.0)`
    /// rather than an error. The four corners are normalized to
    /// `llx <= urx` and `lly <= ury` via `min`/`max`, so a rectangle array
    /// stored with corners in reverse order is still returned upright.
    ///
    /// # Errors
    ///
    /// Propagates any error from resolving the annotation object or an
    /// indirect `/Rect` array element.
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
    /// let r = annot.get_rect()?;
    /// println!("[{} {} {} {}]", r.llx, r.lly, r.urx, r.ury);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn get_rect(&mut self) -> Result<PageBox> {
        let rect = self.resolved_key(b"/Rect")?;
        self.array_as_rectangle(&rect)
    }

    /// Resolve `handle` as a 4-element numeric array into a [`PageBox`],
    /// mirroring `QPDFObjectHandle::getArrayAsRectangle`.
    fn array_as_rectangle(&mut self, handle: &ObjectHandle) -> Result<PageBox> {
        let zero = PageBox::new(0.0, 0.0, 0.0, 0.0);
        let Some(items) = handle.as_array() else {
            return Ok(zero);
        };
        if items.len() != 4 {
            return Ok(zero);
        }
        let mut nums = [0.0f64; 4];
        for (i, item) in items.iter().enumerate() {
            self.pdf.resolve_object_handle(item)?;
            let Some(n) = as_number(item) else {
                return Ok(zero);
            };
            nums[i] = n;
        }
        Ok(PageBox::new(
            nums[0].min(nums[2]),
            nums[1].min(nums[3]),
            nums[0].max(nums[2]),
            nums[1].max(nums[3]),
        ))
    }

    // -----------------------------------------------------------------------
    // get_appearance_dictionary — /AP (leaf-only)
    // -----------------------------------------------------------------------

    /// Return the resolved `/AP` value.
    ///
    /// Mirrors `QPDFAnnotationObjectHelper::getAppearanceDictionary`
    /// (`libqpdf/QPDFAnnotationObjectHelper.cc:24-27`): returns whatever
    /// `/AP` resolves to verbatim, without checking that it is actually a
    /// dictionary. Callers that need the dictionary should check the
    /// returned handle's type; a null handle means `/AP` was absent.
    ///
    /// # Errors
    ///
    /// Propagates any error from resolving the annotation object or `/AP`.
    pub fn get_appearance_dictionary(&mut self) -> Result<ObjectHandle> {
        self.resolved_key(b"/AP")
    }

    // -----------------------------------------------------------------------
    // get_appearance_state — /AS (Name, leaf-only)
    // -----------------------------------------------------------------------

    /// Return the appearance state (`/AS`) as raw name bytes, or an empty
    /// `Vec` if `/AS` is absent or not a name.
    ///
    /// Mirrors `QPDFAnnotationObjectHelper::getAppearanceState`
    /// (`libqpdf/QPDFAnnotationObjectHelper.cc:30-38`).
    ///
    /// # Errors
    ///
    /// Propagates any error from resolving the annotation object or `/AS`.
    pub fn get_appearance_state(&mut self) -> Result<Vec<u8>> {
        Ok(self.resolved_key(b"/AS")?.as_name().unwrap_or_default())
    }

    // -----------------------------------------------------------------------
    // get_flags — /F (Integer, leaf-only)
    // -----------------------------------------------------------------------

    /// Return the annotation flags (`/F`), a logical OR of
    /// `pdf_annotation_flag_e` bits (ISO 32000-1 Table 165), or `0` if `/F`
    /// is absent or not an integer.
    ///
    /// Mirrors `QPDFAnnotationObjectHelper::getFlags`
    /// (`libqpdf/QPDFAnnotationObjectHelper.cc:41-45`).
    ///
    /// # Errors
    ///
    /// Propagates any error from resolving the annotation object or `/F`.
    pub fn get_flags(&mut self) -> Result<i64> {
        Ok(self.resolved_key(b"/F")?.as_integer().unwrap_or(0))
    }

    // -----------------------------------------------------------------------
    // get_appearance_stream — /AP/<which>[/<state>]
    // -----------------------------------------------------------------------

    /// Select an appearance stream from `/AP`.
    ///
    /// `which` selects the entry within `/AP` — typically `b"N"` (normal),
    /// `b"R"` (rollover), or `b"D"` (down), as a decoded PDF name (no
    /// leading `/`, matching [`Self::get_subtype`]/
    /// [`Self::get_appearance_state`]'s own convention — [`ObjectHandle::
    /// get_key`] requires the `/`, so both `which` and `state` get it
    /// prepended internally). If `/AP/<which>` is itself a stream, it is
    /// returned directly. If it is a subdictionary (a state dictionary),
    /// `state` selects a key within it when non-empty, falling back to
    /// [`Self::get_appearance_state`]'s `/AS` value when `state` is `None`
    /// or empty. Returns a null [`ObjectHandle`] when no stream can be
    /// selected.
    ///
    /// Mirrors `QPDFAnnotationObjectHelper::getAppearanceStream`
    /// (`libqpdf/QPDFAnnotationObjectHelper.cc:48-71`), including qpdf issue
    /// #949's observed behavior: when `/AP/<which>` is already a stream, the
    /// state is disregarded even if one was requested.
    ///
    /// # Errors
    ///
    /// Propagates any error from resolving the annotation object, `/AP`,
    /// `/AS`, or the selected appearance entries.
    pub fn get_appearance_stream(
        &mut self,
        which: &[u8],
        state: Option<&[u8]>,
    ) -> Result<ObjectHandle> {
        let ap = self.get_appearance_dictionary()?;
        let desired_state: Vec<u8> = match state {
            Some(s) if !s.is_empty() => s.to_vec(),
            _ => self.get_appearance_state()?,
        };
        if ap.as_dictionary().is_some() {
            let ap_sub = ap.get_key(&dict_key(which));
            self.pdf.resolve_object_handle(&ap_sub)?;
            if ap_sub.as_stream_dict().is_some() {
                return Ok(ap_sub);
            }
            if ap_sub.as_dictionary().is_some() && !desired_state.is_empty() {
                let ap_sub_val = ap_sub.get_key(&dict_key(&desired_state));
                self.pdf.resolve_object_handle(&ap_sub_val)?;
                if ap_sub_val.as_stream_dict().is_some() {
                    return Ok(ap_sub_val);
                }
            }
        }
        Ok(ObjectHandle::null())
    }
}

// ---------------------------------------------------------------------------
// Private free functions
// ---------------------------------------------------------------------------

/// Prepend the `/` [`ObjectHandle::get_key`] requires to a decoded PDF name
/// value (e.g. from [`AnnotationObjectHelper::get_appearance_state`], which
/// like every other name-valued accessor in this crate returns the name
/// without it).
fn dict_key(name: &[u8]) -> Vec<u8> {
    let mut key = Vec::with_capacity(name.len() + 1);
    key.push(b'/');
    key.extend_from_slice(name);
    key
}

/// Coerce a resolved [`ObjectHandle`] to `f64` if it is an integer or real,
/// mirroring `QPDFObjectHandle::getValueAsNumber`.
fn as_number(handle: &ObjectHandle) -> Option<f64> {
    handle
        .as_integer()
        .map(|n| n as f64)
        .or_else(|| handle.as_real())
}
