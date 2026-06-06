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
            Object::Real(r) => *r,
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
                    Object::Real(r) => *r,
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
                        Object::Real(r) => *r,
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
}
