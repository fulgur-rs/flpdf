//! qpdf correspondence: QPDFPageObjectHelper.cc page-to-Form-XObject conversion split from the page helper.
//! Convert a page into a Form XObject, mirroring qpdf's
//! `QPDFPageObjectHelper::getFormXObjectForPage`.
//!
//! This is the building block beneath qpdf's overlay/underlay feature: each
//! destination page is wrapped into a Form XObject (its content moves inside
//! the XObject, leaving the page free to draw it via a `Do` operator), and each
//! overlay/underlay source page is likewise wrapped and imported into the
//! destination document.
//!
//! # qpdf parity
//!
//! The produced Form XObject dictionary contains exactly these keys (written in
//! lexicographic order, the same order qpdf's `std::map`-backed dictionaries
//! serialize):
//!
//! - `/BBox` — the page's effective `/TrimBox` (falling back to `/CropBox` then
//!   `/MediaBox`), copied verbatim so its element types (integer vs. real) are
//!   preserved.
//! - `/Group` — present only when the page dictionary carries a `/Group`;
//!   shallow-copied (an indirect reference is materialized one level into a
//!   direct dictionary, matching qpdf's `shallowCopy`).
//! - `/Matrix` — the transformation matrix from the page's `/Rotate` and
//!   `/UserUnit`. Emitted only when at least one of `/Rotate` (inherited) or
//!   `/UserUnit` (leaf) is present; identity `[1 0 0 1 0 0]` for an explicit
//!   rotation 0 with unit scale.
//! - `/Resources` — the page's effective resources (inheritance resolved),
//!   inserted as a direct dictionary with its inner references preserved.
//! - `/Subtype` — `/Form`.
//! - `/Type` — `/XObject`.
//!
//! `/FormType` is deliberately not added (qpdf's `getFormXObjectForPage` does
//! not add it). No resource-name prefixing is performed (qpdf keeps each page's
//! resources inside its own XObject, so collisions cannot occur).
//!
//! The XObject stream holds the page's decoded, coalesced content bytes
//! uncompressed; the writer applies compression on output.
//!
//! qpdf 11.9.0 keeps this operation warning-based when the effective `/BBox`
//! is absent or malformed: `QPDFPageObjectHelper::getFormXObjectForPage`
//! creates the stream, calls `warnIfPossible`, and continues
//! (`libqpdf/QPDFPageObjectHelper.cc:706-733`). The production wrapper below
//! therefore delegates directly to the canonical `PageObjectHelper` instead
//! of pre-validating the box through a legacy `Object` snapshot. Attribute
//! inheritance and content streaming remain owned by the live handle route
//! (`libqpdf/QPDFPageObjectHelper.cc:220-310,439-476`;
//! `libqpdf/QPDFObjectHandle.cc:1289-1341`).
//!
//! The historical raw-object geometry/import helpers are `cfg(test)` only;
//! they are retained solely for old unit fixtures and cannot be reached by a
//! production consumer.

// The production entry points below are consumed by the overlay/underlay
// content-wiring path. The old raw-object geometry helpers remain only as
// `cfg(test)` fixtures for historical edge coverage; the production module has
// one canonical PageObjectHelper/ObjectHandle implementation.
use std::io::{Read, Seek};

use crate::page_object_helper::PageObjectHelper;
use crate::{Error, ObjectRef, Pdf, Result};

#[cfg(test)]
use crate::object_handle::ObjectHandle;
#[cfg(test)]
use crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH;
#[cfg(test)]
use crate::Matrix;
#[cfg(test)]
use std::collections::BTreeSet;

/// Convert the page at `page_ref` into a Form XObject within the same document,
/// insert it as a new object, and return its [`ObjectRef`].
///
/// Mirrors `QPDFPageObjectHelper::getFormXObjectForPage` (qpdf 11.9.0): the new
/// XObject's `/BBox` is the page's effective `/TrimBox` (copied verbatim),
/// `/Matrix` encodes the page's `/Rotate` and `/UserUnit` and is emitted only when
/// at least one of them is present, `/Resources` are the page's inheritance-resolved
/// resources, and the stream holds the page's decoded content. `/Group` is
/// shallow-copied when present. `/FormType` is not added.
///
/// # Errors
///
/// - [`Error::Unsupported`] when `page_ref` is not a `/Type /Page` dictionary
///   or when the object-number space is exhausted.
/// - Any error propagated from [`Pdf::resolve`] or content extraction.
pub(crate) fn get_form_xobject_for_page<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<ObjectRef> {
    let mut helper = PageObjectHelper::new(page_ref, pdf);
    let form = helper.get_form_xobject_for_page(true)?;
    // cov:ignore-start: Pdf::new_stream registers every canonical Form as an
    // indirect document-owned object; this is an allocation invariant guard.
    form.object_ref().ok_or_else(|| {
        Error::Internal(
            "canonical Form XObject was not registered as an indirect object".to_owned(),
        )
    })
    // cov:ignore-end
}

/// Convert a page from `source` into a Form XObject and import it into `dest`.
///
/// Mirrors qpdf's `copyForeignObject` step in overlay/underlay: the page is
/// first wrapped into a Form XObject *inside `source`*, then the XObject and
/// every object it transitively references are deep-copied into `dest` via
/// [`Pdf::copy_foreign_object`]. The returned
/// [`ObjectRef`] is the imported XObject's reference in `dest`.
///
/// # Errors
///
/// - [`Error::Unsupported`] when the source page cannot be converted or when
///   the destination object-number space is exhausted.
/// - Any error propagated from [`Pdf::resolve`] or the cross-document copier.
#[cfg(test)]
pub(crate) fn import_page_as_form_xobject<RS, RT>(
    dest: &mut Pdf<RT>,
    source: &mut Pdf<RS>,
    source_page_ref: ObjectRef,
) -> Result<ObjectRef>
where
    RS: Read + Seek,
    RT: Read + Seek,
{
    // 1. Build the Form XObject inside the source document (qpdf order:
    //    getFormXObjectForPage runs on the source page first).
    let xobject_ref = get_form_xobject_for_page(source, source_page_ref)?;

    // 2. Copy the XObject through qpdf's persistent canonical foreign graph
    // operation. The destination retains the source identity map, so shared
    // descendants are reused exactly as they are for overlay/underlay.
    let source_handle = source.get_object_handle(xobject_ref);
    let copied = dest.copy_foreign_object(&source_handle)?;
    copied
        .object_ref()
        .ok_or_else(|| Error::Unsupported("imported Form XObject is not indirect".to_string()))
}

/// Import several `source` pages into `dest` as Form XObjects in a single
/// cross-document copy, returning each page's imported XObject [`ObjectRef`] in
/// the same order as `source_page_refs`.
///
/// Unlike calling [`import_page_as_form_xobject`] once per page, this performs
/// all copies against one destination document and therefore reuses qpdf's
/// persistent foreign→local identity map. A single source resource referenced
/// by more than one page (a `/Font`, `/ProcSet`, image, …) is copied once,
/// matching qpdf's `copyForeignObject` behavior.
///
/// `source_page_refs` should already be distinct; duplicate refs would request
/// the same imported XObject twice and are not deduplicated here.
///
/// # Errors
///
/// - [`Error::Unsupported`] when a source page cannot be converted, when the
///   destination object-number space is exhausted, or when an imported XObject
///   is unexpectedly absent from the copy map.
/// - Any error propagated from [`Pdf::resolve`] or the cross-document copier.
#[cfg(test)]
pub(crate) fn import_pages_as_form_xobjects<RS, RT>(
    dest: &mut Pdf<RT>,
    source: &mut Pdf<RS>,
    source_page_refs: &[ObjectRef],
) -> Result<Vec<ObjectRef>>
where
    RS: Read + Seek,
    RT: Read + Seek,
{
    // 1. Build a Form XObject inside `source` for each page.
    let mut xobject_refs = Vec::with_capacity(source_page_refs.len());
    for &page_ref in source_page_refs {
        let xobject_ref = get_form_xobject_for_page(source, page_ref)?;
        xobject_refs.push(xobject_ref);
    }

    // 2. Copy each root through the same persistent destination-side map. The
    //    canonical copier deduplicates shared descendants across these calls.
    xobject_refs
        .iter()
        .map(|xref| {
            let source_handle = source.get_object_handle(*xref);
            let copied = dest.copy_foreign_object(&source_handle)?;
            let copied_ref = copied.object_ref();
            if copied_ref.is_none() {
                // cov:ignore-start: qpdf guarantees an indirect result for this indirect Form XObject
                return Err(Error::Unsupported(
                    "imported Form XObject is not indirect".to_string(),
                ));
                // cov:ignore-end
            }
            Ok(copied_ref.expect("copyForeignObject returned an indirect Form XObject"))
        })
        .collect()
}

