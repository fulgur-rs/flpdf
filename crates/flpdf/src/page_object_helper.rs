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
//! - [`rotate`](PageObjectHelper::rotate) — **getter** that uses the page-local
//!   inherited `/Rotate` lookup.
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

use crate::content_stream::{ObjectHandleParserCallbacks, ParseControl};
use crate::object_handle::{ObjectHandle, ObjectHandleIdentity};
use crate::pages::{
    is_inheritable_page_attribute, next_page_parent,
    resolve_inherited_handle_from_node_with_max_depth, DEFAULT_MAX_PAGE_TREE_DEPTH,
};
use crate::pipeline::{Pipeline, PipelineError, PlString};
use crate::token_filter::TokenFilter;
use crate::tokenizer::{Token, TokenType};
use crate::writer::DecodeLevel;
use crate::{Dictionary, Error, Matrix, Object, ObjectRef, Pdf, Rectangle, Result};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::io::{Read, Seek};
use std::rc::Rc;

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
    object: ObjectHandle,
    page_ref: Option<ObjectRef>,
    pdf: &'a mut Pdf<R>,
}

#[derive(Default)]
struct ObjectRecordingCallbacks {
    objects: Vec<Object>,
}

struct ExternalizedInlineImage {
    name: Vec<u8>,
    dictionary: ObjectHandle,
    data: Vec<u8>,
}

struct InlineImageExternalizer {
    min_size: usize,
    color_spaces: Option<ObjectHandle>,
    resource_names: std::collections::BTreeSet<Vec<u8>>,
    min_suffix: usize,
    bi_bytes: Vec<u8>,
    dict_bytes: Vec<u8>,
    in_inline_image: bool,
    images: Vec<ExternalizedInlineImage>,
    unresolved_color_spaces: Vec<Vec<u8>>,
}

impl InlineImageExternalizer {
    fn new(
        min_size: usize,
        color_spaces: Option<ObjectHandle>,
        resource_names: std::collections::BTreeSet<Vec<u8>>,
    ) -> Self {
        Self {
            min_size,
            color_spaces,
            resource_names,
            min_suffix: 1,
            bi_bytes: Vec::new(),
            dict_bytes: Vec::new(),
            in_inline_image: false,
            images: Vec::new(),
            unresolved_color_spaces: Vec::new(),
        }
    }

    fn convert_inline_image_dictionary(
        &mut self,
        input: &[u8],
        image_len: usize,
    ) -> std::result::Result<ObjectHandle, PipelineError> {
        let parsed = ObjectHandle::parse(input)
            .map_err(|error| PipelineError::runtime(error.to_string()))?;
        let Some(entries) = parsed.as_dictionary() else {
            return Err(PipelineError::runtime(
                "inline image dictionary did not parse as a dictionary",
            ));
        };
        let result = ObjectHandle::dictionary(Vec::new());
        result
            .replace_key(b"/Type", ObjectHandle::name(b"XObject".to_vec()))
            .map_err(|error| PipelineError::runtime(error.to_string()))?;
        result
            .replace_key(b"/Subtype", ObjectHandle::name(b"Image".to_vec()))
            .map_err(|error| PipelineError::runtime(error.to_string()))?;

        for (key, value) in entries {
            let target_key = match key.as_slice() {
                b"/BPC" => b"/BitsPerComponent".as_slice(),
                b"/CS" => b"/ColorSpace".as_slice(),
                b"/D" => b"/Decode".as_slice(),
                b"/DP" => b"/DecodeParms".as_slice(),
                b"/F" => b"/Filter".as_slice(),
                b"/H" => b"/Height".as_slice(),
                b"/IM" => b"/ImageMask".as_slice(),
                b"/I" => b"/Interpolate".as_slice(),
                b"/W" => b"/Width".as_slice(),
                _ => key.as_slice(),
            };
            let value = if target_key == b"/ColorSpace" {
                self.convert_color_space(value)
            } else if target_key == b"/Filter" {
                self.convert_filters(value)
            } else {
                value
            };
            result
                .replace_key(target_key, value)
                .map_err(|error| PipelineError::runtime(error.to_string()))?;
        }
        result
            .replace_key(
                b"/Length",
                ObjectHandle::integer(i64::try_from(image_len).unwrap_or(i64::MAX)),
            )
            .map_err(|error| PipelineError::runtime(error.to_string()))?;
        Ok(result)
    }

    fn convert_color_space(&mut self, value: ObjectHandle) -> ObjectHandle {
        let Some(name) = value.as_name() else {
            return value;
        };
        let name = name.strip_prefix(b"/").unwrap_or(&name);
        let builtin = match name {
            b"G" => Some(b"DeviceGray".as_slice()),
            b"RGB" => Some(b"DeviceRGB".as_slice()),
            b"CMYK" => Some(b"DeviceCMYK".as_slice()),
            b"I" => Some(b"Indexed".as_slice()),
            _ => None,
        };
        if let Some(name) = builtin {
            return ObjectHandle::name(name.to_vec());
        }
        if let Some(color_spaces) = &self.color_spaces {
            let mut key = b"/".to_vec();
            key.extend_from_slice(name);
            if color_spaces.has_key(&key) {
                return color_spaces.get_key(&key);
            }
        }
        self.unresolved_color_spaces.push(name.to_vec());
        value
    }

    fn convert_filters(&self, value: ObjectHandle) -> ObjectHandle {
        let Some(name) = value.as_name() else {
            let Some(items) = value.as_array() else {
                return value;
            };
            return ObjectHandle::array(
                items
                    .into_iter()
                    .map(|item| self.convert_filter_name(item))
                    .collect(),
            );
        };
        self.convert_filter_name(ObjectHandle::name(name))
    }

    fn convert_filter_name(&self, value: ObjectHandle) -> ObjectHandle {
        let Some(name) = value.as_name() else {
            return value;
        };
        let name = name.strip_prefix(b"/").unwrap_or(&name);
        let expanded = match name {
            b"AHx" => Some(b"ASCIIHexDecode".as_slice()),
            b"A85" => Some(b"ASCII85Decode".as_slice()),
            b"LZW" => Some(b"LZWDecode".as_slice()),
            b"Fl" => Some(b"FlateDecode".as_slice()),
            b"RL" => Some(b"RunLengthDecode".as_slice()),
            b"CCF" => Some(b"CCITTFaxDecode".as_slice()),
            b"DCT" => Some(b"DCTDecode".as_slice()),
            _ => None,
        };
        expanded
            .map(|name| ObjectHandle::name(name.to_vec()))
            .unwrap_or(value)
    }

    fn next_name(&mut self) -> Vec<u8> {
        loop {
            let mut name = b"/IIm".to_vec();
            name.extend_from_slice(self.min_suffix.to_string().as_bytes());
            self.min_suffix += 1;
            if self.resource_names.insert(name.clone()) {
                return name;
            }
        }
    }
}

impl TokenFilter for InlineImageExternalizer {
    fn handle_token(
        &mut self,
        token: &Token,
        output: &mut crate::TokenFilterOutput<'_>,
    ) -> crate::PipelineResult<()> {
        if self.in_inline_image {
            if token.token_type == TokenType::InlineImage {
                if token.value.len() >= self.min_size {
                    let dict_bytes = self.dict_bytes.clone();
                    let dictionary =
                        self.convert_inline_image_dictionary(&dict_bytes, token.value.len())?;
                    let name = self.next_name();
                    self.images.push(ExternalizedInlineImage {
                        name: name.clone(),
                        dictionary,
                        data: token.value.clone(),
                    });
                    output.write(&name)?;
                    output.write(b" Do\n")?;
                } else {
                    output.write(&self.bi_bytes)?;
                    output.write_token(token)?;
                    self.in_inline_image = false;
                }
                return Ok(());
            }
            if token.is_word_value(b"ID") {
                self.bi_bytes.extend_from_slice(&token.value);
                self.dict_bytes.extend_from_slice(b" >>");
            } else if token.is_word_value(b"EI") {
                self.in_inline_image = false;
            } else {
                self.bi_bytes.extend_from_slice(&token.raw);
                self.dict_bytes.extend_from_slice(&token.raw);
            }
            return Ok(());
        }

        if token.is_word_value(b"BI") {
            self.bi_bytes = token.value.clone();
            self.dict_bytes = b"<< ".to_vec();
            self.in_inline_image = true;
        } else {
            output.write_token(token)?;
        }
        Ok(())
    }
}

impl ObjectHandleParserCallbacks for ObjectRecordingCallbacks {
    fn handle_object(
        &mut self,
        object: ObjectHandle,
        _offset: usize,
        _length: usize,
    ) -> Result<ParseControl> {
        self.objects.push(object.materialize()?);
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
        let object = pdf.get_object_handle(page_ref);
        Self {
            object,
            page_ref: Some(page_ref),
            pdf,
        }
    }

    /// Create a helper over a page or Form XObject handle.
    ///
    /// A Form XObject is represented by its stream handle; page attributes are
    /// read from the stream dictionary and do not walk a page-tree parent
    /// chain, matching qpdf's `QPDFPageObjectHelper(QPDFObjectHandle)`.
    pub fn from_object_handle(object: ObjectHandle, pdf: &'a mut Pdf<R>) -> Self {
        let page_ref = object.object_ref();
        Self {
            object,
            page_ref,
            pdf,
        }
    }

    fn target_description(&self) -> String {
        self.page_ref
            .map(|object_ref| object_ref.to_string())
            .unwrap_or_else(|| "direct object".to_owned())
    }

    fn require_page_ref(&self) -> Result<ObjectRef> {
        self.page_ref.ok_or_else(|| {
            Error::Unsupported("operation requires a page object reference".to_owned())
        })
    }

    /// Resolve the target and return whether it is a Form XObject. Page
    /// dictionaries and Form stream dictionaries are the only supported qpdf
    /// PageObjectHelper targets.
    fn resolved_attribute_target(&mut self) -> Result<(ObjectHandle, bool)> {
        let description = self.target_description();
        resolve_attribute_target(self.pdf, self.object.clone(), &description)
    }

