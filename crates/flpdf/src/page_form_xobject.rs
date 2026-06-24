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

// The two entry points (`page_to_form_xobject`, `import_page_as_form_xobject`)
// and their private helpers are consumed by the overlay/underlay content-wiring
// and CLI layers, which are not yet implemented. Until those call sites land,
// allow the unused-code lint at the module level (exercised by unit tests here).
#![allow(dead_code)]

use std::collections::BTreeSet;
use std::io::{Read, Seek};

use crate::pages::{resolve_inherited_resources, DEFAULT_MAX_PAGE_TREE_DEPTH};
use crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result, Stream};

/// Maximum reference-chain depth when collecting a Form XObject's reachable
/// object closure. Mirrors the page-tree depth guard used elsewhere; bounds the
/// DFS so a malformed, deeply-nested document cannot overflow the stack.
const MAX_XOBJECT_CLOSURE_DEPTH: usize = DEFAULT_MAX_PAGE_TREE_DEPTH;

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
/// - [`Error::Unsupported`] when `page_ref` is not a `/Type /Page` dictionary,
///   when a required box rectangle is malformed, or when the object-number space
///   is exhausted.
/// - Any error propagated from [`Pdf::resolve`] or content extraction.
pub(crate) fn page_to_form_xobject<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<ObjectRef> {
    // /BBox = effective TrimBox (TrimBox -> CropBox -> MediaBox), copied verbatim
    // so the original integer/real element types are preserved (qpdf shallow-
    // copies getTrimBox(false)).
    let bbox = effective_box_array(pdf, page_ref)?;

    // /Matrix from /Rotate + /UserUnit (getMatrixForTransformations). qpdf emits
    // /Matrix ONLY when /Rotate (inherited) or /UserUnit (leaf) is present; width
    // and height come from the same TrimBox rectangle used for /BBox.
    let transform = read_page_transform(pdf, page_ref)?;
    let (bbox_w, bbox_h) = rectangle_dimensions(&bbox);

    // /Resources = effective (inheritance-resolved) resources, inserted as a
    // direct dictionary with inner references kept (qpdf shallowCopy semantics).
    let resources = resolve_inherited_resources(pdf, page_ref)?;

    // /Group shallow-copied from the page dict when present (an indirect ref is
    // materialized one level into a direct dict, mirroring qpdf's shallowCopy).
    let group = page_group(pdf, page_ref)?;

    // Stream data = decoded + coalesced page content, stored uncompressed.
    let content = crate::pages::page_content_bytes(pdf, page_ref)?;

    let mut dict = Dictionary::new();
    dict.insert("Type", Object::Name(b"XObject".to_vec()));
    dict.insert("Subtype", Object::Name(b"Form".to_vec()));
    dict.insert("BBox", Object::Array(bbox));
    if transform.rotate_present || transform.uu_present {
        let matrix = transformation_matrix(&transform, bbox_w, bbox_h, false);
        dict.insert("Matrix", Object::Array(matrix_objects(&matrix)));
    }
    if let Some(res) = resources {
        dict.insert("Resources", Object::Dictionary(res));
    }
    if let Some(g) = group {
        dict.insert("Group", g);
    }

    let new_ref = next_object_ref(pdf)?;
    pdf.set_object(new_ref, Object::Stream(Stream::new(dict, content)));
    Ok(new_ref)
}

/// Convert a page from `source` into a Form XObject and import it into `dest`.
///
/// Mirrors qpdf's `copyForeignObject` step in overlay/underlay: the page is
/// first wrapped into a Form XObject *inside `source`*, then the XObject and
/// every object it transitively references are deep-copied into `dest` via
/// [`copy_objects`](crate::object_copy::copy_objects). The returned
/// [`ObjectRef`] is the imported XObject's reference in `dest`.
///
/// # Errors
///
/// - [`Error::Unsupported`] when the source page cannot be converted, when the
///   reachable closure exceeds the depth guard, or when the destination object-
///   number space is exhausted.
/// - Any error propagated from [`Pdf::resolve`] or the cross-document copier.
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
    let xobject_ref = page_to_form_xobject(source, source_page_ref)?;

    // 2. Compute the XObject's reachable object closure.
    let closure = xobject_object_closure(source, xobject_ref)?;

    // 3. Deep-copy the closure into the destination, renumbering references.
    let map = crate::object_copy::copy_objects(source, dest, &closure)?;

    // The XObject ref is the DFS seed of the closure, so it is always present
    // in the copy map; the error arm is defensive and structurally unreachable.
    // cov:ignore-start: xobject_ref is always in the closure (the DFS seed), so
    // it is always a key in the copy map; this error arm cannot be reached.
    map.get(&xobject_ref).copied().ok_or_else(|| {
        Error::Unsupported("imported Form XObject reference missing from copy map".to_string())
    })
    // cov:ignore-end
}

