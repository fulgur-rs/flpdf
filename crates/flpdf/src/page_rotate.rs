//! `/Rotate` manipulation for PDF pages.
//!
//! Applies rotation to a set of leaf `Page` objects in two modes:
//!
//! - **Assign** — replaces the existing `/Rotate` value (or the inherited one) with
//!   the supplied angle.
//! - **Add** — adds the supplied angle to the resolved (inherited) `/Rotate` value.
//!
//! All results are *normalized* to one of `{0, 90, 180, 270}` (modulo 360).
//! Inheritance is resolved before writing: if a leaf page has no `/Rotate` entry of
//! its own, the value is read from the first ancestor `Pages` node that carries one,
//! and then the computed value is *materialized* (written explicitly on the leaf),
//! so the leaf no longer depends on inheritance.
//!
//! ISO 32000-1 §7.7.3.4 lists `/Rotate` as an inheritable page attribute; its default
//! when absent at every level is `0` (§7.7.3.3 Table 30).
//!
//! # `/Rotate` flattening
//!
//! [`flatten_rotation_on_pages`] *bakes* a page's effective `/Rotate` into its
//! geometry so the page reads upright with `/Rotate = 0`. A single affine matrix
//! `M` is prepended to the content stream (`q M cm … Q`) and the **same** `M` is
//! applied to every present page box (`/MediaBox`, `/CropBox`, `/BleedBox`,
//! `/TrimBox`, `/ArtBox`) and to each annotation `/Rect`. Because content and
//! geometry share one matrix, visual rendering is unchanged by construction.
//!
//! ## Caveats (held for review)
//!
//! - **Annotation appearance is not rotated.** Only `/Rect` is transformed;
//!   `/QuadPoints` and the appearance `/AP` `/Matrix` are left as-is, so an
//!   annotation's *appearance* orientation can change.
//! - **Not byte-identical.** The page content is rebuilt from its decoded bytes
//!   wrapped in one new stream, so exact byte-parity with the source is not a
//!   goal. qpdf exposes `flattenRotation` only through its C++ API (no CLI), so
//!   there is no qpdf oracle; correctness is asserted via invariants in tests.

use crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH;
use crate::{Dictionary, Error, Object, ObjectRef, PageBox, Pdf, Result, Stream};
use std::collections::BTreeSet;
use std::io::{Read, Seek};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Whether to replace or add to the existing `/Rotate` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotateMode {
    /// Replace the (resolved, inherited) `/Rotate` with the supplied angle.
    Assign,
    /// Add the supplied angle to the (resolved, inherited) `/Rotate`.
    Add,
}

/// A rotation operation: mode plus angle in degrees.
///
/// Angles need not be multiples of 90; they will be composed and then
/// normalized to one of `{0, 90, 180, 270}` by [`normalize_rotate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RotateOp {
    /// Whether this is an assignment or an additive rotation.
    pub mode: RotateMode,
    /// Angle in degrees (positive = clockwise per PDF convention). May be
    /// negative or exceed 360.
    pub degrees: i32,
}

// ---------------------------------------------------------------------------
// Pure helper functions
// ---------------------------------------------------------------------------

/// Normalize any integer degrees value to one of `{0, 90, 180, 270}`.
///
/// The algorithm:
/// 1. Add 45 to bias toward the *nearest* 90° boundary.
/// 2. Integer-divide by 90 with `div_euclid` (its remainder is always in
///    `[0, 90)`) to obtain the nearest-multiple index. The quotient itself may
///    be negative for sufficiently negative inputs (e.g. `-46 → -1`); it is the
///    final `rem_euclid(360)` in step 4 — not this division — that guarantees a
///    non-negative result.
/// 3. Multiply back by 90 to recover the snapped angle.
/// 4. Take `rem_euclid(360)` to wrap into `[0, 360)`.
///
/// **Non-multiples-of-90 inputs**: ISO 32000-1 §7.7.3.3 Table 30 restricts
/// `/Rotate` to `{0, 90, 180, 270}`, but malformed PDFs sometimes carry other
/// values.  Our policy is to snap to the nearest valid boundary rather than
/// rejecting them, so a malformed `/Rotate` never aborts a page operation.
///
/// Examples:
/// - `  0` → `  0`
/// - ` 90` → ` 90`
/// - `180` → `180`
/// - `270` → `270`
/// - `360` → `  0`
/// - `450` → ` 90`
/// - `-90` → `270`
/// - ` 45` → ` 90`  (rounded up — nearest boundary)
/// - ` 44` → `  0`  (rounded down — nearest boundary)
pub fn normalize_rotate(deg: i32) -> i32 {
    normalize_rotate_i64(deg as i64)
}

/// Normalize an `i64` rotation to `{0, 90, 180, 270}`.
///
/// Internal helper so every entry point — public `i32` API, composed sums,
/// and raw PDF `/Rotate` integers (which are `i64`) — normalizes *without*
/// a narrowing cast that could truncate or overflow before normalization.
fn normalize_rotate_i64(deg: i64) -> i32 {
    // Round `deg` to the nearest 90° boundary, then keep within [0, 360).
    // Widen to i128: `deg + 45` would overflow i64 for inputs near
    // `i64::MAX`/`i64::MIN`.
    // `div_euclid`'s remainder is always in `[0, 90)`, but its quotient can be
    // negative for sufficiently negative `deg` (e.g. `deg + 45 == -1` → `-1`).
    // The final `rem_euclid(360)` is what guarantees a non-negative result in
    // `[0, 360)`, even when `(deg + 45).div_euclid(90) * 90` is negative.
    let snapped = (deg as i128 + 45).div_euclid(90) * 90;
    snapped.rem_euclid(360) as i32
}

/// Compute the final `/Rotate` value for a page given `existing` (the resolved,
/// inherited current value) and `op`.
///
/// The returned value is always normalized to `{0, 90, 180, 270}`.
pub fn compose_rotate(existing: i32, op: &RotateOp) -> i32 {
    let raw: i64 = match op.mode {
        RotateMode::Assign => op.degrees as i64,
        RotateMode::Add => existing as i64 + op.degrees as i64,
    };
    normalize_rotate_i64(raw)
}

// ---------------------------------------------------------------------------
// Inheritance resolution
// ---------------------------------------------------------------------------

/// Return the effective `/Rotate` value for `page_ref`, walking up the `/Parent`
/// chain until a node carries a `/Rotate` entry.
///
/// Returns `0` (the PDF-spec default, ISO 32000-1 §7.7.3.3 Table 30) if no
/// node in the chain has a `/Rotate` entry.
///
/// Uses [`DEFAULT_MAX_PAGE_TREE_DEPTH`] as the depth limit.
///
/// # Errors
///
/// Propagates any error from [`resolve_inherited_rotate_with_max_depth`].
pub fn resolve_inherited_rotate<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<i32> {
    resolve_inherited_rotate_with_max_depth(pdf, page_ref, DEFAULT_MAX_PAGE_TREE_DEPTH)
}

