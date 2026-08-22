//! qpdf correspondence: `QPDFFormFieldObjectHelper.cc` rendering primitives.
//! Appearance-stream generators for AcroForm widgets.
//!
//! This module builds the `/AP/N` (normal-appearance) Form XObject for
//! AcroForm **Tx** and **Ch** widgets. Button appearance generation is not
//! part of the production path: qpdf's
//! `QPDFFormFieldObjectHelper::generateAppearance` dispatches only `/Tx` and
//! `/Ch` (`QPDFFormFieldObjectHelper.cc:472-478`).
//!
//! The production route follows qpdf's limited generator rather than adding a
//! font-metrics layout engine: quadding is ignored, and text is encoded as
//! ASCII unless qpdf finds a `/Encoding /WinAnsiEncoding` or
//! `/MacRomanEncoding` font in the existing appearance resources or
//! document-level `/AcroForm /DR` (`QPDFFormFieldObjectHelper.cc:576-577,
//! 811-849`).

use std::cell::RefCell;
use std::io::{Read, Seek};
use std::rc::Rc;

use crate::annotation_helper::AnnotationObjectHelper;
use crate::content_stream::{parse_content_stream_data, ParseControl, ParserCallbacks};
use crate::default_appearance::parse_default_appearance;
use crate::form_field_object_helper::FormFieldObjectHelper;
use crate::object::write_literal_string;
use crate::object_handle::ObjectHandle;
use crate::page_object_helper::PageBox;
use crate::pipeline::PipelineResult;
use crate::token_filter::{TokenFilter, TokenFilterOutput};
use crate::tokenizer::{Token, TokenType};
use crate::{Error, Object, ObjectRef, Pdf, Result};

/// Resolve one canonical handle hop without materializing a legacy `Object`.
fn resolve_canonical<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    handle: ObjectHandle,
) -> Result<ObjectHandle> {
    pdf.resolve_object_handle(&handle)?;
    Ok(handle)
}

/// Read a rectangle-shaped array (`/Rect`, `/BBox`, …) at `key` through a
/// live handle.
fn resolve_rectangle_canonical<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    handle: &ObjectHandle,
    key: &[u8],
) -> Result<Option<PageBox>> {
    let rect = resolve_canonical(pdf, handle.get_key(key))?;
    let Some(items) = rect.as_array() else {
        return Ok(None);
    };
    if items.len() != 4 {
        return Ok(None);
    }

    let mut values = [0.0; 4];
    for (index, item) in items.into_iter().enumerate() {
        let item = resolve_canonical(pdf, item)?;
        let Some(value) = item
            .as_real()
            .or_else(|| item.as_integer().map(|n| n as f64))
        else {
            return Ok(None);
        };
        values[index] = value;
    }
    Ok(Some(PageBox::new(
        values[0], values[1], values[2], values[3],
    )))
}

/// Read a widget rectangle through the live annotation handle.
fn resolve_rect_canonical<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    widget: &ObjectHandle,
) -> Result<Option<PageBox>> {
    resolve_rectangle_canonical(pdf, widget, b"/Rect")
}

/// Resolve the bounding box used to size a widget's `/AP/N` appearance
/// content.
///
/// qpdf's `QPDFFormFieldObjectHelper::generateTextAppearance` only derives
/// the box from the widget's `/Rect` when creating a fresh `/AP/N` stream.
/// When `/AP/N` is *already* a stream, it lays out content against that
/// stream's own `/BBox` instead -- never against the current `/Rect` -- and
/// leaves `/BBox` itself completely untouched
/// (`libqpdf/QPDFFormFieldObjectHelper.cc:766-793`). If that existing
/// `/BBox` is not a valid four-number rectangle, qpdf warns and aborts
/// appearance generation entirely rather than falling back to `/Rect`
/// (`QPDFFormFieldObjectHelper.cc:788-791`).
///
/// Verified against a real `qpdf 11.9.0 --generate-appearances` run: a
/// widget with an existing `/AP/N` `/BBox [0 0 190 20]` and an enlarged
/// `/Rect [10 10 400 130]` is rewritten with `/BBox` still `[0 0 190 20]`,
/// and the appearance content's `Td` offsets are computed from that
/// unchanged 190×20 box.
fn resolve_appearance_bbox_canonical<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    widget: &ObjectHandle,
) -> Result<Option<PageBox>> {
    let normal = resolve_normal_appearance_canonical(pdf, widget)?;
    if let Some(stream_dict) = normal.as_stream_dict() {
        return resolve_rectangle_canonical(pdf, &stream_dict, b"/BBox");
    }
    resolve_rect_canonical(pdf, widget)
}

/// Select `/AP/N`, including qpdf's `/AS` lookup when `/N` is a state
/// dictionary. The annotation helper owns this qpdf responsibility.
fn resolve_normal_appearance_canonical<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    widget: &ObjectHandle,
) -> Result<ObjectHandle> {
    let mut annotation = AnnotationObjectHelper::from_object_handle(widget.clone(), pdf);
    annotation.get_appearance_stream(b"N", None)
}

#[derive(Clone, Copy)]
enum AppearanceEncoding {
    Ascii,
    WinAnsi,
    MacRoman,
}

#[derive(Clone)]
struct AppearanceFont {
    resource_name: Vec<u8>,
    font: ObjectHandle,
    encoding: AppearanceEncoding,
    from_default_resources: bool,
}