/// Import several `source` pages into `dest` as Form XObjects in a single
/// cross-document copy, returning each page's imported XObject [`ObjectRef`] in
/// the same order as `source_page_refs`.
///
/// Unlike calling [`import_page_as_form_xobject`] once per page, this builds the
/// Form XObject for every page inside `source`, **unions** their reachable object
/// closures, and runs [`copy_objects`](crate::object_copy::copy_objects) exactly
/// once over that union. A single copy shares any indirect object referenced by
/// more than one page (a `/Font`, `/ProcSet`, image, …) instead of duplicating
/// it — matching qpdf's `copyForeignObject`, which keeps one foreign→local map
/// per source document. Per-page copies would emit a duplicate of every shared
/// resource, so the result would not be byte-identical to qpdf's overlay output.
///
/// `source_page_refs` should already be distinct; duplicate refs would request
/// the same imported XObject twice and are not deduplicated here.
///
/// # Errors
///
/// - [`Error::Unsupported`] when a source page cannot be converted, when a
///   reachable closure exceeds the depth guard, when the destination object-
///   number space is exhausted, or when an imported XObject is unexpectedly
///   absent from the copy map.
/// - Any error propagated from [`Pdf::resolve`] or the cross-document copier.
pub(crate) fn import_pages_as_form_xobjects<RS, RT>(
    dest: &mut Pdf<RT>,
    source: &mut Pdf<RS>,
    source_page_refs: &[ObjectRef],
) -> Result<Vec<ObjectRef>>
where
    RS: Read + Seek,
    RT: Read + Seek,
{
    // 1. Build a Form XObject inside `source` for each page and union the
    //    reachable object closures (a shared child object appears once in the set).
    let mut xobject_refs = Vec::with_capacity(source_page_refs.len());
    let mut union: BTreeSet<ObjectRef> = BTreeSet::new();
    for &page_ref in source_page_refs {
        let xobject_ref = page_to_form_xobject(source, page_ref)?;
        union.extend(xobject_object_closure(source, xobject_ref)?);
        xobject_refs.push(xobject_ref);
    }

    // 2. One copy over the union deduplicates shared child objects.
    let map = crate::object_copy::copy_objects(source, dest, &union)?;

    // 3. Map each per-page XObject seed to its imported destination ref.
    xobject_refs
        .iter()
        .map(|xref| {
            // Each XObject ref seeds its own closure, so it is always in the
            // union and thus in the copy map; the error arm is defensive.
            // cov:ignore-start: every seed is in the union -> in the copy map.
            map.get(xref).copied().ok_or_else(|| {
                Error::Unsupported(
                    "imported Form XObject reference missing from copy map".to_string(),
                )
            })
            // cov:ignore-end
        })
        .collect()
}

/// Resolve the page's effective box as a raw `Object::Array`, following qpdf's
/// `getTrimBox(false)` fallback chain: `/TrimBox` → `/CropBox` → `/MediaBox`.
///
/// The array is returned verbatim (original integer/real element types kept) so
/// `/BBox` is byte-identical to qpdf's shallow copy of the source rectangle.
///
/// `/TrimBox` is leaf-only (not inheritable, ISO 32000-1 Table 30), while
/// `/CropBox` and `/MediaBox` are inheritable and resolved through the `/Parent`
/// chain — matching qpdf's `getTrimBox`/`getCropBox`/`getMediaBox` fallback and
/// flpdf's own [`PageObjectHelper::crop_box`](crate::PageObjectHelper::crop_box).
fn effective_box_array<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<Vec<Object>> {
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
/// raw rectangle array. Returns `Ok(None)` when absent or null.
fn leaf_box_array<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    key: &[u8],
) -> Result<Option<Vec<Object>>> {
    let page_obj = pdf.resolve_borrowed(page_ref)?;
    let Some(dict) = page_obj.as_dict() else {
        return Ok(None);
    };
    let val = match dict.get(key).cloned() {
        None | Some(Object::Null) => return Ok(None),
        Some(v) => v,
    };
    resolve_rect_array(pdf, val, page_ref, key)
}

