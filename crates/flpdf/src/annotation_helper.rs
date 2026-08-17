//! qpdf correspondence: `QPDFAnnotationObjectHelper.cc`.
//! Typed accessor helpers for annotation objects.
//!
//! [`AnnotationObjectHelper`] wraps an annotation [`ObjectRef`] together with
//! a `&mut Pdf<R>` and exposes typed, fail-soft read-only accessors for the
//! common annotation attributes, mirroring qpdf's own transparently
//! dereferencing `QPDFObjectHandle` API on top of this crate's
//! [`ObjectHandle`], which requires an explicit resolve at every hop
//! (`Pdf::resolve_object_handle`) — see
//! [`crate::form_field_object_helper::FormFieldObjectHelper`] for the same
//! established shape.
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
use crate::{Matrix, ObjectRef, Pdf, Rectangle, Result};
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

#[derive(Default)]
pub(crate) struct AppearanceContentOverrides {
    geometry: Option<(Rectangle, Matrix)>,
    flags: Option<i64>,
}

impl AppearanceContentOverrides {
    pub(crate) fn with_geometry(bbox: Rectangle, matrix: Matrix, flags: i64) -> Self {
        Self {
            geometry: Some((bbox, matrix)),
            flags: Some(flags),
        }
    }
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

    /// Construct a helper from the canonical annotation handle returned by
    /// [`crate::PageObjectHelper::get_annotation_handles`]. This preserves
    /// direct annotation dictionaries, which have no [`ObjectRef`], as qpdf's
    /// `QPDFAnnotationObjectHelper` does.
    pub fn from_object_handle(annot: ObjectHandle, pdf: &'a mut Pdf<R>) -> Self {
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
        if ap.as_dictionary().is_some() {
            let ap_sub = ap.get_key(&dict_key(which));
            self.pdf.resolve_object_handle(&ap_sub)?;
            if ap_sub.as_stream_dict().is_some() {
                // qpdf issue #949: a direct appearance stream disregards
                // state entirely (`QPDFAnnotationObjectHelper.cc:59-63`).
                // `/AS` must not even be resolved on this path — qpdf's own
                // eager `getAppearanceState()` call is infallible in C++,
                // but this crate's `/AS` resolution can genuinely error (a
                // malformed or cyclic indirect reference), and that error
                // must not surface for a state qpdf never consults here.
                return Ok(ap_sub);
            }
            if ap_sub.as_dictionary().is_some() {
                let desired_state: Vec<u8> = match state {
                    Some(s) if !s.is_empty() => s.to_vec(),
                    _ => self.get_appearance_state()?,
                };
                if !desired_state.is_empty() {
                    let ap_sub_val = ap_sub.get_key(&dict_key(&desired_state));
                    self.pdf.resolve_object_handle(&ap_sub_val)?;
                    if ap_sub_val.as_stream_dict().is_some() {
                        return Ok(ap_sub_val);
                    }
                }
            } // cov:ignore: llvm-cov brace-region artifact, not untested — reached by both annotation_handle_appearance_stream_missing_state_returns_null and _state_dictionary_key_missing_returns_null, same as the pre-existing single-block version of this brace
        } // cov:ignore: llvm-cov brace-region artifact, not untested — same two tests fall through to this outer brace after the inner one
        Ok(ObjectHandle::null())
    }

    // -----------------------------------------------------------------------
    // get_page_content_for_appearance — qpdf appearance placement
    // -----------------------------------------------------------------------

    /// Generate page content that draws the normal appearance as a Form XObject.
    ///
    /// This is the Rust counterpart of
    /// `QPDFAnnotationObjectHelper::getPageContentForAppearance`
    /// (`libqpdf/QPDFAnnotationObjectHelper.cc:78-226`). `name` is the complete
    /// PDF resource name, including its leading slash (for example,
    /// `"/Fxo1"`). An empty result means that the annotation has no usable
    /// normal appearance, its flags do not satisfy the requested contract, its
    /// rectangle or appearance bounding box is invalid, or its transformed
    /// appearance has zero width or height.
    pub fn get_page_content_for_appearance(
        &mut self,
        name: &str,
        rotate: i32,
        required_flags: i64,
        forbidden_flags: i64,
    ) -> Result<Vec<u8>> {
        let appearance = self.get_appearance_stream(b"N", None)?;
        self.build_page_content_for_appearance(
            name,
            appearance,
            rotate,
            required_flags,
            forbidden_flags,
            AppearanceContentOverrides::default(),
        )
    }

    /// Compatibility fallback for a bridge-selected stream whose `/BBox`,
    /// `/Matrix`, or `/F` value is itself a `Pdf::set_object` redirect. The
    /// bridge supplies those already-normalized values; all qpdf placement,
    /// flag gating, stream mutation, and emitted content remain here.
    pub(crate) fn get_page_content_for_selected_appearance_with_geometry(
        &mut self,
        name: &str,
        appearance_ref: ObjectRef,
        rotate: i32,
        required_flags: i64,
        forbidden_flags: i64,
        overrides: AppearanceContentOverrides,
    ) -> Result<Vec<u8>> {
        let appearance = self.pdf.get_object_handle(appearance_ref);
        self.pdf.resolve_object_handle(&appearance)?;
        self.build_page_content_for_appearance(
            name,
            appearance,
            rotate,
            required_flags,
            forbidden_flags,
            overrides,
        )
    }