fn font_from_resources<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    resources: ObjectHandle,
    resource_name: &[u8],
) -> Result<Option<ObjectHandle>> {
    let resources = resolve_canonical(pdf, resources)?;
    if resources.as_dictionary().is_none() {
        return Ok(None);
    }
    let font_dict = resolve_canonical(pdf, resources.get_key(b"/Font"))?;
    if font_dict.as_dictionary().is_none() {
        return Ok(None);
    }
    let resource_key = {
        let mut key = Vec::with_capacity(resource_name.len() + 1);
        key.push(b'/');
        key.extend_from_slice(resource_name);
        key
    };
    let font = resolve_canonical(pdf, font_dict.get_key(&resource_key))?;
    Ok((!font.is_null()).then_some(font))
}

fn appearance_font_encoding<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    font: &ObjectHandle,
) -> Result<AppearanceEncoding> {
    let font = resolve_canonical(pdf, font.clone())?;
    let encoding = resolve_canonical(pdf, font.get_key(b"/Encoding"))?;
    Ok(match encoding.as_name().as_deref() {
        Some(b"WinAnsiEncoding") => AppearanceEncoding::WinAnsi,
        Some(b"MacRomanEncoding") => AppearanceEncoding::MacRoman,
        _ => AppearanceEncoding::Ascii,
    })
}

fn lookup_appearance_font<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    field: ObjectHandle,
    normal_appearance: &ObjectHandle,
    resource_name: Option<&[u8]>,
) -> Result<Option<AppearanceFont>> {
    let Some(resource_name) = resource_name.filter(|name| !name.is_empty()) else {
        return Ok(None);
    };

    if let Some(stream_dict) = normal_appearance.as_stream_dict() {
        let resources = resolve_canonical(pdf, stream_dict.get_key(b"/Resources"))?;
        if let Some(font) = font_from_resources(pdf, resources, resource_name)? {
            let encoding = appearance_font_encoding(pdf, &font)?;
            return Ok(Some(AppearanceFont {
                resource_name: resource_name.to_vec(),
                font,
                encoding,
                from_default_resources: false,
            }));
        }
    }

    let resources = FormFieldObjectHelper::from_object_handle(field, pdf).default_resources()?;
    let Some(resources) = resources else {
        return Ok(None);
    };
    let Some(font) = font_from_resources(pdf, resources, resource_name)? else {
        return Ok(None);
    };
    let encoding = appearance_font_encoding(pdf, &font)?;
    Ok(Some(AppearanceFont {
        resource_name: resource_name.to_vec(),
        font,
        encoding,
        from_default_resources: true,
    }))
}

fn encode_appearance_text(value: &str, encoding: AppearanceEncoding) -> Vec<u8> {
    match encoding {
        AppearanceEncoding::Ascii => crate::qutil::utf8_to_ascii(value.as_bytes()),
        AppearanceEncoding::WinAnsi => crate::qutil::utf8_to_win_ansi(value.as_bytes()),
        AppearanceEncoding::MacRoman => crate::qutil::utf8_to_mac_roman(value.as_bytes()),
    }
}

#[derive(Clone, Copy)]
enum AppearanceFilterState {
    Top,
    Bmc,
    Emc,
    End,
}

/// qpdf's `ValueSetter` boundary for an existing `/AP/N` stream. The
/// generated appearance is stored as the replacement body only: the original
/// `/Tx BMC` prefix and any post-`EMC` tokens remain owned by the source
/// stream, exactly as `QPDFFormFieldObjectHelper.cc:524-570` does.
struct AppearanceTokenFilter {
    replacement_body: Vec<u8>,
    state: AppearanceFilterState,
    replaced: bool,
}

impl AppearanceTokenFilter {
    fn new(content: &[u8]) -> Self {
        const PREFIX: &[u8] = b"/Tx BMC\n";
        const SUFFIX: &[u8] = b"EMC\n";
        let replacement_body = content
            .strip_prefix(PREFIX)
            .and_then(|content| content.strip_suffix(SUFFIX))
            .unwrap_or(content)
            .to_vec();
        Self {
            replacement_body,
            state: AppearanceFilterState::Top,
            replaced: false,
        }
    }
}

impl TokenFilter for AppearanceTokenFilter {
    fn handle_token(
        &mut self,
        token: &Token,
        output: &mut TokenFilterOutput<'_>,
    ) -> PipelineResult<()> {
        let mut replace = false;
        match self.state {
            AppearanceFilterState::Top => {
                output.write_token(token)?;
                if token.is_word_value(b"BMC") {
                    self.state = AppearanceFilterState::Bmc;
                }
            }
            AppearanceFilterState::Bmc => {
                if matches!(token.token_type, TokenType::Space | TokenType::Comment) {
                    output.write_token(token)?;
                } else {
                    self.state = AppearanceFilterState::Emc;
                    if token.is_word_value(b"EMC") {
                        replace = true;
                        self.state = AppearanceFilterState::End;
                    }
                }
            }
            AppearanceFilterState::Emc => {
                if token.is_word_value(b"EMC") {
                    replace = true;
                    self.state = AppearanceFilterState::End;
                }
            }
            AppearanceFilterState::End => output.write_token(token)?,
        }
        if replace {
            self.replaced = true;
            output.write(&self.replacement_body)?;
            output.write(b"EMC")?;
        }
        Ok(())
    }

    fn handle_eof(&mut self, output: &mut TokenFilterOutput<'_>) -> PipelineResult<()> {
        if !self.replaced {
            output.write(b"/Tx BMC\n")?;
            output.write(&self.replacement_body)?;
            output.write(b"EMC")?;
            self.replaced = true;
        }
        Ok(())
    }
}

