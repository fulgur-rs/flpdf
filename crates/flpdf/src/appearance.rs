//! Appearance-stream generators for AcroForm widgets.
//!
//! This module builds the `/AP/N` (normal-appearance) Form XObject for
//! AcroForm widget annotations.  Both **Tx (text field)** and **Btn
//! (checkbox / radio / pushbutton)** appearance streams are implemented.
//! Shared helpers are exported as `pub(crate)` for use by future field-type
//! renderers (Ch list fields, etc.).
//!
//! # Observable-equivalence policy
//!
//! Appearance streams target observable equivalence with qpdf (same rendered
//! value/position), **not byte-identical output**.  Whitespace, operator
//! ordering, and auto-size heuristics may differ.
//!
//! # Limitations
//!
//! - **WinAnsi re-encoding**: Characters in the range U+0080–U+009F are
//!   represented with a `?` byte because their WinAnsi (cp1252) mappings are
//!   not implemented.  All other Latin-1 / WinAnsi characters round-trip
//!   correctly.
//! - **Comb fields** (`/Ff` bit 25 set, with `/MaxLen`): not implemented.
//!   The text is rendered as a plain single-line field.  Document "Comb
//!   layout is a known unimplemented feature" for callers.
//! - Only the 14 standard PDF fonts are supported via the embedded metrics
//!   table.  Unknown fonts fall back to Helvetica.
//! - **Btn `/MK/BG` background fill and `/MK/BC` border** are not rendered.
//!   These are best-effort decorations; callers that require them should
//!   generate the appearance themselves.

use std::collections::BTreeSet;
use std::io::{Read, Seek};

use crate::default_appearance::{parse_default_appearance, TextColor};
use crate::json_inspect::decode_pdf_text_string;
use crate::object::write_literal_string;
use crate::page_object_helper::PageBox;
use crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH;
use crate::ref_chain::resolve_ref_chain;
use crate::standard_font_metrics::StandardFont;
use crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result, Stream};

// ── Public-crate helpers ─────────────────────────────────────────────────────

/// Emit fill-colour operators for `color` into `out`.
///
/// Produces one of `g`, `rg`, or `k` (ISO 32000-1 §8.6.8) with all numeric
/// values formatted by [`fmt_f64`].
pub(crate) fn color_ops(color: &TextColor, out: &mut Vec<u8>) {
    match color {
        TextColor::Gray(g) => {
            out.extend_from_slice(fmt_f64(*g).as_bytes());
            out.extend_from_slice(b" g\n");
        }
        TextColor::Rgb(r, g, b) => {
            out.extend_from_slice(fmt_f64(*r).as_bytes());
            out.push(b' ');
            out.extend_from_slice(fmt_f64(*g).as_bytes());
            out.push(b' ');
            out.extend_from_slice(fmt_f64(*b).as_bytes());
            out.extend_from_slice(b" rg\n");
        }
        TextColor::Cmyk(c, m, y, k) => {
            out.extend_from_slice(fmt_f64(*c).as_bytes());
            out.push(b' ');
            out.extend_from_slice(fmt_f64(*m).as_bytes());
            out.push(b' ');
            out.extend_from_slice(fmt_f64(*y).as_bytes());
            out.push(b' ');
            out.extend_from_slice(fmt_f64(*k).as_bytes());
            out.extend_from_slice(b" k\n");
        }
    }
}

/// Build and install a new `/AP/N` Form XObject on the widget at `widget_ref`.
///
/// Writes two objects: the XObject stream itself (uncompressed) and optionally
/// a one-font `/Resources/Font` dictionary.  Both are inserted with
/// [`Pdf::set_object`].  The widget dictionary at `widget_ref` is updated with
/// a new `/AP` entry pointing to the XObject.
///
/// The `font_resource` tuple is `(resource_key, base_font_name, obj_ref)`:
/// - `resource_key`: the name used as the key in `/Resources/Font` and in the
///   `Tf` operator (e.g. `b"Helv"` as it appears in the `/DA` string).
/// - `base_font_name`: the official PDF BaseFont name written into the font
///   dictionary (e.g. `b"Helvetica"`). This must be a valid standard-14 name
///   so viewers select the correct built-in metrics.
/// - `obj_ref`: the pre-allocated [`ObjectRef`] for the font dictionary object.
///
/// Returns the [`ObjectRef`] of the newly-created Form XObject.
pub(crate) fn install_normal_appearance<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    widget_ref: ObjectRef,
    content: Vec<u8>,
    bbox_w: f64,
    bbox_h: f64,
    font_resource: Option<(Vec<u8>, Vec<u8>, ObjectRef)>,
) -> Result<ObjectRef> {
    // Allocate font object first (if needed) so the XObject allocation is
    // sequential and there is no number collision.
    let font_resource_ref = if let Some((ref res_key, ref base_name, font_obj_ref)) = font_resource
    {
        let mut inner_font_dict = Dictionary::new();
        inner_font_dict.insert("Type", Object::Name(b"Font".to_vec()));
        inner_font_dict.insert("Subtype", Object::Name(b"Type1".to_vec()));
        inner_font_dict.insert("BaseFont", Object::Name(base_name.clone()));
        inner_font_dict.insert("Encoding", Object::Name(b"WinAnsiEncoding".to_vec()));
        pdf.set_object(font_obj_ref, Object::Dictionary(inner_font_dict));
        let mut font_dict = Dictionary::new();
        font_dict.insert(
            String::from_utf8_lossy(res_key).into_owned(),
            Object::Reference(font_obj_ref),
        );
        Some(font_dict)
    } else {
        None
    };

    // Build the Form XObject stream dictionary.
    let xobj_ref = next_object_ref(pdf)?;

    let mut sdict = Dictionary::new();
    sdict.insert("Type", Object::Name(b"XObject".to_vec()));
    sdict.insert("Subtype", Object::Name(b"Form".to_vec()));
    sdict.insert("FormType", Object::Integer(1));
    sdict.insert(
        "BBox",
        Object::Array(vec![
            Object::Real(0.0),
            Object::Real(0.0),
            Object::Real(bbox_w),
            Object::Real(bbox_h),
        ]),
    );

    if let Some(fdict) = font_resource_ref {
        let mut resources = Dictionary::new();
        resources.insert("Font", Object::Dictionary(fdict));
        sdict.insert("Resources", Object::Dictionary(resources));
    }

    pdf.set_object(xobj_ref, Object::Stream(Stream::new(sdict, content)));

    // Update the widget's /AP/N entry.
    let widget_obj = pdf.resolve(widget_ref)?;
    let mut widget_dict = match widget_obj {
        Object::Dictionary(d) => d,
        _ => {
            return Err(Error::Unsupported(format!(
                "widget object {widget_ref} is not a dictionary"
            )))
        }
    };

    let mut ap_dict = match widget_dict.get("AP") {
        Some(Object::Dictionary(d)) => d.clone(),
        // `/AP` may be stored behind a holder chain (`ref → ref → dict`); follow
        // it to the terminal dict so a pre-existing `/AP/D`/`/AP/R` is preserved.
        Some(value @ Object::Reference(_)) => resolve_ref_chain(pdf, value)?
            .0
            .into_dict()
            .unwrap_or_default(),
        _ => Dictionary::new(),
    };

    ap_dict.insert("N", Object::Reference(xobj_ref));
    widget_dict.insert("AP", Object::Dictionary(ap_dict));
    pdf.set_object(widget_ref, Object::Dictionary(widget_dict));

    Ok(xobj_ref)
}

// ── TextAppearanceParams ─────────────────────────────────────────────────────

/// Parameters for the pure text-appearance builder [`build_text_appearance_content`].
///
/// All string data has already been decoded from PDF text-string encoding and
/// re-encoded to WinAnsi bytes.  All measurements are in user-space units.
pub(crate) struct TextAppearanceParams {
    /// WinAnsi-encoded text to render.
    pub text_bytes: Vec<u8>,
    /// Resource name for the font in the XObject `/Resources/Font` dict
    /// (same as the name used in the `Tf` operator).
    pub font_resource_name: Vec<u8>,
    /// Font size in points.
    pub font_size: f64,
    /// Text colour.
    pub color: TextColor,
    /// Width of the field bounding box.
    pub bbox_w: f64,
    /// Height of the field bounding box.
    pub bbox_h: f64,
    /// Quadding: 0 = left, 1 = centre, 2 = right.
    pub quadding: i64,
    /// Whether the field is multiline (`/Ff` bit 13).
    pub multiline: bool,
    /// Optional standard font for width measurement.  `None` → left-align
    /// (no measurement).
    pub std_font: Option<StandardFont>,
}

/// Build the raw byte content of a Tx appearance stream.
///
/// The produced stream follows the structure:
///
/// ```text
/// /Tx BMC
/// BT
/// /FontName size Tf
/// <color op>
/// <Td Tj per line>
/// ET
/// EMC
/// ```
///
/// This function is pure (no `Pdf` access) and is tested independently from
/// object allocation.
pub(crate) fn build_text_appearance_content(p: &TextAppearanceParams) -> Vec<u8> {
    let mut out = Vec::new();

    out.extend_from_slice(b"/Tx BMC\n");
    out.extend_from_slice(b"BT\n");

    // Tf operator. The resource name is a decoded PDF name, so re-escape any
    // delimiter/whitespace bytes (e.g. a `/DA` font named `/F#20A`) instead of
    // appending raw bytes — otherwise the operand stream would be malformed.
    out.push(b'/');
    crate::object::write_name_escaped(&mut out, &p.font_resource_name);
    out.push(b' ');
    out.extend_from_slice(fmt_f64(p.font_size).as_bytes());
    out.extend_from_slice(b" Tf\n");

    // Fill colour.
    color_ops(&p.color, &mut out);

    if p.multiline {
        render_multiline(p, &mut out);
    } else {
        render_singleline(p, &mut out);
    }

    out.extend_from_slice(b"ET\n");
    out.extend_from_slice(b"EMC\n");

    out
}

// ── Public document API ──────────────────────────────────────────────────────

/// Generate and install a normal appearance stream for the Tx (text) widget
/// at `widget_ref`.
///
/// Returns `Ok(Some(xobj_ref))` on success, `Ok(None)` when the widget should
/// be skipped (not a Tx field, missing /V, or degenerate bounding box).
///
/// # Errors
///
/// Returns [`Error::Unsupported`] when the field-tree depth limit is exceeded
/// or an object is structurally invalid.
///
/// # Examples
///
/// ```no_run
/// use flpdf::{generate_text_field_appearance, ObjectRef, Pdf};
/// use std::fs::File;
/// use std::io::BufReader;
///
/// let mut pdf = Pdf::open(BufReader::new(File::open("form.pdf")?))?;
/// let widget = ObjectRef::new(10, 0);
/// if let Some(ap_ref) = generate_text_field_appearance(&mut pdf, widget)? {
///     println!("appearance stream written at {ap_ref}");
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn generate_text_field_appearance<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    widget_ref: ObjectRef,
) -> Result<Option<ObjectRef>> {
    // ── 1. Verify /FT is Tx ────────────────────────────────────────────────
    let ft = resolve_inherited_name(pdf, widget_ref, b"FT")?;
    if ft.as_deref() != Some(b"Tx") {
        return Ok(None);
    }

    // ── 2. /V — field value ────────────────────────────────────────────────
    let raw_value = resolve_inherited_object(pdf, widget_ref, b"V")?;
    let value_bytes: Option<Vec<u8>> = match raw_value {
        None => None,
        Some(Object::String(bytes)) => decode_pdf_text_string(&bytes)
            .map(|s| to_winansi_bytes(&s))
            .or(Some(bytes)),
        Some(Object::Reference(r)) => {
            let obj = pdf.resolve(r)?;
            match obj {
                Object::String(bytes) => decode_pdf_text_string(&bytes)
                    .map(|s| to_winansi_bytes(&s))
                    .or(Some(bytes)),
                _ => None,
            }
        }
        _ => None,
    };

    // Distinguish a missing `/V` (nothing to render — leave any inherited or
    // pre-existing appearance alone) from a present-but-empty `/V` (the field
    // was explicitly cleared). An empty value must still produce a blank
    // appearance stream so a cleared field stops rendering its stale `/AP/N`.
    let text_bytes = match value_bytes {
        Some(b) => b,
        None => return Ok(None),
    };

    // ── 3. /Rect — bounding box ────────────────────────────────────────────
    let rect = resolve_rect(pdf, widget_ref)?;
    let (bbox_w, bbox_h) = match rect {
        Some(r) => {
            let w = (r.urx - r.llx).abs();
            let h = (r.ury - r.lly).abs();
            if !w.is_finite() || !h.is_finite() || w < 1.0 || h < 1.0 {
                return Ok(None);
            }
            (w, h)
        }
        None => return Ok(None),
    };

    // ── 4. /DA — default appearance ───────────────────────────────────────
    // Walk /Parent chain first; if absent, fall back to /AcroForm /DA.
    let da_bytes = resolve_da(pdf, widget_ref)?;
    let da = parse_default_appearance(da_bytes.as_deref().unwrap_or(b""));

    // ── 5. /Q — quadding (0 = left, 1 = centre, 2 = right) ───────────────
    let quadding = resolve_inherited_integer(pdf, widget_ref, b"Q")?.unwrap_or(0);

    // ── 6. /Ff — field flags (bit 13 = multiline, 0-indexed) ─────────────
    // A negative or out-of-range /Ff is malformed; treat it as "no flags". A
    // bare `as u32` would wrap a negative value to a large unsigned int and
    // could spuriously set the multiline bit (review pattern #3).
    let ff =
        u32::try_from(resolve_inherited_integer(pdf, widget_ref, b"Ff")?.unwrap_or(0)).unwrap_or(0);
    let multiline = (ff >> 12) & 1 != 0; // bit 13 (1-indexed) = bit 12 (0-indexed)

    // ── 7. Font resolution — DA font name → standard font ─────────────────
    // The /DA name may be (a) a direct standard-font alias (e.g. "Helv"), or
    // (b) a /DR resource key (e.g. "F1") whose /BaseFont is a standard font.
    // Fall back to Helvetica only when neither resolves.
    let font_name_bytes: Vec<u8> = da.font_name.clone().unwrap_or_else(|| b"Helv".to_vec());

    let std_font = match StandardFont::from_base_name(&font_name_bytes) {
        Some(sf) => sf,
        None => lookup_dr_basefont(pdf, widget_ref, &font_name_bytes)?
            .unwrap_or(StandardFont::Helvetica),
    };
    let base_font_name = official_base_name(std_font).to_vec();
    let font_obj_ref = next_object_ref(pdf)?;

    // ── 8. Font size (auto-size heuristic) ────────────────────────────────
    let font_size = if da.auto_size {
        // Single-line auto: fit height with small top/bottom padding.
        let candidate = (bbox_h - 2.0).clamp(4.0, 12.0);
        // For multiline use the same heuristic; the rendering loop handles
        // overflow by just clipping at the bottom.
        candidate
    } else {
        da.font_size
    };

    // ── 9. Resource name — the Tf operator and the synthesized
    //       /Resources/Font key both use the name exactly as /DA wrote it
    //       (e.g. "Helv" or "F1"), which we map to the standard font dict
    //       installed below.
    let font_resource_name: Vec<u8> = font_name_bytes.clone();

    // ── 10. Build content stream ───────────────────────────────────────────
    let params = TextAppearanceParams {
        text_bytes,
        font_resource_name: font_resource_name.clone(),
        font_size,
        color: da.color,
        bbox_w,
        bbox_h,
        quadding,
        multiline,
        std_font: Some(std_font),
    };
    let content = build_text_appearance_content(&params);

    // ── 11. Install ────────────────────────────────────────────────────────
    // Pass three names: resource key (from /DA, e.g. "Helv"), official
    // BaseFont name (e.g. "Helvetica"), and the pre-allocated ObjectRef.
    let xobj_ref = install_normal_appearance(
        pdf,
        widget_ref,
        content,
        bbox_w,
        bbox_h,
        Some((font_resource_name, base_font_name, font_obj_ref)),
    )?;

    Ok(Some(xobj_ref))
}

// ── Btn appearance ────────────────────────────────────────────────────────────

/// Generate and install appearance streams for a Btn (button) widget.
///
/// Handles three Btn sub-types determined by `/Ff` bit flags
/// (ISO 32000-1, Table 226):
///
/// - **Pushbutton** (bit 17 set, `0x10000`): renders the `/MK/CA` caption
///   centred using Helvetica.  Installs a single `/AP/N` Form XObject.
/// - **Radio** (bit 16 set, `0x8000`, bit 17 clear): renders on/off state
///   appearances with ZapfDingbats glyph `l` (U+006C, bullet) as the on
///   indicator.
/// - **Checkbox** (neither bit set): renders on/off state appearances with
///   ZapfDingbats glyph `4` (U+0034, check mark ✔) as the on indicator.
///
/// Returns `Ok(Some(on_ref))` (the on-state XObject reference) for
/// checkbox/radio, `Ok(Some(ap_ref))` for pushbutton, and `Ok(None)` when
/// the widget should be skipped (not a Btn field, degenerate bounding box).
///
/// # Observable-equivalence caveat
///
/// On/off glyph positioning and caption centering target **observable
/// equivalence** (same rendered appearance) with standard viewers such as
/// qpdf / Acrobat.  Byte-identical output is not a goal; whitespace,
/// operator order, and layout heuristics may differ.
///
/// # Errors
///
/// Returns [`Error::Unsupported`] when the field-tree depth limit is
/// exceeded or an object is structurally invalid.
///
/// # Examples
///
/// ```no_run
/// use flpdf::{generate_button_field_appearance, ObjectRef, Pdf};
/// use std::fs::File;
/// use std::io::BufReader;
///
/// let mut pdf = Pdf::open(BufReader::new(File::open("form.pdf")?))?;
/// let widget = ObjectRef::new(10, 0);
/// if let Some(ap_ref) = generate_button_field_appearance(&mut pdf, widget)? {
///     println!("button appearance stream written at {ap_ref}");
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn generate_button_field_appearance<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    widget_ref: ObjectRef,
) -> Result<Option<ObjectRef>> {
    // ── 1. /FT must be Btn ────────────────────────────────────────────────
    let ft = resolve_inherited_name(pdf, widget_ref, b"FT")?;
    if ft.as_deref() != Some(b"Btn") {
        return Ok(None);
    }

    // ── 2. /Ff — field flags ──────────────────────────────────────────────
    // A negative or out-of-range /Ff is malformed; treat it as "no flags".
    // Testing bits on a raw i64 would let `/Ff -1` (all bits set) read as both
    // pushbutton and radio, mis-classifying a broken field instead of falling
    // back to the safe checkbox path (review pattern #3).
    let ff =
        u32::try_from(resolve_inherited_integer(pdf, widget_ref, b"Ff")?.unwrap_or(0)).unwrap_or(0);
    let is_pushbutton = ff & 0x10000 != 0; // bit 17 (1-indexed)
    let is_radio = !is_pushbutton && ff & 0x8000 != 0; // bit 16

    // ── 3. /Rect — bounding box ───────────────────────────────────────────
    let rect = resolve_rect(pdf, widget_ref)?;
    let (bbox_w, bbox_h) = match rect {
        Some(r) => {
            let w = (r.urx - r.llx).abs();
            let h = (r.ury - r.lly).abs();
            if !w.is_finite() || !h.is_finite() || w < 1.0 || h < 1.0 {
                return Ok(None);
            }
            (w, h)
        }
        None => return Ok(None),
    };

    // ── 4. /MK — appearance characteristics (widget-direct, no inherit) ───
    // Must resolve possible indirect reference before extracting entries
    // (review-pattern #2).
    let mk_dict: Option<Dictionary> = {
        let widget_obj = pdf.resolve_borrowed(widget_ref)?;
        let mk_val = widget_obj.as_dict().and_then(|d| d.get("MK").cloned());
        let _ = widget_obj;
        resolve_to_dict(pdf, mk_val)?
    };

    if is_pushbutton {
        generate_pushbutton_appearance(pdf, widget_ref, bbox_w, bbox_h, mk_dict.as_ref())
    } else {
        generate_checkbox_radio_appearance(
            pdf,
            widget_ref,
            bbox_w,
            bbox_h,
            mk_dict.as_ref(),
            is_radio,
        )
    }
}

/// Pushbutton appearance: render `/MK/CA` caption centred in the bbox.
fn generate_pushbutton_appearance<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    widget_ref: ObjectRef,
    bbox_w: f64,
    bbox_h: f64,
    mk_dict: Option<&Dictionary>,
) -> Result<Option<ObjectRef>> {
    // Extract caption from /MK/CA. It may be a String or an indirect reference
    // to one; resolve the reference first (review pattern #2) so the decode path
    // is not duplicated across the direct and indirect cases.
    let caption_obj = match mk_dict.and_then(|d| d.get("CA").cloned()) {
        Some(Object::Reference(r)) => Some(pdf.resolve(r)?),
        other => other,
    };
    let caption_bytes: Vec<u8> = match caption_obj {
        Some(Object::String(s)) => decode_pdf_text_string(&s)
            .map(|us| to_winansi_bytes(&us))
            .unwrap_or(s),
        _ => Vec::new(),
    };

    // Auto font size: fit height with small padding.
    let font_size = (bbox_h - 4.0).clamp(4.0, 12.0);

    let params = TextAppearanceParams {
        text_bytes: caption_bytes,
        font_resource_name: b"Helv".to_vec(),
        font_size,
        color: TextColor::Gray(0.0),
        bbox_w,
        bbox_h,
        quadding: 1, // centre
        multiline: false,
        std_font: Some(StandardFont::Helvetica),
    };
    let content = build_text_appearance_content(&params);

    let font_obj_ref = next_object_ref(pdf)?;
    let xobj_ref = install_normal_appearance(
        pdf,
        widget_ref,
        content,
        bbox_w,
        bbox_h,
        Some((b"Helv".to_vec(), b"Helvetica".to_vec(), font_obj_ref)),
    )?;

    Ok(Some(xobj_ref))
}

/// Checkbox / radio appearance: build on + off XObjects and install as
/// `/AP` << `/N` << `/<on>` `/<off>` >> `/D` << ... >> >>.
fn generate_checkbox_radio_appearance<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    widget_ref: ObjectRef,
    bbox_w: f64,
    bbox_h: f64,
    mk_dict: Option<&Dictionary>,
    is_radio: bool,
) -> Result<Option<ObjectRef>> {
    // ── 4a. Determine on-state name ────────────────────────────────────────
    // Priority: existing /AP/N dict key other than Off → /AS(Name) → "Yes".
    // /AP and /AS must be resolved from indirect references (rule #2).
    let on_state_name: Vec<u8> = {
        // Read the widget dict once so we can inspect /AP and /AS.
        let widget_obj = pdf.resolve_borrowed(widget_ref)?;
        let wdict = widget_obj.as_dict();

        // Try /AP/N dict keys.
        let ap_val = wdict.and_then(|d| d.get("AP").cloned());
        let as_val = wdict.and_then(|d| d.get("AS").cloned());
        let _ = widget_obj;

        // Resolve /AP (may be indirect).
        let ap_dict = resolve_to_dict(pdf, ap_val)?;
        let n_dict = if let Some(ap) = ap_dict {
            let n_val = ap.get("N").cloned();
            resolve_to_dict(pdf, n_val)?
        } else {
            None
        };

        // Pick first key that is not "Off".
        let from_ap = n_dict.and_then(|d| {
            d.iter()
                .find(|(k, _)| *k != b"Off")
                .map(|(k, _)| k.to_vec())
        });

        if let Some(name) = from_ap {
            name
        } else {
            // Resolve /AS (may be indirect ref).
            let as_resolved = match as_val {
                Some(Object::Reference(r)) => Some(pdf.resolve(r)?),
                other => other,
            };
            match as_resolved {
                Some(Object::Name(n)) if n != b"Off" => n,
                _ => b"Yes".to_vec(),
            }
        }
    };

    // ── 4b. CA glyph from /MK/CA ──────────────────────────────────────────
    // May be a String or an indirect reference to one; resolve first (review
    // pattern #2) so the direct and indirect cases share one match arm.
    let default_glyph: &[u8] = if is_radio { b"l" } else { b"4" };
    let ca_obj = match mk_dict.and_then(|d| d.get("CA").cloned()) {
        Some(Object::Reference(r)) => Some(pdf.resolve(r)?),
        other => other,
    };
    let ca_bytes: Vec<u8> = match ca_obj {
        Some(Object::String(s)) => {
            // /MK/CA is a PDF text string naming a ZapfDingbats glyph, and the
            // appearance font below is /ZaDb — so the byte(s) directly select the
            // glyph and must NOT be WinAnsi-remapped the way the pushbutton
            // caption is. A UTF-16BE wrapper (BOM) must still be decoded, or its
            // `FE FF` bytes would leak into the Tj as garbage; a plain string is
            // already the ZapfDingbats code byte(s) and is used verbatim.
            if s.starts_with(&[0xFE, 0xFF]) {
                match decode_pdf_text_string(&s) {
                    Some(text) if !text.is_empty() && text.chars().all(|c| (c as u32) <= 0xFF) => {
                        text.chars().map(|c| c as u8).collect()
                    }
                    // Empty after the BOM, or a char outside the single-byte
                    // ZapfDingbats code range: no usable glyph → default.
                    _ => default_glyph.to_vec(),
                }
            } else if s.is_empty() {
                default_glyph.to_vec()
            } else {
                s
            }
        }
        _ => default_glyph.to_vec(),
    };

    // ── 4c. Build on / off content streams ────────────────────────────────
    //
    // Glyph size: fit within min(w, h), clamp to reasonable range.
    // Approximate ZapfDingbats centering heuristics (observable-equivalence):
    //   tx ≈ (w - size * 0.7) / 2   (glyph advance width ≈ 70% of em)
    //   ty ≈ (h - size) / 2 + size * 0.15  (descender / baseline offset)
    // Exact metrics are not available for ZapfDingbats in this build; the
    // approximation targets the same visual result as standard viewers.
    let size = (bbox_w.min(bbox_h) * 0.8).clamp(4.0, 72.0);
    let tx = ((bbox_w - size * 0.7) / 2.0).max(0.0);
    let ty = ((bbox_h - size) / 2.0 + size * 0.15).max(0.0);

    let mut on_content = Vec::new();
    on_content.extend_from_slice(b"q\nBT\n/ZaDb ");
    on_content.extend_from_slice(fmt_f64(size).as_bytes());
    on_content.extend_from_slice(b" Tf\n0 g\n");
    on_content.extend_from_slice(fmt_f64(tx).as_bytes());
    on_content.push(b' ');
    on_content.extend_from_slice(fmt_f64(ty).as_bytes());
    on_content.extend_from_slice(b" Td\n");
    write_literal_string(&mut on_content, &ca_bytes);
    on_content.extend_from_slice(b" Tj\nET\nQ\n");

    let off_content: Vec<u8> = b"q Q\n".to_vec();

    // ── 5. Install state appearances ──────────────────────────────────────
    install_state_appearances(
        pdf,
        widget_ref,
        &on_state_name,
        on_content,
        off_content,
        bbox_w,
        bbox_h,
    )
}

/// Install on + off XObjects as `/AP` << `/N` << ... >> `/D` << ... >> >>
/// on the widget, and set `/AS /Off` when no `/AS` is present.
///
/// **Object allocation order is critical**: each `set_object` call must
/// immediately follow the matching `next_object_ref` call so that the max
/// object number advances before the next allocation.  Collecting all refs
/// up front would yield duplicates because `next_object_ref` reads the
/// current max with no reservation.
///
/// Returns the `ObjectRef` of the on-state XObject.
fn install_state_appearances<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    widget_ref: ObjectRef,
    on_state_name: &[u8],
    on_content: Vec<u8>,
    off_content: Vec<u8>,
    bbox_w: f64,
    bbox_h: f64,
) -> Result<Option<ObjectRef>> {
    // ── Allocate + install ZapfDingbats font dict ─────────────────────────
    // ZapfDingbats does NOT use WinAnsiEncoding; the font dict is hand-built
    // without /Encoding so viewers apply the font's built-in encoding.
    let font_ref = next_object_ref(pdf)?;
    {
        let mut fd = Dictionary::new();
        fd.insert("Type", Object::Name(b"Font".to_vec()));
        fd.insert("Subtype", Object::Name(b"Type1".to_vec()));
        fd.insert("BaseFont", Object::Name(b"ZapfDingbats".to_vec()));
        pdf.set_object(font_ref, Object::Dictionary(fd));
    }

    // ── Build /Resources for XObjects ────────────────────────────────────
    let make_resources = || -> Dictionary {
        let mut font_dict = Dictionary::new();
        font_dict.insert("ZaDb", Object::Reference(font_ref));
        let mut res = Dictionary::new();
        res.insert("Font", Object::Dictionary(font_dict));
        res
    };

    // ── Allocate + install on-state XObject ───────────────────────────────
    let on_ref = next_object_ref(pdf)?;
    {
        let mut sdict = Dictionary::new();
        sdict.insert("Type", Object::Name(b"XObject".to_vec()));
        sdict.insert("Subtype", Object::Name(b"Form".to_vec()));
        sdict.insert("FormType", Object::Integer(1));
        sdict.insert(
            "BBox",
            Object::Array(vec![
                Object::Real(0.0),
                Object::Real(0.0),
                Object::Real(bbox_w),
                Object::Real(bbox_h),
            ]),
        );
        sdict.insert("Resources", Object::Dictionary(make_resources()));
        pdf.set_object(on_ref, Object::Stream(Stream::new(sdict, on_content)));
    }

    // ── Allocate + install off-state XObject ──────────────────────────────
    let off_ref = next_object_ref(pdf)?;
    {
        let mut sdict = Dictionary::new();
        sdict.insert("Type", Object::Name(b"XObject".to_vec()));
        sdict.insert("Subtype", Object::Name(b"Form".to_vec()));
        sdict.insert("FormType", Object::Integer(1));
        sdict.insert(
            "BBox",
            Object::Array(vec![
                Object::Real(0.0),
                Object::Real(0.0),
                Object::Real(bbox_w),
                Object::Real(bbox_h),
            ]),
        );
        sdict.insert("Resources", Object::Dictionary(make_resources()));
        pdf.set_object(off_ref, Object::Stream(Stream::new(sdict, off_content)));
    }

    // ── Build /AP << /N << ... >> /D << ... >> >> ─────────────────────────
    // `Dictionary::insert` takes `impl AsRef<[u8]>`, so pass the raw on-state
    // name bytes directly — no String::from_utf8_lossy round-trip (which would
    // both allocate and silently corrupt any non-UTF-8 name bytes).
    let build_state_dict = |on: ObjectRef, off: ObjectRef| -> Dictionary {
        let mut d = Dictionary::new();
        d.insert(on_state_name, Object::Reference(on));
        d.insert("Off", Object::Reference(off));
        d
    };

    let mut ap = Dictionary::new();
    ap.insert("N", Object::Dictionary(build_state_dict(on_ref, off_ref)));
    ap.insert("D", Object::Dictionary(build_state_dict(on_ref, off_ref)));

    // ── Update widget dict ─────────────────────────────────────────────────
    let widget_obj = pdf.resolve(widget_ref)?;
    let mut wdict = match widget_obj {
        Object::Dictionary(d) => d,
        _ => {
            return Err(Error::Unsupported(format!(
                "widget object {widget_ref} is not a dictionary"
            )))
        }
    };

    wdict.insert("AP", Object::Dictionary(ap));

    // Set /AS to /Off when not already present (default off display).
    if wdict.get("AS").is_none() {
        wdict.insert("AS", Object::Name(b"Off".to_vec()));
    }

    pdf.set_object(widget_ref, Object::Dictionary(wdict));

    Ok(Some(on_ref))
}

