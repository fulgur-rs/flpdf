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

use crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH;
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
/// 2. Integer-divide by 90 (Euclidean, so the quotient is non-negative even for
///    negative inputs) to obtain the nearest multiple index.
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
    // Round `deg` to the nearest 90° boundary, then keep within [0, 360).
    // `div_euclid` gives a non-negative quotient even for negative `deg`;
    // `rem_euclid` ensures the final result is non-negative even when
    // `(deg + 45).div_euclid(90) * 90` is negative (e.g. -45 → -1*90 = -90).
    let snapped = (deg + 45).div_euclid(90) * 90;
    snapped.rem_euclid(360)
}

/// Compute the final `/Rotate` value for a page given `existing` (the resolved,
/// inherited current value) and `op`.
///
/// The returned value is always normalized to `{0, 90, 180, 270}`.
pub fn compose_rotate(existing: i32, op: &RotateOp) -> i32 {
    let raw = match op.mode {
        RotateMode::Assign => op.degrees,
        RotateMode::Add => existing + op.degrees,
    };
    normalize_rotate(raw)
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
pub fn resolve_inherited_rotate<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<i32> {
    resolve_inherited_rotate_with_max_depth(pdf, page_ref, DEFAULT_MAX_PAGE_TREE_DEPTH)
}

/// Like [`resolve_inherited_rotate`] but with a caller-supplied recursion limit.
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

        let node_obj = pdf.resolve(current)?;
        let Object::Dictionary(dict) = node_obj else {
            // Not a dictionary — cannot walk further; use default.
            return Ok(0);
        };

        // Check for /Rotate on this node.
        // Per ISO 32000-1 §7.3.9, a null value is equivalent to absent.
        if let Some(rotate_val) = dict.get("Rotate").cloned() {
            match rotate_val {
                // null → treat as absent; continue walking.
                Object::Null => {}
                Object::Integer(n) => return Ok(normalize_rotate(n as i32)),
                Object::Reference(r) => {
                    let resolved = pdf.resolve(r)?;
                    match resolved {
                        Object::Null => {}
                        Object::Integer(n) => return Ok(normalize_rotate(n as i32)),
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
        let parent_val = match dict.get("Parent").cloned() {
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
/// resolve to a dictionary, or if the page-tree depth limit is exceeded.
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

        // 4. Materialize the new /Rotate on the leaf.
        //    We always write it explicitly (even for 0) so the leaf is no longer
        //    dependent on any ancestor's /Rotate.
        page_dict.insert("Rotate", Object::Integer(new_rotate as i64));
        pdf.set_object(page_ref, Object::Dictionary(page_dict));
    }
    Ok(())
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
        let obj = pdf.resolve(page_ref).unwrap();
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

        let obj = pdf.resolve(page_ref).unwrap();
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

        let obj = pdf.resolve(page_ref).unwrap();
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

        let obj = pdf.resolve(page_ref).unwrap();
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

        let obj = pdf.resolve(page_ref).unwrap();
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

        let obj = pdf.resolve(page_ref).unwrap();
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
        let obj = pdf.resolve(ObjectRef::new(3, 0)).unwrap();
        let Object::Dictionary(dict) = obj else {
            panic!("not a dict")
        };
        assert_eq!(dict.get("Rotate"), Some(&Object::Integer(90)));
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

        let obj2 = pdf2.resolve(page_refs[0]).unwrap();
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

        let obj2 = pdf2.resolve(page_refs2[0]).unwrap();
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

        let obj1 = pdf.resolve(page1).unwrap();
        let Object::Dictionary(dict1) = obj1 else {
            panic!("not a dict")
        };
        assert_eq!(dict1.get("Rotate"), Some(&Object::Integer(180)), "page 1");

        let obj2 = pdf.resolve(page2).unwrap();
        let Object::Dictionary(dict2) = obj2 else {
            panic!("not a dict")
        };
        assert_eq!(dict2.get("Rotate"), Some(&Object::Integer(90)), "page 2");
    }
}