/// Build and install a new `/AP/N` Form XObject through canonical handles.
///
/// This is the qpdf `QPDFFormFieldObjectHelper::generateTextAppearance`
/// allocation boundary. New streams and font dictionaries are registered by
/// the document's canonical allocator; the widget and its existing `/AP`
/// dictionary are then mutated in place, preserving every handle identity
/// already held by the caller.
///
/// Returns `Ok(None)` when `/AP` is present but not a dictionary: qpdf's
/// `replaceKey` no-ops (with a warning) on a non-dictionary receiver
/// (`QPDFObjectHandle::replaceKey`, `QPDFObjectHandle.cc:1199-1210`), so the
/// freshly built stream is never linked into the widget's `/AP/N`. `None`
/// keeps that outcome visible to the caller instead of returning a stream
/// ref the widget does not actually reference.
fn resource_key(name: &[u8]) -> Vec<u8> {
    let mut key = Vec::with_capacity(name.len() + 1);
    key.push(b'/');
    key.extend_from_slice(name);
    key
}

fn add_default_font_to_existing_appearance<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    normal: &ObjectHandle,
    font: &AppearanceFont,
) -> Result<()> {
    let Some(stream_dict) = normal.as_stream_dict() else {
        return Ok(()); // cov:ignore: qpdf calls this helper only after selecting a stream appearance
    };
    let resources = resolve_canonical(pdf, stream_dict.get_key(b"/Resources"))?;
    if resources.as_dictionary().is_none() {
        return Ok(()); // cov:ignore: the missing-resources branch is covered by the malformed AP fixture
    }
    let resources = if resources.is_indirect() {
        let copy = resources.shallow_copy()?;
        let indirect = pdf.make_indirect_object_handle(copy)?;
        stream_dict.replace_key(b"/Resources", indirect.clone())?;
        indirect
    } else {
        resources
    };
    let empty_font = ObjectHandle::dictionary(vec![(
        b"/Font".to_vec(),
        ObjectHandle::dictionary(Vec::new()),
    )]);
    resources.merge_resources(&empty_font, None)?;
    resources
        .get_key(b"/Font")
        .replace_key(&resource_key(&font.resource_name), font.font.clone())?;
    pdf.mark_object_handle_dirty(&resources)?;
    pdf.mark_object_handle_dirty(&stream_dict)
}

fn install_normal_appearance_canonical_handles<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    widget: ObjectHandle,
    content: Vec<u8>,
    bbox_w: f64,
    bbox_h: f64,
    font_resource: Option<AppearanceFont>,
) -> Result<Option<ObjectRef>> {
    pdf.resolve_object_handle(&widget)?;
    let normal = resolve_normal_appearance_canonical(pdf, &widget)?;

    // qpdf keeps an existing normal appearance stream and installs a
    // `ValueSetter` token filter on that same canonical stream
    // (`QPDFFormFieldObjectHelper.cc:766-860`).
    if normal.as_stream_dict().is_some() {
        if let Some(font) = font_resource
            .as_ref()
            .filter(|font| font.from_default_resources)
        {
            add_default_font_to_existing_appearance(pdf, &normal, font)?;
        }
        normal.add_token_filter(Rc::new(RefCell::new(AppearanceTokenFilter::new(&content))))?;
        pdf.mark_object_handle_dirty(
            &normal
                .as_stream_dict()
                .expect("stream checked immediately above"),
        )?; // cov:ignore: llvm-cov maps the successful continuation to a zero-count region
        return normal
            .object_ref()
            .ok_or_else(|| {
                Error::Unsupported("normal appearance stream is not indirect".to_string())
            })
            .map(Some);
    }

    let ap = resolve_canonical(pdf, widget.get_key(b"/AP"))?;
    let stream = pdf.new_stream_with_data(Rc::new(content))?;
    let stream_dict = stream
        .as_stream_dict()
        .ok_or_else(|| Error::Unsupported("new appearance stream has no dictionary".to_string()))?;
    stream_dict.replace_key(b"/Type", ObjectHandle::name(b"XObject".to_vec()))?;
    stream_dict.replace_key(b"/Subtype", ObjectHandle::name(b"Form".to_vec()))?;
    let bbox = ObjectHandle::array(vec![
        ObjectHandle::real(0.0),
        ObjectHandle::real(0.0),
        ObjectHandle::real(bbox_w),
        ObjectHandle::real(bbox_h),
    ]);
    stream_dict.replace_key(b"/BBox", bbox)?;

    let resources = ObjectHandle::dictionary(vec![(
        b"/ProcSet".to_vec(),
        ObjectHandle::array(vec![
            ObjectHandle::name(b"PDF".to_vec()),
            ObjectHandle::name(b"Text".to_vec()),
        ]),
    )]);
    if let Some(font) = font_resource {
        resources.replace_key(
            b"/Font",
            ObjectHandle::dictionary(vec![(resource_key(&font.resource_name), font.font)]),
        )?; // cov:ignore: llvm-cov maps the successful resource insertion continuation to a zero-count region
    }
    stream_dict.replace_key(b"/Resources", resources)?;
    pdf.mark_object_handle_dirty(&stream_dict)?;

    let ap = if ap.is_null() {
        let ap = ObjectHandle::dictionary(Vec::new());
        widget.replace_key(b"/AP", ap.clone())?;
        pdf.mark_object_handle_dirty(&widget)?;
        ap
    } else {
        ap
    };

    // qpdf's replaceKey is a no-op for a non-dictionary /AP value.
    if ap.as_dictionary().is_some() {
        ap.replace_key(b"/N", stream.clone())?;
        pdf.mark_object_handle_dirty(&ap)?;
    } else {
        return Ok(None);
    }

    // cov:ignore-start: new_stream_with_data always allocates an indirect stream
    let stream_ref = stream.object_ref().ok_or_else(|| {
        Error::Unsupported("canonical appearance stream is not indirect".to_string())
    })?;
    // cov:ignore-end
    Ok(Some(stream_ref))
}

