//! Provide live typed access to one PDF page.
//!
//! qpdf correspondence: QPDFPageObjectHelper.cc responsibilities shared with page form, resource, flatten, and overlay modules.
//!
//! [`PageObjectHelper`] wraps a qpdf `QPDFObjectHandle`-shaped target together
//! with its owning `&mut Pdf<R>` and exposes qpdf's page/Form operations. All
//! operations are delegated to the underlying infrastructure — no
//! page-dictionary state is copied or cached inside this struct.
//!
//! # Design
//!
//! The helper is intentionally thin. It re-reads the live document on every
//! call so that mutations applied through other helpers remain visible
//! immediately.
//!
//! - [`get_attribute`](PageObjectHelper::get_attribute) — reads the qpdf
//!   page/Form attribute and inheritance route, including `/Rotate`.
//! - [`get_object_handle`](PageObjectHelper::get_object_handle) — returns the
//!   exact handle retained by the helper, matching qpdf's base accessor.
//! - [`get_annotations`](PageObjectHelper::get_annotations) — returns qpdf-shaped
//!   annotation helpers from the page's fail-soft `/Annots` enumeration.
//! - [`get_media_box`](PageObjectHelper::get_media_box) and
//!   [`get_crop_box`](PageObjectHelper::get_crop_box) — return qpdf-shaped raw
//!   handles, including inheritance and fallback behavior.
//! - [`get_bleed_box`](PageObjectHelper::get_bleed_box),
//!   [`get_trim_box`](PageObjectHelper::get_trim_box), and
//!   [`get_art_box`](PageObjectHelper::get_art_box) — return qpdf-shaped raw
//!   handles and fallback values without projecting them into another type.
//!
//! - [`for_each_xobject_with_selector`](PageObjectHelper::for_each_xobject_with_selector)
//!   — filters callback invocations while preserving qpdf's recursive Form
//!   traversal and resource-scope callbacks.
//!
//! # Examples
//!
//! ## Read the qpdf-shaped media box handle
//!
//! ```no_run
//! use std::fs::File;
//! use std::io::BufReader;
//! use flpdf::{PageDocumentHelper, Pdf, PageObjectHelper};
//!
//! let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
//! let pages = PageDocumentHelper::new(&mut pdf).get_all_pages()?;
//! if let Some(page) = pages.into_iter().next() {
//!     let mut helper = PageObjectHelper::from_object_handle(page, &mut pdf);
//!     let media_box = helper.get_media_box()?;
//!     let rectangle = media_box.try_get_array_as_rectangle()?;
//!     println!("MediaBox: {:?}", rectangle);
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! ## Read the effective rotation attribute
//!
//! ```no_run
//! use std::fs::File;
//! use std::io::BufReader;
//! use flpdf::{PageDocumentHelper, Pdf, PageObjectHelper};
//!
//! let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
//! let pages = PageDocumentHelper::new(&mut pdf).get_all_pages()?;
//! if let Some(page) = pages.into_iter().next() {
//!     let mut helper = PageObjectHelper::from_object_handle(page, &mut pdf);
//!     let rotate = helper.get_attribute(b"/Rotate", false)?;
//!     if !rotate.try_is_null()? {
//!         println!("page rotation: {}°", rotate.try_get_int_value_as_int()?);
//!     }
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! ## List annotations
//!
//! ```no_run
//! use std::fs::File;
//! use std::io::BufReader;
//! use flpdf::{PageDocumentHelper, Pdf, PageObjectHelper};
//!
//! let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
//! let pages = PageDocumentHelper::new(&mut pdf).get_all_pages()?;
//! if let Some(page) = pages.into_iter().next() {
//!     let mut helper = PageObjectHelper::from_object_handle(page, &mut pdf);
//!     let annots = helper.get_annotations()?;
//!     println!("{} annotations on page 1", annots.len());
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use crate::annotation_object_helper::AnnotationObjectHelper;
use crate::content_stream::ObjectHandleParserCallbacks;
use crate::object_handle::{ObjectHandle, ObjectHandleIdentity};
use crate::pages::is_inheritable_page_attribute;
use crate::pipeline::{Pipeline, PipelineError, PlString};
use crate::token_filter::TokenFilter;
use crate::tokenizer::{Token, TokenType};
use crate::writer::DecodeLevel;
use crate::{Error, Matrix, ObjectRef, Pdf, Rectangle, Result};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::io::{Read, Seek};
use std::rc::Rc;

// ---------------------------------------------------------------------------
// PageObjectHelper
// ---------------------------------------------------------------------------

/// Per-page typed accessor helper.
///
/// Construct with [`PageObjectHelper::from_object_handle`], then use the provided methods to
/// inspect the page's attributes, content streams, resources, annotations, and
/// bounding boxes. All operations are delegated to the underlying `Pdf<R>`
/// infrastructure; no state is cached inside this struct.
///
/// qpdf exposes one annotation enumeration operation with an optional subtype
/// filter and typed annotation-helper results through `get_annotations`.
/// There is no public raw-handle collector:
///
/// ```compile_fail,E0599
/// use flpdf::PageObjectHelper;
/// use std::io::Cursor;
///
/// let _method = PageObjectHelper::<Cursor<Vec<u8>>>::get_annotations_filtered;
/// ```
///
/// ```compile_fail,E0624
/// use flpdf::PageObjectHelper;
/// use std::io::Cursor;
///
/// let _method = PageObjectHelper::<Cursor<Vec<u8>>>::get_annotation_handles;
/// ```
pub struct PageObjectHelper<'a, R: Read + Seek + 'static> {
    object: ObjectHandle,
    page_ref: Option<ObjectRef>,
    pdf: &'a mut Pdf<R>,
}

struct InlineImageExternalizer<'a, R: Read + Seek + 'static> {
    pdf: &'a mut Pdf<R>,
    min_size: usize,
    resources: ObjectHandle,
    min_suffix: usize,
    bi_bytes: Vec<u8>,
    dict_bytes: Vec<u8>,
    in_inline_image: bool,
    any_images: bool,
}