    /// Return the live canonical handle for this page after validating its
    /// `/Type`. This is the page-helper equivalent of qpdf's
    /// `QPDFPageObjectHelper` construction over a `QPDFObjectHandle`: all
    /// subsequent page attributes and mutations operate on the resolver-backed
    /// object graph rather than on a legacy `Object` snapshot.
    fn resolved_page_handle(&mut self) -> Result<ObjectHandle> {
        let (object, is_form) = self.resolved_attribute_target()?;
        if is_form {
            return Err(Error::Unsupported(format!(
                "object {} is a Form XObject, expected /Type /Page",
                self.target_description()
            )));
        }
        Ok(object)
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
        let description = self.target_description();
        get_attribute_for_target(
            self.pdf,
            self.object.clone(),
            key,
            copy_if_shared,
            &description,
        )
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

    /// Convert this page into a new, document-owned Form XObject.
    ///
    /// The new stream retains a provider over the page's canonical content
    /// route, so conversion does not eagerly decode or concatenate page bytes.
    /// `/Resources`, `/Group`, and the effective `/TrimBox` are shallow-copied;
    /// `/Matrix` is emitted when requested and either `/Rotate` or `/UserUnit`
    /// is present, matching qpdf's `getFormXObjectForPage`
    /// (`libqpdf/QPDFPageObjectHelper.cc:740-782`).
    pub fn get_form_xobject_for_page(
        &mut self,
        handle_transformations: bool,
    ) -> Result<ObjectHandle> {
        let page = self.resolved_page_handle()?;
        // Capture the page's original content container before a consumer can
        // replace `/Contents` on the page (overlay does exactly that after
        // creating /Fx0). The provider remains lazy and ObjectHandle-backed,
        // but its source must be the content graph observed at conversion
        // time; otherwise a later page rewrite makes the Form provider read
        // the newly inserted /Fx0 Do fragment recursively.
        let page_contents = page.try_get_key(b"/Contents")?;
        let page_description = format!(
            "contents from page object {}",
            object_handle_description(&page)
        );
        let form = self.pdf.new_stream()?;
        let dict = form
            .as_stream_dict()
            .ok_or_else(|| Error::Internal("new stream has no dictionary".to_owned()))?;
        dict.replace_key(b"/Type", ObjectHandle::name(b"XObject".to_vec()))?;
        dict.replace_key(b"/Subtype", ObjectHandle::name(b"Form".to_vec()))?;

        let resources = self.get_resources(false)?.shallow_copy()?;
        dict.replace_key(b"/Resources", resources)?;
        let group = self.get_attribute(b"/Group", false)?.shallow_copy()?;
        dict.replace_key(b"/Group", group)?;
        let bbox = self.get_trim_box(false, false)?.shallow_copy()?;
        if rectangle_from_handle(self.pdf, &bbox)?.is_none() {
            self.object.warn_if_possible(
                "bounding box is invalid; form XObject created from page will not work",
            )?; // cov:ignore: qpdf warning emission is infallible for a live page handle; only the defensive logger error edge is excluded
        }
        dict.replace_key(b"/BBox", bbox)?;

        if handle_transformations {
            let rotate = self.get_attribute(b"/Rotate", false)?;
            let user_unit = self.get_attribute(b"/UserUnit", false)?;
            if !rotate.is_null() || !user_unit.is_null() {
                let matrix = self.get_matrix_for_transformations(false)?;
                dict.replace_key(
                    b"/Matrix",
                    ObjectHandle::array(
                        matrix
                            .get_as_matrix()
                            .into_iter()
                            .map(ObjectHandle::real)
                            .collect(),
                    ),
                )?; // cov:ignore: canonical Matrix construction and dictionary replacement cannot fail after new_stream allocation
            }
        }

        form.replace_stream_data_with_callback(
            move |pipeline| {
                let mut all_description = String::new();
                page_contents.pipe_content_streams(
                    pipeline,
                    &page_description,
                    &mut all_description,
                )
            },
            None,
            None,
        )?; // cov:ignore: the provider closure is built from canonical page contents; this is only its defensive setup error edge
        self.pdf.mark_object_handle_dirty(&form)?;
        Ok(form)
    }

    /// Return qpdf's page/Form transformation matrix, using the effective
    /// `/TrimBox`, inherited `/Rotate`, and leaf `/UserUnit`.
    pub fn get_matrix_for_transformations(&mut self, invert: bool) -> Result<Matrix> {
        let bbox = self.get_trim_box(false, false)?;
        let Some(rect) = self.rectangle_for_matrix(&bbox)? else {
            return Ok(Matrix::default());
        };
        let rotate_obj = self.get_attribute(b"/Rotate", false)?;
        let scale_obj = self.get_attribute(b"/UserUnit", false)?;
        if rotate_obj.is_null() && scale_obj.is_null() {
            return Ok(Matrix::default());
        }

        let mut scale = scale_obj
            .as_integer()
            .map(|value| value as f64)
            .or_else(|| scale_obj.as_real())
            .unwrap_or(1.0);
        let mut rotate = rotate_obj.as_integer().unwrap_or(0) as i32;
        if invert {
            if scale == 0.0 {
                return Ok(Matrix::default());
            }
            scale = 1.0 / scale;
            rotate = 360 - rotate;
        }
        let width = rect.urx - rect.llx;
        let height = rect.ury - rect.lly;
        Ok(match rotate {
            90 => Matrix::new(0.0, -scale, scale, 0.0, 0.0, width * scale),
            180 => Matrix::new(-scale, 0.0, 0.0, -scale, width * scale, height * scale),
            270 => Matrix::new(0.0, scale, -scale, 0.0, height * scale, 0.0),
            _ => Matrix::new(scale, 0.0, 0.0, scale, 0.0, 0.0),
        })
    }

    /// Compute the qpdf placement matrix for `form` inside `rect`.
    ///
    /// Form `/BBox` and `/Matrix` are read from live canonical handles; the
    /// destination inverse transformation comes from this page helper. A
    /// malformed or degenerate Form returns `Ok(None)`, matching qpdf's empty
    /// `QPDFMatrix` result from `getMatrixForFormXObjectPlacement`
    /// (`libqpdf/QPDFPageObjectHelper.cc:764-838`).
    pub fn get_matrix_for_form_xobject_placement(
        &mut self,
        form: ObjectHandle,
        rect: Rectangle,
        invert_transformations: bool,
        allow_shrink: bool,
        allow_expand: bool,
    ) -> Result<Option<Matrix>> {
        self.pdf.resolve(&form)?;
        let form_dict = if form.is_form_xobject()? {
            // cov:ignore-start: is_form_xobject only returns true for a stream
            // with a canonical stream dictionary, so this defensive branch is
            // unreachable from the public ObjectHandle API.
            form.as_stream_dict().ok_or_else(|| {
                Error::Unsupported("Form XObject has no stream dictionary".to_owned())
            })?
            // cov:ignore-end
        } else {
            return Ok(None);
        };
        let bbox = self
            .pdf
            .resolve_to_terminal(&form_dict.try_get_key(b"/BBox")?)?;
        let Some(bbox) = rectangle_from_handle(self.pdf, &bbox)? else {
            return Ok(None);
        };
        let form_matrix = self
            .pdf
            .resolve_to_terminal(&form_dict.try_get_key(b"/Matrix")?)?;
        let form_matrix = matrix_from_handle(self.pdf, &form_matrix)?.unwrap_or_default();
        let transform = if invert_transformations {
            self.get_matrix_for_transformations(true)?
        } else {
            Matrix::default()
        };

        let mut work = Matrix::default();
        work.concat(transform);
        work.concat(form_matrix);
        let transformed = work.transform_rectangle(bbox);
        if transformed.urx == transformed.llx || transformed.ury == transformed.lly {
            return Ok(None);
        }

        let rect_w = rect.urx - rect.llx;
        let rect_h = rect.ury - rect.lly;
        let xscale = rect_w / (transformed.urx - transformed.llx);
        let yscale = rect_h / (transformed.ury - transformed.lly);
        let mut scale = xscale.min(yscale);
        if scale > 1.0 {
            if !allow_expand {
                scale = 1.0;
            }
        } else if scale < 1.0 && !allow_shrink {
            scale = 1.0;
        }

        work = Matrix::default();
        work.scale(scale, scale);
        work.concat(transform);
        work.concat(form_matrix);
        let transformed = work.transform_rectangle(bbox);
        let tx = (rect.llx + rect.urx) / 2.0 - (transformed.llx + transformed.urx) / 2.0;
        let ty = (rect.lly + rect.ury) / 2.0 - (transformed.lly + transformed.ury) / 2.0;
        let mut result = Matrix::default();
        result.translate(tx, ty);
        result.scale(scale, scale);
        result.concat(transform);
        Ok(Some(result))
    }

    /// Build qpdf's `placeFormXObject` content fragment and return the matrix
    /// used to place the Form. `name` is the complete PDF resource name,
    /// including its leading slash. A malformed or degenerate Form uses
    /// qpdf's identity-matrix fallback.
    pub fn place_form_xobject(
        &mut self,
        form: ObjectHandle,
        name: &str,
        rect: Rectangle,
        invert_transformations: bool,
        allow_shrink: bool,
        allow_expand: bool,
    ) -> Result<(String, Matrix)> {
        let matrix = self
            .get_matrix_for_form_xobject_placement(
                form,
                rect,
                invert_transformations,
                allow_shrink,
                allow_expand,
            )? // cov:ignore: only the defensive provider-error edge of this multiline call is untestable with a valid Form
            .unwrap_or_default();
        let fragment = format!("q\n{} cm\n{} Do\nQ\n", matrix.unparse(), name);
        Ok((fragment, matrix))
    }

    /// Variant of [`Self::place_form_xobject`] that writes the placement
    /// matrix into the caller's slot, matching qpdf's overload that accepts a
    /// `QPDFMatrix&`.
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors qpdf's placeFormXObject overload and keeps placement flags explicit"
    )]
    pub fn place_form_xobject_with_matrix(
        &mut self,
        form: ObjectHandle,
        name: &str,
        rect: Rectangle,
        matrix: &mut Matrix,
        invert_transformations: bool,
        allow_shrink: bool,
        allow_expand: bool,
    ) -> Result<String> {
        let (fragment, computed) = self.place_form_xobject(
            form,
            name,
            rect,
            invert_transformations,
            allow_shrink,
            allow_expand,
        )?; // cov:ignore: matrix placement is validated by the helper before this overload; only its defensive error edge is excluded
        *matrix = computed;
        Ok(fragment)
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
        let (_, is_form) = self.resolved_attribute_target()?;
        let page = if is_form {
            // cov:ignore-start: is_form is returned only for a stream whose
            // canonical dictionary was already validated by is_form_xobject.
            self.object.as_stream_dict().ok_or_else(|| {
                Error::Unsupported("Form XObject has no stream dictionary".to_owned())
            })?
            // cov:ignore-end
        } else {
            self.object.clone()
        };
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
        self.resolved_page_handle().map(|_| ())
    }

    // -----------------------------------------------------------------------
    // content_stream_objects
    // -----------------------------------------------------------------------

    /// Return the qpdf-shaped content object events for this page.
    ///
    /// Aggregates the page's `/Contents` entry (single stream or array), decodes
    /// each stream through its filter pipeline (same as
    /// [`crate::pages::page_content_bytes`]), then parses the concatenated bytes
    /// through [`crate::content_stream::parse_content_stream_data`].
    ///
    /// Returns an empty `Vec` when the page has no `/Contents`.
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] when `page_ref` does not resolve to a
    ///   `/Type /Page` dictionary, or when a `/Contents` element is not a stream.
    /// - Any error from [`crate::pages::page_content_bytes`] or
    ///   [`crate::content_stream::parse_content_stream_data`].
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
        let mut callbacks = ObjectRecordingCallbacks::default();
        self.parse_contents(&mut callbacks)?;
        Ok(callbacks.objects)
    }

    /// Return the page's `/Contents` as canonical stream handles.
    ///
    /// This is the direct `QPDFPageObjectHelper::getPageContents` route
    /// (`libqpdf/QPDFPageObjectHelper.cc:439-442`) and deliberately preserves
    /// each stream's identity and lazy provider instead of decoding it into a
    /// byte buffer or legacy [`Object`] value.
    pub fn get_page_contents(&mut self) -> Result<Vec<ObjectHandle>> {
        let (target, _) = self.resolved_attribute_target()?;
        target.get_page_contents()
    }

    /// Add a canonical stream to the beginning or end of `/Contents`.
    ///
    /// Mirrors `QPDFPageObjectHelper::addPageContents`
    /// (`libqpdf/QPDFPageObjectHelper.cc:449-452`).
    pub fn add_page_contents(&mut self, contents: ObjectHandle, first: bool) -> Result<()> {
        let (target, _) = self.resolved_attribute_target()?;
        target.add_page_contents(contents, first)?;
        self.pdf.mark_object_handle_dirty(&target)
    }

    /// Rotate the page in the live object graph.
    ///
    /// Mirrors `QPDFPageObjectHelper::rotatePage`
    /// (`libqpdf/QPDFPageObjectHelper.cc:468-470`).
    pub fn rotate_page(&mut self, angle: i32, relative: bool) -> Result<()> {
        let (target, _) = self.resolved_attribute_target()?;
        target.rotate_page(angle, relative)?;
        self.pdf.mark_object_handle_dirty(&target)
    }

    /// Bake the page's direct qpdf `/Rotate` value into its boxes, contents,
    /// and annotations.
    ///
    /// This is `QPDFPageObjectHelper::flattenRotation`
    /// (`libqpdf/QPDFPageObjectHelper.cc:862-991`). qpdf intentionally reads
    /// `/Rotate`, `/MediaBox`, and the optional page boxes directly from the
    /// page object here; inherited values are not materialized by this method.
    /// The page-document orchestration that calls this method remains outside
    /// [`PageObjectHelper`]. Annotation field-tree work is delegated to
    /// [`crate::AcroFormDocumentHelper`]'s canonical transform route.
    pub fn flatten_rotation(&mut self) -> Result<()> {
        self.require_page_ref()?;
        let page = self.resolved_page_handle()?;

        let rotate = page.try_get_key(b"/Rotate")?.try_as_integer()?.unwrap_or(0);
        if !matches!(rotate, 90 | 180 | 270) {
            return Ok(());
        }

        let media = self
            .pdf
            .resolve_to_terminal(&page.try_get_key(b"/MediaBox")?)?;
        let Some(media) = rectangle_from_handle(self.pdf, &media)? else {
            return Ok(());
        };
        let matrix = flatten_rotation_matrix(rotate, media);

        for key in [
            b"/MediaBox".as_slice(),
            b"/CropBox",
            b"/BleedBox",
            b"/TrimBox",
            b"/ArtBox",
        ] {
            let value = self.pdf.resolve_to_terminal(&page.try_get_key(key)?)?;
            let Some(rectangle) = rectangle_from_handle(self.pdf, &value)? else {
                continue;
            };
            page.replace_key(
                key,
                rectangle_to_handle(flatten_rotation_box(rotate, media, rectangle)),
            )?; // cov:ignore: LLVM maps this multiline box replacement to a defensive continuation edge
        }

        let prefix = self.pdf.new_stream_with_data(Rc::new(
            format!("q\n{} cm\n", matrix.unparse()).into_bytes(),
        ))?; // cov:ignore: LLVM maps this multiline prefix-stream allocation to a defensive continuation edge
        self.add_page_contents(prefix, true)?;
        let suffix = self.pdf.new_stream_with_data(Rc::new(b"\nQ\n".to_vec()))?;
        self.add_page_contents(suffix, false)?;

        page.remove_key(b"/Rotate");
        // `getAttribute(..., false)` is qpdf's inherited lookup after the
        // direct key is removed. If an ancestor supplied rotation, materialize
        // the zero that masks it on this page.
        let inherited_rotate = self.get_attribute(b"/Rotate", false)?;
        if !inherited_rotate.is_null() {
            page.replace_key(b"/Rotate", ObjectHandle::integer(0))?;
        }
        self.pdf.mark_object_handle_dirty(&page)?;

        let old_annots = page.try_get_key(b"/Annots")?;
        if old_annots.try_as_array()?.is_some() {
            let transformed = {
                let mut acroform = crate::AcroFormDocumentHelper::new(self.pdf)?;
                let transformed = acroform.transform_annotations(old_annots, matrix)?;
                acroform.remove_form_fields(&transformed.old_fields)?;
                acroform.add_form_fields(transformed.new_fields.clone())?;
                transformed
            };
            page.replace_key(b"/Annots", ObjectHandle::array(transformed.new_annotations))?;
            self.pdf.mark_object_handle_dirty(&page)?;
        }

        Ok(())
    }

    /// Copy annotations from another page in the same document, applying
    /// `cm` to every copied rectangle and appearance matrix.
    ///
    /// This is the same-document branch of qpdf's
    /// `QPDFPageObjectHelper::copyAnnotations`
    /// (`libqpdf/QPDFPageObjectHelper.cc:992-1039`). The canonical AcroForm
    /// helper owns field-tree copying and qualified-name renaming.
    pub fn copy_annotations(&mut self, from_page: ObjectHandle, cm: Matrix) -> Result<()> {
        self.copy_annotations_with_reserved_names(from_page, cm, &BTreeSet::new())
    }

    /// Same-document annotation copy with qpdf's still-live primary field-name
    /// reservations applied to collision renaming.
    pub(crate) fn copy_annotations_with_reserved_names(
        &mut self,
        from_page: ObjectHandle,
        cm: Matrix,
        reserved_names: &BTreeSet<Vec<u8>>,
    ) -> Result<()> {
        self.copy_annotations_with_reserved_names_impl(from_page, cm, reserved_names, false)
    }

    /// Same-document annotation copy for qpdf's page-selection replay.
    ///
    /// `QPDFJob::handlePageSpecs` constructs its destination AcroForm helper
    /// before repeated-page copies begin. The merged flpdf target already
    /// contains those page copies when this replay starts, so a fresh full
    /// `analyze()` would report their not-yet-added widgets as orphaned. Keep
    /// the canonical field-tree copy/rename route, but defer the page orphan
    /// scan to the completed output boundary.
    pub(crate) fn copy_annotations_with_field_tree_only(
        &mut self,
        from_page: ObjectHandle,
        cm: Matrix,
        reserved_names: &BTreeSet<Vec<u8>>,
    ) -> Result<()> {
        self.copy_annotations_with_reserved_names_impl(from_page, cm, reserved_names, true)
    }

    fn copy_annotations_with_reserved_names_impl(
        &mut self,
        from_page: ObjectHandle,
        cm: Matrix,
        reserved_names: &BTreeSet<Vec<u8>>,
        field_tree_only: bool,
    ) -> Result<()> {
        let destination = self.resolved_page_handle()?;
        self.require_page_ref()?;
        validate_same_document_page_handle(self.pdf, &from_page)?;
        let old_annots = self
            .pdf
            .resolve_to_terminal(&from_page.try_get_key(b"/Annots")?)?;
        if old_annots.try_as_array()?.is_none() {
            return Ok(());
        }

        let transformed = {
            let mut acroform = if field_tree_only {
                crate::AcroFormDocumentHelper::new_for_field_tree(self.pdf)?
            } else {
                crate::AcroFormDocumentHelper::new(self.pdf)?
            };
            let transformed = acroform.transform_annotations(old_annots, cm)?;
            acroform.add_and_rename_form_fields_with_reserved_names(
                transformed.new_fields.clone(),
                reserved_names,
            )?; // cov:ignore: malformed field-copy errors are covered by AcroForm transform tests.
            transformed
        };
        append_annotation_handles(self.pdf, &destination, transformed.new_annotations)?;
        Ok(())
    }

    /// Copy annotations from a page owned by `source`, applying `cm` to every
    /// copied rectangle and appearance matrix.
    ///
    /// This is qpdf's foreign-document `copyAnnotations` branch. The source
    /// handle must be an indirect page handle owned by the supplied source
    /// document; destination field/resource reconciliation remains in the
    /// canonical [`crate::AcroFormDocumentHelper`] implementation.
    pub fn copy_annotations_from<RS: Read + Seek>(
        &mut self,
        from_page: ObjectHandle,
        cm: Matrix,
        source: &mut Pdf<RS>,
    ) -> Result<()> {
        self.copy_annotations_from_with_reserved_names(from_page, cm, source, &BTreeSet::new())
    }

    /// Fix annotations after a foreign page has already been inserted into a
    /// destination document, applying qpdf's replacement semantics.
    ///
    /// This is qpdf's `QPDFAcroFormDocumentHelper::fixCopiedAnnotations`
    /// (`libqpdf/QPDFAcroFormDocumentHelper.cc:1017-1047`), not
    /// `QPDFPageObjectHelper::copyAnnotations`: the page insertion has
    /// already copied the source `/Annots`, so the transformed annotations
    /// replace the destination array instead of being appended to it.
    pub(crate) fn fix_copied_annotations_from<RS: Read + Seek>(
        &mut self,
        from_page: ObjectHandle,
        source: &mut Pdf<RS>,
    ) -> Result<()> {
        let destination = self.resolved_page_handle()?;
        self.require_page_ref()?;
        validate_foreign_page_handle(source, self.pdf, &from_page)?;
        let old_annots = from_page.try_get_key(b"/Annots")?;
        if old_annots.try_as_array()?.is_none() {
            return Ok(());
        }

        let transformed = {
            let mut acroform = crate::AcroFormDocumentHelper::new(self.pdf)?;
            let transformed =
                acroform.transform_annotations_from(old_annots, Matrix::default(), source)?;
            acroform.add_and_rename_form_fields_with_reserved_names(
                transformed.new_fields.clone(),
                &BTreeSet::new(),
            )?; // cov:ignore: malformed field-copy errors are covered by AcroForm transform tests.
            transformed
        };
        destination.replace_key(b"/Annots", ObjectHandle::array(transformed.new_annotations))?;
        self.pdf.mark_object_handle_dirty(&destination)?;
        Ok(())
    }

    /// Foreign-document annotation copy with qpdf's still-live primary
    /// field-name reservations applied to collision renaming.
    pub(crate) fn copy_annotations_from_with_reserved_names<RS: Read + Seek>(
        &mut self,
        from_page: ObjectHandle,
        cm: Matrix,
        source: &mut Pdf<RS>,
        reserved_names: &BTreeSet<Vec<u8>>,
    ) -> Result<()> {
        self.copy_annotations_from_with_reserved_names_impl(
            from_page,
            cm,
            source,
            reserved_names,
            false,
        )
    }

    /// Foreign annotation copy for qpdf's page-selection replay. See
    /// [`Self::copy_annotations_with_field_tree_only`] for why the destination
    /// helper must defer its page orphan scan while copied pages are pending.
    pub(crate) fn copy_annotations_from_with_field_tree_only<RS: Read + Seek>(
        &mut self,
        from_page: ObjectHandle,
        cm: Matrix,
        source: &mut Pdf<RS>,
        reserved_names: &BTreeSet<Vec<u8>>,
    ) -> Result<()> {
        self.copy_annotations_from_with_reserved_names_impl(
            from_page,
            cm,
            source,
            reserved_names,
            true,
        )
    }

    fn copy_annotations_from_with_reserved_names_impl<RS: Read + Seek>(
        &mut self,
        from_page: ObjectHandle,
        cm: Matrix,
        source: &mut Pdf<RS>,
        reserved_names: &BTreeSet<Vec<u8>>,
        field_tree_only: bool,
    ) -> Result<()> {
        let destination = self.resolved_page_handle()?;
        self.require_page_ref()?;
        validate_foreign_page_handle(source, self.pdf, &from_page)?;
        let old_annots = source.resolve_to_terminal(&from_page.try_get_key(b"/Annots")?)?;
        if old_annots.try_as_array()?.is_none() {
            return Ok(());
        }

        let transformed = {
            let mut acroform = if field_tree_only {
                crate::AcroFormDocumentHelper::new_for_field_tree(self.pdf)?
            } else {
                crate::AcroFormDocumentHelper::new(self.pdf)?
            };
            let transformed = acroform.transform_annotations_from(old_annots, cm, source)?;
            acroform.add_and_rename_form_fields_with_reserved_names(
                transformed.new_fields.clone(),
                reserved_names,
            )?; // cov:ignore: malformed field-copy errors are covered by AcroForm transform tests.
            transformed
        };
        append_annotation_handles(self.pdf, &destination, transformed.new_annotations)?;
        Ok(())
    }

    /// Coalesce the page's content streams into one lazy provider-backed stream.
    pub fn coalesce_content_streams(&mut self) -> Result<()> {
        let (target, _) = self.resolved_attribute_target()?;
        target.coalesce_content_streams()?;
        self.pdf.mark_object_handle_dirty(&target)
    }

    /// Return a new indirect page whose dictionary is a qpdf-style shallow
    /// copy of this page.
    ///
    /// Direct child dictionaries/arrays are copied while indirect content and
    /// resource objects retain their identity, matching
    /// `QPDFPageObjectHelper::shallowCopyPage`
    /// (`libqpdf/QPDFPageObjectHelper.cc:654-662`). The copy is not inserted
    /// into the page tree; callers may add it through the document helper.
    pub fn shallow_copy_page(&mut self) -> Result<ObjectHandle> {
        let page = self.resolved_page_handle()?;
        if page.is_direct() {
            return Err(Error::Internal(
                "shallowCopyPage called with a direct object".to_owned(),
            ));
        }
        let copy = page.shallow_copy()?;
        self.pdf.make_indirect_object_handle(copy)
    }

    /// Parse the page contents through canonical ObjectHandle parser callbacks.
    pub fn parse_page_contents<C: ObjectHandleParserCallbacks>(
        &mut self,
        callbacks: &mut C,
    ) -> Result<()> {
        self.parse_contents(callbacks)
    }

    /// qpdf's old name for [`Self::parse_page_contents`].
    pub fn parse_contents<C: ObjectHandleParserCallbacks>(
        &mut self,
        callbacks: &mut C,
    ) -> Result<()> {
        let (target, is_form) = self.resolved_attribute_target()?;
        if is_form {
            target.parse_as_contents(callbacks)
        } else {
            target.parse_page_contents(callbacks)
        }
    }

    /// Apply a lexical token filter to decoded page contents.
    pub fn filter_page_contents<'b>(
        &mut self,
        filter: &'b mut dyn TokenFilter,
        next: Option<&'b mut dyn Pipeline>,
    ) -> Result<()> {
        self.filter_contents(filter, next)
    }

    /// qpdf's old name for [`Self::filter_page_contents`].
    pub fn filter_contents<'b>(
        &mut self,
        filter: &'b mut dyn TokenFilter,
        next: Option<&'b mut dyn Pipeline>,
    ) -> Result<()> {
        let (target, is_form) = self.resolved_attribute_target()?;
        if is_form {
            target.filter_as_contents(filter, next)
        } else {
            target.filter_page_contents(filter, next)
        }
    }

    /// Pipe decoded page contents into a pipeline.
    pub fn pipe_page_contents(&mut self, pipeline: &mut dyn Pipeline) -> Result<()> {
        self.pipe_contents(pipeline)
    }

    /// qpdf's old name for [`Self::pipe_page_contents`].
    pub fn pipe_contents(&mut self, pipeline: &mut dyn Pipeline) -> Result<()> {
        let (target, is_form) = self.resolved_attribute_target()?;
        if is_form {
            let mut filtering_attempted = false;
            let succeeded = target.pipe_stream_data(
                pipeline,
                &mut filtering_attempted,
                0,
                DecodeLevel::Specialized,
                false,
                false,
            )?; // cov:ignore: a failed Form provider is represented by succeeded=false; this is only the defensive provider-error edge
            if succeeded {
                Ok(())
            } else {
                Err(Error::Unsupported(format!(
                    "object {}: errors while decoding content stream",
                    object_handle_description(&target)
                )))
            }
        } else {
            target.pipe_page_contents(pipeline)
        }
    }

    /// Attach a lazy token filter to the page's content stream.
    pub fn add_content_token_filter(&mut self, filter: Rc<RefCell<dyn TokenFilter>>) -> Result<()> {
        let (target, is_form) = self.resolved_attribute_target()?;
        if is_form {
            target.add_token_filter(filter)?;
        } else {
            target.add_content_token_filter(filter)?;
        }
        self.pdf.mark_object_handle_dirty(&target)
    }

    /// Remove unused `/Font` and `/XObject` entries from this page or Form's
    /// resource scope through the canonical ObjectHandle parser route.
    ///
    /// This is qpdf's `removeUnreferencedResources`
    /// (`libqpdf/QPDFPageObjectHelper.cc:539-649`). The document-level
    /// `PageDocumentHelper` facade uses this same per-target operation.
    pub fn remove_unreferenced_resources(&mut self) -> Result<()> {
        let (target, is_form) = self.resolved_attribute_target()?;
        if is_form {
            crate::resources::remove_unreferenced_resources_on_form(self.pdf, target)
        } else {
            let page_ref = self.require_page_ref()?;
            crate::resources::remove_unreferenced_resources_on_page(self.pdf, page_ref)
        }
    }

    /// Convert inline images into ordinary Image XObjects.
    ///
    /// This mirrors qpdf's `externalizeInlineImages` implementation
    /// (`libqpdf/QPDFPageObjectHelper.cc:398-437`). The content is filtered
    /// through the canonical page/Form pipeline, while image streams are
    /// allocated and attached after filtering so the callback never needs a
    /// second mutable borrow of the document. With `shallow == false`, nested
    /// Form XObjects are processed in the same bounded traversal as qpdf;
    /// `true` limits the operation to this target.
    pub fn externalize_inline_images(&mut self, min_size: usize, shallow: bool) -> Result<()> {
        let target = self.object.clone();
        let description = self.target_description();
        let mut nested_forms = Vec::new();
        if !shallow {
            self.for_each_form_xobject(true, |object, _, _| {
                nested_forms.push(object);
                Ok(())
            })?;
        }

        externalize_inline_images_for_target(self.pdf, target, &description, min_size)?;
        if !shallow {
            for form in nested_forms {
                let description = object_handle_description(&form);
                externalize_inline_images_for_target(self.pdf, form, &description, min_size)?;
            }
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // resources
    // -----------------------------------------------------------------------

    /// Return the effective `/Resources` dictionary handle.
    pub fn get_resources(&mut self, copy_if_shared: bool) -> Result<ObjectHandle> {
        self.get_attribute(b"/Resources", copy_if_shared)
    }

    /// Visit every XObject directly reachable from this page or Form XObject.
    ///
    /// The callback receives the XObject handle, its containing `/XObject`
    /// dictionary handle, and the decoded resource key. With `recursive=true`,
    /// Form XObjects are visited breadth-first and canonical identity prevents
    /// cycles, matching qpdf's `forEachXObject` traversal
    /// (`libqpdf/QPDFPageObjectHelper.cc:318-357`).
    pub fn for_each_xobject<F>(&mut self, recursive: bool, action: F) -> Result<()>
    where
        F: FnMut(ObjectHandle, ObjectHandle, Vec<u8>) -> Result<()>,
    {
        self.for_each_xobject_filtered(recursive, |_| Ok(true), action)
    }

    fn for_each_xobject_filtered<S, F>(
        &mut self,
        recursive: bool,
        mut selector: S,
        mut action: F,
    ) -> Result<()>
    where
        S: FnMut(&ObjectHandle) -> Result<bool>,
        F: FnMut(ObjectHandle, ObjectHandle, Vec<u8>) -> Result<()>,
    {
        let root = self.resolved_attribute_target()?.0;
        let mut queue = VecDeque::from([root]);
        #[allow(
            clippy::mutable_key_type,
            reason = "qpdf traversal identity intentionally keys on canonical handle identity"
        )]
        let mut seen: HashSet<ObjectHandleIdentity> = HashSet::new();

        while let Some(node) = queue.pop_front() {
            if !seen.insert(node.identity_key()) {
                continue;
            }

            let node_description = object_handle_description(&node);
            let resources = get_attribute_for_target(
                self.pdf,
                node.clone(),
                b"/Resources",
                false,
                &node_description,
            )?; // cov:ignore: traversal already validates each canonical page/Form target; only a defensive resolver error can reach this edge
            if resources.is_null() {
                continue;
            }
            let xobjects = self
                .pdf
                .resolve_to_terminal(&resources.try_get_key(b"/XObject")?)?;
            let Some(entries) = xobjects.as_dictionary() else {
                continue;
            };

            for (key, value) in entries {
                let object = self.pdf.resolve_to_terminal(&value)?;
                if selector(&object)? {
                    action(object.clone(), xobjects.clone(), key)?;
                }
                if recursive && object.is_form_xobject()? {
                    queue.push_back(object);
                }
            }
        }
        Ok(())
    }

    /// Visit image XObjects, optionally recursing through nested Forms.
    pub fn for_each_image<F>(&mut self, recursive: bool, action: F) -> Result<()>
    where
        F: FnMut(ObjectHandle, ObjectHandle, Vec<u8>) -> Result<()>,
    {
        self.for_each_xobject_filtered(recursive, |object| object.is_image(false), action)
    }

    /// Visit Form XObjects, optionally recursing through nested Forms.
    pub fn for_each_form_xobject<F>(&mut self, recursive: bool, action: F) -> Result<()>
    where
        F: FnMut(ObjectHandle, ObjectHandle, Vec<u8>) -> Result<()>,
    {
        self.for_each_xobject_filtered(recursive, |object| object.is_form_xobject(), action)
    }

    /// Return direct image XObjects keyed by their resource names.
    pub fn get_images(&mut self) -> Result<BTreeMap<Vec<u8>, ObjectHandle>> {
        let mut result = BTreeMap::new();
        self.for_each_image(false, |object, _, key| {
            result.insert(key, object);
            Ok(())
        })?;
        Ok(result)
    }

    /// qpdf's old `getPageImages` name for [`Self::get_images`].
    pub fn get_page_images(&mut self) -> Result<BTreeMap<Vec<u8>, ObjectHandle>> {
        self.get_images()
    }

    /// Return direct Form XObjects keyed by their resource names.
    pub fn get_form_xobjects(&mut self) -> Result<BTreeMap<Vec<u8>, ObjectHandle>> {
        let mut result = BTreeMap::new();
        self.for_each_form_xobject(false, |object, _, key| {
            result.insert(key, object);
            Ok(())
        })?;
        Ok(result)
    }

    /// Return image XObjects from this target and all nested Forms.
    pub fn get_images_recursive(&mut self) -> Result<BTreeMap<Vec<u8>, ObjectHandle>> {
        let mut result = BTreeMap::new();
        self.for_each_image(true, |object, _, key| {
            result.insert(key, object);
            Ok(())
        })?;
        Ok(result)
    }

    /// Return Form XObjects from this target and all nested Forms.
    pub fn get_form_xobjects_recursive(&mut self) -> Result<BTreeMap<Vec<u8>, ObjectHandle>> {
        let mut result = BTreeMap::new();
        self.for_each_form_xobject(true, |object, _, key| {
            result.insert(key, object);
            Ok(())
        })?;
        Ok(result)
    }

    /// Return the effective `/Resources` dictionary for this page, walking up
    /// the `/Parent` chain until one is found.
    ///
    /// Returns `Ok(None)` when no node in the inheritance chain carries a
    /// `/Resources` entry.
    ///
    /// This delegates to the canonical [`Self::get_resources`] handle route.
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
        let value = self.get_resources(false)?;
        if value.is_null() {
            return Ok(None);
        }
        match value.materialize()? {
            Object::Dictionary(dictionary) => Ok(Some(dictionary)),
            other => Err(Error::Unsupported(format!(
                "/Resources on page {} has unexpected type {}",
                self.target_description(),
                object_type_name(&other)
            ))),
        }
    }

    // -----------------------------------------------------------------------
    // rotate  (GETTER — resolves inherited value, does not mutate)
    // -----------------------------------------------------------------------

    /// Return the effective `/Rotate` value for this page in degrees, resolved
    /// through the `/Parent` chain.
    ///
    /// Returns `0` (the PDF default, ISO 32000-1 §7.7.3.3 Table 30) when no
    /// node in the chain carries a `/Rotate` entry. A present value is
    /// returned as-is, including one that is not a multiple of 90, matching
    /// qpdf's raw `getAttribute("/Rotate", false)` passthrough
    /// (`QPDFPageObjectHelper.cc:670`) -- normalization to
    /// `{0, 90, 180, 270}` only happens as part of a *mutation* via
    /// [`crate::job::apply_rotate_to_pages`].
    ///
    /// This is a **getter** — it does not mutate the document. To rotate pages,
    /// use [`crate::job::apply_rotate_to_pages`].
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
        let page_ref = self.require_page_ref()?;
        resolve_inherited_rotate(self.pdf, page_ref)
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
        let page = self.resolved_page_handle()?;
        let page_ref = self.require_page_ref()?;
        let annots = self
            .pdf
            .resolve_to_terminal(&page.try_get_key(b"/Annots")?)?;
        if annots.is_null() {
            return Ok(Vec::new());
        }
        let Some(annots_array) = annots.as_array() else {
            return Err(Error::Unsupported(format!(
                "/Annots on page {page_ref} does not resolve to an array"
            )));
        };

        let mut refs = Vec::with_capacity(annots_array.len());
        for (index, elem) in annots_array.iter().enumerate() {
            let Some(object_ref) = elem.object_ref() else {
                return Err(Error::Unsupported(format!(
                    "/Annots element {index} on page {page_ref} is not an indirect reference"
                )));
            };
            refs.push(object_ref);
        }
        Ok(refs)
    }

    /// Return canonical annotation handles, optionally restricted to a
    /// `/Subtype` name, mirroring qpdf's fail-soft
    /// `QPDFPageObjectHelper::getAnnotations`
    /// (`libqpdf/QPDFPageObjectHelper.cc:439-454`). A missing, null, or
    /// non-array `/Annots` value yields an empty result; non-dictionary array
    /// members are skipped. Direct annotation dictionaries are preserved in
    /// this handle-native method even though [`Self::get_annotations`] retains
    /// its historical indirect-reference contract.
    pub fn get_annotation_handles(
        &mut self,
        only_subtype: Option<&[u8]>,
    ) -> Result<Vec<ObjectHandle>> {
        let page = self.resolved_page_handle()?;
        let annots = self
            .pdf
            .resolve_to_terminal(&page.try_get_key(b"/Annots")?)?;
        let Some(annots_array) = annots.as_array() else {
            return Ok(Vec::new());
        };
        let only_subtype = only_subtype
            .map(|value| value.strip_prefix(b"/").unwrap_or(value))
            .filter(|value| !value.is_empty());
        let mut result = Vec::with_capacity(annots_array.len());
        for item in annots_array {
            let annotation = self.pdf.resolve_to_terminal(&item)?;
            if annotation.as_dictionary().is_none() {
                continue;
            }
            if let Some(expected) = only_subtype {
                let subtype = self
                    .pdf
                    .resolve_to_terminal(&annotation.try_get_key(b"/Subtype")?)?;
                if subtype.as_name().as_deref() != Some(expected) {
                    continue;
                }
            }
            result.push(item);
        }
        Ok(result)
    }

    /// Return canonical annotation handles using qpdf's filtered, fail-soft
    /// enumeration boundary. Direct and indirect annotation dictionaries are
    /// both retained, matching the `QPDFAnnotationObjectHelper` values
    /// returned by qpdf.
    pub fn get_annotations_filtered(
        &mut self,
        only_subtype: Option<&[u8]>,
    ) -> Result<Vec<ObjectHandle>> {
        self.get_annotation_handles(only_subtype)
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
        self.pdf.resolve(&value)?;
        let Some(items) = value.as_array() else {
            return Err(Error::Unsupported(format!(
                "{} on page {} does not resolve to an array",
                String::from_utf8_lossy(key),
                self.target_description()
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
            self.pdf.resolve(&item)?;
            let Some(value) = item
                .as_integer()
                .map(|value| value as f64)
                .or_else(|| item.as_real())
            else {
                let type_name = item.type_name()?;
                return Err(Error::Unsupported(format!(
                    "{} rectangle element {index} has type {type_name} (expected number)",
                    String::from_utf8_lossy(key),
                )));
            };
            coords[index] = value;
        }
        Ok(Some(PageBox::new(
            coords[0], coords[1], coords[2], coords[3],
        )))
    }

    fn rectangle_for_matrix(&mut self, value: &ObjectHandle) -> Result<Option<PageBox>> {
        self.pdf.resolve(value)?;
        let Some(items) = value.as_array() else {
            return Ok(None);
        };
        if items.len() != 4 {
            return Ok(None);
        }
        let mut coords = [0.0f64; 4];
        for (index, item) in items.into_iter().take(4).enumerate() {
            self.pdf.resolve(&item)?;
            let Some(number) = item
                .as_integer()
                .map(|value| value as f64)
                .or_else(|| item.as_real())
            else {
                return Ok(None);
            };
            coords[index] = number;
        }
        Ok(Some(PageBox::new(
            coords[0].min(coords[2]),
            coords[1].min(coords[3]),
            coords[0].max(coords[2]),
            coords[1].max(coords[3]),
        )))
    }
}

// ---------------------------------------------------------------------------
// Private free functions
// ---------------------------------------------------------------------------

fn collect_resource_names<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    resources: &ObjectHandle,
) -> Result<std::collections::BTreeSet<Vec<u8>>> {
    let mut result = std::collections::BTreeSet::new();
    let resources = pdf.resolve_to_terminal(resources)?;
    let Some(entries) = resources.as_dictionary() else {
        return Ok(result);
    };
    for value in entries.into_values() {
        let value = pdf.resolve_to_terminal(&value)?;
        if let Some(entries) = value.as_dictionary() {
            result.extend(entries.into_keys());
        }
    }
    Ok(result)
}

fn resolve_resource_dictionary<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    resources: &ObjectHandle,
    key: &[u8],
) -> Result<Option<ObjectHandle>> {
    let value = resources.get_key(key);
    if value.is_null() {
        return Ok(None);
    }
    let value = pdf.resolve_to_terminal(&value)?;
    Ok(value.as_dictionary().map(|_| value))
}