/// Resolve the page's effective box as canonical child handles, following qpdf's
/// `getTrimBox(false)` fallback chain: `/TrimBox` → `/CropBox` → `/MediaBox`.
///
/// The array is returned verbatim (original integer/real element types kept) so
/// `/BBox` is byte-identical to qpdf's shallow copy of the source rectangle.
///
/// `/TrimBox` is leaf-only (not inheritable, ISO 32000-1 Table 30), while
/// `/CropBox` and `/MediaBox` are inheritable and resolved through the `/Parent`
/// chain — matching qpdf's `getTrimBox`/`getCropBox`/`getMediaBox` fallback and
/// flpdf's own [`PageObjectHelper::crop_box`](crate::PageObjectHelper::crop_box).
#[cfg(test)]
#[allow(dead_code)]
fn effective_box_array<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<Vec<ObjectHandle>> {
    // TrimBox: leaf only.
    if let Some(arr) = leaf_box_array(pdf, page_ref, b"TrimBox")? {
        return Ok(arr);
    }
    // CropBox then MediaBox: both inheritable, walk the /Parent chain.
    for key in [b"CropBox".as_slice(), b"MediaBox".as_slice()] {
        if let Some(arr) = inherited_box_array(pdf, page_ref, key)? {
            return Ok(arr);
        }
    }
    Err(Error::Unsupported(format!(
        "page {page_ref} has no /TrimBox, /CropBox, or /MediaBox for the Form XObject /BBox"
    )))
}

/// Read a box `key` from the leaf page dictionary only (not inheritable), as a
/// handle rectangle array. Returns `Ok(None)` when absent or null.
#[cfg(test)]
fn leaf_box_array<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    key: &[u8],
) -> Result<Option<Vec<ObjectHandle>>> {
    let page = pdf.get_object_handle(page_ref);
    pdf.resolve(&page)?;
    let Some(dict) = page.as_dictionary() else {
        return Ok(None);
    };
    // Dictionary keys carry qpdf's canonical leading `/` (see
    // ObjectHandle::as_dictionary); `key` is the slash-less name used for
    // error-message formatting below, so the lookup key is built here.
    let lookup_key: Vec<u8> = [b"/".as_slice(), key].concat();
    let val = match dict.get(lookup_key.as_slice()).cloned() {
        None => return Ok(None),
        Some(v) => v,
    };
    if val.is_null() {
        return Ok(None);
    }
    resolve_rect_array(pdf, val, page_ref, key)
}

/// Walk the `/Parent` chain looking for an inheritable box `key` as a raw
/// rectangle array. Cycle- and depth-guarded.
#[cfg(test)]
fn inherited_box_array<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    key: &[u8],
) -> Result<Option<Vec<ObjectHandle>>> {
    let mut seen: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut current = page_ref;
    let mut depth: usize = 0;

    loop {
        if depth >= DEFAULT_MAX_PAGE_TREE_DEPTH {
            return Err(Error::Unsupported(format!(
                "page tree depth exceeds maximum of {DEFAULT_MAX_PAGE_TREE_DEPTH} at {current}"
            )));
        }
        if !seen.insert(current) {
            return Ok(None);
        }

        let node = pdf.get_object_handle(current);
        pdf.resolve(&node)?;
        let Some(dict) = node.as_dictionary() else {
            return Ok(None);
        };
        // Per PDF §7.3.9 a null value is equivalent to the key being absent, so
        // skip it and climb to /Parent. Dictionary keys carry qpdf's canonical
        // leading `/` (see ObjectHandle::as_dictionary); `key` is the
        // slash-less name used for error-message formatting, so the lookup
        // key is built here.
        let lookup_key: Vec<u8> = [b"/".as_slice(), key].concat();
        let val = match dict.get(lookup_key.as_slice()).cloned() {
            Some(v) if !v.is_null() => Some(v),
            _ => None,
        };
        let parent_val = dict.get(b"/Parent".as_slice()).cloned();

        if let Some(value) = val {
            if let Some(array) = resolve_rect_array(pdf, value, current, key)? {
                return Ok(Some(array));
            }
        }

        match parent_val.and_then(|value| value.object_ref().or_else(|| value.as_reference())) {
            Some(r) => {
                current = r;
                depth += 1;
            }
            _ => return Ok(None),
        }
    }
}

/// Coerce a box value (a direct array or a reference to one) into a handle
/// rectangle array, validating it has at least four elements.
#[cfg(test)]
fn resolve_rect_array<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    val: ObjectHandle,
    node: ObjectRef,
    key: &[u8],
) -> Result<Option<Vec<ObjectHandle>>> {
    if val.is_indirect() || val.as_reference().is_some() {
        pdf.resolve(&val)?;
    }
    let resolved = val;
    let Some(arr) = resolved.as_array() else {
        if resolved.is_null() {
            return Ok(None);
        }
        return Err(Error::Unsupported(format!(
            "/{} value on node {node} does not resolve to an array",
            String::from_utf8_lossy(key)
        )));
    };
    if arr.len() < 4 {
        return Err(Error::Unsupported(format!(
            "/{} rectangle on node {node} has {} elements, expected 4",
            String::from_utf8_lossy(key),
            arr.len()
        )));
    }
    Ok(Some(arr))
}

/// Return the indirect target reference carried by a child handle, including
/// the private redirect shape used by older mutation fixtures.
#[cfg(test)]
fn handle_reference(handle: &ObjectHandle) -> Option<ObjectRef> {
    handle.object_ref().or_else(|| handle.as_reference())
}

/// Compute the normalized `(width, height)` of a rectangle array, coercing each
/// numeric element to `f64`. Non-numeric elements contribute 0.0.
///
/// qpdf reads box geometry through `QPDFObjectHandle::getArrayAsRectangle`, which
/// normalizes corners with min/max, so the extents are always non-negative even for
/// a reversed box (`urx < llx` or `ury < lly`): `width = |urx - llx|`,
/// `height = |ury - lly|`.
#[cfg(test)]
fn rectangle_dimensions(arr: &[ObjectHandle]) -> (f64, f64) {
    let n = |o: &ObjectHandle| -> f64 {
        o.as_integer()
            .map(|value| value as f64)
            .or_else(|| o.as_real())
            .unwrap_or(0.0)
    };
    let llx = n(&arr[0]);
    let lly = n(&arr[1]);
    let urx = n(&arr[2]);
    let ury = n(&arr[3]);
    ((urx - llx).abs(), (ury - lly).abs())
}