/// Canonical Tx appearance generation. Graph reads and writes stay on the
/// field/widget handles; only the appearance content itself is a byte buffer.
#[cfg(test)]
pub(crate) fn render_text_field_canonical<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    field_ref: ObjectRef,
    widget_ref: ObjectRef,
) -> Result<Option<ObjectRef>> {
    let field = pdf.get_object_handle(field_ref);
    let widget = pdf.get_object_handle(widget_ref);
    render_text_field_canonical_handles(pdf, field, widget)
}

/// Canonical Tx appearance generation from live field and Widget handles.
pub(crate) fn render_text_field_canonical_handles<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    field: ObjectHandle,
    widget: ObjectHandle,
) -> Result<Option<ObjectRef>> {
    let field_type = FormFieldObjectHelper::from_object_handle(field.clone(), pdf).field_type()?;
    if field_type.as_deref() != Some(b"/Tx") {
        return Ok(None);
    }

    let value = FormFieldObjectHelper::from_object_handle(field.clone(), pdf).value_as_string()?;
    pdf.resolve_object_handle(&widget)?;
    let Some(rect) = resolve_appearance_bbox_canonical(pdf, &widget)? else {
        return Ok(None);
    };
    let bbox_w = (rect.urx - rect.llx).abs();
    let bbox_h = (rect.ury - rect.lly).abs();
    // qpdf's QPDFObjectHandle::isRectangle validates only that all four
    // entries are numbers (`QPDFObjectHandle.cc:788-800`). It does not impose
    // a positive or minimum width/height before generateTextAppearance builds
    // the appearance from the rectangle (`QPDFFormFieldObjectHelper.cc:766-851`).
    if !bbox_w.is_finite() || !bbox_h.is_finite() {
        return Ok(None);
    }

    let mut helper = FormFieldObjectHelper::from_object_handle(field.clone(), pdf);
    let default_appearance = helper.default_appearance()?;
    let da = parse_default_appearance(default_appearance.as_bytes());
    drop(helper);

    let normal_appearance = resolve_normal_appearance_canonical(pdf, &widget)?;
    let font = lookup_appearance_font(
        pdf,
        field.clone(),
        &normal_appearance,
        da.font_name.as_deref(),
    )?; // cov:ignore: lookup error propagation is covered by the qpdf-shaped helper tests
    let encoding = font
        .as_ref()
        .map(|font| font.encoding)
        .unwrap_or(AppearanceEncoding::Ascii);
    // qpdf's ValueSetter ignores quadding and uses 11pt when the /DA Tf
    // operand is absent or auto-sized (`QPDFFormFieldObjectHelper.cc:797-860`).
    let font_size = if da.auto_size { 11.0 } else { da.font_size };
    let content = build_qpdf_choice_appearance_content(
        default_appearance.as_bytes(),
        &encode_appearance_text(&value, encoding),
        &[],
        bbox_w,
        bbox_h,
        font_size,
        true,
    );
    install_normal_appearance_canonical_handles(pdf, widget, content, bbox_w, bbox_h, font)
}

/// Canonical Ch appearance generation from live field and Widget handles.
pub(crate) fn render_choice_field_canonical_handles<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    field: ObjectHandle,
    widget: ObjectHandle,
) -> Result<Option<ObjectRef>> {
    let field_type = FormFieldObjectHelper::from_object_handle(field.clone(), pdf).field_type()?;
    if field_type.as_deref() != Some(b"/Ch") {
        return Ok(None);
    }

    pdf.resolve_object_handle(&widget)?;
    let Some(rect) = resolve_appearance_bbox_canonical(pdf, &widget)? else {
        return Ok(None);
    };
    let bbox_w = (rect.urx - rect.llx).abs();
    let bbox_h = (rect.ury - rect.lly).abs();
    // Keep qpdf's numeric-rectangle contract: a finite subunit or zero-sized
    // rectangle still reaches the ValueSetter/content builder.
    if !bbox_w.is_finite() || !bbox_h.is_finite() {
        return Ok(None);
    }

    let mut helper = FormFieldObjectHelper::from_object_handle(field.clone(), pdf);
    let default_appearance = helper.default_appearance()?;
    let da = parse_default_appearance(default_appearance.as_bytes());
    // qpdf's `getFlags()` returns a signed C++ `int` and tests the combo
    // bit directly on that signed value (`getFlags() & ff_ch_combo`,
    // `QPDFFormFieldObjectHelper.cc:231-234,800`): for `/Ff -1`, all bits
    // are set in two's complement, so the combo bit reads as set. Do not
    // route through `u32::try_from`, which fails (and previously defaulted
    // to zero) for any negative `/Ff`, incorrectly clearing the combo bit.
    //
    // qpdf's `getFlags()` reads `/Ff` through
    // `QPDFObjectHandle::getIntValueAsInt` (`QPDFObjectHandle.cc:525-540`),
    // which *saturates* an out-of-`int`-range value to `INT_MIN`/`INT_MAX`
    // rather than passing its bit pattern through -- so it is the
    // saturated 32-bit value, not the raw stored integer, that qpdf masks
    // against `ff_ch_combo`. Clamp to the `i32` range before masking so an
    // out-of-range `/Ff` (e.g. `4294967296`, which saturates to `INT_MAX`
    // and so has the combo bit set) agrees with qpdf instead of testing a
    // bit pattern qpdf never actually forms.
    let flags = helper
        .field_flags()?
        .unwrap_or(0)
        .clamp(i64::from(i32::MIN), i64::from(i32::MAX));
    let value = helper.value_as_string()?;
    let options = helper.choices()?;
    drop(helper);

    let normal_appearance = resolve_normal_appearance_canonical(pdf, &widget)?;
    let font = lookup_appearance_font(
        pdf,
        field.clone(),
        &normal_appearance,
        da.font_name.as_deref(),
    )?; // cov:ignore: lookup error propagation is covered by the qpdf-shaped helper tests
    let encoding = font
        .as_ref()
        .map(|font| font.encoding)
        .unwrap_or(AppearanceEncoding::Ascii);
    // qpdf's ValueSetter starts from the Tf operand in /DA. A missing or
    // auto-sized Tf uses its source default of 11pt; it does not invent a
    // field-height-dependent size here.
    let font_size = if da.auto_size { 11.0 } else { da.font_size };
    let content = build_qpdf_choice_appearance_content(
        default_appearance.as_bytes(),
        &encode_appearance_text(&value, encoding),
        &options
            .iter()
            .map(|option| encode_appearance_text(option, encoding))
            .collect::<Vec<_>>(),
        bbox_w,
        bbox_h,
        font_size,
        flags & 0x20000 != 0,
    );
    install_normal_appearance_canonical_handles(pdf, widget, content, bbox_w, bbox_h, font)
}