fn externalize_inline_images_for_target<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    object: ObjectHandle,
    description: &str,
    min_size: usize,
) -> Result<()> {
    let (target, is_form) = resolve_attribute_target(pdf, object, description)?;
    let resources =
        get_attribute_for_target(pdf, target.clone(), b"/Resources", true, description)?;

    // qpdf uses mergeResources to make /XObject direct and private before the
    // filter runs. This is a no-op when /Resources is absent or malformed,
    // preserving qpdf's warning/no-resource boundary for those documents.
    let existing_xobjects = resources.get_key(b"/XObject");
    pdf.resolve(&existing_xobjects)?;
    let empty_xobjects = ObjectHandle::dictionary(Vec::new());
    let seed = ObjectHandle::dictionary(vec![(b"/XObject".to_vec(), empty_xobjects)]);
    resources.merge_resources(&seed, None)?;
    pdf.mark_object_handle_dirty(&resources)?;

    let resource_names = collect_resource_names(pdf, &resources)?;
    let color_spaces = resolve_resource_dictionary(pdf, &resources, b"/ColorSpace")?;
    let mut filter = InlineImageExternalizer::new(min_size, color_spaces, resource_names);
    let mut rewritten = Vec::new();
    {
        let mut helper = PageObjectHelper::from_object_handle(target.clone(), pdf);
        let mut sink = PlString::new("externalized inline image content", None, &mut rewritten);
        // qpdf catches filter/parser failures here, warns, and leaves the
        // original content untouched. The Rust Result boundary is retained
        // for setup/mutation errors; an unsuccessful filter is the same
        // warning-only no-op rather than a partially rewritten page.
        let filter_result = helper.filter_contents(&mut filter, Some(&mut sink));
        if let Err(error) = filter_result {
            // cov:ignore-start: the canonical decoder either fails before any
            // inline-image header is committed or completes the filter; it
            // cannot leave unresolved colorspaces in this error branch.
            for name in filter.unresolved_color_spaces.drain(..) {
                resources.warn_if_possible(&format!(
                    "unable to resolve colorspace /{}",
                    String::from_utf8_lossy(&name)
                ))?;
            }
            // cov:ignore-end
            target.warn_if_possible(&format!(
                "Unable to filter content stream: {error}; not attempting to externalize inline images from this stream"
            ))?;
            return Ok(());
        }
    }
    for name in filter.unresolved_color_spaces.drain(..) {
        resources.warn_if_possible(&format!(
            "unable to resolve colorspace /{}",
            String::from_utf8_lossy(&name)
        ))?;
    }
    if filter.images.is_empty() {
        return Ok(());
    }

    let xobjects = resources.get_key(b"/XObject");
    pdf.resolve(&xobjects)?;
    if xobjects.as_dictionary().is_some() {
        for image in filter.images {
            let stream = pdf.new_stream_with_data(Rc::new(image.data))?;
            // cov:ignore-start: Pdf::new_stream_with_data always returns a
            // document-owned stream with a stream dictionary.
            let stream_dict = stream.as_stream_dict().ok_or_else(|| {
                Error::Internal("new inline image stream has no dictionary".to_owned())
            })?;
            // cov:ignore-end
            if let Some(entries) = image.dictionary.as_dictionary() {
                for (key, value) in entries {
                    stream_dict.replace_key(&key, value)?;
                }
            }
            pdf.mark_object_handle_dirty(&stream)?;
            xobjects.replace_key(&image.name, stream)?;
        }
        pdf.mark_object_handle_dirty(&xobjects)?;
    }

    if is_form {
        target.replace_stream_data(
            Rc::new(rewritten),
            Some(ObjectHandle::null()),
            Some(ObjectHandle::null()),
        );
    } else {
        let contents = pdf.new_stream_with_data(Rc::new(rewritten))?;
        target.replace_key(b"/Contents", contents)?;
    }
    pdf.mark_object_handle_dirty(&target)
}

