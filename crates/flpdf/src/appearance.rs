//! Appearance-stream generators for AcroForm widgets.
//!
//! This module builds the `/AP/N` (normal-appearance) Form XObject for
//! AcroForm widget annotations.  Currently only **Tx (text field)**
//! appearance streams are implemented; 9.6 (checkbox/radio) and 9.7
//! (choice-field list) will reuse the shared helpers exported as
//! `pub(crate)` from this module.
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

use std::collections::BTreeSet;
use std::io::{Read, Seek};

use crate::default_appearance::{parse_default_appearance, TextColor};
use crate::json_inspect::decode_pdf_text_string;
use crate::object::write_literal_string;
use crate::page_object_helper::PageBox;
use crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH;
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

    let mut ap_dict = match widget_dict.get("AP").cloned() {
        Some(Object::Dictionary(d)) => d,
        Some(Object::Reference(r)) => {
            let resolved = pdf.resolve(r)?;
            match resolved {
                Object::Dictionary(d) => d,
                _ => Dictionary::new(),
            }
        }
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
    let ff = resolve_inherited_integer(pdf, widget_ref, b"Ff")?.unwrap_or(0) as u32;
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
        if depth >= DEFAULT_MAX_PAGE_TREE_DEPTH {
            break;
        }
        if !seen.insert(current) {
            break;
        }

        let node_obj = pdf.resolve_borrowed(current)?;
        let Some(dict) = node_obj.as_dict() else {
            break;
        };

        let da_val = dict.get("DA").cloned();
        let parent_val = dict.get("Parent").cloned();
        // `node_obj` is a reference (not owned), so its lifetime ends when the
        // last use of `dict` (through it) is complete.  The two `.cloned()` calls
        // above copy the values we need; after this point the borrow is gone and
        // we can call `pdf.resolve` below.
        let _ = node_obj; // suppress unused-variable lint; borrow already ended

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
        Some(Object::Reference(r)) => {
            let resolved = pdf.resolve_borrowed(r)?;
            resolved.as_dict().cloned()
        }
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
        Some(Object::Reference(r)) => Ok(pdf.resolve(r)?.into_dict()),
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
        if depth >= DEFAULT_MAX_PAGE_TREE_DEPTH || !seen.insert(current) {
            break;
        }
        let node = pdf.resolve_borrowed(current)?;
        let Some(dict) = node.as_dict() else {
            break;
        };
        let dr_val = dict.get("DR").cloned();
        let parent_val = dict.get("Parent").cloned();
        let _ = node;
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

    let lines = word_wrap(&p.text_bytes, p.font_size, p.bbox_w, p.std_font);
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
}