/// Canonical Ch appearance generation. Graph reads and writes stay on the
/// field/widget handles; only the appearance content itself is a byte buffer.
#[cfg(test)]
pub(crate) fn render_choice_field_canonical<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    field_ref: ObjectRef,
    widget_ref: ObjectRef,
) -> Result<Option<ObjectRef>> {
    let field = pdf.get_object_handle(field_ref);
    let widget = pdf.get_object_handle(widget_ref);
    render_choice_field_canonical_handles(pdf, field, widget)
}

/// Substitute `/DA`'s `Tf` size operand with `resolved_font_size` when it
/// differs from the parsed literal by more than qpdf's `0.001` tolerance,
/// mirroring `TfFinder::getDA()` (`QPDFFormFieldObjectHelper.cc:729-746`).
///
/// Returns `default_appearance` unchanged when no `<name> <size> Tf` pattern
/// is present at all: qpdf's `tf_idx` never matches a token index when
/// `/DA` has no `Tf` operator (`tf_idx` stays `-1`,
/// `QPDFFormFieldObjectHelper.cc:689-716`), so `getDA()` performs no
/// substitution there and no `Tf` operator is invented either.
///
/// Unlike qpdf's raw last-number/last-name tracking (which persists across
/// unrelated operators), this locates the candidate operand using the same
/// `<name> <size> Tf` operand-stack pattern as [`parse_default_appearance`]
/// (operands clear at every operator boundary), so the two functions always
/// agree on which `Tf` occurrence is authoritative. This is within the
/// module's documented observable-equivalence policy for `/DA` text, not
/// byte-identical reproduction of qpdf's own token-level bookkeeping.
fn substitute_da_tf_operand(default_appearance: &[u8], resolved_font_size: f64) -> Vec<u8> {
    struct TfOperandFinder {
        operands: Vec<(Object, usize, usize)>,
        tf_size_span: Option<(usize, usize, f64)>,
    }

    impl ParserCallbacks for TfOperandFinder {
        fn handle_object(
            &mut self,
            object: Object,
            offset: usize,
            length: usize,
        ) -> Result<ParseControl> {
            match object {
                Object::Operator(operator) => {
                    if operator == b"Tf" {
                        if let [.., name, size] = self.operands.as_slice() {
                            if let (Some(_), Some(value)) = (
                                name.0.as_name(),
                                size.0
                                    .as_real()
                                    .or_else(|| size.0.as_integer().map(|n| n as f64)),
                            ) {
                                self.tf_size_span = Some((size.1, size.2, value));
                            }
                        }
                    }
                    self.operands.clear();
                }
                Object::InlineImage(_) => {}
                operand => self.operands.push((operand, offset, length)),
            }
            Ok(ParseControl::Continue)
        }

        fn handle_eof(&mut self) -> Result<()> {
            Ok(())
        }
    }

    let mut finder = TfOperandFinder {
        operands: Vec::new(),
        tf_size_span: None,
    };
    // Best-effort, matching parse_default_appearance's own "skip malformed,
    // last wins" recovery: a parse error partway through still leaves
    // whatever valid Tf occurrence was already found in place.
    let _ = parse_content_stream_data(default_appearance, &mut finder);

    let Some((offset, length, raw_value)) = finder.tf_size_span else {
        return default_appearance.to_vec();
    };
    if (raw_value - resolved_font_size).abs() <= 0.001 {
        return default_appearance.to_vec();
    }

    let mut out = Vec::with_capacity(default_appearance.len());
    out.extend_from_slice(&default_appearance[..offset]);
    out.extend_from_slice(fmt_f64(resolved_font_size).as_bytes());
    out.extend_from_slice(&default_appearance[offset + length..]);
    out
}