fn object_handle_description(object: &ObjectHandle) -> String {
    object
        .object_ref()
        .map(|object_ref| object_ref.to_string())
        .unwrap_or_else(|| "direct object".to_owned())
}

pub(crate) fn rectangle_from_handle<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    handle: &ObjectHandle,
) -> Result<Option<Rectangle>> {
    pdf.resolve(handle)?;
    let Some(items) = handle.as_array() else {
        return Ok(None);
    };
    if items.len() != 4 {
        return Ok(None);
    }
    let mut values = [0.0f64; 4];
    for (index, item) in items.into_iter().enumerate() {
        pdf.resolve(&item)?;
        let Some(value) = item
            .as_integer()
            .map(|value| value as f64)
            .or_else(|| item.as_real())
        else {
            return Ok(None);
        };
        values[index] = value;
    }
    Ok(Some(Rectangle::new(
        values[0].min(values[2]),
        values[1].min(values[3]),
        values[0].max(values[2]),
        values[1].max(values[3]),
    )))
}

fn flatten_rotation_matrix(rotate: i64, media: Rectangle) -> Matrix {
    let mut matrix = Matrix::new(0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    match rotate {
        90 => {
            matrix.b = -1.0;
            matrix.c = 1.0;
            matrix.f = media.urx + media.llx;
        }
        180 => {
            matrix.a = -1.0;
            matrix.d = -1.0;
            matrix.e = media.urx + media.llx;
            matrix.f = media.ury + media.lly;
        }
        270 => {
            matrix.b = 1.0;
            matrix.c = -1.0;
            matrix.e = media.ury + media.lly;
        }
        _ => {}
    }
    matrix
}

fn flatten_rotation_box(rotate: i64, media: Rectangle, rectangle: Rectangle) -> Rectangle {
    let left_x = rectangle.llx - media.llx;
    let right_x = media.urx - rectangle.urx;
    let bottom_y = rectangle.lly - media.lly;
    let top_y = media.ury - rectangle.ury;
    match rotate {
        90 => Rectangle::new(
            media.lly + bottom_y,
            media.llx + right_x,
            media.ury - top_y,
            media.urx - left_x,
        ),
        180 => Rectangle::new(
            media.llx + right_x,
            media.lly + top_y,
            media.urx - left_x,
            media.ury - bottom_y,
        ),
        270 => Rectangle::new(
            media.lly + top_y,
            media.llx + left_x,
            media.ury - bottom_y,
            media.urx - right_x,
        ),
        _ => rectangle,
    }
}

fn rectangle_to_handle(rectangle: Rectangle) -> ObjectHandle {
    ObjectHandle::array(vec![
        ObjectHandle::real(rectangle.llx),
        ObjectHandle::real(rectangle.lly),
        ObjectHandle::real(rectangle.urx),
        ObjectHandle::real(rectangle.ury),
    ])
}

fn append_annotation_handles<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page: &ObjectHandle,
    annotations: Vec<ObjectHandle>,
) -> Result<()> {
    let existing = pdf.resolve_to_terminal(&page.try_get_key(b"/Annots")?)?;
    let annots = if existing.as_array().is_some() {
        existing
    } else {
        let replacement = ObjectHandle::array(Vec::new());
        page.replace_key(b"/Annots", replacement.clone())?;
        replacement
    };
    for annotation in annotations {
        annots.append_array_item(annotation)?;
    }
    pdf.mark_object_handle_dirty(&annots)?;
    pdf.mark_object_handle_dirty(page)?;
    Ok(())
}