// ── Private helpers ──────────────────────────────────────────────────────────

/// Return the official PDF BaseFont name for a [`StandardFont`].
fn official_base_name(font: StandardFont) -> &'static [u8] {
    match font {
        StandardFont::Helvetica => b"Helvetica",
        StandardFont::HelveticaBold => b"Helvetica-Bold",
        StandardFont::HelveticaOblique => b"Helvetica-Oblique",
        StandardFont::HelveticaBoldOblique => b"Helvetica-BoldOblique",
        StandardFont::TimesRoman => b"Times-Roman",
        StandardFont::TimesBold => b"Times-Bold",
        StandardFont::TimesItalic => b"Times-Italic",
        StandardFont::TimesBoldItalic => b"Times-BoldItalic",
        StandardFont::Courier => b"Courier",
        StandardFont::CourierBold => b"Courier-Bold",
        StandardFont::CourierOblique => b"Courier-Oblique",
        StandardFont::CourierBoldOblique => b"Courier-BoldOblique",
        StandardFont::Symbol => b"Symbol",
        StandardFont::ZapfDingbats => b"ZapfDingbats",
    }
}

/// Format an `f64` for use in a PDF content stream (locale-independent).
///
/// Trailing zeros after the decimal point are stripped; integers are emitted
/// without a decimal point.
fn fmt_f64(v: f64) -> String {
    if !v.is_finite() {
        return "0".to_string();
    }
    // Round to 4 decimal places — sufficient for point coordinates.
    let s = format!("{:.4}", v);
    let s = s.trim_end_matches('0');
    let s = s.trim_end_matches('.');
    if s.is_empty() || s == "-" {
        return "0".to_string();
    }
    s.to_string()
}

/// Re-encode a Rust `&str` to WinAnsi bytes (Latin-1 / cp1252 subset).
///
/// Characters U+0000–U+007F and U+00A0–U+00FF map directly (the WinAnsi byte
/// equals the code point value).  Characters U+0080–U+009F are replaced with
/// `b'?'` (known limitation documented in the module doc).  Characters outside
/// U+00FF are replaced with `b'?'`.
fn to_winansi_bytes(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for ch in s.chars() {
        let code = ch as u32;
        if code <= 0x007F || (0x00A0..=0x00FF).contains(&code) {
            // ASCII and Latin-1 share code points with WinAnsi (CP1252).
            out.push(code as u8);
        } else {
            // The CP1252 0x80–0x9F range holds common typographic characters
            // (Euro, smart quotes, dashes, ellipsis, bullet, …) that standard
            // PDF fonts render. Map them to their CP1252 byte rather than `?`.
            // Undefined CP1252 slots (0x81/0x8D/0x8F/0x90/0x9D) fall through to `?`.
            let byte = match ch {
                '\u{20AC}' => 0x80, // €  Euro Sign
                '\u{201A}' => 0x82, // ‚  Single Low-9 Quotation Mark
                '\u{0192}' => 0x83, // ƒ  Latin Small Letter F With Hook
                '\u{201E}' => 0x84, // „  Double Low-9 Quotation Mark
                '\u{2026}' => 0x85, // …  Horizontal Ellipsis
                '\u{2020}' => 0x86, // †  Dagger
                '\u{2021}' => 0x87, // ‡  Double Dagger
                '\u{02C6}' => 0x88, // ˆ  Modifier Letter Circumflex Accent
                '\u{2030}' => 0x89, // ‰  Per Mille Sign
                '\u{0160}' => 0x8A, // Š  Latin Capital Letter S With Caron
                '\u{2039}' => 0x8B, // ‹  Single Left-Pointing Angle Quotation Mark
                '\u{0152}' => 0x8C, // Œ  Latin Capital Ligature OE
                '\u{017D}' => 0x8E, // Ž  Latin Capital Letter Z With Caron
                '\u{2018}' => 0x91, // '  Left Single Quotation Mark
                '\u{2019}' => 0x92, // '  Right Single Quotation Mark
                '\u{201C}' => 0x93, // "  Left Double Quotation Mark
                '\u{201D}' => 0x94, // "  Right Double Quotation Mark
                '\u{2022}' => 0x95, // •  Bullet
                '\u{2013}' => 0x96, // –  En Dash
                '\u{2014}' => 0x97, // —  Em Dash
                '\u{02DC}' => 0x98, // ˜  Small Tilde
                '\u{2122}' => 0x99, // ™  Trade Mark Sign
                '\u{0161}' => 0x9A, // š  Latin Small Letter S With Caron
                '\u{203A}' => 0x9B, // ›  Single Right-Pointing Angle Quotation Mark
                '\u{0153}' => 0x9C, // œ  Latin Small Ligature OE
                '\u{017E}' => 0x9E, // ž  Latin Small Letter Z With Caron
                '\u{0178}' => 0x9F, // Ÿ  Latin Capital Letter Y With Dieresis
                _ => b'?',
            };
            out.push(byte);
        }
    }
    out
}

/// Allocate the next available object reference.
fn next_object_ref<R: Read + Seek>(pdf: &Pdf<R>) -> Result<ObjectRef> {
    let n = pdf
        .object_refs()
        .iter()
        .map(|r| r.number)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| Error::Unsupported("object-number space exhausted".to_string()))?;
    Ok(ObjectRef::new(n, 0))
}

/// Walk the `/Parent` chain looking for a `Name` value for `key`.
fn resolve_inherited_name<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    start: ObjectRef,
    key: &[u8],
) -> Result<Option<Vec<u8>>> {
    let mut seen: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut current = start;
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

        let node_obj = pdf.resolve_borrowed(current)?;
        let Some(dict) = node_obj.as_dict() else {
            return Err(Error::Unsupported(format!(
                "field tree node {current} is not a dictionary"
            )));
        };

        let val = dict.get(key).cloned();
        let parent_val = dict.get("Parent").cloned();
        // The two `.cloned()` calls above copy what we need; the `node_obj`
        // borrow ends here so `pdf.resolve` can run below.
        let _ = node_obj;

        if let Some(val) = val {
            // The value may itself be an indirect reference (review pattern #2);
            // resolve it before matching the type, otherwise an indirect `/FT`
            // would be missed and the field skipped.
            let resolved = match val {
                Object::Reference(r) => pdf.resolve(r)?,
                other => other,
            };
            match resolved {
                Object::Null => {}
                Object::Name(bytes) => return Ok(Some(bytes)),
                _ => {}
            }
        }

        match parent_val {
            Some(Object::Reference(r)) => {
                current = r;
                depth += 1;
            }
            _ => return Ok(None),
        }
    }
}

/// Walk the `/Parent` chain looking for an arbitrary `Object` value for `key`.
fn resolve_inherited_object<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    start: ObjectRef,
    key: &[u8],
) -> Result<Option<Object>> {
    let mut seen: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut current = start;
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

        let node_obj = pdf.resolve_borrowed(current)?;
        let Some(dict) = node_obj.as_dict() else {
            return Err(Error::Unsupported(format!(
                "field tree node {current} is not a dictionary"
            )));
        };

        if let Some(val) = dict.get(key).cloned() {
            match val {
                Object::Null => {}
                other => return Ok(Some(other)),
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

/// Walk the `/Parent` chain looking for an `Integer` value for `key`.
fn resolve_inherited_integer<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    start: ObjectRef,
    key: &[u8],
) -> Result<Option<i64>> {
    let mut seen: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut current = start;
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

        let node_obj = pdf.resolve_borrowed(current)?;
        let Some(dict) = node_obj.as_dict() else {
            return Err(Error::Unsupported(format!(
                "field tree node {current} is not a dictionary"
            )));
        };

        let val = dict.get(key).cloned();
        let parent_val = dict.get("Parent").cloned();
        // Values cloned above; release the `node_obj` borrow before `pdf.resolve`.
        let _ = node_obj;

        if let Some(val) = val {
            // Inherited integer properties such as `/Ff` (field flags) or `/Q`
            // (quadding) may be stored as indirect references (review pattern #2);
            // resolve before matching so they are not missed (which would fall
            // back to single-line / left-aligned defaults).
            let resolved = match val {
                Object::Reference(r) => pdf.resolve(r)?,
                other => other,
            };
            match resolved {
                Object::Null => {}
                Object::Integer(n) => return Ok(Some(n)),
                _ => {}
            }
        }

        match parent_val {
            Some(Object::Reference(r)) => {
                current = r;
                depth += 1;
            }
            _ => return Ok(None),
        }
    }
}

/// Look up `/DA` (default appearance string) by walking the `/Parent` chain
/// first, then falling back to `/AcroForm/DA` at the document level.
///
/// Returns `None` when no `/DA` is found anywhere in the chain or in
/// `/AcroForm`.
fn resolve_da<R: Read + Seek>(pdf: &mut Pdf<R>, start: ObjectRef) -> Result<Option<Vec<u8>>> {
    // Walk /Parent chain for /DA first.
    let mut seen: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut current = start;
    let mut depth: usize = 0;

    loop {
        // Malformed parent chains (over-deep, cyclic, non-dictionary node) are
        // not fatal: stop walking, warn, and fall back to /AcroForm /DA — the
        // same graceful degradation qpdf performs (it warns and continues rather
        // than aborting on a broken field tree). The depth limit + `seen` set
        // also bound traversal per review pattern #4 (DoS).
        if depth >= DEFAULT_MAX_PAGE_TREE_DEPTH {
            pdf.push_warning(format!(
                "/DA inheritance: parent chain exceeded max depth {DEFAULT_MAX_PAGE_TREE_DEPTH} at {current}; falling back to /AcroForm /DA"
            ));
            break;
        }
        if !seen.insert(current) {
            pdf.push_warning(format!(
                "/DA inheritance: cycle detected at {current}; falling back to /AcroForm /DA"
            ));
            break;
        }

        let node_obj = pdf.resolve_borrowed(current)?;
        // Extract the values we need while the borrow is live, then drop it so
        // `pdf` is free for `push_warning` / `resolve` below.
        let extracted = node_obj
            .as_dict()
            .map(|dict| (dict.get("DA").cloned(), dict.get("Parent").cloned()));
        let _ = node_obj;
        let Some((da_val, parent_val)) = extracted else {
            pdf.push_warning(format!(
                "/DA inheritance: /Parent node {current} is not a dictionary; falling back to /AcroForm /DA"
            ));
            break;
        };

        if let Some(val) = da_val {
            // /DA may itself be an indirect reference (rule #2 in review patterns).
            let resolved_val = match val {
                Object::Reference(r) => pdf.resolve(r)?,
                other => other,
            };
            match resolved_val {
                Object::Null => {}
                Object::String(bytes) => return Ok(Some(bytes)),
                _ => {}
            }
        }

        match parent_val {
            // No /Parent — reached the field-tree root. Normal termination, not
            // an anomaly, so no warning.
            Some(Object::Reference(r)) => {
                current = r;
                depth += 1;
            }
            _ => break,
        }
    }

    // Fallback: /AcroForm /DA at document root.
    let Some(root_ref) = pdf.root_ref() else {
        return Ok(None);
    };
    let catalog_obj = pdf.resolve_borrowed(root_ref)?;
    let Some(catalog) = catalog_obj.as_dict() else {
        return Ok(None);
    };
    let acroform_val = catalog.get("AcroForm").cloned();

    let acroform_dict: Option<Dictionary> = match acroform_val {
        None | Some(Object::Null) => None,
        Some(Object::Dictionary(d)) => Some(d),
        // `/AcroForm` may be stored behind a holder chain; follow it to the
        // terminal dict so the document-level `/DA` fallback is found.
        Some(Object::Reference(r)) => resolve_ref_chain(pdf, &Object::Reference(r))?.0.into_dict(),
        _ => None,
    };

    // /AcroForm /DA may also be an indirect reference.
    let da_raw = acroform_dict.as_ref().and_then(|d| d.get("DA")).cloned();

    let da = match da_raw {
        None | Some(Object::Null) => None,
        Some(Object::String(bytes)) => Some(bytes),
        Some(Object::Reference(r)) => {
            let resolved = pdf.resolve(r)?;
            match resolved {
                Object::String(bytes) => Some(bytes),
                _ => None,
            }
        }
        _ => None,
    };

    Ok(da)
}

/// Resolve a (possibly indirect) dictionary-valued entry to an owned
/// [`Dictionary`], returning `None` for absent/null/non-dict values.
fn resolve_to_dict<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    value: Option<Object>,
) -> Result<Option<Dictionary>> {
    match value {
        None | Some(Object::Null) => Ok(None),
        Some(Object::Dictionary(d)) => Ok(Some(d)),
        // The value may be stored behind a holder chain (`ref → ref → dict`);
        // follow it to the terminal so a doubled-indirect container is not lost.
        Some(Object::Reference(r)) => {
            Ok(resolve_ref_chain(pdf, &Object::Reference(r))?.0.into_dict())
        }
        Some(_) => Ok(None),
    }
}

/// Resolve a `/DA` font resource name (e.g. `b"F1"`) to a [`StandardFont`] by
/// consulting the `/DR` `/Font` resource dictionaries.
///
/// `/DA` references a font by the resource key it carries in `/DR` (the field's
/// own `/DR`, walked up the `/Parent` chain, then the `/AcroForm` `/DR`). The
/// referenced font's `/BaseFont` may name a standard-14 font even when the
/// resource key itself (`F1`) is not a recognised alias. Returns the standard
/// font when `/BaseFont` resolves to one, else `None`.
///
/// All intermediate values are resolved through indirect references
/// (review-pattern #2): `/DR`, `/Font`, the font resource, and `/BaseFont` can
/// each be stored indirectly.
fn lookup_dr_basefont<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    start: ObjectRef,
    resource_name: &[u8],
) -> Result<Option<StandardFont>> {
    // Collect candidate /DR dictionaries: the field /Parent chain first
    // (most specific), then the document /AcroForm /DR.
    let mut dr_candidates: Vec<Dictionary> = Vec::new();

    let mut seen: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut current = start;
    let mut depth: usize = 0;
    loop {
        // Same graceful-degradation policy as resolve_da: warn and fall back to
        // /AcroForm /DR (and ultimately the default font) on an over-deep,
        // cyclic, or non-dictionary parent chain rather than aborting.
        if depth >= DEFAULT_MAX_PAGE_TREE_DEPTH {
            pdf.push_warning(format!(
                "/DR font lookup: parent chain exceeded max depth {DEFAULT_MAX_PAGE_TREE_DEPTH} at {current}; falling back to /AcroForm /DR"
            ));
            break;
        }
        if !seen.insert(current) {
            pdf.push_warning(format!(
                "/DR font lookup: cycle detected at {current}; falling back to /AcroForm /DR"
            ));
            break;
        }
        let node = pdf.resolve_borrowed(current)?;
        let extracted = node
            .as_dict()
            .map(|dict| (dict.get("DR").cloned(), dict.get("Parent").cloned()));
        let _ = node;
        let Some((dr_val, parent_val)) = extracted else {
            pdf.push_warning(format!(
                "/DR font lookup: /Parent node {current} is not a dictionary; falling back to /AcroForm /DR"
            ));
            break;
        };
        if let Some(dr) = resolve_to_dict(pdf, dr_val)? {
            dr_candidates.push(dr);
        }
        match parent_val {
            Some(Object::Reference(r)) => {
                current = r;
                depth += 1;
            }
            _ => break,
        }
    }

    if let Some(root_ref) = pdf.root_ref() {
        let catalog = pdf.resolve_borrowed(root_ref)?;
        let acroform_val = catalog.as_dict().and_then(|c| c.get("AcroForm").cloned());
        let _ = catalog;
        if let Some(acroform) = resolve_to_dict(pdf, acroform_val)? {
            let dr_val = acroform.get("DR").cloned();
            if let Some(dr) = resolve_to_dict(pdf, dr_val)? {
                dr_candidates.push(dr);
            }
        }
    }

    for dr in dr_candidates {
        let font_val = dr.get("Font").cloned();
        let Some(font_dict) = resolve_to_dict(pdf, font_val)? else {
            continue;
        };
        let res_val = font_dict.get(resource_name).cloned();
        let Some(res) = resolve_to_dict(pdf, res_val)? else {
            continue;
        };
        let base_font = match res.get("BaseFont").cloned() {
            Some(Object::Name(n)) => Some(n),
            Some(Object::Reference(r)) => pdf.resolve(r)?.into_name(),
            _ => None,
        };
        if let Some(bf) = base_font {
            if let Some(sf) = StandardFont::from_base_name(&bf) {
                return Ok(Some(sf));
            }
        }
    }

    Ok(None)
}

/// Extract the `/Rect` of the widget as a [`PageBox`].
fn resolve_rect<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    widget_ref: ObjectRef,
) -> Result<Option<PageBox>> {
    let obj = pdf.resolve_borrowed(widget_ref)?;
    let Some(dict) = obj.as_dict() else {
        return Ok(None);
    };
    let rect_val = match dict.get("Rect").cloned() {
        None | Some(Object::Null) => return Ok(None),
        Some(v) => v,
    };

    let arr = match rect_val {
        Object::Array(a) => a,
        Object::Reference(r) => {
            let resolved = pdf.resolve(r)?;
            match resolved {
                Object::Array(a) => a,
                _ => return Ok(None),
            }
        }
        _ => return Ok(None),
    };

    if arr.len() != 4 {
        return Ok(None);
    }

    let nums: Vec<f64> = arr
        .iter()
        .map(|o| match o {
            Object::Real(f) => Some(*f),
            Object::Integer(i) => Some(*i as f64),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()
        .unwrap_or_default();

    if nums.len() != 4 {
        return Ok(None);
    }

    Ok(Some(PageBox::new(nums[0], nums[1], nums[2], nums[3])))
}

/// Render a single-line text appearance (no word-wrap, one `Td Tj` pair).
fn render_singleline(p: &TextAppearanceParams, out: &mut Vec<u8>) {
    let x = compute_x_offset(p, &p.text_bytes);
    // Vertical baseline: midpoint + 20% of font size for typical descender.
    let y = ((p.bbox_h - p.font_size) / 2.0 + p.font_size * 0.2).max(2.0);

    out.extend_from_slice(fmt_f64(x).as_bytes());
    out.push(b' ');
    out.extend_from_slice(fmt_f64(y).as_bytes());
    out.extend_from_slice(b" Td\n");

    write_literal_string(out, &p.text_bytes);
    out.extend_from_slice(b" Tj\n");
}

/// Render a multiline text appearance (simple word-wrap on space boundaries).
///
/// `Td` is a *relative* move on the text line matrix.  Each subsequent `Td`
/// is expressed as a delta `(x_curr − x_prev, −leading)` rather than an
/// absolute coordinate so that x offsets do not accumulate across lines.
fn render_multiline(p: &TextAppearanceParams, out: &mut Vec<u8>) {
    let leading = p.font_size * 1.15;
    // Top baseline — allow small top margin.
    let first_y = p.bbox_h - p.font_size - 2.0;

    // Text is drawn inset ~2 units from each side (see compute_x_offset and the
    // left margin used for the first Td), so wrap against the effective drawable
    // width — `bbox_w - 4.0` — to avoid lines that fit `bbox_w` yet overrun the
    // right edge. Clamp to non-negative for degenerate boxes.
    let effective_w = (p.bbox_w - 4.0).max(0.0);
    let lines = word_wrap(&p.text_bytes, p.font_size, effective_w, p.std_font);
    let mut prev_x = 0.0_f64; // tracks last Td x so we can emit deltas

    for (i, line) in lines.iter().enumerate() {
        let x = compute_x_offset(p, line);
        if i == 0 {
            // First line: absolute position via Td.
            out.extend_from_slice(fmt_f64(x).as_bytes());
            out.push(b' ');
            out.extend_from_slice(fmt_f64(first_y).as_bytes());
            out.extend_from_slice(b" Td\n");
        } else {
            // Subsequent lines: emit delta x and -leading.
            let dx = x - prev_x;
            out.extend_from_slice(fmt_f64(dx).as_bytes());
            out.push(b' ');
            out.extend_from_slice(fmt_f64(-leading).as_bytes());
            out.extend_from_slice(b" Td\n");
        }
        prev_x = x;
        write_literal_string(out, line);
        out.extend_from_slice(b" Tj\n");
    }
}

/// Compute the horizontal starting offset for a text run with the given
/// quadding setting.
fn compute_x_offset(p: &TextAppearanceParams, text: &[u8]) -> f64 {
    match p.quadding {
        1 => {
            // Centre.
            let w = p
                .std_font
                .map_or(0.0, |sf| sf.string_width(text, p.font_size));
            ((p.bbox_w - w) / 2.0).max(2.0)
        }
        2 => {
            // Right.
            let w = p
                .std_font
                .map_or(0.0, |sf| sf.string_width(text, p.font_size));
            (p.bbox_w - w - 2.0).max(2.0)
        }
        _ => 2.0, // Left (default).
    }
}

/// Simple word-wrap: splits `text` into lines that fit within `max_width`.
///
/// Splits on space bytes only.  Returns at least one element (may overflow
/// if a single word exceeds `max_width`).
fn word_wrap(
    text: &[u8],
    font_size: f64,
    max_width: f64,
    std_font: Option<StandardFont>,
) -> Vec<Vec<u8>> {
    let Some(sf) = std_font else {
        // Without font metrics, return the whole text as one line.
        return vec![text.to_vec()];
    };

    if max_width <= 0.0 {
        return vec![text.to_vec()];
    }

    let mut lines: Vec<Vec<u8>> = Vec::new();

    // First honour explicit hard line breaks the user typed into the field
    // value (`\r\n`, `\r`, or `\n`); only then soft-wrap each segment on
    // spaces. Without this, embedded newlines would be carried into a single
    // `Tj` literal and render as one run (or a control glyph) instead of
    // preserving the entered line structure.
    for segment in split_hard_lines(text) {
        let mut current: Vec<u8> = Vec::new();
        // Track the first word explicitly rather than testing `current.is_empty()`:
        // a segment that begins with a space yields an empty first word, and an
        // emptiness test would treat the following word as the first one again,
        // silently dropping the leading space.
        let mut first = true;
        for word in segment.split(|&b| b == b' ') {
            if first {
                current.extend_from_slice(word);
                first = false;
            } else {
                // Tentatively append.
                let mut candidate = current.clone();
                candidate.push(b' ');
                candidate.extend_from_slice(word);
                if sf.string_width(&candidate, font_size) <= max_width {
                    current = candidate;
                } else {
                    lines.push(current.clone());
                    current.clear();
                    current.extend_from_slice(word);
                }
            }
        }
        // Preserve the segment even when empty (a blank hard line).
        lines.push(current);
    }

    if lines.is_empty() {
        lines.push(Vec::new());
    }

    lines
}

/// Split `text` into hard lines on `\r\n`, `\r`, or `\n`, preserving empty
/// segments (so blank lines entered by the user are kept).
fn split_hard_lines(text: &[u8]) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut line: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < text.len() {
        match text[i] {
            b'\r' => {
                out.push(std::mem::take(&mut line));
                // Treat CRLF as a single break.
                if text.get(i + 1) == Some(&b'\n') {
                    i += 1;
                }
            }
            b'\n' => out.push(std::mem::take(&mut line)),
            b => line.push(b),
        }
        i += 1;
    }
    out.push(line);
    out
}

// ── Ch (choice: combo/list) appearance ───────────────────────────────────────

/// Generate and install a normal appearance stream for a Ch (choice) widget.
///
/// Handles two Ch sub-types determined by `/Ff` bit 18 (1-indexed, `0x20000`):
///
/// - **Combo** (bit 18 set): renders the selected value (`/V`) as a single-line
///   text field, equivalent to a Tx single-line field.  If `/V` is absent the
///   stream is a blank appearance.
/// - **List** (bit 18 clear): renders each option from `/Opt` as a separate row,
///   with a coloured highlight rectangle behind selected rows.  `/I` (selected
///   indices) is consulted first; if absent, `/V` is matched against `/Opt`
///   export values.  `/TI` (top-visible index) offsets the starting row.
///
/// Returns `Ok(Some(xobj_ref))` on success, `Ok(None)` when the widget should
/// be skipped (not a Ch field or degenerate bounding box).
///
/// # Observable-equivalence policy
///
/// Rendering of selected values/options and highlight rectangles targets
/// **observable equivalence** — the same rendered appearance as standard
/// viewers.  Byte-identical output is not a goal; whitespace, operator
/// ordering, highlight colour, and auto-size heuristics may differ between
/// this implementation and any specific viewer.
///
/// # Errors
///
/// Returns [`Error::Unsupported`] when the field-tree depth limit is exceeded
/// or an object is structurally invalid.
///
/// # Examples
///
/// ```no_run
/// use flpdf::{generate_choice_field_appearance, ObjectRef, Pdf};
/// use std::fs::File;
/// use std::io::BufReader;
///
/// let mut pdf = Pdf::open(BufReader::new(File::open("form.pdf")?))?;
/// let widget = ObjectRef::new(10, 0);
/// if let Some(ap_ref) = generate_choice_field_appearance(&mut pdf, widget)? {
///     println!("choice appearance stream written at {ap_ref}");
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn generate_choice_field_appearance<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    widget_ref: ObjectRef,
) -> Result<Option<ObjectRef>> {
    // ── 1. Verify /FT is Ch ───────────────────────────────────────────────────
    let ft = resolve_inherited_name(pdf, widget_ref, b"FT")?;
    if ft.as_deref() != Some(b"Ch") {
        return Ok(None);
    }

    // ── 2. /Ff — field flags; bit 18 (1-indexed) = Combo ─────────────────────
    let ff = resolve_inherited_integer(pdf, widget_ref, b"Ff")?.unwrap_or(0);
    let is_combo = ff & 0x20000 != 0; // bit 18 (1-indexed)

    // ── 3. /Rect — bounding box ───────────────────────────────────────────────
    let rect = resolve_rect(pdf, widget_ref)?;
    let (bbox_w, bbox_h) = match rect {
        Some(r) => {
            let w = (r.urx - r.llx).abs();
            let h = (r.ury - r.lly).abs();
            if !w.is_finite() || !h.is_finite() || w < 1.0 || h < 1.0 {
                return Ok(None);
            }
            (w, h)
        }
        None => return Ok(None),
    };

    // ── 4. /DA → parse → font resolution (same pattern as Tx) ────────────────
    let da_bytes = resolve_da(pdf, widget_ref)?;
    let da = parse_default_appearance(da_bytes.as_deref().unwrap_or(b""));

    let font_name_bytes: Vec<u8> = da.font_name.clone().unwrap_or_else(|| b"Helv".to_vec());
    let std_font = match StandardFont::from_base_name(&font_name_bytes) {
        Some(sf) => sf,
        None => lookup_dr_basefont(pdf, widget_ref, &font_name_bytes)?
            .unwrap_or(StandardFont::Helvetica),
    };
    let base_font_name = official_base_name(std_font).to_vec();
    let font_resource_name = font_name_bytes.clone();
    let font_info = ChFontInfo {
        resource_name: font_resource_name,
        base_name: base_font_name,
        std_font,
    };

    // ── 5. /Q — quadding ──────────────────────────────────────────────────────
    let quadding = resolve_inherited_integer(pdf, widget_ref, b"Q")?.unwrap_or(0);

    if is_combo {
        generate_combo_appearance(pdf, widget_ref, bbox_w, bbox_h, &da, font_info, quadding)
    } else {
        generate_list_appearance(pdf, widget_ref, bbox_w, bbox_h, &da, font_info)
    }
}

/// Resolved font information for Ch appearance rendering.
struct ChFontInfo {
    /// Name used in the Tf operator and `/Resources/Font` key (from /DA).
    resource_name: Vec<u8>,
    /// Official PDF BaseFont name (e.g. `b"Helvetica"`).
    base_name: Vec<u8>,
    /// Resolved standard font variant.
    std_font: StandardFont,
}

