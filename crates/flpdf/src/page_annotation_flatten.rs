//! Annotation flattening: burn annotation appearances into page content.
//!
//! [`flatten_annotations_on_page`] processes every eligible annotation on a
//! single leaf page:
//!
//! 1. Reads the annotation's `/AP/N` appearance stream (a Form XObject).
//! 2. Registers that stream in the page `/Resources/XObject` dictionary.
//! 3. Appends `q {A} cm /{name} Do Q` to the page content stream, where `A` is
//!    the affine matrix that maps the appearance's bounding box onto the
//!    annotation `/Rect`.
//! 4. Removes the annotation from the page `/Annots` array.
//!
//! [`flatten_annotations`] applies this to every leaf page in the document.
//!
//! # Modes
//!
//! The [`FlattenMode`] enum controls which annotations are included:
//!
//! | Mode | Condition |
//! |------|-----------|
//! | `All` | Appearance present, not Hidden (`/F` bit 2 unset) |
//! | `Print` | Appearance present, Print bit (`/F` bit 3) set, not Hidden |
//! | `Screen` | Appearance present, Print bit **not** set, not Hidden and not NoView |
//!
//! Annotations without an `/AP/N` entry are silently skipped regardless of mode.
//!
//! # Observable-equivalence caveat
//!
//! Flattening aims for **visual equivalence**: the flattened appearance is
//! placed at the same position and with the same visual content as the original
//! annotation. Byte-level identity with the source PDF is **not** a goal: the
//! page content is rebuilt from its decoded bytes with new stream objects, so
//! exact byte-parity with qpdf or any other tool is not preserved. Content-
//! stream layout (whitespace, number precision) may differ from the source.

use crate::page_annotation_enum::enumerate_page_annotations;
use crate::pages::{coalesce_page_contents, page_content_bytes, resolve_inherited_resources};
use crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result, Stream};
use std::io::{Read, Seek};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Controls which annotations are included in flattening.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlattenMode {
    /// Flatten all annotations that have an appearance, except Hidden ones.
    All,
    /// Flatten only annotations that have the Print bit set (and are not Hidden).
    Print,
    /// Flatten only annotations that do *not* have the Print bit set
    /// (and are not Hidden or NoView).
    Screen,
}

// ---------------------------------------------------------------------------
// Annotation /F flag bit constants (1-indexed per PDF spec)
// ---------------------------------------------------------------------------
/// Bit 2 (0x02): Hidden — do not display or print.
const FLAG_HIDDEN: i64 = 0x2;
/// Bit 3 (0x04): Print — print when printing.
const FLAG_PRINT: i64 = 0x4;
/// Bit 6 (0x20): NoView — do not display on screen.
const FLAG_NO_VIEW: i64 = 0x20;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Flatten eligible annotations on one leaf page.
///
/// Returns the number of annotations that were flattened (removed from
/// `/Annots` and burned into the page content).
///
/// # Errors
///
/// - [`Error::Unsupported`] if `page_ref` does not resolve to a `/Type /Page`
///   dictionary.
/// - Any error from [`Pdf::resolve`] or content-stream decoding.
pub fn flatten_annotations_on_page<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    mode: FlattenMode,
) -> Result<usize> {
    // ── Step 1: enumerate all annotations on the page ─────────────────────
    let all_annots = enumerate_page_annotations(pdf, page_ref)?;

    // ── Step 2: for each annotation, decide eligibility and collect data ───
    struct AnnotData {
        xobj_ref: ObjectRef, // the Form XObject to place
        matrix_a: [f64; 6],  // placement matrix A
    }

    let mut to_flatten: Vec<AnnotData> = Vec::new();
    // Track annot_refs that should be removed from /Annots (same set).
    let mut to_remove: Vec<ObjectRef> = Vec::new();

    for ea in &all_annots {
        // Read /F flags from the annotation dict (indirect ref resolved).
        let flags = read_annot_flags(pdf, ea.annot_ref)?;
        let hidden = (flags & FLAG_HIDDEN) != 0;
        let print_bit = (flags & FLAG_PRINT) != 0;
        let no_view = (flags & FLAG_NO_VIEW) != 0;

        // Mode eligibility.
        let eligible = match mode {
            FlattenMode::All => !hidden,
            FlattenMode::Print => print_bit && !hidden,
            FlattenMode::Screen => !print_bit && !hidden && !no_view,
        };
        if !eligible {
            continue;
        }

        // Resolve /AP/N → the Form XObject.
        let xobj_ref = match resolve_ap_n(pdf, ea.annot_ref)? {
            Some(r) => r,
            None => continue, // no appearance — skip
        };

        // Read /Rect from the enumerated annotation (already resolved by enum).
        let rect = match &ea.rect {
            Some(r) => *r,
            None => continue, // no rect — cannot place
        };

        // Normalize rect: ensure rx0<rx1, ry0<ry1.
        let (rx0, rx1) = if rect.llx <= rect.urx {
            (rect.llx, rect.urx)
        } else {
            (rect.urx, rect.llx)
        };
        let (ry0, ry1) = if rect.lly <= rect.ury {
            (rect.lly, rect.ury)
        } else {
            (rect.ury, rect.lly)
        };

        // Degenerate rect — skip (avoids 0-division in matrix).
        if (rx1 - rx0).abs() < 1e-6 || (ry1 - ry0).abs() < 1e-6 {
            continue;
        }

        // Read /BBox and /Matrix from the Form XObject stream dict.
        let (bbox, ap_matrix) = read_xobj_bbox_and_matrix(pdf, xobj_ref)?;
        let bbox = match bbox {
            Some(b) => b,
            None => continue, // /BBox required
        };

        // Transform the 4 corners of /BBox by /Matrix to get the transformed bbox.
        let corners = [
            apply_matrix(ap_matrix, bbox[0], bbox[1]),
            apply_matrix(ap_matrix, bbox[2], bbox[1]),
            apply_matrix(ap_matrix, bbox[2], bbox[3]),
            apply_matrix(ap_matrix, bbox[0], bbox[3]),
        ];
        let tx0 = corners.iter().map(|c| c.0).fold(f64::INFINITY, f64::min);
        let tx1 = corners
            .iter()
            .map(|c| c.0)
            .fold(f64::NEG_INFINITY, f64::max);
        let ty0 = corners.iter().map(|c| c.1).fold(f64::INFINITY, f64::min);
        let ty1 = corners
            .iter()
            .map(|c| c.1)
            .fold(f64::NEG_INFINITY, f64::max);

        let tw = tx1 - tx0;
        let th = ty1 - ty0;

        // Degenerate transformed BBox — skip to avoid 0-division.
        if tw.abs() < 1e-10 || th.abs() < 1e-10 {
            continue;
        }

        // Compute placement matrix A (PDF 32000-1 §12.5.5 algorithm).
        let sx = (rx1 - rx0) / tw;
        let sy = (ry1 - ry0) / th;
        let matrix_a = [sx, 0.0, 0.0, sy, rx0 - sx * tx0, ry0 - sy * ty0];

        to_flatten.push(AnnotData { xobj_ref, matrix_a });
        to_remove.push(ea.annot_ref);
    }

    if to_flatten.is_empty() {
        return Ok(0);
    }

    // ── Step 3: Coalesce /Contents before appending ────────────────────────
    // Must happen before we read page_content_bytes to get the unified content.
    coalesce_page_contents(pdf, page_ref)?;

    // ── Step 4: Materialize /Resources on the leaf page ────────────────────
    // Resolve inherited resources, then clone them so we can add /XObject
    // entries without mutating shared parent /Resources dicts.
    let inherited_resources = resolve_inherited_resources(pdf, page_ref)?;
    let mut resources_dict = inherited_resources.unwrap_or_default();

    // Get existing /XObject sub-dict (or create empty).
    let mut xobj_dict: Dictionary = match resources_dict.remove("XObject") {
        Some(Object::Dictionary(d)) => d,
        Some(Object::Reference(r)) => match pdf.resolve(r)? {
            Object::Dictionary(d) => d,
            _ => Dictionary::new(),
        },
        _ => Dictionary::new(),
    };

    // ── Step 5: Build content appendix and register XObjects ──────────────
    let mut append_bytes: Vec<u8> = Vec::new();
    // Counter for unique XObject name generation.
    let mut xobj_counter: u32 = 1;

    for data in &to_flatten {
        // Choose a name that doesn't collide with existing /XObject keys.
        let xobj_name = loop {
            let candidate = format!("FlAnnot{xobj_counter}");
            xobj_counter += 1;
            if xobj_dict.get(candidate.as_str()).is_none() {
                break candidate;
            }
        };

        // Register the Form XObject.
        xobj_dict.insert(xobj_name.as_str(), Object::Reference(data.xobj_ref));

        // Build "q {a b c d e f} cm /{name} Do Q\n".
        let cm_line = format!(
            "q\n{} {} {} {} {} {} cm\n/{} Do\nQ\n",
            fmt_f64(data.matrix_a[0]),
            fmt_f64(data.matrix_a[1]),
            fmt_f64(data.matrix_a[2]),
            fmt_f64(data.matrix_a[3]),
            fmt_f64(data.matrix_a[4]),
            fmt_f64(data.matrix_a[5]),
            xobj_name,
        );
        append_bytes.extend_from_slice(cm_line.as_bytes());
    }

    // ── Step 6: Append to page content ─────────────────────────────────────
    let existing_content = page_content_bytes(pdf, page_ref)?;
    let mut new_content = existing_content;
    if !new_content.is_empty() && new_content.last() != Some(&b'\n') {
        new_content.push(b'\n');
    }
    new_content.extend_from_slice(&append_bytes);

    // Allocate a new stream object and set it as /Contents.
    // IMPORTANT: set_object must be called immediately after next_object_ref
    // so that object_refs() includes the new ref before any subsequent allocation.
    let stream_ref = next_object_ref(pdf)?;
    let mut sdict = Dictionary::new();
    sdict.insert("Length", Object::Integer(new_content.len() as i64));
    pdf.set_object(stream_ref, Object::Stream(Stream::new(sdict, new_content)));

    // ── Step 7: Update the page dict (re-resolve after mutations) ──────────
    let page_obj = pdf.resolve(page_ref)?;
    let Object::Dictionary(mut page_dict) = page_obj else {
        return Err(Error::Unsupported(format!(
            "object {page_ref} is not a dictionary after flatten"
        )));
    };

    // Point /Contents at the new stream.
    page_dict.insert("Contents", Object::Reference(stream_ref));

    // Write updated /Resources with the new /XObject entries.
    resources_dict.insert("XObject", Object::Dictionary(xobj_dict));
    page_dict.insert("Resources", Object::Dictionary(resources_dict));

    // ── Step 8: Remove flattened annotations from /Annots ─────────────────
    let new_annots = build_pruned_annots_array(pdf, &page_dict, &to_remove)?;
    page_dict.insert("Annots", Object::Array(new_annots));

    pdf.set_object(page_ref, Object::Dictionary(page_dict));

    Ok(to_flatten.len())
}