fn validate_same_document_page_handle<R: Read + Seek>(
    pdf: &Pdf<R>,
    page: &ObjectHandle,
) -> Result<()> {
    if page.object_ref().is_none() {
        return Err(Error::Unsupported(
            "copyAnnotations: source page is a direct object".to_owned(),
        ));
    }
    if page.owning_pdf_unique_id() != Some(pdf.unique_id()) {
        return Err(Error::Unsupported(
            "copyAnnotations: source page belongs to another Pdf".to_owned(),
        ));
    }
    Ok(())
}

fn validate_foreign_page_handle<RS: Read + Seek, RD: Read + Seek>(
    source: &Pdf<RS>,
    destination: &Pdf<RD>,
    page: &ObjectHandle,
) -> Result<()> {
    if page.object_ref().is_none() {
        return Err(Error::Unsupported(
            "copyAnnotations: source page is a direct object".to_owned(),
        ));
    }
    let Some(source_id) = page.owning_pdf_unique_id() else {
        return Err(Error::Unsupported(
            "copyAnnotations: source page has no owning Pdf".to_owned(),
        ));
    };
    if source_id != source.unique_id() {
        return Err(Error::Unsupported(
            "copyAnnotations: source page belongs to a different Pdf".to_owned(),
        ));
    }
    if source.unique_id() == destination.unique_id() {
        return Err(Error::Unsupported(
            "copyAnnotations: foreign source is the destination Pdf".to_owned(),
        ));
    }
    Ok(())
}