/// A page's `/Rotate` and `/UserUnit` attributes as qpdf's
/// `getMatrixForTransformations` reads them.
///
/// qpdf decides whether to emit a Form XObject `/Matrix` from *presence*
/// (`isNull`), and computes the matrix from *value*; the two are tracked
/// separately so a present-but-malformed attribute still forces emission while
/// contributing its qpdf default to the matrix.
#[cfg(test)]
pub(crate) struct PageTransform {
    /// `/Rotate` is present (non-null) somewhere in the inheritance chain.
    pub rotate_present: bool,
    /// Raw `/Rotate` integer; 0 when present-but-not-an-integer or absent
    /// (qpdf uses `getIntValueAsInt()` and falls back to 0 for non-integers).
    pub rotate: i32,
    /// `/UserUnit` is present (non-null) on the leaf page.
    pub uu_present: bool,
    /// `/UserUnit` numeric value; 1.0 when present-but-not-a-number or absent.
    pub scale: f64,
}

/// Read a page's `/Rotate` and `/UserUnit` the way qpdf's `getAttribute` does:
/// `/Rotate` is inheritable (walk the `/Parent` chain), `/UserUnit` is leaf-only.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn read_page_transform<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<PageTransform> {
    let (rotate_present, rotate) = inherited_rotate_attribute(pdf, page_ref)?;
    let (uu_present, scale) = leaf_user_unit(pdf, page_ref)?;
    Ok(PageTransform {
        rotate_present,
        rotate,
        uu_present,
        scale,
    })
}

/// Walk the `/Parent` chain for the first non-null `/Rotate`, mirroring qpdf's
/// inheritable `getAttribute("/Rotate", false)`. Returns `(present, raw_int)`:
/// `present` is whether any node carried a non-null `/Rotate`; `raw_int` is its
/// integer value (0 when present-but-not-an-integer). Cycle- and depth-guarded.
#[cfg(test)]
fn inherited_rotate_attribute<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<(bool, i32)> {
    let mut seen: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut current = page_ref;
    let mut depth: usize = 0;

    loop {
        if depth >= DEFAULT_MAX_PAGE_TREE_DEPTH {
            return Err(Error::Unsupported(format!(
                "page tree depth exceeds maximum of {DEFAULT_MAX_PAGE_TREE_DEPTH} at {current}"
            )));
        }
        if !seen.insert(current) {
            return Ok((false, 0));
        }

        let (rotate_val, parent_val) = {
            let node = pdf.get_object_handle(current);
            pdf.resolve(&node)?;
            let Some(dict) = node.as_dictionary() else {
                return Ok((false, 0));
            };
            (
                dict.get(b"/Rotate".as_slice()).cloned(),
                dict.get(b"/Parent".as_slice()).cloned(),
            )
        };

        if let Some(val) = rotate_val {
            // /Rotate may be stored as an indirect reference; resolve it first.
            if val.is_indirect() || val.as_reference().is_some() {
                pdf.resolve(&val)?;
            }
            let resolved = val;
            if let Some(n) = resolved.as_integer() {
                return Ok((true, n as i32));
            }
            if !resolved.is_null() {
                return Ok((true, 0));
            }
        }

        match parent_val.and_then(|value| handle_reference(&value)) {
            Some(r) => {
                current = r;
                depth += 1;
            }
            _ => return Ok((false, 0)),
        }
    }
}

/// Read the leaf page's `/UserUnit` (not inheritable). Returns `(present, value)`:
/// `present` is whether the leaf carried a non-null `/UserUnit`; `value` is its
/// numeric value (1.0 when present-but-not-a-number).
#[cfg(test)]
fn leaf_user_unit<R: Read + Seek>(pdf: &mut Pdf<R>, page_ref: ObjectRef) -> Result<(bool, f64)> {
    let page = pdf.get_object_handle(page_ref);
    pdf.resolve(&page)?;
    let Some(dict) = page.as_dictionary() else {
        return Ok((false, 1.0));
    };
    let uu_val = dict.get(b"/UserUnit".as_slice()).cloned();
    let Some(val) = uu_val else {
        return Ok((false, 1.0));
    };
    if val.is_indirect() || val.as_reference().is_some() {
        pdf.resolve(&val)?;
    }
    let resolved = val;
    if resolved.is_null() {
        Ok((false, 1.0))
    } else {
        Ok((
            true,
            resolved
                .as_integer()
                .map(|value| value as f64)
                .or_else(|| resolved.as_real())
                .unwrap_or(1.0),
        ))
    }
}

/// Compute a page's transformation matrix `[a b c d e f]`, mirroring qpdf's
/// `getMatrixForTransformations` (qpdf 11.9.0) exactly.
///
/// Returns the identity when neither `/Rotate` nor `/UserUnit` is present. With
/// `scale` = `/UserUnit` (or 1.0) and `rotate` the raw integer:
/// - rotate  90 → `[0 -scale scale 0 0 width*scale]`
/// - rotate 180 → `[-scale 0 0 -scale width*scale height*scale]`
/// - rotate 270 → `[0 scale -scale 0 height*scale 0]`
/// - otherwise  → `[scale 0 0 scale 0 0]`
///
/// `invert` inverts the destination-page transform (used by overlay placement):
/// `scale` becomes `1/scale` (identity when `scale == 0`) and `rotate` becomes
/// `360 - rotate` before the switch.
#[cfg(test)]
pub(crate) fn get_matrix_for_transformations(
    t: &PageTransform,
    width: f64,
    height: f64,
    invert: bool,
) -> Matrix {
    if !(t.rotate_present || t.uu_present) {
        return Matrix::default();
    }
    let mut scale = t.scale;
    let mut rotate = t.rotate;
    if invert {
        if scale == 0.0 {
            return Matrix::default();
        }
        scale = 1.0 / scale;
        rotate = 360 - rotate;
    }
    match rotate {
        90 => Matrix::new(0.0, -scale, scale, 0.0, 0.0, width * scale),
        180 => Matrix::new(-scale, 0.0, 0.0, -scale, width * scale, height * scale),
        270 => Matrix::new(0.0, scale, -scale, 0.0, height * scale, 0.0),
        _ => Matrix::new(scale, 0.0, 0.0, scale, 0.0, 0.0),
    }
}