/// Flatten eligible annotations on every leaf page in the document.
///
/// Returns the total number of annotations flattened across all pages.
///
/// # Errors
///
/// Propagates any error from [`flatten_annotations_on_page`] or
/// [`crate::pages::page_refs`].
pub fn flatten_annotations<R: Read + Seek>(pdf: &mut Pdf<R>, mode: FlattenMode) -> Result<usize> {
    let page_refs = crate::pages::page_refs(pdf)?;
    let mut total = 0;
    for page_ref in page_refs {
        total += flatten_annotations_on_page(pdf, page_ref, mode)?;
    }
    Ok(total)
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Apply a 6-element PDF matrix [a b c d e f] to point (x, y).
/// Row-vector convention: (a*x + c*y + e, b*x + d*y + f).
fn apply_matrix(m: [f64; 6], x: f64, y: f64) -> (f64, f64) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

/// Format an f64 as a compact, locale-independent PDF number.
/// Integers are emitted without a decimal point (e.g. `200.0` → `"200"`).
fn fmt_f64(v: f64) -> String {
    if v.is_finite() && v == v.trunc() && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.6}");
        let trimmed = s.trim_end_matches('0').trim_end_matches('.');
        trimmed.to_string()
    }
}

/// Read the annotation's `/F` flags integer, resolving indirect references.
/// Returns 0 if absent (absence = no special flags).
fn read_annot_flags<R: Read + Seek>(pdf: &mut Pdf<R>, annot_ref: ObjectRef) -> Result<i64> {
    let obj = pdf.resolve_borrowed(annot_ref)?;
    let Some(dict) = obj.as_dict() else {
        return Ok(0);
    };
    let flags_val = match dict.get("F").cloned() {
        None | Some(Object::Null) => return Ok(0),
        Some(v) => v,
    };
    // Resolve indirect reference (review-pattern #2).
    let resolved = match flags_val {
        Object::Reference(r) => pdf.resolve(r)?,
        other => other,
    };
    Ok(resolved.as_integer().unwrap_or(0))
}

/// Resolve an annotation's `/AP/N` to a Form XObject object ref.
///
/// Handles three `/AP/N` forms:
/// - `Reference → Stream`: returns the ref as-is (no clone, review-pattern #1).
/// - Inline `Stream`: materializes as a new indirect object, returns its ref.
/// - Sub-dictionary (state dict, e.g. checkbox): selects the stream indicated
///   by `/AS` on the annotation dict; if `/AS` is absent or missing, returns `None`.
///
/// Returns `None` if `/AP` or `/AP/N` is absent, or if the state cannot be
/// resolved.
fn resolve_ap_n<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    annot_ref: ObjectRef,
) -> Result<Option<ObjectRef>> {
    // Read /AP dict from annotation (resolve indirect /AP reference).
    let ap_val = {
        let obj = pdf.resolve_borrowed(annot_ref)?;
        let Some(dict) = obj.as_dict() else {
            return Ok(None);
        };
        dict.get("AP").cloned()
    };
    let ap_val = match ap_val {
        None | Some(Object::Null) => return Ok(None),
        Some(v) => v,
    };
    let ap_dict: Dictionary = match ap_val {
        Object::Dictionary(d) => d,
        Object::Reference(r) => match pdf.resolve(r)? {
            Object::Dictionary(d) => d,
            _ => return Ok(None),
        },
        _ => return Ok(None),
    };

    // Get /N value from /AP dict.
    let n_val = match ap_dict.get("N").cloned() {
        None | Some(Object::Null) => return Ok(None),
        Some(v) => v,
    };

    // Resolve /N.
    let n_resolved_for_type: Object = match &n_val {
        Object::Reference(r) => {
            // Peek at the type without consuming ownership.
            let peeked = pdf.resolve_borrowed(*r)?;
            match peeked {
                Object::Stream(_) => return Ok(Some(*r)), // ← ref to stream: use as-is
                Object::Dictionary(_) => {
                    // State dict case — fall through to select by /AS.
                    pdf.resolve(*r)?
                }
                _ => return Ok(None),
            }
        }
        // Per PDF 32000-1 §7.3.8.1, streams must be indirect objects; a direct
        // stream here would only appear in structurally-malformed PDFs.  The
        // flpdf parser never emits direct Object::Stream values as dictionary
        // entries, so the two branches below are defensive dead-code for real
        // PDFs.  They materialize the stream so callers get a valid ref even on
        // malformed input, rather than silently dropping the annotation.
        Object::Stream(_) => n_val.clone(), // inline stream (malformed PDF)
        Object::Dictionary(_) => n_val.clone(), // inline state dict
        _ => return Ok(None),
    };

    match n_resolved_for_type {
        Object::Stream(s) => {
            // Inline stream in malformed PDF — materialize as new indirect object.
            let new_ref = next_object_ref(pdf)?;
            pdf.set_object(new_ref, Object::Stream(s));
            Ok(Some(new_ref))
        }
        Object::Dictionary(state_dict) => {
            // Sub-dictionary: select stream by annotation's /AS name.
            let as_name: Vec<u8> = {
                let obj = pdf.resolve_borrowed(annot_ref)?;
                let Some(adict) = obj.as_dict() else {
                    return Ok(None);
                };
                match adict.get("AS").cloned() {
                    Some(Object::Name(n)) => n,
                    Some(Object::Reference(r)) => match pdf.resolve(r)? {
                        Object::Name(n) => n,
                        _ => return Ok(None),
                    },
                    _ => return Ok(None),
                }
            };
            let as_key = String::from_utf8_lossy(&as_name).into_owned();
            match state_dict.get(as_key.as_str()).cloned() {
                Some(Object::Reference(r)) => match pdf.resolve_borrowed(r)? {
                    Object::Stream(_) => Ok(Some(r)),
                    _ => Ok(None),
                },
                Some(Object::Stream(s)) => {
                    // Inline stream in state dict of malformed PDF.
                    let new_ref = next_object_ref(pdf)?;
                    pdf.set_object(new_ref, Object::Stream(s));
                    Ok(Some(new_ref))
                }
                _ => Ok(None),
            }
        }
        _ => Ok(None),
    }
}

/// Read `/BBox` and `/Matrix` from a Form XObject stream dictionary.
///
/// Returns `(Some([x0,y0,x1,y1]), [a,b,c,d,e,f])`.
/// `/BBox` is required; returns `(None, identity)` if absent or invalid.
/// `/Matrix` defaults to identity if absent.
fn read_xobj_bbox_and_matrix<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    xobj_ref: ObjectRef,
) -> Result<(Option<[f64; 4]>, [f64; 6])> {
    let identity: [f64; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

    let obj = pdf.resolve_borrowed(xobj_ref)?;
    let stream_dict = match obj {
        Object::Stream(s) => s.dict.clone(),
        _ => return Ok((None, identity)),
    };

    // Read /BBox — must be a 4-element array (review-pattern #2: resolve ref).
    let bbox_val = stream_dict.get("BBox").cloned();
    let bbox = match bbox_val {
        None | Some(Object::Null) => return Ok((None, identity)),
        Some(v) => v,
    };
    let bbox_arr = match bbox {
        Object::Array(a) => a,
        Object::Reference(r) => match pdf.resolve(r)? {
            Object::Array(a) => a,
            _ => return Ok((None, identity)),
        },
        _ => return Ok((None, identity)),
    };
    if bbox_arr.len() != 4 {
        return Ok((None, identity));
    }
    let mut bbox_vals = [0.0f64; 4];
    for (i, elem) in bbox_arr.iter().take(4).enumerate() {
        bbox_vals[i] = match elem {
            Object::Integer(n) => *n as f64,
            Object::Real(r) | Object::RealLiteral { value: r, .. } => *r,
            _ => return Ok((None, identity)),
        };
    }

    // Read /Matrix — 6-element array, defaults to identity (review-pattern #2).
    // `stream_dict` is already an owned clone, so reuse it — no second resolve.
    let matrix_val = stream_dict.get("Matrix").cloned();
    let ap_matrix = match matrix_val {
        None | Some(Object::Null) => identity,
        Some(Object::Array(a)) if a.len() == 6 => {
            let mut m = [0.0f64; 6];
            let mut valid = true;
            for (i, elem) in a.iter().take(6).enumerate() {
                m[i] = match elem {
                    Object::Integer(n) => *n as f64,
                    Object::Real(r) | Object::RealLiteral { value: r, .. } => *r,
                    _ => {
                        valid = false;
                        break;
                    }
                };
            }
            if valid {
                m
            } else {
                identity
            }
        }
        Some(Object::Reference(r)) => match pdf.resolve(r)? {
            Object::Array(a) if a.len() == 6 => {
                let mut m = [0.0f64; 6];
                let mut valid = true;
                for (i, elem) in a.iter().take(6).enumerate() {
                    m[i] = match elem {
                        Object::Integer(n) => *n as f64,
                        Object::Real(r) | Object::RealLiteral { value: r, .. } => *r,
                        _ => {
                            valid = false;
                            break;
                        }
                    };
                }
                if valid {
                    m
                } else {
                    identity
                }
            }
            _ => identity,
        },
        _ => identity,
    };

    Ok((Some(bbox_vals), ap_matrix))
}

/// Build the pruned `/Annots` array, removing all refs in `to_remove`.
///
/// Resolves the existing `/Annots` value (which may be an indirect reference)
/// from `page_dict`. Returns a direct array without the removed entries.
fn build_pruned_annots_array<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_dict: &Dictionary,
    to_remove: &[ObjectRef],
) -> Result<Vec<Object>> {
    let annots_val = match page_dict.get("Annots").cloned() {
        None | Some(Object::Null) => return Ok(Vec::new()),
        Some(v) => v,
    };
    let annots_arr = match annots_val {
        Object::Array(a) => a,
        Object::Reference(r) => match pdf.resolve(r)? {
            Object::Array(a) => a,
            _ => return Ok(Vec::new()),
        },
        _ => return Ok(Vec::new()),
    };

    let pruned: Vec<Object> = annots_arr
        .into_iter()
        .filter(|entry| match entry {
            Object::Reference(r) => !to_remove.contains(r),
            _ => true, // keep non-ref entries (unusual, but don't drop them)
        })
        .collect();

    Ok(pruned)
}

