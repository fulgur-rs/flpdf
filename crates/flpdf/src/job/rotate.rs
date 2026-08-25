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
#[cfg(test)]
use crate::page_object_helper::{
    resolve_inherited_rotate, resolve_inherited_rotate_with_max_depth,
};
#[cfg(test)]
use crate::Object;
use crate::{Error, ObjectRef, Pdf, Result};
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
/// `degrees` must be a multiple of 90, matching qpdf's own `--rotate`
/// parsing; [`apply_rotate_to_pages`] rejects any other value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RotateOp {
    /// Whether this is an assignment or an additive rotation.
    pub mode: RotateMode,
    /// Angle in degrees, a multiple of 90 (positive = clockwise per PDF
    /// convention). May be negative or exceed 360; the final `/Rotate` is
    /// normalized to one of `{0, 90, 180, 270}`.
    pub degrees: i32,
}

// ---------------------------------------------------------------------------
// Main mutating entry point
// ---------------------------------------------------------------------------

/// Apply `op` to each `ObjectRef` in `pages`, materializing the resulting
/// `/Rotate` explicitly on every leaf page dictionary.
///
/// Ports `QPDFPageObjectHelper::rotatePage`/`QPDFObjectHandle::rotatePage`
/// (`libqpdf/QPDFPageObjectHelper.cc:468-470`,
/// `libqpdf/QPDFObjectHandle.cc:1517-1546`): for an additive rotation, the
/// existing `/Rotate` is read by walking `/Parent` on the live handle (an
/// existing value that is not itself a multiple of 90 is treated as `0`,
/// matching qpdf), and the combined angle is written directly on the leaf,
/// so the leaf no longer depends on the parent's value. This walk has no
/// depth bound — only cycle detection — matching qpdf's own unbounded
/// `/Parent` walk in `rotatePage`.
///
/// # Errors
///
/// Returns [`Error::Unsupported`] if any of the supplied `ObjectRef`s does not
/// resolve to a dictionary, or does not resolve to a leaf `/Page` object (e.g.
/// it points at a `/Pages` tree node). Returns [`Error::System`] if
/// `op.degrees` is not a multiple of 90.
pub fn apply_rotate_to_pages<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    pages: &[ObjectRef],
    op: &RotateOp,
) -> Result<()> {
    for &page_ref in pages {
        // qpdf's QPDFPageObjectHelper::rotatePage operates on the live handle,
        // not a materialized Object snapshot. Resolve and validate the page
        // through the same canonical handle before mutating it.
        let page = pdf.get_object_handle(page_ref);
        pdf.resolve(&page)?;
        if page.as_dictionary().is_none() {
            return Err(Error::Unsupported(format!(
                "object {page_ref} is not a dictionary, cannot set /Rotate"
            )));
        }

        // Guard: only leaf `/Page` objects are valid targets. Writing /Rotate
        // onto a `/Pages` tree node (or any non-Page dict) would change the
        // inherited rotation of every descendant page, violating the
        // per-leaf-page contract.
        let page_type = page.try_get_key(b"/Type")?;
        match page_type.try_as_name()? {
            Some(name) if name.as_slice() == b"Page" => {}
            _ => {
                return Err(Error::Unsupported(format!(
                    "object {page_ref} is not a leaf /Page (missing or non-/Page /Type), cannot set /Rotate"
                )));
            }
        }

        // `relative` selects qpdf's inherited-parent walk; both modes make
        // the result explicit even when it is zero.
        page.rotate_page(op.degrees, matches!(op.mode, RotateMode::Add))?;
        pdf.mark_object_handle_dirty(&page)?;
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
    use crate::{pages, ObjectHandle, PageBox, Pdf};
    use std::io::Cursor;

    fn handle_to_pagebox(pdf: &mut Pdf<Cursor<Vec<u8>>>, obj: &ObjectHandle) -> Option<PageBox> {
        pdf.resolve(obj).ok()?;
        let values = obj.as_array()?;
        if values.len() != 4 {
            return None;
        }
        let mut numbers = [0.0; 4];
        for (index, value) in values.iter().enumerate() {
            pdf.resolve(value).ok()?;
            numbers[index] = value
                .as_integer()
                .map(|value| value as f64)
                .or_else(|| value.as_real())?;
        }
        Some(PageBox::new(
            numbers[0].min(numbers[2]),
            numbers[1].min(numbers[3]),
            numbers[0].max(numbers[2]),
            numbers[1].max(numbers[3]),
        ))
    }

    fn object_key_handle(
        pdf: &mut Pdf<Cursor<Vec<u8>>>,
        object_ref: ObjectRef,
        key: &[u8],
    ) -> ObjectHandle {
        let owner = pdf.get_object_handle(object_ref);
        pdf.resolve(&owner).expect("object resolves");
        let value = owner.get_key(key);
        pdf.resolve(&value).expect("object key resolves");
        value
    }

    fn pagebox_for(pdf: &mut Pdf<Cursor<Vec<u8>>>, object_ref: ObjectRef, key: &[u8]) -> PageBox {
        let value = object_key_handle(pdf, object_ref, key);
        handle_to_pagebox(pdf, &value).expect("page box must be a four-number array")
    }

    fn rotate_value(pdf: &mut Pdf<Cursor<Vec<u8>>>, page_ref: ObjectRef) -> Option<i64> {
        let page = pdf.get_object_handle(page_ref);
        pdf.resolve(&page).expect("page resolves");
        let rotate = page.get_key(b"/Rotate");
        pdf.resolve(&rotate).expect("/Rotate resolves");
        rotate.as_integer()
    }

    #[test]
    fn handle_to_pagebox_rejects_bad_shapes_and_accepts_real_literals() {
        let mut pdf = Pdf::empty().unwrap();
        assert!(handle_to_pagebox(&mut pdf, &ObjectHandle::integer(1)).is_none());
        assert!(handle_to_pagebox(
            &mut pdf,
            &ObjectHandle::array(vec![ObjectHandle::integer(1)])
        )
        .is_none());
        assert_eq!(
            handle_to_pagebox(
                &mut pdf,
                &ObjectHandle::array(vec![
                    ObjectHandle::real_literal(1.5, b"1.5".to_vec()),
                    ObjectHandle::integer(2),
                    ObjectHandle::real(11.5),
                    ObjectHandle::integer(22),
                ])
            ),
            Some(PageBox::new(1.5, 2.0, 11.5, 22.0))
        );
        assert!(handle_to_pagebox(
            &mut pdf,
            &ObjectHandle::array(vec![
                ObjectHandle::integer(1),
                ObjectHandle::null(),
                ObjectHandle::integer(11),
                ObjectHandle::integer(22),
            ])
        )
        .is_none());
    }

    #[test]
    fn handle_to_pagebox_resolves_indirect_numeric_items() {
        let bytes = assemble_pdf(&[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".to_owned()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [1 4 0 R 3 4] >>".to_owned(),
            ),
            (4, "7".to_owned()),
        ]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page = pages::page_refs(&mut pdf).unwrap()[0];
        let media_box = object_key_handle(&mut pdf, page, b"/MediaBox");

        assert_eq!(
            handle_to_pagebox(&mut pdf, &media_box),
            Some(PageBox::new(1.0, 4.0, 3.0, 7.0))
        );
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
            .resolve_object(ObjectRef::new(2, 0))
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
            .resolve_object(ObjectRef::new(3, 0))
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
    fn resolve_preserves_non_standard_value() {
        // The getter observes the effective page attribute; only a relative
        // rotate operation treats an invalid existing value as zero.
        let bytes = build_single_page_pdf(Some(45), None);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);
        assert_eq!(resolve_inherited_rotate(&mut pdf, page_ref).unwrap(), 45);
    }

    #[test]
    fn resolve_reports_depth_limit_at_a_direct_page_tree_node() {
        let bytes = build_single_page_pdf(None, None);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);
        let mut page = pdf
            .resolve_object(page_ref)
            .unwrap()
            .into_dict()
            .expect("page must be a dictionary");
        let mut parent = crate::Dictionary::new();
        parent.insert("Rotate", Object::Integer(90));
        page.insert("Parent", Object::Dictionary(parent));
        pdf.set_object(page_ref, Object::Dictionary(page));

        let error = resolve_inherited_rotate_with_max_depth(&mut pdf, page_ref, 1).unwrap_err();
        assert!(matches!(
            error,
            Error::Unsupported(message)
                if message.contains("page tree depth exceeds maximum of 1")
                    && message.contains("direct page-tree dictionary")
        ));
    }

    #[test]
    fn resolve_rejects_a_non_integer_rotate_entry() {
        let bytes = build_single_page_pdf(None, None);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);
        let mut page = pdf
            .resolve_object(page_ref)
            .unwrap()
            .into_dict()
            .expect("page must be a dictionary");
        page.insert("Rotate", Object::Name(b"Bad".to_vec()));
        pdf.set_object(page_ref, Object::Dictionary(page));

        let error = resolve_inherited_rotate(&mut pdf, page_ref).unwrap_err();
        assert!(matches!(
            error,
            Error::Unsupported(message) if message.contains("/Rotate entry")
                && message.contains("has unexpected type")
        ));
    }

    // -----------------------------------------------------------------------
    // apply_rotate_to_pages tests
    // -----------------------------------------------------------------------

    #[test]
    fn apply_rejects_a_non_multiple_angle_like_qpdf() {
        let bytes = build_single_page_pdf(None, None);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let op = RotateOp {
            mode: RotateMode::Assign,
            degrees: 45,
        };

        let error = apply_rotate_to_pages(&mut pdf, &[ObjectRef::new(3, 0)], &op)
            .expect_err("qpdf rejects direct rotation angles that are not multiples of 90");
        assert!(matches!(
            error,
            Error::System(message)
                if message.contains("angle that is not a multiple of 90")
        ));
    }

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
        assert_eq!(rotate_value(&mut pdf, page_ref), Some(180));
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

        assert_eq!(rotate_value(&mut pdf, page_ref), Some(180));
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

        assert_eq!(rotate_value(&mut pdf, page_ref), Some(0));
    }

    #[test]
    fn add_ignores_non_quarter_turn_existing_rotate_like_qpdf() {
        // QPDFObjectHandle::rotatePage treats an existing /Rotate value that
        // is not a multiple of 90 as zero before applying a relative angle.
        let bytes = build_single_page_pdf(Some(45), None);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let op = RotateOp {
            mode: RotateMode::Add,
            degrees: 90,
        };
        apply_rotate_to_pages(&mut pdf, &[page_ref], &op).unwrap();

        assert_eq!(rotate_value(&mut pdf, page_ref), Some(90));
    }

    #[test]
    fn add_clamps_an_out_of_i32_range_existing_rotate_like_qpdf() {
        // QPDFObjectHandle::rotatePage reads the existing /Rotate through
        // getValueAsInt(int&), which saturates an out-of-range integer to
        // INT_MIN/INT_MAX (QPDFObjectHandle.cc:525-543) rather than using the
        // raw value. Live-probed against qpdf 11.9.0: a page with
        // `/Rotate 2147483700` (> i32::MAX, itself a multiple of 90) rotated
        // by `--rotate=+90` produces `/Rotate 90` — INT_MAX (2147483647) is
        // not a multiple of 90, so the existing-value guard resets it to 0
        // before adding.
        let bytes = build_single_page_pdf(None, None);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);
        let mut page = pdf
            .resolve_object(page_ref)
            .unwrap()
            .into_dict()
            .expect("page must be a dictionary");
        page.insert("Rotate", Object::Integer(2_147_483_700));
        pdf.set_object(page_ref, Object::Dictionary(page));

        let op = RotateOp {
            mode: RotateMode::Add,
            degrees: 90,
        };
        apply_rotate_to_pages(&mut pdf, &[page_ref], &op).unwrap();

        assert_eq!(rotate_value(&mut pdf, page_ref), Some(90));
    }

    #[test]
    fn add_clamps_a_below_i32_range_existing_rotate_like_qpdf() {
        // Mirrors add_clamps_an_out_of_i32_range_existing_rotate_like_qpdf
        // for the "too small" clamp branch. Live-probed against qpdf 11.9.0:
        // `/Rotate -2147483700` (< i32::MIN, itself a multiple of 90)
        // rotated by `--rotate=+90` produces `/Rotate 90` — INT_MIN
        // (-2147483648) is not a multiple of 90 either, so the
        // existing-value guard resets it to 0 before adding.
        let bytes = build_single_page_pdf(None, None);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);
        let mut page = pdf
            .resolve_object(page_ref)
            .unwrap()
            .into_dict()
            .expect("page must be a dictionary");
        page.insert("Rotate", Object::Integer(-2_147_483_700));
        pdf.set_object(page_ref, Object::Dictionary(page));

        let op = RotateOp {
            mode: RotateMode::Add,
            degrees: 90,
        };
        apply_rotate_to_pages(&mut pdf, &[page_ref], &op).unwrap();

        assert_eq!(rotate_value(&mut pdf, page_ref), Some(90));
    }

    #[test]
    fn add_clamps_a_near_i64_max_existing_rotate_without_overflow() {
        // A value near i64::MAX that IS a multiple of 90 (so it survives the
        // existing-value guard below and actually reaches the `+=`) must not
        // panic (integer overflow) or wrap when combined with the relative
        // angle; it must saturate to i32::MAX first, exactly as an
        // out-of-i32-range value does. 9_223_372_036_854_775_800 is the
        // largest multiple of 90 not exceeding i64::MAX.
        let bytes = build_single_page_pdf(None, None);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);
        let mut page = pdf
            .resolve_object(page_ref)
            .unwrap()
            .into_dict()
            .expect("page must be a dictionary");
        page.insert("Rotate", Object::Integer(9_223_372_036_854_775_800));
        pdf.set_object(page_ref, Object::Dictionary(page));

        let op = RotateOp {
            mode: RotateMode::Add,
            degrees: 90,
        };
        apply_rotate_to_pages(&mut pdf, &[page_ref], &op).unwrap();

        assert_eq!(rotate_value(&mut pdf, page_ref), Some(90));
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

        let Object::Dictionary(d) = pdf.resolve_object(page).unwrap() else {
            panic!("not a dict")
        };
        assert!(d.get("Rotate").is_none());
        let mb = pagebox_for(&mut pdf, page, b"/MediaBox");
        assert_eq!((mb.urx - mb.llx, mb.ury - mb.lly), (300.0, 200.0));

        let content = pages::page_content_bytes(&mut pdf, page).unwrap();
        let s = String::from_utf8(content).unwrap();
        assert!(s.starts_with("q\n0 -1 1 0 0 200 cm\n"), "{s:?}");
        assert!(s.contains("BT (x) Tj ET"), "{s:?}");

        // Round-trips through the writer + reparse without error.
        let buf = write_qpdf_to_memory(&mut pdf, |_| {}).unwrap();
        let mut pdf2 = Pdf::open(Cursor::new(buf)).unwrap();
        let p2 = pages::page_refs(&mut pdf2).unwrap()[0];
        let Object::Dictionary(d2) = pdf2.resolve_object(p2).unwrap() else {
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
        let mb = pagebox_for(&mut pdf, page, b"/MediaBox");
        assert_eq!((mb.urx, mb.ury), (200.0, 300.0));
    }

    #[test]
    fn flatten_180_keeps_mediabox_dims_and_zeroes_rotate() {
        let bytes = build_single_page_with_content("[0 0 200 300]", Some(180), "BT (x) Tj ET");
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page = pages::page_refs(&mut pdf).unwrap()[0];
        flatten_rotation_on_pages(&mut pdf, &[page]).unwrap();

        let Object::Dictionary(d) = pdf.resolve_object(page).unwrap() else {
            panic!("not a dict")
        };
        assert!(d.get("Rotate").is_none());
        // 180 maps [0 0 200 300] back onto itself: dims unchanged.
        let mb = pagebox_for(&mut pdf, page, b"/MediaBox");
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

        // 90deg map (x,y)->(y, 200 - x): corners (10,10),(190,290) ->
        // (10,190),(290,10) -> bbox [10 10 290 190].
        let cb = pagebox_for(&mut pdf, page, b"/CropBox");
        assert_eq!((cb.llx, cb.lly, cb.urx, cb.ury), (10.0, 10.0, 290.0, 190.0));
        // And MediaBox is still swapped, independently.
        let mb = pagebox_for(&mut pdf, page, b"/MediaBox");
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
        let page_dict = pdf.resolve_object(page).unwrap().into_dict().unwrap();
        let annot = page_dict
            .get("Annots")
            .and_then(Object::as_array)
            .and_then(|annots| annots.first())
            .and_then(Object::as_ref_id)
            .expect("flattened annotation must remain on the page");
        let r = pagebox_for(&mut pdf, annot, b"/Rect");
        assert_eq!((r.llx, r.lly, r.urx, r.ury), (20.0, 140.0, 40.0, 190.0));
    }

    #[test]
    fn flatten_transforms_direct_and_indirect_annotation_rects() {
        let bytes = build_single_page_with_direct_and_indirect_annots();
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page = pages::page_refs(&mut pdf).unwrap()[0];
        let indirect_annot = ObjectRef::new(5, 0);
        flatten_rotation_on_pages(&mut pdf, &[page]).unwrap();

        let page_handle = pdf.get_object_handle(page);
        pdf.resolve(&page_handle).unwrap();
        let annots_handle = page_handle.get_key(b"/Annots");
        pdf.resolve(&annots_handle).unwrap();
        let annots = annots_handle
            .as_array()
            .expect("/Annots must remain an array");
        let mut rects = annots
            .iter()
            .map(|annotation| {
                pdf.resolve(annotation).unwrap();
                let rect = annotation.get_key(b"/Rect");
                pdf.resolve(&rect).unwrap();
                let rectangle = handle_to_pagebox(&mut pdf, &rect).expect("annotation rectangle");
                (rectangle.llx, rectangle.lly, rectangle.urx, rectangle.ury)
            })
            .collect::<Vec<_>>();
        rects.sort_by(|left, right| left.partial_cmp(right).unwrap());
        assert_eq!(
            rects,
            vec![(20.0, 140.0, 40.0, 190.0), (30.0, 130.0, 50.0, 180.0)]
        );
        let original = object_key_handle(&mut pdf, indirect_annot, b"/Rect");
        assert_eq!(
            handle_to_pagebox(&mut pdf, &original),
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
        let page_dict = pdf.resolve_object(page).unwrap().into_dict().unwrap();
        let annot = page_dict
            .get("Annots")
            .and_then(Object::as_array)
            .and_then(|annots| annots.first())
            .and_then(Object::as_ref_id)
            .expect("flattened annotation must be indirect");
        let r = pagebox_for(&mut pdf, annot, b"/Rect");
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
        let Object::Dictionary(d) = pdf.resolve_object(page).unwrap() else {
            panic!("not a dict")
        };
        assert!(d.get("Rotate").is_none());
        assert_eq!(pages::page_content_bytes(&mut pdf, page).unwrap(), before);
        // The direct page box is untouched because qpdf's facade did not see a
        // direct `/Rotate` value.
        let mb = pagebox_for(&mut pdf, page, b"/MediaBox");
        assert_eq!((mb.urx - mb.llx, mb.ury - mb.lly), (200.0, 300.0));
    }
}