fn matrix_from_handle<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    handle: &ObjectHandle,
) -> Result<Option<Matrix>> {
    pdf.resolve(handle)?;
    let Some(items) = handle.as_array() else {
        return Ok(None);
    };
    if items.len() != 6 {
        return Ok(None);
    }
    let mut values = [0.0f64; 6];
    for (index, item) in items.into_iter().enumerate() {
        pdf.resolve(&item)?;
        let Some(value) = item
            .as_integer()
            .map(|value| value as f64)
            .or_else(|| item.as_real())
        else {
            return Ok(None);
        };
        values[index] = value;
    }
    Ok(Some(Matrix::from(values)))
}

fn resolve_attribute_target<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    object: ObjectHandle,
    description: &str,
) -> Result<(ObjectHandle, bool)> {
    pdf.resolve(&object)?;
    if object.is_form_xobject()? {
        return Ok((object, true));
    }
    if object.as_dictionary().is_none() {
        return Err(Error::Unsupported(format!(
            "object {description} is not a page dictionary or Form XObject"
        )));
    }

    let page_type = object.try_get_key(b"/Type")?;
    pdf.resolve(&page_type)?;
    match page_type.as_name() {
        Some(name) if name.as_slice() == b"Page" => Ok((object, false)),
        Some(name) => Err(Error::Unsupported(format!(
            "object {description} has /Type /{}, expected /Page",
            String::from_utf8_lossy(&name)
        ))),
        None if object.has_key(b"/Type") => Err(Error::Unsupported(format!(
            "object {description} has a non-name /Type entry"
        ))),
        None => Err(Error::Unsupported(format!(
            "object {description} has no /Type entry"
        ))),
    }
}