    fn build_page_content_for_appearance(
        &mut self,
        name: &str,
        appearance: ObjectHandle,
        rotate: i32,
        required_flags: i64,
        forbidden_flags: i64,
        overrides: AppearanceContentOverrides,
    ) -> Result<Vec<u8>> {
        let Some(appearance_dict) = appearance.as_stream_dict() else {
            return Ok(Vec::new());
        };

        let flags = match overrides.flags {
            Some(flags) => flags,
            None => self.get_flags()?,
        };
        if (flags & forbidden_flags) != 0 {
            return Ok(Vec::new());
        }
        if (flags & required_flags) != required_flags {
            return Ok(Vec::new());
        }

        let Some(rect) = self.rectangle_for_key(b"/Rect")? else {
            return Ok(Vec::new());
        };

        let (bbox, matrix) = match overrides.geometry {
            Some((bbox, matrix)) => (bbox, matrix),
            None => {
                let bbox_handle = appearance_dict.get_key(b"/BBox");
                let Some(bbox) = self.rectangle_from_handle(&bbox_handle)? else {
                    return Ok(Vec::new());
                };
                let matrix_handle = appearance_dict.get_key(b"/Matrix");
                let matrix = self.matrix_from_handle(&matrix_handle)?.unwrap_or_default();
                (bbox, matrix)
            }
        };

        let do_rotate = rotate != 0 && (flags & 0x10) != 0;
        let (rect, matrix) = if do_rotate {
            let mut rotated_matrix = Matrix::default();
            rotated_matrix.rotatex90(rotate);
            rotated_matrix.concat(matrix);
            let rect_width = rect.urx - rect.llx;
            let rect_height = rect.ury - rect.lly;
            let rotated_rect = match rotate {
                90 => Rectangle::new(
                    rect.llx,
                    rect.ury,
                    rect.llx + rect_height,
                    rect.ury + rect_width,
                ),
                180 => Rectangle::new(
                    rect.llx - rect_width,
                    rect.ury,
                    rect.llx,
                    rect.ury + rect_height,
                ),
                270 => Rectangle::new(
                    rect.llx - rect_height,
                    rect.ury - rect_width,
                    rect.llx,
                    rect.ury,
                ),
                _ => rect,
            };
            (rotated_rect, rotated_matrix)
        } else {
            (rect, matrix)
        };

        let transformed_bbox = matrix.transform_rectangle(bbox);
        let width = transformed_bbox.urx - transformed_bbox.llx;
        let height = transformed_bbox.ury - transformed_bbox.lly;
        if width == 0.0 || height == 0.0 {
            return Ok(Vec::new());
        }

        let mut placement = Matrix::default();
        placement.translate(rect.llx, rect.lly);
        placement.scale(
            (rect.urx - rect.llx) / width,
            (rect.ury - rect.lly) / height,
        );
        placement.translate(-transformed_bbox.llx, -transformed_bbox.lly);
        if do_rotate {
            placement.rotatex90(rotate);
        }

        appearance_dict.replace_key(b"/Subtype", ObjectHandle::name(b"Form".to_vec()))?;
        self.pdf.mark_object_handle_dirty(&appearance_dict)?;

        Ok(format!("q\n{} cm\n{} Do\nQ\n", placement.unparse(), name).into_bytes())
    }

    /// Return a normalized rectangle for a required numeric array key.
    fn rectangle_for_key(&mut self, key: &[u8]) -> Result<Option<Rectangle>> {
        let handle = self.resolved_key(key)?;
        self.rectangle_from_handle(&handle)
    }

    /// Return a normalized rectangle when `handle` is a valid four-number array.
    fn rectangle_from_handle(&mut self, handle: &ObjectHandle) -> Result<Option<Rectangle>> {
        self.pdf.resolve_object_handle(handle)?;
        let Some(items) = handle.as_array() else {
            return Ok(None);
        };
        if items.len() != 4 {
            return Ok(None);
        }
        let mut numbers = [0.0; 4];
        for (index, item) in items.iter().enumerate() {
            self.pdf.resolve_object_handle(item)?;
            let Some(number) = as_number(item) else {
                return Ok(None);
            };
            numbers[index] = number;
        }
        Ok(Some(Rectangle::new(
            numbers[0].min(numbers[2]),
            numbers[1].min(numbers[3]),
            numbers[0].max(numbers[2]),
            numbers[1].max(numbers[3]),
        )))
    }

    /// Return a six-number matrix, or `None` when qpdf would use identity.
    fn matrix_from_handle(&mut self, handle: &ObjectHandle) -> Result<Option<Matrix>> {
        self.pdf.resolve_object_handle(handle)?;
        let Some(items) = handle.as_array() else {
            return Ok(None);
        };
        if items.len() != 6 {
            return Ok(None);
        }
        let mut numbers = [0.0; 6];
        for (index, item) in items.iter().enumerate() {
            self.pdf.resolve_object_handle(item)?;
            let Some(number) = as_number(item) else {
                return Ok(None);
            };
            numbers[index] = number;
        }
        Ok(Some(Matrix::from(numbers)))
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
