//! qpdf correspondence: QPDFJob.cc page rotation plus QPDFPageObjectHelper.cc matrix responsibilities.
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
//! [`flatten_rotation_on_pages`] delegates to the live
//! [`PageObjectHelper::flatten_rotation`] facade. That facade follows qpdf's
//! direct-key semantics, prepends the affine matrix to page contents, remaps
//! every direct page box, and delegates annotation/field/AP transformation to
//! [`crate::AcroFormDocumentHelper`].

use crate::page_object_helper::PageObjectHelper;
use crate::pages::{
    next_page_parent, page_parent_entries, PageParentCursor, DEFAULT_MAX_PAGE_TREE_DEPTH,
};
use crate::{Error, Object, ObjectRef, Pdf, Result};
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
    let mut current = PageParentCursor::Reference(page_ref);
    let mut depth: usize = 0;

    loop {
        if depth >= max_depth {
            return Err(Error::Unsupported(format!(
                "page tree depth exceeds maximum of {max_depth} at {current}"
            )));
        }

        // Cycle guard.
        if let PageParentCursor::Reference(reference) = &current {
            if !seen.insert(*reference) {
                // We hit a cycle before finding /Rotate — default to 0.
                return Ok(0);
            }
        }

        let Some((rotate_val, parent_val)) = page_parent_entries(pdf, &current, "Rotate")? else {
            // Not a dictionary — cannot walk further; use default.
            return Ok(0);
        };

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

        let Some(parent) = next_page_parent(parent_val) else {
            return Ok(0);
        };
        current = parent;
        depth += 1;
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
// Public API: flatten_rotation_on_pages
// ---------------------------------------------------------------------------

/// Apply qpdf's `QPDFPageObjectHelper::flattenRotation` to each selected page.
/// The page-level facade owns the direct `/Rotate` and box semantics; this
/// function only performs page selection and iteration.
///
/// # Errors
/// Propagates the facade's validation, warning, and resolver errors.
pub fn flatten_rotation_on_pages<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    pages: &[ObjectRef],
) -> Result<()> {
    for &page_ref in pages {
        let mut page = PageObjectHelper::new(page_ref, pdf);
        page.flatten_rotation()?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::writer::write_qpdf_to_memory;
    use crate::{pages, PageBox, Pdf};
    use std::io::Cursor;

    fn object_to_pagebox(obj: &Object) -> Option<PageBox> {
        let Object::Array(values) = obj else {
            return None;
        };
        if values.len() != 4 {
            return None;
        }
        let mut numbers = [0.0; 4];
        for (index, value) in values.iter().enumerate() {
            numbers[index] = match value {
                Object::Integer(value) => *value as f64,
                Object::Real(value) => *value,
                Object::RealLiteral { value, .. } => *value,
                _ => return None,
            };
        }
        Some(PageBox::new(
            numbers[0].min(numbers[2]),
            numbers[1].min(numbers[3]),
            numbers[0].max(numbers[2]),
            numbers[1].max(numbers[3]),
        ))
    }

    #[test]
    fn object_to_pagebox_rejects_bad_shapes_and_accepts_real_literals() {
        assert!(object_to_pagebox(&Object::Integer(1)).is_none());
        assert!(object_to_pagebox(&Object::Array(vec![Object::Integer(1)])).is_none());
        assert_eq!(
            object_to_pagebox(&Object::Array(vec![
                Object::RealLiteral {
                    value: 1.5,
                    literal: b"1.5".to_vec(),
                },
                Object::Integer(2),
                Object::Real(11.5),
                Object::Integer(22),
            ])),
            Some(PageBox::new(1.5, 2.0, 11.5, 22.0))
        );
        assert!(object_to_pagebox(&Object::Array(vec![
            Object::Integer(1),
            Object::Null,
            Object::Integer(11),
            Object::Integer(22),
        ]))
        .is_none());
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
    fn resolve_defaults_to_zero_for_a_parent_cycle() {
        let bytes = build_single_page_pdf(None, None);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let mut parent = pdf
            .resolve(ObjectRef::new(2, 0))
            .unwrap()
            .into_dict()
            .expect("parent must be a dictionary");
        parent.insert("Parent", Object::Reference(ObjectRef::new(3, 0)));
        pdf.set_object(ObjectRef::new(2, 0), Object::Dictionary(parent));

        assert_eq!(
            resolve_inherited_rotate(&mut pdf, ObjectRef::new(3, 0)).unwrap(),
            0
        );
    }

    #[test]
    fn resolve_defaults_to_zero_for_a_non_dictionary_parent() {
        let bytes = build_single_page_pdf(None, None);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let mut page = pdf
            .resolve(ObjectRef::new(3, 0))
            .unwrap()
            .into_dict()
            .expect("page must be a dictionary");
        page.insert("Parent", Object::Integer(42));
        pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));

        assert_eq!(
            resolve_inherited_rotate(&mut pdf, ObjectRef::new(3, 0)).unwrap(),
            0
        );
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
        let out = write_qpdf_to_memory(&mut pdf, |_| {}).unwrap();

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
        let out = write_qpdf_to_memory(&mut pdf, |_| {}).unwrap();
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
        assert!(d.get("Rotate").is_none());
        let mb = object_to_pagebox(d.get("MediaBox").unwrap()).unwrap();
        assert_eq!((mb.urx - mb.llx, mb.ury - mb.lly), (300.0, 200.0));

        let content = pages::page_content_bytes(&mut pdf, page).unwrap();
        let s = String::from_utf8(content).unwrap();
        assert!(s.starts_with("q\n0 -1 1 0 0 200 cm\n"), "{s:?}");
        assert!(s.contains("BT (x) Tj ET"), "{s:?}");

        // Round-trips through the writer + reparse without error.
        let buf = write_qpdf_to_memory(&mut pdf, |_| {}).unwrap();
        let mut pdf2 = Pdf::open(Cursor::new(buf)).unwrap();
        let p2 = pages::page_refs(&mut pdf2).unwrap()[0];
        let Object::Dictionary(d2) = pdf2.resolve(p2).unwrap() else {
            panic!("not a dict")
        };
        assert!(d2.get("Rotate").is_none());
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
        assert!(d.get("Rotate").is_none());
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

    /// 1-page PDF with one direct annotation and one indirect annotation in
    /// the same `/Annots` array. qpdf's `getAnnotations` preserves both
    /// handles (`QPDFPageObjectHelper.cc:439-454`), so flattening must update
    /// both rectangles.
    fn build_single_page_with_direct_and_indirect_annots() -> Vec<u8> {
        assemble_pdf(&[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            ),
            (
                2,
                "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            ),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] /Contents 4 0 R /Rotate 90 /Annots [<< /Type /Annot /Subtype /Text /Rect [10 20 60 40] >> 5 0 R] >>".to_string(),
            ),
            (4, content_obj_body("BT (x) Tj ET")),
            (
                5,
                "<< /Type /Annot /Subtype /Text /Rect [20 30 70 50] >>".to_string(),
            ),
        ])
    }

    #[test]
    fn flatten_transforms_annotation_rect() {
        let bytes = build_single_page_with_annot("[0 0 200 300]", 90, "[10 20 60 40]");
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page = pages::page_refs(&mut pdf).unwrap()[0];
        flatten_rotation_on_pages(&mut pdf, &[page]).unwrap();

        // 90deg map (x,y)->(y, 200 - x): corners (10,20),(60,40) ->
        // (20,190),(40,140) -> bbox [20 140 40 190].
        let page_dict = pdf.resolve(page).unwrap().into_dict().unwrap();
        let annot = page_dict
            .get("Annots")
            .and_then(Object::as_array)
            .and_then(|annots| annots.first())
            .and_then(Object::as_ref_id)
            .expect("flattened annotation must remain on the page");
        let Object::Dictionary(ad) = pdf.resolve(annot).unwrap() else {
            panic!("not a dict")
        };
        let r = object_to_pagebox(ad.get("Rect").unwrap()).unwrap();
        assert_eq!((r.llx, r.lly, r.urx, r.ury), (20.0, 140.0, 40.0, 190.0));
    }

    #[test]
    fn flatten_transforms_direct_and_indirect_annotation_rects() {
        let bytes = build_single_page_with_direct_and_indirect_annots();
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page = pages::page_refs(&mut pdf).unwrap()[0];
        let indirect_annot = ObjectRef::new(5, 0);
        flatten_rotation_on_pages(&mut pdf, &[page]).unwrap();

        let page_dict = pdf
            .resolve(page)
            .unwrap()
            .into_dict()
            .expect("not a page dict");
        let annots = page_dict
            .get("Annots")
            .and_then(Object::as_array)
            .expect("/Annots must remain an array");
        let mut rects = annots
            .iter()
            .map(|annotation| {
                let annotation = match annotation {
                    Object::Reference(reference) => pdf.resolve(*reference).unwrap(),
                    direct => direct.clone(), // cov:ignore: qpdf transformAnnotations materializes every transformed annotation as an indirect object; retain this malformed-fixture fallback.
                };
                let annotation = annotation.into_dict().expect("annotation dictionary");
                let rectangle = object_to_pagebox(annotation.get("Rect").unwrap()).unwrap();
                (rectangle.llx, rectangle.lly, rectangle.urx, rectangle.ury)
            })
            .collect::<Vec<_>>();
        rects.sort_by(|left, right| left.partial_cmp(right).unwrap());
        assert_eq!(
            rects,
            vec![(20.0, 140.0, 40.0, 190.0), (30.0, 130.0, 50.0, 180.0)]
        );
        let original = pdf.resolve(indirect_annot).unwrap().into_dict().unwrap();
        assert_eq!(
            object_to_pagebox(original.get("Rect").unwrap()),
            Some(PageBox::new(20.0, 30.0, 70.0, 50.0))
        );
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
        flatten_rotation_on_pages(&mut pdf, &[page]).unwrap();

        // Same mapping as the direct-array case: [10 20 60 40] -> [20 140 40 190].
        let page_dict = pdf.resolve(page).unwrap().into_dict().unwrap();
        let annot = page_dict
            .get("Annots")
            .and_then(Object::as_array)
            .and_then(|annots| annots.first())
            .and_then(Object::as_ref_id)
            .expect("flattened annotation must be indirect");
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
    fn flatten_does_not_materialize_inherited_rotate() {
        let bytes = build_single_page_inherited_rotate(270);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page = pages::page_refs(&mut pdf).unwrap()[0];
        let before = pages::page_content_bytes(&mut pdf, page).unwrap();
        flatten_rotation_on_pages(&mut pdf, &[page]).unwrap();
        let Object::Dictionary(d) = pdf.resolve(page).unwrap() else {
            panic!("not a dict")
        };
        assert!(d.get("Rotate").is_none());
        assert_eq!(pages::page_content_bytes(&mut pdf, page).unwrap(), before);
        // The direct page box is untouched because qpdf's facade did not see a
        // direct `/Rotate` value.
        let mb = object_to_pagebox(d.get("MediaBox").unwrap()).unwrap();
        assert_eq!((mb.urx - mb.llx, mb.ury - mb.lly), (200.0, 300.0));
    }
}