fn get_attribute_for_target<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    object: ObjectHandle,
    key: &[u8],
    copy_if_shared: bool,
    description: &str,
) -> Result<ObjectHandle> {
    let (object, is_form) = resolve_attribute_target(pdf, object, description)?;
    let dict = if is_form {
        // cov:ignore-start: resolve_attribute_target classifies a Form only
        // after is_form_xobject confirms that its stream dictionary exists.
        object.as_stream_dict().ok_or_else(|| {
            Error::Unsupported(format!("object {description} is not a Form stream"))
        })?
        // cov:ignore-end
    } else {
        object.clone()
    };
    let inheritable = !is_form && is_inheritable_page_attribute(key);
    let mut result = pdf.resolve_to_terminal(&dict.try_get_key(key)?)?;
    let mut inherited = false;

    if result.is_null() && inheritable {
        // qpdf's own loop (`QPDFPageObjectHelper.cc:236-247`) checks the
        // leaf's key once before the loop, then its `while (seen.add(node)
        // && node.hasKey("/Parent")) { node = node.getKey("/Parent"); result
        // = node.getKey(name); ... }` body only ever advances to and
        // examines an ancestor -- the leaf itself is never re-entered. The
        // leaf's own key was already checked above (found null), so advance
        // to the first parent before invoking the shared walk to mirror
        // that same shape: otherwise the shared walk's depth count would
        // charge one slot to re-examining the already-checked leaf, one
        // level short of qpdf's structure (qpdf itself has no numeric depth
        // cap -- DEFAULT_MAX_PAGE_TREE_DEPTH is flpdf's own DoS bound layered
        // on top of qpdf's cycle-only guard).
        let parent_ref = dict.try_get_key(b"/Parent")?;
        if let Some(cursor) = next_page_parent(parent_ref)? {
            if let Some(value) = resolve_inherited_handle_from_node_with_max_depth(
                pdf,
                cursor.handle(),
                key,
                DEFAULT_MAX_PAGE_TREE_DEPTH,
            )? {
                result = pdf.resolve_to_terminal(&value)?;
                inherited = true;
            }
        }
    }

    if copy_if_shared && (inherited || result.is_indirect()) {
        let copy = result.shallow_copy()?;
        dict.replace_key(key, copy.clone())?;
        pdf.mark_object_handle_dirty(&dict)?;
        result = copy;
    }
    Ok(result)
}

/// Return the effective `/Rotate` value for a page, keeping the inherited
/// lookup beside the other page-local attribute accessors.
pub(crate) fn resolve_inherited_rotate<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<i32> {
    resolve_inherited_rotate_with_max_depth(pdf, page_ref, DEFAULT_MAX_PAGE_TREE_DEPTH)
}

/// Test-supporting form of [`resolve_inherited_rotate`] with an explicit
/// page-tree depth bound.
pub(crate) fn resolve_inherited_rotate_with_max_depth<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    max_depth: usize,
) -> Result<i32> {
    let mut current = pdf.get_object_handle(page_ref);
    let mut depth: usize = 0;
    #[allow(
        clippy::mutable_key_type,
        reason = "qpdf identity keys intentionally retain the live handle allocation"
    )]
    let mut seen = HashSet::new();

    loop {
        if depth >= max_depth {
            return Err(Error::Unsupported(format!(
                "page tree depth exceeds maximum of {max_depth} at {}",
                current_description(&current)
            )));
        }
        if !seen.insert(current.identity_key()) {
            return Ok(0);
        }

        let rotate = current.try_get_key(b"/Rotate")?;
        if rotate.try_as_integer()?.is_some() {
            return rotate.try_get_int_value_as_int();
        }
        if !rotate.is_null() {
            return Err(Error::Unsupported(format!(
                "/Rotate entry on node {} has unexpected type",
                current_description(&current)
            )));
        }

        let parent = current.try_get_key(b"/Parent")?;
        parent.try_dereference()?;
        if parent.as_dictionary().is_none() {
            return Ok(0);
        }
        current = parent;
        depth += 1;
    }
}