/// Read the page dictionary's `/Group` value with qpdf `shallowCopy` semantics:
/// an indirect reference is resolved **one level** into the direct object it
/// points to (inner references inside that object are left untouched). A direct
/// value is returned as-is. Returns `Ok(None)` when absent or null.
///
/// qpdf's `getFormXObjectForPage` stores `getAttribute("/Group", false)
/// .shallowCopy()`, the same mechanism it uses for `/Resources`; observed in
/// qpdf 11.9.0 overlay output, an indirect `/Group` becomes a direct dictionary
/// in the Form XObject (`/Group << ... >>`, no separate object). `/Group` is not
/// inheritable (ISO 32000-1 Table 30), so only the leaf page is consulted.
#[cfg(test)]
fn page_group<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<Option<ObjectHandle>> {
    let page = pdf.get_object_handle(page_ref);
    pdf.resolve(&page)?;
    let Some(dict) = page.as_dictionary() else {
        return Ok(None);
    };
    let group_val = dict.get(b"/Group".as_slice()).cloned();
    match group_val {
        None => Ok(None),
        Some(value) if value.is_null() => Ok(None),
        // qpdf calls shallowCopy() on the attribute value unconditionally
        // (libqpdf/QPDFPageObjectHelper.cc:715), not only when it is
        // indirect: an indirect value is first resolved one level (ref ->
        // direct dict), then both branches shallow-copy so the returned
        // handle never shares mutable identity with the page's own /Group.
        Some(value) if value.is_indirect() || value.as_reference().is_some() => {
            pdf.resolve(&value)?;
            Ok(Some(value.shallow_copy()?))
        }
        Some(direct) => Ok(Some(direct.shallow_copy()?)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object_handle::canonical_dictionary_key;

    #[derive(Clone)]
    struct TestHandle(ObjectHandle);

    impl TestHandle {
        fn as_dict(&self) -> Option<TestDict> {
            self.0.as_dictionary().map(TestDict)
        }

        fn as_array(&self) -> Option<Vec<Self>> {
            self.0
                .as_array()
                .map(|items| items.into_iter().map(Self).collect())
        }

        fn as_name(&self) -> Option<Vec<u8>> {
            self.0.as_name()
        }

        fn as_integer(&self) -> Option<i64> {
            self.0.as_integer()
        }

        fn as_real(&self) -> Option<f64> {
            self.0.as_real()
        }

        fn object_ref(&self) -> Option<ObjectRef> {
            self.0.object_ref().or_else(|| self.0.as_reference())
        }
    }

    struct TestDict(std::collections::BTreeMap<Vec<u8>, ObjectHandle>);

    impl TestDict {
        fn get(&self, key: &str) -> Option<TestHandle> {
            let key = canonical_dictionary_key(key.as_bytes());
            self.0.get(&key).cloned().map(TestHandle)
        }

        fn iter(&self) -> impl Iterator<Item = (&Vec<u8>, &ObjectHandle)> {
            self.0.iter()
        }
    }

    struct TestStream {
        dict: TestDict,
        data: Vec<u8>,
    }

    /// Build a valid single-object-table PDF from `(number, body)` definitions
    /// plus a `/Root` number, computing xref offsets so the bytes parse. Object
    /// numbers must be contiguous starting at 1 (the test fixtures all are).
    fn build_pdf(objects: &[(u32, &str)], root: u32) -> Vec<u8> {
        let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
        let mut offsets: std::collections::BTreeMap<u32, u64> = std::collections::BTreeMap::new();
        let max = objects.iter().map(|(n, _)| *n).max().unwrap_or(0);
        for (n, body) in objects {
            offsets.insert(*n, out.len() as u64);
            out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
        }
        let xref_start = out.len() as u64;
        let size = max + 1;
        out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for n in 1..=max {
            let off = offsets
                .get(&n)
                .expect("test fixtures use contiguous object numbers");
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {size} /Root {root} 0 R >>\nstartxref\n{xref_start}\n%%EOF\n"
            )
            .as_bytes(),
        );
        out
    }

    /// A one-page PDF: catalog(1) → pages(2) → page(3), font(4), content(5).
    /// `page_extra` is spliced into the page dict (e.g. `/Rotate 90` or
    /// `/TrimBox [...]` or `/Group N 0 R`).
    fn one_page_doc(page_extra: &str, content: &str, extra_objs: &[(u32, &str)]) -> Vec<u8> {
        let page_body = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R {page_extra} >>"
        );
        let content_body = format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        );
        let mut objs: Vec<(u32, String)> = vec![
            (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()),
            (3, page_body),
            (
                4,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
            ),
            (5, content_body),
        ];
        for (n, b) in extra_objs {
            objs.push((*n, (*b).to_string()));
        }
        let borrowed: Vec<(u32, &str)> = objs.iter().map(|(n, b)| (*n, b.as_str())).collect();
        build_pdf(&borrowed, 1)
    }

    fn open(bytes: Vec<u8>) -> Pdf<std::io::Cursor<Vec<u8>>> {
        Pdf::open_mem_owned(bytes).unwrap()
    }

    /// Resolve the Form XObject at `xref` and return its stream.
    fn form_stream<R: Read + Seek>(pdf: &mut Pdf<R>, xref: ObjectRef) -> TestStream {
        let stream = pdf.get_object_handle(xref);
        pdf.resolve(&stream).unwrap();
        let dict = stream
            .as_stream_dict()
            .expect("Form XObject must be a stream")
            .as_dictionary()
            .expect("Form XObject stream dictionary");
        let data = stream
            .as_stream_data()
            .map(|data| data.as_ref().clone())
            .or_else(|| {
                stream
                    .get_raw_stream_data()
                    .ok()
                    .map(|data| data.as_ref().clone())
            })
            .expect("Form XObject stream data");
        TestStream {
            dict: TestDict(dict),
            data,
        }
    }

    /// Coerce a rectangle/matrix array's numeric elements to `i64` for whole-
    /// number comparison (each element resolves via the numeric accessors).
    fn numbers(arr: impl AsRef<[TestHandle]>) -> Vec<i64> {
        arr.as_ref()
            .iter()
            .map(|handle| {
                handle
                    .as_integer()
                    .or_else(|| handle.as_real().map(|r| r as i64))
                    .expect("numeric array element")
            })
            .collect()
    }

    #[test]
    fn page_to_form_xobject_builds_expected_dict_and_stream() {
        let mut pdf = open(one_page_doc("", "Hello content", &[]));
        let page_ref = ObjectRef::new(3, 0);
        let expected_content = crate::pages::page_content_bytes(&mut pdf, page_ref).unwrap();

        let xref = get_form_xobject_for_page(&mut pdf, page_ref).unwrap();
        let stream = form_stream(&mut pdf, xref);
        let dict = &stream.dict;

        // Exact key set: BBox, Resources, Subtype, Type. NO FormType, and NO
        // /Matrix because this page carries neither /Rotate nor /UserUnit (qpdf's
        // getFormXObjectForPage omits /Matrix in that case).
        let keys: BTreeSet<Vec<u8>> = dict
            .iter()
            .map(|(key, _)| key.strip_prefix(b"/").unwrap_or(key).to_vec())
            .collect();
        let expected: BTreeSet<Vec<u8>> = [
            b"BBox".to_vec(),
            b"Resources".to_vec(),
            b"Subtype".to_vec(),
            b"Type".to_vec(),
        ]
        .into_iter()
        .collect();
        assert_eq!(keys, expected, "unexpected key set on Form XObject");
        assert!(
            dict.get("FormType").is_none(),
            "qpdf getFormXObjectForPage must NOT add /FormType"
        );
        assert!(
            dict.get("Matrix").is_none(),
            "qpdf omits /Matrix when neither /Rotate nor /UserUnit is present"
        );

        // /Subtype /Form, /Type /XObject.
        assert_eq!(
            dict.get("Subtype").unwrap().as_name(),
            Some(b"Form".to_vec())
        );
        assert_eq!(
            dict.get("Type").unwrap().as_name(),
            Some(b"XObject".to_vec())
        );

        // /BBox == page TrimBox (== MediaBox via fallback) [0 0 612 792].
        let bbox = dict.get("BBox").unwrap().as_array().unwrap();
        assert_eq!(numbers(bbox), vec![0, 0, 612, 792]);

        // /Resources present (carries the page's font, ref preserved).
        let res = dict.get("Resources").unwrap().as_dict().unwrap();
        assert!(res.get("Font").is_some(), "Resources should carry /Font");

        // Stream body == decoded page content.
        assert_eq!(stream.data, expected_content);
    }

    #[test]
    fn page_to_form_xobject_uses_trimbox_when_present() {
        // TrimBox != MediaBox; with one real (fractional) coordinate so element
        // types are preserved verbatim.
        let mut pdf = open(one_page_doc("/TrimBox [10 10 500.5 600]", "x", &[]));
        let xref = get_form_xobject_for_page(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        let stream = form_stream(&mut pdf, xref);
        let bbox = stream.dict.get("BBox").unwrap().as_array().unwrap();
        // Verbatim copy: integers stay integers, real stays real.
        assert_eq!(bbox[0].as_integer(), Some(10));
        assert_eq!(bbox[1].as_integer(), Some(10));
        assert!((bbox[2].as_real().unwrap() - 500.5).abs() < 1e-9);
        assert_eq!(bbox[3].as_integer(), Some(600));
    }

    #[test]
    fn page_to_form_xobject_bbox_falls_back_to_inherited_cropbox() {
        // Leaf page has neither /TrimBox nor /CropBox; the ancestor /Pages node
        // carries an inheritable /CropBox. qpdf's getTrimBox -> getCropBox is
        // inheritable, so the /BBox must be that CropBox, NOT the MediaBox.
        let page = "<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>";
        let pages = "<< /Type /Pages /Kids [3 0 R] /Count 1 \
                      /MediaBox [0 0 612 792] /CropBox [5 5 300 400] >>";
        let content = "<< /Length 1 >>\nstream\nx\nendstream";
        let mut pdf = open(build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, pages),
                (3, page),
                (4, content),
            ],
            1,
        ));
        let xref = get_form_xobject_for_page(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        let stream = form_stream(&mut pdf, xref);
        let bbox = stream.dict.get("BBox").unwrap().as_array().unwrap();
        assert_eq!(
            numbers(bbox),
            vec![5, 5, 300, 400],
            "inherited CropBox must win over MediaBox for /BBox"
        );
    }

    #[test]
    fn page_to_form_xobject_bbox_resolves_indirect_box_array() {
        // /TrimBox stored as an indirect reference to an array exercises the
        // reference-resolution arm of resolve_rect_array.
        let trimbox = (6u32, "[1 2 3 4]");
        let mut pdf = open(one_page_doc("/TrimBox 6 0 R", "x", &[trimbox]));
        let xref = get_form_xobject_for_page(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        let stream = form_stream(&mut pdf, xref);
        let bbox = stream.dict.get("BBox").unwrap().as_array().unwrap();
        assert_eq!(numbers(bbox), vec![1, 2, 3, 4]);
    }

    #[test]
    fn page_to_form_xobject_creates_form_for_short_box_array_with_warning() {
        // qpdf keeps the Form XObject and warns when /BBox is malformed; the
        // canonical helper must not turn that warning into an error.
        let mut pdf = open(one_page_doc("/TrimBox [0 0 5]", "x", &[]));
        let xref = get_form_xobject_for_page(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        assert!(form_stream(&mut pdf, xref).dict.get("BBox").is_some());
    }

    #[test]
    fn page_to_form_xobject_creates_form_for_non_array_box_with_warning() {
        // A /TrimBox that is a name, not an array, is malformed, but qpdf
        // creates the Form XObject and emits a warning.
        let mut pdf = open(one_page_doc("/TrimBox /NotARect", "x", &[]));
        let xref = get_form_xobject_for_page(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        assert!(form_stream(&mut pdf, xref).dict.get("BBox").is_some());
    }

    #[test]
    fn page_to_form_xobject_creates_form_for_box_ref_to_non_array_with_warning() {
        // /CropBox is an indirect ref that resolves to a dictionary, not an
        // array. qpdf keeps the copied value in /BBox and warns.
        let bad = (6u32, "<< /Type /Foo >>");
        let mut pdf = open(one_page_doc("/CropBox 6 0 R", "x", &[bad]));
        let xref = get_form_xobject_for_page(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        assert!(form_stream(&mut pdf, xref).dict.get("BBox").is_some());
    }

    #[test]
    fn page_to_form_xobject_creates_form_when_no_box_exists_with_warning() {
        // No /TrimBox, /CropBox, or /MediaBox on the page or its parent. qpdf
        // stores a null /BBox (which the dictionary omits) and warns.
        let page = "<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>";
        let content = "<< /Length 1 >>\nstream\nx\nendstream";
        let mut pdf = open(build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (3, page),
                (4, content),
            ],
            1,
        ));
        let xref = get_form_xobject_for_page(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        assert!(form_stream(&mut pdf, xref).dict.get("BBox").is_none());
    }

    #[test]
    fn page_to_form_xobject_handles_non_numeric_box_element() {
        // A rectangle with a non-numeric element: rectangle_dimensions treats it
        // as 0.0 (the array is still copied verbatim into /BBox). /Rotate 0 keeps
        // /Matrix present so the identity assertion is exercised.
        let mut pdf = open(one_page_doc("/TrimBox [0 0 /X 100] /Rotate 0", "x", &[]));
        let xref = get_form_xobject_for_page(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        let stream = form_stream(&mut pdf, xref);
        let bbox = stream.dict.get("BBox").unwrap().as_array().unwrap();
        assert!(bbox[2].as_name().is_some());
        // Matrix is identity (rotate 0), so the non-numeric width is irrelevant.
        let matrix = stream.dict.get("Matrix").unwrap().as_array().unwrap();
        assert_eq!(numbers(matrix), vec![1, 0, 0, 1, 0, 0]);
    }

    #[test]
    fn rectangle_dimensions_normalizes_swapped_box() {
        // qpdf reads box geometry through getArrayAsRectangle, so a reversed box
        // ([612 792 0 0]) yields non-negative width/height; an ordered box is
        // unchanged.
        let swapped = [
            ObjectHandle::integer(612),
            ObjectHandle::integer(792),
            ObjectHandle::integer(0),
            ObjectHandle::integer(0),
        ];
        assert_eq!(rectangle_dimensions(&swapped), (612.0, 792.0));
        let ordered = [
            ObjectHandle::integer(0),
            ObjectHandle::integer(0),
            ObjectHandle::integer(612),
            ObjectHandle::integer(792),
        ];
        assert_eq!(rectangle_dimensions(&ordered), (612.0, 792.0));
    }

    #[test]
    fn page_to_form_xobject_rotate_90_matrix() {
        // MediaBox 0 0 612 792, /Rotate 90 -> matrix [0 -1 1 0 0 width].
        let mut pdf = open(one_page_doc("/Rotate 90", "x", &[]));
        let xref = get_form_xobject_for_page(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        let stream = form_stream(&mut pdf, xref);
        let matrix = stream.dict.get("Matrix").unwrap().as_array().unwrap();
        // width = urx - llx = 612 - 0 = 612.
        assert_eq!(numbers(matrix), vec![0, -1, 1, 0, 0, 612]);
    }

    #[test]
    fn page_to_form_xobject_rotate_180_and_270() {
        // 180 -> [-1 0 0 -1 width height]; 270 -> [0 1 -1 0 height 0].
        let mut pdf = open(one_page_doc("/Rotate 180", "x", &[]));
        let xref = get_form_xobject_for_page(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        let stream = form_stream(&mut pdf, xref);
        let m = stream.dict.get("Matrix").unwrap().as_array().unwrap();
        assert_eq!(numbers(m), vec![-1, 0, 0, -1, 612, 792]);

        let mut pdf = open(one_page_doc("/Rotate 270", "x", &[]));
        let xref = get_form_xobject_for_page(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        let stream = form_stream(&mut pdf, xref);
        let m = stream.dict.get("Matrix").unwrap().as_array().unwrap();
        assert_eq!(numbers(m), vec![0, 1, -1, 0, 792, 0]);
    }

    #[test]
    fn page_to_form_xobject_explicit_rotate_0_emits_identity_matrix() {
        // An explicit /Rotate 0 is *present* (non-null), so qpdf still emits
        // /Matrix — the identity. (Absence of /Rotate omits it; this guards that
        // the presence check, not the value, drives emission.)
        let mut pdf = open(one_page_doc("/Rotate 0", "x", &[]));
        let xref = get_form_xobject_for_page(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        let stream = form_stream(&mut pdf, xref);
        let m = stream.dict.get("Matrix").unwrap().as_array().unwrap();
        assert_eq!(numbers(m), vec![1, 0, 0, 1, 0, 0]);
    }

    #[test]
    fn page_to_form_xobject_inherited_rotate_emits_matrix() {
        // The leaf page has no /Rotate; the ancestor /Pages node carries
        // /Rotate 90. qpdf inherits /Rotate, so /Matrix is emitted for the
        // inherited rotation. MediaBox 0 0 612 792 -> [0 -1 1 0 0 612].
        let page = "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>";
        let pages = "<< /Type /Pages /Kids [3 0 R] /Count 1 /Rotate 90 >>";
        let content = "<< /Length 1 >>\nstream\nx\nendstream";
        let mut pdf = open(build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, pages),
                (3, page),
                (4, content),
            ],
            1,
        ));
        let xref = get_form_xobject_for_page(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        let stream = form_stream(&mut pdf, xref);
        let m = stream.dict.get("Matrix").unwrap().as_array().unwrap();
        assert_eq!(numbers(m), vec![0, -1, 1, 0, 0, 612]);
    }

    #[test]
    fn page_to_form_xobject_userunit_only_emits_scale_matrix() {
        // /UserUnit 2 with no /Rotate: qpdf emits /Matrix with the scale folded in
        // (rotate-0 default branch -> [scale 0 0 scale 0 0]).
        let mut pdf = open(one_page_doc("/UserUnit 2", "x", &[]));
        let xref = get_form_xobject_for_page(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        let stream = form_stream(&mut pdf, xref);
        let m = stream.dict.get("Matrix").unwrap().as_array().unwrap();
        assert_eq!(numbers(m), vec![2, 0, 0, 2, 0, 0]);
    }

    #[test]
    fn page_to_form_xobject_userunit_and_rotate_90() {
        // /UserUnit 2 /Rotate 90 on 612x792 -> [0 -2 2 0 0 width*scale=1224].
        let mut pdf = open(one_page_doc("/UserUnit 2 /Rotate 90", "x", &[]));
        let xref = get_form_xobject_for_page(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        let stream = form_stream(&mut pdf, xref);
        let m = stream.dict.get("Matrix").unwrap().as_array().unwrap();
        assert_eq!(numbers(m), vec![0, -2, 2, 0, 0, 1224]);
    }

    #[test]
    fn page_to_form_xobject_present_non_integer_rotate_emits_identity() {
        // A present-but-non-integer /Rotate is non-null, so /Matrix is emitted;
        // qpdf treats a non-integer rotation as 0 -> identity.
        let mut pdf = open(one_page_doc("/Rotate /X", "x", &[]));
        let xref = get_form_xobject_for_page(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        let stream = form_stream(&mut pdf, xref);
        let m = stream.dict.get("Matrix").unwrap().as_array().unwrap();
        assert_eq!(numbers(m), vec![1, 0, 0, 1, 0, 0]);
    }

    #[test]
    fn page_to_form_xobject_present_non_numeric_userunit_scale_one() {
        // A present-but-non-numeric /UserUnit is non-null, so /Matrix is emitted;
        // qpdf uses scale 1.0 when /UserUnit is not a number.
        let mut pdf = open(one_page_doc("/UserUnit /X", "x", &[]));
        let xref = get_form_xobject_for_page(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        let stream = form_stream(&mut pdf, xref);
        let m = stream.dict.get("Matrix").unwrap().as_array().unwrap();
        assert_eq!(numbers(m), vec![1, 0, 0, 1, 0, 0]);
    }

    #[test]
    fn page_to_form_xobject_shallow_copies_indirect_group() {
        // Page with /Group as an indirect reference. qpdf shallowCopies it, so
        // the Form XObject's /Group is a DIRECT dictionary (not a reference) with
        // the original inner entries (observed in qpdf 11.9.0 overlay output).
        let group_obj = (6u32, "<< /Type /Group /S /Transparency /CS /DeviceRGB >>");
        let mut pdf = open(one_page_doc("/Group 6 0 R", "x", &[group_obj]));
        let xref = get_form_xobject_for_page(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        let stream = form_stream(&mut pdf, xref);
        let group = stream
            .dict
            .get("Group")
            .expect("Group must be copied")
            .as_dict()
            .expect("indirect /Group must be shallow-copied to a direct dict");
        assert_eq!(
            group.get("Type").unwrap().as_name(),
            Some(b"Group".to_vec())
        );
        assert_eq!(
            group.get("S").unwrap().as_name(),
            Some(b"Transparency".to_vec())
        );
        assert_eq!(
            group.get("CS").unwrap().as_name(),
            Some(b"DeviceRGB".to_vec())
        );
    }

    #[test]
    fn page_to_form_xobject_copies_direct_group_as_is() {
        // A direct /Group dictionary is copied unchanged.
        let mut pdf = open(one_page_doc(
            "/Group << /Type /Group /S /Transparency >>",
            "x",
            &[],
        ));
        let xref = get_form_xobject_for_page(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        let stream = form_stream(&mut pdf, xref);
        let group = stream.dict.get("Group").unwrap().as_dict().unwrap();
        assert_eq!(
            group.get("S").unwrap().as_name(),
            Some(b"Transparency".to_vec())
        );
    }

    #[test]
    fn page_to_form_xobject_rejects_non_page() {
        // Object 2 is /Type /Pages, not /Page -> content extraction fails.
        let mut pdf = open(one_page_doc("", "x", &[]));
        let err = get_form_xobject_for_page(&mut pdf, ObjectRef::new(2, 0));
        assert!(matches!(err, Err(Error::Unsupported(_))));
    }

    #[test]
    fn import_page_as_form_xobject_renumbers_foreign_refs() {
        // Source document with a page carrying a font; import into a fresh dest
        // and confirm the imported XObject's /Resources font resolves in dest.
        let mut source = open(one_page_doc("", "source content", &[]));
        let source_page = ObjectRef::new(3, 0);

        // Destination: a separate minimal document.
        let mut dest = open(build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>"),
            ],
            1,
        ));

        let imported = import_page_as_form_xobject(&mut dest, &mut source, source_page).unwrap();

        // The imported XObject exists in dest and is a Form stream.
        let stream = form_stream(&mut dest, imported);
        assert_eq!(
            stream.dict.get("Subtype").unwrap().as_name(),
            Some(b"Form".to_vec())
        );

        // /Resources/Font/F1 must be a reference into dest that resolves to a
        // font dictionary (foreign refs correctly renumbered).
        let res = stream.dict.get("Resources").unwrap().as_dict().unwrap();
        let font_dict = res.get("Font").unwrap().as_dict().unwrap();
        let font_ref = match font_dict.get("F1") {
            Some(handle) => handle
                .object_ref()
                .expect("F1 should be an indirect handle"),
            None => panic!("F1 should be a reference"), // cov:ignore: defensive — fixture guarantees a reference
        };
        let font_obj = dest.get_object_handle(font_ref);
        dest.resolve(&font_obj).unwrap();
        let font = TestHandle(font_obj)
            .as_dict()
            .expect("font ref resolves to a dict in dest");
        assert_eq!(font.get("Type").unwrap().as_name(), Some(b"Font".to_vec()));
        assert_eq!(
            font.get("BaseFont").unwrap().as_name(),
            Some(b"Helvetica".to_vec())
        );
    }

    #[test]
    fn import_pages_as_form_xobjects_shares_one_copy_of_a_common_resource() {
        // Two source pages that both reference the same font (F1 -> object 4).
        // Importing both through the same destination must reuse qpdf's
        // persistent foreign->local map, so the shared font is copied into
        // dest a single time and both imported XObjects' /Resources point at
        // the same dest object (per-source map reuse is qpdf's
        // copyForeignObject contract).
        let mut source = open(build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>"),
                (
                    3,
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                     /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>",
                ),
                (4, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
                (5, "<< /Length 3 >>\nstream\nfoo\nendstream"),
                (
                    6,
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                     /Resources << /Font << /F1 4 0 R >> >> /Contents 7 0 R >>",
                ),
                (7, "<< /Length 3 >>\nstream\nbar\nendstream"),
            ],
            1,
        ));

        let mut dest = open(build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>"),
            ],
            1,
        ));

        let imported = import_pages_as_form_xobjects(
            &mut dest,
            &mut source,
            &[ObjectRef::new(3, 0), ObjectRef::new(6, 0)],
        )
        .unwrap();
        assert_eq!(imported.len(), 2);

        let stream0 = form_stream(&mut dest, imported[0]);
        let res0 = stream0.dict.get("Resources").unwrap().as_dict().unwrap();
        let font_ref0 = match res0.get("Font").unwrap().as_dict().unwrap().get("F1") {
            Some(handle) => handle
                .object_ref()
                .expect("F1 should be an indirect handle"),
            None => panic!("F1 should be a reference"), // cov:ignore: defensive — fixture guarantees a reference
        };

        let stream1 = form_stream(&mut dest, imported[1]);
        let res1 = stream1.dict.get("Resources").unwrap().as_dict().unwrap();
        let font_ref1 = match res1.get("Font").unwrap().as_dict().unwrap().get("F1") {
            Some(handle) => handle
                .object_ref()
                .expect("F1 should be an indirect handle"),
            None => panic!("F1 should be a reference"), // cov:ignore: defensive — fixture guarantees a reference
        };

        assert_eq!(
            font_ref0, font_ref1,
            "the shared font must be copied into dest exactly once"
        );
        let font_obj = dest.get_object_handle(font_ref0);
        dest.resolve(&font_obj).unwrap();
        let font = TestHandle(font_obj)
            .as_dict()
            .expect("font ref resolves to a dict in dest");
        assert_eq!(
            font.get("BaseFont").unwrap().as_name(),
            Some(b"Helvetica".to_vec())
        );
    }

    // ---- helper-level coverage of defensive arms (direct calls) ----

    /// A minimal document whose object 3 resolves to a non-dictionary (an
    /// integer), used to exercise the "not a dictionary" guard arms.
    fn doc_with_non_dict_obj3() -> Pdf<std::io::Cursor<Vec<u8>>> {
        open(build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [] /Count 0 >>"),
                (3, "42"),
            ],
            1,
        ))
    }

    #[test]
    fn leaf_box_array_returns_none_for_non_dict() {
        let mut pdf = doc_with_non_dict_obj3();
        let got = leaf_box_array(&mut pdf, ObjectRef::new(3, 0), b"TrimBox").unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn inherited_box_array_returns_none_for_non_dict() {
        let mut pdf = doc_with_non_dict_obj3();
        let got = inherited_box_array(&mut pdf, ObjectRef::new(3, 0), b"MediaBox").unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn page_group_returns_none_for_non_dict() {
        let mut pdf = doc_with_non_dict_obj3();
        let got = page_group(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn inherited_box_array_breaks_on_parent_cycle() {
        // Two nodes whose /Parent points at each other, neither carrying the box
        // key -> the cycle guard returns None instead of looping forever.
        let mut pdf = open(build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (3, "<< /Type /Page /Parent 4 0 R >>"),
                (4, "<< /Type /Pages /Parent 3 0 R >>"),
            ],
            1,
        ));
        let got = inherited_box_array(&mut pdf, ObjectRef::new(3, 0), b"MediaBox").unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn inherited_box_array_skips_null_box_ref_and_climbs_to_parent() {
        // Leaf /CropBox is an indirect ref that resolves to null (treated as
        // absent): resolve_rect_array returns Ok(None), so the walk continues to
        // the parent, which has no box -> overall None. Exercises the null-ref
        // arm and the "present but None" fall-through.
        let mut pdf = open(build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /CropBox 4 0 R >>"),
                (4, "null"),
            ],
            1,
        ));
        let got = inherited_box_array(&mut pdf, ObjectRef::new(3, 0), b"CropBox").unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn inherited_box_array_errors_on_parent_chain_too_deep() {
        // A /Parent chain longer than DEFAULT_MAX_PAGE_TREE_DEPTH must error
        // rather than recurse unboundedly.
        let total = DEFAULT_MAX_PAGE_TREE_DEPTH + 5;
        let mut objs: Vec<(u32, String)> = vec![
            (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()),
        ];
        // Object 3 is the leaf; objects 3..total each point /Parent at the next.
        for n in 3..=(total as u32) {
            objs.push((n, format!("<< /Type /Pages /Parent {} 0 R >>", n + 1)));
        }
        let borrowed: Vec<(u32, &str)> = objs.iter().map(|(n, b)| (*n, b.as_str())).collect();
        let mut pdf = open(build_pdf(&borrowed, 1));
        let err = inherited_box_array(&mut pdf, ObjectRef::new(3, 0), b"MediaBox");
        assert!(matches!(err, Err(Error::Unsupported(_))));
    }

    #[test]
    fn resolve_rect_array_handles_ref_to_null() {
        let mut pdf = open(build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [] /Count 0 >>"),
                (3, "null"),
            ],
            1,
        ));
        let value = pdf.get_object_handle(ObjectRef::new(3, 0));
        let got = resolve_rect_array(&mut pdf, value, ObjectRef::new(1, 0), b"TrimBox").unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn resolve_rect_array_errors_on_non_array_non_null_value() {
        // A reference that resolves to a non-array, non-null value (an
        // integer here) is malformed input: neither the ref-to-array nor the
        // ref-to-null arm applies, so this must error rather than silently
        // treating the box as absent.
        let mut pdf = open(build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [] /Count 0 >>"),
                (3, "42"),
            ],
            1,
        ));
        let value = pdf.get_object_handle(ObjectRef::new(3, 0));
        let err = resolve_rect_array(&mut pdf, value, ObjectRef::new(1, 0), b"TrimBox");
        assert!(matches!(err, Err(Error::Unsupported(_))));
    }

    #[test]
    fn effective_box_array_prefers_trimbox_over_inherited_boxes() {
        // qpdf's getTrimBox(false) fallback chain: /TrimBox (leaf-only) wins
        // even when an inheritable /MediaBox is also present.
        let mut pdf = open(build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (
                    2,
                    "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>",
                ),
                (3, "<< /Type /Page /Parent 2 0 R /TrimBox [0 0 50 60] >>"),
            ],
            1,
        ));
        let got = effective_box_array(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        assert_eq!(got.len(), 4);
    }

    #[test]
    fn leaf_box_array_returns_none_for_absent_key() {
        let mut pdf = open(one_page_doc("", "x", &[]));
        let got = leaf_box_array(&mut pdf, ObjectRef::new(3, 0), b"TrimBox").unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn leaf_box_array_returns_none_for_direct_null() {
        let mut pdf = open(one_page_doc("/TrimBox null", "x", &[]));
        let got = leaf_box_array(&mut pdf, ObjectRef::new(3, 0), b"TrimBox").unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn leaf_box_array_returns_array_for_present_direct_box() {
        let mut pdf = open(one_page_doc("/TrimBox [0 0 100 200]", "x", &[]));
        let got = leaf_box_array(&mut pdf, ObjectRef::new(3, 0), b"TrimBox")
            .unwrap()
            .expect("a present direct box array must resolve");
        assert_eq!(got.len(), 4);
    }

    #[test]
    fn inherited_box_array_finds_box_on_ancestor() {
        // The leaf carries no /MediaBox; its /Pages parent does. The walk
        // climbs one hop and returns the ancestor's array.
        let mut pdf = open(build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (
                    2,
                    "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>",
                ),
                (3, "<< /Type /Page /Parent 2 0 R >>"),
            ],
            1,
        ));
        let got = inherited_box_array(&mut pdf, ObjectRef::new(3, 0), b"MediaBox")
            .unwrap()
            .expect("ancestor MediaBox must be found");
        assert_eq!(got.len(), 4);
    }

    #[test]
    fn page_group_returns_none_for_absent_group() {
        let mut pdf = open(one_page_doc("", "x", &[]));
        let got = page_group(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn page_group_returns_none_for_null_group() {
        let mut pdf = open(one_page_doc("/Group null", "x", &[]));
        let got = page_group(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn page_group_returns_a_direct_dict() {
        let mut pdf = open(one_page_doc(
            "/Group << /Type /Group /S /Transparency >>",
            "x",
            &[],
        ));
        let got = page_group(&mut pdf, ObjectRef::new(3, 0))
            .unwrap()
            .expect("a present direct /Group dict must be returned");
        assert!(got.as_dictionary().is_some());
    }

    #[test]
    fn page_group_shallow_copies_a_direct_group() {
        // qpdf calls getAttribute("/Group", false).shallowCopy()
        // unconditionally (libqpdf/QPDFPageObjectHelper.cc:715), not only
        // for an indirect value. Mutating the returned handle must not
        // perturb the page's own /Group dict -- they must not share
        // mutable identity.
        let mut pdf = open(one_page_doc(
            "/Group << /Type /Group /S /Transparency >>",
            "x",
            &[],
        ));
        let got = page_group(&mut pdf, ObjectRef::new(3, 0))
            .unwrap()
            .expect("a present direct /Group dict must be returned");
        got.replace_key(b"/S", ObjectHandle::name(b"Mutated".to_vec()))
            .unwrap();

        let page = pdf.get_object_handle(ObjectRef::new(3, 0));
        pdf.resolve(&page).unwrap();
        let original_group = page.get_key(b"/Group");
        let original_s = original_group.get_key(b"/S");
        assert_eq!(
            original_s.as_name(),
            Some(b"Transparency".to_vec()),
            "mutating the returned handle must not affect the page's own /Group"
        );
    }

    #[test]
    fn page_group_shallow_copies_indirect_group() {
        // qpdf's getFormXObjectForPage stores getAttribute("/Group", false)
        // .shallowCopy(): an indirect /Group is resolved one level into a
        // direct dictionary (libqpdf/QPDFPageObjectHelper.cc:706-733).
        let mut pdf = open(one_page_doc(
            "/Group 6 0 R",
            "x",
            &[(6, "<< /Type /Group /S /Transparency >>")],
        ));
        let got = page_group(&mut pdf, ObjectRef::new(3, 0))
            .unwrap()
            .expect("an indirect /Group must shallow-copy to a direct dict");
        assert!(got.is_direct());
        assert!(got.as_dictionary().is_some());
    }

    // ---- inherited_rotate_attribute (edge arms) ----------------------------

    #[test]
    fn inherited_rotate_attribute_returns_absent_for_non_dict() {
        let mut pdf = doc_with_non_dict_obj3();
        assert_eq!(
            inherited_rotate_attribute(&mut pdf, ObjectRef::new(3, 0)).unwrap(),
            (false, 0)
        );
    }

    #[test]
    fn inherited_rotate_attribute_resolves_indirect_reference() {
        // /Rotate stored as an indirect reference to an integer.
        let mut pdf = open(one_page_doc("/Rotate 6 0 R", "x", &[(6, "90")]));
        assert_eq!(
            inherited_rotate_attribute(&mut pdf, ObjectRef::new(3, 0)).unwrap(),
            (true, 90)
        );
    }

    #[test]
    fn inherited_rotate_attribute_treats_null_as_absent_and_climbs() {
        // Leaf /Rotate is null (equivalent to absent); the parent /Pages node has
        // no /Rotate either, so the walk reports absent.
        let mut pdf = open(one_page_doc("/Rotate null", "x", &[]));
        assert_eq!(
            inherited_rotate_attribute(&mut pdf, ObjectRef::new(3, 0)).unwrap(),
            (false, 0)
        );
    }

    #[test]
    fn inherited_rotate_attribute_present_non_integer_is_treated_as_present_zero() {
        // A present, non-null, non-integer /Rotate (a name here) is treated
        // as present with value 0, not as absent.
        let mut pdf = open(one_page_doc("/Rotate /Weird", "x", &[]));
        assert_eq!(
            inherited_rotate_attribute(&mut pdf, ObjectRef::new(3, 0)).unwrap(),
            (true, 0)
        );
    }

    #[test]
    fn inherited_rotate_attribute_breaks_on_parent_cycle() {
        // /Parent nodes point at each other; neither carries /Rotate.
        let mut pdf = open(build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (3, "<< /Type /Page /Parent 4 0 R >>"),
                (4, "<< /Type /Pages /Parent 3 0 R >>"),
            ],
            1,
        ));
        assert_eq!(
            inherited_rotate_attribute(&mut pdf, ObjectRef::new(3, 0)).unwrap(),
            (false, 0)
        );
    }

    #[test]
    fn inherited_rotate_attribute_errors_on_parent_chain_too_deep() {
        let total = DEFAULT_MAX_PAGE_TREE_DEPTH + 5;
        let mut objs: Vec<(u32, String)> = vec![
            (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()),
        ];
        for n in 3..=(total as u32) {
            objs.push((n, format!("<< /Type /Pages /Parent {} 0 R >>", n + 1)));
        }
        let borrowed: Vec<(u32, &str)> = objs.iter().map(|(n, b)| (*n, b.as_str())).collect();
        let mut pdf = open(build_pdf(&borrowed, 1));
        let err = inherited_rotate_attribute(&mut pdf, ObjectRef::new(3, 0));
        assert!(matches!(err, Err(Error::Unsupported(_))));
    }

    // ---- leaf_user_unit (edge arms) ----------------------------------------

    #[test]
    fn leaf_user_unit_returns_absent_for_non_dict() {
        let mut pdf = doc_with_non_dict_obj3();
        assert_eq!(
            leaf_user_unit(&mut pdf, ObjectRef::new(3, 0)).unwrap(),
            (false, 1.0)
        );
    }

    #[test]
    fn leaf_user_unit_resolves_indirect_reference() {
        let mut pdf = open(one_page_doc("/UserUnit 6 0 R", "x", &[(6, "3")]));
        assert_eq!(
            leaf_user_unit(&mut pdf, ObjectRef::new(3, 0)).unwrap(),
            (true, 3.0)
        );
    }

    #[test]
    fn leaf_user_unit_treats_null_as_absent() {
        let mut pdf = open(one_page_doc("/UserUnit null", "x", &[]));
        assert_eq!(
            leaf_user_unit(&mut pdf, ObjectRef::new(3, 0)).unwrap(),
            (false, 1.0)
        );
    }

    #[test]
    fn leaf_user_unit_reads_real_value() {
        let mut pdf = open(one_page_doc("/UserUnit 1.5", "x", &[]));
        assert_eq!(
            leaf_user_unit(&mut pdf, ObjectRef::new(3, 0)).unwrap(),
            (true, 1.5)
        );
    }

    // ---- get_matrix_for_transformations (invert scale==0 guard) ---------------------

    #[test]
    fn transformation_matrix_invert_zero_scale_is_identity() {
        // A /UserUnit 0 destination would invert to a 1/0 scale; qpdf guards this
        // by returning the identity.
        let t = PageTransform {
            rotate_present: false,
            rotate: 0,
            uu_present: true,
            scale: 0.0,
        };
        assert_eq!(
            get_matrix_for_transformations(&t, 612.0, 792.0, true),
            Matrix::default()
        );
    }
}