/// Render a Combo-box appearance: the selected `/V` value as a single-line text.
fn generate_combo_appearance<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    widget_ref: ObjectRef,
    bbox_w: f64,
    bbox_h: f64,
    da: &crate::default_appearance::DefaultAppearance,
    font: ChFontInfo,
    quadding: i64,
) -> Result<Option<ObjectRef>> {
    // Read /V — combo boxes have a single String value.
    // Resolve the raw value through any indirect reference (review-pattern #2).
    let raw_value = resolve_inherited_object(pdf, widget_ref, b"V")?;
    let text_bytes: Vec<u8> = match raw_value {
        None => Vec::new(),
        Some(Object::String(bytes)) => decode_pdf_text_string(&bytes)
            .map(|s| to_winansi_bytes(&s))
            .unwrap_or(bytes),
        Some(Object::Reference(r)) => {
            let obj = pdf.resolve(r)?;
            match obj {
                Object::String(bytes) => decode_pdf_text_string(&bytes)
                    .map(|s| to_winansi_bytes(&s))
                    .unwrap_or(bytes),
                _ => Vec::new(),
            }
        }
        _ => Vec::new(),
    };

    // Best-effort: if /Opt has [export,display] entries and /V matches an
    // export string, use the display string instead.
    let text_bytes = resolve_combo_display(pdf, widget_ref, text_bytes);

    // Font size: same single-line heuristic as Tx.
    let font_size = if da.auto_size {
        (bbox_h - 2.0).clamp(4.0, 12.0)
    } else {
        da.font_size
    };

    let params = TextAppearanceParams {
        text_bytes,
        font_resource_name: font.resource_name.clone(),
        font_size,
        color: da.color.clone(),
        bbox_w,
        bbox_h,
        quadding,
        multiline: false,
        std_font: Some(font.std_font),
    };
    let content = build_text_appearance_content(&params);

    let font_obj_ref = next_object_ref(pdf)?;
    let xobj_ref = install_normal_appearance(
        pdf,
        widget_ref,
        content,
        bbox_w,
        bbox_h,
        Some((font.resource_name, font.base_name, font_obj_ref)),
    )?;

    Ok(Some(xobj_ref))
}

/// Best-effort: map a combo /V export value to its display string via /Opt.
///
/// Returns `value` unchanged when /Opt is absent, malformed, or does not
/// contain an `[export, display]` pair matching `value`.  All /Opt elements
/// and their sub-elements are resolved through indirect references
/// (review-pattern #2).
fn resolve_combo_display<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    widget_ref: ObjectRef,
    value: Vec<u8>,
) -> Vec<u8> {
    // /Opt may live on a parent field (child widget carries only /Parent), so
    // walk the field inheritance chain like /FT, /V, and /DA do.
    let opt_val = resolve_inherited_object(pdf, widget_ref, b"Opt")
        .ok()
        .flatten();
    let opt_arr = match resolve_opt_array(pdf, opt_val) {
        Some(a) => a,
        None => return value,
    };

    for elem_obj in opt_arr {
        // Each element may be indirect.
        let elem = match elem_obj {
            Object::Reference(r) => match pdf.resolve(r) {
                Ok(o) => o,
                Err(_) => continue,
            },
            other => other,
        };
        match elem {
            Object::Array(pair) => {
                // [export_string, display_string]
                let export = resolve_string_elem(pdf, pair.first().cloned());
                let display = resolve_string_elem(pdf, pair.get(1).cloned());
                if let (Some(exp), Some(disp)) = (export, display) {
                    let exp_wi = decode_pdf_text_string(&exp)
                        .map(|s| to_winansi_bytes(&s))
                        .unwrap_or(exp);
                    if exp_wi == value {
                        return decode_pdf_text_string(&disp)
                            .map(|s| to_winansi_bytes(&s))
                            .unwrap_or(disp);
                    }
                }
            }
            Object::String(s) => {
                let wi = decode_pdf_text_string(&s)
                    .map(|st| to_winansi_bytes(&st))
                    .unwrap_or(s);
                if wi == value {
                    return value; // export == display
                }
            }
            _ => {}
        }
    }
    value
}