/// Like [`resolve_inherited_rotate`] but with a caller-supplied recursion limit.
///
/// # Errors
///
/// - [`Error::Unsupported`] if walking the `/Parent` chain reaches `max_depth`
///   before finding a `/Rotate` entry.
/// - [`Error::Unsupported`] if a `/Rotate` entry is an indirect reference that
///   does not resolve to an integer, or has an otherwise unexpected type.
/// - Any error from resolving objects in the page-tree chain.
pub fn resolve_inherited_rotate_with_max_depth<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    max_depth: usize,
) -> Result<i32> {
    let mut seen: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut current = page_ref;
    let mut depth: usize = 0;

    loop {
        if depth >= max_depth {
            return Err(Error::Unsupported(format!(
                "page tree depth exceeds maximum of {max_depth} at {current}"
            )));
        }

        // Cycle guard.
        if !seen.insert(current) {
            // We hit a cycle before finding /Rotate — default to 0.
            return Ok(0);
        }

        let node_obj = pdf.resolve_borrowed(current)?;
        let Object::Dictionary(dict) = node_obj else {
            // Not a dictionary — cannot walk further; use default.
            return Ok(0);
        };

        let rotate_val = dict.get("Rotate").cloned();
        let parent_val = dict.get("Parent").cloned();

        // Check for /Rotate on this node.
        // Per ISO 32000-1 §7.3.9, a null value is equivalent to absent.
        if let Some(rotate_val) = rotate_val {
            match rotate_val {
                // null → treat as absent; continue walking.
                Object::Null => {}
                Object::Integer(n) => return Ok(normalize_rotate_i64(n)),
                Object::Reference(r) => {
                    let resolved = pdf.resolve_borrowed(r)?;
                    match resolved {
                        Object::Null => {}
                        Object::Integer(n) => return Ok(normalize_rotate_i64(*n)),
                        _ => {
                            return Err(Error::Unsupported(format!(
                                "/Rotate reference {r} on node {current} does not resolve to an integer"
                            )));
                        }
                    }
                }
                _ => {
                    return Err(Error::Unsupported(format!(
                        "/Rotate entry on node {current} has unexpected type"
                    )));
                }
            }
        }

        // No /Rotate here — try the /Parent.
        let parent_val = match parent_val {
            Some(Object::Null) | None => return Ok(0), // no parent, use default
            Some(v) => v,
        };

        match parent_val {
            Object::Reference(r) => {
                current = r;
                depth += 1;
            }
            // Non-reference /Parent is non-standard; treat as absent.
            _ => return Ok(0),
        }
    }
}

// ---------------------------------------------------------------------------
// Main mutating entry point
// ---------------------------------------------------------------------------