/// Reproduce qpdf 11.9.0's `ValueSetter::writeAppearance` layout. In
/// particular, `/I` and `/TI` are intentionally not consulted: the qpdf
/// implementation reads only `/V` and (for non-combo fields) `/Opt` before
/// selecting the visible rows (`QPDFFormFieldObjectHelper.cc:797-860`).
fn build_qpdf_choice_appearance_content(
    default_appearance: &[u8],
    value: &[u8],
    options: &[Vec<u8>],
    bbox_w: f64,
    bbox_h: f64,
    font_size: f64,
    is_combo: bool,
) -> Vec<u8> {
    let tfh = 1.2 * font_size;
    // `bbox_h` and `font_size` are attacker-controlled (`/Rect`, `/DA`): for
    // a small font_size and/or a huge bbox_h, `bbox_h / tfh` can exceed
    // `usize::MAX` as an f64, and `as usize` saturates rather than
    // overflowing (Rust's documented float-to-int cast behavior since
    // 1.45). Clamping immediately — before any signed arithmetic or
    // slicing — removes that saturation risk entirely. The bound is
    // `options.len() + 1`, not `options.len()`: the *found* branch below
    // replaces `lines` outright with a slice of `options` (needs at most
    // `options.len()` rows, and the fixup loop converges to the same
    // window for any max_rows at or above that), but the *not-found*
    // branch keeps the unmatched value as its own row and appends up to
    // `max_rows - 1` options on top, so it needs room for the value plus
    // every option -- `options.len() + 1` rows -- when the bounding box is
    // tall enough to hold them (flpdf-25kg.3.8.2.3's clamp under-served
    // this branch by exactly one row).
    let max_rows = ((bbox_h / tfh).max(0.0) as usize).min(options.len().saturating_add(1));
    let mut lines = vec![value.to_vec()];
    let mut highlight = false;
    let mut highlight_index = 0usize;

    if !is_combo && !options.is_empty() && max_rows >= 2 {
        if let Some(found_index) = options.iter().position(|option| option == value) {
            // All-`usize` window arithmetic (no `as isize` bit
            // reinterpretation): `max_rows >= 2` here, so `first + max_rows
            // - 1` cannot underflow, and the fixup loop only decrements
            // `last` while it is still `>= options.len() >= 1`, so it can
            // never underflow past 0 either.
            let mut first = found_index.saturating_sub(1);
            let mut last = first + max_rows - 1;
            while last >= options.len() {
                first = first.saturating_sub(1);
                last -= 1;
            }
            highlight = true;
            highlight_index = found_index - first;
            lines = options[first..=last].to_vec();
        } else {
            highlight = true;
            lines.extend(options.iter().take(max_rows - 1).cloned());
        }
    }

    let mut out = Vec::new();
    out.extend_from_slice(b"/Tx BMC\n");
    let line_count = lines.len() as f64;
    let mut y = bbox_h - ((bbox_h - (line_count * tfh)) / 2.0);
    if highlight {
        out.extend_from_slice(b"q\n0.85 0.85 0.85 rg\n");
        out.extend_from_slice(b"0 ");
        out.extend_from_slice(fmt_f64(y - tfh * (highlight_index as f64 + 1.0)).as_bytes());
        out.push(b' ');
        out.extend_from_slice(fmt_f64(bbox_w).as_bytes());
        out.push(b' ');
        out.extend_from_slice(fmt_f64(tfh).as_bytes());
        out.extend_from_slice(b" re f\nQ\n");
    }
    y -= font_size;
    out.extend_from_slice(b"q\nBT\n");
    out.extend_from_slice(&substitute_da_tf_operand(default_appearance, font_size));
    out.push(b'\n');
    for (index, line) in lines.iter().enumerate() {
        if index == 0 {
            out.extend_from_slice(b"1 ");
            out.extend_from_slice(fmt_f64(y).as_bytes());
            out.extend_from_slice(b" Td\n");
        } else {
            out.extend_from_slice(b"0 ");
            out.extend_from_slice(fmt_f64(-tfh).as_bytes());
            out.extend_from_slice(b" Td\n");
        }
        write_literal_string(&mut out, line);
        out.extend_from_slice(b" Tj\n");
    }
    out.extend_from_slice(b"ET\nQ\nEMC\n");
    out
}