/// Walk the `/Parent` chain looking for an inheritable box `key` as a raw
/// rectangle array. Cycle- and depth-guarded.
fn inherited_box_array<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    key: &[u8],
) -> Result<Option<Vec<Object>>> {
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

        let node_obj = pdf.resolve_borrowed(current)?;
        let Some(dict) = node_obj.as_dict() else {
            return Ok(None);
        };
        // Per PDF §7.3.9 a null value is equivalent to the key being absent, so
        // skip it and climb to /Parent.
        let val = match dict.get(key).cloned() {
            Some(v) if !matches!(v, Object::Null) => Some(v),
            _ => None,
        };
        let parent_val = dict.get("Parent").cloned();

        if let Some(arr) = val.and_then(|v| resolve_rect_array(pdf, v, current, key).transpose()) {
            return Ok(Some(arr?));
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

/// Coerce a box value (a direct array or a reference to one) into a raw
/// rectangle array, validating it has at least four elements.
fn resolve_rect_array<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    val: Object,
    node: ObjectRef,
    key: &[u8],
) -> Result<Option<Vec<Object>>> {
    let arr = match val {
        Object::Array(arr) => arr,
        Object::Reference(r) => match pdf.resolve(r)? {
            Object::Array(arr) => arr,
            Object::Null => return Ok(None),
            _ => {
                return Err(Error::Unsupported(format!(
                    "/{} reference {r} on node {node} does not resolve to an array",
                    String::from_utf8_lossy(key)
                )));
            }
        },
        _ => {
            return Err(Error::Unsupported(format!(
                "/{} entry on node {node} is not a rectangle array",
                String::from_utf8_lossy(key)
            )));
        }
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

/// Compute the normalized `(width, height)` of a rectangle array, coercing each
/// numeric element to `f64`. Non-numeric elements contribute 0.0.
///
/// qpdf reads box geometry through `QPDFObjectHandle::getArrayAsRectangle`, which
/// normalizes corners with min/max, so the extents are always non-negative even for
/// a reversed box (`urx < llx` or `ury < lly`): `width = |urx - llx|`,
/// `height = |ury - lly|`.
fn rectangle_dimensions(arr: &[Object]) -> (f64, f64) {
    let n = |o: &Object| -> f64 {
        match o {
            Object::Integer(i) => *i as f64,
            Object::Real(r) => *r,
            _ => 0.0,
        }
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
            let node_obj = pdf.resolve_borrowed(current)?;
            let Some(dict) = node_obj.as_dict() else {
                return Ok((false, 0));
            };
            (dict.get("Rotate").cloned(), dict.get("Parent").cloned())
        };

        if let Some(val) = rotate_val {
            // /Rotate may be stored as an indirect reference; resolve it first.
            let resolved = match val {
                Object::Reference(r) => pdf.resolve(r)?,
                other => other,
            };
            match resolved {
                // Per PDF §7.3.9 a null value is equivalent to absent: climb on.
                Object::Null => {}
                Object::Integer(n) => return Ok((true, n as i32)),
                _ => return Ok((true, 0)),
            }
        }

        match parent_val {
            Some(Object::Reference(r)) => {
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
fn leaf_user_unit<R: Read + Seek>(pdf: &mut Pdf<R>, page_ref: ObjectRef) -> Result<(bool, f64)> {
    let uu_val = {
        let page_obj = pdf.resolve_borrowed(page_ref)?;
        let Some(dict) = page_obj.as_dict() else {
            return Ok((false, 1.0));
        };
        dict.get("UserUnit").cloned()
    };
    let Some(val) = uu_val else {
        return Ok((false, 1.0));
    };
    let resolved = match val {
        Object::Reference(r) => pdf.resolve(r)?,
        other => other,
    };
    match resolved {
        Object::Null => Ok((false, 1.0)),
        Object::Integer(n) => Ok((true, n as f64)),
        Object::Real(r) => Ok((true, r)),
        _ => Ok((true, 1.0)),
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
pub(crate) fn transformation_matrix(
    t: &PageTransform,
    width: f64,
    height: f64,
    invert: bool,
) -> [f64; 6] {
    const IDENTITY: [f64; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    if !(t.rotate_present || t.uu_present) {
        return IDENTITY;
    }
    let mut scale = t.scale;
    let mut rotate = t.rotate;
    if invert {
        if scale == 0.0 {
            return IDENTITY;
        }
        scale = 1.0 / scale;
        rotate = 360 - rotate;
    }
    match rotate {
        90 => [0.0, -scale, scale, 0.0, 0.0, width * scale],
        180 => [-scale, 0.0, 0.0, -scale, width * scale, height * scale],
        270 => [0.0, scale, -scale, 0.0, height * scale, 0.0],
        _ => [scale, 0.0, 0.0, scale, 0.0, 0.0],
    }
}

/// Convert a `[f64; 6]` matrix to a PDF array of [`Object::Real`], mirroring
/// qpdf's `QPDFObjectHandle::newReal`; whole values serialize without a decimal
/// point.
fn matrix_objects(m: &[f64; 6]) -> Vec<Object> {
    m.iter().map(|&x| Object::Real(x)).collect()
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
fn page_group<R: Read + Seek>(pdf: &mut Pdf<R>, page_ref: ObjectRef) -> Result<Option<Object>> {
    let group_val = {
        let page_obj = pdf.resolve_borrowed(page_ref)?;
        let Some(dict) = page_obj.as_dict() else {
            return Ok(None);
        };
        dict.get("Group").cloned()
    };
    match group_val {
        None | Some(Object::Null) => Ok(None),
        // shallowCopy: materialize the top level only (ref -> direct dict).
        Some(Object::Reference(r)) => Ok(Some(pdf.resolve(r)?)),
        Some(direct) => Ok(Some(direct)),
    }
}

/// Compute the transitive reachable object closure of the Form XObject at
/// `xobject_ref` (the XObject itself plus every object reachable through its
/// references). Bounded DFS with a visited set (cycle guard) and a depth limit.
fn xobject_object_closure<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    xobject_ref: ObjectRef,
) -> Result<BTreeSet<ObjectRef>> {
    let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut stack: Vec<(ObjectRef, usize)> = vec![(xobject_ref, 0)];
    visited.insert(xobject_ref);

    while let Some((current, depth)) = stack.pop() {
        if depth >= MAX_XOBJECT_CLOSURE_DEPTH {
            return Err(Error::Unsupported(format!(
                "Form XObject object graph exceeds depth {MAX_XOBJECT_CLOSURE_DEPTH} at {current}"
            )));
        }
        let obj = pdf.resolve(current)?;
        let mut refs = Vec::new();
        collect_object_refs(&obj, &mut refs);
        for r in refs {
            if visited.insert(r) {
                stack.push((r, depth + 1));
            }
        }
    }

    Ok(visited)
}

/// Collect every [`ObjectRef`] embedded directly in `obj` (one indirect object's
/// worth) into `out`. Stream payload bytes are opaque and never contain
/// references, so only the stream dictionary is traversed.
fn collect_object_refs(obj: &Object, out: &mut Vec<ObjectRef>) {
    match obj {
        Object::Reference(r) => out.push(*r),
        Object::Array(items) => {
            for item in items {
                collect_object_refs(item, out);
            }
        }
        Object::Dictionary(dict) => {
            for (_key, value) in dict.iter() {
                collect_object_refs(value, out);
            }
        }
        Object::Stream(stream) => {
            for (_key, value) in stream.dict.iter() {
                collect_object_refs(value, out);
            }
        }
        Object::Null
        | Object::Boolean(_)
        | Object::Integer(_)
        | Object::Real(_)
        | Object::Name(_)
        | Object::String(_) => {}
    }
}

/// Allocate the next available object reference (`max(numbers) + 1`, generation
/// 0), matching the allocation pattern used elsewhere in the crate.
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn form_stream<R: Read + Seek>(pdf: &mut Pdf<R>, xref: ObjectRef) -> Stream {
        pdf.resolve(xref)
            .unwrap()
            .into_stream()
            .expect("Form XObject must be a stream")
    }

    /// Coerce a rectangle/matrix array's numeric elements to `i64` for whole-
    /// number comparison (each element resolves via the numeric accessors).
    fn numbers(arr: &[Object]) -> Vec<i64> {
        arr.iter()
            .map(|o| {
                o.as_integer()
                    .or_else(|| o.as_real().map(|r| r as i64))
                    .expect("numeric array element")
            })
            .collect()
    }

    #[test]
    fn page_to_form_xobject_builds_expected_dict_and_stream() {
        let mut pdf = open(one_page_doc("", "Hello content", &[]));
        let page_ref = ObjectRef::new(3, 0);
        let expected_content = crate::pages::page_content_bytes(&mut pdf, page_ref).unwrap();

        let xref = page_to_form_xobject(&mut pdf, page_ref).unwrap();
        let stream = form_stream(&mut pdf, xref);
        let dict = &stream.dict;

        // Exact key set: BBox, Resources, Subtype, Type. NO FormType, and NO
        // /Matrix because this page carries neither /Rotate nor /UserUnit (qpdf's
        // getFormXObjectForPage omits /Matrix in that case).
        let keys: BTreeSet<Vec<u8>> = dict.iter().map(|(k, _)| k.to_vec()).collect();
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
            Some(b"Form".as_slice())
        );
        assert_eq!(
            dict.get("Type").unwrap().as_name(),
            Some(b"XObject".as_slice())
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
        let xref = page_to_form_xobject(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        let stream = form_stream(&mut pdf, xref);
        let bbox = stream.dict.get("BBox").unwrap().as_array().unwrap();
        // Verbatim copy: integers stay integers, real stays real.
        assert!(matches!(bbox[0], Object::Integer(10)));
        assert!(matches!(bbox[1], Object::Integer(10)));
        assert!(matches!(bbox[2], Object::Real(v) if (v - 500.5).abs() < 1e-9));
        assert!(matches!(bbox[3], Object::Integer(600)));
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
        let xref = page_to_form_xobject(&mut pdf, ObjectRef::new(3, 0)).unwrap();
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
        let xref = page_to_form_xobject(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        let stream = form_stream(&mut pdf, xref);
        let bbox = stream.dict.get("BBox").unwrap().as_array().unwrap();
        assert_eq!(numbers(bbox), vec![1, 2, 3, 4]);
    }

    #[test]
    fn page_to_form_xobject_rejects_short_box_array() {
        // A rectangle with < 4 elements is malformed.
        let mut pdf = open(one_page_doc("/TrimBox [0 0 5]", "x", &[]));
        let err = page_to_form_xobject(&mut pdf, ObjectRef::new(3, 0));
        assert!(matches!(err, Err(Error::Unsupported(_))));
    }

    #[test]
    fn page_to_form_xobject_rejects_non_array_box() {
        // A /TrimBox that is a name, not an array, is malformed.
        let mut pdf = open(one_page_doc("/TrimBox /NotARect", "x", &[]));
        let err = page_to_form_xobject(&mut pdf, ObjectRef::new(3, 0));
        assert!(matches!(err, Err(Error::Unsupported(_))));
    }

    #[test]
    fn page_to_form_xobject_rejects_box_ref_to_non_array() {
        // /CropBox is an indirect ref that resolves to a dictionary, not an
        // array — exercises the reference-arm error path in inherited_box_array.
        let bad = (6u32, "<< /Type /Foo >>");
        let mut pdf = open(one_page_doc("/CropBox 6 0 R", "x", &[bad]));
        let err = page_to_form_xobject(&mut pdf, ObjectRef::new(3, 0));
        assert!(matches!(err, Err(Error::Unsupported(_))));
    }

    #[test]
    fn page_to_form_xobject_errors_when_no_box_anywhere() {
        // No /TrimBox, /CropBox, or /MediaBox on the page or its parent.
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
        let err = page_to_form_xobject(&mut pdf, ObjectRef::new(3, 0));
        assert!(matches!(err, Err(Error::Unsupported(_))));
    }

    #[test]
    fn page_to_form_xobject_handles_non_numeric_box_element() {
        // A rectangle with a non-numeric element: rectangle_dimensions treats it
        // as 0.0 (the array is still copied verbatim into /BBox). /Rotate 0 keeps
        // /Matrix present so the identity assertion is exercised.
        let mut pdf = open(one_page_doc("/TrimBox [0 0 /X 100] /Rotate 0", "x", &[]));
        let xref = page_to_form_xobject(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        let stream = form_stream(&mut pdf, xref);
        let bbox = stream.dict.get("BBox").unwrap().as_array().unwrap();
        assert!(matches!(bbox[2], Object::Name(_)));
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
            Object::Integer(612),
            Object::Integer(792),
            Object::Integer(0),
            Object::Integer(0),
        ];
        assert_eq!(rectangle_dimensions(&swapped), (612.0, 792.0));
        let ordered = [
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(612),
            Object::Integer(792),
        ];
        assert_eq!(rectangle_dimensions(&ordered), (612.0, 792.0));
    }

    #[test]
    fn page_to_form_xobject_rotate_90_matrix() {
        // MediaBox 0 0 612 792, /Rotate 90 -> matrix [0 -1 1 0 0 width].
        let mut pdf = open(one_page_doc("/Rotate 90", "x", &[]));
        let xref = page_to_form_xobject(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        let stream = form_stream(&mut pdf, xref);
        let matrix = stream.dict.get("Matrix").unwrap().as_array().unwrap();
        // width = urx - llx = 612 - 0 = 612.
        assert_eq!(numbers(matrix), vec![0, -1, 1, 0, 0, 612]);
    }

    #[test]
    fn page_to_form_xobject_rotate_180_and_270() {
        // 180 -> [-1 0 0 -1 width height]; 270 -> [0 1 -1 0 height 0].
        let mut pdf = open(one_page_doc("/Rotate 180", "x", &[]));
        let xref = page_to_form_xobject(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        let stream = form_stream(&mut pdf, xref);
        let m = stream.dict.get("Matrix").unwrap().as_array().unwrap();
        assert_eq!(numbers(m), vec![-1, 0, 0, -1, 612, 792]);

        let mut pdf = open(one_page_doc("/Rotate 270", "x", &[]));
        let xref = page_to_form_xobject(&mut pdf, ObjectRef::new(3, 0)).unwrap();
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
        let xref = page_to_form_xobject(&mut pdf, ObjectRef::new(3, 0)).unwrap();
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
        let xref = page_to_form_xobject(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        let stream = form_stream(&mut pdf, xref);
        let m = stream.dict.get("Matrix").unwrap().as_array().unwrap();
        assert_eq!(numbers(m), vec![0, -1, 1, 0, 0, 612]);
    }

    #[test]
    fn page_to_form_xobject_userunit_only_emits_scale_matrix() {
        // /UserUnit 2 with no /Rotate: qpdf emits /Matrix with the scale folded in
        // (rotate-0 default branch -> [scale 0 0 scale 0 0]).
        let mut pdf = open(one_page_doc("/UserUnit 2", "x", &[]));
        let xref = page_to_form_xobject(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        let stream = form_stream(&mut pdf, xref);
        let m = stream.dict.get("Matrix").unwrap().as_array().unwrap();
        assert_eq!(numbers(m), vec![2, 0, 0, 2, 0, 0]);
    }

    #[test]
    fn page_to_form_xobject_userunit_and_rotate_90() {
        // /UserUnit 2 /Rotate 90 on 612x792 -> [0 -2 2 0 0 width*scale=1224].
        let mut pdf = open(one_page_doc("/UserUnit 2 /Rotate 90", "x", &[]));
        let xref = page_to_form_xobject(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        let stream = form_stream(&mut pdf, xref);
        let m = stream.dict.get("Matrix").unwrap().as_array().unwrap();
        assert_eq!(numbers(m), vec![0, -2, 2, 0, 0, 1224]);
    }

    #[test]
    fn page_to_form_xobject_present_non_integer_rotate_emits_identity() {
        // A present-but-non-integer /Rotate is non-null, so /Matrix is emitted;
        // qpdf treats a non-integer rotation as 0 -> identity.
        let mut pdf = open(one_page_doc("/Rotate /X", "x", &[]));
        let xref = page_to_form_xobject(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        let stream = form_stream(&mut pdf, xref);
        let m = stream.dict.get("Matrix").unwrap().as_array().unwrap();
        assert_eq!(numbers(m), vec![1, 0, 0, 1, 0, 0]);
    }

    #[test]
    fn page_to_form_xobject_present_non_numeric_userunit_scale_one() {
        // A present-but-non-numeric /UserUnit is non-null, so /Matrix is emitted;
        // qpdf uses scale 1.0 when /UserUnit is not a number.
        let mut pdf = open(one_page_doc("/UserUnit /X", "x", &[]));
        let xref = page_to_form_xobject(&mut pdf, ObjectRef::new(3, 0)).unwrap();
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
        let xref = page_to_form_xobject(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        let stream = form_stream(&mut pdf, xref);
        let group = stream
            .dict
            .get("Group")
            .expect("Group must be copied")
            .as_dict()
            .expect("indirect /Group must be shallow-copied to a direct dict");
        assert_eq!(
            group.get("Type").unwrap().as_name(),
            Some(b"Group".as_slice())
        );
        assert_eq!(
            group.get("S").unwrap().as_name(),
            Some(b"Transparency".as_slice())
        );
        assert_eq!(
            group.get("CS").unwrap().as_name(),
            Some(b"DeviceRGB".as_slice())
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
        let xref = page_to_form_xobject(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        let stream = form_stream(&mut pdf, xref);
        let group = stream.dict.get("Group").unwrap().as_dict().unwrap();
        assert_eq!(
            group.get("S").unwrap().as_name(),
            Some(b"Transparency".as_slice())
        );
    }

    #[test]
    fn page_to_form_xobject_rejects_non_page() {
        // Object 2 is /Type /Pages, not /Page -> content extraction fails.
        let mut pdf = open(one_page_doc("", "x", &[]));
        let err = page_to_form_xobject(&mut pdf, ObjectRef::new(2, 0));
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
            Some(b"Form".as_slice())
        );

        // /Resources/Font/F1 must be a reference into dest that resolves to a
        // font dictionary (foreign refs correctly renumbered).
        let res = stream.dict.get("Resources").unwrap().as_dict().unwrap();
        let font_dict = res.get("Font").unwrap().as_dict().unwrap();
        let font_ref = match font_dict.get("F1") {
            Some(Object::Reference(r)) => *r,
            other => panic!("F1 should be a reference, got {other:?}"), // cov:ignore: defensive — fixture guarantees a reference
        };
        let font_obj = dest.resolve(font_ref).unwrap();
        let font = font_obj
            .as_dict()
            .expect("font ref resolves to a dict in dest");
        assert_eq!(
            font.get("Type").unwrap().as_name(),
            Some(b"Font".as_slice())
        );
        assert_eq!(
            font.get("BaseFont").unwrap().as_name(),
            Some(b"Helvetica".as_slice())
        );
    }

    #[test]
    fn xobject_object_closure_handles_cyclic_references() {
        // Two objects referencing each other plus the XObject -> the cycle guard
        // (visited set) must terminate and include both.
        let mut pdf = open(build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] \
                     /Resources << /A 4 0 R >> >>",
                ),
                (4, "<< /Peer 5 0 R >>"),
                (5, "<< /Peer 4 0 R >>"),
            ],
            1,
        ));
        let xref = page_to_form_xobject(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        let closure = xobject_object_closure(&mut pdf, xref).unwrap();
        assert!(closure.contains(&xref));
        assert!(closure.contains(&ObjectRef::new(4, 0)));
        assert!(closure.contains(&ObjectRef::new(5, 0)));
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
        let got = resolve_rect_array(
            &mut pdf,
            Object::Reference(ObjectRef::new(3, 0)),
            ObjectRef::new(1, 0),
            b"TrimBox",
        )
        .unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn xobject_object_closure_errors_when_ref_chain_too_deep() {
        // A linear reference chain deeper than MAX_XOBJECT_CLOSURE_DEPTH must
        // error rather than overflow the stack.
        let total = MAX_XOBJECT_CLOSURE_DEPTH + 5;
        let mut objs: Vec<(u32, String)> = vec![
            (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] \
                 /Resources << /A 4 0 R >> >>"
                    .to_string(),
            ),
        ];
        // Objects 4..total form a linear /Next chain off the page's resources.
        for n in 4..=(total as u32) {
            objs.push((n, format!("<< /Next {} 0 R >>", n + 1)));
        }
        objs.push(((total + 1) as u32, "<< /Leaf true >>".to_string()));
        let borrowed: Vec<(u32, &str)> = objs.iter().map(|(n, b)| (*n, b.as_str())).collect();
        let mut pdf = open(build_pdf(&borrowed, 1));
        let xref = page_to_form_xobject(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        let err = xobject_object_closure(&mut pdf, xref);
        assert!(matches!(err, Err(Error::Unsupported(_))));
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

    // ---- transformation_matrix (invert scale==0 guard) ---------------------

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
            transformation_matrix(&t, 612.0, 792.0, true),
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]
        );
    }
}