fn current_description(current: &ObjectHandle) -> String {
    current
        .object_ref()
        .map(|reference| reference.to_string())
        .unwrap_or_else(|| "direct page-tree dictionary".to_owned())
}

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
    use std::collections::BTreeSet;
    use std::io::Cursor;

    use super::*;

    /// Build a minimal valid PDF from a contiguous run of `1..=objects.len()`
    /// objects, in `(object_number, body_literal)` order. `catalog_ref` is
    /// the object number of the `/Catalog` object.
    fn pdf_from_objects(catalog_ref: u32, objects: &[(u32, String)]) -> Vec<u8> {
        let mut data: Vec<u8> = b"%PDF-1.4\n".to_vec();
        let mut offsets: Vec<u64> = Vec::with_capacity(objects.len());
        for (num, body) in objects {
            offsets.push(data.len() as u64);
            data.extend_from_slice(format!("{num} 0 obj\n{body}\nendobj\n").as_bytes());
        }
        let xref_start = data.len() as u64;
        let total = objects.len() + 1;
        let mut xref = format!("xref\n0 {total}\n0000000000 65535 f \n");
        for off in &offsets {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        data.extend_from_slice(xref.as_bytes());
        let trailer = format!(
            "trailer\n<< /Size {total} /Root {catalog_ref} 0 R >>\nstartxref\n{xref_start}\n%%EOF\n"
        );
        data.extend_from_slice(trailer.as_bytes());
        data
    }

    /// `get_attribute` (via `get_media_box`) must reach exactly qpdf's
    /// depth: `QPDFPageObjectHelper::getAttribute` (`libqpdf/QPDFPageObjectHelper.cc:236-247`)
    /// checks the leaf's own key once, then its loop only ever advances to
    /// and examines an ancestor. A value set on the 100th ancestor -- the
    /// deepest level `DEFAULT_MAX_PAGE_TREE_DEPTH` still permits -- must be
    /// found, not rejected one level short by charging the leaf a depth slot.
    #[test]
    fn get_media_box_reaches_the_100th_ancestor() {
        // Objects 2..=101 are 100 nested /Pages nodes (2 = outermost, the
        // 100th ancestor of the leaf; 101 = the leaf's immediate parent).
        // /MediaBox is set only on object 2.
        let mut objects: Vec<(u32, String)> =
            vec![(1, "<< /Type /Catalog /Pages 2 0 R >>".to_string())];
        for depth in 0..100u32 {
            let num = 2 + depth;
            let kid = num + 1;
            let parent_entry = if depth == 0 {
                String::new()
            } else {
                format!(" /Parent {} 0 R", num - 1)
            };
            let media_box_entry = if depth == 0 {
                " /MediaBox [0 0 612 792]"
            } else {
                ""
            };
            objects.push((
                num,
                format!(
                    "<< /Type /Pages /Kids [{kid} 0 R] /Count 1{parent_entry}{media_box_entry} >>"
                ),
            ));
        }
        let leaf_ref = 2 + 100;
        objects.push((
            leaf_ref,
            format!("<< /Type /Page /Parent {} 0 R >>", leaf_ref - 1),
        ));

        let bytes = pdf_from_objects(1, &objects);
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        let mut helper = PageObjectHelper::new(ObjectRef::new(leaf_ref, 0), &mut pdf);
        let media_box = helper
            .get_media_box(false)
            .expect("the 100th ancestor's /MediaBox must be reachable");
        assert!(!media_box.try_is_null().expect("resolved handle"));
    }

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

    #[test]
    fn resource_lookup_helpers_cover_missing_and_non_dictionary_values() {
        let mut pdf = Pdf::<Cursor<Vec<u8>>>::empty().expect("empty PDF should be available");

        assert!(collect_resource_names(&mut pdf, &ObjectHandle::integer(1))
            .expect("non-dictionary resources have no names")
            .is_empty());

        let missing = ObjectHandle::dictionary(Vec::new());
        assert!(
            resolve_resource_dictionary(&mut pdf, &missing, b"/ColorSpace")
                .expect("missing resource category is allowed")
                .is_none()
        );

        let non_dictionary =
            ObjectHandle::dictionary(vec![(b"/ColorSpace".to_vec(), ObjectHandle::integer(1))]);
        assert!(
            resolve_resource_dictionary(&mut pdf, &non_dictionary, b"/ColorSpace")
                .expect("non-dictionary resource category is ignored")
                .is_none()
        );

        let dictionary = ObjectHandle::dictionary(vec![(
            b"/ColorSpace".to_vec(),
            ObjectHandle::dictionary(vec![(
                b"/Spot".to_vec(),
                ObjectHandle::name(b"Separation".to_vec()),
            )]),
        )]);
        assert!(
            resolve_resource_dictionary(&mut pdf, &dictionary, b"/ColorSpace")
                .expect("dictionary resource category should resolve")
                .is_some()
        );
    }

    #[test]
    fn inline_image_dictionary_expands_qpdf_abbreviations() {
        let mut externalizer = InlineImageExternalizer::new(
            0,
            Some(ObjectHandle::dictionary(vec![(
                b"/Spot".to_vec(),
                ObjectHandle::name(b"Separation".to_vec()),
            )])),
            BTreeSet::from([b"/IIm1".to_vec()]),
        );

        let image = externalizer
            .convert_inline_image_dictionary(
                b"<< /BPC 8 /CS /RGB /D [0 1] /DP << >> /F /AHx /H 2 /IM true /I false /W 1 /Other 5 >>",
                3,
            )
            .expect("valid inline-image dictionaries should convert");
        assert_eq!(image.get_key(b"/Type").as_name(), Some(b"XObject".to_vec()));
        assert_eq!(
            image.get_key(b"/Subtype").as_name(),
            Some(b"Image".to_vec())
        );
        assert_eq!(image.get_key(b"/BitsPerComponent").as_integer(), Some(8));
        assert_eq!(
            image.get_key(b"/ColorSpace").as_name(),
            Some(b"DeviceRGB".to_vec())
        );
        assert_eq!(
            image.get_key(b"/Filter").as_name(),
            Some(b"ASCIIHexDecode".to_vec())
        );
        assert_eq!(image.get_key(b"/Height").as_integer(), Some(2));
        assert_eq!(image.get_key(b"/Width").as_integer(), Some(1));
        assert_eq!(image.get_key(b"/Length").as_integer(), Some(3));
        assert_eq!(image.get_key(b"/Other").as_integer(), Some(5));

        let error = externalizer
            .convert_inline_image_dictionary(b"[]", 0)
            .expect_err("an inline-image header must be a dictionary");
        assert!(error.to_string().contains("did not parse as a dictionary"));
    }

    #[test]
    fn inline_image_externalizer_covers_colorspace_filters_and_name_conflicts() {
        let mut externalizer = InlineImageExternalizer::new(
            0,
            Some(ObjectHandle::dictionary(vec![(
                b"/Custom".to_vec(),
                ObjectHandle::name(b"Resolved".to_vec()),
            )])),
            BTreeSet::from([b"/IIm1".to_vec()]),
        );

        for (short, expanded) in [
            (b"G".as_slice(), b"DeviceGray".as_slice()),
            (b"RGB".as_slice(), b"DeviceRGB".as_slice()),
            (b"CMYK".as_slice(), b"DeviceCMYK".as_slice()),
            (b"I".as_slice(), b"Indexed".as_slice()),
        ] {
            assert_eq!(
                externalizer
                    .convert_color_space(ObjectHandle::name(short.to_vec()))
                    .as_name(),
                Some(expanded.to_vec())
            );
        }
        assert_eq!(
            externalizer
                .convert_color_space(ObjectHandle::name(b"Custom".to_vec()))
                .as_name(),
            Some(b"Resolved".to_vec())
        );
        assert_eq!(
            externalizer
                .convert_color_space(ObjectHandle::name(b"Missing".to_vec()))
                .as_name(),
            Some(b"Missing".to_vec())
        );
        assert_eq!(
            externalizer
                .convert_color_space(ObjectHandle::integer(7))
                .as_integer(),
            Some(7)
        );
        assert_eq!(
            externalizer.unresolved_color_spaces,
            vec![b"Missing".to_vec()]
        );

        for (short, expanded) in [
            (b"AHx".as_slice(), b"ASCIIHexDecode".as_slice()),
            (b"A85".as_slice(), b"ASCII85Decode".as_slice()),
            (b"LZW".as_slice(), b"LZWDecode".as_slice()),
            (b"Fl".as_slice(), b"FlateDecode".as_slice()),
            (b"RL".as_slice(), b"RunLengthDecode".as_slice()),
            (b"CCF".as_slice(), b"CCITTFaxDecode".as_slice()),
            (b"DCT".as_slice(), b"DCTDecode".as_slice()),
        ] {
            assert_eq!(
                externalizer
                    .convert_filters(ObjectHandle::name(short.to_vec()))
                    .as_name(),
                Some(expanded.to_vec())
            );
        }
        let array = externalizer.convert_filters(ObjectHandle::array(vec![
            ObjectHandle::name(b"Fl".to_vec()),
            ObjectHandle::integer(9),
            ObjectHandle::name(b"Unknown".to_vec()),
        ]));
        let items = array.as_array().expect("filter arrays remain arrays");
        assert_eq!(items[0].as_name(), Some(b"FlateDecode".to_vec()));
        assert_eq!(items[1].as_integer(), Some(9));
        assert_eq!(items[2].as_name(), Some(b"Unknown".to_vec()));
        assert_eq!(
            externalizer
                .convert_filters(ObjectHandle::integer(4))
                .as_integer(),
            Some(4)
        );
        assert_eq!(
            externalizer
                .convert_filter_name(ObjectHandle::integer(4))
                .as_integer(),
            Some(4)
        );

        assert_eq!(externalizer.next_name(), b"/IIm2".to_vec());
        assert_eq!(externalizer.next_name(), b"/IIm3".to_vec());
    }

    #[test]
    fn flatten_rotation_geometry_covers_all_quarter_turns_and_identity() {
        let media = Rectangle::new(10.0, 20.0, 210.0, 320.0);
        let rectangle = Rectangle::new(30.0, 50.0, 70.0, 100.0);

        assert_eq!(
            flatten_rotation_matrix(90, media),
            Matrix::new(0.0, -1.0, 1.0, 0.0, 0.0, 220.0)
        );
        assert_eq!(
            flatten_rotation_matrix(180, media),
            Matrix::new(-1.0, 0.0, 0.0, -1.0, 220.0, 340.0)
        );
        assert_eq!(
            flatten_rotation_matrix(270, media),
            Matrix::new(0.0, 1.0, -1.0, 0.0, 340.0, 0.0)
        );
        assert_eq!(
            flatten_rotation_matrix(0, media),
            Matrix::new(0.0, 0.0, 0.0, 0.0, 0.0, 0.0)
        );

        assert_eq!(
            flatten_rotation_box(90, media, rectangle),
            Rectangle::new(50.0, 150.0, 100.0, 190.0)
        );
        assert_eq!(
            flatten_rotation_box(180, media, rectangle),
            Rectangle::new(150.0, 240.0, 190.0, 290.0)
        );
        assert_eq!(
            flatten_rotation_box(270, media, rectangle),
            Rectangle::new(240.0, 30.0, 290.0, 70.0)
        );
        assert_eq!(flatten_rotation_box(0, media, rectangle), rectangle);
    }

    #[test]
    fn page_handle_validators_reject_direct_foreign_unowned_and_same_document_handles() {
        let mut pdf = Pdf::<Cursor<Vec<u8>>>::empty().expect("empty PDF should be available");
        let mut other = Pdf::<Cursor<Vec<u8>>>::empty().expect("empty PDF should be available");
        let direct = ObjectHandle::dictionary(Vec::new());

        assert!(matches!(
            validate_same_document_page_handle(&pdf, &direct),
            Err(Error::Unsupported(message)) if message.contains("direct object")
        ));
        assert!(matches!(
            validate_foreign_page_handle(&pdf, &other, &direct),
            Err(Error::Unsupported(message)) if message.contains("direct object")
        ));

        let unowned = ObjectHandle::new_indirect_unresolved(ObjectRef::new(99, 0), 0);
        assert!(matches!(
            validate_foreign_page_handle(&pdf, &other, &unowned),
            Err(Error::Unsupported(message)) if message.contains("no owning Pdf")
        ));

        let other_page = other.get_object_handle(ObjectRef::new(3, 0));
        assert!(matches!(
            validate_same_document_page_handle(&pdf, &other_page),
            Err(Error::Unsupported(message)) if message.contains("another Pdf")
        ));
        assert!(matches!(
            validate_foreign_page_handle(&pdf, &other, &other_page),
            Err(Error::Unsupported(message)) if message.contains("different Pdf")
        ));

        let source_page = pdf.get_object_handle(ObjectRef::new(3, 0));
        assert!(validate_foreign_page_handle(&pdf, &other, &source_page).is_ok());
        assert!(matches!(
            validate_foreign_page_handle(&pdf, &pdf, &source_page),
            Err(Error::Unsupported(message)) if message.contains("destination Pdf")
        ));
    }
}