/// Allocate the next unused indirect-object reference.
///
/// Uses the same idiom as `page_rotate::next_object_ref`: one past the current
/// highest object number in the cache.
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::{page_content_bytes, page_refs};
    use crate::writer::write_pdf;
    use crate::{Object, ObjectRef, Pdf};
    use std::io::Cursor;

    // -----------------------------------------------------------------------
    // Minimal PDF builder
    // -----------------------------------------------------------------------

    /// Build a minimal PDF byte vector.
    ///
    /// `page_extra` is appended to the page dict (e.g. `/Annots [4 0 R]`).
    /// `extra_objects` is `(object_number, raw_bytes)`.
    fn build_pdf(page_extra: &str, extra_objects: &[(u32, Vec<u8>)]) -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        let off3 = pdf.len() as u64;
        let page_body = format!(
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] {page_extra} >>\nendobj\n"
        );
        pdf.extend_from_slice(page_body.as_bytes());

        let mut extra_offsets: Vec<(u32, u64)> = Vec::new();
        for &(num, ref body) in extra_objects.iter() {
            let off = pdf.len() as u64;
            extra_offsets.push((num, off));
            pdf.extend_from_slice(body);
        }

        let xref_start = pdf.len() as u64;
        let max_num = extra_offsets.iter().map(|(n, _)| *n).max().unwrap_or(3);
        let total = max_num as usize + 1;
        let mut xref = format!("xref\n0 {total}\n0000000000 65535 f \n");
        xref.push_str(&format!("{:010} 00000 n \n", off1));
        xref.push_str(&format!("{:010} 00000 n \n", off2));
        xref.push_str(&format!("{:010} 00000 n \n", off3));
        for i in 4u32..=max_num {
            if let Some((_, off)) = extra_offsets.iter().find(|(n, _)| *n == i) {
                xref.push_str(&format!("{:010} 00000 n \n", off));
            } else {
                xref.push_str("0000000000 65535 f \n");
            }
        }
        pdf.extend_from_slice(xref.as_bytes());
        let trailer =
            format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    /// Build a minimal Form XObject stream with given /BBox.
    fn make_xobj_stream(bbox: [f64; 4], content: &[u8]) -> Vec<u8> {
        let inner = format!(
            "<< /Type /XObject /Subtype /Form /BBox [{} {} {} {}] /Length {} >>",
            bbox[0],
            bbox[1],
            bbox[2],
            bbox[3],
            content.len()
        );
        let mut out = inner.into_bytes();
        out.extend_from_slice(b"\nstream\n");
        out.extend_from_slice(content);
        out.extend_from_slice(b"\nendstream\n");
        out
    }

    /// Wrap raw stream bytes with object header/footer.
    fn obj_wrap(num: u32, body: Vec<u8>) -> (u32, Vec<u8>) {
        let header = format!("{num} 0 obj\n").into_bytes();
        let footer = b"endobj\n".to_vec();
        let mut out = header;
        out.extend_from_slice(&body);
        out.extend_from_slice(&footer);
        (num, out)
    }

    /// Wrap a dictionary string as an indirect object.
    fn obj_dict(num: u32, dict: &str) -> (u32, Vec<u8>) {
        let body = format!("{dict}\n").into_bytes();
        obj_wrap(num, body)
    }

    // -----------------------------------------------------------------------
    // Test: basic widget flattening with All mode
    // -----------------------------------------------------------------------
    #[test]
    fn flatten_widget_all_mode() {
        // obj 4 = annotation with /AP/N pointing to obj 5 (a Form XObject)
        // obj 5 = Form XObject stream /BBox [0 0 100 20]
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"0.5 g 0 0 100 20 re f");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body);

        // Annotation with /AP/N referencing obj 5, /Rect [50 50 150 70]
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [50 50 150 70] /AP << /N 5 0 R >> >>",
        );

        let bytes = build_pdf("/Annots [4 0 R]", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();
        assert_eq!(count, 1);

        // Page /Resources/XObject should have one entry pointing to the xobj.
        let page_obj = pdf.resolve_borrowed(page_ref).unwrap();
        let page_dict = page_obj.as_dict().unwrap();

        let resources = match page_dict.get("Resources").unwrap() {
            Object::Dictionary(d) => d.clone(),
            _ => panic!("Resources should be a dict"),
        };
        let xobj_dict = match resources.get("XObject").unwrap() {
            Object::Dictionary(d) => d.clone(),
            _ => panic!("XObject should be a dict"),
        };
        assert_eq!(xobj_dict.iter().count(), 1, "exactly one XObject entry");
        // The value should reference obj 5.
        let xobj_val = xobj_dict.iter().next().unwrap().1;
        assert_eq!(xobj_val, &Object::Reference(ObjectRef::new(5, 0)));

        // Page content should contain "cm" and "Do".
        let content = page_content_bytes(&mut pdf, page_ref).unwrap();
        let content_str = String::from_utf8_lossy(&content);
        assert!(content_str.contains("cm"), "content should contain cm");
        assert!(content_str.contains("Do"), "content should contain Do");
        assert!(content_str.contains('q'), "content should contain q");
        assert!(content_str.contains('Q'), "content should contain Q");

        // /Annots should be empty after flattening.
        let page_obj2 = pdf.resolve_borrowed(page_ref).unwrap();
        let page_dict2 = page_obj2.as_dict().unwrap();
        let annots = match page_dict2.get("Annots").unwrap() {
            Object::Array(a) => a.clone(),
            _ => panic!("Annots should be an array"),
        };
        assert!(annots.is_empty(), "Annots should be empty after flattening");
    }

    // -----------------------------------------------------------------------
    // Test: placement matrix values
    // -----------------------------------------------------------------------
    #[test]
    fn placement_matrix_values() {
        // BBox [0 0 100 20] identity matrix, Rect [50 50 150 70]
        // sx = (150-50)/(100-0) = 1.0, sy = (70-50)/(20-0) = 1.0
        // A = [1 0 0 1 50 50]
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body);
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [50 50 150 70] /AP << /N 5 0 R >> >>",
        );

        let bytes = build_pdf("/Annots [4 0 R]", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();

        let content = page_content_bytes(&mut pdf, page_ref).unwrap();
        let content_str = String::from_utf8_lossy(&content);

        // Matrix should be "1 0 0 1 50 50 cm"
        assert!(
            content_str.contains("1 0 0 1 50 50 cm"),
            "expected identity+translate matrix, got: {content_str}"
        );
    }

    // -----------------------------------------------------------------------
    // Test: Print mode — only annotations with Print bit
    // -----------------------------------------------------------------------
    #[test]
    fn print_mode_only_prints_print_bit_annotations() {
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body.clone());
        let (n6, obj6_bytes) = obj_wrap(6, xobj_body);

        // obj 4: Print bit set (0x04)
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [0 0 100 20] /F 4 /AP << /N 5 0 R >> >>",
        );
        // obj 7: No Print bit (F=0)
        let (n7, obj7_bytes) = obj_dict(
            7,
            "<< /Type /Annot /Subtype /Widget /Rect [100 0 200 20] /F 0 /AP << /N 6 0 R >> >>",
        );

        let bytes = build_pdf(
            "/Annots [4 0 R 7 0 R]",
            &[
                (n4, obj4_bytes),
                (n5, obj5_bytes),
                (n6, obj6_bytes),
                (n7, obj7_bytes),
            ],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::Print).unwrap();
        assert_eq!(
            count, 1,
            "only the Print-bit annotation should be flattened"
        );

        // The non-Print annotation (obj 7) should still be in /Annots.
        let page_obj = pdf.resolve_borrowed(page_ref).unwrap();
        let page_dict = page_obj.as_dict().unwrap();
        let annots = match page_dict.get("Annots").unwrap() {
            Object::Array(a) => a.clone(),
            _ => panic!("expected array"),
        };
        assert_eq!(annots.len(), 1, "one annotation should remain");
        assert_eq!(annots[0], Object::Reference(ObjectRef::new(7, 0)));
    }

    // -----------------------------------------------------------------------
    // Test: Screen mode — only annotations without Print bit
    // -----------------------------------------------------------------------
    #[test]
    fn screen_mode_only_flattens_non_print_annotations() {
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body.clone());
        let (n6, obj6_bytes) = obj_wrap(6, xobj_body);

        // obj 4: Print bit set
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [0 0 100 20] /F 4 /AP << /N 5 0 R >> >>",
        );
        // obj 7: No Print bit (screen annotation)
        let (n7, obj7_bytes) = obj_dict(
            7,
            "<< /Type /Annot /Subtype /Widget /Rect [100 0 200 20] /F 0 /AP << /N 6 0 R >> >>",
        );

        let bytes = build_pdf(
            "/Annots [4 0 R 7 0 R]",
            &[
                (n4, obj4_bytes),
                (n5, obj5_bytes),
                (n6, obj6_bytes),
                (n7, obj7_bytes),
            ],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::Screen).unwrap();
        assert_eq!(count, 1, "only the no-Print annotation should be flattened");

        // The Print annotation (obj 4) should still be in /Annots.
        let page_obj = pdf.resolve_borrowed(page_ref).unwrap();
        let page_dict = page_obj.as_dict().unwrap();
        let annots = match page_dict.get("Annots").unwrap() {
            Object::Array(a) => a.clone(),
            _ => panic!("expected array"),
        };
        assert_eq!(annots.len(), 1);
        assert_eq!(annots[0], Object::Reference(ObjectRef::new(4, 0)));
    }

    // -----------------------------------------------------------------------
    // Test: Hidden annotation is skipped in All mode
    // -----------------------------------------------------------------------
    #[test]
    fn hidden_annotation_skipped_in_all_mode() {
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body);
        // Hidden bit = 0x2
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [0 0 100 20] /F 2 /AP << /N 5 0 R >> >>",
        );

        let bytes = build_pdf("/Annots [4 0 R]", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();
        assert_eq!(count, 0, "hidden annotation should be skipped");
    }

    // -----------------------------------------------------------------------
    // Test: annotation without /AP is skipped (no error)
    // -----------------------------------------------------------------------
    #[test]
    fn annotation_without_ap_is_skipped() {
        let (n4, obj4_bytes) =
            obj_dict(4, "<< /Type /Annot /Subtype /Widget /Rect [0 0 100 20] >>");

        let bytes = build_pdf("/Annots [4 0 R]", &[(n4, obj4_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();
        assert_eq!(count, 0);
    }

    // -----------------------------------------------------------------------
    // Test: checkbox state dict (/AP/N is a sub-dict with /AS selection)
    // -----------------------------------------------------------------------
    #[test]
    fn checkbox_state_dict_with_as_selection() {
        // obj 5 = Form XObject for /On state
        let xobj_on = make_xobj_stream([0.0, 0.0, 20.0, 20.0], b"1 g 0 0 20 20 re f");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_on);

        // obj 4 = checkbox annotation with /AP/N as state dict, /AS /On
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [10 10 30 30] \
             /AS /On /AP << /N << /On 5 0 R /Off 0 0 R >> >> >>",
        );

        let bytes = build_pdf("/Annots [4 0 R]", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();
        assert_eq!(count, 1, "checkbox On state should be flattened");

        let content = page_content_bytes(&mut pdf, page_ref).unwrap();
        let content_str = String::from_utf8_lossy(&content);
        assert!(content_str.contains("Do"), "content should contain Do");
    }

    // -----------------------------------------------------------------------
    // Test: write_pdf round-trip — output is valid parseable PDF
    // -----------------------------------------------------------------------
    #[test]
    fn write_pdf_round_trip_is_valid() {
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"0.5 g 0 0 100 20 re f");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body);
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [50 50 150 70] /AP << /N 5 0 R >> >>",
        );

        let bytes = build_pdf("/Annots [4 0 R]", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();

        // Write and re-open the PDF.
        let mut out = Vec::new();
        write_pdf(&mut pdf, &mut out).unwrap();

        let mut pdf2 = Pdf::open(Cursor::new(out)).unwrap();
        let pages = page_refs(&mut pdf2).unwrap();
        assert_eq!(pages.len(), 1);

        // Content must contain Do.
        let content = page_content_bytes(&mut pdf2, pages[0]).unwrap();
        assert!(content.windows(2).any(|w| w == b"Do"));
    }

    // -----------------------------------------------------------------------
    // Test: non-identity /Matrix in the Form XObject
    // -----------------------------------------------------------------------
    #[test]
    fn non_identity_matrix_in_xobj() {
        // /Matrix [2 0 0 2 0 0] scales BBox [0 0 50 10] → transformed bbox [0 0 100 20]
        // Rect [50 50 150 70]: rx_width=100, ry_height=20
        // tx_width=100, ty_height=20 → sx=1.0, sy=1.0
        // A = [1 0 0 1 50 50]
        let xobj_str =
            "<< /Type /XObject /Subtype /Form /BBox [0 0 50 10] /Matrix [2 0 0 2 0 0] /Length 0 >>\nstream\n\nendstream\n";
        let (n5, obj5_bytes) = obj_wrap(5, xobj_str.as_bytes().to_vec());
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [50 50 150 70] /AP << /N 5 0 R >> >>",
        );

        let bytes = build_pdf("/Annots [4 0 R]", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();

        let content = page_content_bytes(&mut pdf, page_ref).unwrap();
        let content_str = String::from_utf8_lossy(&content);
        // transformed bbox is [0 0 100 20], rx0=50 ry0=50
        // sx=100/100=1, sy=20/20=1, e=50-1*0=50, f=50-1*0=50
        assert!(
            content_str.contains("1 0 0 1 50 50 cm"),
            "expected A=[1 0 0 1 50 50], got: {content_str}"
        );
    }

    // -----------------------------------------------------------------------
    // Test: return count is correct for multiple annotations
    // -----------------------------------------------------------------------
    #[test]
    fn multiple_annotations_flattened_count() {
        let xobj_body1 = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let xobj_body2 = make_xobj_stream([0.0, 0.0, 50.0, 50.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body1);
        let (n6, obj6_bytes) = obj_wrap(6, xobj_body2);
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [0 0 100 20] /AP << /N 5 0 R >> >>",
        );
        let (n7, obj7_bytes) = obj_dict(
            7,
            "<< /Type /Annot /Subtype /Widget /Rect [100 100 150 150] /AP << /N 6 0 R >> >>",
        );

        let bytes = build_pdf(
            "/Annots [4 0 R 7 0 R]",
            &[
                (n4, obj4_bytes),
                (n5, obj5_bytes),
                (n6, obj6_bytes),
                (n7, obj7_bytes),
            ],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();
        assert_eq!(count, 2);

        // Both XObjects in /Resources.
        let page_obj = pdf.resolve_borrowed(page_ref).unwrap();
        let page_dict = page_obj.as_dict().unwrap();
        let resources = match page_dict.get("Resources").unwrap() {
            Object::Dictionary(d) => d.clone(),
            _ => panic!("expected dict"),
        };
        let xobj_dict = match resources.get("XObject").unwrap() {
            Object::Dictionary(d) => d.clone(),
            _ => panic!("expected dict"),
        };
        assert_eq!(xobj_dict.iter().count(), 2, "two XObject entries");

        // /Annots empty.
        let annots = match page_dict.get("Annots").unwrap() {
            Object::Array(a) => a.clone(),
            _ => panic!("expected array"),
        };
        assert!(annots.is_empty());
    }

    // -----------------------------------------------------------------------
    // Test: fmt_f64 helper
    // -----------------------------------------------------------------------
    #[test]
    fn fmt_f64_formats_correctly() {
        assert_eq!(fmt_f64(0.0), "0");
        assert_eq!(fmt_f64(1.0), "1");
        assert_eq!(fmt_f64(-1.0), "-1");
        assert_eq!(fmt_f64(50.0), "50");
        assert_eq!(fmt_f64(0.5), "0.5");
        assert_eq!(fmt_f64(1.25), "1.25");
    }

    // -----------------------------------------------------------------------
    // Test: flatten_annotations (document-level)
    // -----------------------------------------------------------------------
    #[test]
    fn flatten_annotations_document_level() {
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body);
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [50 50 150 70] /AP << /N 5 0 R >> >>",
        );

        let bytes = build_pdf("/Annots [4 0 R]", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let count = flatten_annotations(&mut pdf, FlattenMode::All).unwrap();
        assert_eq!(count, 1);
    }

    // -----------------------------------------------------------------------
    // Test: annotation with no /Rect is skipped (line 125)
    // -----------------------------------------------------------------------
    #[test]
    fn annotation_without_rect_is_skipped() {
        // Build annotation with /AP but no /Rect
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body);
        let (n4, obj4_bytes) =
            obj_dict(4, "<< /Type /Annot /Subtype /Widget /AP << /N 5 0 R >> >>");

        let bytes = build_pdf("/Annots [4 0 R]", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();
        assert_eq!(count, 0, "annotation without /Rect should be skipped");
    }

    // -----------------------------------------------------------------------
    // Test: inverted /Rect (llx>urx, lly>ury) is normalized (lines 132,137)
    // -----------------------------------------------------------------------
    #[test]
    fn inverted_rect_normalized_and_flattened() {
        // /Rect [150 70 50 50] → swapped to [50 50 150 70]
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body);
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [150 70 50 50] /AP << /N 5 0 R >> >>",
        );

        let bytes = build_pdf("/Annots [4 0 R]", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();
        assert_eq!(count, 1, "inverted rect should be normalized and flattened");

        let content = page_content_bytes(&mut pdf, page_ref).unwrap();
        let content_str = String::from_utf8_lossy(&content);
        assert!(content_str.contains("cm"), "content should contain cm");
    }

    // -----------------------------------------------------------------------
    // Test: degenerate /Rect (zero-dimension) is skipped (line 142)
    // -----------------------------------------------------------------------
    #[test]
    fn degenerate_zero_dim_rect_is_skipped() {
        // /Rect [50 50 50 70] → zero width → skipped
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body);
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [50 50 50 70] /AP << /N 5 0 R >> >>",
        );

        let bytes = build_pdf("/Annots [4 0 R]", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();
        assert_eq!(count, 0, "zero-dim rect should be skipped");
    }

    // -----------------------------------------------------------------------
    // Test: XObject without /BBox is skipped (line 149)
    // -----------------------------------------------------------------------
    #[test]
    fn xobj_without_bbox_is_skipped() {
        // Form XObject stream without /BBox entry
        let no_bbox_xobj = "<< /Type /XObject /Subtype /Form /Length 0 >>\nstream\n\nendstream\n";
        let (n5, obj5_bytes) = obj_wrap(5, no_bbox_xobj.as_bytes().to_vec());
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [50 50 150 70] /AP << /N 5 0 R >> >>",
        );

        let bytes = build_pdf("/Annots [4 0 R]", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();
        assert_eq!(count, 0, "XObject without /BBox should be skipped");
    }

    // -----------------------------------------------------------------------
    // Test: degenerate transformed BBox (zero-area matrix) is skipped (line 175)
    // -----------------------------------------------------------------------
    #[test]
    fn degenerate_transformed_bbox_is_skipped() {
        // /Matrix [0 0 0 0 0 0] collapses all corners to (0,0) → tw=0, th=0
        let xobj_str = "<< /Type /XObject /Subtype /Form /BBox [0 0 100 20] /Matrix [0 0 0 0 0 0] /Length 0 >>\nstream\n\nendstream\n";
        let (n5, obj5_bytes) = obj_wrap(5, xobj_str.as_bytes().to_vec());
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [50 50 150 70] /AP << /N 5 0 R >> >>",
        );

        let bytes = build_pdf("/Annots [4 0 R]", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();
        assert_eq!(count, 0, "degenerate transformed BBox should be skipped");
    }

    // -----------------------------------------------------------------------
    // Test: /XObject as indirect ref in /Resources (lines 203-206)
    // -----------------------------------------------------------------------
    #[test]
    fn resources_xobject_as_indirect_ref() {
        // Build a PDF with /Resources/XObject as an indirect reference
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body);
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [50 50 150 70] /AP << /N 5 0 R >> >>",
        );
        // obj 6: an XObject dictionary as indirect object (will be referenced by /Resources)
        let (n6, obj6_bytes) = obj_dict(6, "<< /ExistingEntry 5 0 R >>");

        // Page has /Resources with /XObject pointing to obj 6 (indirect ref)
        let bytes = build_pdf(
            "/Annots [4 0 R] /Resources << /XObject 6 0 R >>",
            &[(n4, obj4_bytes), (n5, obj5_bytes), (n6, obj6_bytes)],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();
        assert_eq!(count, 1);

        let content = page_content_bytes(&mut pdf, page_ref).unwrap();
        assert!(
            content.windows(2).any(|w| w == b"Do"),
            "content should contain Do"
        );
    }

    // -----------------------------------------------------------------------
    // Test: XObject name collision forces loop to find unique name (line 223)
    // -----------------------------------------------------------------------
    #[test]
    fn xobj_name_collision_forces_unique_name() {
        // Pre-populate /Resources/XObject with FlAnnot1 so the loop must increment
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body);
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [50 50 150 70] /AP << /N 5 0 R >> >>",
        );

        let bytes = build_pdf(
            "/Annots [4 0 R] /Resources << /XObject << /FlAnnot1 5 0 R >> >>",
            &[(n4, obj4_bytes), (n5, obj5_bytes)],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();
        assert_eq!(count, 1);

        // The content should contain FlAnnot2 (since FlAnnot1 was taken)
        let content = page_content_bytes(&mut pdf, page_ref).unwrap();
        let content_str = String::from_utf8_lossy(&content);
        assert!(
            content_str.contains("FlAnnot2"),
            "expected FlAnnot2 due to name collision, got: {content_str}"
        );
    }

    // -----------------------------------------------------------------------
    // Test: page content not ending in newline gets one appended (line 247)
    // -----------------------------------------------------------------------
    #[test]
    fn page_content_without_trailing_newline_gets_newline() {
        // Content stream that does NOT end in '\n'
        // We need to put raw content bytes in the page — use a content stream obj
        let content_data = b"BT /F1 12 Tf 100 700 Td (hello) Tj ET"; // no trailing newline
        let content_len = content_data.len();
        let stream_header = format!("<< /Length {content_len} >>\nstream\n");
        let mut stream_bytes = stream_header.into_bytes();
        stream_bytes.extend_from_slice(content_data);
        stream_bytes.extend_from_slice(b"\nendstream\n");
        let (n6, obj6_bytes) = obj_wrap(6, stream_bytes);

        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body);
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [50 50 150 70] /AP << /N 5 0 R >> >>",
        );

        // Page with /Contents pointing to obj 6 and /Annots
        let bytes = build_pdf(
            "/Annots [4 0 R] /Contents 6 0 R",
            &[(n4, obj4_bytes), (n5, obj5_bytes), (n6, obj6_bytes)],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();
        assert_eq!(count, 1);

        let content = page_content_bytes(&mut pdf, page_ref).unwrap();
        assert!(
            content.windows(2).any(|w| w == b"Do"),
            "content should contain Do"
        );
    }

    // -----------------------------------------------------------------------
    // Test: Screen mode with NoView flag skips annotation (lines 110,112)
    // -----------------------------------------------------------------------
    #[test]
    fn screen_mode_skips_noview_annotation() {
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body);
        // F=0x20 = NoView bit set, no Print bit
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [0 0 100 20] /F 32 /AP << /N 5 0 R >> >>",
        );

        let bytes = build_pdf("/Annots [4 0 R]", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::Screen).unwrap();
        assert_eq!(
            count, 0,
            "NoView annotation should be skipped in Screen mode"
        );
    }

    // -----------------------------------------------------------------------
    // Test: Hidden annotation skipped in Print mode (line 109)
    // -----------------------------------------------------------------------
    #[test]
    fn print_mode_skips_hidden_annotation() {
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body);
        // F = Hidden(0x2) | Print(0x4) = 0x6
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [0 0 100 20] /F 6 /AP << /N 5 0 R >> >>",
        );

        let bytes = build_pdf("/Annots [4 0 R]", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::Print).unwrap();
        assert_eq!(
            count, 0,
            "Hidden+Print annotation should be skipped in Print mode"
        );
    }

    // -----------------------------------------------------------------------
    // Unit tests for read_xobj_bbox_and_matrix private fn
    // -----------------------------------------------------------------------

    /// Build a minimal PDF with one stream object and return a Pdf handle + its ref.
    fn build_pdf_with_stream_obj(stream_dict_str: &str, data: &[u8]) -> (Vec<u8>, ObjectRef) {
        let data_len = data.len();
        let header = format!("{stream_dict_str} /Length {data_len}");
        // Build it as obj 4
        let mut body = format!("<< {header} >>\nstream\n").into_bytes();
        body.extend_from_slice(data);
        body.extend_from_slice(b"\nendstream\n");
        let (n4, obj4_bytes) = obj_wrap(4, body);
        let pdf_bytes = build_pdf("", &[(n4, obj4_bytes)]);
        (pdf_bytes, ObjectRef::new(4, 0))
    }

    #[test]
    fn read_bbox_matrix_non_stream_object_returns_none_bbox() {
        // obj 4 is a plain dict (not a stream) → should return (None, identity)
        let (n4, obj4_bytes) = obj_dict(4, "<< /Foo /Bar >>");
        let bytes = build_pdf("", &[(n4, obj4_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let xobj_ref = ObjectRef::new(4, 0);

        let (bbox, matrix) = read_xobj_bbox_and_matrix(&mut pdf, xobj_ref).unwrap();
        assert!(bbox.is_none(), "non-stream should return None bbox");
        assert_eq!(
            matrix,
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            "non-stream should return identity matrix"
        );
    }

    #[test]
    fn read_bbox_matrix_missing_bbox_returns_none() {
        // Stream dict without /BBox → (None, identity)
        let (pdf_bytes, xobj_ref) = build_pdf_with_stream_obj("/Type /XObject /Subtype /Form", b"");
        let mut pdf = Pdf::open(Cursor::new(pdf_bytes)).unwrap();

        let (bbox, matrix) = read_xobj_bbox_and_matrix(&mut pdf, xobj_ref).unwrap();
        assert!(bbox.is_none());
        assert_eq!(matrix, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    }

    #[test]
    fn read_bbox_matrix_bbox_wrong_length_returns_none() {
        // /BBox with 3 elements (not 4) → (None, identity)
        let xobj_str =
            "<< /Type /XObject /Subtype /Form /BBox [0 0 100] /Length 0 >>\nstream\n\nendstream\n";
        let (n4, obj4_bytes) = obj_wrap(4, xobj_str.as_bytes().to_vec());
        let bytes = build_pdf("", &[(n4, obj4_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let (bbox, matrix) = read_xobj_bbox_and_matrix(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(bbox.is_none(), "BBox with wrong length should return None");
        assert_eq!(matrix, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    }

    #[test]
    fn read_bbox_matrix_bbox_with_real_values() {
        // /BBox with Real values (e.g. 0.5) covers the Object::Real arm (line 487)
        let xobj_str =
            "<< /Type /XObject /Subtype /Form /BBox [0.5 0.5 100.5 20.5] /Length 0 >>\nstream\n\nendstream\n";
        let (n4, obj4_bytes) = obj_wrap(4, xobj_str.as_bytes().to_vec());
        let bytes = build_pdf("", &[(n4, obj4_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let (bbox, matrix) = read_xobj_bbox_and_matrix(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(bbox.is_some(), "real-valued BBox should succeed");
        let b = bbox.unwrap();
        assert!((b[0] - 0.5).abs() < 1e-10);
        assert!((b[2] - 100.5).abs() < 1e-10);
        assert_eq!(matrix, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    }

    #[test]
    fn read_bbox_matrix_bbox_non_numeric_element_returns_none() {
        // /BBox with a non-numeric element → (None, identity)
        let xobj_str =
            "<< /Type /XObject /Subtype /Form /BBox [0 0 /Bad 20] /Length 0 >>\nstream\n\nendstream\n";
        let (n4, obj4_bytes) = obj_wrap(4, xobj_str.as_bytes().to_vec());
        let bytes = build_pdf("", &[(n4, obj4_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let (bbox, _matrix) = read_xobj_bbox_and_matrix(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(
            bbox.is_none(),
            "non-numeric BBox element should return None"
        );
    }

    #[test]
    fn read_bbox_matrix_matrix_with_real_values() {
        // /Matrix with Real values covers the Object::Real arm for matrix (line 503)
        let xobj_str =
            "<< /Type /XObject /Subtype /Form /BBox [0 0 100 20] /Matrix [1.5 0.0 0.0 1.5 0.0 0.0] /Length 0 >>\nstream\n\nendstream\n";
        let (n4, obj4_bytes) = obj_wrap(4, xobj_str.as_bytes().to_vec());
        let bytes = build_pdf("", &[(n4, obj4_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let (bbox, matrix) = read_xobj_bbox_and_matrix(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(bbox.is_some());
        assert!((matrix[0] - 1.5).abs() < 1e-10, "matrix[0] should be 1.5");
        assert!((matrix[3] - 1.5).abs() < 1e-10, "matrix[3] should be 1.5");
    }

    #[test]
    fn read_bbox_matrix_matrix_with_non_numeric_element_falls_back_to_identity() {
        // /Matrix with a non-numeric element → identity (lines 505-506, 513)
        let xobj_str =
            "<< /Type /XObject /Subtype /Form /BBox [0 0 100 20] /Matrix [1 0 0 /Bad 0 0] /Length 0 >>\nstream\n\nendstream\n";
        let (n4, obj4_bytes) = obj_wrap(4, xobj_str.as_bytes().to_vec());
        let bytes = build_pdf("", &[(n4, obj4_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let (bbox, matrix) = read_xobj_bbox_and_matrix(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(bbox.is_some());
        assert_eq!(
            matrix,
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            "should fall back to identity"
        );
    }

    #[test]
    fn read_bbox_matrix_matrix_wrong_length_falls_back_to_identity() {
        // /Matrix with wrong length (5 elements instead of 6) → identity (line 538)
        let xobj_str =
            "<< /Type /XObject /Subtype /Form /BBox [0 0 100 20] /Matrix [1 0 0 1 0] /Length 0 >>\nstream\n\nendstream\n";
        let (n4, obj4_bytes) = obj_wrap(4, xobj_str.as_bytes().to_vec());
        let bytes = build_pdf("", &[(n4, obj4_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let (bbox, matrix) = read_xobj_bbox_and_matrix(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(bbox.is_some());
        assert_eq!(
            matrix,
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            "wrong-length matrix falls back to identity"
        );
    }

    // -----------------------------------------------------------------------
    // Unit tests for read_xobj_bbox_and_matrix: /BBox and /Matrix as indirect refs
    // -----------------------------------------------------------------------

    #[test]
    fn read_bbox_via_indirect_ref() {
        // /BBox as indirect reference to an Array object (lines 474-476)
        // obj 5 = Array [0 0 100 20]
        // obj 4 = Stream with /BBox 5 0 R
        let (n5, obj5_bytes) = {
            let body = "[0 0 100 20]\n";
            obj_wrap(5, body.as_bytes().to_vec())
        };
        // Build stream with /BBox pointing to obj 5
        let stream_header = "<< /Type /XObject /Subtype /Form /BBox 5 0 R /Length 0 >>";
        let mut stream_body = stream_header.as_bytes().to_vec();
        stream_body.extend_from_slice(b"\nstream\n\nendstream\n");
        let (n4, obj4_bytes) = obj_wrap(4, stream_body);

        let bytes = build_pdf("", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // We need to register obj5 as an Array in the pdf
        // Since the parser may not handle a bare array as an indirect object,
        // let's set it directly using set_object
        let array_ref = ObjectRef::new(5, 0);
        pdf.set_object(
            array_ref,
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(20),
            ]),
        );

        let (bbox, matrix) = read_xobj_bbox_and_matrix(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(bbox.is_some(), "BBox via indirect ref should be parsed");
        let b = bbox.unwrap();
        assert_eq!(b[2] as i64, 100);
        assert_eq!(matrix, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    }

    #[test]
    fn read_bbox_indirect_ref_non_array_returns_none() {
        // /BBox indirect ref resolving to non-array (line 476)
        // Set up stream with /BBox pointing to obj 5, which is a dict (not array)
        let stream_header = "<< /Type /XObject /Subtype /Form /BBox 5 0 R /Length 0 >>";
        let mut stream_body = stream_header.as_bytes().to_vec();
        stream_body.extend_from_slice(b"\nstream\n\nendstream\n");
        let (n4, obj4_bytes) = obj_wrap(4, stream_body);
        let (n5, obj5_bytes) = obj_dict(5, "<< /NotAnArray true >>");

        let bytes = build_pdf("", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let (bbox, matrix) = read_xobj_bbox_and_matrix(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(bbox.is_none(), "BBox ref → non-array should return None");
        assert_eq!(matrix, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    }

    #[test]
    fn read_matrix_via_indirect_ref() {
        // /Matrix as indirect reference (lines 516-536)
        // Build stream with /BBox [0 0 100 20] and /Matrix pointing to obj 5
        let stream_header =
            "<< /Type /XObject /Subtype /Form /BBox [0 0 100 20] /Matrix 5 0 R /Length 0 >>";
        let mut stream_body = stream_header.as_bytes().to_vec();
        stream_body.extend_from_slice(b"\nstream\n\nendstream\n");
        let (n4, obj4_bytes) = obj_wrap(4, stream_body);
        let (n5, obj5_bytes) = obj_dict(5, "<< /NotAnArray true >>");

        let bytes = build_pdf("", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // Override obj 5 to be a proper 6-element array via set_object
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Array(vec![
                Object::Real(2.0),
                Object::Integer(0),
                Object::Integer(0),
                Object::Real(2.0),
                Object::Integer(0),
                Object::Integer(0),
            ]),
        );

        let (bbox, matrix) = read_xobj_bbox_and_matrix(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(bbox.is_some());
        assert!(
            (matrix[0] - 2.0).abs() < 1e-10,
            "matrix[0] via indirect ref should be 2.0"
        );
        assert!(
            (matrix[3] - 2.0).abs() < 1e-10,
            "matrix[3] via indirect ref should be 2.0"
        );
    }

    #[test]
    fn read_matrix_indirect_ref_non_array_returns_identity() {
        // /Matrix ref → non-array → identity (line 536)
        let stream_header =
            "<< /Type /XObject /Subtype /Form /BBox [0 0 100 20] /Matrix 5 0 R /Length 0 >>";
        let mut stream_body = stream_header.as_bytes().to_vec();
        stream_body.extend_from_slice(b"\nstream\n\nendstream\n");
        let (n4, obj4_bytes) = obj_wrap(4, stream_body);
        let (n5, obj5_bytes) = obj_dict(5, "<< /NotAnArray true >>");

        let bytes = build_pdf("", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let (bbox, matrix) = read_xobj_bbox_and_matrix(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(bbox.is_some());
        assert_eq!(
            matrix,
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            "non-array matrix ref → identity"
        );
    }

    #[test]
    fn read_matrix_indirect_ref_with_non_numeric_element_falls_back_to_identity() {
        // /Matrix ref → 6-element array with non-numeric element → identity (lines 524-526, 532-533)
        let stream_header =
            "<< /Type /XObject /Subtype /Form /BBox [0 0 100 20] /Matrix 5 0 R /Length 0 >>";
        let mut stream_body = stream_header.as_bytes().to_vec();
        stream_body.extend_from_slice(b"\nstream\n\nendstream\n");
        let (n4, obj4_bytes) = obj_wrap(4, stream_body);
        let (n5, obj5_bytes) = obj_dict(5, "<< /NotAnArray true >>");

        let bytes = build_pdf("", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // Set obj5 to 6-element array with a bad element
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Array(vec![
                Object::Integer(1),
                Object::Integer(0),
                Object::Integer(0),
                Object::Name(b"BadElement".to_vec()), // non-numeric
                Object::Integer(0),
                Object::Integer(0),
            ]),
        );

        let (bbox, matrix) = read_xobj_bbox_and_matrix(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(bbox.is_some());
        assert_eq!(
            matrix,
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            "bad element in matrix ref → identity"
        );
    }

    #[test]
    fn read_matrix_indirect_ref_wrong_length_returns_identity() {
        // /Matrix ref → array with wrong length (not 6) → identity (line 517 guard fails)
        let stream_header =
            "<< /Type /XObject /Subtype /Form /BBox [0 0 100 20] /Matrix 5 0 R /Length 0 >>";
        let mut stream_body = stream_header.as_bytes().to_vec();
        stream_body.extend_from_slice(b"\nstream\n\nendstream\n");
        let (n4, obj4_bytes) = obj_wrap(4, stream_body);
        let (n5, obj5_bytes) = obj_dict(5, "<< /NotAnArray true >>");

        let bytes = build_pdf("", &[(n4, obj4_bytes), (n5, obj5_bytes)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // Set obj5 to a 4-element array (wrong length)
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Array(vec![
                Object::Integer(1),
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(1),
            ]),
        );

        let (bbox, matrix) = read_xobj_bbox_and_matrix(&mut pdf, ObjectRef::new(4, 0)).unwrap();
        assert!(bbox.is_some());
        assert_eq!(
            matrix,
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            "wrong-length matrix ref → identity"
        );
    }

    // -----------------------------------------------------------------------
    // Unit tests for read_annot_flags private fn
    // -----------------------------------------------------------------------

    #[test]
    fn read_annot_flags_non_dict_returns_zero() {
        // annot_ref resolves to a non-dict object → returns 0 (line 327)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // Register obj 10 as an Integer (not a dict)
        let annot_ref = ObjectRef::new(10, 0);
        pdf.set_object(annot_ref, Object::Integer(42));

        let flags = read_annot_flags(&mut pdf, annot_ref).unwrap();
        assert_eq!(flags, 0, "non-dict annot should return flags=0");
    }

    #[test]
    fn read_annot_flags_f_as_indirect_ref() {
        // /F value is an indirect reference to an Integer (line 335)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // obj 11 = Integer 4 (Print bit)
        let flag_ref = ObjectRef::new(11, 0);
        pdf.set_object(flag_ref, Object::Integer(4));

        // obj 10 = annotation dict with /F → 11 0 R
        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        annot_dict.insert("Type", Object::Name(b"Annot".to_vec()));
        annot_dict.insert("F", Object::Reference(flag_ref));
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let flags = read_annot_flags(&mut pdf, annot_ref).unwrap();
        assert_eq!(
            flags, 4,
            "/F via indirect ref should resolve to 4 (Print bit)"
        );
    }

    // -----------------------------------------------------------------------
    // Unit tests for resolve_ap_n private fn
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_ap_n_non_dict_annot_returns_none() {
        // annot_ref resolves to non-dict → None (line 359)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let annot_ref = ObjectRef::new(10, 0);
        pdf.set_object(annot_ref, Object::Integer(99));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert!(result.is_none(), "non-dict annot should return None");
    }

    #[test]
    fn resolve_ap_n_ap_null_returns_none() {
        // /AP is null → None (line 364)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        annot_dict.insert("AP", Object::Null);
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn resolve_ap_n_ap_as_indirect_ref_to_dict() {
        // /AP is an indirect ref to a dict (lines 369-370)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // obj 11 = Form XObject stream
        let xobj_ref = ObjectRef::new(11, 0);
        let mut xobj_dict = Dictionary::new();
        xobj_dict.insert("Type", Object::Name(b"XObject".to_vec()));
        xobj_dict.insert("Subtype", Object::Name(b"Form".to_vec()));
        xobj_dict.insert(
            "BBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(20),
            ]),
        );
        pdf.set_object(xobj_ref, Object::Stream(Stream::new(xobj_dict, vec![])));

        // obj 12 = AP dict {N: 11 0 R} as indirect object
        let ap_dict_ref = ObjectRef::new(12, 0);
        let mut ap_dict = Dictionary::new();
        ap_dict.insert("N", Object::Reference(xobj_ref));
        pdf.set_object(ap_dict_ref, Object::Dictionary(ap_dict));

        // obj 10 = annotation with /AP as indirect ref → obj 12
        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        annot_dict.insert("Type", Object::Name(b"Annot".to_vec()));
        annot_dict.insert("AP", Object::Reference(ap_dict_ref));
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert_eq!(
            result,
            Some(xobj_ref),
            "/AP as indirect ref should resolve to xobj"
        );
    }

    #[test]
    fn resolve_ap_n_ap_ref_to_non_dict_returns_none() {
        // /AP is indirect ref to non-dict → None (line 371)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let bad_ref = ObjectRef::new(11, 0);
        pdf.set_object(bad_ref, Object::Integer(42));

        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        annot_dict.insert("AP", Object::Reference(bad_ref));
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert!(result.is_none(), "/AP ref → non-dict should return None");
    }

    #[test]
    fn resolve_ap_n_ap_direct_non_dict_returns_none() {
        // /AP is a direct non-dict value (e.g. Integer) → None (line 373)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        annot_dict.insert("AP", Object::Integer(99));
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert!(result.is_none(), "non-dict /AP should return None");
    }

    #[test]
    fn resolve_ap_n_n_null_returns_none() {
        // /N is null → None (line 378)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        let mut ap_dict = Dictionary::new();
        ap_dict.insert("N", Object::Null);
        annot_dict.insert("AP", Object::Dictionary(ap_dict));
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert!(result.is_none(), "/N null should return None");
    }

    #[test]
    fn resolve_ap_n_n_ref_to_dict_selects_by_as() {
        // /N is ref → dict (state dict case), selects by /AS (lines 391,393)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // obj 11 = Form XObject stream (the "On" state)
        let xobj_ref = ObjectRef::new(11, 0);
        let mut xobj_dict = Dictionary::new();
        xobj_dict.insert("Type", Object::Name(b"XObject".to_vec()));
        xobj_dict.insert("Subtype", Object::Name(b"Form".to_vec()));
        xobj_dict.insert(
            "BBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(20),
                Object::Integer(20),
            ]),
        );
        pdf.set_object(xobj_ref, Object::Stream(Stream::new(xobj_dict, vec![])));

        // obj 12 = state dict {On: 11 0 R, Off: ...}
        let state_dict_ref = ObjectRef::new(12, 0);
        let mut state_dict = Dictionary::new();
        state_dict.insert("On", Object::Reference(xobj_ref));
        pdf.set_object(state_dict_ref, Object::Dictionary(state_dict));

        // obj 10 = annotation with /AP/N as ref to state dict, /AS /On
        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        let mut ap_dict = Dictionary::new();
        ap_dict.insert("N", Object::Reference(state_dict_ref));
        annot_dict.insert("AP", Object::Dictionary(ap_dict));
        annot_dict.insert("AS", Object::Name(b"On".to_vec()));
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert_eq!(
            result,
            Some(xobj_ref),
            "state dict /AS selection should return correct xobj"
        );
    }

    #[test]
    fn resolve_ap_n_n_ref_to_non_stream_non_dict_returns_none() {
        // /N ref → non-stream/non-dict → None (line 393)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let bad_ref = ObjectRef::new(11, 0);
        pdf.set_object(bad_ref, Object::Integer(99));

        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        let mut ap_dict = Dictionary::new();
        ap_dict.insert("N", Object::Reference(bad_ref));
        annot_dict.insert("AP", Object::Dictionary(ap_dict));
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert!(
            result.is_none(),
            "/N ref → non-stream/dict should return None"
        );
    }

    #[test]
    fn resolve_ap_n_n_direct_integer_returns_none() {
        // /N is a direct non-stream/dict/ref value → None (line 404)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        let mut ap_dict = Dictionary::new();
        ap_dict.insert("N", Object::Integer(42));
        annot_dict.insert("AP", Object::Dictionary(ap_dict));
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert!(result.is_none(), "direct integer /N should return None");
    }

    #[test]
    fn resolve_ap_n_as_via_indirect_ref() {
        // /AS is an indirect ref → Name (lines 423-424)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // obj 11 = Form XObject stream
        let xobj_ref = ObjectRef::new(11, 0);
        let mut xobj_dict = Dictionary::new();
        xobj_dict.insert("Type", Object::Name(b"XObject".to_vec()));
        xobj_dict.insert("Subtype", Object::Name(b"Form".to_vec()));
        xobj_dict.insert(
            "BBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(20),
                Object::Integer(20),
            ]),
        );
        pdf.set_object(xobj_ref, Object::Stream(Stream::new(xobj_dict, vec![])));

        // obj 13 = Name "On" (indirect)
        let name_ref = ObjectRef::new(13, 0);
        pdf.set_object(name_ref, Object::Name(b"On".to_vec()));

        // obj 12 = state dict
        let state_dict_ref = ObjectRef::new(12, 0);
        let mut state_dict = Dictionary::new();
        state_dict.insert("On", Object::Reference(xobj_ref));
        pdf.set_object(state_dict_ref, Object::Dictionary(state_dict));

        // obj 10 = annotation with /AS as indirect ref → Name "On"
        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        let mut ap_dict = Dictionary::new();
        ap_dict.insert("N", Object::Reference(state_dict_ref));
        annot_dict.insert("AP", Object::Dictionary(ap_dict));
        annot_dict.insert("AS", Object::Reference(name_ref));
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert_eq!(result, Some(xobj_ref), "/AS via indirect ref should work");
    }

    #[test]
    fn resolve_ap_n_as_ref_to_non_name_returns_none() {
        // /AS ref → non-Name → None (line 425)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // obj 13 = Integer (not a Name)
        let bad_ref = ObjectRef::new(13, 0);
        pdf.set_object(bad_ref, Object::Integer(42));

        // obj 12 = state dict
        let state_dict_ref = ObjectRef::new(12, 0);
        let mut state_dict = Dictionary::new();
        state_dict.insert("On", Object::Integer(0));
        pdf.set_object(state_dict_ref, Object::Dictionary(state_dict));

        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        let mut ap_dict = Dictionary::new();
        ap_dict.insert("N", Object::Reference(state_dict_ref));
        annot_dict.insert("AP", Object::Dictionary(ap_dict));
        annot_dict.insert("AS", Object::Reference(bad_ref));
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert!(result.is_none(), "/AS ref → non-name should return None");
    }

    #[test]
    fn resolve_ap_n_state_dict_as_absent_returns_none() {
        // No /AS in annotation when state dict selected → None (line 427)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // obj 12 = state dict
        let state_dict_ref = ObjectRef::new(12, 0);
        let mut state_dict = Dictionary::new();
        state_dict.insert("On", Object::Integer(0));
        pdf.set_object(state_dict_ref, Object::Dictionary(state_dict));

        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        let mut ap_dict = Dictionary::new();
        ap_dict.insert("N", Object::Reference(state_dict_ref));
        annot_dict.insert("AP", Object::Dictionary(ap_dict));
        // No /AS key
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert!(result.is_none(), "missing /AS should return None");
    }

    #[test]
    fn resolve_ap_n_state_dict_entry_ref_to_non_stream_returns_none() {
        // State dict entry ref → non-stream → None (line 434)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // obj 11 = Integer (not a stream)
        let non_stream_ref = ObjectRef::new(11, 0);
        pdf.set_object(non_stream_ref, Object::Integer(99));

        // obj 12 = state dict with /On → bad ref
        let state_dict_ref = ObjectRef::new(12, 0);
        let mut state_dict = Dictionary::new();
        state_dict.insert("On", Object::Reference(non_stream_ref));
        pdf.set_object(state_dict_ref, Object::Dictionary(state_dict));

        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        let mut ap_dict = Dictionary::new();
        ap_dict.insert("N", Object::Reference(state_dict_ref));
        annot_dict.insert("AP", Object::Dictionary(ap_dict));
        annot_dict.insert("AS", Object::Name(b"On".to_vec()));
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert!(
            result.is_none(),
            "state entry ref → non-stream should return None"
        );
    }

    #[test]
    fn resolve_ap_n_state_dict_missing_key_returns_none() {
        // State dict does not have the /AS key → None (line 442)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // obj 12 = state dict without "On" key
        let state_dict_ref = ObjectRef::new(12, 0);
        let mut state_dict = Dictionary::new();
        state_dict.insert("Off", Object::Integer(0));
        pdf.set_object(state_dict_ref, Object::Dictionary(state_dict));

        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        let mut ap_dict = Dictionary::new();
        ap_dict.insert("N", Object::Reference(state_dict_ref));
        annot_dict.insert("AP", Object::Dictionary(ap_dict));
        annot_dict.insert("AS", Object::Name(b"On".to_vec())); // key not in state dict
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert!(
            result.is_none(),
            "missing state dict key should return None"
        );
    }

    // -----------------------------------------------------------------------
    // Unit tests for build_pruned_annots_array private fn
    // -----------------------------------------------------------------------

    #[test]
    fn pruned_annots_no_annots_entry_returns_empty() {
        // page_dict has no /Annots → empty vec (line 554)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let page_dict = Dictionary::new();
        let result = build_pruned_annots_array(&mut pdf, &page_dict, &[]).unwrap();
        assert!(result.is_empty(), "no /Annots should return empty vec");
    }

    #[test]
    fn pruned_annots_annots_as_indirect_ref_to_array() {
        // /Annots is an indirect ref to an array (lines 559-560)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let annot_ref = ObjectRef::new(10, 0);
        let keep_ref = ObjectRef::new(11, 0);

        // Set up obj 20 = array [annot_ref, keep_ref]
        let arr_ref = ObjectRef::new(20, 0);
        pdf.set_object(
            arr_ref,
            Object::Array(vec![
                Object::Reference(annot_ref),
                Object::Reference(keep_ref),
            ]),
        );

        let mut page_dict = Dictionary::new();
        page_dict.insert("Annots", Object::Reference(arr_ref));

        let result = build_pruned_annots_array(&mut pdf, &page_dict, &[annot_ref]).unwrap();
        assert_eq!(result.len(), 1, "one annot should be pruned");
        assert_eq!(result[0], Object::Reference(keep_ref));
    }

    #[test]
    fn pruned_annots_annots_ref_to_non_array_returns_empty() {
        // /Annots indirect ref → non-array → empty (line 561)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let bad_ref = ObjectRef::new(20, 0);
        pdf.set_object(bad_ref, Object::Integer(42));

        let mut page_dict = Dictionary::new();
        page_dict.insert("Annots", Object::Reference(bad_ref));

        let result = build_pruned_annots_array(&mut pdf, &page_dict, &[]).unwrap();
        assert!(result.is_empty(), "ref → non-array should return empty");
    }

    #[test]
    fn pruned_annots_annots_direct_non_array_returns_empty() {
        // /Annots is a direct non-array value → empty (line 563)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let mut page_dict = Dictionary::new();
        page_dict.insert("Annots", Object::Integer(99));

        let result = build_pruned_annots_array(&mut pdf, &page_dict, &[]).unwrap();
        assert!(
            result.is_empty(),
            "direct non-array /Annots should return empty"
        );
    }

    #[test]
    fn pruned_annots_non_ref_entries_are_kept() {
        // Array with non-ref entries (unusual) — these should be kept (line 570)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let keep_ref = ObjectRef::new(11, 0);
        let remove_ref = ObjectRef::new(10, 0);

        let mut page_dict = Dictionary::new();
        page_dict.insert(
            "Annots",
            Object::Array(vec![
                Object::Reference(remove_ref),
                Object::Integer(42), // non-ref entry — keep
                Object::Reference(keep_ref),
            ]),
        );

        let result = build_pruned_annots_array(&mut pdf, &page_dict, &[remove_ref]).unwrap();
        assert_eq!(result.len(), 2, "non-ref entries should be kept");
        assert_eq!(result[0], Object::Integer(42));
        assert_eq!(result[1], Object::Reference(keep_ref));
    }

    // -----------------------------------------------------------------------
    // Test: /AP/N direct stream materializes as new indirect object (lines 402, 408-412)
    // -----------------------------------------------------------------------
    #[test]
    fn resolve_ap_n_direct_stream_materializes() {
        // /AP/N is a direct Object::Stream (defensive path for malformed PDFs)
        // This requires constructing the object directly via set_object
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // Build an inline (direct) stream for /N
        let mut xobj_dict = Dictionary::new();
        xobj_dict.insert("Type", Object::Name(b"XObject".to_vec()));
        xobj_dict.insert("Subtype", Object::Name(b"Form".to_vec()));
        xobj_dict.insert(
            "BBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(20),
            ]),
        );
        let inline_stream = Object::Stream(Stream::new(xobj_dict, b"q Q".to_vec()));

        // obj 10 = annotation dict with /AP/N as direct stream
        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        let mut ap_dict = Dictionary::new();
        ap_dict.insert("N", inline_stream);
        annot_dict.insert("AP", Object::Dictionary(ap_dict));
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert!(
            result.is_some(),
            "direct stream /N should be materialized and returned"
        );
    }

    // -----------------------------------------------------------------------
    // Test: state dict with direct stream entry materializes (lines 436-440)
    // -----------------------------------------------------------------------
    #[test]
    fn resolve_ap_n_state_dict_direct_stream_entry_materializes() {
        // State dict entry is a direct Object::Stream (defensive path)
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // Build inline stream for /On state entry
        let mut xobj_dict = Dictionary::new();
        xobj_dict.insert("Type", Object::Name(b"XObject".to_vec()));
        xobj_dict.insert("Subtype", Object::Name(b"Form".to_vec()));
        xobj_dict.insert(
            "BBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(20),
                Object::Integer(20),
            ]),
        );
        let inline_stream = Object::Stream(Stream::new(xobj_dict, vec![]));

        // obj 10 = annotation with /AP/N as direct state dict
        let annot_ref = ObjectRef::new(10, 0);
        let mut annot_dict = Dictionary::new();
        let mut ap_dict = Dictionary::new();
        let mut state_dict = Dictionary::new();
        state_dict.insert("On", inline_stream);
        ap_dict.insert("N", Object::Dictionary(state_dict));
        annot_dict.insert("AP", Object::Dictionary(ap_dict));
        annot_dict.insert("AS", Object::Name(b"On".to_vec()));
        pdf.set_object(annot_ref, Object::Dictionary(annot_dict));

        let result = resolve_ap_n(&mut pdf, annot_ref).unwrap();
        assert!(
            result.is_some(),
            "state dict direct stream entry should be materialized"
        );
    }

    // -----------------------------------------------------------------------
    // Test: /XObject in resources is ref → non-dict → creates empty dict (line 206)
    // -----------------------------------------------------------------------
    #[test]
    fn resources_xobject_indirect_ref_to_non_dict_uses_empty_dict() {
        // /Resources/XObject is an indirect ref to a non-dict object (e.g. Integer)
        // This should fall back to an empty XObject dict
        let xobj_body = make_xobj_stream([0.0, 0.0, 100.0, 20.0], b"");
        let (n5, obj5_bytes) = obj_wrap(5, xobj_body);
        let (n4, obj4_bytes) = obj_dict(
            4,
            "<< /Type /Annot /Subtype /Widget /Rect [50 50 150 70] /AP << /N 5 0 R >> >>",
        );
        // obj 6: a non-dict object (Integer), used as /XObject ref
        let (n6, obj6_bytes) = obj_dict(6, "42"); // integer as standalone object

        let bytes = build_pdf(
            "/Annots [4 0 R] /Resources << /XObject 6 0 R >>",
            &[(n4, obj4_bytes), (n5, obj5_bytes), (n6, obj6_bytes)],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // Override obj 6 to be a non-dict value to trigger the fallback path
        pdf.set_object(ObjectRef::new(6, 0), Object::Integer(42));

        let page_ref = ObjectRef::new(3, 0);
        let count = flatten_annotations_on_page(&mut pdf, page_ref, FlattenMode::All).unwrap();
        assert_eq!(count, 1, "should flatten even when /XObject ref → non-dict");

        let content = page_content_bytes(&mut pdf, page_ref).unwrap();
        assert!(content.windows(2).any(|w| w == b"Do"));
    }

    // -----------------------------------------------------------------------
    // Test: /BBox as a direct non-array value (line 478)
    // -----------------------------------------------------------------------
    #[test]
    fn read_bbox_direct_non_array_value_returns_none() {
        // /BBox is a direct non-array, non-ref value (e.g., a Name) → (None, identity)
        // Must use set_object since the parser normalizes, but we can inject it directly
        let bytes = build_pdf("", &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // Build stream object with /BBox as an Integer directly
        let xobj_ref = ObjectRef::new(10, 0);
        let mut xobj_dict = Dictionary::new();
        xobj_dict.insert("Type", Object::Name(b"XObject".to_vec()));
        xobj_dict.insert("Subtype", Object::Name(b"Form".to_vec()));
        // Set /BBox to an Integer (not an array) — malformed
        xobj_dict.insert("BBox", Object::Integer(99));
        pdf.set_object(xobj_ref, Object::Stream(Stream::new(xobj_dict, vec![])));

        let (bbox, matrix) = read_xobj_bbox_and_matrix(&mut pdf, xobj_ref).unwrap();
        assert!(bbox.is_none(), "direct non-array /BBox should return None");
        assert_eq!(matrix, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    }

    // -----------------------------------------------------------------------
    // Test: fmt_f64 for non-finite / non-integer float
    // -----------------------------------------------------------------------
    #[test]
    fn fmt_f64_non_integer_float() {
        // covers the else branch of fmt_f64 (line 316-319)
        assert_eq!(fmt_f64(0.5), "0.5");
        assert_eq!(fmt_f64(1.25), "1.25");
        assert_eq!(fmt_f64(0.123456), "0.123456");
        // trailing zeros removed
        assert_eq!(fmt_f64(1.500_000), "1.5");
        // very large integer-valued float
        assert_eq!(fmt_f64(1_000_000.0), "1000000");
    }
}