/// Apply `op` to each `ObjectRef` in `pages`, materializing the resulting
/// `/Rotate` explicitly on every leaf page dictionary.
///
/// Inheritance is resolved *before* any write: if a leaf has no `/Rotate` of its
/// own, the inherited value is read from the ancestor chain.  The computed angle
/// (via [`compose_rotate`]) is then written directly on the leaf, so the leaf no
/// longer depends on the parent's value.
///
/// # Errors
///
/// Returns [`Error::Unsupported`] if any of the supplied `ObjectRef`s does not
/// resolve to a dictionary, does not resolve to a leaf `/Page` object (e.g. it
/// points at a `/Pages` tree node), or if the page-tree depth limit is
/// exceeded.
pub fn apply_rotate_to_pages<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    pages: &[ObjectRef],
    op: &RotateOp,
) -> Result<()> {
    for &page_ref in pages {
        // 1. Resolve the current (inherited) /Rotate before modification.
        let existing = resolve_inherited_rotate(pdf, page_ref)?;

        // 2. Compute the new value.
        let new_rotate = compose_rotate(existing, op);

        // 3. Re-resolve the page dictionary (it may have changed if there are
        //    multiple pages sharing a parent — re-resolution is safe because
        //    Pdf::resolve goes through the cache).
        let page_obj = pdf.resolve(page_ref)?;
        let Object::Dictionary(mut page_dict) = page_obj else {
            return Err(Error::Unsupported(format!(
                "object {page_ref} is not a dictionary, cannot set /Rotate"
            )));
        };

        // Guard: only leaf `/Page` objects are valid targets. Writing /Rotate
        // onto a `/Pages` tree node (or any non-Page dict) would change the
        // inherited rotation of every descendant page, violating the
        // per-leaf-page contract.
        let is_leaf_page = matches!(
            page_dict.get("Type"),
            Some(Object::Name(t)) if t.as_slice() == b"Page"
        );
        if !is_leaf_page {
            return Err(Error::Unsupported(format!(
                "object {page_ref} is not a leaf /Page (missing or non-/Page /Type), cannot set /Rotate"
            )));
        }

        // 4. Materialize the new /Rotate on the leaf.
        //    We always write it explicitly (even for 0) so the leaf is no longer
        //    dependent on any ancestor's /Rotate.
        page_dict.insert("Rotate", Object::Integer(new_rotate as i64));
        pdf.set_object(page_ref, Object::Dictionary(page_dict));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Affine matrix primitives for /Rotate flattening (flpdf-9hc.9.9)
// ---------------------------------------------------------------------------
// Row-vector convention: a point [x y 1] maps to
//   (a*x + c*y + e, b*x + d*y + f)  for matrix [a b c d e f].
type Mat = [f64; 6];

/// Apply matrix `m` to point (x, y).
fn apply_matrix(m: Mat, x: f64, y: f64) -> (f64, f64) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

/// Compose A then B for a row vector: result == (p * A) * B.
fn mat_mul(a: Mat, b: Mat) -> Mat {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
        a[4] * b[0] + a[5] * b[2] + b[4],
        a[4] * b[1] + a[5] * b[3] + b[5],
    ]
}

/// Pure translation matrix.
fn translate(tx: f64, ty: f64) -> Mat {
    [1.0, 0.0, 0.0, 1.0, tx, ty]
}

/// Origin-0 rotation matrix for a box of width `w`, height `h`. `r` must be one
/// of {90,180,270}; any other value yields identity (callers normalize first and
/// skip r==0).
fn rotate_origin(r: i32, w: f64, h: f64) -> Mat {
    match r {
        90 => [0.0, -1.0, 1.0, 0.0, 0.0, w],
        180 => [-1.0, 0.0, 0.0, -1.0, w, h],
        270 => [0.0, 1.0, -1.0, 0.0, h, 0.0],
        _ => [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
    }
}

/// Build the flatten matrix `M` for normalized rotation `r` and the page's
/// MediaBox: translate the box lower-left to origin, rotate, translate back so
/// the transformed box's lower-left returns to (llx, lly).
fn rotation_matrix(r: i32, mb: PageBox) -> Mat {
    let w = mb.urx - mb.llx;
    let h = mb.ury - mb.lly;
    mat_mul(
        mat_mul(translate(-mb.llx, -mb.lly), rotate_origin(r, w, h)),
        translate(mb.llx, mb.lly),
    )
}

/// Map the 4 corners of `b` through `m` and return their axis-aligned bbox.
fn transform_box(m: Mat, b: PageBox) -> PageBox {
    let corners = [
        apply_matrix(m, b.llx, b.lly),
        apply_matrix(m, b.urx, b.lly),
        apply_matrix(m, b.urx, b.ury),
        apply_matrix(m, b.llx, b.ury),
    ];
    let llx = corners.iter().map(|c| c.0).fold(f64::INFINITY, f64::min);
    let lly = corners.iter().map(|c| c.1).fold(f64::INFINITY, f64::min);
    let urx = corners
        .iter()
        .map(|c| c.0)
        .fold(f64::NEG_INFINITY, f64::max);
    let ury = corners
        .iter()
        .map(|c| c.1)
        .fold(f64::NEG_INFINITY, f64::max);
    PageBox::new(llx, lly, urx, ury)
}

// ---------------------------------------------------------------------------
// Box array <-> PageBox + inherited-present-box lookup (flpdf-9hc.9.9)
// ---------------------------------------------------------------------------

/// Parse a PDF rectangle array `[x1 y1 x2 y2]` (ints or reals) into a `PageBox`,
/// normalizing so `llx<=urx` and `lly<=ury`. Returns `None` on the wrong shape.
fn object_to_pagebox(obj: &Object) -> Option<PageBox> {
    let Object::Array(a) = obj else { return None };
    if a.len() != 4 {
        return None;
    }
    let mut v = [0.0_f64; 4];
    for (i, e) in a.iter().enumerate() {
        v[i] = match e {
            Object::Integer(n) => *n as f64,
            Object::Real(r) => *r,
            _ => return None,
        };
    }
    Some(PageBox::new(
        v[0].min(v[2]),
        v[1].min(v[3]),
        v[0].max(v[2]),
        v[1].max(v[3]),
    ))
}

/// Emit a `PageBox` as a 4-element PDF real array.
fn pagebox_to_object(b: PageBox) -> Object {
    Object::Array(vec![
        Object::Real(b.llx),
        Object::Real(b.lly),
        Object::Real(b.urx),
        Object::Real(b.ury),
    ])
}

/// Return the explicit value of box `key` from the leaf page or the nearest
/// ancestor `/Pages` node that carries it. `None` if absent at every level (so the
/// box is left untouched — no invented boxes). No defaulting between box types.
fn inherited_present_box<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    key: &str,
) -> Result<Option<PageBox>> {
    let mut current = page_ref;
    let mut seen = BTreeSet::new();
    for _ in 0..=DEFAULT_MAX_PAGE_TREE_DEPTH {
        if !seen.insert(current) {
            break; // cycle guard
        }
        let Object::Dictionary(dict) = pdf.resolve(current)? else {
            break;
        };
        if let Some(obj) = dict.get(key).cloned() {
            let resolved = match obj {
                Object::Reference(r) => pdf.resolve(r)?,
                other => other,
            };
            if let Some(b) = object_to_pagebox(&resolved) {
                return Ok(Some(b));
            }
        }
        match dict.get("Parent") {
            Some(Object::Reference(p)) => current = *p,
            _ => break,
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// Content-stream wrapper builder (flpdf-9hc.9.9)
// ---------------------------------------------------------------------------

/// Format an f64 matrix operand without a trailing `.0` and without scientific
/// notation, so `cm` operands stay compact and valid (e.g. 200.0 -> "200").
fn format_matrix_number(v: f64) -> String {
    if v.is_finite() && v == v.trunc() {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.6}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// Wrap `inner` content bytes as `q\n{a b c d e f} cm\n{inner}\nQ\n`.
/// Whitespace at both seams prevents token merges (e.g. `ET`+`Q` -> `ETQ`).
fn wrap_content_with_matrix(m: Mat, inner: &[u8]) -> Vec<u8> {
    let cm = format!(
        "q\n{} {} {} {} {} {} cm\n",
        format_matrix_number(m[0]),
        format_matrix_number(m[1]),
        format_matrix_number(m[2]),
        format_matrix_number(m[3]),
        format_matrix_number(m[4]),
        format_matrix_number(m[5]),
    );
    let mut out = Vec::with_capacity(cm.len() + inner.len() + 3);
    out.extend_from_slice(cm.as_bytes());
    out.extend_from_slice(inner);
    out.extend_from_slice(b"\nQ\n");
    out
}

// ---------------------------------------------------------------------------
// Public API: flatten_rotation_on_pages (flpdf-9hc.9.9)
// ---------------------------------------------------------------------------

/// Box keys flattened, in a deterministic order.
const FLATTEN_BOX_KEYS: [&str; 5] = ["MediaBox", "CropBox", "BleedBox", "TrimBox", "ArtBox"];

/// Flatten the effective `/Rotate` of each leaf page into its content stream and
/// geometry. For each page: resolve the inherited+normalized rotation; if 0, skip.
/// Otherwise build matrix `M` from the rotation and the effective `/MediaBox`,
/// wrap the page content with `M`, transform every present box and annotation
/// `/Rect` with `M`, and materialize `/Rotate = 0` on the leaf. Visual rendering
/// is unchanged because content and geometry share the single matrix `M`.
///
/// # Caveat (held for review)
///
/// Annotation `/QuadPoints` and the appearance `/AP` `/Matrix` are **not**
/// transformed — only `/Rect`. An annotation's appearance orientation may
/// therefore change. Byte-identity with the source is not preserved: the page
/// content is rebuilt from its decoded bytes wrapped in a single new stream.
///
/// # Errors
///
/// - [`Error::Unsupported`] if a target ref is not a leaf `/Type /Page`, or a
///   rotated page has no resolvable `/MediaBox`.
/// - Any error from content decoding or object resolution.
pub fn flatten_rotation_on_pages<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    pages: &[ObjectRef],
) -> Result<()> {
    for &page_ref in pages {
        // Validate the target is a leaf /Page *before* the rotate==0 short-circuit,
        // so an invalid target is never silently accepted just because it happens
        // to resolve to rotation 0 (consistent error contract).
        let Object::Dictionary(mut page_dict) = pdf.resolve(page_ref)? else {
            return Err(Error::Unsupported(format!(
                "object {page_ref} is not a dictionary, cannot flatten /Rotate"
            )));
        };
        let is_leaf_page = matches!(
            page_dict.get("Type"),
            Some(Object::Name(t)) if t.as_slice() == b"Page"
        );
        if !is_leaf_page {
            return Err(Error::Unsupported(format!(
                "object {page_ref} is not a leaf /Page, cannot flatten /Rotate"
            )));
        }

        let rotate = normalize_rotate(resolve_inherited_rotate(pdf, page_ref)?);
        if rotate == 0 {
            continue; // nothing to flatten
        }

        // A sane matrix needs the page's effective MediaBox.
        let Some(mb) = inherited_present_box(pdf, page_ref, "MediaBox")? else {
            return Err(Error::Unsupported(format!(
                "page {page_ref} has no /MediaBox; cannot flatten /Rotate"
            )));
        };
        let m = rotation_matrix(rotate, mb);

        // Wrap the page content with `q M cm ... Q`.
        let inner = crate::pages::page_content_bytes(pdf, page_ref)?;
        let new_content = wrap_content_with_matrix(m, &inner);

        // Allocate a fresh content stream and repoint /Contents at it.
        let mut sdict = Dictionary::new();
        sdict.insert("Length", Object::Integer(new_content.len() as i64));
        let stream_ref = next_object_ref(pdf)?;
        pdf.set_object(stream_ref, Object::Stream(Stream::new(sdict, new_content)));
        page_dict.insert("Contents", Object::Reference(stream_ref));

        // Transform every present box; materialize on the leaf. Reads here see the
        // original boxes because `page_dict` is not written back until the end.
        for key in FLATTEN_BOX_KEYS {
            if let Some(b) = inherited_present_box(pdf, page_ref, key)? {
                page_dict.insert(key, pagebox_to_object(transform_box(m, b)));
            }
        }

        // Materialize /Rotate = 0 on the leaf (never inherited).
        page_dict.insert("Rotate", Object::Integer(0));

        // Transform annotation /Rect rectangles.
        flatten_annotation_rects(pdf, &page_dict, m)?;

        pdf.set_object(page_ref, Object::Dictionary(page_dict));
    }
    Ok(())
}

/// Transform every annotation's `/Rect` on this page by `m`. Reads `/Annots`
/// from `page_dict`; each entry is an indirect reference to an annotation dict.
///
/// CAVEAT: `/QuadPoints` and the appearance `/AP` `/Matrix` are intentionally
/// left untouched (see [`flatten_rotation_on_pages`]).
fn flatten_annotation_rects<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_dict: &Dictionary,
    m: Mat,
) -> Result<()> {
    // `/Annots` may be a direct array or an indirect reference to one; resolve
    // the reference before treating it as an array.
    let Some(annots_obj) = page_dict.get("Annots").cloned() else {
        return Ok(());
    };
    let annots_obj = match annots_obj {
        Object::Reference(r) => pdf.resolve(r)?,
        other => other,
    };
    let Object::Array(annots) = annots_obj else {
        return Ok(());
    };
    for entry in annots {
        let Object::Reference(annot_ref) = entry else {
            continue;
        };
        let Object::Dictionary(mut ad) = pdf.resolve(annot_ref)? else {
            continue;
        };
        if let Some(rect_obj) = ad.get("Rect").cloned() {
            let resolved = match rect_obj {
                Object::Reference(r) => pdf.resolve(r)?,
                other => other,
            };
            if let Some(b) = object_to_pagebox(&resolved) {
                ad.insert("Rect", pagebox_to_object(transform_box(m, b)));
                pdf.set_object(annot_ref, Object::Dictionary(ad));
            }
        }
    }
    Ok(())
}

/// Allocate a fresh indirect-object reference (the new-object idiom used across
/// the crate): one past the current highest object number.
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
    use crate::writer::write_pdf;
    use crate::{pages, Pdf};
    use std::io::Cursor;

    // -----------------------------------------------------------------------
    // Matrix primitives (flpdf-9hc.9.9 /Rotate flattening)
    // -----------------------------------------------------------------------

    #[test]
    fn rotation_matrix_origin0_constants() {
        let mb = PageBox::new(0.0, 0.0, 200.0, 300.0); // W=200 H=300
        assert_eq!(rotation_matrix(90, mb), [0.0, -1.0, 1.0, 0.0, 0.0, 200.0]);
        assert_eq!(
            rotation_matrix(180, mb),
            [-1.0, 0.0, 0.0, -1.0, 200.0, 300.0]
        );
        assert_eq!(rotation_matrix(270, mb), [0.0, 1.0, -1.0, 0.0, 300.0, 0.0]);
    }

    #[test]
    fn apply_matrix_maps_points() {
        // 90deg: (x,y) -> (y, W - x), W=200
        let m = [0.0, -1.0, 1.0, 0.0, 0.0, 200.0];
        assert_eq!(apply_matrix(m, 0.0, 0.0), (0.0, 200.0));
        assert_eq!(apply_matrix(m, 200.0, 0.0), (0.0, 0.0));
        assert_eq!(apply_matrix(m, 0.0, 300.0), (300.0, 200.0));
    }

    #[test]
    fn transform_box_swaps_dims_for_90_270() {
        let mb = PageBox::new(0.0, 0.0, 200.0, 300.0);
        let b90 = transform_box(rotation_matrix(90, mb), mb);
        assert_eq!(
            (b90.llx, b90.lly, b90.urx, b90.ury),
            (0.0, 0.0, 300.0, 200.0)
        );
        let b270 = transform_box(rotation_matrix(270, mb), mb);
        assert_eq!(
            (b270.llx, b270.lly, b270.urx, b270.ury),
            (0.0, 0.0, 300.0, 200.0)
        );
    }

    #[test]
    fn transform_box_keeps_dims_for_180() {
        let mb = PageBox::new(0.0, 0.0, 200.0, 300.0);
        let b = transform_box(rotation_matrix(180, mb), mb);
        assert_eq!((b.llx, b.lly, b.urx, b.ury), (0.0, 0.0, 200.0, 300.0));
    }

    #[test]
    fn rotation_matrix_preserves_lower_left_for_nonzero_origin() {
        // The transformed MediaBox lower-left must land back at the original (llx,lly).
        let mb = PageBox::new(10.0, 20.0, 210.0, 320.0); // W=200 H=300
        for r in [90, 180, 270] {
            let m = rotation_matrix(r, mb);
            let b = transform_box(m, mb);
            assert_eq!((b.llx, b.lly), (10.0, 20.0), "rotate {r} lower-left moved");
            let (w, h) = (b.urx - b.llx, b.ury - b.lly);
            if r == 180 {
                assert_eq!((w, h), (200.0, 300.0));
            } else {
                assert_eq!((w, h), (300.0, 200.0));
            }
        }
    }

    // -----------------------------------------------------------------------
    // Box array <-> PageBox (flpdf-9hc.9.9)
    // -----------------------------------------------------------------------

    #[test]
    fn pagebox_array_roundtrip() {
        let b = PageBox::new(10.0, 20.5, 210.0, 320.0);
        let obj = pagebox_to_object(b);
        assert_eq!(object_to_pagebox(&obj), Some(b));
    }

    #[test]
    fn object_to_pagebox_accepts_ints_and_reals() {
        let obj = Object::Array(vec![
            Object::Integer(0),
            Object::Real(1.5),
            Object::Integer(200),
            Object::Integer(300),
        ]);
        assert_eq!(
            object_to_pagebox(&obj),
            Some(PageBox::new(0.0, 1.5, 200.0, 300.0))
        );
    }

    #[test]
    fn object_to_pagebox_normalizes_corner_order() {
        // [urx ury llx lly] style input -> normalized so llx<=urx, lly<=ury.
        let obj = Object::Array(vec![
            Object::Integer(200),
            Object::Integer(300),
            Object::Integer(0),
            Object::Integer(0),
        ]);
        assert_eq!(
            object_to_pagebox(&obj),
            Some(PageBox::new(0.0, 0.0, 200.0, 300.0))
        );
    }

    #[test]
    fn object_to_pagebox_rejects_wrong_arity() {
        assert_eq!(
            object_to_pagebox(&Object::Array(vec![Object::Integer(0)])),
            None
        );
        assert_eq!(object_to_pagebox(&Object::Integer(0)), None);
    }

    // -----------------------------------------------------------------------
    // Content wrapper builder (flpdf-9hc.9.9)
    // -----------------------------------------------------------------------

    #[test]
    fn wrap_content_has_safe_seams_and_cm() {
        let m = [0.0, -1.0, 1.0, 0.0, 0.0, 200.0];
        let inner = b"BT /F1 12 Tf (hi) Tj ET";
        let out = wrap_content_with_matrix(m, inner);
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("q\n0 -1 1 0 0 200 cm\n"), "prefix: {s:?}");
        assert!(s.contains("BT /F1 12 Tf (hi) Tj ET"), "{s:?}");
        // Suffix Q is separated from the preceding token by whitespace (no ET+Q merge).
        assert!(s.ends_with("ET\nQ\n"), "suffix: {s:?}");
    }

    #[test]
    fn format_number_drops_trailing_zeros() {
        assert_eq!(format_matrix_number(200.0), "200");
        assert_eq!(format_matrix_number(-1.0), "-1");
        assert_eq!(format_matrix_number(1.5), "1.5");
        assert_eq!(format_matrix_number(0.0), "0");
    }

    // -----------------------------------------------------------------------
    // Pure function tests: normalize_rotate
    // -----------------------------------------------------------------------

    #[test]
    fn normalize_standard_values() {
        assert_eq!(normalize_rotate(0), 0);
        assert_eq!(normalize_rotate(90), 90);
        assert_eq!(normalize_rotate(180), 180);
        assert_eq!(normalize_rotate(270), 270);
    }

    #[test]
    fn normalize_wraparound() {
        assert_eq!(normalize_rotate(360), 0);
        assert_eq!(normalize_rotate(450), 90);
        assert_eq!(normalize_rotate(540), 180);
        assert_eq!(normalize_rotate(720), 0);
    }

    #[test]
    fn normalize_negative() {
        assert_eq!(normalize_rotate(-90), 270);
        assert_eq!(normalize_rotate(-180), 180);
        assert_eq!(normalize_rotate(-270), 90);
        assert_eq!(normalize_rotate(-360), 0);
        assert_eq!(normalize_rotate(-450), 270);
    }

    #[test]
    fn normalize_non_multiple_of_90_rounds_to_nearest() {
        // 44 → closest multiple is 0 (44 < 45)
        assert_eq!(normalize_rotate(44), 0);
        // 45 → rounds up to 90
        assert_eq!(normalize_rotate(45), 90);
        // 89 → rounds up to 90
        assert_eq!(normalize_rotate(89), 90);
        // 91 → rounds down to 90
        assert_eq!(normalize_rotate(91), 90);
        // 134 → rounds down to 90 (134 - 90 = 44 < 45)
        assert_eq!(normalize_rotate(134), 90);
        // 135 → rounds up to 180
        assert_eq!(normalize_rotate(135), 180);
    }

    #[test]
    fn normalize_extreme_i32_inputs_do_not_overflow() {
        // `deg + 45` must not overflow i32 near the bounds; widening to i64
        // keeps these well-defined instead of panicking (debug) / wrapping.
        let max = normalize_rotate(i32::MAX);
        let min = normalize_rotate(i32::MIN);
        assert!(matches!(max, 0 | 90 | 180 | 270));
        assert!(matches!(min, 0 | 90 | 180 | 270));
    }

    // -----------------------------------------------------------------------
    // Pure function tests: compose_rotate
    // -----------------------------------------------------------------------

    #[test]
    fn compose_assign_overwrites_existing() {
        let op = RotateOp {
            mode: RotateMode::Assign,
            degrees: 90,
        };
        assert_eq!(compose_rotate(270, &op), 90);
        assert_eq!(compose_rotate(0, &op), 90);
        assert_eq!(compose_rotate(90, &op), 90);
    }

    #[test]
    fn compose_add_accumulates() {
        let op = RotateOp {
            mode: RotateMode::Add,
            degrees: 90,
        };
        assert_eq!(compose_rotate(0, &op), 90);
        assert_eq!(compose_rotate(90, &op), 180);
        assert_eq!(compose_rotate(270, &op), 0); // wrap-around
    }

    #[test]
    fn compose_add_negative() {
        let op = RotateOp {
            mode: RotateMode::Add,
            degrees: -90,
        };
        assert_eq!(compose_rotate(0, &op), 270);
        assert_eq!(compose_rotate(90, &op), 0);
    }

    #[test]
    fn compose_add_large() {
        let op = RotateOp {
            mode: RotateMode::Add,
            degrees: 450,
        };
        // 90 + 450 = 540 → normalize → 180
        assert_eq!(compose_rotate(90, &op), 180);
    }

    #[test]
    fn compose_assign_normalizes() {
        let op = RotateOp {
            mode: RotateMode::Assign,
            degrees: 450,
        };
        assert_eq!(compose_rotate(0, &op), 90);
    }

    #[test]
    fn compose_add_extreme_degrees_do_not_overflow() {
        // `existing + op.degrees` is widened to i64 before normalization, so
        // an i32::MAX additive angle no longer panics (debug) / wraps (release).
        let op = RotateOp {
            mode: RotateMode::Add,
            degrees: i32::MAX,
        };
        assert!(matches!(compose_rotate(270, &op), 0 | 90 | 180 | 270));
        let op = RotateOp {
            mode: RotateMode::Add,
            degrees: i32::MIN,
        };
        assert!(matches!(compose_rotate(90, &op), 0 | 90 | 180 | 270));
    }

    // -----------------------------------------------------------------------
    // PDF builder helpers (shared with several tests below)
    // -----------------------------------------------------------------------

    /// Build a minimal PDF with one page.  `page_rotate` is inserted into the page
    /// dict if `Some`; otherwise no `/Rotate` key is present.  `parent_rotate` is
    /// inserted into the parent `/Pages` node.
    ///
    /// Object layout:
    ///   1 0 R  Catalog
    ///   2 0 R  Pages  (optionally has /Rotate = parent_rotate)
    ///   3 0 R  Page   (optionally has /Rotate = page_rotate)
    fn build_single_page_pdf(page_rotate: Option<i32>, parent_rotate: Option<i32>) -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        let pages_str = if let Some(r) = parent_rotate {
            format!("2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 /Rotate {r} >>\nendobj\n")
        } else {
            "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_string()
        };
        pdf.extend_from_slice(pages_str.as_bytes());

        let off3 = pdf.len() as u64;
        let page_str = if let Some(r) = page_rotate {
            format!(
                "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Rotate {r} >>\nendobj\n"
            )
        } else {
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n".to_string()
        };
        pdf.extend_from_slice(page_str.as_bytes());

        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 4\n0000000000 65535 f \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \n",
            off1, off2, off3,
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer =
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    // -----------------------------------------------------------------------
    // resolve_inherited_rotate tests
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_page_has_direct_rotate() {
        let bytes = build_single_page_pdf(Some(90), None);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);
        assert_eq!(resolve_inherited_rotate(&mut pdf, page_ref).unwrap(), 90);
    }

    #[test]
    fn resolve_inherits_from_parent() {
        // Page has no /Rotate, parent /Pages has /Rotate 180.
        let bytes = build_single_page_pdf(None, Some(180));
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);
        assert_eq!(resolve_inherited_rotate(&mut pdf, page_ref).unwrap(), 180);
    }

    #[test]
    fn resolve_defaults_to_zero_when_absent() {
        let bytes = build_single_page_pdf(None, None);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);
        assert_eq!(resolve_inherited_rotate(&mut pdf, page_ref).unwrap(), 0);
    }

    #[test]
    fn resolve_normalizes_non_standard_value() {
        // /Rotate 45 on page — invalid per spec, but we normalize.
        let bytes = build_single_page_pdf(Some(45), None);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);
        assert_eq!(resolve_inherited_rotate(&mut pdf, page_ref).unwrap(), 90);
    }

    // -----------------------------------------------------------------------
    // apply_rotate_to_pages tests
    // -----------------------------------------------------------------------

    #[test]
    fn assign_replaces_existing_rotate() {
        let bytes = build_single_page_pdf(Some(90), None);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let op = RotateOp {
            mode: RotateMode::Assign,
            degrees: 180,
        };
        apply_rotate_to_pages(&mut pdf, &[page_ref], &op).unwrap();

        // The leaf should now carry /Rotate 180 explicitly.
        let obj = pdf.resolve_borrowed(page_ref).unwrap();
        let Object::Dictionary(dict) = obj else {
            panic!("not a dict")
        };
        assert_eq!(dict.get("Rotate"), Some(&Object::Integer(180)));
    }

    #[test]
    fn add_accumulates_onto_direct_rotate() {
        let bytes = build_single_page_pdf(Some(90), None);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let op = RotateOp {
            mode: RotateMode::Add,
            degrees: 90,
        };
        apply_rotate_to_pages(&mut pdf, &[page_ref], &op).unwrap();

        let obj = pdf.resolve_borrowed(page_ref).unwrap();
        let Object::Dictionary(dict) = obj else {
            panic!("not a dict")
        };
        assert_eq!(dict.get("Rotate"), Some(&Object::Integer(180)));
    }

    #[test]
    fn add_wraps_at_360() {
        let bytes = build_single_page_pdf(Some(270), None);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let op = RotateOp {
            mode: RotateMode::Add,
            degrees: 90,
        };
        apply_rotate_to_pages(&mut pdf, &[page_ref], &op).unwrap();

        let obj = pdf.resolve_borrowed(page_ref).unwrap();
        let Object::Dictionary(dict) = obj else {
            panic!("not a dict")
        };
        assert_eq!(dict.get("Rotate"), Some(&Object::Integer(0)));
    }

    #[test]
    fn inherited_rotate_is_materialized_on_leaf() {
        // Page has no /Rotate, parent has /Rotate 90.
        // After Assign 180, the leaf must carry /Rotate 180 explicitly.
        let bytes = build_single_page_pdf(None, Some(90));
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let op = RotateOp {
            mode: RotateMode::Assign,
            degrees: 180,
        };
        apply_rotate_to_pages(&mut pdf, &[page_ref], &op).unwrap();

        let obj = pdf.resolve_borrowed(page_ref).unwrap();
        let Object::Dictionary(dict) = obj else {
            panic!("not a dict")
        };
        // The leaf itself must now carry /Rotate explicitly.
        assert_eq!(dict.get("Rotate"), Some(&Object::Integer(180)));
    }

    #[test]
    fn add_with_inherited_rotate_materializes_combined() {
        // Page has no /Rotate, parent has /Rotate 90.
        // Add 90 → expected 180 materialized on leaf.
        let bytes = build_single_page_pdf(None, Some(90));
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let op = RotateOp {
            mode: RotateMode::Add,
            degrees: 90,
        };
        apply_rotate_to_pages(&mut pdf, &[page_ref], &op).unwrap();

        let obj = pdf.resolve_borrowed(page_ref).unwrap();
        let Object::Dictionary(dict) = obj else {
            panic!("not a dict")
        };
        assert_eq!(dict.get("Rotate"), Some(&Object::Integer(180)));
    }

    #[test]
    fn assign_zero_materializes_zero_explicitly() {
        // Even Assign 0 must write /Rotate 0 on the leaf, not leave it absent.
        let bytes = build_single_page_pdf(None, None);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let op = RotateOp {
            mode: RotateMode::Assign,
            degrees: 0,
        };
        apply_rotate_to_pages(&mut pdf, &[page_ref], &op).unwrap();

        let obj = pdf.resolve_borrowed(page_ref).unwrap();
        let Object::Dictionary(dict) = obj else {
            panic!("not a dict")
        };
        assert_eq!(dict.get("Rotate"), Some(&Object::Integer(0)));
    }

    #[test]
    fn apply_to_empty_slice_is_noop() {
        let bytes = build_single_page_pdf(Some(90), None);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let op = RotateOp {
            mode: RotateMode::Assign,
            degrees: 270,
        };
        apply_rotate_to_pages(&mut pdf, &[], &op).unwrap();

        // Page should still be 90.
        let obj = pdf.resolve_borrowed(ObjectRef::new(3, 0)).unwrap();
        let Object::Dictionary(dict) = obj else {
            panic!("not a dict")
        };
        assert_eq!(dict.get("Rotate"), Some(&Object::Integer(90)));
    }

    #[test]
    fn rejects_pages_tree_node_target() {
        // Passing the intermediate /Pages node (2 0 R) must error rather than
        // silently writing /Rotate onto it (which would change inherited
        // rotation for every descendant page).
        let bytes = build_single_page_pdf(None, None);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let pages_ref = ObjectRef::new(2, 0);

        let op = RotateOp {
            mode: RotateMode::Assign,
            degrees: 90,
        };
        let err = apply_rotate_to_pages(&mut pdf, &[pages_ref], &op).unwrap_err();
        assert!(
            matches!(err, Error::Unsupported(_)),
            "expected Unsupported for /Pages node, got {err:?}"
        );

        // The /Pages node must remain untouched (no /Rotate written).
        let obj = pdf.resolve_borrowed(pages_ref).unwrap();
        let Object::Dictionary(dict) = obj else {
            panic!("not a dict")
        };
        assert_eq!(
            dict.get("Rotate"),
            None,
            "/Pages node must not gain /Rotate"
        );
    }

    // -----------------------------------------------------------------------
    // Round-trip test: write PDF, re-open, verify leaf /Rotate is present.
    // -----------------------------------------------------------------------

    #[test]
    fn round_trip_rotate_preserved_after_write_reopen() {
        // Start with page /Rotate 90, assign 270.
        let bytes = build_single_page_pdf(Some(90), None);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let op = RotateOp {
            mode: RotateMode::Assign,
            degrees: 270,
        };
        apply_rotate_to_pages(&mut pdf, &[page_ref], &op).unwrap();

        // Serialize.
        let mut out: Vec<u8> = Vec::new();
        write_pdf(&mut pdf, &mut out).unwrap();

        // Re-open and verify.
        let mut pdf2 = Pdf::open(Cursor::new(out)).unwrap();
        let page_refs = pages::page_refs(&mut pdf2).unwrap();
        assert_eq!(page_refs.len(), 1);

        let obj2 = pdf2.resolve_borrowed(page_refs[0]).unwrap();
        let Object::Dictionary(dict2) = obj2 else {
            panic!("not a dict after round-trip")
        };
        // The leaf must carry /Rotate 270 explicitly (not inherited).
        assert_eq!(
            dict2.get("Rotate"),
            Some(&Object::Integer(270)),
            "expected /Rotate 270 explicitly on leaf after round-trip"
        );
    }

    #[test]
    fn round_trip_inherited_rotate_materialized_on_leaf() {
        // Page has no /Rotate, parent has /Rotate 180; Add 90 → leaf should be 270.
        let bytes = build_single_page_pdf(None, Some(180));
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_refs_before = pages::page_refs(&mut pdf).unwrap();
        let page_ref = page_refs_before[0];

        let op = RotateOp {
            mode: RotateMode::Add,
            degrees: 90,
        };
        apply_rotate_to_pages(&mut pdf, &[page_ref], &op).unwrap();

        // Serialize and re-open.
        let mut out: Vec<u8> = Vec::new();
        write_pdf(&mut pdf, &mut out).unwrap();
        let mut pdf2 = Pdf::open(Cursor::new(out)).unwrap();
        let page_refs2 = pages::page_refs(&mut pdf2).unwrap();

        let obj2 = pdf2.resolve_borrowed(page_refs2[0]).unwrap();
        let Object::Dictionary(dict2) = obj2 else {
            panic!("not a dict")
        };
        // Must be materialized on leaf, not inherited.
        assert_eq!(
            dict2.get("Rotate"),
            Some(&Object::Integer(270)),
            "expected inherited+add materialized on leaf"
        );
    }

    // -----------------------------------------------------------------------
    // Multi-page test: each leaf is updated independently.
    // -----------------------------------------------------------------------

    /// Build a PDF with two pages that each have their own /Rotate value.
    ///
    /// Object layout:
    ///   1 0 R  Catalog
    ///   2 0 R  Pages  (/Kids [3 0 R 4 0 R])
    ///   3 0 R  Page   (/Rotate = page1_rotate if Some)
    ///   4 0 R  Page   (/Rotate = page2_rotate if Some)
    fn build_two_page_pdf(page1_rotate: Option<i32>, page2_rotate: Option<i32>) -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        let page1_str = if let Some(r) = page1_rotate {
            format!(
                "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Rotate {r} >>\nendobj\n"
            )
        } else {
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n".to_string()
        };
        pdf.extend_from_slice(page1_str.as_bytes());

        let off4 = pdf.len() as u64;
        let page2_str = if let Some(r) = page2_rotate {
            format!(
                "4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Rotate {r} >>\nendobj\n"
            )
        } else {
            "4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n".to_string()
        };
        pdf.extend_from_slice(page2_str.as_bytes());

        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 5\n0000000000 65535 f \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \n",
            off1, off2, off3, off4,
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer =
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    #[test]
    fn apply_to_multiple_pages_each_updated_independently() {
        // Page 1: /Rotate 90; Page 2: /Rotate 0.  Add 90 to both.
        // Expected: page 1 → 180, page 2 → 90.
        let bytes = build_two_page_pdf(Some(90), Some(0));
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let page1 = ObjectRef::new(3, 0);
        let page2 = ObjectRef::new(4, 0);

        let op = RotateOp {
            mode: RotateMode::Add,
            degrees: 90,
        };
        apply_rotate_to_pages(&mut pdf, &[page1, page2], &op).unwrap();

        let obj1 = pdf.resolve_borrowed(page1).unwrap();
        let Object::Dictionary(dict1) = obj1 else {
            panic!("not a dict")
        };
        assert_eq!(dict1.get("Rotate"), Some(&Object::Integer(180)), "page 1");

        let obj2 = pdf.resolve_borrowed(page2).unwrap();
        let Object::Dictionary(dict2) = obj2 else {
            panic!("not a dict")
        };
        assert_eq!(dict2.get("Rotate"), Some(&Object::Integer(90)), "page 2");
    }

    // -----------------------------------------------------------------------
    // flatten_rotation_on_pages (flpdf-9hc.9.9)
    // -----------------------------------------------------------------------

    /// Assemble a minimal PDF from `(number, body)` objects numbered 1..=N in
    /// order. `body` excludes the `N 0 obj` / `endobj` wrapper.
    fn assemble_pdf(objs: &[(u32, String)]) -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let mut offsets = Vec::with_capacity(objs.len());
        for (num, body) in objs {
            offsets.push(pdf.len() as u64);
            pdf.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
            pdf.extend_from_slice(body.as_bytes());
            pdf.extend_from_slice(b"\nendobj\n");
        }
        let n = objs.len() as u32 + 1;
        let xref_start = pdf.len() as u64;
        let mut xref = format!("xref\n0 {n}\n0000000000 65535 f \n");
        for off in &offsets {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        pdf.extend_from_slice(xref.as_bytes());
        let trailer =
            format!("trailer\n<< /Size {n} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    fn content_obj_body(content: &str) -> String {
        format!(
            "<< /Length {} >>\nstream\n{}\nendstream",
            content.len(),
            content
        )
    }

    /// 1-page PDF: MediaBox `mb`, optional leaf `/Rotate`, content stream `content`.
    fn build_single_page_with_content(mb: &str, rotate: Option<i32>, content: &str) -> Vec<u8> {
        build_single_page_with_extra(mb, rotate, content, "")
    }

    /// As [`build_single_page_with_content`] but with `extra` raw dict entries
    /// (e.g. `"/CropBox [10 10 190 290]"`) spliced into the leaf page dictionary.
    fn build_single_page_with_extra(
        mb: &str,
        rotate: Option<i32>,
        content: &str,
        extra: &str,
    ) -> Vec<u8> {
        let rotate_entry = match rotate {
            Some(r) => format!(" /Rotate {r}"),
            None => String::new(),
        };
        let page = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox {mb} /Contents 4 0 R{rotate_entry} {extra} >>"
        );
        assemble_pdf(&[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()),
            (3, page),
            (4, content_obj_body(content)),
        ])
    }

    /// 1-page PDF with `/Rotate` only on the `/Pages` root (inherited by the leaf).
    fn build_single_page_inherited_rotate(parent_rotate: i32) -> Vec<u8> {
        assemble_pdf(&[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
            (
                2,
                format!("<< /Type /Pages /Kids [3 0 R] /Count 1 /Rotate {parent_rotate} >>"),
            ),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] /Contents 4 0 R >>"
                    .to_string(),
            ),
            (4, content_obj_body("BT (x) Tj ET")),
        ])
    }

    #[test]
    fn flatten_90_swaps_mediabox_zeroes_rotate_and_wraps_content() {
        let bytes = build_single_page_with_content("[0 0 200 300]", Some(90), "BT (x) Tj ET");
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page = pages::page_refs(&mut pdf).unwrap()[0];
        flatten_rotation_on_pages(&mut pdf, &[page]).unwrap();

        let Object::Dictionary(d) = pdf.resolve(page).unwrap() else {
            panic!("not a dict")
        };
        assert_eq!(d.get("Rotate"), Some(&Object::Integer(0)));
        let mb = object_to_pagebox(d.get("MediaBox").unwrap()).unwrap();
        assert_eq!((mb.urx - mb.llx, mb.ury - mb.lly), (300.0, 200.0));

        let content = pages::page_content_bytes(&mut pdf, page).unwrap();
        let s = String::from_utf8(content).unwrap();
        assert!(s.starts_with("q\n0 -1 1 0 0 200 cm\n"), "{s:?}");
        assert!(s.contains("BT (x) Tj ET"), "{s:?}");

        // Round-trips through the writer + reparse without error.
        let mut buf = Vec::new();
        write_pdf(&mut pdf, &mut buf).unwrap();
        let mut pdf2 = Pdf::open(Cursor::new(buf)).unwrap();
        let p2 = pages::page_refs(&mut pdf2).unwrap()[0];
        let Object::Dictionary(d2) = pdf2.resolve(p2).unwrap() else {
            panic!("not a dict")
        };
        assert_eq!(d2.get("Rotate"), Some(&Object::Integer(0)));
    }

    #[test]
    fn flatten_is_noop_when_rotate_zero() {
        let bytes = build_single_page_with_content("[0 0 200 300]", None, "BT (x) Tj ET");
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page = pages::page_refs(&mut pdf).unwrap()[0];
        let before = pages::page_content_bytes(&mut pdf, page).unwrap();
        flatten_rotation_on_pages(&mut pdf, &[page]).unwrap();
        let after = pages::page_content_bytes(&mut pdf, page).unwrap();
        assert_eq!(before, after, "content must be untouched when rotate==0");
        let Object::Dictionary(d) = pdf.resolve(page).unwrap() else {
            panic!("not a dict")
        };
        let mb = object_to_pagebox(d.get("MediaBox").unwrap()).unwrap();
        assert_eq!((mb.urx, mb.ury), (200.0, 300.0));
    }

    #[test]
    fn flatten_180_keeps_mediabox_dims_and_zeroes_rotate() {
        let bytes = build_single_page_with_content("[0 0 200 300]", Some(180), "BT (x) Tj ET");
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page = pages::page_refs(&mut pdf).unwrap()[0];
        flatten_rotation_on_pages(&mut pdf, &[page]).unwrap();

        let Object::Dictionary(d) = pdf.resolve(page).unwrap() else {
            panic!("not a dict")
        };
        assert_eq!(d.get("Rotate"), Some(&Object::Integer(0)));
        // 180 maps [0 0 200 300] back onto itself: dims unchanged.
        let mb = object_to_pagebox(d.get("MediaBox").unwrap()).unwrap();
        assert_eq!((mb.llx, mb.lly, mb.urx, mb.ury), (0.0, 0.0, 200.0, 300.0));
        let content = pages::page_content_bytes(&mut pdf, page).unwrap();
        let s = String::from_utf8(content).unwrap();
        assert!(s.starts_with("q\n-1 0 0 -1 200 300 cm\n"), "{s:?}");
    }

    #[test]
    fn flatten_transforms_cropbox_present_on_leaf() {
        // CropBox differs from MediaBox and must be transformed by the same matrix.
        let bytes = build_single_page_with_extra(
            "[0 0 200 300]",
            Some(90),
            "BT (x) Tj ET",
            "/CropBox [10 10 190 290]",
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page = pages::page_refs(&mut pdf).unwrap()[0];
        flatten_rotation_on_pages(&mut pdf, &[page]).unwrap();

        let Object::Dictionary(d) = pdf.resolve(page).unwrap() else {
            panic!("not a dict")
        };
        // 90deg map (x,y)->(y, 200 - x): corners (10,10),(190,290) ->
        // (10,190),(290,10) -> bbox [10 10 290 190].
        let cb = object_to_pagebox(d.get("CropBox").unwrap()).unwrap();
        assert_eq!((cb.llx, cb.lly, cb.urx, cb.ury), (10.0, 10.0, 290.0, 190.0));
        // And MediaBox is still swapped, independently.
        let mb = object_to_pagebox(d.get("MediaBox").unwrap()).unwrap();
        assert_eq!((mb.urx - mb.llx, mb.ury - mb.lly), (300.0, 200.0));
    }

    /// 1-page PDF with one annotation (`/Rect rect`) on a rotated page.
    fn build_single_page_with_annot(mb: &str, rotate: i32, rect: &str) -> Vec<u8> {
        let page = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox {mb} /Contents 4 0 R /Rotate {rotate} /Annots [5 0 R] >>"
        );
        assemble_pdf(&[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()),
            (3, page),
            (4, content_obj_body("BT (x) Tj ET")),
            (5, format!("<< /Type /Annot /Subtype /Text /Rect {rect} >>")),
        ])
    }

    #[test]
    fn flatten_transforms_annotation_rect() {
        let bytes = build_single_page_with_annot("[0 0 200 300]", 90, "[10 20 60 40]");
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page = pages::page_refs(&mut pdf).unwrap()[0];
        let annot = ObjectRef::new(5, 0);
        flatten_rotation_on_pages(&mut pdf, &[page]).unwrap();

        // 90deg map (x,y)->(y, 200 - x): corners (10,20),(60,40) ->
        // (20,190),(40,140) -> bbox [20 140 40 190].
        let Object::Dictionary(ad) = pdf.resolve(annot).unwrap() else {
            panic!("not a dict")
        };
        let r = object_to_pagebox(ad.get("Rect").unwrap()).unwrap();
        assert_eq!((r.llx, r.lly, r.urx, r.ury), (20.0, 140.0, 40.0, 190.0));
    }

    /// 1-page PDF whose `/Annots` is an *indirect reference* to the array (obj 6),
    /// not a direct array. Exercises the reference-resolution path.
    fn build_single_page_indirect_annots(mb: &str, rotate: i32, rect: &str) -> Vec<u8> {
        let page = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox {mb} /Contents 4 0 R /Rotate {rotate} /Annots 6 0 R >>"
        );
        assemble_pdf(&[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()),
            (3, page),
            (4, content_obj_body("BT (x) Tj ET")),
            (5, format!("<< /Type /Annot /Subtype /Text /Rect {rect} >>")),
            (6, "[5 0 R]".to_string()),
        ])
    }

    #[test]
    fn flatten_transforms_annotation_rect_via_indirect_annots() {
        let bytes = build_single_page_indirect_annots("[0 0 200 300]", 90, "[10 20 60 40]");
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page = pages::page_refs(&mut pdf).unwrap()[0];
        let annot = ObjectRef::new(5, 0);
        flatten_rotation_on_pages(&mut pdf, &[page]).unwrap();

        // Same mapping as the direct-array case: [10 20 60 40] -> [20 140 40 190].
        let Object::Dictionary(ad) = pdf.resolve(annot).unwrap() else {
            panic!("not a dict")
        };
        let r = object_to_pagebox(ad.get("Rect").unwrap()).unwrap();
        assert_eq!((r.llx, r.lly, r.urx, r.ury), (20.0, 140.0, 40.0, 190.0));
    }

    #[test]
    fn flatten_rejects_non_leaf_target_even_when_rotate_zero() {
        // obj 2 is the /Pages tree node (not a leaf /Page); its effective /Rotate
        // is 0. The leaf guard must still reject it instead of silently passing.
        let bytes = build_single_page_with_content("[0 0 200 300]", None, "BT (x) Tj ET");
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let pages_node = ObjectRef::new(2, 0);
        let err = flatten_rotation_on_pages(&mut pdf, &[pages_node]).unwrap_err();
        assert!(
            matches!(err, Error::Unsupported(_)),
            "expected Unsupported, got {err:?}"
        );
    }

    #[test]
    fn flatten_materializes_inherited_rotate() {
        let bytes = build_single_page_inherited_rotate(270);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page = pages::page_refs(&mut pdf).unwrap()[0];
        flatten_rotation_on_pages(&mut pdf, &[page]).unwrap();
        let Object::Dictionary(d) = pdf.resolve(page).unwrap() else {
            panic!("not a dict")
        };
        assert_eq!(d.get("Rotate"), Some(&Object::Integer(0)));
        // 270 on a [0 0 200 300] box swaps dims to 300x200.
        let mb = object_to_pagebox(d.get("MediaBox").unwrap()).unwrap();
        assert_eq!((mb.urx - mb.llx, mb.ury - mb.lly), (300.0, 200.0));
    }
}