/// Resolve an /Opt value (possibly indirect) to a Vec<Object> array.
fn resolve_opt_array<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    opt_val: Option<Object>,
) -> Option<Vec<Object>> {
    match opt_val {
        Some(Object::Array(a)) => Some(a),
        Some(Object::Reference(r)) => {
            let resolved = pdf.resolve(r).ok()?;
            match resolved {
                Object::Array(a) => Some(a),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Resolve a single /Opt element (possibly indirect) to a String byte vec.
fn resolve_string_elem<R: Read + Seek>(pdf: &mut Pdf<R>, val: Option<Object>) -> Option<Vec<u8>> {
    match val? {
        Object::String(s) => Some(s),
        Object::Reference(r) => match pdf.resolve(r).ok()? {
            Object::String(s) => Some(s),
            _ => None,
        },
        _ => None,
    }
}

/// Render a List-box appearance: each option as a row, selected rows highlighted.
fn generate_list_appearance<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    widget_ref: ObjectRef,
    bbox_w: f64,
    bbox_h: f64,
    da: &crate::default_appearance::DefaultAppearance,
    font: ChFontInfo,
) -> Result<Option<ObjectRef>> {
    // ── Collect options from /Opt (inherited from a parent field if needed) ──
    // /Opt may sit on a parent field (child widget carries only /Parent) and may
    // itself be an indirect reference (review-pattern #2).
    let opt_val = resolve_inherited_object(pdf, widget_ref, b"Opt")?;
    let opt_arr = resolve_opt_array(pdf, opt_val).unwrap_or_default();
    let n_opts = opt_arr.len();

    // Build display texts and export texts (for /V matching).
    struct OptEntry {
        export: Vec<u8>,  // WinAnsi bytes of export string
        display: Vec<u8>, // WinAnsi bytes of display string
    }

    let mut options: Vec<OptEntry> = Vec::with_capacity(n_opts);
    for elem_obj in opt_arr {
        let elem = match elem_obj {
            Object::Reference(r) => pdf.resolve(r)?,
            other => other,
        };
        match elem {
            Object::Array(pair) => {
                let exp_raw = resolve_string_elem(pdf, pair.first().cloned()).unwrap_or_default();
                let disp_raw = resolve_string_elem(pdf, pair.get(1).cloned()).unwrap_or_default();
                let export = decode_pdf_text_string(&exp_raw)
                    .map(|s| to_winansi_bytes(&s))
                    .unwrap_or(exp_raw);
                let display = decode_pdf_text_string(&disp_raw)
                    .map(|s| to_winansi_bytes(&s))
                    .unwrap_or(disp_raw);
                options.push(OptEntry { export, display });
            }
            Object::String(s) => {
                let wi = decode_pdf_text_string(&s)
                    .map(|st| to_winansi_bytes(&st))
                    .unwrap_or(s);
                options.push(OptEntry {
                    export: wi.clone(),
                    display: wi,
                });
            }
            _ => {
                options.push(OptEntry {
                    export: Vec::new(),
                    display: Vec::new(),
                });
            }
        }
    }

    // ── Determine selected indices ─────────────────────────────────────────────
    // /I (integer array) takes priority; missing/invalid → fall back to /V.
    let selected: std::collections::BTreeSet<usize> = {
        // /I may live on a parent field (like /Opt, /V); walk the inheritance
        // chain so a /Parent-only widget still highlights the selected rows.
        let i_val = resolve_inherited_object(pdf, widget_ref, b"I")?;
        let i_arr = resolve_opt_array(pdf, i_val);

        if let Some(arr) = i_arr {
            let mut set = std::collections::BTreeSet::new();
            for elem in arr {
                let resolved = match elem {
                    Object::Reference(r) => pdf.resolve(r)?,
                    other => other,
                };
                if let Some(idx_i64) = resolved.as_integer() {
                    // Non-negative and in-bounds check before cast (rule #3).
                    if idx_i64 >= 0 {
                        let idx = idx_i64 as usize;
                        if idx < options.len() {
                            set.insert(idx);
                        }
                    }
                }
            }
            set
        } else {
            // Fall back: match /V against /Opt export strings. /V may be a
            // single string or an array (multi-select), and either form may
            // arrive indirectly — resolve a top-level reference first so the
            // indirect array case is handled identically to the direct one.
            let v_val = match resolve_inherited_object(pdf, widget_ref, b"V")? {
                Some(Object::Reference(r)) => Some(pdf.resolve(r)?),
                other => other,
            };
            let mut set = std::collections::BTreeSet::new();
            // Collect candidate value strings (each /V entry may itself be an
            // indirect string).
            let mut value_strings: Vec<Vec<u8>> = Vec::new();
            match v_val {
                Some(Object::String(s)) => value_strings.push(s),
                Some(Object::Array(arr)) => {
                    for elem in arr {
                        let resolved = match elem {
                            Object::Reference(r) => pdf.resolve(r)?,
                            other => other,
                        };
                        if let Object::String(s) = resolved {
                            value_strings.push(s);
                        }
                    }
                }
                _ => {}
            }
            for s in value_strings {
                let wi = decode_pdf_text_string(&s)
                    .map(|st| to_winansi_bytes(&st))
                    .unwrap_or(s);
                for (i, opt) in options.iter().enumerate() {
                    if opt.export == wi {
                        set.insert(i);
                        break;
                    }
                }
            }
            set
        }
    };

    // ── /TI — top index (first visible option) ────────────────────────────────
    // Inherited like /Opt and /I; non-negative and in-bounds (review-pattern #3).
    let ti_val = resolve_inherited_object(pdf, widget_ref, b"TI")?;
    let ti: usize = match ti_val {
        Some(Object::Reference(r)) => {
            let resolved = pdf.resolve(r)?;
            match resolved.as_integer() {
                Some(n) if n >= 0 && (n as usize) < options.len() => n as usize,
                _ => 0,
            }
        }
        Some(Object::Integer(n)) if n >= 0 && (n as usize) < options.len() => n as usize,
        _ => 0,
    };

    // ── Font size ──────────────────────────────────────────────────────────────
    // For list boxes use a fixed reasonable size when /DA has auto-size (0);
    // using bbox_h (the total field height) would be wrong since it spans many rows.
    let font_size = if da.auto_size {
        12.0_f64.min(bbox_h * 0.8).max(4.0)
    } else {
        da.font_size
    };
    let leading = font_size * 1.15;

    // ── Build content stream ───────────────────────────────────────────────────
    // Structure (ISO 32000-1 §9.4.1: path operators must NOT appear inside BT..ET):
    //   q
    //   <highlight rects outside BT>
    //   BT
    //   /Font size Tf
    //   <color ops>
    //   <per-row Td Tj>
    //   ET
    //   Q
    let mut content: Vec<u8> = Vec::new();
    content.extend_from_slice(b"q\n");

    // Emit highlight rectangles first (outside BT..ET).
    // highlight colour: 0.6 0.75 0.85 rg (light blue)
    let mut any_highlight = false;
    for (row, i) in (ti..options.len()).enumerate() {
        let row_top = bbox_h - leading * (row as f64 + 1.0);
        if row_top < -leading {
            break; // clipped below bbox
        }
        if selected.contains(&i) {
            if !any_highlight {
                content.extend_from_slice(b"0.6 0.75 0.85 rg\n");
                any_highlight = true;
            }
            // Rect: x=0, y=row_top, w=bbox_w, h=leading
            content.extend_from_slice(fmt_f64(0.0).as_bytes());
            content.push(b' ');
            content.extend_from_slice(fmt_f64(row_top).as_bytes());
            content.push(b' ');
            content.extend_from_slice(fmt_f64(bbox_w).as_bytes());
            content.push(b' ');
            content.extend_from_slice(fmt_f64(leading).as_bytes());
            content.extend_from_slice(b" re\n");
            content.extend_from_slice(b"f\n");
        }
    }

    content.extend_from_slice(b"BT\n");

    // Tf operator.
    content.push(b'/');
    crate::object::write_name_escaped(&mut content, &font.resource_name);
    content.push(b' ');
    content.extend_from_slice(fmt_f64(font_size).as_bytes());
    content.extend_from_slice(b" Tf\n");

    // Text colour (from /DA).
    color_ops(&da.color, &mut content);

    // Per-row text.
    for (row, i) in (ti..options.len()).enumerate() {
        let row_top = bbox_h - leading * (row as f64 + 1.0);
        if row_top < -leading {
            break;
        }
        // Baseline: midpoint of the row slot with typical descender offset.
        let text_y = row_top + font_size * 0.2;
        let text_x = 2.0_f64; // left margin

        // Each row uses an absolute Td (text line matrix reset each row
        // by issuing Td from origin each time by using a full absolute move).
        // We achieve absolute positioning by resetting the text matrix with
        // a Td that brings us from (0,0) to (text_x, text_y) on the first row,
        // and using relative Td on subsequent rows.
        if row == 0 {
            content.extend_from_slice(fmt_f64(text_x).as_bytes());
            content.push(b' ');
            content.extend_from_slice(fmt_f64(text_y).as_bytes());
            content.extend_from_slice(b" Td\n");
        } else {
            // delta: move down by -leading (y), x stays same
            content.extend_from_slice(b"0 ");
            content.extend_from_slice(fmt_f64(-leading).as_bytes());
            content.extend_from_slice(b" Td\n");
        }

        write_literal_string(&mut content, &options[i].display);
        content.extend_from_slice(b" Tj\n");
    }

    content.extend_from_slice(b"ET\n");
    content.extend_from_slice(b"Q\n");

    // ── Install ────────────────────────────────────────────────────────────────
    let font_obj_ref = next_object_ref(pdf)?;
    let xobj_ref = install_normal_appearance(
        pdf,
        widget_ref,
        content,
        bbox_w,
        bbox_h,
        Some((font.resource_name, font.base_name, font_obj_ref)),
    )?;

    Ok(Some(xobj_ref))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content_stream::{ContentStreamParser, ContentToken};
    use crate::writer::write_pdf;
    use crate::Pdf;
    use std::io::Cursor;

    // ── Unit tests for pure helpers ──────────────────────────────────────────

    #[test]
    fn fmt_f64_integer_no_decimal() {
        assert_eq!(fmt_f64(12.0), "12");
        assert_eq!(fmt_f64(0.0), "0");
        assert_eq!(fmt_f64(-3.0), "-3");
    }

    #[test]
    fn fmt_f64_strips_trailing_zeros() {
        assert_eq!(fmt_f64(1.5), "1.5");
        assert_eq!(fmt_f64(0.25), "0.25");
        // Four decimal places of precision.
        assert_eq!(fmt_f64(1.0 / 3.0), "0.3333");
    }

    #[test]
    fn fmt_f64_non_finite_returns_zero() {
        assert_eq!(fmt_f64(f64::NAN), "0");
        assert_eq!(fmt_f64(f64::INFINITY), "0");
    }

    #[test]
    fn color_ops_gray() {
        let mut out = Vec::new();
        color_ops(&TextColor::Gray(0.0), &mut out);
        assert_eq!(out, b"0 g\n");
    }

    #[test]
    fn color_ops_rgb() {
        let mut out = Vec::new();
        color_ops(&TextColor::Rgb(1.0, 0.0, 0.0), &mut out);
        assert_eq!(out, b"1 0 0 rg\n");
    }

    #[test]
    fn color_ops_cmyk() {
        let mut out = Vec::new();
        color_ops(&TextColor::Cmyk(0.0, 0.0, 0.0, 1.0), &mut out);
        assert_eq!(out, b"0 0 0 1 k\n");
    }

    #[test]
    fn to_winansi_ascii_passthrough() {
        let out = to_winansi_bytes("Hello");
        assert_eq!(out, b"Hello");
    }

    #[test]
    fn to_winansi_latin1_passthrough() {
        // U+00E9 = é = 0xE9 in Latin-1/WinAnsi
        let out = to_winansi_bytes("caf\u{00E9}");
        assert_eq!(out, b"caf\xe9");
    }

    #[test]
    fn to_winansi_c1_range_replaced() {
        // U+0080–U+009F → '?'
        let out = to_winansi_bytes("\u{0080}\u{009F}");
        assert_eq!(out, b"??");
    }

    #[test]
    fn to_winansi_beyond_latin1_replaced() {
        let out = to_winansi_bytes("\u{0100}");
        assert_eq!(out, b"?");
    }

    #[test]
    fn to_winansi_cp1252_typographic_chars_mapped() {
        // CP1252 0x80–0x9F typographic characters map to their CP1252 byte,
        // not '?'.
        assert_eq!(to_winansi_bytes("\u{20AC}"), b"\x80"); // € Euro
        assert_eq!(to_winansi_bytes("\u{2019}"), b"\x92"); // ' right single quote
        assert_eq!(to_winansi_bytes("\u{201C}\u{201D}"), b"\x93\x94"); // " "
        assert_eq!(to_winansi_bytes("\u{2013}\u{2014}"), b"\x96\x97"); // – —
        assert_eq!(to_winansi_bytes("\u{2022}"), b"\x95"); // • bullet
        assert_eq!(to_winansi_bytes("\u{2026}"), b"\x85"); // … ellipsis
        assert_eq!(to_winansi_bytes("\u{0152}\u{0153}"), b"\x8c\x9c"); // Œ œ
        assert_eq!(to_winansi_bytes("\u{2122}"), b"\x99"); // ™
    }

    #[test]
    fn to_winansi_undefined_cp1252_slots_replaced() {
        // A character beyond Latin-1 with no CP1252 slot still maps to '?'.
        assert_eq!(to_winansi_bytes("\u{2603}"), b"?"); // ☃ snowman
    }

    #[test]
    fn official_base_name_all_variants() {
        // Every variant must map to a known official name.
        let cases = [
            (StandardFont::Helvetica, b"Helvetica" as &[u8]),
            (StandardFont::HelveticaBold, b"Helvetica-Bold"),
            (StandardFont::HelveticaOblique, b"Helvetica-Oblique"),
            (StandardFont::HelveticaBoldOblique, b"Helvetica-BoldOblique"),
            (StandardFont::TimesRoman, b"Times-Roman"),
            (StandardFont::TimesBold, b"Times-Bold"),
            (StandardFont::TimesItalic, b"Times-Italic"),
            (StandardFont::TimesBoldItalic, b"Times-BoldItalic"),
            (StandardFont::Courier, b"Courier"),
            (StandardFont::CourierBold, b"Courier-Bold"),
            (StandardFont::CourierOblique, b"Courier-Oblique"),
            (StandardFont::CourierBoldOblique, b"Courier-BoldOblique"),
            (StandardFont::Symbol, b"Symbol"),
            (StandardFont::ZapfDingbats, b"ZapfDingbats"),
        ];
        for (font, expected) in cases {
            assert_eq!(official_base_name(font), expected);
        }
    }

    #[test]
    fn build_text_appearance_basic_tokens() {
        let params = TextAppearanceParams {
            text_bytes: b"Hello".to_vec(),
            font_resource_name: b"Helv".to_vec(),
            font_size: 12.0,
            color: TextColor::Gray(0.0),
            bbox_w: 200.0,
            bbox_h: 20.0,
            quadding: 0,
            multiline: false,
            std_font: StandardFont::from_base_name(b"Helv"),
        };
        let content = build_text_appearance_content(&params);
        let content_str = String::from_utf8_lossy(&content);

        // Must open and close the marked-content sequence.
        assert!(content_str.contains("/Tx BMC"), "missing /Tx BMC");
        assert!(content_str.contains("EMC"), "missing EMC");
        // Must have a text block.
        assert!(content_str.contains("BT"), "missing BT");
        assert!(content_str.contains("ET"), "missing ET");
        // Tf operator must use the font resource name.
        assert!(content_str.contains("/Helv 12 Tf"), "missing /Helv 12 Tf");
        // Tj must appear with the text.
        assert!(content_str.contains("Tj"), "missing Tj");
    }

    #[test]
    fn build_text_appearance_escapes_font_resource_name() {
        // A resource name with a delimiter byte (space) must be #-escaped in
        // the Tf operator so the content stream stays well-formed.
        let params = TextAppearanceParams {
            text_bytes: b"Hi".to_vec(),
            font_resource_name: b"F A".to_vec(),
            font_size: 12.0,
            color: TextColor::Gray(0.0),
            bbox_w: 100.0,
            bbox_h: 20.0,
            quadding: 0,
            multiline: false,
            std_font: StandardFont::from_base_name(b"Helv"),
        };
        let content = build_text_appearance_content(&params);
        let s = String::from_utf8_lossy(&content);
        assert!(s.contains("/F#20A 12 Tf"), "name not escaped: {s}");
        // The raw, unescaped name must not leak into the stream.
        assert!(!s.contains("/F A 12 Tf"), "raw name leaked: {s}");
    }

    #[test]
    fn build_text_appearance_empty_value_is_valid_blank() {
        // A present-but-empty value still yields a structurally valid blank
        // appearance (marked content + text block), which replaces any stale
        // /AP/N on a cleared field.
        let params = TextAppearanceParams {
            text_bytes: Vec::new(),
            font_resource_name: b"Helv".to_vec(),
            font_size: 12.0,
            color: TextColor::Gray(0.0),
            bbox_w: 100.0,
            bbox_h: 20.0,
            quadding: 0,
            multiline: false,
            std_font: StandardFont::from_base_name(b"Helv"),
        };
        let content = build_text_appearance_content(&params);
        // Every token must parse cleanly.
        for tok in ContentStreamParser::new(&content) {
            tok.expect("blank appearance must tokenize");
        }
        let s = String::from_utf8_lossy(&content);
        assert!(s.contains("/Tx BMC") && s.contains("EMC"));
    }

    #[test]
    fn build_text_appearance_parses_with_content_stream_parser() {
        let params = TextAppearanceParams {
            text_bytes: b"Test".to_vec(),
            font_resource_name: b"Helv".to_vec(),
            font_size: 10.0,
            color: TextColor::Gray(0.0),
            bbox_w: 150.0,
            bbox_h: 18.0,
            quadding: 0,
            multiline: false,
            std_font: StandardFont::from_base_name(b"Helv"),
        };
        let content = build_text_appearance_content(&params);

        let mut found_tf = false;
        let mut found_td = false;
        let mut found_tj = false;
        let mut tj_operand: Option<Vec<u8>> = None;
        let mut tf_font_name: Option<Vec<u8>> = None;

        for tok in ContentStreamParser::new(&content).flatten() {
            let ContentToken::Op { operands, operator } = tok else {
                continue;
            };
            match operator.as_slice() {
                b"Tf" => {
                    found_tf = true;
                    tf_font_name = operands
                        .first()
                        .and_then(|o| o.as_name())
                        .map(|n| n.to_vec());
                }
                b"Td" => found_td = true,
                b"Tj" => {
                    found_tj = true;
                    tj_operand = operands.first().and_then(|o| {
                        if let Object::String(bytes) = o {
                            Some(bytes.clone())
                        } else {
                            None
                        }
                    });
                }
                _ => {}
            }
        }

        assert!(found_tf, "Tf operator not found in content stream");
        assert!(found_td, "Td operator not found in content stream");
        assert!(found_tj, "Tj operator not found in content stream");
        assert_eq!(
            tf_font_name.as_deref(),
            Some(b"Helv" as &[u8]),
            "Tf font name mismatch"
        );
        // The Tj string must decode back to the original text.
        let tj_str = tj_operand.expect("Tj has no string operand");
        assert_eq!(tj_str, b"Test", "Tj operand mismatch");
    }

    #[test]
    fn word_wrap_single_word() {
        let lines = word_wrap(b"Hello", 12.0, 200.0, StandardFont::from_base_name(b"Helv"));
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], b"Hello");
    }

    #[test]
    fn word_wrap_no_font_returns_single_line() {
        let lines = word_wrap(b"Hello World", 12.0, 50.0, None);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn word_wrap_preserves_leading_space() {
        // A segment that begins with a space must keep that leading space and
        // not drop it (regression for the first-word emptiness bug). Width is
        // large enough that no soft-wrapping occurs.
        let font = StandardFont::from_base_name(b"Helv");
        assert_eq!(word_wrap(b" hi", 12.0, 1000.0, font), vec![b" hi".to_vec()]);
        // A leading space on a hard line is also preserved.
        assert_eq!(
            word_wrap(b"a\n bc", 12.0, 1000.0, font),
            vec![b"a".to_vec(), b" bc".to_vec()]
        );
    }

    #[test]
    fn word_wrap_honours_explicit_newlines() {
        // Explicit hard line breaks must split into separate lines even when
        // the whole value would fit on one line by width.
        let font = StandardFont::from_base_name(b"Helv");
        assert_eq!(
            word_wrap(b"one\ntwo", 12.0, 500.0, font),
            vec![b"one".to_vec(), b"two".to_vec()]
        );
        // CRLF counts as a single break, and a blank line is preserved.
        assert_eq!(
            word_wrap(b"a\r\n\r\nb", 12.0, 500.0, font),
            vec![b"a".to_vec(), Vec::new(), b"b".to_vec()]
        );
    }

    #[test]
    fn split_hard_lines_variants() {
        assert_eq!(
            split_hard_lines(b"a\nb"),
            vec![b"a".to_vec(), b"b".to_vec()]
        );
        assert_eq!(
            split_hard_lines(b"a\r\nb"),
            vec![b"a".to_vec(), b"b".to_vec()]
        );
        assert_eq!(
            split_hard_lines(b"a\rb"),
            vec![b"a".to_vec(), b"b".to_vec()]
        );
        assert_eq!(split_hard_lines(b"plain"), vec![b"plain".to_vec()]);
    }

    // ── Integration round-trip test ──────────────────────────────────────────

    /// Minimal PDF with one Tx widget in an AcroForm.
    ///
    /// Object layout:
    ///  1 0: Catalog  (with /AcroForm carrying /DA)
    ///  2 0: Pages
    ///  3 0: Page
    ///  4 0: Widget   (Tx field with /V)
    fn build_minimal_tx_pdf() -> Vec<u8> {
        let mut pdf: Vec<u8> = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"1 0 obj\n<</Type /Catalog /Pages 2 0 R /AcroForm \
              <</Fields [4 0 R] /DR <<>> /DA (/Helv 12 Tf 0 g)>>>>\nendobj\n",
        );

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Annots [4 0 R]>>\nendobj\n",
        );

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Tx /T (field1) \
              /V (Hello World) /Rect [100 700 300 720] /P 3 0 R>>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 5\n\
             0000000000 65535 f \n\
             {:010} 00000 n \n\
             {:010} 00000 n \n\
             {:010} 00000 n \n\
             {:010} 00000 n \n",
            off1, off2, off3, off4,
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer = format!("trailer\n<</Size 5 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    #[test]
    fn round_trip_generate_tx_appearance() {
        let raw = build_minimal_tx_pdf();
        let cursor = Cursor::new(raw);
        let mut pdf = Pdf::open(cursor).expect("parse minimal PDF");

        let widget_ref = ObjectRef::new(4, 0);
        let result = generate_text_field_appearance(&mut pdf, widget_ref);
        assert!(
            result.is_ok(),
            "generate_text_field_appearance returned error: {:?}",
            result
        );
        let xobj_ref = result.unwrap();
        assert!(
            xobj_ref.is_some(),
            "generate returned None — field should be handled"
        );
        let xobj_ref = xobj_ref.unwrap();

        // The appearance XObject must exist and be a Stream.
        let xobj = pdf.resolve(xobj_ref).expect("resolve xobj");
        let Object::Stream(stream) = xobj else {
            panic!("XObject is not a stream: {xobj:?}");
        };

        // Subtype must be Form.
        assert_eq!(
            stream.dict.get("Subtype"),
            Some(&Object::Name(b"Form".to_vec())),
            "XObject /Subtype is not /Form"
        );

        // Parse the stream content and verify key operators and operands.
        let content = stream.data.clone();
        let mut found_tf = false;
        let mut found_tj = false;
        let mut tf_name: Option<Vec<u8>> = None;
        let mut tj_val: Option<Vec<u8>> = None;

        for tok in ContentStreamParser::new(&content).flatten() {
            let ContentToken::Op { operands, operator } = tok else {
                continue;
            };
            match operator.as_slice() {
                b"Tf" => {
                    found_tf = true;
                    tf_name = operands
                        .first()
                        .and_then(|o| o.as_name())
                        .map(|n| n.to_vec());
                }
                b"Tj" => {
                    found_tj = true;
                    if let Some(Object::String(bytes)) = operands.first() {
                        tj_val = Some(bytes.clone());
                    }
                }
                _ => {}
            }
        }

        assert!(found_tf, "Tf not in appearance stream");
        assert!(found_tj, "Tj not in appearance stream");

        let font_name = tf_name.expect("Tf has no operand");

        // The /Resources/Font dict must contain a key matching the Tf operator name.
        let resources = stream.dict.get("Resources").expect("no /Resources");
        let Object::Dictionary(res_dict) = resources else {
            panic!("Resources is not a dict");
        };
        let font_dict_obj = res_dict.get("Font").expect("no /Resources/Font");
        let Object::Dictionary(font_dict) = font_dict_obj else {
            panic!("Font resources is not a dict");
        };
        let font_key = String::from_utf8_lossy(&font_name).into_owned();
        let font_obj_entry = font_dict
            .get(font_key.as_str())
            .unwrap_or_else(|| panic!("font key '{font_key}' not found in /Resources/Font"));

        // Resolve the font object and verify /BaseFont is the official name,
        // NOT the alias (e.g. must be "Helvetica", not "Helv").
        let font_obj_ref = match font_obj_entry {
            Object::Reference(r) => *r,
            _ => panic!("font entry is not a reference"),
        };
        let font_obj = pdf.resolve(font_obj_ref).expect("resolve font object");
        let Object::Dictionary(fdict) = font_obj else {
            panic!("font object is not a dictionary");
        };
        assert_eq!(
            fdict.get("BaseFont"),
            Some(&Object::Name(b"Helvetica".to_vec())),
            "/BaseFont must be the official name 'Helvetica', not the alias 'Helv'"
        );

        // The Tj operand must decode back to the field value.
        let rendered = tj_val.expect("Tj has no string operand");
        assert_eq!(rendered, b"Hello World", "Tj does not match field value");

        // Write out and re-parse to make sure the PDF structure is sound.
        let mut out = Vec::new();
        write_pdf(&mut pdf, &mut out).expect("write_pdf");
        let mut reparsed = Pdf::open(Cursor::new(out)).expect("re-parse written PDF");
        let xobj2 = reparsed.resolve(xobj_ref).expect("re-resolve xobj");
        assert!(
            matches!(xobj2, Object::Stream(_)),
            "re-parsed xobj is not a stream"
        );
    }

    /// PDF whose `/DA` references a `/DR` resource key (`/F1`) rather than a
    /// standard-font alias, where `/F1`'s `/BaseFont` is `/Times-Roman`.
    ///
    ///  1 0: Catalog (AcroForm with /DR /Font /F1 5 0 R, /DA (/F1 12 Tf))
    ///  2 0: Pages   3 0: Page   4 0: Widget (Tx)   5 0: Font (Times-Roman)
    fn build_dr_font_tx_pdf() -> Vec<u8> {
        let mut pdf: Vec<u8> = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"1 0 obj\n<</Type /Catalog /Pages 2 0 R /AcroForm \
              <</Fields [4 0 R] /DR <</Font <</F1 5 0 R>>>> /DA (/F1 12 Tf 0 g)>>>>\nendobj\n",
        );
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Annots [4 0 R]>>\nendobj\n",
        );
        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Tx /T (field1) \
              /V (Hi) /Rect [100 700 300 720] /P 3 0 R>>\nendobj\n",
        );
        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"5 0 obj\n<</Type /Font /Subtype /Type1 /BaseFont /Times-Roman>>\nendobj\n",
        );
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 6\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             {off5:010} 00000 n \n",
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer = format!("trailer\n<</Size 6 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    #[test]
    fn da_font_resolves_via_dr_basefont() {
        // /DA names /F1 (a /DR key, not a standard alias). The appearance must
        // resolve /F1 -> /BaseFont /Times-Roman and synthesize a Times-Roman
        // font dict, while the Tf operator keeps the /DA resource name "F1".
        let mut pdf = Pdf::open(Cursor::new(build_dr_font_tx_pdf())).expect("parse");
        let xobj_ref = generate_text_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("generate")
            .expect("Tx field handled");

        let Object::Stream(stream) = pdf.resolve(xobj_ref).expect("resolve xobj") else {
            panic!("not a stream");
        };

        // Tf operator must reference the /DA resource name "F1".
        let mut tf_name: Option<Vec<u8>> = None;
        for tok in ContentStreamParser::new(&stream.data).flatten() {
            if let ContentToken::Op { operands, operator } = tok {
                if operator == b"Tf" {
                    tf_name = operands
                        .first()
                        .and_then(|o| o.as_name())
                        .map(|n| n.to_vec());
                }
            }
        }
        assert_eq!(
            tf_name.as_deref(),
            Some(b"F1" as &[u8]),
            "Tf name must stay F1"
        );

        // The synthesized /Resources/Font/F1 must be Times-Roman.
        let Object::Dictionary(res) = stream.dict.get("Resources").expect("resources") else {
            panic!("resources not dict");
        };
        let Object::Dictionary(fonts) = res.get("Font").expect("font dict") else {
            panic!("font not dict");
        };
        let font_ref = match fonts.get("F1").expect("F1 entry") {
            Object::Reference(r) => *r,
            other => panic!("F1 not a ref: {other:?}"),
        };
        let Object::Dictionary(fdict) = pdf.resolve(font_ref).expect("resolve font") else {
            panic!("font obj not dict");
        };
        assert_eq!(
            fdict.get("BaseFont"),
            Some(&Object::Name(b"Times-Roman".to_vec())),
            "BaseFont must resolve from /DR to Times-Roman"
        );
    }

    fn build_btn_widget_pdf() -> Vec<u8> {
        let mut pdf: Vec<u8> = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Btn /T (chk) /Rect [10 10 30 30]>>\nendobj\n",
        );
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 5\n\
             0000000000 65535 f \n\
             {:010} 00000 n \n\
             {:010} 00000 n \n\
             {:010} 00000 n \n\
             {:010} 00000 n \n",
            off1, off2, off3, off4,
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer = format!("trailer\n<</Size 5 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    /// Build a Btn-widget PDF whose obj-4 dictionary body is supplied verbatim
    /// (so /Ff and /MK can be varied per test).
    fn build_btn_pdf_obj4(obj4_body: &str) -> Vec<u8> {
        let mut pdf: Vec<u8> = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(format!("4 0 obj\n{obj4_body}\nendobj\n").as_bytes());
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 5\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n",
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer = format!("trailer\n<</Size 5 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    /// Read obj-4's /AP/N on-state XObject content after generation, returning
    /// the raw content-stream bytes.
    fn btn_on_state_content(pdf_bytes: Vec<u8>) -> (Object, Vec<u8>) {
        let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).expect("parse");
        let on_ref = generate_button_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("generate")
            .expect("must produce appearance");
        let ap_n = match pdf.resolve(ObjectRef::new(4, 0)).expect("widget") {
            Object::Dictionary(w) => match w.get("AP") {
                Some(Object::Dictionary(ap)) => ap.get("N").cloned(),
                _ => None,
            },
            _ => None,
        };
        let Object::Stream(on) = pdf.resolve(on_ref).expect("on xobj") else {
            panic!("on xobj not a stream");
        };
        (ap_n.expect("AP/N present"), on.data)
    }

    #[test]
    fn btn_negative_ff_falls_back_to_checkbox_not_pushbutton() {
        // /Ff -1 (all bits set) must NOT be read as pushbutton/radio; treat the
        // malformed value as no flags and take the safe checkbox path (which
        // renders a ZapfDingbats glyph and stores a /Yes//Off state dict in /AP/N).
        let pdf = build_btn_pdf_obj4(
            "<</Type /Annot /Subtype /Widget /FT /Btn /T (c) /Ff -1 /Rect [10 10 30 30]>>",
        );
        let (ap_n, content) = btn_on_state_content(pdf);
        assert!(
            matches!(ap_n, Object::Dictionary(_)),
            "negative /Ff must take the checkbox path (/AP/N is a state dict), got: {ap_n:?}"
        );
        assert!(
            String::from_utf8_lossy(&content).contains("/ZaDb"),
            "checkbox on-state must render in ZapfDingbats"
        );
    }

    #[test]
    fn btn_checkbox_mk_ca_utf16be_is_decoded() {
        // /MK/CA stored as UTF-16BE (BOM FE FF + "4") must be decoded to the
        // ZapfDingbats code byte 0x34, not emitted raw (which would leak the BOM
        // bytes into the ZaDb Tj). A plain string is used verbatim.
        let pdf = build_btn_pdf_obj4(
            "<</Type /Annot /Subtype /Widget /FT /Btn /T (c) /MK <</CA <FEFF0034>>> /Rect [10 10 30 30]>>",
        );
        let (_ap_n, content) = btn_on_state_content(pdf);
        let mut tj_bytes: Option<Vec<u8>> = None;
        for tok in ContentStreamParser::new(&content).flatten() {
            if let ContentToken::Op { operands, operator } = tok {
                if operator == b"Tj" {
                    if let Some(Object::String(s)) = operands.first() {
                        tj_bytes = Some(s.clone());
                    }
                }
            }
        }
        assert_eq!(
            tj_bytes.as_deref(),
            Some(b"4" as &[u8]),
            "UTF-16BE /MK/CA must decode to the ZapfDingbats code byte 0x34"
        );
    }

    fn build_tx_no_value_pdf() -> Vec<u8> {
        let mut pdf: Vec<u8> = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Tx /T (f) /DA (/Helv 12 Tf 0 g) /Rect [10 10 100 30]>>\nendobj\n",
        );
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 5\n\
             0000000000 65535 f \n\
             {:010} 00000 n \n\
             {:010} 00000 n \n\
             {:010} 00000 n \n\
             {:010} 00000 n \n",
            off1, off2, off3, off4,
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer = format!("trailer\n<</Size 5 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    #[test]
    fn non_tx_field_returns_none() {
        // A widget with /FT /Btn should be skipped.
        let mut pdf = Pdf::open(Cursor::new(build_btn_widget_pdf())).expect("parse");
        let result = generate_text_field_appearance(&mut pdf, ObjectRef::new(4, 0));
        assert!(result.is_ok());
        assert!(result.unwrap().is_none(), "non-Tx field should return None");
    }

    #[test]
    fn missing_value_returns_none() {
        // A Tx widget with no /V should be skipped.
        let mut pdf = Pdf::open(Cursor::new(build_tx_no_value_pdf())).expect("parse");
        let result = generate_text_field_appearance(&mut pdf, ObjectRef::new(4, 0));
        assert!(result.is_ok());
        assert!(result.unwrap().is_none(), "missing /V should return None");
    }

    #[test]
    fn acroform_da_fallback_used_when_field_has_none() {
        // Widget has no /DA directly — must fall through to /AcroForm /DA.
        // We verify the appearance was generated (not None) and has a
        // valid Tf operator.
        let raw = build_minimal_tx_pdf(); // /AcroForm carries /DA
        let cursor = Cursor::new(raw);
        let mut pdf = Pdf::open(cursor).expect("parse");
        let widget_ref = ObjectRef::new(4, 0);
        let result = generate_text_field_appearance(&mut pdf, widget_ref)
            .expect("generate should not error");
        assert!(
            result.is_some(),
            "should produce appearance via AcroForm /DA fallback"
        );
    }

    /// Verify that multiline Td x-offsets do not accumulate across lines.
    ///
    /// For a left-aligned field (q=0), every line starts at x=2.  The first Td
    /// must be (2, first_y) and every subsequent Td must have x == 0 (delta
    /// from 2 to 2) rather than the absolute 2.
    #[test]
    fn multiline_td_x_offsets_are_deltas_not_absolute() {
        // Three words that should land on separate lines at a narrow width (30pt).
        let params = TextAppearanceParams {
            text_bytes: b"one two three".to_vec(),
            font_resource_name: b"Helv".to_vec(),
            font_size: 10.0,
            color: TextColor::Gray(0.0),
            bbox_w: 30.0, // narrow — forces each word to its own line
            bbox_h: 60.0,
            quadding: 0, // left-align
            multiline: true,
            std_font: StandardFont::from_base_name(b"Helv"),
        };
        let content = build_text_appearance_content(&params);

        let mut td_ops: Vec<(f64, f64)> = Vec::new();
        for tok in ContentStreamParser::new(&content).flatten() {
            let ContentToken::Op { operands, operator } = tok else {
                continue;
            };
            if operator.as_slice() == b"Td" {
                if let (Some(x_obj), Some(y_obj)) = (operands.first(), operands.get(1)) {
                    let x = x_obj
                        .as_real()
                        .or_else(|| x_obj.as_integer().map(|i| i as f64));
                    let y = y_obj
                        .as_real()
                        .or_else(|| y_obj.as_integer().map(|i| i as f64));
                    if let (Some(x), Some(y)) = (x, y) {
                        td_ops.push((x, y));
                    }
                }
            }
        }

        assert!(
            td_ops.len() >= 3,
            "expected at least 3 Td ops for 3 wrapped lines, got {}",
            td_ops.len()
        );

        // First Td: absolute x (2.0 for left-align), positive first_y.
        let (x0, y0) = td_ops[0];
        assert!(
            (x0 - 2.0).abs() < 0.01,
            "first Td x should be 2.0, got {x0}"
        );
        assert!(
            y0 > 0.0,
            "first Td y should be positive (first_y), got {y0}"
        );

        // Subsequent Td ops: x must be the delta from previous x.
        // For left-align, prev_x is always 2.0, so delta == 0.
        for (i, &(x, y)) in td_ops[1..].iter().enumerate() {
            assert!(
                x.abs() < 0.01,
                "Td[{}] x (delta) should be 0.0 for left-align, got {x}",
                i + 1
            );
            assert!(
                y < 0.0,
                "Td[{}] y should be negative (down-move), got {y}",
                i + 1
            );
        }
    }

    /// Build a minimal Tx-field PDF whose `/FT` and `/Q` are stored as
    /// **indirect references** (obj 5 = `/Tx`, obj 6 = `1`), to exercise
    /// indirect-reference resolution in the inherited-property walkers.
    fn build_pdf_with_indirect_field_props() -> Vec<u8> {
        let mut pdf: Vec<u8> = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"1 0 obj\n<</Type /Catalog /Pages 2 0 R /AcroForm \
              <</Fields [4 0 R] /DR <<>> /DA (/Helv 12 Tf 0 g)>>>>\nendobj\n",
        );
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Annots [4 0 R]>>\nendobj\n",
        );
        // /FT and /Q are indirect references (5 0 R / 6 0 R).
        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT 5 0 R /Q 6 0 R /T (field1) \
              /V (Hello World) /Rect [100 700 300 720] /P 3 0 R>>\nendobj\n",
        );
        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(b"5 0 obj\n/Tx\nendobj\n");
        let off6 = pdf.len() as u64;
        pdf.extend_from_slice(b"6 0 obj\n1\nendobj\n");

        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 7\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             {off5:010} 00000 n \n\
             {off6:010} 00000 n \n",
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer = format!("trailer\n<</Size 7 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    #[test]
    fn resolve_inherited_name_follows_indirect_reference() {
        // /FT stored as `5 0 R` (-> /Tx) must resolve to the name, not None.
        let raw = build_pdf_with_indirect_field_props();
        let mut pdf = Pdf::open(Cursor::new(raw)).unwrap();
        let ft = resolve_inherited_name(&mut pdf, ObjectRef::new(4, 0), b"FT").unwrap();
        assert_eq!(ft.as_deref(), Some(b"Tx" as &[u8]));
    }

    #[test]
    fn resolve_inherited_integer_follows_indirect_reference() {
        // /Q stored as `6 0 R` (-> 1) must resolve to the integer, not None.
        let raw = build_pdf_with_indirect_field_props();
        let mut pdf = Pdf::open(Cursor::new(raw)).unwrap();
        let q = resolve_inherited_integer(&mut pdf, ObjectRef::new(4, 0), b"Q").unwrap();
        assert_eq!(q, Some(1));
    }

    #[test]
    fn generate_appearance_recognizes_indirect_ft() {
        // With an indirect /FT, the field must still be recognized as a text
        // field and produce an appearance stream (it would be skipped if the
        // indirect /FT were not resolved).
        let raw = build_pdf_with_indirect_field_props();
        let mut pdf = Pdf::open(Cursor::new(raw)).unwrap();
        let result = generate_text_field_appearance(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(
            result.is_some(),
            "indirect /FT /Tx must be resolved so the field is not skipped"
        );
    }

    // ── Additional tests: compute_x_offset center / right ───────────────────

    #[test]
    fn compute_x_offset_center_quadding() {
        // Q=1 (centre): offset must be > 2.0 for a narrow text, > 2.0 boundary
        let params = TextAppearanceParams {
            text_bytes: b"Hi".to_vec(),
            font_resource_name: b"Helv".to_vec(),
            font_size: 12.0,
            color: TextColor::Gray(0.0),
            bbox_w: 200.0,
            bbox_h: 20.0,
            quadding: 1,
            multiline: false,
            std_font: StandardFont::from_base_name(b"Helvetica"),
        };
        let x = compute_x_offset(&params, &params.text_bytes);
        // Should be significantly more than 2.0 in a 200pt box
        assert!(
            x > 2.0,
            "center quadding x offset should exceed 2.0, got {x}"
        );
        // Should be less than bbox_w / 2 (not too far right)
        assert!(x < 100.0, "center quadding x should be < 100, got {x}");
    }

    #[test]
    fn compute_x_offset_right_quadding() {
        // Q=2 (right): text is flush-right with a 2pt margin
        let params = TextAppearanceParams {
            text_bytes: b"Hi".to_vec(),
            font_resource_name: b"Helv".to_vec(),
            font_size: 12.0,
            color: TextColor::Gray(0.0),
            bbox_w: 200.0,
            bbox_h: 20.0,
            quadding: 2,
            multiline: false,
            std_font: StandardFont::from_base_name(b"Helvetica"),
        };
        let x = compute_x_offset(&params, &params.text_bytes);
        // Should be well above 2.0 (right-aligned in 200pt box)
        assert!(
            x > 2.0,
            "right quadding x offset should exceed 2.0, got {x}"
        );
        // Should be less than bbox_w - 2 (there must be a right margin)
        assert!(x < 198.0, "right quadding x should be < 198, got {x}");
    }

    #[test]
    fn compute_x_offset_center_no_font_clamps_to_2() {
        // Q=1 with no std_font → string width is 0 → (bbox_w - 0) / 2
        let params = TextAppearanceParams {
            text_bytes: b"Hi".to_vec(),
            font_resource_name: b"Helv".to_vec(),
            font_size: 12.0,
            color: TextColor::Gray(0.0),
            bbox_w: 0.0, // degenerate bbox → clamped to 2.0
            bbox_h: 20.0,
            quadding: 1,
            multiline: false,
            std_font: None,
        };
        let x = compute_x_offset(&params, &params.text_bytes);
        // (0.0 - 0.0) / 2 = 0.0 → clamped to 2.0
        assert!(
            (x - 2.0).abs() < 0.001,
            "degenerate centre should clamp to 2.0, got {x}"
        );
    }

    #[test]
    fn compute_x_offset_right_no_font_clamps_to_2() {
        // Q=2 with no std_font → string width is 0 → bbox_w - 0 - 2
        // For a wide box this produces bbox_w - 2; for degenerate (0) it clamps
        let params = TextAppearanceParams {
            text_bytes: b"Hi".to_vec(),
            font_resource_name: b"Helv".to_vec(),
            font_size: 12.0,
            color: TextColor::Gray(0.0),
            bbox_w: 0.0, // degenerate → clamped to 2.0
            bbox_h: 20.0,
            quadding: 2,
            multiline: false,
            std_font: None,
        };
        let x = compute_x_offset(&params, &params.text_bytes);
        assert!(
            (x - 2.0).abs() < 0.001,
            "degenerate right should clamp to 2.0, got {x}"
        );
    }

    #[test]
    fn build_text_appearance_center_quadding_has_td() {
        // Q=1: content stream must include a Td operator for center-aligned text
        let params = TextAppearanceParams {
            text_bytes: b"Test".to_vec(),
            font_resource_name: b"Helv".to_vec(),
            font_size: 12.0,
            color: TextColor::Gray(0.0),
            bbox_w: 200.0,
            bbox_h: 20.0,
            quadding: 1,
            multiline: false,
            std_font: StandardFont::from_base_name(b"Helvetica"),
        };
        let content = build_text_appearance_content(&params);
        let s = String::from_utf8_lossy(&content);
        assert!(s.contains("Td"), "center quadding must emit a Td operator");
        assert!(s.contains("Tj"), "center quadding must emit a Tj operator");
    }

    #[test]
    fn build_text_appearance_right_quadding_has_td() {
        // Q=2: content stream must include a Td operator for right-aligned text
        let params = TextAppearanceParams {
            text_bytes: b"Test".to_vec(),
            font_resource_name: b"Helv".to_vec(),
            font_size: 12.0,
            color: TextColor::Gray(0.0),
            bbox_w: 200.0,
            bbox_h: 20.0,
            quadding: 2,
            multiline: false,
            std_font: StandardFont::from_base_name(b"Helvetica"),
        };
        let content = build_text_appearance_content(&params);
        let s = String::from_utf8_lossy(&content);
        assert!(s.contains("Td"), "right quadding must emit a Td operator");
        assert!(s.contains("Tj"), "right quadding must emit a Tj operator");
    }

    // ── word_wrap edge cases ─────────────────────────────────────────────────

    #[test]
    fn word_wrap_zero_max_width_returns_whole_text() {
        // max_width <= 0.0 → return whole text as one line (early return)
        let lines = word_wrap(
            b"Hello World",
            12.0,
            0.0,
            StandardFont::from_base_name(b"Helv"),
        );
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], b"Hello World");
    }

    #[test]
    fn word_wrap_negative_max_width_returns_whole_text() {
        // negative max_width also triggers the early return path
        let lines = word_wrap(
            b"Hello World",
            12.0,
            -1.0,
            StandardFont::from_base_name(b"Helv"),
        );
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], b"Hello World");
    }

    #[test]
    fn word_wrap_empty_text_returns_one_empty_line() {
        // An empty text must still produce one empty line
        let lines = word_wrap(b"", 12.0, 200.0, StandardFont::from_base_name(b"Helv"));
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], b"");
    }

    // ── to_winansi remaining CP1252 characters ───────────────────────────────

    #[test]
    fn to_winansi_remaining_cp1252_chars() {
        // Test remaining CP1252 typographic characters not covered by existing tests
        assert_eq!(to_winansi_bytes("\u{201A}"), b"\x82"); // ‚ Single Low-9 Quote
        assert_eq!(to_winansi_bytes("\u{0192}"), b"\x83"); // ƒ Latin f with hook
        assert_eq!(to_winansi_bytes("\u{201E}"), b"\x84"); // „ Double Low-9 Quote
        assert_eq!(to_winansi_bytes("\u{2020}"), b"\x86"); // † Dagger
        assert_eq!(to_winansi_bytes("\u{2021}"), b"\x87"); // ‡ Double Dagger
        assert_eq!(to_winansi_bytes("\u{02C6}"), b"\x88"); // ˆ Circumflex Accent
        assert_eq!(to_winansi_bytes("\u{2030}"), b"\x89"); // ‰ Per Mille Sign
        assert_eq!(to_winansi_bytes("\u{0160}"), b"\x8A"); // Š Capital S Caron
        assert_eq!(to_winansi_bytes("\u{2039}"), b"\x8B"); // ‹ Left Angle Quote
        assert_eq!(to_winansi_bytes("\u{017D}"), b"\x8E"); // Ž Capital Z Caron
        assert_eq!(to_winansi_bytes("\u{2018}"), b"\x91"); // ' Left Single Quote
        assert_eq!(to_winansi_bytes("\u{201D}"), b"\x94"); // " Right Double Quote
        assert_eq!(to_winansi_bytes("\u{2013}"), b"\x96"); // – En Dash
        assert_eq!(to_winansi_bytes("\u{02DC}"), b"\x98"); // ˜ Small Tilde
        assert_eq!(to_winansi_bytes("\u{0161}"), b"\x9A"); // š Small s Caron
        assert_eq!(to_winansi_bytes("\u{203A}"), b"\x9B"); // › Right Angle Quote
        assert_eq!(to_winansi_bytes("\u{017E}"), b"\x9E"); // ž Small z Caron
        assert_eq!(to_winansi_bytes("\u{0178}"), b"\x9F"); // Ÿ Capital Y Dieresis
    }

    // ── fmt_f64 edge cases ───────────────────────────────────────────────────

    #[test]
    fn fmt_f64_small_negative_rounds_to_minus_zero() {
        // -0.00001 rounds to "-0.0000" → strip trailing zeros/dot → "-0"
        // This is the actual output; the is_empty / "-" guard does not fire here.
        assert_eq!(fmt_f64(-0.00001), "-0");
    }

    // ── resolve_rect: indirect /Rect and degenerate cases ───────────────────

    /// PDF with /Rect stored as an indirect reference (obj 5 → array)
    fn build_pdf_with_indirect_rect() -> Vec<u8> {
        let mut pdf: Vec<u8> = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        // Widget with /Rect as indirect reference
        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Tx /T (f) \
              /DA (/Helv 12 Tf 0 g) /V (text) /Rect 5 0 R>>\nendobj\n",
        );
        // The actual rect array
        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(b"5 0 obj\n[100 700 300 720]\nendobj\n");
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 6\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             {off5:010} 00000 n \n",
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer = format!("trailer\n<</Size 6 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    #[test]
    fn resolve_rect_follows_indirect_reference() {
        // /Rect stored as indirect reference must be resolved correctly
        let raw = build_pdf_with_indirect_rect();
        let mut pdf = Pdf::open(Cursor::new(raw)).unwrap();
        let rect = resolve_rect(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(
            rect.is_some(),
            "indirect /Rect must resolve to Some(PageBox)"
        );
        let r = rect.unwrap();
        assert!((r.llx - 100.0).abs() < 0.001);
        assert!((r.ury - 720.0).abs() < 0.001);
    }

    #[test]
    fn generate_appearance_with_indirect_rect() {
        // generate_text_field_appearance must work when /Rect is indirect
        let raw = build_pdf_with_indirect_rect();
        let mut pdf = Pdf::open(Cursor::new(raw)).unwrap();
        let result = generate_text_field_appearance(&mut pdf, ObjectRef::new(4, 0));
        assert!(result.is_ok());
        assert!(
            result.unwrap().is_some(),
            "indirect /Rect must not cause None"
        );
    }

    /// PDF with /Rect having only 3 elements (degenerate — should return None)
    fn build_pdf_with_short_rect() -> Vec<u8> {
        let mut pdf: Vec<u8> = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off4 = pdf.len() as u64;
        // /Rect with only 3 elements
        pdf.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Tx /T (f) \
              /DA (/Helv 12 Tf 0 g) /V (text) /Rect [100 700 300]>>\nendobj\n",
        );
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 5\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n",
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer = format!("trailer\n<</Size 5 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    #[test]
    fn resolve_rect_short_array_returns_none() {
        let raw = build_pdf_with_short_rect();
        let mut pdf = Pdf::open(Cursor::new(raw)).unwrap();
        let rect = resolve_rect(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(rect.is_none(), "3-element /Rect must return None");
    }

    #[test]
    fn generate_appearance_short_rect_returns_none() {
        // A degenerate /Rect should cause generate to return Ok(None)
        let raw = build_pdf_with_short_rect();
        let mut pdf = Pdf::open(Cursor::new(raw)).unwrap();
        let result = generate_text_field_appearance(&mut pdf, ObjectRef::new(4, 0));
        assert!(result.is_ok());
        assert!(result.unwrap().is_none(), "short /Rect must cause None");
    }

    /// PDF with /Rect containing a non-numeric element
    fn build_pdf_with_nonnumeric_rect() -> Vec<u8> {
        let mut pdf: Vec<u8> = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off4 = pdf.len() as u64;
        // /Rect with non-numeric 3rd element (/Name)
        pdf.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Tx /T (f) \
              /DA (/Helv 12 Tf 0 g) /V (text) /Rect [100 700 /Bad 720]>>\nendobj\n",
        );
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 5\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n",
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer = format!("trailer\n<</Size 5 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    #[test]
    fn resolve_rect_nonnumeric_element_returns_none() {
        let raw = build_pdf_with_nonnumeric_rect();
        let mut pdf = Pdf::open(Cursor::new(raw)).unwrap();
        let rect = resolve_rect(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(
            rect.is_none(),
            "non-numeric element in /Rect must return None"
        );
    }

    // ── resolve_rect: widget is not a dictionary ─────────────────────────────

    #[test]
    fn resolve_rect_non_dict_object_returns_none() {
        // Use obj 5 from build_pdf_with_indirect_field_props (value /Tx, a Name)
        // resolve_rect on a non-dict object must return Ok(None)
        let raw = build_pdf_with_indirect_field_props();
        let mut pdf = Pdf::open(Cursor::new(raw)).unwrap();
        // obj 5 is /Tx (a Name, not a dict)
        let rect = resolve_rect(&mut pdf, ObjectRef::new(5, 0)).unwrap();
        assert!(
            rect.is_none(),
            "non-dict object must return None from resolve_rect"
        );
    }

    // ── resolve_da: field-level /DA ──────────────────────────────────────────

    /// PDF where the widget itself has a /DA key (not just AcroForm)
    fn build_pdf_with_field_da() -> Vec<u8> {
        let mut pdf: Vec<u8> = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        // AcroForm with /DA
        pdf.extend_from_slice(
            b"1 0 obj\n<</Type /Catalog /Pages 2 0 R /AcroForm \
              <</Fields [4 0 R] /DR <<>> /DA (/Helv 12 Tf 0 g)>>>>\nendobj\n",
        );
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Annots [4 0 R]>>\nendobj\n",
        );
        // Widget with its own /DA (should take precedence over AcroForm /DA)
        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Tx /T (f) \
              /DA (/Courier 10 Tf 0 g) /V (FieldDA) /Rect [10 10 200 30]>>\nendobj\n",
        );
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 5\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n",
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer = format!("trailer\n<</Size 5 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    #[test]
    fn resolve_da_field_level_takes_precedence() {
        // When the widget has /DA directly, it must be returned (not AcroForm /DA)
        let raw = build_pdf_with_field_da();
        let mut pdf = Pdf::open(Cursor::new(raw)).unwrap();
        let da = resolve_da(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(da.is_some(), "field-level /DA must be found");
        let da_bytes = da.unwrap();
        // Field DA starts with /Courier
        assert!(
            da_bytes.starts_with(b"/Courier"),
            "field /DA should be /Courier, got: {:?}",
            String::from_utf8_lossy(&da_bytes)
        );
    }

    /// PDF where /DA in the widget is an indirect reference
    fn build_pdf_with_indirect_da() -> Vec<u8> {
        let mut pdf: Vec<u8> = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        // Widget with /DA as an indirect reference (5 0 R)
        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Tx /T (f) \
              /DA 5 0 R /V (IndirectDA) /Rect [10 10 200 30]>>\nendobj\n",
        );
        // The /DA string
        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(b"5 0 obj\n(/Times-Roman 10 Tf 0 g)\nendobj\n");
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 6\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             {off5:010} 00000 n \n",
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer = format!("trailer\n<</Size 6 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    #[test]
    fn resolve_da_indirect_reference() {
        // /DA as an indirect reference must be resolved
        let raw = build_pdf_with_indirect_da();
        let mut pdf = Pdf::open(Cursor::new(raw)).unwrap();
        let da = resolve_da(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(da.is_some(), "indirect /DA must resolve to Some");
        let da_bytes = da.unwrap();
        assert!(
            da_bytes.contains(&b'T'),
            "indirect /DA value must include 'T', got: {:?}",
            String::from_utf8_lossy(&da_bytes)
        );
    }

    /// PDF where /AcroForm /DA is stored as an indirect reference
    fn build_pdf_with_indirect_acroform_da() -> Vec<u8> {
        let mut pdf: Vec<u8> = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        // AcroForm with /DA as indirect ref
        pdf.extend_from_slice(
            b"1 0 obj\n<</Type /Catalog /Pages 2 0 R /AcroForm \
              <</Fields [4 0 R] /DR <<>> /DA 5 0 R>>>>\nendobj\n",
        );
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Annots [4 0 R]>>\nendobj\n",
        );
        // Widget with NO /DA
        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Tx /T (f) \
              /V (AcroDA) /Rect [10 10 200 30]>>\nendobj\n",
        );
        // The /DA string
        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(b"5 0 obj\n(/Helv 12 Tf 0 g)\nendobj\n");
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 6\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             {off5:010} 00000 n \n",
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer = format!("trailer\n<</Size 6 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    #[test]
    fn resolve_da_acroform_indirect_da() {
        // /AcroForm /DA stored as indirect reference must be resolved
        let raw = build_pdf_with_indirect_acroform_da();
        let mut pdf = Pdf::open(Cursor::new(raw)).unwrap();
        let da = resolve_da(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(da.is_some(), "AcroForm indirect /DA must resolve to Some");
    }

    /// PDF with no /DA anywhere — neither field nor AcroForm
    fn build_pdf_with_no_da() -> Vec<u8> {
        let mut pdf: Vec<u8> = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        // AcroForm without /DA
        pdf.extend_from_slice(
            b"1 0 obj\n<</Type /Catalog /Pages 2 0 R /AcroForm \
              <</Fields [4 0 R] /DR <<>>>>>>\nendobj\n",
        );
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Tx /T (f) \
              /V (NoDA) /Rect [10 10 200 30]>>\nendobj\n",
        );
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 5\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n",
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer = format!("trailer\n<</Size 5 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    #[test]
    fn resolve_da_none_when_no_da_anywhere() {
        // No /DA in field or AcroForm → must return Ok(None)
        let raw = build_pdf_with_no_da();
        let mut pdf = Pdf::open(Cursor::new(raw)).unwrap();
        let da = resolve_da(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(da.is_none(), "no /DA anywhere must return None");
    }

    #[test]
    fn generate_appearance_with_no_da_uses_empty_da() {
        // generate_text_field_appearance must succeed even without /DA
        // (falls back to empty string → default params)
        let raw = build_pdf_with_no_da();
        let mut pdf = Pdf::open(Cursor::new(raw)).unwrap();
        let result = generate_text_field_appearance(&mut pdf, ObjectRef::new(4, 0));
        assert!(result.is_ok(), "no /DA must not cause error");
        assert!(
            result.unwrap().is_some(),
            "no /DA must still produce appearance"
        );
    }

    // ── /V as indirect reference ─────────────────────────────────────────────

    /// PDF where /V is an indirect reference to a string object
    fn build_pdf_with_indirect_v() -> Vec<u8> {
        let mut pdf: Vec<u8> = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"1 0 obj\n<</Type /Catalog /Pages 2 0 R /AcroForm \
              <</Fields [4 0 R] /DR <<>> /DA (/Helv 12 Tf 0 g)>>>>\nendobj\n",
        );
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Annots [4 0 R]>>\nendobj\n",
        );
        // Widget with /V as indirect reference (5 0 R)
        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Tx /T (f) \
              /V 5 0 R /Rect [10 10 200 30]>>\nendobj\n",
        );
        // The actual string value
        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(b"5 0 obj\n(IndirectValue)\nendobj\n");
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 6\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             {off5:010} 00000 n \n",
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer = format!("trailer\n<</Size 6 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    #[test]
    fn generate_appearance_with_indirect_v() {
        // /V stored as indirect reference must be rendered
        let raw = build_pdf_with_indirect_v();
        let mut pdf = Pdf::open(Cursor::new(raw)).unwrap();
        let result = generate_text_field_appearance(&mut pdf, ObjectRef::new(4, 0));
        assert!(result.is_ok(), "indirect /V must not cause error");
        assert!(
            result.unwrap().is_some(),
            "indirect /V must produce appearance"
        );
    }

    // ── install_normal_appearance: font_resource = None ──────────────────────

    #[test]
    fn install_normal_appearance_without_font_resource() {
        // When font_resource is None, no /Resources/Font is added.
        let raw = build_minimal_tx_pdf();
        let mut pdf = Pdf::open(Cursor::new(raw)).unwrap();
        let widget_ref = ObjectRef::new(4, 0);
        let content = b"q Q".to_vec();
        let result = install_normal_appearance(&mut pdf, widget_ref, content, 200.0, 20.0, None);
        assert!(result.is_ok(), "install without font resource must succeed");
        let xobj_ref = result.unwrap();
        let xobj = pdf.resolve(xobj_ref).unwrap();
        // Should be a stream without /Resources/Font
        if let Object::Stream(s) = xobj {
            assert!(
                s.dict.get("Resources").is_none(),
                "no font resource → no /Resources key in xobj"
            );
        }
    }

    // ── install_normal_appearance: existing /AP dict handling ────────────────

    /// PDF where the widget has an existing /AP dictionary
    fn build_pdf_with_existing_ap() -> Vec<u8> {
        let mut pdf: Vec<u8> = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"1 0 obj\n<</Type /Catalog /Pages 2 0 R /AcroForm \
              <</Fields [4 0 R] /DR <<>> /DA (/Helv 12 Tf 0 g)>>>>\nendobj\n",
        );
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Annots [4 0 R]>>\nendobj\n",
        );
        // Widget with existing /AP /N as a reference (obj 5)
        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Tx /T (f) \
              /V (text) /Rect [10 10 200 30] /AP <</N 5 0 R>>>>\nendobj\n",
        );
        // Existing appearance stream (will be replaced)
        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"5 0 obj\n<</Type /XObject /Subtype /Form /FormType 1 \
              /BBox [0 0 190 20]>>\nstream\nq Q\nendstream\nendobj\n",
        );
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 6\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             {off5:010} 00000 n \n",
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer = format!("trailer\n<</Size 6 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    #[test]
    fn generate_appearance_replaces_existing_ap() {
        // When widget has existing /AP, generate_text_field_appearance must
        // replace it with a new appearance (not panic or return None).
        let raw = build_pdf_with_existing_ap();
        let mut pdf = Pdf::open(Cursor::new(raw)).unwrap();
        let result = generate_text_field_appearance(&mut pdf, ObjectRef::new(4, 0));
        assert!(result.is_ok(), "existing /AP must not cause error");
        assert!(
            result.unwrap().is_some(),
            "existing /AP must still produce new appearance"
        );
    }

    // ── resolve_inherited_object: /V as indirect reference ──────────────────

    #[test]
    fn resolve_inherited_object_indirect_reference() {
        // obj 5 in build_pdf_with_indirect_field_props is `/Tx` (a Name).
        // resolve_inherited_object on obj 4 for key "V" gives a String directly.
        // But we can test the Reference arm by using build_pdf_with_indirect_v.
        let raw = build_pdf_with_indirect_v();
        let mut pdf = Pdf::open(Cursor::new(raw)).unwrap();
        let val = resolve_inherited_object(&mut pdf, ObjectRef::new(4, 0), b"V").unwrap();
        // /V is 5 0 R (a Reference) → resolve_inherited_object returns the Reference itself
        // (not the resolved object), so it must be Some(Object::Reference(...))
        assert!(
            val.is_some(),
            "indirect /V must not return None from resolve_inherited_object"
        );
    }

    // ── lookup_dr_basefont: no /DR → returns None ────────────────────────────

    #[test]
    fn lookup_dr_basefont_no_dr_returns_none() {
        // build_minimal_tx_pdf has /DR <<>> (empty), so lookup must return None
        // for any resource name that is not a known alias.
        let raw = build_minimal_tx_pdf();
        let mut pdf = Pdf::open(Cursor::new(raw)).unwrap();
        let sf = lookup_dr_basefont(&mut pdf, ObjectRef::new(4, 0), b"UnknownFont").unwrap();
        assert!(sf.is_none(), "empty /DR must return None");
    }

    /// PDF with /DR /Font at the field level (not just AcroForm)
    fn build_pdf_with_field_dr_font() -> Vec<u8> {
        let mut pdf: Vec<u8> = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        // Catalog has no AcroForm /DR
        pdf.extend_from_slice(
            b"1 0 obj\n<</Type /Catalog /Pages 2 0 R /AcroForm \
              <</Fields [4 0 R] /DR <<>> /DA (/MyFont 12 Tf 0 g)>>>>\nendobj\n",
        );
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Annots [4 0 R]>>\nendobj\n",
        );
        // Widget with its own /DR /Font /MyFont → /BaseFont /Courier-Bold
        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Tx /T (f) \
              /DR <</Font <</MyFont <</Type /Font /Subtype /Type1 /BaseFont /Courier-Bold>>>>>> \
              /V (FieldDR) /Rect [10 10 200 30]>>\nendobj\n",
        );
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 5\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n",
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer = format!("trailer\n<</Size 5 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    #[test]
    fn lookup_dr_basefont_field_level_dr() {
        // /DR /Font at the field level (not AcroForm) must be found
        let raw = build_pdf_with_field_dr_font();
        let mut pdf = Pdf::open(Cursor::new(raw)).unwrap();
        let sf = lookup_dr_basefont(&mut pdf, ObjectRef::new(4, 0), b"MyFont").unwrap();
        assert!(
            sf.is_some(),
            "field-level /DR /Font must resolve to a StandardFont"
        );
        assert_eq!(
            sf.unwrap(),
            StandardFont::CourierBold,
            "must resolve to Courier-Bold"
        );
    }

    #[test]
    fn generate_appearance_field_level_dr_font() {
        // generate_text_field_appearance with field-level /DR must produce appearance
        let raw = build_pdf_with_field_dr_font();
        let mut pdf = Pdf::open(Cursor::new(raw)).unwrap();
        let result = generate_text_field_appearance(&mut pdf, ObjectRef::new(4, 0));
        assert!(result.is_ok());
        assert!(
            result.unwrap().is_some(),
            "field-level /DR font must produce appearance"
        );
    }

    // ── multiline appearance via generate_text_field_appearance ─────────────

    /// PDF with multiline /Ff (bit 13 set = 4096) and multi-word /V
    fn build_pdf_multiline_tx() -> Vec<u8> {
        let mut pdf: Vec<u8> = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"1 0 obj\n<</Type /Catalog /Pages 2 0 R /AcroForm \
              <</Fields [4 0 R] /DR <<>> /DA (/Helv 12 Tf 0 g)>>>>\nendobj\n",
        );
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Annots [4 0 R]>>\nendobj\n",
        );
        // Widget with /Ff 4096 (multiline) and narrow rect → forces wrap
        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Tx /T (f) \
              /Ff 4096 /V (one two three four) /Rect [10 10 50 100]>>\nendobj\n",
        );
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 5\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n",
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer = format!("trailer\n<</Size 5 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    #[test]
    fn generate_multiline_appearance() {
        // /Ff 4096 (bit 13) must trigger multiline rendering
        let raw = build_pdf_multiline_tx();
        let mut pdf = Pdf::open(Cursor::new(raw)).unwrap();
        let result = generate_text_field_appearance(&mut pdf, ObjectRef::new(4, 0));
        assert!(result.is_ok(), "multiline /Ff must not cause error");
        let xobj_ref = result.unwrap();
        assert!(
            xobj_ref.is_some(),
            "multiline field must produce appearance"
        );
        let xobj_ref = xobj_ref.unwrap();
        let obj = pdf.resolve(xobj_ref).unwrap();
        if let Object::Stream(s) = obj {
            let content = String::from_utf8_lossy(&s.data);
            // Must have at least one Td and multiple Tj
            let td_count = content.matches("Td").count();
            assert!(
                td_count >= 1,
                "multiline must have Td operators, got {td_count}"
            );
        }
    }

    // ── center/right quadding via generate_text_field_appearance ────────────

    /// PDF with /Q 1 (center quadding)
    fn build_pdf_with_quadding(q: i64) -> Vec<u8> {
        let mut pdf: Vec<u8> = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"1 0 obj\n<</Type /Catalog /Pages 2 0 R /AcroForm \
              <</Fields [4 0 R] /DR <<>> /DA (/Helv 12 Tf 0 g)>>>>\nendobj\n",
        );
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Annots [4 0 R]>>\nendobj\n",
        );
        let off4 = pdf.len() as u64;
        let widget = format!(
            "4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Tx /T (f) \
              /Q {q} /V (Center) /Rect [10 10 200 30]>>\nendobj\n"
        );
        pdf.extend_from_slice(widget.as_bytes());
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 5\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n",
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer = format!("trailer\n<</Size 5 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    #[test]
    fn generate_appearance_center_quadding() {
        let raw = build_pdf_with_quadding(1);
        let mut pdf = Pdf::open(Cursor::new(raw)).unwrap();
        let result = generate_text_field_appearance(&mut pdf, ObjectRef::new(4, 0));
        assert!(result.is_ok());
        assert!(result.unwrap().is_some(), "Q=1 must produce appearance");
    }

    #[test]
    fn generate_appearance_right_quadding() {
        let raw = build_pdf_with_quadding(2);
        let mut pdf = Pdf::open(Cursor::new(raw)).unwrap();
        let result = generate_text_field_appearance(&mut pdf, ObjectRef::new(4, 0));
        assert!(result.is_ok());
        assert!(result.unwrap().is_some(), "Q=2 must produce appearance");
    }

    // ── resolve_da: cycle detection ──────────────────────────────────────────

    /// PDF where widget's /Parent forms a cycle (4→7→4)
    fn build_pdf_with_parent_cycle() -> Vec<u8> {
        let mut pdf: Vec<u8> = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        // No AcroForm /DA — force fallback to field chain walk where we hit cycle
        pdf.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        // Widget 4 → Parent 7
        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(b"4 0 obj\n<</Type /Annot /FT /Tx /T (f) /Parent 7 0 R>>\nendobj\n");
        // Dummy obj 5 and 6 for padding
        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(b"5 0 obj\n(dummy)\nendobj\n");
        let off6 = pdf.len() as u64;
        pdf.extend_from_slice(b"6 0 obj\n(dummy)\nendobj\n");
        // Obj 7 → Parent 4 (creates a cycle 4→7→4)
        let off7 = pdf.len() as u64;
        pdf.extend_from_slice(b"7 0 obj\n<</FT /Tx /Parent 4 0 R>>\nendobj\n");
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 8\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             {off5:010} 00000 n \n\
             {off6:010} 00000 n \n\
             {off7:010} 00000 n \n",
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer = format!("trailer\n<</Size 8 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    #[test]
    fn resolve_da_cycle_returns_none() {
        // A /Parent cycle must be detected and return Ok(None) without looping,
        // and must record a warning (qpdf-style: warn + fall back, not silent,
        // not abort).
        let raw = build_pdf_with_parent_cycle();
        let mut pdf = Pdf::open(Cursor::new(raw)).unwrap();
        let da = resolve_da(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(da.is_none(), "cycle in /Parent chain must return None");
        assert!(
            pdf.repair_diagnostics().entries().iter().any(|d| {
                d.severity == crate::Severity::Warning && d.message.contains("cycle detected")
            }),
            "a cycle in the /DA parent chain must record a warning, got: {:?}",
            pdf.repair_diagnostics().entries()
        );
    }

    #[test]
    fn resolve_inherited_name_cycle_returns_none() {
        // resolve_inherited_name must detect cycle and return Ok(None)
        let raw = build_pdf_with_parent_cycle();
        let mut pdf = Pdf::open(Cursor::new(raw)).unwrap();
        // Walk from obj 4 for key "DA" — no /DA on either node, cycle detected
        let result = resolve_inherited_name(&mut pdf, ObjectRef::new(4, 0), b"DA").unwrap();
        assert!(
            result.is_none(),
            "cycle must return None for resolve_inherited_name"
        );
    }

    #[test]
    fn resolve_inherited_object_cycle_returns_none() {
        let raw = build_pdf_with_parent_cycle();
        let mut pdf = Pdf::open(Cursor::new(raw)).unwrap();
        let result = resolve_inherited_object(&mut pdf, ObjectRef::new(4, 0), b"DA").unwrap();
        assert!(
            result.is_none(),
            "cycle must return None for resolve_inherited_object"
        );
    }

    #[test]
    fn resolve_inherited_integer_cycle_returns_none() {
        let raw = build_pdf_with_parent_cycle();
        let mut pdf = Pdf::open(Cursor::new(raw)).unwrap();
        let result = resolve_inherited_integer(&mut pdf, ObjectRef::new(4, 0), b"Q").unwrap();
        assert!(
            result.is_none(),
            "cycle must return None for resolve_inherited_integer"
        );
    }

    // ── resolve_inherited_name/object: not-a-dict node ──────────────────────

    #[test]
    fn resolve_inherited_name_non_dict_node_errors() {
        // obj 5 in build_pdf_with_indirect_field_props is /Tx (a Name), not a dict
        // → resolve_inherited_name should return Err (not a dictionary)
        let raw = build_pdf_with_indirect_field_props();
        let mut pdf = Pdf::open(Cursor::new(raw)).unwrap();
        // Start walk from obj 5 (not a dict)
        let result = resolve_inherited_name(&mut pdf, ObjectRef::new(5, 0), b"FT");
        assert!(
            result.is_err(),
            "non-dict node must return Err from resolve_inherited_name"
        );
    }

    #[test]
    fn resolve_inherited_object_non_dict_node_errors() {
        let raw = build_pdf_with_indirect_field_props();
        let mut pdf = Pdf::open(Cursor::new(raw)).unwrap();
        let result = resolve_inherited_object(&mut pdf, ObjectRef::new(5, 0), b"V");
        assert!(
            result.is_err(),
            "non-dict node must return Err from resolve_inherited_object"
        );
    }

    #[test]
    fn resolve_inherited_integer_non_dict_node_errors() {
        let raw = build_pdf_with_indirect_field_props();
        let mut pdf = Pdf::open(Cursor::new(raw)).unwrap();
        let result = resolve_inherited_integer(&mut pdf, ObjectRef::new(5, 0), b"Q");
        assert!(
            result.is_err(),
            "non-dict node must return Err from resolve_inherited_integer"
        );
    }

    // ── auto-size font: font_size branch ─────────────────────────────────────

    /// PDF with /DA that uses auto-size (font-size 0) to exercise the heuristic branch
    fn build_pdf_autosize_tx() -> Vec<u8> {
        let mut pdf: Vec<u8> = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        // /DA with font-size 0 → auto-size heuristic
        pdf.extend_from_slice(
            b"1 0 obj\n<</Type /Catalog /Pages 2 0 R /AcroForm \
              <</Fields [4 0 R] /DR <<>> /DA (/Helv 0 Tf 0 g)>>>>\nendobj\n",
        );
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Annots [4 0 R]>>\nendobj\n",
        );
        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Tx /T (f) \
              /V (AutoSize) /Rect [10 10 200 30]>>\nendobj\n",
        );
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 5\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n",
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer = format!("trailer\n<</Size 5 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    #[test]
    fn generate_appearance_autosize_font() {
        // /DA font-size 0 → auto_size heuristic branch must execute
        let raw = build_pdf_autosize_tx();
        let mut pdf = Pdf::open(Cursor::new(raw)).unwrap();
        let result = generate_text_field_appearance(&mut pdf, ObjectRef::new(4, 0));
        assert!(result.is_ok(), "auto-size /DA must not cause error");
        assert!(
            result.unwrap().is_some(),
            "auto-size must produce appearance"
        );
    }

    // ── Btn appearance tests ─────────────────────────────────────────────────

    use crate::generate_button_field_appearance;

    /// Non-Btn field → generate_button_field_appearance must return None.
    #[test]
    fn btn_non_btn_field_returns_none() {
        // /FT is /Tx — should return None for the Btn generator.
        let raw = build_tx_no_value_pdf();
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let result = generate_button_field_appearance(&mut pdf, ObjectRef::new(4, 0));
        assert!(result.is_ok());
        assert!(
            result.unwrap().is_none(),
            "Tx field must return None from Btn generator"
        );
    }

    /// Degenerate rect → None.
    #[test]
    fn btn_degenerate_rect_returns_none() {
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off4 = raw.len() as u64;
        // Rect with zero width (llx == urx)
        raw.extend_from_slice(b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Btn /T (chk) /Rect [10 10 10 30]>>\nendobj\n");
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 5\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n{off4:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 5 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let result = generate_button_field_appearance(&mut pdf, ObjectRef::new(4, 0));
        assert!(result.is_ok());
        assert!(
            result.unwrap().is_none(),
            "degenerate rect should return None"
        );
    }

    /// /Ff bit detection: pushbutton (bit 17 = 0x10000).
    ///
    /// Pushbutton appearance: the on-state XObject exists as a single /AP/N
    /// stream (not a dict), and it contains /Helv Tf.
    #[test]
    fn btn_pushbutton_ff_bit17_detected() {
        let ff_pushbutton = 0x10000_i64; // bit 17
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off4 = raw.len() as u64;
        let widget_bytes = format!(
            "4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Btn /T (pb) \
             /Ff {} /MK <</CA (OK)>> /Rect [10 10 80 30]>>\nendobj\n",
            ff_pushbutton
        );
        raw.extend_from_slice(widget_bytes.as_bytes());
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 5\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n{off4:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 5 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let result =
            generate_button_field_appearance(&mut pdf, ObjectRef::new(4, 0)).expect("generate");
        let xobj_ref = result.expect("pushbutton must produce appearance");

        // /AP/N must be a direct reference (single stream, not a sub-dict).
        let widget_obj = pdf.resolve(ObjectRef::new(4, 0)).expect("resolve widget");
        let Object::Dictionary(wdict) = widget_obj else {
            panic!("not dict")
        };
        let Object::Dictionary(ap_dict) = wdict.get("AP").expect("AP missing").clone() else {
            panic!("AP not dict")
        };
        // For pushbutton install_normal_appearance sets /AP/N as a direct Reference.
        let n_val = ap_dict.get("N").expect("N missing");
        assert!(
            matches!(n_val, Object::Reference(_)),
            "/AP/N must be a reference for pushbutton"
        );

        // The XObject stream must reference /Helv font.
        let Object::Stream(stream) = pdf.resolve(xobj_ref).expect("resolve xobj") else {
            panic!("not stream")
        };
        let content_str = String::from_utf8_lossy(&stream.data);
        assert!(
            content_str.contains("/Helv"),
            "pushbutton must use Helv font"
        );
        assert!(
            content_str.contains("Tf"),
            "pushbutton must have Tf operator"
        );

        // Caption "OK" must appear in a Tj.
        let mut found_caption = false;
        for tok in ContentStreamParser::new(&stream.data).flatten() {
            let ContentToken::Op { operands, operator } = tok else {
                continue;
            };
            if operator == b"Tj" {
                if let Some(Object::String(s)) = operands.first() {
                    if s == b"OK" {
                        found_caption = true;
                    }
                }
            }
        }
        assert!(found_caption, "pushbutton Tj must contain caption 'OK'");
    }

    /// checkbox (no Ff bits): generate_button_field_appearance produces on + off
    /// state appearances under /AP/N as a sub-dict with /<on> and /Off keys.
    #[test]
    fn btn_checkbox_ap_has_on_and_off_states() {
        let mut pdf = Pdf::open(Cursor::new(build_btn_widget_pdf())).expect("parse");
        let result =
            generate_button_field_appearance(&mut pdf, ObjectRef::new(4, 0)).expect("generate");
        let on_ref = result.expect("checkbox must produce appearance");

        // /AP/N must be a dict with "Yes" and "Off" keys.
        let widget_obj = pdf.resolve(ObjectRef::new(4, 0)).expect("resolve widget");
        let Object::Dictionary(wdict) = widget_obj else {
            panic!("not dict")
        };

        let Object::Dictionary(ap) = wdict.get("AP").expect("AP missing").clone() else {
            panic!("AP not dict")
        };

        let n_val = ap.get("N").expect("N missing");
        let Object::Dictionary(n_dict) = n_val else {
            panic!("N is not a dict for checkbox (should be state dict), got: {n_val:?}");
        };

        let yes_ref = match n_dict.get("Yes").expect("Yes key missing") {
            Object::Reference(r) => *r,
            other => panic!("Yes is not a ref: {other:?}"),
        };
        let off_ref_n = match n_dict.get("Off").expect("Off key missing") {
            Object::Reference(r) => *r,
            other => panic!("Off is not a ref: {other:?}"),
        };

        // on_ref returned from generate must equal the /Yes ref in /AP/N.
        assert_eq!(on_ref, yes_ref, "returned on_ref must match /AP/N/Yes ref");
        assert_ne!(yes_ref, off_ref_n, "on and off refs must differ");

        // /AP/D must also exist with Yes and Off.
        let d_val = ap.get("D").expect("D missing");
        let Object::Dictionary(d_dict) = d_val else {
            panic!("D is not a dict")
        };
        assert!(d_dict.get("Yes").is_some(), "/AP/D must have Yes key");
        assert!(d_dict.get("Off").is_some(), "/AP/D must have Off key");

        // on-state XObject stream must contain /ZaDb Tf and (4) Tj.
        let Object::Stream(on_stream) = pdf.resolve(on_ref).expect("resolve on xobj") else {
            panic!("on xobj not stream")
        };
        let on_str = String::from_utf8_lossy(&on_stream.data);
        assert!(
            on_str.contains("/ZaDb"),
            "on stream must contain /ZaDb font ref"
        );
        assert!(on_str.contains("Tf"), "on stream must have Tf operator");

        // Tj operand must be "4" (default checkbox glyph).
        let mut found_glyph = false;
        for tok in ContentStreamParser::new(&on_stream.data).flatten() {
            let ContentToken::Op { operands, operator } = tok else {
                continue;
            };
            if operator == b"Tj" {
                if let Some(Object::String(s)) = operands.first() {
                    if s == b"4" {
                        found_glyph = true;
                    }
                }
            }
        }
        assert!(found_glyph, "checkbox on stream Tj must contain glyph '4'");

        // off-state XObject stream must be blank.
        let Object::Stream(off_stream) = pdf.resolve(off_ref_n).expect("resolve off xobj") else {
            panic!("off xobj not stream")
        };
        // "q Q\n" is the only content; no Tj.
        let off_str = String::from_utf8_lossy(&off_stream.data);
        assert!(!off_str.contains("Tj"), "off stream must not contain Tj");

        // /AS must be set to /Off (default off display).
        assert_eq!(
            wdict.get("AS"),
            Some(&Object::Name(b"Off".to_vec())),
            "/AS must be /Off for a new checkbox"
        );
    }

    /// radio (Ff bit 16 = 0x8000): default glyph is 'l' (bullet), not '4'.
    #[test]
    fn btn_radio_uses_bullet_glyph() {
        let ff_radio = 0x8000_i64; // bit 16
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off4 = raw.len() as u64;
        let widget_bytes = format!(
            "4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Btn /T (rb) \
             /Ff {} /Rect [10 10 30 30]>>\nendobj\n",
            ff_radio
        );
        raw.extend_from_slice(widget_bytes.as_bytes());
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 5\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n{off4:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 5 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let on_ref = generate_button_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("generate")
            .expect("radio must produce appearance");

        let Object::Stream(on_stream) = pdf.resolve(on_ref).expect("resolve on") else {
            panic!("not stream")
        };

        // Tj must contain 'l' (radio bullet).
        let mut found = false;
        for tok in ContentStreamParser::new(&on_stream.data).flatten() {
            let ContentToken::Op { operands, operator } = tok else {
                continue;
            };
            if operator == b"Tj" {
                if let Some(Object::String(s)) = operands.first() {
                    if s == b"l" {
                        found = true;
                    }
                }
            }
        }
        assert!(found, "radio on stream Tj must contain glyph 'l'");
    }

    /// on-state name comes from existing /AP/N dict (non-Off key).
    #[test]
    fn btn_on_state_name_from_existing_ap_n_dict() {
        // Widget already has /AP/N << /On 5 0 R /Off 6 0 R >> (pre-existing).
        // The generator must pick "On" as the on-state name.
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        // Pre-existing /AP/N with "On" and "Off" keys (values are stubs, just refs).
        let off5 = raw.len() as u64;
        raw.extend_from_slice(b"5 0 obj\n<</Type /XObject /Subtype /Form /FormType 1 /BBox [0 0 20 20]>>\nstream\nq Q\nendstream\nendobj\n");
        let off6 = raw.len() as u64;
        raw.extend_from_slice(b"6 0 obj\n<</Type /XObject /Subtype /Form /FormType 1 /BBox [0 0 20 20]>>\nstream\nq Q\nendstream\nendobj\n");
        let off4 = raw.len() as u64;
        raw.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Btn /T (chk2) \
              /AP << /N << /On 5 0 R /Off 6 0 R >> >> /Rect [10 10 30 30]>>\nendobj\n",
        );
        let xref_start = raw.len() as u64;
        // 7 objects: 0 free, 1-6.
        let xref = format!(
            "xref\n0 7\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n\
             {off3:010} 00000 n \n{off4:010} 00000 n \n{off5:010} 00000 n \n{off6:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 7 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        generate_button_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("generate")
            .expect("should produce appearance");

        let widget_obj = pdf.resolve(ObjectRef::new(4, 0)).expect("resolve widget");
        let Object::Dictionary(wdict) = widget_obj else {
            panic!("not dict")
        };
        let Object::Dictionary(ap) = wdict.get("AP").expect("AP").clone() else {
            panic!("AP not dict")
        };
        let Object::Dictionary(n_dict) = ap.get("N").expect("N") else {
            panic!("N not dict")
        };

        // Must have "On" as the on-state key (picked from pre-existing /AP/N).
        assert!(
            n_dict.get("On").is_some(),
            "/AP/N must have 'On' key (from pre-existing)"
        );
        assert!(n_dict.get("Off").is_some(), "/AP/N must have 'Off' key");
        // Must NOT have "Yes" (should have used "On").
        assert!(
            n_dict.get("Yes").is_none(),
            "/AP/N must not have 'Yes' when 'On' was pre-existing"
        );
    }

    /// on-state name from /AS when no existing /AP/N dict.
    #[test]
    fn btn_on_state_name_from_as() {
        // Widget has /AS /Checked but no /AP.
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off4 = raw.len() as u64;
        raw.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Btn /T (chk3) \
              /AS /Checked /Rect [10 10 30 30]>>\nendobj\n",
        );
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 5\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n{off4:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 5 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        generate_button_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("generate")
            .expect("should produce appearance");

        let widget_obj = pdf.resolve(ObjectRef::new(4, 0)).expect("resolve widget");
        let Object::Dictionary(wdict) = widget_obj else {
            panic!("not dict")
        };
        let Object::Dictionary(ap) = wdict.get("AP").expect("AP").clone() else {
            panic!("AP not dict")
        };
        let Object::Dictionary(n_dict) = ap.get("N").expect("N") else {
            panic!("N not dict")
        };

        // Must have "Checked" (from /AS).
        assert!(
            n_dict.get("Checked").is_some(),
            "/AP/N must have 'Checked' key from /AS"
        );
        assert!(n_dict.get("Off").is_some(), "/AP/N must have 'Off' key");
    }

    /// /MK/CA overrides the default glyph.
    #[test]
    fn btn_mk_ca_overrides_default_glyph() {
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off4 = raw.len() as u64;
        // /MK/CA is "8" (custom checkmark glyph).
        raw.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Btn /T (chk4) \
              /MK <</CA (8)>> /Rect [10 10 30 30]>>\nendobj\n",
        );
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 5\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n{off4:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 5 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let on_ref = generate_button_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("generate")
            .expect("should produce appearance");

        let Object::Stream(on_stream) = pdf.resolve(on_ref).expect("resolve on") else {
            panic!("not stream")
        };

        let mut found_glyph = false;
        for tok in ContentStreamParser::new(&on_stream.data).flatten() {
            let ContentToken::Op { operands, operator } = tok else {
                continue;
            };
            if operator == b"Tj" {
                if let Some(Object::String(s)) = operands.first() {
                    if s == b"8" {
                        found_glyph = true;
                    }
                }
            }
        }
        assert!(
            found_glyph,
            "/MK/CA '8' must appear in Tj, overriding default '4'"
        );
    }

    /// Object refs for font, on, off must be distinct (no ref collision).
    #[test]
    fn btn_checkbox_object_refs_are_distinct() {
        let mut pdf = Pdf::open(Cursor::new(build_btn_widget_pdf())).expect("parse");
        generate_button_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("generate")
            .expect("must produce appearance");

        let widget_obj = pdf.resolve(ObjectRef::new(4, 0)).expect("resolve widget");
        let Object::Dictionary(wdict) = widget_obj else {
            panic!("not dict")
        };
        let Object::Dictionary(ap) = wdict.get("AP").expect("AP").clone() else {
            panic!("AP not dict")
        };
        let Object::Dictionary(n_dict) = ap.get("N").expect("N") else {
            panic!("N not dict")
        };

        let on_ref = match n_dict.get("Yes").expect("Yes") {
            Object::Reference(r) => *r,
            other => panic!("Yes not ref: {other:?}"),
        };
        let off_ref = match n_dict.get("Off").expect("Off") {
            Object::Reference(r) => *r,
            other => panic!("Off not ref: {other:?}"),
        };

        // Resolve both streams, find /Resources/Font/ZaDb -> font ref.
        let Object::Stream(on_stream) = pdf.resolve(on_ref).expect("resolve on") else {
            panic!("on not stream")
        };
        let Object::Stream(off_stream) = pdf.resolve(off_ref).expect("resolve off") else {
            panic!("off not stream")
        };

        // Extract font ref from /Resources/Font/ZaDb.
        let get_zadb_ref = |sdict: &Dictionary| -> ObjectRef {
            let Object::Dictionary(res) = sdict.get("Resources").expect("resources") else {
                panic!("resources not dict")
            };
            let Object::Dictionary(fonts) = res.get("Font").expect("Font") else {
                panic!("Font not dict")
            };
            match fonts.get("ZaDb").expect("ZaDb") {
                Object::Reference(r) => *r,
                other => panic!("ZaDb not ref: {other:?}"),
            }
        };

        let font_ref_on = get_zadb_ref(&on_stream.dict);
        let font_ref_off = get_zadb_ref(&off_stream.dict);

        // All three refs must be distinct.
        assert_ne!(on_ref, off_ref, "on and off refs must differ");
        assert_ne!(on_ref, font_ref_on, "on ref must differ from font ref");
        assert_ne!(off_ref, font_ref_on, "off ref must differ from font ref");

        // Both on/off share the same font ref (shared ZapfDingbats dict).
        assert_eq!(
            font_ref_on, font_ref_off,
            "on and off streams must share font ref"
        );
    }

    /// ZapfDingbats font dict must NOT have /Encoding.
    #[test]
    fn btn_zapf_font_dict_has_no_encoding() {
        let mut pdf = Pdf::open(Cursor::new(build_btn_widget_pdf())).expect("parse");
        generate_button_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("generate")
            .expect("must produce appearance");

        let widget_obj = pdf.resolve(ObjectRef::new(4, 0)).expect("resolve widget");
        let Object::Dictionary(wdict) = widget_obj else {
            panic!("not dict")
        };
        let Object::Dictionary(ap) = wdict.get("AP").expect("AP").clone() else {
            panic!("AP not dict")
        };
        let Object::Dictionary(n_dict) = ap.get("N").expect("N") else {
            panic!("N not dict")
        };
        let on_ref = match n_dict.get("Yes").expect("Yes") {
            Object::Reference(r) => *r,
            other => panic!("Yes not ref: {other:?}"),
        };
        let Object::Stream(on_stream) = pdf.resolve(on_ref).expect("resolve on") else {
            panic!("not stream")
        };
        let Object::Dictionary(res) = on_stream.dict.get("Resources").expect("resources") else {
            panic!("not dict")
        };
        let Object::Dictionary(fonts) = res.get("Font").expect("Font") else {
            panic!("not dict")
        };
        let font_ref = match fonts.get("ZaDb").expect("ZaDb") {
            Object::Reference(r) => *r,
            other => panic!("ZaDb not ref: {other:?}"),
        };
        let Object::Dictionary(fdict) = pdf.resolve(font_ref).expect("resolve font") else {
            panic!("font not dict")
        };
        assert!(
            fdict.get("Encoding").is_none(),
            "ZapfDingbats font dict must NOT have /Encoding, got: {:?}",
            fdict.get("Encoding")
        );
        assert_eq!(
            fdict.get("BaseFont"),
            Some(&Object::Name(b"ZapfDingbats".to_vec())),
            "font dict BaseFont must be ZapfDingbats"
        );
    }

    /// Round-trip: write and re-read the PDF; /AP must survive.
    #[test]
    fn btn_checkbox_round_trip() {
        let mut pdf = Pdf::open(Cursor::new(build_btn_widget_pdf())).expect("parse");
        let on_ref = generate_button_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("generate")
            .expect("must produce appearance");

        let mut out = Vec::new();
        write_pdf(&mut pdf, &mut out).expect("write_pdf");

        let mut pdf2 = Pdf::open(Cursor::new(out)).expect("re-parse");
        let widget_obj = pdf2.resolve(ObjectRef::new(4, 0)).expect("resolve widget");
        let Object::Dictionary(wdict) = widget_obj else {
            panic!("not dict")
        };
        let ap_obj = wdict.get("AP").expect("/AP missing after round-trip");
        assert!(
            matches!(ap_obj, Object::Dictionary(_)),
            "/AP must be a dict after round-trip"
        );

        // on-state XObject must be re-resolvable.
        let xobj2 = pdf2.resolve(on_ref).expect("re-resolve on xobj");
        assert!(
            matches!(xobj2, Object::Stream(_)),
            "on xobj must be a stream after round-trip"
        );
    }

    // ── Ch (choice) appearance tests ─────────────────────────────────────────

    use crate::generate_choice_field_appearance;

    /// Build a minimal PDF with a Ch combo widget.
    fn build_combo_pdf(value: &str, ff: i64) -> Vec<u8> {
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(
            b"1 0 obj\n<</Type /Catalog /Pages 2 0 R /AcroForm \
              <</Fields [4 0 R] /DR <<>> /DA (/Helv 12 Tf 0 g)>>>>\nendobj\n",
        );
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [4 0 R]>>\nendobj\n",
        );
        let off4 = raw.len() as u64;
        let widget = format!(
            "4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Ch /T (sel) \
             /Ff {} /V ({}) /DA (/Helv 12 Tf 0 g) \
             /Rect [100 700 300 720]>>\nendobj\n",
            ff, value
        );
        raw.extend_from_slice(widget.as_bytes());
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 5\n0000000000 65535 f \n\
             {off1:010} 00000 n \n{off2:010} 00000 n \n\
             {off3:010} 00000 n \n{off4:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 5 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        raw
    }

    /// Build a minimal PDF with a Ch list widget.
    ///
    /// `opts_str` is raw PDF array content, e.g. `"(Red) (Green) (Blue)"`.
    /// `sel_i_str` is the raw /I array, e.g. `"[1]"` or empty string for absent.
    /// `sel_v_str` is the raw /V value, e.g. `"(Green)"` or empty.
    fn build_list_pdf(opts_str: &str, sel_i_str: &str, sel_v_str: &str) -> Vec<u8> {
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(
            b"1 0 obj\n<</Type /Catalog /Pages 2 0 R /AcroForm \
              <</Fields [4 0 R] /DR <<>> /DA (/Helv 12 Tf 0 g)>>>>\nendobj\n",
        );
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [4 0 R]>>\nendobj\n",
        );
        let off4 = raw.len() as u64;
        let ff = 0_i64; // List box (combo bit not set)
        let i_entry = if sel_i_str.is_empty() {
            String::new()
        } else {
            format!("/I {sel_i_str} ")
        };
        let v_entry = if sel_v_str.is_empty() {
            String::new()
        } else {
            format!("/V {sel_v_str} ")
        };
        let widget = format!(
            "4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Ch /T (list) \
             /Ff {} /Opt [{opts_str}] {i_entry}{v_entry}\
             /DA (/Helv 10 Tf 0 g) /Rect [100 600 300 700]>>\nendobj\n",
            ff
        );
        raw.extend_from_slice(widget.as_bytes());
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 5\n0000000000 65535 f \n\
             {off1:010} 00000 n \n{off2:010} 00000 n \n\
             {off3:010} 00000 n \n{off4:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 5 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        raw
    }

    /// /Opt on a PARENT field, child widget carries only /Parent.
    ///
    ///  1 Catalog  2 Pages  3 Page  4 Widget(/Parent 5)  5 Ch field(/Opt,/V)
    fn build_inherited_opt_list_pdf() -> Vec<u8> {
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(
            b"1 0 obj\n<</Type /Catalog /Pages 2 0 R /AcroForm \
              <</Fields [5 0 R] /DR <<>> /DA (/Helv 10 Tf 0 g)>>>>\nendobj\n",
        );
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [4 0 R]>>\nendobj\n",
        );
        let off4 = raw.len() as u64;
        // Child widget: only /Parent + /Rect, no /FT, /Opt, /V of its own.
        raw.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /Parent 5 0 R \
              /Rect [100 600 300 700]>>\nendobj\n",
        );
        let off5 = raw.len() as u64;
        // Parent field: carries /FT, /Ff (list), /Opt, /V.
        raw.extend_from_slice(
            b"5 0 obj\n<</FT /Ch /T (list) /Ff 0 /Opt [(Red) (Green) (Blue)] \
              /V (Green) /Kids [4 0 R]>>\nendobj\n",
        );
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 6\n0000000000 65535 f \n\
             {off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n\
             {off4:010} 00000 n \n{off5:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 6 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        raw
    }

    /// Regression: /Opt inherited from a parent field must still populate the
    /// list appearance (the child widget has no direct /Opt).
    #[test]
    fn ch_list_inherits_opt_from_parent_field() {
        let mut pdf = Pdf::open(Cursor::new(build_inherited_opt_list_pdf())).expect("parse");
        let xobj_ref = generate_choice_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("generate")
            .expect("Ch widget handled");
        let Object::Stream(stream) = pdf.resolve(xobj_ref).expect("resolve xobj") else {
            panic!("not a stream");
        };
        // All three inherited options must be rendered as Tj strings.
        let mut tj_strings: Vec<Vec<u8>> = Vec::new();
        for tok in ContentStreamParser::new(&stream.data).flatten() {
            if let ContentToken::Op { operands, operator } = tok {
                if operator == b"Tj" {
                    if let Some(Object::String(b)) = operands.first() {
                        tj_strings.push(b.clone());
                    }
                }
            }
        }
        assert!(
            tj_strings.iter().any(|s| s == b"Red")
                && tj_strings.iter().any(|s| s == b"Green")
                && tj_strings.iter().any(|s| s == b"Blue"),
            "inherited /Opt not rendered: {tj_strings:?}"
        );
    }

    /// Parent field carries /Opt + /I; child widget has only /Parent + /Rect.
    fn build_inherited_i_list_pdf() -> Vec<u8> {
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(
            b"1 0 obj\n<</Type /Catalog /Pages 2 0 R /AcroForm \
              <</Fields [5 0 R] /DR <<>> /DA (/Helv 10 Tf 0 g)>>>>\nendobj\n",
        );
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [4 0 R]>>\nendobj\n",
        );
        let off4 = raw.len() as u64;
        raw.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /Parent 5 0 R \
              /Rect [100 600 300 700]>>\nendobj\n",
        );
        let off5 = raw.len() as u64;
        // Parent: list field with /I [1] (Green selected), no /V.
        raw.extend_from_slice(
            b"5 0 obj\n<</FT /Ch /T (list) /Ff 0 /Opt [(Red) (Green) (Blue)] \
              /I [1] /Kids [4 0 R]>>\nendobj\n",
        );
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 6\n0000000000 65535 f \n\
             {off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n\
             {off4:010} 00000 n \n{off5:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 6 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        raw
    }

    /// Regression: /I inherited from a parent field must drive the selection
    /// highlight (the child widget has no direct /I or /V).
    #[test]
    fn ch_list_inherits_i_from_parent_field() {
        let mut pdf = Pdf::open(Cursor::new(build_inherited_i_list_pdf())).expect("parse");
        let xobj_ref = generate_choice_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("generate")
            .expect("Ch widget handled");
        let Object::Stream(stream) = pdf.resolve(xobj_ref).expect("resolve xobj") else {
            panic!("not a stream");
        };
        // A highlight rectangle (re … f) must be emitted because a row is
        // selected via the inherited /I. With no selection there would be no
        // fill op preceding the text.
        let mut saw_re = false;
        let mut saw_fill = false;
        for tok in ContentStreamParser::new(&stream.data).flatten() {
            if let ContentToken::Op { operator, .. } = tok {
                if operator == b"re" {
                    saw_re = true;
                } else if operator == b"f" && saw_re {
                    saw_fill = true;
                }
            }
        }
        assert!(
            saw_fill,
            "inherited /I did not produce a selection highlight"
        );
    }

    /// List whose /V is an INDIRECT reference to a multi-select array.
    ///
    ///  1 Catalog 2 Pages 3 Page 4 Widget(/V 5 0 R) 5 [(Red)(Blue)]
    fn build_indirect_array_v_list_pdf() -> Vec<u8> {
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(
            b"1 0 obj\n<</Type /Catalog /Pages 2 0 R /AcroForm \
              <</Fields [4 0 R] /DR <<>> /DA (/Helv 10 Tf 0 g)>>>>\nendobj\n",
        );
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [4 0 R]>>\nendobj\n",
        );
        let off4 = raw.len() as u64;
        // Multi-select list, /V points to an indirect array.
        raw.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Ch /T (list) /Ff 0 \
              /Opt [(Red) (Green) (Blue)] /V 5 0 R \
              /DA (/Helv 10 Tf 0 g) /Rect [100 600 300 700]>>\nendobj\n",
        );
        let off5 = raw.len() as u64;
        raw.extend_from_slice(b"5 0 obj\n[(Red) (Blue)]\nendobj\n");
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 6\n0000000000 65535 f \n\
             {off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n\
             {off4:010} 00000 n \n{off5:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 6 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        raw
    }

    /// Regression: an indirect-reference /V resolving to a multi-select array
    /// must still highlight the selected rows (Red and Blue → two highlights).
    #[test]
    fn ch_list_indirect_array_v_highlights_multiselect() {
        let mut pdf = Pdf::open(Cursor::new(build_indirect_array_v_list_pdf())).expect("parse");
        let xobj_ref = generate_choice_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("generate")
            .expect("Ch widget handled");
        let Object::Stream(stream) = pdf.resolve(xobj_ref).expect("resolve xobj") else {
            panic!("not a stream");
        };
        // Two selected rows ⇒ two highlight fills (re … f).
        let fills = ContentStreamParser::new(&stream.data)
            .flatten()
            .filter(|t| matches!(t, ContentToken::Op { operator, .. } if operator == b"f"))
            .count();
        assert_eq!(
            fills, 2,
            "expected two selection highlights for Red+Blue, got {fills}"
        );
    }

    /// non-Ch field → generate_choice_field_appearance must return None.
    #[test]
    fn ch_non_ch_field_returns_none() {
        let mut pdf = Pdf::open(Cursor::new(build_btn_widget_pdf())).expect("parse");
        let result = generate_choice_field_appearance(&mut pdf, ObjectRef::new(4, 0));
        assert!(result.is_ok());
        assert!(
            result.unwrap().is_none(),
            "Btn field must return None from Ch generator"
        );
    }

    /// Degenerate rect → None.
    #[test]
    fn ch_degenerate_rect_returns_none() {
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off4 = raw.len() as u64;
        // Rect [10 10 10 30] — zero width
        raw.extend_from_slice(b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Ch /T (f) /Rect [10 10 10 30]>>\nendobj\n");
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 5\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n{off4:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 5 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let result = generate_choice_field_appearance(&mut pdf, ObjectRef::new(4, 0));
        assert!(result.is_ok());
        assert!(
            result.unwrap().is_none(),
            "degenerate rect should return None"
        );
    }

    /// Combo with /V "Blue" → single /AP/N stream containing "Blue" in a Tj.
    #[test]
    fn ch_combo_v_renders_as_tj() {
        let ff_combo = 0x20000_i64; // bit 18
        let raw = build_combo_pdf("Blue", ff_combo);
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let result =
            generate_choice_field_appearance(&mut pdf, ObjectRef::new(4, 0)).expect("generate");
        let xobj_ref = result.expect("combo must produce appearance");

        let Object::Stream(stream) = pdf.resolve(xobj_ref).expect("resolve xobj") else {
            panic!("not stream");
        };

        // /AP/N must be a single reference (not a sub-dict).
        let widget_obj = pdf.resolve(ObjectRef::new(4, 0)).expect("resolve widget");
        let Object::Dictionary(wdict) = widget_obj else {
            panic!("not dict")
        };
        let Object::Dictionary(ap_dict) = wdict.get("AP").expect("AP").clone() else {
            panic!("AP not dict")
        };
        assert!(
            matches!(ap_dict.get("N"), Some(Object::Reference(_))),
            "/AP/N must be a reference for combo"
        );

        // Content must have Tj with "Blue".
        let mut found = false;
        for tok in ContentStreamParser::new(&stream.data).flatten() {
            let ContentToken::Op { operands, operator } = tok else {
                continue;
            };
            if operator == b"Tj" {
                if let Some(Object::String(s)) = operands.first() {
                    if s == b"Blue" {
                        found = true;
                    }
                }
            }
        }
        assert!(found, "combo Tj must contain value 'Blue'");
    }

    /// Combo with no /V → blank appearance (Tj with empty or no string, but stream exists).
    #[test]
    fn ch_combo_no_value_blank_appearance() {
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(
            b"1 0 obj\n<</Type /Catalog /Pages 2 0 R /AcroForm \
              <</Fields [4 0 R] /DR <<>> /DA (/Helv 12 Tf 0 g)>>>>\nendobj\n",
        );
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [4 0 R]>>\nendobj\n",
        );
        let off4 = raw.len() as u64;
        // /Ff has combo bit; no /V
        raw.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Ch /T (c) \
              /Ff 131072 /DA (/Helv 12 Tf 0 g) /Rect [100 700 300 720]>>\nendobj\n",
        );
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 5\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n{off4:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 5 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let result =
            generate_choice_field_appearance(&mut pdf, ObjectRef::new(4, 0)).expect("generate");
        // Combo always generates (even blank), so this must be Some.
        assert!(
            result.is_some(),
            "combo without /V must still produce blank appearance"
        );
    }

    /// List with /Opt [(Red) (Green) (Blue)] and /I [1] → Green highlighted.
    #[test]
    fn ch_list_selected_index_highlighted() {
        let raw = build_list_pdf("(Red) (Green) (Blue)", "[1]", "");
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let result =
            generate_choice_field_appearance(&mut pdf, ObjectRef::new(4, 0)).expect("generate");
        let xobj_ref = result.expect("list must produce appearance");

        let Object::Stream(stream) = pdf.resolve(xobj_ref).expect("resolve xobj") else {
            panic!("not stream");
        };

        let content_str = String::from_utf8_lossy(&stream.data);

        // All three options must appear as Tj operands.
        let mut tj_vals: Vec<Vec<u8>> = Vec::new();
        for tok in ContentStreamParser::new(&stream.data).flatten() {
            let ContentToken::Op { operands, operator } = tok else {
                continue;
            };
            if operator == b"Tj" {
                if let Some(Object::String(s)) = operands.first() {
                    tj_vals.push(s.clone());
                }
            }
        }
        assert!(
            tj_vals.contains(&b"Red".to_vec()),
            "Red must appear in Tj; got: {tj_vals:?}"
        );
        assert!(
            tj_vals.contains(&b"Green".to_vec()),
            "Green must appear in Tj"
        );
        assert!(
            tj_vals.contains(&b"Blue".to_vec()),
            "Blue must appear in Tj"
        );

        // Highlight rect must appear (rg + re + f) for the selected row.
        assert!(
            content_str.contains("rg")
                && content_str.contains("re\n")
                && content_str.contains("f\n"),
            "selected row must have highlight (rg re f), got:\n{content_str}"
        );

        // Highlight must appear BEFORE BT (path ops outside text object).
        let bt_pos = content_str.find("BT").expect("BT missing");
        let rg_pos = content_str.find("rg").expect("rg missing");
        assert!(
            rg_pos < bt_pos,
            "highlight (rg) must appear before BT; rg@{rg_pos}, BT@{bt_pos}"
        );
    }

    /// List multi-select: /I [0 2] → Red and Blue highlighted.
    #[test]
    fn ch_list_multi_select_highlights() {
        let raw = build_list_pdf("(Red) (Green) (Blue)", "[0 2]", "");
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let xobj_ref = generate_choice_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("generate")
            .expect("must produce appearance");

        let Object::Stream(stream) = pdf.resolve(xobj_ref).expect("resolve xobj") else {
            panic!("not stream")
        };
        let content_str = String::from_utf8_lossy(&stream.data);

        // Two `f` operators expected (one per highlighted row).
        let f_count = content_str.lines().filter(|l| l.trim() == "f").count();
        assert_eq!(f_count, 2, "two selected rows → two 'f' ops; got {f_count}");
    }

    /// List: /V selection (no /I) matches by /Opt export.
    #[test]
    fn ch_list_v_selection_matches_opt_export() {
        // No /I; /V selects "Green".
        let raw = build_list_pdf("(Red) (Green) (Blue)", "", "(Green)");
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let xobj_ref = generate_choice_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("generate")
            .expect("must produce appearance");

        let Object::Stream(stream) = pdf.resolve(xobj_ref).expect("resolve xobj") else {
            panic!("not stream")
        };
        let content_str = String::from_utf8_lossy(&stream.data);

        // Exactly one highlight row.
        let f_count = content_str.lines().filter(|l| l.trim() == "f").count();
        assert_eq!(f_count, 1, "single /V match → one highlight; got {f_count}");

        // Highlight must be before BT.
        let bt_pos = content_str.find("BT").expect("BT missing");
        let rg_pos = content_str.find("rg");
        if let Some(rg) = rg_pos {
            assert!(rg < bt_pos, "highlight before BT");
        }
    }

    /// List with [export, display] /Opt pairs → display text rendered.
    #[test]
    fn ch_list_opt_export_display_pair() {
        // /Opt contains [[exportA displayA] [exportB displayB]].
        // We verify the display strings appear in Tj, not export strings.
        let raw = build_list_pdf("[(expA)(dispA)] [(expB)(dispB)]", "[0]", "");
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let xobj_ref = generate_choice_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("generate")
            .expect("must produce appearance");

        let Object::Stream(stream) = pdf.resolve(xobj_ref).expect("resolve xobj") else {
            panic!("not stream")
        };

        let mut tj_vals: Vec<Vec<u8>> = Vec::new();
        for tok in ContentStreamParser::new(&stream.data).flatten() {
            let ContentToken::Op { operands, operator } = tok else {
                continue;
            };
            if operator == b"Tj" {
                if let Some(Object::String(s)) = operands.first() {
                    tj_vals.push(s.clone());
                }
            }
        }
        assert!(
            tj_vals.contains(&b"dispA".to_vec()),
            "display string 'dispA' must appear in Tj; got {tj_vals:?}"
        );
        assert!(
            tj_vals.contains(&b"dispB".to_vec()),
            "display string 'dispB' must appear in Tj"
        );
        // Export strings must NOT appear.
        assert!(
            !tj_vals.contains(&b"expA".to_vec()),
            "export string must not appear in Tj"
        );
    }

    /// Combo with [export, display] /Opt: /V matches export → display rendered.
    #[test]
    fn ch_combo_opt_display_substitution() {
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(
            b"1 0 obj\n<</Type /Catalog /Pages 2 0 R /AcroForm \
              <</Fields [4 0 R] /DR <<>> /DA (/Helv 12 Tf 0 g)>>>>\nendobj\n",
        );
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [4 0 R]>>\nendobj\n",
        );
        let off4 = raw.len() as u64;
        // /V is "expX"; /Opt has [expX, DisplayX]. Combo should render "DisplayX".
        raw.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Ch /T (c2) \
              /Ff 131072 /V (expX) /Opt [[(expX)(DisplayX)] [(expY)(DisplayY)]] \
              /DA (/Helv 12 Tf 0 g) /Rect [100 700 300 720]>>\nendobj\n",
        );
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 5\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n{off4:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 5 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let xobj_ref = generate_choice_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("generate")
            .expect("must produce appearance");

        let Object::Stream(stream) = pdf.resolve(xobj_ref).expect("resolve xobj") else {
            panic!("not stream")
        };

        let mut found_display = false;
        for tok in ContentStreamParser::new(&stream.data).flatten() {
            let ContentToken::Op { operands, operator } = tok else {
                continue;
            };
            if operator == b"Tj" {
                if let Some(Object::String(s)) = operands.first() {
                    if s == b"DisplayX" {
                        found_display = true;
                    }
                }
            }
        }
        assert!(
            found_display,
            "combo must render display string 'DisplayX' from /Opt"
        );
    }

    /// Round-trip: write and re-read PDF; /AP survives.
    #[test]
    fn ch_list_round_trip() {
        let raw = build_list_pdf("(Red) (Green) (Blue)", "[1]", "");
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let xobj_ref = generate_choice_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("generate")
            .expect("must produce appearance");

        let mut out = Vec::new();
        write_pdf(&mut pdf, &mut out).expect("write_pdf");

        let mut pdf2 = Pdf::open(Cursor::new(out)).expect("re-parse");
        let widget_obj = pdf2.resolve(ObjectRef::new(4, 0)).expect("resolve widget");
        let Object::Dictionary(wdict) = widget_obj else {
            panic!("not dict")
        };
        assert!(wdict.get("AP").is_some(), "/AP missing after round-trip");

        let xobj2 = pdf2.resolve(xobj_ref).expect("re-resolve xobj");
        assert!(
            matches!(xobj2, Object::Stream(_)),
            "xobj must be stream after round-trip"
        );
    }

    // ── New tests for previously uncovered production branches ──────────────

    /// Btn widget with no /Rect key → generate_button_field_appearance returns Ok(None).
    #[test]
    fn btn_no_rect_returns_none() {
        let pdf = build_btn_pdf_obj4("<</Type /Annot /Subtype /Widget /FT /Btn /T (c)>>");
        let mut pdf = Pdf::open(Cursor::new(pdf)).expect("parse");
        let result = generate_button_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("must not error");
        assert!(result.is_none(), "missing /Rect must return None");
    }

    /// Pushbutton: /MK/CA stored as indirect reference to a string.
    #[test]
    fn btn_pushbutton_indirect_mk_ca() {
        // obj-5 holds the CA string; obj-4 widget references it via /MK/CA 5 0 R.
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off5 = raw.len() as u64;
        raw.extend_from_slice(b"5 0 obj\n(OK)\nendobj\n");
        let off4 = raw.len() as u64;
        // /Ff bit-17 (0x10000) = pushbutton; /MK/CA is 5 0 R (indirect string).
        raw.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Btn /T (pb) \
              /Ff 65536 /MK <</CA 5 0 R>> /Rect [10 10 80 30]>>\nendobj\n",
        );
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 6\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             {off5:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 6 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let result = generate_button_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("must not error");
        // Pushbutton appearance should be produced.
        assert!(
            result.is_some(),
            "pushbutton with indirect /MK/CA must produce appearance"
        );
    }

    /// Pushbutton: /MK dict present but no /CA key → empty caption fallback.
    #[test]
    fn btn_pushbutton_no_mk_ca_empty_caption() {
        // /Ff = 0x10000 (pushbutton); /MK has no /CA → caption_bytes = []
        let pdf = build_btn_pdf_obj4(
            "<</Type /Annot /Subtype /Widget /FT /Btn /T (pb2) \
             /Ff 65536 /MK <<>> /Rect [10 10 80 30]>>",
        );
        let mut pdf = Pdf::open(Cursor::new(pdf)).expect("parse");
        let result = generate_button_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("must not error");
        assert!(
            result.is_some(),
            "pushbutton with empty /MK must produce appearance"
        );
    }

    /// Checkbox: /AS stored as an indirect reference to a Name.
    #[test]
    fn btn_as_indirect_reference() {
        // obj-5 holds the Name /Checked; widget references it as /AS 5 0 R.
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off5 = raw.len() as u64;
        raw.extend_from_slice(b"5 0 obj\n/Checked\nendobj\n");
        let off4 = raw.len() as u64;
        // No /AP/N dict, /AS is 5 0 R (indirect name).
        raw.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Btn /T (chk_as_ind) \
              /AS 5 0 R /Rect [10 10 30 30]>>\nendobj\n",
        );
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 6\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             {off5:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 6 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        generate_button_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("must not error")
            .expect("must produce appearance");

        let widget_obj = pdf.resolve(ObjectRef::new(4, 0)).expect("resolve widget");
        let wdict = widget_obj.into_dict().expect("widget dict");
        let ap = wdict.get("AP").expect("/AP").clone();
        let ap_dict = ap.into_dict().expect("/AP dict");
        let n_dict_obj = ap_dict.get("N").expect("/AP/N").clone();
        let n_dict = n_dict_obj.into_dict().expect("/AP/N dict");
        // on-state must be "Checked" (from the indirectly-resolved /AS).
        assert!(
            n_dict.get(b"Checked".as_ref()).is_some(),
            "/AP/N must have 'Checked' from indirect /AS"
        );
    }

    /// Checkbox: /MK/CA stored as indirect reference to a string.
    #[test]
    fn btn_checkbox_indirect_mk_ca() {
        // obj-5 holds the glyph byte string (single char "8").
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off5 = raw.len() as u64;
        raw.extend_from_slice(b"5 0 obj\n(8)\nendobj\n");
        let off4 = raw.len() as u64;
        // /MK/CA is 5 0 R (indirect string).
        raw.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Btn /T (chk_ind_ca) \
              /MK <</CA 5 0 R>> /Rect [10 10 30 30]>>\nendobj\n",
        );
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 6\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             {off5:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 6 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let on_ref = generate_button_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("must not error")
            .expect("must produce appearance");

        // Confirm on_ref resolves to a stream (structure check).
        let on_obj = pdf.resolve(on_ref).expect("resolve on");
        assert!(
            matches!(on_obj, Object::Stream(_)),
            "on-state must be a stream"
        );
    }

    /// Checkbox: /MK/CA is empty string → use default glyph "4".
    #[test]
    fn btn_checkbox_empty_ca_uses_default_glyph() {
        let pdf = build_btn_pdf_obj4(
            "<</Type /Annot /Subtype /Widget /FT /Btn /T (c_empty_ca) \
             /MK <</CA ()>> /Rect [10 10 30 30]>>",
        );
        let (_ap_n, content) = btn_on_state_content(pdf);
        // Default glyph for checkbox is "4" (ZapfDingbats checkmark).
        let mut found_default = false;
        for tok in ContentStreamParser::new(&content).flatten() {
            if let ContentToken::Op { operands, operator } = tok {
                if operator == b"Tj" {
                    if let Some(Object::String(s)) = operands.first() {
                        if s == b"4" {
                            found_default = true;
                        }
                    }
                }
            }
        }
        assert!(
            found_default,
            "empty /MK/CA must fall back to default glyph '4'"
        );
    }

    /// Checkbox: /MK/CA is UTF-16BE with a character > 0xFF → use default glyph.
    #[test]
    fn btn_checkbox_utf16be_out_of_range_uses_default_glyph() {
        // FE FF 01 00 = UTF-16BE BOM + U+0100 (Ā), which is > 0xFF.
        let pdf = build_btn_pdf_obj4(
            "<</Type /Annot /Subtype /Widget /FT /Btn /T (c_utf16_oor) \
             /MK <</CA <FEFF0100>>> /Rect [10 10 30 30]>>",
        );
        let (_ap_n, content) = btn_on_state_content(pdf);
        let mut found_default = false;
        for tok in ContentStreamParser::new(&content).flatten() {
            if let ContentToken::Op { operands, operator } = tok {
                if operator == b"Tj" {
                    if let Some(Object::String(s)) = operands.first() {
                        if s == b"4" {
                            found_default = true;
                        }
                    }
                }
            }
        }
        assert!(
            found_default,
            "out-of-range UTF-16BE char must fall back to default glyph '4'"
        );
    }

    /// install_state_appearances: widget object is not a dictionary → Err returned.
    #[test]
    fn install_state_appearances_widget_not_dict() {
        // Build a minimal PDF; then replace obj-4 with a non-dict object (an integer).
        // Calling install_state_appearances must return an Err.
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off4 = raw.len() as u64;
        raw.extend_from_slice(b"4 0 obj\n42\nendobj\n");
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 5\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 5 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let result = install_state_appearances(
            &mut pdf,
            ObjectRef::new(4, 0),
            b"Yes",
            b"on_content".to_vec(),
            b"q Q\n".to_vec(),
            20.0,
            20.0,
        );
        assert!(
            result.is_err(),
            "non-dict widget must return Err from install_state_appearances"
        );
    }

    /// install_normal_appearance: widget object is not a dictionary → Err returned.
    #[test]
    fn install_normal_appearance_widget_not_dict() {
        // Build a minimal PDF; obj-4 is an integer, not a dict.
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off4 = raw.len() as u64;
        raw.extend_from_slice(b"4 0 obj\n99\nendobj\n");
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 5\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 5 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let result = install_normal_appearance(
            &mut pdf,
            ObjectRef::new(4, 0),
            b"q Q".to_vec(),
            100.0,
            20.0,
            None,
        );
        assert!(
            result.is_err(),
            "non-dict widget must return Err from install_normal_appearance"
        );
    }

    /// install_normal_appearance: widget /AP is an indirect reference to a dict.
    #[test]
    fn install_normal_appearance_ap_indirect_reference() {
        // obj-5 is an existing /AP dictionary; widget obj-4 has /AP 5 0 R.
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off5 = raw.len() as u64;
        // Pre-existing /AP dict (empty) stored as obj-5.
        raw.extend_from_slice(b"5 0 obj\n<<>>\nendobj\n");
        let off4 = raw.len() as u64;
        raw.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Tx /T (f) \
              /DA (/Helv 12 Tf 0 g) /V (hello) /Rect [10 10 200 30] \
              /AP 5 0 R>>\nendobj\n",
        );
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 6\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             {off5:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 6 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let result = install_normal_appearance(
            &mut pdf,
            ObjectRef::new(4, 0),
            b"q Q".to_vec(),
            190.0,
            20.0,
            None,
        );
        assert!(
            result.is_ok(),
            "indirect /AP must be handled without error: {:?}",
            result
        );
    }

    /// resolve_inherited_name: value resolves to Null → skip, keep walking.
    #[test]
    fn resolve_inherited_name_null_value() {
        // obj-5 is a Null object; widget /FT 5 0 R resolves to Null → walk to parent.
        // No parent → returns None, then generate_text_field_appearance returns None.
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off5 = raw.len() as u64;
        raw.extend_from_slice(b"5 0 obj\nnull\nendobj\n");
        let off4 = raw.len() as u64;
        // /FT 5 0 R → resolves to Null → treated as "no FT" → not Btn → None
        raw.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT 5 0 R /T (f) \
              /Rect [10 10 30 30]>>\nendobj\n",
        );
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 6\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             {off5:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 6 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        // /FT is Null → not Btn → generate_button returns None; not Tx → generate_text returns None.
        let result = generate_button_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("must not error");
        assert!(result.is_none(), "Null /FT must return None");
    }

    /// resolve_inherited_integer: value resolves to Null → skip (line 1054).
    #[test]
    fn resolve_inherited_integer_null_value() {
        // obj-5 is null; widget /Ff 5 0 R resolves to Null → skip, use default 0.
        // Result: checkbox (no pushbutton/radio bit set) → appearance produced.
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off5 = raw.len() as u64;
        raw.extend_from_slice(b"5 0 obj\nnull\nendobj\n");
        let off4 = raw.len() as u64;
        raw.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Btn /T (c_null_ff) \
              /Ff 5 0 R /Rect [10 10 30 30]>>\nendobj\n",
        );
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 6\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             {off5:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 6 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        // /Ff resolves to Null → defaults to 0 → checkbox → produces appearance.
        let result = generate_button_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("must not error");
        assert!(
            result.is_some(),
            "Null /Ff must fall back to 0 (checkbox) and produce appearance"
        );
    }

    /// resolve_inherited_integer: value resolves to non-integer (name) → skip (line 1056).
    #[test]
    fn resolve_inherited_integer_non_integer_value() {
        // obj-5 is /Name; widget /Ff 5 0 R → resolves to Name → skip → default 0 → checkbox.
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off5 = raw.len() as u64;
        raw.extend_from_slice(b"5 0 obj\n/BadType\nendobj\n");
        let off4 = raw.len() as u64;
        raw.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Btn /T (c_bad_ff) \
              /Ff 5 0 R /Rect [10 10 30 30]>>\nendobj\n",
        );
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 6\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             {off5:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 6 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let result = generate_button_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("must not error");
        assert!(
            result.is_some(),
            "non-integer /Ff must fall back to 0 and produce appearance"
        );
    }

    /// resolve_da: /DA value resolves to Null → skip (line 1121/1123).
    #[test]
    fn resolve_da_null_da_value() {
        // Widget /DA 5 0 R → Null; no AcroForm /DA → resolve_da returns None.
        // But generate_text_field_appearance still succeeds (uses default font).
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off5 = raw.len() as u64;
        raw.extend_from_slice(b"5 0 obj\nnull\nendobj\n");
        let off4 = raw.len() as u64;
        raw.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Tx /T (f) \
              /DA 5 0 R /V (hello) /Rect [10 10 200 30]>>\nendobj\n",
        );
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 6\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             {off5:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 6 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        // Null /DA → resolve_da returns None → generate falls back to defaults → still produces appearance.
        let result =
            generate_text_field_appearance(&mut pdf, ObjectRef::new(4, 0)).expect("must not error");
        assert!(
            result.is_some(),
            "Null /DA must not prevent appearance generation"
        );
    }

    /// resolve_da: /DA value resolves to non-String (integer) → skip (line 1123 else-arm).
    #[test]
    fn resolve_da_non_string_da_value() {
        // Widget /DA 5 0 R → Integer → skip; no AcroForm /DA → None.
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off5 = raw.len() as u64;
        raw.extend_from_slice(b"5 0 obj\n42\nendobj\n");
        let off4 = raw.len() as u64;
        raw.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Tx /T (f2) \
              /DA 5 0 R /V (world) /Rect [10 10 200 30]>>\nendobj\n",
        );
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 6\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             {off5:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 6 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let result =
            generate_text_field_appearance(&mut pdf, ObjectRef::new(4, 0)).expect("must not error");
        assert!(
            result.is_some(),
            "non-string /DA must not prevent appearance generation"
        );
    }

    /// resolve_da: AcroForm /DA is indirect reference to a string (lines 1164-1168).
    #[test]
    fn resolve_da_acroform_da_indirect_ref() {
        // AcroForm /DA 5 0 R → String → used as DA.
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off5 = raw.len() as u64;
        raw.extend_from_slice(b"5 0 obj\n(/Helv 12 Tf 0 g)\nendobj\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(
            b"1 0 obj\n<</Type /Catalog /Pages 2 0 R \
              /AcroForm <</Fields [4 0 R] /DA 5 0 R>>>>\nendobj\n",
        );
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off4 = raw.len() as u64;
        // Widget has no /DA itself → falls back to AcroForm /DA (indirect).
        raw.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Tx /T (f_ind_da) \
              /V (test) /Rect [10 10 200 30]>>\nendobj\n",
        );
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 6\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             {off5:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 6 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let result =
            generate_text_field_appearance(&mut pdf, ObjectRef::new(4, 0)).expect("must not error");
        assert!(
            result.is_some(),
            "indirect AcroForm /DA must produce appearance"
        );
    }

    /// resolve_da: AcroForm stored as indirect reference (lines 1151-1155).
    #[test]
    fn resolve_da_acroform_indirect_ref() {
        // /AcroForm 5 0 R → dict with /DA.
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off5 = raw.len() as u64;
        raw.extend_from_slice(b"5 0 obj\n<</Fields [4 0 R] /DA (/Helv 12 Tf 0 g)>>\nendobj\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(
            b"1 0 obj\n<</Type /Catalog /Pages 2 0 R /AcroForm 5 0 R>>\nendobj\n",
        );
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off4 = raw.len() as u64;
        raw.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Tx /T (f_acro_ind) \
              /V (hello) /Rect [10 10 200 30]>>\nendobj\n",
        );
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 6\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             {off5:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 6 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let result =
            generate_text_field_appearance(&mut pdf, ObjectRef::new(4, 0)).expect("must not error");
        assert!(
            result.is_some(),
            "indirect /AcroForm must produce appearance via indirect /DA"
        );
    }

    /// resolve_rect: /Rect is indirect reference to non-array → returns None (lines 1310/1313).
    #[test]
    fn resolve_rect_indirect_to_non_array() {
        // /Rect 5 0 R → integer (not array) → resolve_rect returns None → generate returns None.
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off5 = raw.len() as u64;
        raw.extend_from_slice(b"5 0 obj\n42\nendobj\n");
        let off4 = raw.len() as u64;
        raw.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Btn /T (c_bad_rect) \
              /Rect 5 0 R>>\nendobj\n",
        );
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 6\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             {off5:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 6 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let result = generate_button_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("must not error");
        assert!(
            result.is_none(),
            "indirect /Rect → non-array must return None"
        );
    }

    /// resolve_rect: /Rect array has non-numeric element → returns None (line 1323).
    #[test]
    fn resolve_rect_array_with_non_numeric_element() {
        // /Rect [10 10 /Bad 30] — last element is a Name, not a number.
        let pdf = build_btn_pdf_obj4(
            "<</Type /Annot /Subtype /Widget /FT /Btn /T (c_bad_rect_elem) \
             /Rect [10 10 /Bad 30]>>",
        );
        let mut pdf = Pdf::open(Cursor::new(pdf)).expect("parse");
        let result = generate_button_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("must not error");
        assert!(
            result.is_none(),
            "/Rect with non-numeric element must return None"
        );
    }

    /// Tx: /V is an indirect reference to a non-String → value_bytes = None (line 298).
    #[test]
    fn tx_indirect_v_non_string_returns_none() {
        // obj-5 is an integer; /V 5 0 R → resolves to integer → not a string → Ok(None).
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off5 = raw.len() as u64;
        raw.extend_from_slice(b"5 0 obj\n123\nendobj\n");
        let off4 = raw.len() as u64;
        raw.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Tx /T (f) \
              /DA (/Helv 12 Tf 0 g) /V 5 0 R /Rect [10 10 200 30]>>\nendobj\n",
        );
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 6\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             {off5:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 6 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let result =
            generate_text_field_appearance(&mut pdf, ObjectRef::new(4, 0)).expect("must not error");
        assert!(
            result.is_none(),
            "indirect /V → non-String must return None"
        );
    }

    /// Tx: /V is a non-String, non-Reference (e.g. Integer) → Ok(None) (line 301).
    #[test]
    fn tx_v_is_integer_returns_none() {
        // /V is a direct integer (malformed); must return Ok(None).
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off4 = raw.len() as u64;
        raw.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Tx /T (f) \
              /DA (/Helv 12 Tf 0 g) /V 42 /Rect [10 10 200 30]>>\nendobj\n",
        );
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 5\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 5 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let result =
            generate_text_field_appearance(&mut pdf, ObjectRef::new(4, 0)).expect("must not error");
        assert!(result.is_none(), "integer /V must return None");
    }

    /// Tx: /Rect dimensions too small → degenerate → Ok(None) (line 320).
    #[test]
    fn tx_degenerate_small_rect_returns_none() {
        // /Rect [10 10 10.5 30] → width = 0.5 < 1.0 → degenerate.
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off4 = raw.len() as u64;
        raw.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Tx /T (f) \
              /DA (/Helv 12 Tf 0 g) /V (test) /Rect [10 10 10.5 30]>>\nendobj\n",
        );
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 5\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 5 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let result =
            generate_text_field_appearance(&mut pdf, ObjectRef::new(4, 0)).expect("must not error");
        assert!(
            result.is_none(),
            "degenerate (narrow) /Rect must return None"
        );
    }

    /// lookup_dr_basefont: /BaseFont is indirect reference → resolves to name (lines 1277-1278).
    #[test]
    fn lookup_dr_basefont_basefont_indirect_ref() {
        // /AcroForm /DR /Font /Helv → dict with /BaseFont 6 0 R (indirect name).
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off6 = raw.len() as u64;
        // obj-6: the indirect BaseFont name.
        raw.extend_from_slice(b"6 0 obj\n/Helvetica\nendobj\n");
        let off5 = raw.len() as u64;
        // obj-5: font resource dict with /BaseFont 6 0 R.
        raw.extend_from_slice(
            b"5 0 obj\n<</Type /Font /Subtype /Type1 /BaseFont 6 0 R>>\nendobj\n",
        );
        let off1 = raw.len() as u64;
        raw.extend_from_slice(
            b"1 0 obj\n<</Type /Catalog /Pages 2 0 R \
              /AcroForm <</Fields [4 0 R] /DR <</Font <</Helv 5 0 R>>>> \
              /DA (/Helv 12 Tf 0 g)>>>>\nendobj\n",
        );
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off4 = raw.len() as u64;
        raw.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Tx /T (f_ind_bf) \
              /V (test) /Rect [10 10 200 30]>>\nendobj\n",
        );
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 7\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             {off5:010} 00000 n \n\
             {off6:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 7 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        // With /Helv→Helvetica mapped indirectly, appearance should use Helvetica font.
        let result =
            generate_text_field_appearance(&mut pdf, ObjectRef::new(4, 0)).expect("must not error");
        assert!(
            result.is_some(),
            "indirect /BaseFont must be resolved and produce appearance"
        );
    }

    /// resolve_inherited_name: resolves to a non-Name object → skip (line 952 else-arm).
    #[test]
    fn resolve_inherited_name_non_name_value() {
        // obj-5 is a String; /FT 5 0 R → resolves to String → not a Name → skip → returns None.
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off5 = raw.len() as u64;
        raw.extend_from_slice(b"5 0 obj\n(NotAName)\nendobj\n");
        let off4 = raw.len() as u64;
        raw.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT 5 0 R /T (f) \
              /Rect [10 10 30 30]>>\nendobj\n",
        );
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 6\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             {off5:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 6 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        // /FT resolves to a String → not a Name → resolve_inherited_name returns None
        // → neither Tx nor Btn → both generate functions return None.
        let btn_result = generate_button_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("must not error");
        assert!(
            btn_result.is_none(),
            "non-Name /FT must return None from Btn generator"
        );
    }

    /// resolve_inherited_object: /V resolves to Null → returns None (line 996).
    #[test]
    fn resolve_inherited_object_null_value() {
        // obj-5 is null; /V 5 0 R → Null → resolve_inherited_object returns None → Ok(None).
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off5 = raw.len() as u64;
        raw.extend_from_slice(b"5 0 obj\nnull\nendobj\n");
        let off4 = raw.len() as u64;
        raw.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Tx /T (f_null_v) \
              /DA (/Helv 12 Tf 0 g) /V 5 0 R /Rect [10 10 200 30]>>\nendobj\n",
        );
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 6\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             {off5:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 6 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        // /V is Null → no value to render → Ok(None).
        let result =
            generate_text_field_appearance(&mut pdf, ObjectRef::new(4, 0)).expect("must not error");
        assert!(result.is_none(), "Null /V must return None");
    }

    /// resolve_da: /DA parent node is not a dictionary → warning, fall back (line 1108/1111).
    /// Calls resolve_da directly so resolve_inherited_name's same check doesn't interfere.
    #[test]
    fn resolve_da_parent_is_not_dict() {
        // Widget /Parent 5 0 R where obj-5 is an integer (not a dict).
        // resolve_da should warn and fall back to /AcroForm /DA.
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off5 = raw.len() as u64;
        raw.extend_from_slice(b"5 0 obj\n99\nendobj\n"); // non-dict parent
        let off1 = raw.len() as u64;
        raw.extend_from_slice(
            b"1 0 obj\n<</Type /Catalog /Pages 2 0 R \
              /AcroForm <</Fields [4 0 R] /DA (/Helv 12 Tf 0 g)>>>>\nendobj\n",
        );
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off4 = raw.len() as u64;
        // Widget with /Parent 5 0 R (non-dict) but no /FT (so generate functions skip early).
        raw.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /T (f_ndict_parent) \
              /Parent 5 0 R>>\nendobj\n",
        );
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 6\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             {off5:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 6 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        // Call resolve_da directly: non-dict parent → warning + fall back to /AcroForm /DA.
        let da = resolve_da(&mut pdf, ObjectRef::new(4, 0)).expect("must not error");
        // Should have found /AcroForm /DA = "/Helv 12 Tf 0 g".
        assert!(
            da.is_some(),
            "non-dict /Parent must fall back to AcroForm /DA"
        );
    }

    /// lookup_dr_basefont: /DR /Font missing → skip (lines 1245-1249, 1273).
    #[test]
    fn lookup_dr_basefont_no_font_in_dr() {
        // /AcroForm /DR has no /Font key → lookup_dr_basefont returns None → default font used.
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(
            b"1 0 obj\n<</Type /Catalog /Pages 2 0 R \
              /AcroForm <</Fields [4 0 R] /DR <<>> /DA (/Helv 12 Tf 0 g)>>>>\nendobj\n",
        );
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off4 = raw.len() as u64;
        raw.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Tx /T (f_no_dr_font) \
              /V (test) /Rect [10 10 200 30]>>\nendobj\n",
        );
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 5\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 5 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        // /DR has no /Font → font lookup fails → default Helvetica is used → appearance produced.
        let result =
            generate_text_field_appearance(&mut pdf, ObjectRef::new(4, 0)).expect("must not error");
        assert!(
            result.is_some(),
            "missing /DR /Font must fall back to default font"
        );
    }

    /// lookup_dr_basefont: AcroForm /DR produced from indirect AcroForm (lines 1263-1264).
    #[test]
    fn lookup_dr_basefont_from_indirect_acroform_dr() {
        // /AcroForm 5 0 R → dict with /DR /Font /Helv → Helvetica.
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off5 = raw.len() as u64;
        raw.extend_from_slice(
            b"5 0 obj\n<</Fields [4 0 R] /DR <</Font <</Helv \
              <</Type /Font /Subtype /Type1 /BaseFont /Helvetica \
              /Encoding /WinAnsiEncoding>>>>>> /DA (/Helv 12 Tf 0 g)>>\nendobj\n",
        );
        let off1 = raw.len() as u64;
        raw.extend_from_slice(
            b"1 0 obj\n<</Type /Catalog /Pages 2 0 R /AcroForm 5 0 R>>\nendobj\n",
        );
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off4 = raw.len() as u64;
        raw.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Tx /T (f_ind_acro_dr) \
              /V (test) /Rect [10 10 200 30]>>\nendobj\n",
        );
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 6\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             {off5:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 6 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let result =
            generate_text_field_appearance(&mut pdf, ObjectRef::new(4, 0)).expect("must not error");
        assert!(
            result.is_some(),
            "indirect AcroForm /DR must yield appearance"
        );
    }

    /// lookup_dr_basefont: /BaseFont matches a standard font → Some(sf) returned (lines 1283-1284).
    #[test]
    fn lookup_dr_basefont_standard_font_matched() {
        // /AcroForm /DR /Font /TimesR /BaseFont /Times-Roman → StandardFont::TimesRoman.
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(
            b"1 0 obj\n<</Type /Catalog /Pages 2 0 R \
              /AcroForm <</Fields [4 0 R] \
              /DR <</Font <</TimesR <</Type /Font /Subtype /Type1 /BaseFont /Times-Roman>>>>>> \
              /DA (/TimesR 12 Tf 0 g)>>>>\nendobj\n",
        );
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );
        let off4 = raw.len() as u64;
        raw.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Tx /T (f_times) \
              /V (test) /Rect [10 10 200 30]>>\nendobj\n",
        );
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 5\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 5 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let result =
            generate_text_field_appearance(&mut pdf, ObjectRef::new(4, 0)).expect("must not error");
        assert!(
            result.is_some(),
            "Times-Roman /BaseFont in /DR must produce appearance"
        );
    }

    // ── Additional Ch renderer coverage tests ─────────────────────────────────

    /// Build a minimal Ch-widget PDF with a custom obj-4 body.
    ///
    /// Uses 5 objects (1=Catalog, 2=Pages, 3=Page, 4=Widget).
    fn build_ch_pdf_obj4(obj4_body: &str) -> Vec<u8> {
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(
            b"1 0 obj\n<</Type /Catalog /Pages 2 0 R /AcroForm \
              <</Fields [4 0 R] /DR <<>> /DA (/Helv 12 Tf 0 g)>>>>\nendobj\n",
        );
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [4 0 R]>>\nendobj\n",
        );
        let off4 = raw.len() as u64;
        raw.extend_from_slice(format!("4 0 obj\n{obj4_body}\nendobj\n").as_bytes());
        let xref_start = raw.len() as u64;
        let xref = format!(
            "xref\n0 5\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n"
        );
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size 5 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        raw
    }

    /// Build a Ch-widget PDF with a custom obj-4 body plus additional extra objects
    /// (obj-5 onward). `extra_objs` is a list of raw object body strings; they are
    /// written as obj-5, obj-6, … in order.
    fn build_ch_pdf_with_extras(obj4_body: &str, extra_objs: &[&str]) -> Vec<u8> {
        let n = 5 + extra_objs.len();
        let mut raw = Vec::<u8>::new();
        raw.extend_from_slice(b"%PDF-1.4\n");
        let off1 = raw.len() as u64;
        raw.extend_from_slice(
            b"1 0 obj\n<</Type /Catalog /Pages 2 0 R /AcroForm \
              <</Fields [4 0 R] /DR <<>> /DA (/Helv 12 Tf 0 g)>>>>\nendobj\n",
        );
        let off2 = raw.len() as u64;
        raw.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = raw.len() as u64;
        raw.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [4 0 R]>>\nendobj\n",
        );
        let off4 = raw.len() as u64;
        raw.extend_from_slice(format!("4 0 obj\n{obj4_body}\nendobj\n").as_bytes());
        let mut extra_offsets = Vec::new();
        for body in extra_objs {
            extra_offsets.push(raw.len() as u64);
            let idx = extra_offsets.len() + 4; // obj-5, obj-6, …
            raw.extend_from_slice(format!("{idx} 0 obj\n{body}\nendobj\n").as_bytes());
        }
        let xref_start = raw.len() as u64;
        let mut xref = format!("xref\n0 {n}\n0000000000 65535 f \n");
        for off in [off1, off2, off3, off4].iter().chain(extra_offsets.iter()) {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        raw.extend_from_slice(xref.as_bytes());
        raw.extend_from_slice(
            format!("trailer\n<</Size {n} /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        raw
    }

    /// Extract the content-stream bytes from the /AP/N XObject generated for obj-4.
    fn ch_ap_content(pdf: &mut Pdf<Cursor<Vec<u8>>>, widget: ObjectRef) -> Vec<u8> {
        let xobj_ref = generate_choice_field_appearance(pdf, widget)
            .expect("generate")
            .expect("must produce appearance");
        match pdf.resolve(xobj_ref).expect("resolve xobj") {
            Object::Stream(s) => s.data,
            other => panic!("xobj must be a stream, got {other:?}"),
        }
    }

    /// Collect all Tj string operands from a content stream.
    fn tj_strings_from(content: &[u8]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for tok in ContentStreamParser::new(content).flatten() {
            if let ContentToken::Op { operands, operator } = tok {
                if operator == b"Tj" {
                    if let Some(Object::String(s)) = operands.first() {
                        out.push(s.clone());
                    }
                }
            }
        }
        out
    }

    /// Count `f` (fill) operators in a content stream (each is one highlight rect).
    fn count_fills(content: &[u8]) -> usize {
        ContentStreamParser::new(content)
            .flatten()
            .filter(|t| matches!(t, ContentToken::Op { operator, .. } if operator == b"f"))
            .count()
    }

    // ── Ch: missing /Rect → None (line 1569) ────────────────────────────────

    /// Ch widget with no /Rect key → generate_choice_field_appearance returns None.
    #[test]
    fn ch_no_rect_returns_none() {
        let raw =
            build_ch_pdf_obj4("<</Type /Annot /Subtype /Widget /FT /Ch /T (c) /Ff 131072 /V (A)>>");
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let result = generate_choice_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("must not error");
        assert!(result.is_none(), "missing /Rect must return None");
    }

    // ── Ch: DA font not standard-14 → lookup_dr_basefont (lines 1579-1580) ──

    /// /DA with a non-standard font name → falls back to Helvetica.
    #[test]
    fn ch_combo_non_standard_da_font() {
        // /DA uses /F1 which is not in the Standard 14; no /DR entry for it either.
        let raw = build_ch_pdf_obj4(
            "<</Type /Annot /Subtype /Widget /FT /Ch /T (c) /Ff 131072 \
             /V (Hello) /DA (/F1 10 Tf 0 g) /Rect [100 700 300 720]>>",
        );
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let content = ch_ap_content(&mut pdf, ObjectRef::new(4, 0));
        // Must not panic; Tj must contain the value.
        let tjs = tj_strings_from(&content);
        assert!(
            tjs.contains(&b"Hello".to_vec()),
            "value must appear in Tj; got {tjs:?}"
        );
    }

    // ── Ch: combo /V as indirect reference → string (lines 1628-1634) ────────

    /// Combo /V stored as indirect reference to a string (line 1628-1633).
    #[test]
    fn ch_combo_v_indirect_string() {
        // obj-5 holds the string "Indirect"; obj-4 /V 5 0 R.
        let raw = build_ch_pdf_with_extras(
            "<</Type /Annot /Subtype /Widget /FT /Ch /T (c) /Ff 131072 \
             /V 5 0 R /DA (/Helv 12 Tf 0 g) /Rect [100 700 300 720]>>",
            &["(Indirect)"],
        );
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let content = ch_ap_content(&mut pdf, ObjectRef::new(4, 0));
        let tjs = tj_strings_from(&content);
        assert!(
            tjs.contains(&b"Indirect".to_vec()),
            "indirect /V string must appear in Tj; got {tjs:?}"
        );
    }

    /// Combo /V as indirect reference to a non-string (e.g. integer) → empty text (line 1634).
    #[test]
    fn ch_combo_v_indirect_non_string() {
        // obj-5 is an integer, not a string.
        let raw = build_ch_pdf_with_extras(
            "<</Type /Annot /Subtype /Widget /FT /Ch /T (c) /Ff 131072 \
             /V 5 0 R /DA (/Helv 12 Tf 0 g) /Rect [100 700 300 720]>>",
            &["42"],
        );
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        // Must produce a (blank) appearance without panic.
        let result = generate_choice_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("must not error");
        assert!(
            result.is_some(),
            "combo must still produce appearance for non-string indirect /V"
        );
    }

    /// Combo /V is a direct non-string value (e.g. Name or Array) → empty text (line 1637).
    #[test]
    fn ch_combo_v_direct_non_string() {
        // /V /SomeName — not a String, not a Reference.
        let raw = build_ch_pdf_obj4(
            "<</Type /Annot /Subtype /Widget /FT /Ch /T (c) /Ff 131072 \
             /V /SomeName /DA (/Helv 12 Tf 0 g) /Rect [100 700 300 720]>>",
        );
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let result = generate_choice_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("must not error");
        assert!(
            result.is_some(),
            "combo must still produce appearance for non-string /V"
        );
    }

    // ── Ch: combo auto-size (line 1646) ─────────────────────────────────────

    /// Combo /DA with auto-size (0 pt) → font size is clamped from bbox height.
    #[test]
    fn ch_combo_auto_size() {
        // /DA with 0 font size → auto-size path.
        let raw = build_ch_pdf_obj4(
            "<</Type /Annot /Subtype /Widget /FT /Ch /T (c) /Ff 131072 \
             /V (Auto) /DA (/Helv 0 Tf 0 g) /Rect [100 700 300 720]>>",
        );
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let content = ch_ap_content(&mut pdf, ObjectRef::new(4, 0));
        let tjs = tj_strings_from(&content);
        assert!(
            tjs.contains(&b"Auto".to_vec()),
            "auto-size combo must render value; got {tjs:?}"
        );
    }

    // ── Ch: resolve_combo_display — indirect /Opt element (lines 1701-1703) ─

    /// Combo /Opt has one element stored as indirect reference to a [export,display] array.
    #[test]
    fn ch_combo_opt_element_indirect_array() {
        // obj-5 = [(expA)(dispA)]; /Opt [5 0 R], /V (expA)
        let raw = build_ch_pdf_with_extras(
            "<</Type /Annot /Subtype /Widget /FT /Ch /T (c) /Ff 131072 \
             /V (expA) /Opt [5 0 R] /DA (/Helv 12 Tf 0 g) /Rect [100 700 300 720]>>",
            &["[(expA)(dispA)]"],
        );
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let content = ch_ap_content(&mut pdf, ObjectRef::new(4, 0));
        let tjs = tj_strings_from(&content);
        assert!(
            tjs.contains(&b"dispA".to_vec()),
            "indirect /Opt pair: display must be rendered; got {tjs:?}"
        );
    }

    // ── Ch: resolve_opt_array indirect ref (lines 1744-1748) ─────────────────

    /// /Opt itself is an indirect reference to an array.
    #[test]
    fn ch_combo_opt_indirect_array() {
        // obj-5 = [(A)(B)(C)]; /Opt 5 0 R (indirect array), /V (A)
        let raw = build_ch_pdf_with_extras(
            "<</Type /Annot /Subtype /Widget /FT /Ch /T (c) /Ff 131072 \
             /V (A) /Opt 5 0 R /DA (/Helv 12 Tf 0 g) /Rect [100 700 300 720]>>",
            &["[(A)(A) (B)(B) (C)(C)]"],
        );
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let content = ch_ap_content(&mut pdf, ObjectRef::new(4, 0));
        // /V (A) matches the first pair's export → display "A" rendered.
        let tjs = tj_strings_from(&content);
        assert!(
            tjs.contains(&b"A".to_vec()),
            "indirect /Opt array must be resolved; got {tjs:?}"
        );
    }

    /// /Opt is an indirect reference to a non-array → treated as absent (no /Opt).
    #[test]
    fn ch_combo_opt_indirect_non_array() {
        // obj-5 = integer 42 (not an array); /Opt 5 0 R
        let raw = build_ch_pdf_with_extras(
            "<</Type /Annot /Subtype /Widget /FT /Ch /T (c) /Ff 131072 \
             /V (Hello) /Opt 5 0 R /DA (/Helv 12 Tf 0 g) /Rect [100 700 300 720]>>",
            &["42"],
        );
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        // Must not panic; /V is rendered as-is (no display substitution).
        let content = ch_ap_content(&mut pdf, ObjectRef::new(4, 0));
        let tjs = tj_strings_from(&content);
        assert!(
            tjs.contains(&b"Hello".to_vec()),
            "combo must render /V when /Opt non-array; got {tjs:?}"
        );
    }

    // ── Ch: resolve_string_elem indirect ref (lines 1759-1763) ───────────────

    /// /Opt pair elements stored as indirect strings (lines 1759-1761).
    #[test]
    fn ch_combo_opt_pair_elements_indirect_strings() {
        // obj-5 = (expB); obj-6 = (dispB); /Opt [[5 0 R 6 0 R]], /V (expB)
        let raw = build_ch_pdf_with_extras(
            "<</Type /Annot /Subtype /Widget /FT /Ch /T (c) /Ff 131072 \
             /V (expB) /Opt [[5 0 R 6 0 R]] /DA (/Helv 12 Tf 0 g) /Rect [100 700 300 720]>>",
            &["(expB)", "(dispB)"],
        );
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let content = ch_ap_content(&mut pdf, ObjectRef::new(4, 0));
        let tjs = tj_strings_from(&content);
        assert!(
            tjs.contains(&b"dispB".to_vec()),
            "indirect pair string elements must be resolved; got {tjs:?}"
        );
    }

    /// /Opt pair element that is indirect ref to non-string → pair skipped (line 1763).
    #[test]
    fn ch_combo_opt_pair_element_indirect_non_string() {
        // obj-5 = integer (non-string export); combo /V (X) → no match → renders "X" as-is.
        let raw = build_ch_pdf_with_extras(
            "<</Type /Annot /Subtype /Widget /FT /Ch /T (c) /Ff 131072 \
             /V (X) /Opt [[5 0 R (dispZ)]] /DA (/Helv 12 Tf 0 g) /Rect [100 700 300 720]>>",
            &["99"],
        );
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        // Must not panic; /V is rendered unchanged.
        let content = ch_ap_content(&mut pdf, ObjectRef::new(4, 0));
        let tjs = tj_strings_from(&content);
        assert!(
            tjs.contains(&b"X".to_vec()),
            "non-string export ref: /V must be rendered as-is; got {tjs:?}"
        );
    }

    // ── Ch: combo /Opt flat-string match (lines 1723-1729) ───────────────────

    /// Combo /Opt as flat strings [(A)(B)(C)], /V (B) → /V rendered (export == display match).
    #[test]
    fn ch_combo_opt_flat_string_match() {
        // /V (B) matches the second element of flat /Opt.
        let raw = build_ch_pdf_obj4(
            "<</Type /Annot /Subtype /Widget /FT /Ch /T (c) /Ff 131072 \
             /V (B) /Opt [(A) (B) (C)] /DA (/Helv 12 Tf 0 g) /Rect [100 700 300 720]>>",
        );
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let content = ch_ap_content(&mut pdf, ObjectRef::new(4, 0));
        let tjs = tj_strings_from(&content);
        assert!(
            tjs.contains(&b"B".to_vec()),
            "flat /Opt match: /V must be rendered; got {tjs:?}"
        );
    }

    /// Combo /Opt flat strings, /V does NOT match → /V rendered unchanged (falls through).
    #[test]
    fn ch_combo_opt_flat_string_no_match() {
        // /V (Z) does not appear in flat /Opt → rendered as-is.
        let raw = build_ch_pdf_obj4(
            "<</Type /Annot /Subtype /Widget /FT /Ch /T (c) /Ff 131072 \
             /V (Z) /Opt [(A) (B) (C)] /DA (/Helv 12 Tf 0 g) /Rect [100 700 300 720]>>",
        );
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let content = ch_ap_content(&mut pdf, ObjectRef::new(4, 0));
        let tjs = tj_strings_from(&content);
        assert!(
            tjs.contains(&b"Z".to_vec()),
            "flat /Opt no-match: /V must be rendered as-is; got {tjs:?}"
        );
    }

    /// Combo /Opt element that is neither string nor array (e.g. integer) → skipped (line 1731).
    #[test]
    fn ch_combo_opt_element_neither_string_nor_array() {
        // /Opt contains an integer (invalid) then a valid pair.
        let raw = build_ch_pdf_obj4(
            "<</Type /Annot /Subtype /Widget /FT /Ch /T (c) /Ff 131072 \
             /V (expX) /Opt [99 [(expX)(dispX)]] /DA (/Helv 12 Tf 0 g) /Rect [100 700 300 720]>>",
        );
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let content = ch_ap_content(&mut pdf, ObjectRef::new(4, 0));
        let tjs = tj_strings_from(&content);
        // After skipping the integer, the pair match should still find dispX.
        assert!(
            tjs.contains(&b"dispX".to_vec()),
            "invalid /Opt element skipped; valid pair must still match; got {tjs:?}"
        );
    }

    // ── Ch: list /Opt element as indirect reference (line 1792) ──────────────

    /// List /Opt has elements stored as indirect references to strings.
    #[test]
    fn ch_list_opt_element_indirect_string() {
        // obj-5 = (Red); obj-6 = (Green); obj-7 = (Blue); /Opt [5 0 R 6 0 R 7 0 R]
        let raw = build_ch_pdf_with_extras(
            "<</Type /Annot /Subtype /Widget /FT /Ch /T (list) /Ff 0 \
             /Opt [5 0 R 6 0 R 7 0 R] /I [1] \
             /DA (/Helv 10 Tf 0 g) /Rect [100 600 300 700]>>",
            &["(Red)", "(Green)", "(Blue)"],
        );
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let content = ch_ap_content(&mut pdf, ObjectRef::new(4, 0));
        let tjs = tj_strings_from(&content);
        assert!(
            tjs.contains(&b"Red".to_vec())
                && tjs.contains(&b"Green".to_vec())
                && tjs.contains(&b"Blue".to_vec()),
            "indirect /Opt elements must be rendered; got {tjs:?}"
        );
    }

    // ── Ch: list /Opt element neither array nor string (lines 1816-1821) ─────

    /// List /Opt contains an integer element → empty display (neither array nor string branch).
    #[test]
    fn ch_list_opt_element_invalid_type() {
        // /Opt has an integer which is neither string nor array.
        let raw = build_ch_pdf_obj4(
            "<</Type /Annot /Subtype /Widget /FT /Ch /T (list) /Ff 0 \
             /Opt [42 (Valid)] /I [0] \
             /DA (/Helv 10 Tf 0 g) /Rect [100 600 300 700]>>",
        );
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        // Must not panic; one valid and one invalid element.
        let content = ch_ap_content(&mut pdf, ObjectRef::new(4, 0));
        let tjs = tj_strings_from(&content);
        // "Valid" must appear (idx=1, but /I [0] selects the integer which renders empty)
        assert!(
            tjs.contains(&b"Valid".to_vec()),
            "valid /Opt entry must still render; got {tjs:?}"
        );
    }

    // ── Ch: list /I element as indirect reference (line 1837) ────────────────

    /// List /I with elements stored as indirect references to integers.
    #[test]
    fn ch_list_i_element_indirect_integer() {
        // obj-5 = 1 (integer); /I [5 0 R] → selects index 1 (Green).
        let raw = build_ch_pdf_with_extras(
            "<</Type /Annot /Subtype /Widget /FT /Ch /T (list) /Ff 0 \
             /Opt [(Red) (Green) (Blue)] /I [5 0 R] \
             /DA (/Helv 10 Tf 0 g) /Rect [100 600 300 700]>>",
            &["1"],
        );
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let content = ch_ap_content(&mut pdf, ObjectRef::new(4, 0));
        // Exactly one highlight (Green at index 1).
        assert_eq!(
            count_fills(&content),
            1,
            "indirect /I integer must select one row"
        );
    }

    // ── Ch: list /I with out-of-bounds / negative index (lines 1847-1848) ────

    /// List /I contains negative and out-of-bounds indices → none inserted (rule #3).
    #[test]
    fn ch_list_i_negative_and_out_of_bounds_indices() {
        // /I [-1 99 1] → only index 1 is valid (Green).
        let raw = build_list_pdf("(Red) (Green) (Blue)", "[-1 99 1]", "");
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let content = ch_ap_content(&mut pdf, ObjectRef::new(4, 0));
        // Only the valid index (1) must produce a highlight.
        assert_eq!(
            count_fills(&content),
            1,
            "negative and out-of-bounds /I indices must be ignored"
        );
    }

    // ── Ch: list /V array with indirect-ref element (line 1869) ──────────────

    /// List /V is a direct array; one element is an indirect reference to a string.
    #[test]
    fn ch_list_v_array_with_indirect_string_elem() {
        // obj-5 = (Blue); /V [(Red) 5 0 R] → Red and Blue selected.
        let raw = build_ch_pdf_with_extras(
            "<</Type /Annot /Subtype /Widget /FT /Ch /T (list) /Ff 0 \
             /Opt [(Red) (Green) (Blue)] /V [(Red) 5 0 R] \
             /DA (/Helv 10 Tf 0 g) /Rect [100 600 300 700]>>",
            &["(Blue)"],
        );
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let content = ch_ap_content(&mut pdf, ObjectRef::new(4, 0));
        assert_eq!(
            count_fills(&content),
            2,
            "indirect string elem in /V array must select two rows"
        );
    }

    /// List /V resolves to something that is neither string nor array (line 1877).
    #[test]
    fn ch_list_v_non_string_non_array() {
        // /V /SomeName — not a string, not an array → no selection.
        let raw = build_ch_pdf_obj4(
            "<</Type /Annot /Subtype /Widget /FT /Ch /T (list) /Ff 0 \
             /Opt [(Red) (Green) (Blue)] /V /SomeName \
             /DA (/Helv 10 Tf 0 g) /Rect [100 600 300 700]>>",
        );
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let content = ch_ap_content(&mut pdf, ObjectRef::new(4, 0));
        assert_eq!(
            count_fills(&content),
            0,
            "/V as Name must produce no selection highlight"
        );
    }

    // ── Ch: list /TI as indirect reference (lines 1898-1905) ─────────────────

    /// List /TI stored as indirect reference to an integer (lines 1898-1902).
    #[test]
    fn ch_list_ti_indirect_integer() {
        // obj-5 = 1; /TI 5 0 R → skip first option (Red), show Green and Blue.
        let raw = build_ch_pdf_with_extras(
            "<</Type /Annot /Subtype /Widget /FT /Ch /T (list) /Ff 0 \
             /Opt [(Red) (Green) (Blue)] /TI 5 0 R /I [1] \
             /DA (/Helv 10 Tf 0 g) /Rect [100 600 300 700]>>",
            &["1"],
        );
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let content = ch_ap_content(&mut pdf, ObjectRef::new(4, 0));
        // /TI 1 → only Green and Blue rendered; /I [1] (Green) is still selected.
        let tjs = tj_strings_from(&content);
        assert!(
            tjs.contains(&b"Green".to_vec()),
            "indirect /TI must offset display; got {tjs:?}"
        );
        assert!(
            !tjs.contains(&b"Red".to_vec()),
            "Red must be scrolled off by /TI=1; got {tjs:?}"
        );
    }

    /// List /TI as indirect ref pointing to out-of-bounds value → defaults to 0 (line 1902).
    #[test]
    fn ch_list_ti_indirect_out_of_bounds() {
        // obj-5 = 999 (way out of bounds → ti = 0)
        let raw = build_ch_pdf_with_extras(
            "<</Type /Annot /Subtype /Widget /FT /Ch /T (list) /Ff 0 \
             /Opt [(Red) (Green) (Blue)] /TI 5 0 R \
             /DA (/Helv 10 Tf 0 g) /Rect [100 600 300 700]>>",
            &["999"],
        );
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let content = ch_ap_content(&mut pdf, ObjectRef::new(4, 0));
        // Out-of-bounds /TI → all options shown.
        let tjs = tj_strings_from(&content);
        assert!(
            tjs.contains(&b"Red".to_vec()),
            "out-of-bounds indirect /TI must default to 0 (show all); got {tjs:?}"
        );
    }

    /// List /TI as direct integer in-bounds (line 1905).
    #[test]
    fn ch_list_ti_direct_integer() {
        // /TI 2 → only Blue shown (first two skipped).
        let raw = build_ch_pdf_obj4(
            "<</Type /Annot /Subtype /Widget /FT /Ch /T (list) /Ff 0 \
             /Opt [(Red) (Green) (Blue)] /TI 2 \
             /DA (/Helv 10 Tf 0 g) /Rect [100 600 300 700]>>",
        );
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let content = ch_ap_content(&mut pdf, ObjectRef::new(4, 0));
        let tjs = tj_strings_from(&content);
        assert!(
            tjs.contains(&b"Blue".to_vec()),
            "/TI 2 must show only Blue; got {tjs:?}"
        );
        assert!(
            !tjs.contains(&b"Red".to_vec()),
            "Red must be scrolled off by /TI=2; got {tjs:?}"
        );
    }

    // ── Ch: list auto-size (line 1913) ───────────────────────────────────────

    /// List /DA with 0 font size → auto-size path (font size computed from bbox).
    #[test]
    fn ch_list_auto_size() {
        let raw = build_ch_pdf_obj4(
            "<</Type /Annot /Subtype /Widget /FT /Ch /T (list) /Ff 0 \
             /Opt [(Red) (Green)] /I [0] \
             /DA (/Helv 0 Tf 0 g) /Rect [100 600 300 700]>>",
        );
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let content = ch_ap_content(&mut pdf, ObjectRef::new(4, 0));
        let tjs = tj_strings_from(&content);
        assert!(
            tjs.contains(&b"Red".to_vec()),
            "auto-size list must render options; got {tjs:?}"
        );
    }

    // ── Ch: list rows clip below bbox (lines 1938, 1974) ─────────────────────

    /// Many options in a small bbox → lower rows clip (row_top < -leading).
    #[test]
    fn ch_list_rows_clip_below_bbox() {
        // Small bbox (height 30) with many options → only a few rows visible.
        let raw = build_ch_pdf_obj4(
            "<</Type /Annot /Subtype /Widget /FT /Ch /T (list) /Ff 0 \
             /Opt [(A) (B) (C) (D) (E) (F) (G) (H) (I) (J)] /I [0] \
             /DA (/Helv 12 Tf 0 g) /Rect [100 700 300 730]>>",
        );
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let content = ch_ap_content(&mut pdf, ObjectRef::new(4, 0));
        // Must not panic; must render at least the first option.
        let tjs = tj_strings_from(&content);
        assert!(
            !tjs.is_empty(),
            "at least first option must be rendered before clipping"
        );
        // Not all 10 options can fit in 30pt height at 12pt font.
        assert!(
            tjs.len() < 10,
            "some rows must be clipped; got {}",
            tjs.len()
        );
    }

    /// /TI offsets start + small bbox: both highlight and text loops hit the clip break.
    #[test]
    fn ch_list_ti_with_clip() {
        // /TI 5 → start at index 5; small bbox height → only a couple visible.
        let raw = build_ch_pdf_obj4(
            "<</Type /Annot /Subtype /Widget /FT /Ch /T (list) /Ff 0 \
             /Opt [(A) (B) (C) (D) (E) (F) (G) (H)] /TI 5 /I [5 6] \
             /DA (/Helv 12 Tf 0 g) /Rect [100 700 300 730]>>",
        );
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let content = ch_ap_content(&mut pdf, ObjectRef::new(4, 0));
        // Must not panic; rows starting at F, G, H with small bbox; F and G selected.
        let tjs = tj_strings_from(&content);
        assert!(
            tjs.contains(&b"F".to_vec()),
            "/TI=5 must show option F; got {tjs:?}"
        );
    }

    // ── Ch: list /Opt empty → still produces appearance (no options) ──────────

    /// List with empty /Opt array → should produce an appearance (empty content).
    #[test]
    fn ch_list_empty_opt() {
        let raw = build_ch_pdf_obj4(
            "<</Type /Annot /Subtype /Widget /FT /Ch /T (list) /Ff 0 \
             /Opt [] /DA (/Helv 10 Tf 0 g) /Rect [100 600 300 700]>>",
        );
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let result = generate_choice_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("must not error");
        assert!(
            result.is_some(),
            "list with empty /Opt must still produce appearance"
        );
    }

    // ── Ch: resolve_string_elem _ => None (line 1763) ────────────────────────

    /// /Opt pair whose export element is a direct non-string non-reference value
    /// (e.g. an integer literal) → resolve_string_elem returns None, pair skipped.
    #[test]
    fn ch_combo_opt_pair_export_direct_integer() {
        // /Opt [[99 (disp99)]] — export is integer literal, not string; /V (99) won't match.
        let raw = build_ch_pdf_obj4(
            "<</Type /Annot /Subtype /Widget /FT /Ch /T (c) /Ff 131072 \
             /V (fallback) /Opt [[99 (disp99)]] \
             /DA (/Helv 12 Tf 0 g) /Rect [100 700 300 720]>>",
        );
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let content = ch_ap_content(&mut pdf, ObjectRef::new(4, 0));
        // The pair is skipped (non-string export); /V rendered as-is.
        let tjs = tj_strings_from(&content);
        assert!(
            tjs.contains(&b"fallback".to_vec()),
            "non-string export in pair → /V rendered unchanged; got {tjs:?}"
        );
    }

    // ── Ch: combo /Opt pair with matching export in display lookup (line 1720) ─

    /// Combo /Opt pair with export matching /V → display returned (covers the return branch).
    #[test]
    fn ch_combo_opt_pair_export_matches_v_returns_display() {
        // /Opt [[(matchKey)(ShowThis)]] ; /V (matchKey) → should render "ShowThis".
        let raw = build_ch_pdf_obj4(
            "<</Type /Annot /Subtype /Widget /FT /Ch /T (c) /Ff 131072 \
             /V (matchKey) /Opt [[(matchKey)(ShowThis)] [(other)(Other)]] \
             /DA (/Helv 12 Tf 0 g) /Rect [100 700 300 720]>>",
        );
        let mut pdf = Pdf::open(Cursor::new(raw)).expect("parse");
        let content = ch_ap_content(&mut pdf, ObjectRef::new(4, 0));
        let tjs = tj_strings_from(&content);
        assert!(
            tjs.contains(&b"ShowThis".to_vec()),
            "matching export key must return display string; got {tjs:?}"
        );
    }

    // ── Holder-chain (ref→ref→value) robustness for structural dicts ──────────
    //
    // `Pdf::resolve` is single-hop; a structural dict stored behind two indirect
    // hops (`a 0 R → b 0 R → dict`) is dropped by code that resolves once then
    // type-checks. Each test stores one structural dict as a 2-hop holder chain
    // and asserts the EFFECT that only chain-following enables.

    /// Tx PDF whose widget `/AP` is a two-hop holder chain `4→6→7`, with the
    /// terminal `/AP` dict carrying a sentinel entry `/D 99 0 R`. A correct
    /// chain-follow preserves that sentinel; a single-hop resolve sees the
    /// second-hop `Reference`, falls back to an empty dict, and drops it.
    fn build_ap_holder_chain_pdf() -> Vec<u8> {
        let mut pdf: Vec<u8> = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"1 0 obj\n<</Type /Catalog /Pages 2 0 R /AcroForm \
              <</Fields [4 0 R] /DR <<>> /DA (/Helv 12 Tf 0 g)>>>>\nendobj\n",
        );
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Annots [4 0 R]>>\nendobj\n",
        );
        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Tx /T (field1) \
              /V (Hello World) /Rect [100 700 300 720] /P 3 0 R /AP 6 0 R>>\nendobj\n",
        );
        let off5 = pdf.len() as u64;
        // First hop of the /AP chain.
        pdf.extend_from_slice(b"6 0 obj\n7 0 R\nendobj\n");
        let off6 = pdf.len() as u64;
        // Terminal /AP dict with a sentinel entry that must survive.
        pdf.extend_from_slice(b"7 0 obj\n<</D 99 0 R>>\nendobj\n");
        let xref_start = pdf.len() as u64;
        // Object 5 is unused (the chain uses 6/7), so its slot is free.
        let xref = format!(
            "xref\n0 8\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             0000000000 00000 f \n\
             {off5:010} 00000 n \n\
             {off6:010} 00000 n \n",
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer = format!("trailer\n<</Size 8 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    /// Site 1: the pre-existing `/AP` sub-dict reached through a 2-hop holder
    /// chain must be preserved (sentinel `/D` survives) while `/N` is added.
    #[test]
    fn ap_holder_chain_preserves_existing_entries() {
        let mut pdf = Pdf::open(Cursor::new(build_ap_holder_chain_pdf())).expect("parse");
        generate_text_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("generate")
            .expect("Tx field handled");

        let widget = pdf.resolve(ObjectRef::new(4, 0)).expect("widget");
        let widget = widget.as_dict().expect("widget is a dict");
        let ap = widget
            .get("AP")
            .and_then(Object::as_dict)
            .expect("/AP is a direct dict after generation");
        // The new normal appearance was installed.
        assert!(ap.get("N").is_some(), "/AP/N must be installed");
        // The pre-existing /D sentinel (reached only via the 2-hop chain) survives.
        assert_eq!(
            ap.get("D"),
            Some(&Object::Reference(ObjectRef::new(99, 0))),
            "pre-existing /AP/D must survive: holder chain to the /AP dict was dropped"
        );
    }

    /// Tx PDF whose catalog `/AcroForm` is a two-hop holder chain `1→6→7`. The
    /// terminal AcroForm dict carries a red `/DA` (`1 0 0 rg`). Only a correct
    /// chain-follow reaches that DA; a single-hop resolve sees the second-hop
    /// `Reference`, finds no AcroForm DA, and emits the default black `0 g`.
    fn build_acroform_holder_chain_pdf() -> Vec<u8> {
        let mut pdf: Vec<u8> = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"1 0 obj\n<</Type /Catalog /Pages 2 0 R /AcroForm 6 0 R>>\nendobj\n",
        );
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Annots [4 0 R]>>\nendobj\n",
        );
        let off4 = pdf.len() as u64;
        // Widget carries no own /DA so the AcroForm DA is the only colour source.
        pdf.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Tx /T (field1) \
              /V (Hi) /Rect [100 700 300 720] /P 3 0 R>>\nendobj\n",
        );
        let off5 = pdf.len() as u64;
        // First hop of the /AcroForm chain.
        pdf.extend_from_slice(b"6 0 obj\n7 0 R\nendobj\n");
        let off6 = pdf.len() as u64;
        // Terminal /AcroForm dict with a red DA.
        pdf.extend_from_slice(
            b"7 0 obj\n<</Fields [4 0 R] /DR <<>> /DA (/Helv 12 Tf 1 0 0 rg)>>\nendobj\n",
        );
        let xref_start = pdf.len() as u64;
        // Object 5 is unused (the chain uses 6/7), so its slot is free.
        let xref = format!(
            "xref\n0 8\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             0000000000 00000 f \n\
             {off5:010} 00000 n \n\
             {off6:010} 00000 n \n",
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer = format!("trailer\n<</Size 8 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    /// Site 2: the catalog `/AcroForm` dict reached through a 2-hop holder chain
    /// must yield its `/DA`, so the red colour operator appears in the stream.
    #[test]
    fn acroform_holder_chain_resolves_default_da() {
        let mut pdf = Pdf::open(Cursor::new(build_acroform_holder_chain_pdf())).expect("parse");
        let xobj_ref = generate_text_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("generate")
            .expect("Tx field handled");
        let xobj = pdf.resolve(xobj_ref).expect("resolve xobj");
        let stream = xobj.as_stream().expect("xobj is a stream");
        let content = String::from_utf8_lossy(&stream.data).into_owned();
        assert!(
            content.contains("1 0 0 rg"),
            "AcroForm /DA red colour must reach the stream via the holder chain; got:\n{content}"
        );
    }

    /// Tx PDF whose AcroForm `/DR` is a two-hop holder chain `1→6→7`. The DA is
    /// inline (`/F1 12 Tf`) so the AcroForm dict itself is direct — only the
    /// `/DR` lookup exercises the chain. The terminal `/DR` maps `/F1` to a
    /// Times-Roman font; a single-hop `into_dict()` on the second-hop
    /// `Reference` yields `None`, so the font falls back to Helvetica.
    fn build_dr_holder_chain_pdf() -> Vec<u8> {
        let mut pdf: Vec<u8> = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"1 0 obj\n<</Type /Catalog /Pages 2 0 R /AcroForm \
              <</Fields [4 0 R] /DR 6 0 R /DA (/F1 12 Tf 0 g)>>>>\nendobj\n",
        );
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [3 0 R] /Count 1>>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Annots [4 0 R]>>\nendobj\n",
        );
        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<</Type /Annot /Subtype /Widget /FT /Tx /T (field1) \
              /V (Hi) /Rect [100 700 300 720] /P 3 0 R>>\nendobj\n",
        );
        let off5 = pdf.len() as u64;
        // First hop of the /DR chain.
        pdf.extend_from_slice(b"6 0 obj\n7 0 R\nendobj\n");
        let off6 = pdf.len() as u64;
        // Terminal /DR dict mapping /F1 to a Times-Roman font (obj 8).
        pdf.extend_from_slice(b"7 0 obj\n<</Font <</F1 8 0 R>>>>\nendobj\n");
        let off7 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"8 0 obj\n<</Type /Font /Subtype /Type1 /BaseFont /Times-Roman>>\nendobj\n",
        );
        let xref_start = pdf.len() as u64;
        // Object 5 is unused (the chain uses 6/7/8), so its slot is free.
        let xref = format!(
            "xref\n0 9\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             0000000000 00000 f \n\
             {off5:010} 00000 n \n\
             {off6:010} 00000 n \n\
             {off7:010} 00000 n \n",
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer = format!("trailer\n<</Size 9 /Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    /// Site 3 (`resolve_to_dict`): the `/DR` dict reached through a 2-hop holder
    /// chain must be followed so `/F1` resolves to its Times-Roman `/BaseFont`.
    #[test]
    fn dr_holder_chain_resolves_font_basefont() {
        let mut pdf = Pdf::open(Cursor::new(build_dr_holder_chain_pdf())).expect("parse");
        let xobj_ref = generate_text_field_appearance(&mut pdf, ObjectRef::new(4, 0))
            .expect("generate")
            .expect("Tx field handled");
        let xobj = pdf.resolve(xobj_ref).expect("resolve xobj");
        let stream = xobj.as_stream().expect("xobj is a stream");
        let res = stream
            .dict
            .get("Resources")
            .and_then(Object::as_dict)
            .expect("/Resources is a dict");
        let fonts = res
            .get("Font")
            .and_then(Object::as_dict)
            .expect("/Resources/Font is a dict");
        let f1 = fonts.get("F1").expect("F1 entry");
        let fdict = resolve_ref_chain(&mut pdf, f1)
            .expect("resolve F1")
            .0
            .into_dict()
            .expect("F1 resolves to a font dict");
        assert_eq!(
            fdict.get("BaseFont"),
            Some(&Object::Name(b"Times-Roman".to_vec())),
            "BaseFont must resolve from the /DR holder chain to Times-Roman"
        );
    }
}