/// Format an f64 for use in a PDF content stream (locale-independent).
fn fmt_f64(value: f64) -> String {
    if !value.is_finite() {
        return "0".to_string();
    }
    let formatted = format!("{value:.4}");
    let formatted = formatted.trim_end_matches('0');
    let formatted = formatted.trim_end_matches('.');
    if formatted.is_empty() || formatted == "-" {
        "0".to_string() // cov:ignore: fixed four-decimal formatting cannot produce an empty string or lone minus
    } else {
        formatted.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::{buffer::Buffer, qpdf_tokenizer::QpdfTokenizer, Pipeline};
    use std::io::Cursor;

    fn run_appearance_filter(source: &[u8], replacement: &[u8]) -> Vec<u8> {
        let mut filter = AppearanceTokenFilter::new(replacement);
        let mut sink = Buffer::new("appearance filter test", None);
        let mut tokenizer =
            QpdfTokenizer::new("appearance filter test", &mut filter, Some(&mut sink));
        tokenizer.write(source).expect("tokenizer write");
        tokenizer.finish().expect("tokenizer finish");
        drop(tokenizer);
        sink.take_buffer().expect("appearance filter output")
    }

    fn pdf_with_objects(objects: &[String]) -> Vec<u8> {
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::with_capacity(objects.len());
        for (index, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", index + 1, object).as_bytes());
        }
        let xref_start = pdf.len();
        pdf.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }

    fn minimal_tx_pdf() -> Vec<u8> {
        pdf_with_objects(&[
            "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R] /DR <<>> /DA (/Helv 12 Tf 0 g) >> >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [4 0 R] >>".to_string(),
            "<< /Type /Annot /Subtype /Widget /FT /Tx /V (Hello) /Rect [10 10 200 30] >>".to_string(),
        ])
    }

    fn dr_font_tx_pdf(encoding: Option<&str>, value: &str) -> Vec<u8> {
        let font_encoding = encoding
            .map(|name| format!(" /Encoding /{name}"))
            .unwrap_or_default();
        pdf_with_objects(&[
            "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R] /DR << /Font << /F1 5 0 R >> >> /DA (/F1 12 Tf 0 g) >> >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [4 0 R] >>".to_string(),
            format!("<< /Type /Annot /Subtype /Widget /FT /Tx /V {value} /Rect [10 10 200 30] >>"),
            format!("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica{font_encoding} >>"),
        ])
    }

    fn generated_stream<R: Read + Seek>(pdf: &mut Pdf<R>, reference: ObjectRef) -> ObjectHandle {
        let stream = pdf.get_object_handle(reference);
        pdf.resolve_object_handle(&stream)
            .expect("resolve appearance");
        stream
    }

    fn existing_ap_with_dr_font_pdf(resources_indirect: bool) -> Vec<u8> {
        let resources = if resources_indirect { "7 0 R" } else { "<<>>" };
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R] /DR << /Font << /F1 6 0 R >> >> /DA (/F1 12 Tf 0 g) >> >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [4 0 R] >>".to_string(),
            "<< /Type /Annot /Subtype /Widget /FT /Tx /V (Hello) /Rect [10 10 200 30] /AP << /N 5 0 R >> >>".to_string(),
            format!("<< /Type /XObject /Subtype /Form /BBox [0 0 190 20] /Resources {resources} /Length 4 >>\nstream\nq Q\nendstream"),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string(),
        ];
        if resources_indirect {
            objects.push("<<>>".to_string());
        }
        pdf_with_objects(&objects)
    }

    fn replace_bytes_once(input: &mut Vec<u8>, from: &[u8], to: &[u8]) {
        let start = input
            .windows(from.len())
            .position(|window| window == from)
            .expect("fixture replacement must be present");
        input.splice(start..start + from.len(), to.iter().copied());
    }

    #[test]
    fn canonical_tx_adds_a_dr_font_to_direct_and_indirect_existing_resources() {
        for resources_indirect in [false, true] {
            let mut pdf = Pdf::open(Cursor::new(existing_ap_with_dr_font_pdf(
                resources_indirect,
            )))
            .expect("parse existing AP");
            let reference =
                render_text_field_canonical(&mut pdf, ObjectRef::new(4, 0), ObjectRef::new(4, 0))
                    .expect("generate")
                    .expect("Tx handled");
            assert_eq!(reference, ObjectRef::new(5, 0));
            let stream = generated_stream(&mut pdf, reference);
            let resources = stream
                .as_stream_dict()
                .expect("stream dictionary")
                .get_key(b"/Resources");
            pdf.resolve_object_handle(&resources)
                .expect("resolve resources");
            let fonts = resources.get_key(b"/Font");
            pdf.resolve_object_handle(&fonts).expect("resolve fonts");
            assert_eq!(
                fonts.get_key(b"/F1").object_ref(),
                Some(ObjectRef::new(6, 0))
            );
        }
    }

    #[test]
    fn canonical_tx_leaves_existing_appearance_without_resources_unchanged() {
        let mut raw = existing_ap_with_dr_font_pdf(false);
        replace_bytes_once(&mut raw, b"/Resources <<>> ", b"");
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse missing resources");
        let reference =
            render_text_field_canonical(&mut pdf, ObjectRef::new(4, 0), ObjectRef::new(4, 0))
                .expect("generate")
                .expect("Tx handled");
        assert_eq!(reference, ObjectRef::new(5, 0));
    }

    #[test]
    fn canonical_tx_uses_a_local_winansi_appearance_font() {
        let raw = pdf_with_objects(&[
            "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R] /DR <<>> /DA (/Helv 12 Tf 0 g) >> >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [4 0 R] >>".to_string(),
            "<< /Type /Annot /Subtype /Widget /FT /Tx /V (Hello) /DA (/F1 12 Tf 0 g) /Rect [10 10 200 30] /AP << /N 5 0 R >> >>".to_string(),
            "<< /Type /XObject /Subtype /Form /BBox [0 0 190 20] /Resources << /Font << /F1 6 0 R >> >> /Length 4 >>\nstream\nq Q\nendstream".to_string(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string(),
        ]);
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse local AP resource");
        let reference =
            render_text_field_canonical(&mut pdf, ObjectRef::new(4, 0), ObjectRef::new(4, 0))
                .expect("generate")
                .expect("Tx handled");
        assert_eq!(reference, ObjectRef::new(5, 0));
    }

    #[test]
    fn canonical_choice_uses_the_same_resource_lookup_boundary() {
        let raw = pdf_with_objects(&[
            "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R] /DR <<>> /DA (/Helv 12 Tf 0 g) >> >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [4 0 R] >>".to_string(),
            "<< /Type /Annot /Subtype /Widget /FT /Ch /V (Beta) /Opt [(Alpha) (Beta)] /DA (/Helv 10 Tf 0 g) /Rect [10 10 200 30] >>".to_string(),
        ]);
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse choice");
        assert!(render_choice_field_canonical(
            &mut pdf,
            ObjectRef::new(4, 0),
            ObjectRef::new(4, 0)
        )
        .expect("generate choice")
        .is_some());
    }

    #[test]
    fn appearance_token_filter_matches_qpdf_value_setter_boundary() {
        let replacement = b"/Tx BMC\nreplacement\nEMC\n";
        assert_eq!(
            run_appearance_filter(b"/Tx BMC %old\n EMC q", replacement),
            b"/Tx BMC %old\n replacement\nEMC q"
        );
        assert_eq!(
            run_appearance_filter(b"q", replacement),
            b"q/Tx BMC\nreplacement\nEMC"
        );
    }

    #[test]
    fn canonical_tx_without_dr_has_procset_only_and_no_form_type() {
        let mut pdf = Pdf::open(Cursor::new(minimal_tx_pdf())).expect("parse");
        let reference =
            render_text_field_canonical(&mut pdf, ObjectRef::new(4, 0), ObjectRef::new(4, 0))
                .expect("generate")
                .expect("Tx handled");
        let stream = generated_stream(&mut pdf, reference);
        let dict = stream.as_stream_dict().expect("stream dictionary");
        let resources = dict.get_key(b"/Resources");
        pdf.resolve_object_handle(&resources)
            .expect("resolve resources");
        assert!(resources.get_key(b"/Font").is_null());
        assert!(dict.get_key(b"/FormType").is_null());
    }

    #[test]
    fn canonical_tx_reuses_dr_font_resource() {
        let mut pdf = Pdf::open(Cursor::new(dr_font_tx_pdf(None, "(Hello)"))).expect("parse");
        let reference =
            render_text_field_canonical(&mut pdf, ObjectRef::new(4, 0), ObjectRef::new(4, 0))
                .expect("generate")
                .expect("Tx handled");
        assert_eq!(reference, ObjectRef::new(6, 0));
        let stream = generated_stream(&mut pdf, reference);
        let resources = stream
            .as_stream_dict()
            .expect("stream dictionary")
            .get_key(b"/Resources");
        pdf.resolve_object_handle(&resources)
            .expect("resolve resources");
        let fonts = resources.get_key(b"/Font");
        pdf.resolve_object_handle(&fonts).expect("resolve fonts");
        assert_eq!(
            fonts.get_key(b"/F1").object_ref(),
            Some(ObjectRef::new(5, 0))
        );
    }

    #[test]
    fn canonical_tx_reuses_the_state_selected_stream() {
        let mut pdf = Pdf::open(Cursor::new(state_appearance_pdf())).expect("parse state AP");
        let reference =
            render_text_field_canonical(&mut pdf, ObjectRef::new(4, 0), ObjectRef::new(4, 0))
                .expect("generate")
                .expect("Tx handled");
        assert_eq!(reference, ObjectRef::new(5, 0));
    }

    fn state_appearance_pdf() -> Vec<u8> {
        pdf_with_objects(&[
            "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R] /DR <<>> /DA (/Helv 12 Tf 0 g) >> >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [4 0 R] >>".to_string(),
            "<< /Type /Annot /Subtype /Widget /FT /Tx /V (Hello) /Rect [10 10 200 30] /AP << /N << /On 5 0 R >> >> /AS /On >>".to_string(),
            "<< /Type /XObject /Subtype /Form /BBox [0 0 190 20] /Length 4 >>\nstream\nq Q\nendstream".to_string(),
        ])
    }

    #[test]
    fn canonical_tx_uses_qpdf_macroman_conversion() {
        let mut pdf = Pdf::open(Cursor::new(dr_font_tx_pdf(
            Some("MacRomanEncoding"),
            "<e9>",
        )))
        .expect("parse");
        let reference =
            render_text_field_canonical(&mut pdf, ObjectRef::new(4, 0), ObjectRef::new(4, 0))
                .expect("generate")
                .expect("Tx handled");
        let stream = generated_stream(&mut pdf, reference);
        let data = stream.as_stream_data().expect("appearance data");
        assert!(data
            .windows(b"\\216".len())
            .any(|window| window == b"\\216"));
    }

    #[test]
    fn qpdf_choice_builder_ignores_quadding_and_selects_visible_rows() {
        let content = build_qpdf_choice_appearance_content(
            b"/Helv 10 Tf 0 g",
            b"B",
            &[b"A".to_vec(), b"B".to_vec(), b"C".to_vec()],
            100.0,
            36.0,
            10.0,
            false,
        );
        assert!(content
            .windows(b"0.85 0.85 0.85 rg".len())
            .any(|window| { window == b"0.85 0.85 0.85 rg" }));
        assert!(content
            .windows(b"(B) Tj".len())
            .any(|window| window == b"(B) Tj"));
    }

    #[test]
    fn substitute_da_tf_follows_qpdf_tolerance_and_missing_operator() {
        assert_eq!(
            substitute_da_tf_operand(b"/Helv 12 Tf 0 g", 12.0005),
            b"/Helv 12 Tf 0 g"
        );
        assert_eq!(
            substitute_da_tf_operand(b"/Helv 12 Tf 0 g", 11.0),
            b"/Helv 11 Tf 0 g"
        );
        assert_eq!(substitute_da_tf_operand(b"0 g", 11.0), b"0 g");
    }

    #[test]
    fn fmt_f64_matches_qpdf_numeric_formatting() {
        assert_eq!(fmt_f64(12.0), "12");
        assert_eq!(fmt_f64(1.5), "1.5");
        assert_eq!(fmt_f64(0.00001), "0");
        assert_eq!(fmt_f64(f64::NAN), "0");
    }
}