impl<'a, R: Read + Seek + 'static> InlineImageExternalizer<'a, R> {
    fn new(min_size: usize, resources: ObjectHandle, pdf: &'a mut Pdf<R>) -> Self {
        Self {
            pdf,
            min_size,
            resources,
            min_suffix: 1,
            bi_bytes: Vec::new(),
            dict_bytes: Vec::new(),
            in_inline_image: false,
            any_images: false,
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
                self.convert_color_space(value)?
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

    fn convert_color_space(
        &mut self,
        value: ObjectHandle,
    ) -> std::result::Result<ObjectHandle, PipelineError> {
        let Some(name) = value.as_name() else {
            return Ok(value);
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
            return Ok(ObjectHandle::name(name.to_vec()));
        }
        let color_spaces = resolve_resource_dictionary(&self.resources, b"/ColorSpace")
            .map_err(|error| PipelineError::runtime(error.to_string()))?;
        if let Some(color_spaces) = color_spaces {
            let mut key = b"/".to_vec();
            key.extend_from_slice(name);
            if color_spaces
                .try_has_key(&key)
                .map_err(|error| PipelineError::runtime(error.to_string()))?
            {
                return color_spaces
                    .try_get_key(&key)
                    .map_err(|error| PipelineError::runtime(error.to_string()));
            }
        }
        self.resources
            .warn_if_possible(&format!(
                "unable to resolve colorspace /{}",
                String::from_utf8_lossy(name)
            ))
            .map_err(|error| PipelineError::runtime(error.to_string()))?;
        Ok(value)
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

    fn next_name(&mut self) -> std::result::Result<Vec<u8>, PipelineError> {
        self.resources
            .get_unique_resource_name(b"/IIm", &mut self.min_suffix)
            .map_err(|error| PipelineError::runtime(error.to_string()))
    }
}

impl<R: Read + Seek + 'static> TokenFilter for InlineImageExternalizer<'_, R> {
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
                    let name = self.next_name()?;

                    let stream = self
                        .pdf
                        .new_stream_with_data(Rc::new(token.value.clone()))
                        .map_err(|error| PipelineError::runtime(error.to_string()))?;
                    // cov:ignore-start: Pdf::new_stream_with_data always returns a stream dictionary.
                    let stream_dict = stream.as_stream_dict().ok_or_else(|| {
                        PipelineError::runtime("new inline image stream has no dictionary")
                    })?;
                    // cov:ignore-end
                    // cov:ignore-start: convert_inline_image_dictionary always returns a dictionary.
                    if let Some(entries) = dictionary.as_dictionary() {
                        for (key, value) in entries {
                            stream_dict
                                .replace_key(&key, value)
                                .map_err(|error| PipelineError::runtime(error.to_string()))?;
                        }
                    }
                    // cov:ignore-end
                    self.resources
                        .try_get_key(b"/XObject")
                        .map_err(|error| PipelineError::runtime(error.to_string()))?
                        .replace_key(&name, stream)
                        .map_err(|error| PipelineError::runtime(error.to_string()))?;
                    self.any_images = true;
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

impl<'a, R: Read + Seek> PageObjectHelper<'a, R> {
    /// Create a helper over an object handle.
    ///
    /// For attribute access, Form XObjects use their stream dictionary and all
    /// other objects are queried directly, matching qpdf's
    /// `QPDFPageObjectHelper(QPDFObjectHandle)` and `getAttribute`. Other
    /// page-specific operations apply their own target requirements.
    pub fn from_object_handle(object: ObjectHandle, pdf: &'a mut Pdf<R>) -> Self {
        let page_ref = object.object_ref();
        Self {
            object,
            page_ref,
            pdf,
        }
    }

    /// Return the exact object handle retained by this helper, matching the
    /// inherited `QPDFObjectHelper::getObjectHandle` accessor
    /// (`include/qpdf/QPDFObjectHelper.hh:34-55`). This preserves direct
    /// objects and raw indirect identity without resolving or projecting
    /// through `ObjectRef`.
    pub fn get_object_handle(&self) -> ObjectHandle {
        self.object.clone()
    }

    fn target_description(&self) -> String {
        let object_gen = self.object.get_obj_gen();
        if object_gen.is_indirect() {
            object_gen.to_string()
        } else {
            "direct object".to_owned()
        }
    }

    fn require_page_ref(&self) -> Result<ObjectRef> {
        self.page_ref.ok_or_else(|| {
            Error::Unsupported("operation requires a page object reference".to_owned())
        })
    }

    /// Resolve the target and return whether it is a Form XObject. Page
    /// attribute and content helpers classify only Form XObjects; qpdf does
    /// not preflight the other targets as `/Type /Page` dictionaries.
    fn resolved_attribute_target(&mut self) -> Result<(ObjectHandle, bool)> {
        resolve_attribute_target(self.object.clone())
    }

    /// Return the live handle used by qpdf-delegating helper operations.
    /// `QPDFPageObjectHelper` does not preflight the target as `/Type /Page`;
    /// the delegated operation determines its own behavior for the handle.
    fn resolved_page_handle(&mut self) -> Result<ObjectHandle> {
        Ok(self.object.clone())
    }

    /// Return a live attribute, applying qpdf's page-tree inheritance rules
    /// for `/MediaBox`, `/CropBox`, `/Resources`, and `/Rotate` on non-Form
    /// targets. As in qpdf's `getAttribute`, this lookup does not require a
    /// `/Type /Page` entry.
    ///
    /// When `copy_if_shared` is true, an inherited or indirect value is
    /// shallow-copied into the page dictionary before it is returned. The
    /// returned handle is therefore the value that a caller may mutate without
    /// changing the shared source attribute, matching
    /// `QPDFPageObjectHelper::getAttribute` (`libqpdf/QPDFPageObjectHelper.cc:224-260`).
    /// Missing and null attributes return a direct null handle.
    pub fn get_attribute(&mut self, key: &[u8], copy_if_shared: bool) -> Result<ObjectHandle> {
        let description = self.target_description();
        get_attribute_for_target(self.object.clone(), key, copy_if_shared, &description)
    }

    /// Return the effective `/MediaBox` handle with qpdf's default
    /// `copy_if_shared = false` behavior.
    pub fn get_media_box(&mut self) -> Result<ObjectHandle> {
        self.get_media_box_with_options(false)
    }

    /// Return the effective `/MediaBox` handle with explicit qpdf copy options.
    ///
    /// This is the Rust spelling for qpdf's `getMediaBox(copy_if_shared)`;
    /// Rust has no default arguments, so the no-option form is
    /// [`get_media_box`](Self::get_media_box).
    pub fn get_media_box_with_options(&mut self, copy_if_shared: bool) -> Result<ObjectHandle> {
        self.get_attribute(b"/MediaBox", copy_if_shared)
    }

    /// Return the effective `/CropBox` handle, falling back to `/MediaBox`,
    /// with qpdf's default copy flags (`false`, `false`).
    pub fn get_crop_box(&mut self) -> Result<ObjectHandle> {
        self.get_crop_box_with_options(false, false)
    }

    /// Return the effective `/CropBox` handle with explicit qpdf copy options.
    ///
    /// This is the Rust spelling for qpdf's
    /// `getCropBox(copy_if_shared, copy_if_fallback)`; Rust has no default
    /// arguments, so the no-option form is [`get_crop_box`](Self::get_crop_box).
    pub fn get_crop_box_with_options(
        &mut self,
        copy_if_shared: bool,
        copy_if_fallback: bool,
    ) -> Result<ObjectHandle> {
        let result = self.get_attribute(b"/CropBox", copy_if_shared)?;
        if !result.try_is_null()? {
            return Ok(result);
        }
        let fallback = self.get_media_box_with_options(copy_if_shared)?;
        self.apply_fallback(b"/CropBox", fallback, copy_if_fallback)
    }

    /// Return the effective `/BleedBox` handle, falling back to `/CropBox`,
    /// with qpdf's default copy flags (`false`, `false`).
    pub fn get_bleed_box(&mut self) -> Result<ObjectHandle> {
        self.get_bleed_box_with_options(false, false)
    }

    /// Return the effective `/BleedBox` handle with explicit qpdf copy options.
    pub fn get_bleed_box_with_options(
        &mut self,
        copy_if_shared: bool,
        copy_if_fallback: bool,
    ) -> Result<ObjectHandle> {
        let result = self.get_attribute(b"/BleedBox", copy_if_shared)?;
        if !result.try_is_null()? {
            return Ok(result);
        }
        let fallback = self.get_crop_box_with_options(copy_if_shared, copy_if_fallback)?;
        self.apply_fallback(b"/BleedBox", fallback, copy_if_fallback)
    }

    /// Return the effective `/TrimBox` handle, falling back to `/CropBox`,
    /// with qpdf's default copy flags (`false`, `false`).
    pub fn get_trim_box(&mut self) -> Result<ObjectHandle> {
        self.get_trim_box_with_options(false, false)
    }

    /// Return the effective `/TrimBox` handle with explicit qpdf copy options.
    pub fn get_trim_box_with_options(
        &mut self,
        copy_if_shared: bool,
        copy_if_fallback: bool,
    ) -> Result<ObjectHandle> {
        let result = self.get_attribute(b"/TrimBox", copy_if_shared)?;
        if !result.try_is_null()? {
            return Ok(result);
        }
        let fallback = self.get_crop_box_with_options(copy_if_shared, copy_if_fallback)?;
        self.apply_fallback(b"/TrimBox", fallback, copy_if_fallback)
    }

    /// Return the effective `/ArtBox` handle, falling back to `/CropBox`,
    /// with qpdf's default copy flags (`false`, `false`).
    pub fn get_art_box(&mut self) -> Result<ObjectHandle> {
        self.get_art_box_with_options(false, false)
    }

    /// Return the effective `/ArtBox` handle with explicit qpdf copy options.
    pub fn get_art_box_with_options(
        &mut self,
        copy_if_shared: bool,
        copy_if_fallback: bool,
    ) -> Result<ObjectHandle> {
        let result = self.get_attribute(b"/ArtBox", copy_if_shared)?;
        if !result.try_is_null()? {
            return Ok(result);
        }
        let fallback = self.get_crop_box_with_options(copy_if_shared, copy_if_fallback)?;
        self.apply_fallback(b"/ArtBox", fallback, copy_if_fallback)
    }

    /// Convert this indirect handle into a new, document-owned Form XObject.
    ///
    /// The new stream retains a provider over the page's canonical content
    /// route, so conversion does not eagerly decode or concatenate page bytes.
    /// `/Resources`, `/Group`, and the effective `/TrimBox` are shallow-copied;
    /// `/Matrix` is emitted when requested and either `/Rotate` or `/UserUnit`
    /// is present, matching qpdf's `getFormXObjectForPage`
    /// (`libqpdf/QPDFPageObjectHelper.cc:706-734`). qpdf accepts Form handles
    /// here as well; the provider reads `/Contents` from the original handle
    /// only when the new stream is materialized. The no-option call defaults
    /// `handle_transformations` to `true`.
    pub fn get_form_xobject_for_page(&mut self) -> Result<ObjectHandle> {
        self.get_form_xobject_for_page_with_options(true)
    }

    /// Convert this indirect page or Form handle into a Form XObject with an
    /// explicit qpdf transformation option. Use
    /// [`get_form_xobject_for_page`](Self::get_form_xobject_for_page) for the
    /// default `true` behavior.
    pub fn get_form_xobject_for_page_with_options(
        &mut self,
        handle_transformations: bool,
    ) -> Result<ObjectHandle> {
        let page = self.resolved_page_handle()?;
        if !page.get_obj_gen().is_indirect() {
            return Err(Error::Unsupported(
                "QPDFPageObjectHelper::getFormXObjectForPage called with a direct object"
                    .to_owned(),
            ));
        }
        // qpdf: "contents from page object " + getObjGen().unparse(' ')
        // (`libqpdf/QPDFPageObjectHelper.cc:35`), e.g. "3 0" without " R".
        let page_description = format!(
            "contents from page object {}",
            page.get_obj_gen().unparse_with_separator(' ')
        );
        let form = self.pdf.new_stream()?;
        let dict = form
            .as_stream_dict()
            .ok_or_else(|| Error::Internal("new stream has no dictionary".to_owned()))?;
        dict.replace_key(b"/Type", ObjectHandle::name(b"XObject".to_vec()))?;
        dict.replace_key(b"/Subtype", ObjectHandle::name(b"Form".to_vec()))?;

        let resources = self.get_attribute(b"/Resources", false)?.shallow_copy()?;
        dict.replace_key(b"/Resources", resources)?;
        let group = self.get_attribute(b"/Group", false)?.shallow_copy()?;
        dict.replace_key(b"/Group", group)?;
        let bbox = self.get_trim_box()?.shallow_copy()?;
        if rectangle_from_handle(&bbox)?.is_none() {
            self.object.warn_if_possible(
                "bounding box is invalid; form XObject created from page will not work",
            )?; // cov:ignore: qpdf warning emission is infallible for a live page handle; only the defensive logger error edge is excluded
        }
        dict.replace_key(b"/BBox", bbox)?;

        // qpdf's ContentProvider retains the original object handle and looks
        // up /Contents only when stream data is requested. This also preserves
        // qpdf's stream-handle type warning and null result for Form targets.
        let provider_page = page.clone();

        // qpdf installs the lazy provider before reading the transformation
        // attributes (`QPDFPageObjectHelper.cc:716-729`). Both attributes are
        // read even when `handle_transformations` is false; only matrix
        // insertion is conditional.
        form.replace_stream_data_with_callback(
            move |pipeline| {
                let mut all_description = String::new();
                provider_page
                    .try_get_key(b"/Contents")?
                    .pipe_content_streams(pipeline, &page_description, &mut all_description)
            },
            None,
            None,
        )?; // cov:ignore: the provider closure is built from canonical page contents; this is only its defensive setup error edge

        let rotate = self.get_attribute(b"/Rotate", false)?;
        let user_unit = self.get_attribute(b"/UserUnit", false)?;
        if handle_transformations && (!rotate.try_is_null()? || !user_unit.try_is_null()?) {
            let matrix = self.get_matrix_for_transformations()?;
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

        Ok(form)
    }

    /// Return qpdf's page/Form transformation matrix using the effective
    /// `/TrimBox`, inherited `/Rotate`, and leaf `/UserUnit`, with qpdf's
    /// default `invert = false` behavior.
    pub fn get_matrix_for_transformations(&mut self) -> Result<Matrix> {
        self.get_matrix_for_transformations_with_options(false)
    }

    /// Return qpdf's transformation matrix with an explicit inversion flag.
    /// Use [`get_matrix_for_transformations`](Self::get_matrix_for_transformations)
    /// for the default `false` behavior.
    pub fn get_matrix_for_transformations_with_options(&mut self, invert: bool) -> Result<Matrix> {
        let bbox = self.get_trim_box()?;
        let Some(rect) = self.rectangle_for_matrix(&bbox)? else {
            return Ok(Matrix::default());
        };
        let rotate_obj = self.get_attribute(b"/Rotate", false)?;
        let scale_obj = self.get_attribute(b"/UserUnit", false)?;
        if rotate_obj.try_is_null()? && scale_obj.try_is_null()? {
            return Ok(Matrix::default());
        }

        let scale = scale_obj.try_as_integer()?.map(|value| value as f64);
        let mut scale = scale.or_else(|| scale_obj.as_real()).unwrap_or(1.0);
        let mut rotate = rotate_obj.try_as_integer()?.unwrap_or(0) as i32;
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
    /// (`libqpdf/QPDFPageObjectHelper.cc:764-838`). The no-option method uses
    /// qpdf's defaults `invert_transformations = true`, `allow_shrink = true`,
    /// and `allow_expand = false`.
    pub fn get_matrix_for_form_xobject_placement(
        &mut self,
        form: ObjectHandle,
        rect: Rectangle,
    ) -> Result<Option<Matrix>> {
        self.get_matrix_for_form_xobject_placement_with_options(form, rect, true, true, false)
    }

    /// Compute qpdf's placement matrix with explicit transformation and
    /// scaling flags. Use the no-option method for qpdf defaults.
    pub fn get_matrix_for_form_xobject_placement_with_options(
        &mut self,
        form: ObjectHandle,
        rect: Rectangle,
        invert_transformations: bool,
        allow_shrink: bool,
        allow_expand: bool,
    ) -> Result<Option<Matrix>> {
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
        let bbox = form_dict.try_get_key(b"/BBox")?;
        let Some(bbox) = rectangle_from_handle(&bbox)? else {
            return Ok(None);
        };
        let form_matrix = form_dict.try_get_key(b"/Matrix")?;
        let form_matrix = matrix_from_handle(&form_matrix)?.unwrap_or_default();
        let transform = if invert_transformations {
            self.get_matrix_for_transformations_with_options(true)?
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
    /// qpdf's identity-matrix fallback. The no-option method uses qpdf's
    /// defaults `invert_transformations = true`, `allow_shrink = true`, and
    /// `allow_expand = false`.
    pub fn place_form_xobject(
        &mut self,
        form: ObjectHandle,
        name: &str,
        rect: Rectangle,
    ) -> Result<(String, Matrix)> {
        self.place_form_xobject_with_options(form, name, rect, true, true, false)
    }

    /// Build qpdf's `placeFormXObject` fragment with explicit transformation
    /// and scaling flags. Use the no-option method for qpdf defaults.
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors qpdf's placeFormXObject flag parameters"
    )]
    pub fn place_form_xobject_with_options(
        &mut self,
        form: ObjectHandle,
        name: &str,
        rect: Rectangle,
        invert_transformations: bool,
        allow_shrink: bool,
        allow_expand: bool,
    ) -> Result<(String, Matrix)> {
        let matrix = self
            .get_matrix_for_form_xobject_placement_with_options(
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
    /// `QPDFMatrix&`. This no-option form uses qpdf's defaults
    /// `invert_transformations = true`, `allow_shrink = true`, and
    /// `allow_expand = false`.
    pub fn place_form_xobject_with_matrix(
        &mut self,
        form: ObjectHandle,
        name: &str,
        rect: Rectangle,
        matrix: &mut Matrix,
    ) -> Result<String> {
        self.place_form_xobject_with_matrix_with_options(
            form, name, rect, matrix, true, true, false,
        )
    }

    /// Variant of [`Self::place_form_xobject_with_matrix`] with explicit
    /// transformation and scaling flags.
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors qpdf's placeFormXObject overload and retains all flags"
    )]
    pub fn place_form_xobject_with_matrix_with_options(
        &mut self,
        form: ObjectHandle,
        name: &str,
        rect: Rectangle,
        matrix: &mut Matrix,
        invert_transformations: bool,
        allow_shrink: bool,
        allow_expand: bool,
    ) -> Result<String> {
        let (fragment, computed) = self.place_form_xobject_with_options(
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
        if !copy_if_fallback || fallback.try_is_null()? {
            return Ok(fallback);
        }
        // qpdf copies the fallback into the same dictionary `getAttribute`
        // reads: a Form's stream dictionary, otherwise the supplied handle
        // itself, with no `/Type /Page` requirement
        // (`libqpdf/QPDFPageObjectHelper.cc:224-262`).
        let is_form = self.object.is_form_xobject()?;
        let page = if is_form {
            // cov:ignore-start: is_form_xobject returns true only for a stream
            // with a canonical dictionary.
            self.object.as_stream_dict().ok_or_else(|| {
                Error::Unsupported("Form XObject has no stream dictionary".to_owned())
            })?
            // cov:ignore-end
        } else {
            self.object.clone()
        };
        let copy = fallback.shallow_copy()?;
        page.replace_key(key, copy.clone())?;
        Ok(copy)
    }

    /// Return the page's `/Contents` as canonical stream handles.
    ///
    /// This is the direct `QPDFPageObjectHelper::getPageContents` route
    /// (`libqpdf/QPDFPageObjectHelper.cc:455-459`) and deliberately preserves
    /// each stream's identity and lazy provider instead of decoding it into a
    /// byte buffer or legacy raw value. Like qpdf, it delegates directly on
    /// the stored handle without a `/Type /Page` preflight.
    pub fn get_page_contents(&mut self) -> Result<Vec<ObjectHandle>> {
        self.object.get_page_contents()
    }

    /// Add a canonical stream to the beginning or end of `/Contents`.
    ///
    /// Mirrors `QPDFPageObjectHelper::addPageContents`
    /// (`libqpdf/QPDFPageObjectHelper.cc:461-465`).
    pub fn add_page_contents(&mut self, contents: ObjectHandle, first: bool) -> Result<()> {
        self.object.add_page_contents(contents, first)?;
        Ok(())
    }

    /// Rotate the page in the live object graph.
    ///
    /// Mirrors `QPDFPageObjectHelper::rotatePage`
    /// (`libqpdf/QPDFPageObjectHelper.cc:467-471`).
    pub fn rotate_page(&mut self, angle: i32, relative: bool) -> Result<()> {
        self.object.rotate_page(angle, relative)?;
        Ok(())
    }

    /// Bake the handle's direct qpdf `/Rotate` value into its boxes, contents,
    /// and annotations.
    ///
    /// This is `QPDFPageObjectHelper::flattenRotation`
    /// (`libqpdf/QPDFPageObjectHelper.cc:862-991`). qpdf intentionally reads
    /// `/Rotate`, `/MediaBox`, and the optional page boxes directly from the
    /// page object here; inherited values are not materialized by this method.
    /// It operates on the live handle and does not require an
    /// `ObjectRef` projection. Since flpdf stores the mutable `Pdf` separately
    /// from the handle, the handle must belong to that same `Pdf`; qpdf's
    /// `QPDFObjectHelper` stores only the handle.
    /// The page-document orchestration that calls this method remains outside
    /// [`PageObjectHelper`]. Annotation field-tree work is delegated to
    /// [`crate::AcroFormDocumentHelper`]'s canonical transform route.
    pub fn flatten_rotation(&mut self) -> Result<()> {
        let Some(page_pdf_id) = self.object.owning_pdf_unique_id() else {
            return Err(Error::System(
                "QPDFPageObjectHelper::flattenRotation called with a direct object".to_owned(),
            ));
        };
        if page_pdf_id != self.pdf.unique_id() {
            return Err(Error::Unsupported(
                "flattenRotation: page belongs to another Pdf".to_owned(),
            ));
        }
        let page = self.resolved_page_handle()?;

        let rotate = page.try_get_key(b"/Rotate")?.try_as_integer()?.unwrap_or(0);
        if !matches!(rotate, 90 | 180 | 270) {
            return Ok(());
        }

        let media = page.try_get_key(b"/MediaBox")?;
        let Some(media) = rectangle_from_handle(&media)? else {
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
            let value = page.try_get_key(key)?;
            let Some(rectangle) = rectangle_from_handle(&value)? else {
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

        page.remove_key(b"/Rotate")?;
        // `getAttribute(..., false)` is qpdf's inherited lookup after the
        // direct key is removed. If an ancestor supplied rotation, materialize
        // the zero that masks it on this page.
        let inherited_rotate = self.get_attribute(b"/Rotate", false)?;
        if !inherited_rotate.try_is_null()? {
            page.replace_key(b"/Rotate", ObjectHandle::integer(0))?;
        }

        let old_annots = page.try_get_key(b"/Annots")?;
        if old_annots.try_is_array()? {
            let transformed = {
                let mut acroform = crate::AcroFormDocumentHelper::new(self.pdf)?;
                let transformed = acroform.transform_annotations(old_annots, matrix)?;
                acroform.remove_form_fields(&transformed.old_field_objgens)?;
                acroform.add_form_fields(transformed.new_fields.clone())?;
                transformed
            };
            page.replace_key(b"/Annots", ObjectHandle::array(transformed.new_annotations))?;
        }

        Ok(())
    }

    /// Copy annotations from another page in the same document using qpdf's
    /// default identity transformation matrix.
    ///
    /// qpdf 11.9.0 declares `copyAnnotations` with `cm = QPDFMatrix()`
    /// (`include/qpdf/QPDFPageObjectHelper.hh:393-397`), whose default
    /// constructor is the identity matrix (`libqpdf/QPDFMatrix.cc:6-14`).
    /// Use [`copy_annotations_with_matrix`](Self::copy_annotations_with_matrix)
    /// to provide a custom transform.
    pub fn copy_annotations(&mut self, from_page: ObjectHandle) -> Result<()> {
        self.copy_annotations_with_matrix(from_page, Matrix::default())
    }

    /// Copy annotations from another page in the same document, applying
    /// `cm` to every copied rectangle and appearance matrix.
    ///
    /// This is the same-document branch of qpdf's
    /// `QPDFPageObjectHelper::copyAnnotations`
    /// (`libqpdf/QPDFPageObjectHelper.cc:992-1039`). The canonical AcroForm
    /// helper owns field-tree copying and qualified-name renaming.
    pub fn copy_annotations_with_matrix(
        &mut self,
        from_page: ObjectHandle,
        cm: Matrix,
    ) -> Result<()> {
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
    /// scan to the completed output boundary. This helper is retained only by
    /// tests of the former fresh-target merge.
    #[cfg(test)]
    pub(crate) fn copy_annotations_with_field_tree_only(
        &mut self,
        from_page: ObjectHandle,
        cm: Matrix,
        reserved_names: &BTreeSet<Vec<u8>>,
    ) -> Result<()> {
        self.copy_annotations_with_reserved_names_impl(from_page, cm, reserved_names, true)
    }

    /// Replace copied annotations using qpdf's same-document
    /// `QPDFAcroFormDocumentHelper::fixCopiedAnnotations` boundary. The page
    /// already has its shallow-copied `/Annots`; qpdf clones the source
    /// annotation/field tree and replaces that array after each repeated page
    /// occurrence (`QPDFAcroFormDocumentHelper.cc:1017-1047`).
    pub(crate) fn fix_copied_annotations_with_field_tree_only(
        &mut self,
        from_page: ObjectHandle,
        reserved_names: &BTreeSet<Vec<u8>>,
    ) -> Result<()> {
        let destination = self.resolved_page_handle()?;
        self.require_page_ref()?;
        validate_same_document_page_handle(self.pdf, &from_page)?;
        let mut acroform = crate::AcroFormDocumentHelper::new_for_field_tree(self.pdf)?;
        acroform.fix_copied_annotations_with_reserved_names(destination, from_page, reserved_names)
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
        let old_annots = from_page.try_get_key(b"/Annots")?;
        if !old_annots.try_is_array()? {
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
        append_annotation_handles(&destination, transformed.new_annotations)?;
        Ok(())
    }

    /// Copy annotations from a page owned by `source` using qpdf's default
    /// identity transformation matrix.
    ///
    /// qpdf 11.9.0 declares `copyAnnotations` with `cm = QPDFMatrix()`
    /// (`include/qpdf/QPDFPageObjectHelper.hh:393-397`).
    ///
    /// Use [`copy_annotations_from_with_matrix`](Self::copy_annotations_from_with_matrix)
    /// to provide an explicit transform.
    pub fn copy_annotations_from<RS: Read + Seek>(
        &mut self,
        from_page: ObjectHandle,
        source: &mut Pdf<RS>,
    ) -> Result<()> {
        self.copy_annotations_from_with_matrix(from_page, Matrix::default(), source)
    }

    /// Copy annotations from a page owned by `source`, applying `cm` to every
    /// copied rectangle and appearance matrix.
    ///
    /// This is qpdf's foreign-document `copyAnnotations` branch. The source
    /// handle must be an indirect page handle owned by the supplied source
    /// document; destination field/resource reconciliation remains in the
    /// canonical [`crate::AcroFormDocumentHelper`] implementation.
    pub fn copy_annotations_from_with_matrix<RS: Read + Seek>(
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
    /// `Pdf::acroform_cache` memoizes the source's full AcroForm analysis
    /// per source `Pdf` (`QPDFJob::get_afdh_for_qpdf`, `QPDFJob.cc:1847-1856`),
    /// so a repeated `AcroFormDocumentHelper::new(source)` across per-page
    /// copies reuses that warm cache instead of rescanning; the orphan-widget
    /// page walk that self-associates a widget unreachable from
    /// `/AcroForm/Fields` therefore still runs exactly once per source.
    pub(crate) fn fix_copied_annotations_from<RS: Read + Seek>(
        &mut self,
        from_page: ObjectHandle,
        source: &mut Pdf<RS>,
    ) -> Result<()> {
        let destination = self.resolved_page_handle()?;
        self.require_page_ref()?;
        validate_foreign_page_handle(source, self.pdf, &from_page)?;
        let mut acroform = crate::AcroFormDocumentHelper::new(self.pdf)?;
        acroform.fix_copied_annotations_from_with_reserved_names(
            destination,
            from_page,
            source,
            &BTreeSet::new(),
        )
    }

    /// Foreign-page variant of qpdf's `fixCopiedAnnotations` that keeps the
    /// destination's page-orphan scan deferred until page selection finishes.
    /// The persistent foreign object copier supplies the shared source identity
    /// map; `field_tree_only` preserves qpdf's per-occurrence field-copy order
    /// without treating page annotations pending replacement as orphans.
    pub(crate) fn fix_copied_annotations_from_with_field_tree_only<RS: Read + Seek>(
        &mut self,
        from_page: ObjectHandle,
        source: &mut Pdf<RS>,
        reserved_names: &BTreeSet<Vec<u8>>,
    ) -> Result<()> {
        let destination = self.resolved_page_handle()?;
        self.require_page_ref()?;
        validate_foreign_page_handle(source, self.pdf, &from_page)?;
        {
            let mut acroform = crate::AcroFormDocumentHelper::new_for_field_tree(self.pdf)?;
            acroform.fix_copied_annotations_from_with_field_tree_only(
                destination,
                from_page,
                source,
                reserved_names,
            )?;
        }
        *self.pdf.acroform_cache.borrow_mut() = None;
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
    #[cfg(test)]
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
        let old_annots = from_page.try_get_key(b"/Annots")?;
        if !old_annots.try_is_array()? {
            return Ok(());
        }
        validate_foreign_page_handle(source, self.pdf, &from_page)?;
        if !destination.get_obj_gen().is_indirect() {
            return Err(Error::Unsupported(
                "QPDFPageObjectHelper::copyAnnotations: this page is a direct object".to_owned(),
            ));
        }

        let (transformed, invalidate_cache) = {
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
            let invalidate_cache = if field_tree_only {
                true
            } else {
                acroform
                    .copied_annotations_require_cache_invalidation(&transformed.new_annotations)?
            };
            (transformed, invalidate_cache)
        };
        append_annotation_handles(&destination, transformed.new_annotations)?;
        if invalidate_cache {
            // qpdf's QPDFAcroFormDocumentHelper contract requires
            // invalidateCache after manually changing a page's annotation
            // dictionary when an unregistered Widget may need the orphan
            // fallback (`QPDFAcroFormDocumentHelper.hh:76-83`). Field-backed
            // Widgets have already been registered by addFormField, so keep
            // that warm cache instead of rescanning every destination page.
            *self.pdf.acroform_cache.borrow_mut() = None;
        }
        Ok(())
    }

    /// Coalesce the page's content streams into one lazy provider-backed stream.
    pub fn coalesce_content_streams(&mut self) -> Result<()> {
        self.object.coalesce_content_streams()?;
        Ok(())
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

    /// Parse this helper's decoded contents into callback-visited objects.
    ///
    /// A helper built over a Form XObject parses that Form's own
    /// content stream rather than a page's.
    ///
    /// qpdf's old name for [`Self::parse_page_contents`], kept as an alias.
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

    /// Apply a lexical token filter to this helper's decoded contents.
    ///
    /// A helper built over a Form XObject filters that Form's own
    /// content stream rather than a page's.
    pub fn filter_page_contents(&mut self, filter: &mut dyn TokenFilter) -> Result<()> {
        self.filter_page_contents_with_pipeline(filter, None)
    }

    /// Apply a lexical token filter and send output to the optional pipeline.
    ///
    /// Passing `None` discards filter output, matching qpdf's default
    /// `next = nullptr` argument.
    pub fn filter_page_contents_with_pipeline<'b>(
        &mut self,
        filter: &'b mut dyn TokenFilter,
        next: Option<&'b mut dyn Pipeline>,
    ) -> Result<()> {
        self.filter_contents_with_pipeline(filter, next)
    }

    /// Apply a lexical token filter to this helper's decoded contents.
    ///
    /// A helper built over a Form XObject filters that Form's own
    /// content stream rather than a page's.
    ///
    /// qpdf's old name for [`Self::filter_page_contents`], kept as an alias.
    pub fn filter_contents(&mut self, filter: &mut dyn TokenFilter) -> Result<()> {
        self.filter_contents_with_pipeline(filter, None)
    }

    /// Apply a lexical token filter and send output to the optional pipeline.
    ///
    /// This is qpdf's `filterContents(filter, next)` route
    /// (`include/qpdf/QPDFPageObjectHelper.hh:261-265`). Passing `None`
    /// discards generated output, which is qpdf's default `next = nullptr`
    /// behavior.
    pub fn filter_contents_with_pipeline<'b>(
        &mut self,
        filter: &'b mut dyn TokenFilter,
        next: Option<&'b mut dyn Pipeline>,
    ) -> Result<()> {
        let (target, is_form) = self.resolved_attribute_target()?;
        if is_form {
            target.filter_as_contents_with_pipeline(filter, next)
        } else {
            target.filter_page_contents_with_pipeline(filter, next)
        }
    }

    /// Pipe this helper's decoded contents into a pipeline.
    ///
    /// A helper built over a Form XObject pipes that Form's own
    /// content stream rather than a page's.
    pub fn pipe_page_contents(&mut self, pipeline: &mut dyn Pipeline) -> Result<()> {
        self.pipe_contents(pipeline)
    }

    /// Pipe this helper's decoded contents into a pipeline.
    ///
    /// A helper built over a Form XObject pipes that Form's own
    /// content stream rather than a page's.
    ///
    /// qpdf's old name for [`Self::pipe_page_contents`], kept as an alias.
    pub fn pipe_contents(&mut self, pipeline: &mut dyn Pipeline) -> Result<()> {
        let (target, is_form) = self.resolved_attribute_target()?;
        if is_form {
            let mut filtering_attempted = false;
            // QPDFPageObjectHelper::pipeContents calls the legacy
            // pipeStreamData overload for Forms and ignores its bool. Only
            // provider/source/sink errors cross this Result boundary; a
            // false overall result is a valid no-op for this overload.
            target.pipe_stream_data(
                pipeline,
                &mut filtering_attempted,
                0,
                DecodeLevel::Specialized,
                false,
                false,
            )?;
            Ok(())
        } else {
            target.pipe_page_contents(pipeline)
        }
    }

    /// Attach a lazy token filter to the page's content stream.
    pub fn add_content_token_filter(&mut self, filter: Rc<RefCell<dyn TokenFilter>>) -> Result<()> {
        let (target, is_form) = self.resolved_attribute_target()?;
        if is_form {
            return target.add_token_filter(filter);
        }
        target.add_content_token_filter(filter)
    }

    /// Remove unused `/Font` and `/XObject` entries from this target's
    /// resource scope through the canonical ObjectHandle parser route.
    ///
    /// This is qpdf's `removeUnreferencedResources`
    /// (`libqpdf/QPDFPageObjectHelper.cc:539-649`). The document-level
    /// `PageDocumentHelper` facade uses this same per-target operation.
    pub fn remove_unreferenced_resources(&mut self) -> Result<()> {
        crate::resources::remove_unreferenced_resources_on_target(self.pdf, self.object.clone())
    }

    /// Convert inline images into ordinary Image XObjects using qpdf's
    /// defaults `min_size = 0` and `shallow = false`.
    pub fn externalize_inline_images(&mut self) -> Result<()> {
        self.externalize_inline_images_with_options(0, false)
    }

    /// Convert inline images into ordinary Image XObjects with explicit qpdf
    /// `min_size` and `shallow` options.
    ///
    /// This mirrors qpdf's `externalizeInlineImages` implementation
    /// (`libqpdf/QPDFPageObjectHelper.cc:398-437`). The canonical page/Form
    /// token pipeline allocates and attaches each image stream at the same
    /// point that qpdf handles the qualifying inline-image token; the final
    /// content stream is installed only after successful filtering. With
    /// `shallow == false`, nested Form XObjects are processed in the same
    /// bounded traversal as qpdf; `true` limits the operation to this target.
    pub fn externalize_inline_images_with_options(
        &mut self,
        min_size: usize,
        shallow: bool,
    ) -> Result<()> {
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

    /// Visit XObjects accepted by `selector`, preserving each object's
    /// containing `/XObject` dictionary and resource key.
    ///
    /// When `recursive` is true, rejecting a Form skips its callback but does
    /// not prevent traversal into that Form's XObjects, matching qpdf's
    /// `forEachXObject(recursive, action, selector)`
    /// (`libqpdf/QPDFPageObjectHelper.cc:318-349`). The selector receives a
    /// clone of the live object handle, matching qpdf's by-value
    /// `QPDFObjectHandle` callback. Use [`for_each_xobject`](Self::for_each_xobject)
    /// when no selector is needed.
    pub fn for_each_xobject_with_selector<S, F>(
        &mut self,
        recursive: bool,
        mut action: F,
        mut selector: S,
    ) -> Result<()>
    where
        S: FnMut(ObjectHandle) -> Result<bool>,
        F: FnMut(ObjectHandle, ObjectHandle, Vec<u8>) -> Result<()>,
    {
        self.for_each_xobject_filtered(recursive, |object| selector(object.clone()), &mut action)
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
            let resources =
                get_attribute_for_target(node.clone(), b"/Resources", false, &node_description)?; // cov:ignore: traversal already validates each canonical page/Form target; only a defensive resolver error can reach this edge
            if resources.try_is_null()? {
                continue;
            }
            let xobjects = resources.try_get_key(b"/XObject")?;
            if !xobjects.try_is_dictionary()? {
                continue;
            }

            // qpdf iterates `xobj_dict.getKeys()`, which omits keys whose values
            // resolve to null, and reads each value with `getKey`
            // (`libqpdf/QPDFPageObjectHelper.cc:335-338`,
            // `libqpdf/QPDF_Dictionary.cc:118-127`).
            for key in xobjects.try_get_keys()? {
                let object = xobjects.try_get_key(&key)?;
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
    ///
    /// Image masks are excluded, matching qpdf's `isImage()` default
    /// (`QPDFObjectHandle.hh:1331-1334`) and `forEachImage` selector
    /// (`QPDFPageObjectHelper.cc:352-357`).
    pub fn for_each_image<F>(&mut self, recursive: bool, action: F) -> Result<()>
    where
        F: FnMut(ObjectHandle, ObjectHandle, Vec<u8>) -> Result<()>,
    {
        self.for_each_xobject_filtered(recursive, |object| object.is_image(), action)
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

    /// Return direct image XObjects keyed by their resource names.
    ///
    /// qpdf's old `getPageImages` name for [`Self::get_images`], kept as an
    /// alias.
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

    // -----------------------------------------------------------------------
    // get_annotations
    // -----------------------------------------------------------------------

    /// Return all annotation helpers, matching qpdf's default
    /// `getAnnotations("")` call (`include/qpdf/QPDFPageObjectHelper.hh:211`,
    /// `libqpdf/QPDFPageObjectHelper.cc:439-454`). Missing, null, or non-array
    /// `/Annots` values yield an empty result; non-dictionary array members
    /// are skipped. Each returned helper retains the exact direct or indirect
    /// annotation handle. On non-Form receivers, `/Annots` is read directly
    /// without requiring `/Type /Page`.
    pub fn get_annotations(&mut self) -> Result<Vec<AnnotationObjectHelper>> {
        self.get_annotations_with_subtype(b"")
    }

    /// Return annotation helpers restricted to qpdf's `only_subtype` string.
    /// It is compared with the decoded PDF name, so pass `b"Widget"` rather
    /// than `b"/Widget"`; an empty slice disables filtering. This maps
    /// `QPDFPageObjectHelper::getAnnotations(only_subtype)`
    /// (`include/qpdf/QPDFPageObjectHelper.hh:211`,
    /// `libqpdf/QPDFPageObjectHelper.cc:439-454`).
    pub fn get_annotations_with_subtype(
        &mut self,
        only_subtype: &[u8],
    ) -> Result<Vec<AnnotationObjectHelper>> {
        Ok(self
            .get_annotation_handles((!only_subtype.is_empty()).then_some(only_subtype))?
            .into_iter()
            .map(AnnotationObjectHelper::new)
            .collect())
    }

    /// Collect raw annotation handles for crate-internal consumers that
    /// mutate or associate the underlying PDF objects. Public qpdf-shaped
    /// surfaces are [`Self::get_annotations`] and
    /// [`Self::get_annotations_with_subtype`].
    pub(crate) fn get_annotation_handles(
        &mut self,
        only_subtype: Option<&[u8]>,
    ) -> Result<Vec<ObjectHandle>> {
        let annots = self.object.try_get_key(b"/Annots")?;
        let Some(annots_array) = annots.try_as_array()? else {
            return Ok(Vec::new());
        };
        let only_subtype = only_subtype.filter(|value| !value.is_empty());
        let mut result = Vec::with_capacity(annots_array.len());
        for item in annots_array {
            let annotation = &item;
            if !annotation
                .try_is_dictionary_of_type_with_subtype(b"", only_subtype.unwrap_or(b""))?
            {
                continue;
            }
            result.push(item);
        }
        Ok(result)
    }

    // -----------------------------------------------------------------------
    // Bounding boxes
    // -----------------------------------------------------------------------

    fn rectangle_for_matrix(&mut self, value: &ObjectHandle) -> Result<Option<Rectangle>> {
        if !value.try_is_rectangle()? {
            return Ok(None);
        }
        Ok(Some(value.try_get_array_as_rectangle()?))
    }
}

// ---------------------------------------------------------------------------
// Private free functions
// ---------------------------------------------------------------------------

fn resolve_resource_dictionary(
    resources: &ObjectHandle,
    key: &[u8],
) -> Result<Option<ObjectHandle>> {
    let value = resources.try_get_key(key)?;
    if value.try_is_null()? {
        return Ok(None);
    }
    Ok(value.try_is_dictionary()?.then_some(value))
}

fn externalize_inline_images_for_target<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    object: ObjectHandle,
    description: &str,
    min_size: usize,
) -> Result<()> {
    let (target, is_form) = resolve_attribute_target(object)?;
    let resources = get_attribute_for_target(target.clone(), b"/Resources", true, description)?;

    // qpdf uses mergeResources to make /XObject direct and private before the
    // filter runs. This is a no-op when /Resources is absent or malformed,
    // preserving qpdf's warning/no-resource boundary for those documents.
    let empty_xobjects = ObjectHandle::dictionary(Vec::new());
    let seed = ObjectHandle::dictionary(vec![(b"/XObject".to_vec(), empty_xobjects)]);
    resources.merge_resources(&seed)?;

    let mut rewritten = Vec::new();
    let any_images = {
        let mut filter = InlineImageExternalizer::new(min_size, resources.clone(), pdf);
        let mut sink = PlString::new("externalized inline image content", None, &mut rewritten);
        // qpdf catches filter/parser failures here, warns, and leaves the
        // original content untouched. The Rust Result boundary is retained
        // for setup/mutation errors; an unsuccessful filter is the same
        // warning-only no-op rather than a partially rewritten page.
        let filter_result = if is_form {
            target.filter_as_contents_with_pipeline(&mut filter, Some(&mut sink))
        } else {
            target.filter_page_contents_with_pipeline(&mut filter, Some(&mut sink))
        };
        match filter_result {
            Ok(()) => filter.any_images,
            Err(error) => {
                target.warn_if_possible(&format!(
                    "Unable to filter content stream: {error}; not attempting to externalize inline images from this stream"
                ))?;
                return Ok(());
            }
        }
    };
    if !any_images {
        return Ok(());
    }

    if is_form {
        target.replace_stream_data(
            Rc::new(rewritten),
            Some(ObjectHandle::null()),
            Some(ObjectHandle::null()),
        )?; // cov:ignore: this branch already established a same-Pdf Form stream and passes direct null filters.
    } else {
        let contents = pdf.new_stream_with_data(Rc::new(rewritten))?;
        target.replace_key(b"/Contents", contents)?;
    }
    Ok(())
}

fn object_handle_description(object: &ObjectHandle) -> String {
    object
        .object_ref()
        .map(|object_ref| object_ref.to_string())
        .unwrap_or_else(|| "direct object".to_owned())
}

pub(crate) fn rectangle_from_handle(handle: &ObjectHandle) -> Result<Option<Rectangle>> {
    let Some(items) = handle.try_as_array()? else {
        return Ok(None);
    };
    if items.len() != 4 {
        return Ok(None);
    }
    let mut values = [0.0f64; 4];
    for (index, item) in items.into_iter().enumerate() {
        let Some(value) = item
            .try_as_integer()?
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

fn append_annotation_handles(page: &ObjectHandle, annotations: Vec<ObjectHandle>) -> Result<()> {
    let existing = page.try_get_key(b"/Annots")?;
    let annots = if existing.try_is_array()? {
        existing
    } else {
        let replacement = ObjectHandle::array(Vec::new());
        page.replace_key(b"/Annots", replacement.clone())?;
        replacement
    };
    for annotation in annotations {
        annots.append_array_item(annotation)?;
    }
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
    if !page.get_obj_gen().is_indirect() {
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

fn matrix_from_handle(handle: &ObjectHandle) -> Result<Option<Matrix>> {
    let Some(items) = handle.try_as_array()? else {
        return Ok(None);
    };
    if items.len() != 6 {
        return Ok(None);
    }
    let mut values = [0.0f64; 6];
    for (index, item) in items.into_iter().enumerate() {
        let Some(value) = item
            .try_as_integer()?
            .map(|value| value as f64)
            .or_else(|| item.as_real())
        else {
            return Ok(None);
        };
        values[index] = value;
    }
    Ok(Some(Matrix::from(values)))
}

fn resolve_attribute_target(object: ObjectHandle) -> Result<(ObjectHandle, bool)> {
    // QPDFPageObjectHelper dispatches by Form XObject only. Page and other
    // non-Form handles flow into the delegated QPDFObjectHandle operation,
    // which owns its own type warnings and failures.
    let is_form = object.is_form_xobject()?;
    Ok((object, is_form))
}

fn get_attribute_for_target(
    object: ObjectHandle,
    key: &[u8],
    copy_if_shared: bool,
    description: &str,
) -> Result<ObjectHandle> {
    // qpdf's getAttribute classifies only Form XObjects. It reads a Form's
    // stream dictionary and otherwise invokes getKey directly on the supplied
    // handle, without requiring a /Type /Page entry or a dictionary preflight.
    let is_form = object.is_form_xobject()?;
    let dict = if is_form {
        // cov:ignore-start: is_form_xobject returns true only for a stream
        // with a canonical dictionary.
        object.as_stream_dict().ok_or_else(|| {
            Error::Unsupported(format!("object {description} is not a Form stream"))
        })?
        // cov:ignore-end
    } else {
        object.clone()
    };
    let inheritable = !is_form && is_inheritable_page_attribute(key);
    let mut result = dict.try_get_key(key)?;
    let mut inherited = false;

    if result.try_is_null()? && inheritable {
        // qpdf starts its seen set with this dictionary, then tests and
        // follows each parent in source order. Identity terminates cycles;
        // the walk has no numeric depth limit.
        #[allow(
            clippy::mutable_key_type,
            reason = "qpdf cycle detection keys the live object identity"
        )]
        let mut seen: HashSet<ObjectHandleIdentity> = HashSet::new();
        let mut node = dict.clone();
        loop {
            if !seen.insert(node.identity_key()) || !node.try_has_key(b"/Parent")? {
                break;
            }
            node = node.try_get_key(b"/Parent")?;
            result = node.try_get_key(key)?;
            if !result.try_is_null()? {
                inherited = true;
                break;
            }
        }
    }

    if copy_if_shared && (inherited || result.is_indirect()) {
        let copy = result.shallow_copy()?;
        dict.replace_key(key, copy.clone())?;
        result = copy;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn helper_for_ref(
        pdf: &mut Pdf<Cursor<Vec<u8>>>,
        object_ref: ObjectRef,
    ) -> PageObjectHelper<'_, Cursor<Vec<u8>>> {
        let object = pdf.get_object_handle(object_ref);
        PageObjectHelper::from_object_handle(object, pdf)
    }

    struct NoopTokenFilter;

    impl TokenFilter for NoopTokenFilter {
        fn handle_token(
            &mut self,
            token: &Token,
            output: &mut crate::TokenFilterOutput<'_>,
        ) -> crate::PipelineResult<()> {
            output.write_token(token)
        }
    }

    #[test]
    fn externalize_inline_images_replaces_a_form_stream_with_qpdf_stream_data() -> Result<()> {
        let mut pdf = Pdf::empty()?;
        let mut content = b"q 200 0 0 200 0 0 cm BI /W 2 /H 2 /CS /G /BPC 8 ID\n".to_vec();
        content.extend_from_slice(&[0, 64, 128, 255]);
        content.extend_from_slice(b"\nEI Q\n");
        let form = pdf.new_stream_with_data(Rc::new(content))?;
        let dictionary = form.try_get_stream_dict()?;
        dictionary.replace_key(b"/Type", ObjectHandle::name(b"XObject".to_vec()))?;
        dictionary.replace_key(b"/Subtype", ObjectHandle::name(b"Form".to_vec()))?;
        let bounding_box = ObjectHandle::array(vec![
            ObjectHandle::integer(0),
            ObjectHandle::integer(0),
            ObjectHandle::integer(200),
            ObjectHandle::integer(200),
        ]);
        dictionary
            .replace_key(b"/BBox", bounding_box)
            .expect("new Form stream dictionary accepts /BBox");
        dictionary.replace_key(b"/Resources", ObjectHandle::dictionary(Vec::new()))?;

        externalize_inline_images_for_target(&mut pdf, form.clone(), "form object", 0)?;

        let rewritten = form.get_raw_stream_data()?;
        assert!(rewritten
            .windows(b"/IIm1 Do".len())
            .any(|bytes| bytes == b"/IIm1 Do"));
        let xobjects = dictionary
            .try_get_key(b"/Resources")?
            .try_get_key(b"/XObject")?;
        assert!(xobjects.try_has_key(b"/IIm1")?);
        assert!(dictionary.try_has_key(b"/Length")?);
        Ok(())
    }

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

    fn direct_page_handle() -> ObjectHandle {
        ObjectHandle::dictionary(vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"Page".to_vec())),
            (
                b"/MediaBox".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(612),
                    ObjectHandle::integer(792),
                ]),
            ),
            (b"/Resources".to_vec(), ObjectHandle::dictionary(Vec::new())),
            (b"/Annots".to_vec(), ObjectHandle::array(Vec::new())),
        ])
    }

    #[test]
    fn copy_annotations_defaults_to_identity_and_preserves_explicit_matrix() -> Result<()> {
        let bytes = pdf_from_objects(
            1,
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>".to_owned()),
                (2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>".to_owned()),
                (
                    3,
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << >> /Annots [6 0 R] >>".to_owned(),
                ),
                (
                    4,
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << >> /Annots [] >>".to_owned(),
                ),
                (
                    5,
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << >> /Annots [] >>".to_owned(),
                ),
                (
                    6,
                    "<< /Type /Annot /Subtype /Link /Rect [1.25 2.5 11.75 22.125] >>".to_owned(),
                ),
            ],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("annotation fixture should parse");
        let source_page = pdf.get_object_handle(ObjectRef::new(3, 0));
        let identity_target = pdf.get_object_handle(ObjectRef::new(4, 0));
        let translated_target = pdf.get_object_handle(ObjectRef::new(5, 0));

        PageObjectHelper::from_object_handle(identity_target.clone(), &mut pdf)
            .copy_annotations(source_page.clone())?;

        PageObjectHelper::from_object_handle(translated_target.clone(), &mut pdf)
            .copy_annotations_with_matrix(source_page, Matrix::new(1.0, 0.0, 0.0, 1.0, 10.0, 20.0))
            .expect("explicit custom-matrix copy should succeed");

        let identity_annotation = identity_target
            .try_get_key(b"/Annots")?
            .try_get_array_item(0)?;
        let identity_rect = identity_annotation
            .try_get_key(b"/Rect")?
            .try_get_array_as_rectangle()?;
        assert_eq!(
            identity_rect,
            crate::Rectangle::new(1.25, 2.5, 11.75, 22.125)
        );

        let translated_annotation = translated_target
            .try_get_key(b"/Annots")?
            .try_get_array_item(0)?;
        let translated_rect = translated_annotation
            .try_get_key(b"/Rect")?
            .try_get_array_as_rectangle()?;
        assert_eq!(
            translated_rect,
            crate::Rectangle::new(11.25, 22.5, 21.75, 42.125)
        );
        Ok(())
    }

    #[test]
    fn get_form_xobject_for_page_rejects_a_direct_page_handle() {
        let mut pdf = Pdf::empty().expect("empty document should be available");
        let mut helper = PageObjectHelper::from_object_handle(direct_page_handle(), &mut pdf);

        let error = helper
            .get_form_xobject_for_page()
            .expect_err("qpdf rejects getFormXObjectForPage on a direct page");
        assert!(
            error.to_string().contains(
                "QPDFPageObjectHelper::getFormXObjectForPage called with a direct object"
            ),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn copy_annotations_rejects_a_direct_destination_page_handle() {
        let source_bytes = pdf_from_objects(
            1,
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>".to_owned()),
                (
                    2,
                    "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
                ),
                (
                    3,
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Annots [] >>".to_owned(),
                ),
            ],
        );
        let mut source = Pdf::open(Cursor::new(source_bytes)).expect("source PDF should parse");
        let source_page = source.get_object_handle(ObjectRef::new(3, 0));
        let mut target = Pdf::empty().expect("empty document should be available");
        let mut destination =
            PageObjectHelper::from_object_handle(direct_page_handle(), &mut target);

        let error = destination
            .copy_annotations_from(source_page, &mut source)
            .expect_err("qpdf rejects copyAnnotations on a direct destination page");
        assert!(
            error
                .to_string()
                .contains("QPDFPageObjectHelper::copyAnnotations: this page is a direct object"),
            "unexpected error: {error}"
        );
    }

    /// qpdf `getAttribute` (`libqpdf/QPDFPageObjectHelper.cc:236-247`) walks
    /// an acyclic `/Parent` chain until it finds the value, without a numeric
    /// depth limit. Keep a case beyond flpdf's former limit so the live helper
    /// remains aligned with that behavior.
    #[test]
    fn get_media_box_reaches_a_120th_ancestor_like_qpdf() {
        // Objects 2..=121 are 120 nested /Pages nodes (2 = outermost, the
        // 120th ancestor of the leaf; 121 = the leaf's immediate parent).
        // /MediaBox is set only on object 2.
        let mut objects: Vec<(u32, String)> =
            vec![(1, "<< /Type /Catalog /Pages 2 0 R >>".to_string())];
        for depth in 0..120u32 {
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
        let leaf_ref = 2 + 120;
        objects.push((
            leaf_ref,
            format!("<< /Type /Page /Parent {} 0 R >>", leaf_ref - 1),
        ));

        let bytes = pdf_from_objects(1, &objects);
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        let mut helper = helper_for_ref(&mut pdf, ObjectRef::new(leaf_ref, 0));
        let media_box = helper
            .get_media_box()
            .expect("the 100th ancestor's /MediaBox must be reachable");
        assert!(!media_box.try_is_null().expect("resolved handle"));
    }

    #[test]
    fn page_helper_propagates_unowned_resolution_errors() {
        let mut pdf = Pdf::<Cursor<Vec<u8>>>::empty().expect("empty PDF should be available");
        let object = ObjectHandle::new_indirect_unresolved(ObjectRef::new(99, 0), 0);
        let mut helper = PageObjectHelper::from_object_handle(object, &mut pdf);

        let error = helper
            .get_media_box()
            .expect_err("an unowned indirect handle must not be treated as a direct page");
        assert!(
            error.to_string().contains("belongs to a dropped PDF"),
            "unexpected resolver error: {error}"
        );
    }

    #[test]
    fn add_content_token_filter_uses_the_live_page_and_form_routes() {
        let bytes = pdf_from_objects(
            1,
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>".to_owned()),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned()),
                (
                    3,
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 4 0 R >>"
                        .to_owned(),
                ),
                (4, "<< /Length 1 >>\nstream\nq\nendstream".to_owned()),
            ],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
        helper_for_ref(&mut pdf, ObjectRef::new(3, 0))
            .add_content_token_filter(Rc::new(RefCell::new(NoopTokenFilter)))
            .expect("page content filter should use the page route");
        pdf.get_object_handle(ObjectRef::new(4, 0))
            .get_stream_data(DecodeLevel::Specialized)
            .expect("page filter should remain executable through the live stream");

        let form = ObjectHandle::direct_stream(
            ObjectHandle::dictionary(vec![
                (b"/Type".to_vec(), ObjectHandle::name(b"XObject".to_vec())),
                (b"/Subtype".to_vec(), ObjectHandle::name(b"Form".to_vec())),
                (
                    b"/BBox".to_vec(),
                    ObjectHandle::array(vec![
                        ObjectHandle::integer(0),
                        ObjectHandle::integer(0),
                        ObjectHandle::integer(10),
                        ObjectHandle::integer(10),
                    ]),
                ),
            ]),
            Rc::new(b"q".to_vec()),
        );
        let form_for_read = form.clone();
        PageObjectHelper::from_object_handle(form, &mut pdf)
            .add_content_token_filter(Rc::new(RefCell::new(NoopTokenFilter)))
            .expect("Form XObject filter should use the stream route");
        form_for_read
            .get_stream_data(DecodeLevel::Specialized)
            .expect("Form filter should remain executable through the live stream");
    }

    #[test]
    fn form_xobject_without_transformations_resolves_transform_attributes() {
        // qpdf reads /Rotate and /UserUnit before checking whether the
        // transformation matrix should be installed
        // (QPDFPageObjectHelper.cc:722-729). A dangling /Rotate reference
        // therefore still records its resolution warning for the false
        // variant.
        let bytes = pdf_from_objects(
            1,
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>".to_owned()),
                (
                    2,
                    "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
                ),
                (
                    3,
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Rotate 99 0 R /UserUnit 100 0 R >>".to_owned(),
                ),
                (4, "<< /Length 0 >>\nstream\n\nendstream".to_owned()),
            ],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");

        let form = helper_for_ref(&mut pdf, ObjectRef::new(3, 0))
            .get_form_xobject_for_page_with_options(false)
            .expect("false transformation variant should still create a Form XObject");

        assert!(form.is_form_xobject().expect("classify Form XObject"));
        let rotate = pdf.get_object_handle(ObjectRef::new(99, 0));
        assert!(rotate.is_resolved(), "false variant must resolve /Rotate");
        let user_unit = pdf.get_object_handle(ObjectRef::new(100, 0));
        assert!(
            user_unit.is_resolved(),
            "false variant must resolve /UserUnit"
        );
    }

    #[test]
    fn resource_lookup_helpers_cover_missing_and_non_dictionary_values() {
        assert!(ObjectHandle::integer(1)
            .get_resource_names()
            .expect("non-dictionary resources have no names")
            .is_empty());

        let nested = ObjectHandle::dictionary(vec![(
            b"/Font".to_vec(),
            ObjectHandle::dictionary(vec![(b"/F1".to_vec(), ObjectHandle::integer(1))]),
        )]);
        assert!(nested
            .get_resource_names()
            .expect("nested resource dictionaries have names")
            .contains(b"/F1".as_slice()));

        let missing = ObjectHandle::dictionary(Vec::new());
        assert!(resolve_resource_dictionary(&missing, b"/ColorSpace")
            .expect("missing resource category is allowed")
            .is_none());

        let non_dictionary =
            ObjectHandle::dictionary(vec![(b"/ColorSpace".to_vec(), ObjectHandle::integer(1))]);
        assert!(resolve_resource_dictionary(&non_dictionary, b"/ColorSpace")
            .expect("non-dictionary resource category is ignored")
            .is_none());

        let dictionary = ObjectHandle::dictionary(vec![(
            b"/ColorSpace".to_vec(),
            ObjectHandle::dictionary(vec![(
                b"/Spot".to_vec(),
                ObjectHandle::name(b"Separation".to_vec()),
            )]),
        )]);
        assert!(resolve_resource_dictionary(&dictionary, b"/ColorSpace")
            .expect("dictionary resource category should resolve")
            .is_some());
    }

    #[test]
    fn fix_copied_annotations_retains_the_orphan_widget_s_self_association() {
        // qpdf's `QPDFAcroFormDocumentHelper::analyze` self-associates a page
        // Widget unreachable from `/AcroForm/Fields` as its own field
        // (`QPDFAcroFormDocumentHelper.cc`'s orphan-widget fallback). That
        // association must survive a `fix_copied_annotations_from` copy: the
        // destination's own `/AcroForm` must end up containing the copied
        // orphan widget, not merely succeed without error.
        let mut source = Pdf::open_mem_owned(
            include_bytes!("../../../tests/fixtures/compat/acroform-sig-orphan-widget.pdf")
                .to_vec(),
        )
        .expect("source fixture should parse");
        let source_page_ref = crate::pages::page_refs(&mut source)
            .expect("source pages should resolve")
            .into_iter()
            .next()
            .expect("source should have one page");
        let source_page = source.get_object_handle(source_page_ref);
        let mut target = Pdf::empty().expect("target should be constructible");
        crate::PageDocumentHelper::new(&mut target)
            .add_page(
                crate::PageInput::foreign(&mut source, source_page.clone()),
                false,
            )
            .expect("foreign page should copy");
        let new_page = crate::PageDocumentHelper::new(&mut target)
            .get_all_pages()
            .expect("read copied target page")
            .into_iter()
            .next()
            .expect("target should contain the copied page");

        PageObjectHelper::from_object_handle(new_page, &mut target)
            .fix_copied_annotations_from(source_page, &mut source)
            .expect("copied annotations should be repaired");

        let root_ref = target.root_ref().expect("target has a catalog");
        let root = target.get_object_handle(root_ref);
        root.try_is_scalar().expect("resolve target catalog");
        let acroform = root.try_get_key(b"/AcroForm").expect("read /AcroForm key");
        acroform.try_is_scalar().expect("resolve /AcroForm");
        assert!(
            acroform.as_dictionary().is_some(),
            "the copied orphan widget must produce a destination /AcroForm"
        );
        let fields = acroform.try_get_key(b"/Fields").expect("read /Fields key");
        fields.try_is_scalar().expect("resolve /Fields");
        assert_eq!(
            fields
                .try_as_array()
                .expect("resolve /Fields array")
                .expect("/Fields is an array")
                .len(),
            1,
            "the orphan widget must be registered as its own field"
        );
    }

    fn target_with_warm_empty_acroform() -> (Pdf<Cursor<Vec<u8>>>, ObjectHandle) {
        let mut target = Pdf::empty().expect("target should be constructible");
        let direct_page = ObjectHandle::dictionary(vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"Page".to_vec())),
            (
                b"/MediaBox".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(612),
                    ObjectHandle::integer(792),
                ]),
            ),
        ]);
        crate::PageDocumentHelper::new(&mut target)
            .add_page(crate::PageInput::target(direct_page), false)
            .expect("direct page should be inserted");
        let new_page = crate::PageDocumentHelper::new(&mut target)
            .get_all_pages()
            .expect("read inserted page")
            .into_iter()
            .next()
            .expect("target should contain the inserted page");

        {
            let mut acroform = crate::AcroFormDocumentHelper::new(&mut target)
                .expect("target AcroForm helper should initialize");
            acroform
                .canonical_get_or_create_acroform()
                .expect("target AcroForm should be created");
            acroform.invalidate_cache();
        }
        let _ = crate::AcroFormDocumentHelper::new(&mut target)
            .expect("target AcroForm cache should warm");
        assert!(target.acroform_cache.borrow().is_some());
        (target, new_page)
    }

    fn orphan_widget_source() -> (Pdf<Cursor<Vec<u8>>>, ObjectRef) {
        let bytes = pdf_from_objects(
            1,
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>".to_owned()),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned()),
                (
                    3,
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [4 0 R] >>"
                        .to_owned(),
                ),
                (
                    4,
                    "<< /Type /Annot /Subtype /Widget /Rect [0 0 10 10] >>".to_owned(),
                ),
            ],
        );
        let mut source = Pdf::open(Cursor::new(bytes)).expect("orphan source should parse");
        let page = crate::pages::page_refs(&mut source)
            .expect("orphan source pages should resolve")
            .into_iter()
            .next()
            .expect("orphan source should have one page");
        (source, page)
    }

    #[test]
    fn copy_annotations_keeps_warm_cache_for_multi_kid_field_widgets() {
        let mut source = Pdf::open_mem_owned(
            include_bytes!("../../../tests/fixtures/compat/form-fields-and-annotations.pdf")
                .to_vec(),
        )
        .expect("field source should parse");
        let source_page_ref = crate::pages::page_refs(&mut source)
            .expect("field source pages should resolve")
            .into_iter()
            .next()
            .expect("field source should have one page");
        let source_page = source.get_object_handle(source_page_ref);
        let (mut target, new_page) = target_with_warm_empty_acroform();

        PageObjectHelper::from_object_handle(new_page.clone(), &mut target)
            .copy_annotations_from(source_page, &mut source)
            .expect("field-backed annotations should copy");

        assert!(
            target.acroform_cache.borrow().is_some(),
            "field-backed annotation copies must retain the incrementally updated cache"
        );

        let page = new_page.clone();
        page.try_is_scalar().expect("copied page should resolve");
        let annots = page.try_get_key(b"/Annots").expect("read /Annots");
        annots
            .try_is_scalar()
            .expect("copied annotations should resolve");
        let widget_refs: Vec<ObjectRef> = annots
            .try_as_array()
            .expect("/Annots should be an array")
            .expect("/Annots should be present")
            .into_iter()
            .filter_map(|annotation| {
                annotation
                    .try_is_scalar()
                    .expect("annotation should resolve");
                annotation
                    .try_is_dictionary_of_type_with_subtype(b"", b"Widget")
                    .expect("widget classification should resolve")
                    .then(|| annotation.object_ref())
                    .flatten()
            })
            .collect();
        assert!(
            widget_refs.len() >= 3,
            "fixture should exercise a field with multiple widget kids"
        );

        let mut acroform = crate::AcroFormDocumentHelper::new(&mut target)
            .expect("target AcroForm helper should reuse the warm cache");
        for widget_ref in widget_refs {
            assert!(
                acroform
                    .get_field_for_annotation(widget_ref)
                    .expect("field association should resolve")
                    .is_some(),
                "every copied field-backed widget must be registered incrementally"
            );
        }
    }

    #[test]
    fn copy_annotations_keeps_warm_cache_for_non_widget_annotations() {
        let mut source = Pdf::open_mem_owned(
            include_bytes!("../../../tests/fixtures/compat/link-annot-no-acroform.pdf").to_vec(),
        )
        .expect("link source should parse");
        let source_page_ref = crate::pages::page_refs(&mut source)
            .expect("link source pages should resolve")
            .into_iter()
            .next()
            .expect("link source should have one page");
        let source_page = source.get_object_handle(source_page_ref);
        let (mut target, new_page) = target_with_warm_empty_acroform();

        PageObjectHelper::from_object_handle(new_page.clone(), &mut target)
            .copy_annotations_from(source_page, &mut source)
            .expect("non-widget annotations should copy");

        assert!(
            target.acroform_cache.borrow().is_some(),
            "non-Widget annotation copies must not invalidate the AcroForm cache"
        );
    }

    #[test]
    fn copy_annotations_invalidates_cache_for_an_unassociated_widget() {
        let (mut source, source_page_ref) = orphan_widget_source();
        let source_page = source.get_object_handle(source_page_ref);
        let (mut target, new_page) = target_with_warm_empty_acroform();

        PageObjectHelper::from_object_handle(new_page.clone(), &mut target)
            .copy_annotations_from(source_page, &mut source)
            .expect("orphan annotation should copy");

        assert!(
            target.acroform_cache.borrow().is_none(),
            "an unassociated copied Widget must invalidate the orphan-scan cache"
        );

        let page = new_page.clone();
        page.try_is_scalar().expect("copied page should resolve");
        let annotation = page.try_get_key(b"/Annots").expect("read /Annots");
        annotation
            .try_is_scalar()
            .expect("copied annotations should resolve");
        let annotation = annotation
            .try_as_array()
            .expect("/Annots should be an array")
            .expect("/Annots should be present")
            .into_iter()
            .next()
            .expect("copied orphan should be present");
        annotation.try_is_scalar().expect("orphan should resolve");
        let annotation_ref = annotation
            .object_ref()
            .expect("orphan copy should be indirect");

        let mut acroform = crate::AcroFormDocumentHelper::new(&mut target)
            .expect("target AcroForm helper should rebuild after invalidation");
        assert_eq!(
            acroform
                .get_field_for_annotation(annotation_ref)
                .expect("orphan association should resolve"),
            Some(annotation_ref),
            "the rebuilt qpdf orphan scan must self-associate the copied Widget"
        );
    }

    #[test]
    fn inline_image_dictionary_expands_qpdf_abbreviations() {
        let mut pdf = Pdf::<Cursor<Vec<u8>>>::empty().expect("empty PDF should be available");
        let mut externalizer =
            InlineImageExternalizer::new(0, ObjectHandle::dictionary(Vec::new()), &mut pdf);

        let image = externalizer
            .convert_inline_image_dictionary(
                b"<< /BPC 8 /CS /RGB /D [0 1] /DP << >> /F /AHx /H 2 /IM true /I false /W 1 /Other 5 >>",
                3,
            )
            .expect("valid inline-image dictionaries should convert");
        assert_eq!(
            image.try_get_key(b"/Type").unwrap().as_name(),
            Some(b"XObject".to_vec())
        );
        assert_eq!(
            image.try_get_key(b"/Subtype").unwrap().as_name(),
            Some(b"Image".to_vec())
        );
        assert_eq!(
            image
                .try_get_key(b"/BitsPerComponent")
                .unwrap()
                .as_integer(),
            Some(8)
        );
        assert_eq!(
            image.try_get_key(b"/ColorSpace").unwrap().as_name(),
            Some(b"DeviceRGB".to_vec())
        );
        assert_eq!(
            image.try_get_key(b"/Filter").unwrap().as_name(),
            Some(b"ASCIIHexDecode".to_vec())
        );
        assert_eq!(image.try_get_key(b"/Height").unwrap().as_integer(), Some(2));
        assert_eq!(image.try_get_key(b"/Width").unwrap().as_integer(), Some(1));
        assert_eq!(image.try_get_key(b"/Length").unwrap().as_integer(), Some(3));
        assert_eq!(image.try_get_key(b"/Other").unwrap().as_integer(), Some(5));

        let error = externalizer
            .convert_inline_image_dictionary(b"[]", 0)
            .expect_err("an inline-image header must be a dictionary");
        assert!(error.to_string().contains("did not parse as a dictionary"));
    }

    #[test]
    fn inline_image_externalizer_covers_colorspace_filters_and_name_conflicts() {
        let mut pdf = Pdf::<Cursor<Vec<u8>>>::empty().expect("empty PDF should be available");
        let mut externalizer = InlineImageExternalizer::new(
            0,
            ObjectHandle::dictionary(vec![
                (
                    b"/ColorSpace".to_vec(),
                    ObjectHandle::dictionary(vec![(
                        b"/Custom".to_vec(),
                        ObjectHandle::name(b"Resolved".to_vec()),
                    )]),
                ),
                (
                    b"/XObject".to_vec(),
                    ObjectHandle::dictionary(vec![(b"/IIm1".to_vec(), ObjectHandle::integer(1))]),
                ),
            ]),
            &mut pdf,
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
                    .expect("builtin colorspace conversion")
                    .as_name(),
                Some(expanded.to_vec())
            );
        }
        assert_eq!(
            externalizer
                .convert_color_space(ObjectHandle::name(b"Custom".to_vec()))
                .expect("custom colorspace conversion")
                .as_name(),
            Some(b"Resolved".to_vec())
        );
        assert_eq!(
            externalizer
                .convert_color_space(ObjectHandle::name(b"Missing".to_vec()))
                .expect("missing colorspace conversion")
                .as_name(),
            Some(b"Missing".to_vec())
        );
        assert_eq!(
            externalizer
                .convert_color_space(ObjectHandle::integer(7))
                .expect("non-name colorspace conversion")
                .as_integer(),
            Some(7)
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

        assert_eq!(externalizer.next_name().unwrap(), b"/IIm2".to_vec());
        assert_eq!(externalizer.next_name().unwrap(), b"/IIm2".to_vec());
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
    fn flatten_rotation_rejects_a_page_owned_by_another_pdf_before_mutation() {
        let bytes = pdf_from_objects(
            1,
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>".to_owned()),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned()),
                (
                    3,
                    "<< /Type /Page /Parent 2 0 R /Rotate 90 /MediaBox [0 0 200 100] /Contents 4 0 R >>"
                        .to_owned(),
                ),
                (4, "<< /Length 0 >>\nstream\n\nendstream".to_owned()),
            ],
        );
        let mut source = Pdf::open(Cursor::new(bytes)).expect("source PDF should parse");
        let page = source.get_object_handle(ObjectRef::new(3, 0));
        let mut other = Pdf::<Cursor<Vec<u8>>>::empty().expect("other PDF should be available");
        let other_object_count = other
            .get_object_count()
            .expect("other PDF object count should resolve");

        let result =
            PageObjectHelper::from_object_handle(page.clone(), &mut other).flatten_rotation();

        assert!(
            matches!(result, Err(Error::Unsupported(ref message)) if message.contains("another Pdf")),
            "a page handle owned by another Pdf must be rejected, got {result:?}"
        );
        assert_eq!(
            page.try_get_key(b"/Rotate")
                .expect("page rotation should resolve")
                .try_as_integer()
                .expect("page rotation should be an integer"),
            Some(90),
            "the source page must remain unchanged"
        );
        assert_eq!(
            page.try_get_key(b"/MediaBox")
                .expect("page box should resolve")
                .unparse_resolved()
                .unwrap(),
            b"[ 0 0 200 100 ]",
            "the source page box must remain unchanged"
        );
        assert_eq!(
            page.try_get_key(b"/Contents")
                .expect("page contents should resolve")
                .object_ref(),
            Some(ObjectRef::new(4, 0)),
            "the source page contents must remain unchanged"
        );
        assert_eq!(
            other
                .get_object_count()
                .expect("other PDF object count should resolve"),
            other_object_count,
            "the helper PDF must not receive rotation streams"
        );
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
