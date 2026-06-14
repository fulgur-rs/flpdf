//! Apply overlay/underlay content to a destination page, mirroring qpdf's
//! `QPDFPageObjectHelper::placeFormXObject` and `QPDFJob::doUnderOverlayForPage`
//! (qpdf 11.9.0).
//!
//! Each destination page that receives at least one overlay or underlay is
//! rewritten as follows (see [`page_to_form_xobject`](crate::page_form_xobject)):
//!
//! 1. The destination page itself becomes a Form XObject named `/Fx0`.
//! 2. Each source (underlay or overlay) is a Form XObject already imported into
//!    the destination; it is named `/Fx1`, `/Fx2`, … in
//!    underlays-then-overlays declaration order (qpdf's
//!    `getUniqueResourceName("/Fx", …)`).
//! 3. The page `/Resources` is replaced with `<< /XObject << /Fx0 … /FxN >> >>`
//!    (the original resources now live inside `/Fx0`).
//! 4. The page `/Contents` is replaced with a single new stream that draws, in
//!    order, the underlays, then `/Fx0`, then the overlays. Each is placed with
//!    a `placeFormXObject` fragment: underlays/overlays into the destination
//!    `/TrimBox`, `/Fx0` into the destination `/MediaBox`.
//!
//! The placement matrix follows qpdf's `getMatrixForFormXObjectPlacement`:
//! scale-to-fit (never scaling up) and centring the transformed `/BBox` inside
//! the placement rectangle. Numbers are formatted like qpdf's
//! `QUtil::double_to_string` (`%.5f` with trailing zeros and a trailing `.`
//! stripped).

// The per-page apply entry point and its helpers are consumed by the
// overlay/underlay page-range mapping and CLI layers, which are not yet
// implemented. Until those call sites land, allow the unused-code lint at the
// module level (the public functions are exercised by unit tests here and the
// feature-gated byte-comparison test).
#![allow(dead_code)]

use std::io::{Read, Seek};

use crate::page_form_xobject::page_to_form_xobject;
use crate::page_object_helper::{PageBox, PageObjectHelper};
use crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result, Stream};

/// Whether a source page is drawn beneath (`Underlay`) or above (`Overlay`) the
/// destination page's own content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OverlayKind {
    /// Drawn beneath the destination page content (before `/Fx0`).
    Underlay,
    /// Drawn above the destination page content (after `/Fx0`).
    Overlay,
}

/// A single overlay/underlay source: a Form XObject already imported into the
/// destination document, plus whether it is an overlay or an underlay.
#[derive(Debug, Clone, Copy)]
pub(crate) struct OverlaySource {
    /// The source's kind (overlay or underlay).
    pub kind: OverlayKind,
    /// Reference to the imported Form XObject in the destination document.
    pub xobject_ref: ObjectRef,
}

/// Format `v` like qpdf's `QUtil::double_to_string` with five decimal places:
/// round to `%.5f`, then strip trailing zeros and a trailing `.`.
///
/// Examples: `1.0 -> "1"`, `0.0 -> "0"`, `155.5 -> "155.5"`,
/// `0.181818… -> "0.18182"`, `94.36364 -> "94.36364"`. Negative zero formats as
/// `"0"` (qpdf normalizes `-0`).
fn fmt_number(v: f64) -> String {
    // `%.5f` rounding. `format!("{:.5}", -0.0)` yields "-0.00000", which would
    // strip to "-0"; normalize the sign so a rounded-to-zero value is "0".
    let mut s = format!("{v:.5}");
    if s.starts_with("-0.00000") {
        s = "0.00000".to_string();
    }
    // Strip trailing zeros, then a trailing '.' if the fraction was all zeros.
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    trimmed.to_string()
}

/// Build a `placeFormXObject` content fragment placing the Form XObject named
/// `name` (whose `/BBox` is `bbox`) into the rectangle `rect`, mirroring qpdf's
/// `QPDFPageObjectHelper::placeFormXObject` +
/// `getMatrixForFormXObjectPlacement` (allow_shrink=true, allow_expand=false).
///
/// The fragment is exactly `"q\n" + matrix + " cm\n/" + name + " Do\nQ\n"`,
/// where `matrix` is the six space-separated components formatted by
/// [`fmt_number`].
fn place_form_xobject(bbox: [f64; 4], rect: [f64; 4], name: &str) -> String {
    let [bllx, blly, burx, bury] = bbox;
    let [rllx, rlly, rurx, rury] = rect;

    let bbox_w = burx - bllx;
    let bbox_h = bury - blly;
    let rect_w = rurx - rllx;
    let rect_h = rury - rlly;

    // Scale to fit: smaller of the x/y ratios. Guard against a zero-area /BBox
    // (qpdf would divide by zero); fall back to scale 1 so output stays finite.
    let scale = if bbox_w == 0.0 || bbox_h == 0.0 {
        1.0
    } else {
        let xscale = rect_w / bbox_w;
        let yscale = rect_h / bbox_h;
        let mut scale = xscale.min(yscale);
        // allow_expand defaults false: never scale up.
        if scale > 1.0 {
            scale = 1.0;
        }
        scale
    };

    // Centre the transformed /BBox in the rectangle. T = scale * bbox; the
    // translation moves the transformed bbox centre onto the rect centre.
    let t_cx = scale * (bllx + burx) / 2.0;
    let t_cy = scale * (blly + bury) / 2.0;
    let rect_cx = (rllx + rurx) / 2.0;
    let rect_cy = (rlly + rury) / 2.0;
    let tx = rect_cx - t_cx;
    let ty = rect_cy - t_cy;

    format!(
        "q\n{} 0 0 {} {} {} cm\n/{} Do\nQ\n",
        fmt_number(scale),
        fmt_number(scale),
        fmt_number(tx),
        fmt_number(ty),
        name,
    )
}

/// Apply an ordered list of overlay/underlay `sources` to the destination page
/// at `dest_page_ref`, mirroring qpdf's `QPDFJob::doUnderOverlayForPage`.
///
/// The destination page becomes Form XObject `/Fx0`; each source (already
/// imported into `dest` as a Form XObject) is named `/Fx1`, `/Fx2`, … in
/// underlays-then-overlays declaration order. The page `/Resources` is replaced
/// with `<< /XObject << /Fx0 … >> >>` and `/Contents` with one new stream that
/// draws the underlays, then `/Fx0`, then the overlays — underlays/overlays into
/// the page's `/TrimBox`, `/Fx0` into its `/MediaBox`. All other page keys are
/// preserved.
///
/// # Errors
///
/// - [`Error::Unsupported`] when `dest_page_ref` is not a `/Type /Page`
///   dictionary, when the page or a source XObject lacks a usable box, or when
///   the object-number space is exhausted while building `/Fx0`.
/// - Any error propagated from [`Pdf::resolve`] or page-to-XObject conversion.
pub(crate) fn apply_overlays_to_page<R: Read + Seek>(
    dest: &mut Pdf<R>,
    dest_page_ref: ObjectRef,
    sources: &[OverlaySource],
) -> Result<()> {
    // qpdf orders sources underlays-then-overlays for BOTH naming and drawing.
    let mut underlays: Vec<ObjectRef> = Vec::new();
    let mut overlays: Vec<ObjectRef> = Vec::new();
    for src in sources {
        match src.kind {
            OverlayKind::Underlay => underlays.push(src.xobject_ref),
            OverlayKind::Overlay => overlays.push(src.xobject_ref),
        }
    }

    // Destination placement rectangles, read before /Fx0 conversion mutates the
    // page dict (it does not touch the boxes, but reading first keeps the box
    // accessors operating on the original /Type /Page dictionary).
    let media_box = page_box_or_err(dest, dest_page_ref, BoxKind::Media)?;
    let trim_box = page_box_or_err(dest, dest_page_ref, BoxKind::Trim)?;

    // 1. Convert the destination page itself to Form XObject /Fx0.
    let fx0_ref = page_to_form_xobject(dest, dest_page_ref)?;

    // 2. Name the sources /Fx1.. in underlays-then-overlays order and build the
    //    new page /Resources /XObject mapping. /Fx0 is the page; the unique-name
    //    counter continues from there (getUniqueResourceName).
    let mut xobject_dict = Dictionary::new();
    xobject_dict.insert("Fx0", Object::Reference(fx0_ref));
    let mut next_index = 1u32;
    let mut underlay_names: Vec<(String, ObjectRef)> = Vec::new();
    let mut overlay_names: Vec<(String, ObjectRef)> = Vec::new();
    for xref in &underlays {
        let name = format!("Fx{next_index}");
        xobject_dict.insert(name.as_bytes(), Object::Reference(*xref));
        underlay_names.push((name, *xref));
        next_index += 1;
    }
    for xref in &overlays {
        let name = format!("Fx{next_index}");
        xobject_dict.insert(name.as_bytes(), Object::Reference(*xref));
        overlay_names.push((name, *xref));
        next_index += 1;
    }

    // 3. Build the new page /Contents in draw order: underlays -> /Fx0 ->
    //    overlays. Underlays/overlays place into the page /TrimBox; /Fx0 places
    //    into the page /MediaBox.
    let mut content = String::new();
    for (name, xref) in &underlay_names {
        let bbox = xobject_bbox(dest, *xref)?;
        content.push_str(&place_form_xobject(bbox, page_box_array(&trim_box), name));
    }
    {
        let bbox = xobject_bbox(dest, fx0_ref)?;
        content.push_str(&place_form_xobject(bbox, page_box_array(&media_box), "Fx0"));
    }
    for (name, xref) in &overlay_names {
        let bbox = xobject_bbox(dest, *xref)?;
        content.push_str(&place_form_xobject(bbox, page_box_array(&trim_box), name));
    }

    // 4. Allocate the new /Contents stream (uncompressed, no /Filter; the writer
    //    compresses on output).
    let contents_ref = next_object_ref(dest)?;
    let contents_stream = Stream::new(Dictionary::new(), content.into_bytes());
    dest.set_object(contents_ref, Object::Stream(contents_stream));

    // 5. Rewrite the page dictionary: replace /Resources and /Contents, keep all
    //    other keys.
    let mut page_dict = page_dictionary(dest, dest_page_ref)?;
    let mut resources = Dictionary::new();
    resources.insert("XObject", Object::Dictionary(xobject_dict));
    page_dict.insert("Resources", Object::Dictionary(resources));
    page_dict.insert("Contents", Object::Reference(contents_ref));
    dest.set_object(dest_page_ref, Object::Dictionary(page_dict));

    Ok(())
}

/// Which destination page box a placement rectangle comes from.
#[derive(Clone, Copy)]
enum BoxKind {
    Media,
    Trim,
}

/// Read the destination page's effective `/MediaBox` or `/TrimBox` (inheritance
/// and fallback resolved by [`PageObjectHelper`]), erroring when absent.
fn page_box_or_err<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    kind: BoxKind,
) -> Result<PageBox> {
    let mut helper = PageObjectHelper::new(page_ref, pdf);
    let opt = match kind {
        BoxKind::Media => helper.media_box()?,
        BoxKind::Trim => helper.trim_box()?,
    };
    opt.ok_or_else(|| {
        Error::Unsupported(format!(
            "destination page {page_ref} has no usable placement box"
        ))
    })
}

/// Convert a [`PageBox`] to the `[llx lly urx ury]` array `place_form_xobject`
/// consumes.
fn page_box_array(b: &PageBox) -> [f64; 4] {
    [b.llx, b.lly, b.urx, b.ury]
}

/// Read the imported Form XObject's `/BBox` as a numeric `[llx lly urx ury]`
/// array. Non-numeric elements contribute `0.0` (matching qpdf's numeric
/// coercion); an array shorter than four elements is an error.
fn xobject_bbox<R: Read + Seek>(pdf: &mut Pdf<R>, xobject_ref: ObjectRef) -> Result<[f64; 4]> {
    let obj = pdf.resolve(xobject_ref)?;
    let dict = match &obj {
        Object::Stream(s) => &s.dict,
        Object::Dictionary(d) => d,
        _ => {
            return Err(Error::Unsupported(format!(
                "Form XObject {xobject_ref} is not a stream or dictionary"
            )));
        }
    };
    let arr = dict.get("BBox").and_then(Object::as_array).ok_or_else(|| {
        Error::Unsupported(format!("Form XObject {xobject_ref} has no /BBox array"))
    })?;
    if arr.len() < 4 {
        return Err(Error::Unsupported(format!(
            "Form XObject {xobject_ref} /BBox has {} elements, expected 4",
            arr.len()
        )));
    }
    let n = |o: &Object| -> f64 {
        o.as_integer()
            .map(|i| i as f64)
            .or_else(|| o.as_real())
            .unwrap_or(0.0)
    };
    Ok([n(&arr[0]), n(&arr[1]), n(&arr[2]), n(&arr[3])])
}

/// Resolve `page_ref` to an owned page `Dictionary`, erroring when it is not a
/// dictionary.
fn page_dictionary<R: Read + Seek>(pdf: &mut Pdf<R>, page_ref: ObjectRef) -> Result<Dictionary> {
    match pdf.resolve(page_ref)? {
        Object::Dictionary(d) => Ok(d),
        _ => Err(Error::Unsupported(format!(
            "page {page_ref} is not a dictionary"
        ))),
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

// Feature-gated byte-identity gate: a single overlay applied to a destination
// page, written through the `--static-id` full-rewrite path, must be
// byte-identical to qpdf 11.9.0's overlay output. Gated on `qpdf-zlib-compat`
// because byte-identity requires flpdf's deflate output to match qpdf's classic
// libz output (see CLAUDE.md DEFLATE carve-out). It lives inside the crate, not
// in `tests/`, because the overlay entry points are `pub(crate)`.
#[cfg(all(test, feature = "qpdf-zlib-compat"))]
mod byte_gate {
    use super::{apply_overlays_to_page, OverlayKind, OverlaySource};
    use crate::page_form_xobject::import_page_as_form_xobject;
    use crate::pages::page_refs;
    use crate::{write_pdf_with_options, NewlineBeforeEndstream, Pdf, WriteOptions};
    use std::path::Path;

    fn fixture(name: &str) -> Pdf<std::io::BufReader<std::fs::File>> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat")
            .join(name);
        let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
        Pdf::open(std::io::BufReader::new(file)).unwrap()
    }

    fn golden() -> Vec<u8> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/golden/references/overlay/three-page-overlay-one-page.pdf");
        std::fs::read(&path).unwrap_or_else(|e| panic!("read golden {path:?}: {e}"))
    }

    /// Report the first differing byte offset for a readable failure message.
    fn first_diff(a: &[u8], b: &[u8]) -> Option<usize> {
        if a == b {
            return None;
        }
        let common = a.len().min(b.len());
        (0..common).find(|&i| a[i] != b[i]).or(Some(common))
    }

    #[test]
    fn three_page_overlay_one_page_is_byte_identical() {
        // dest = three-page.pdf, source = one-page.pdf.
        let mut dest = fixture("three-page.pdf");
        let mut source = fixture("one-page.pdf");

        let source_page = page_refs(&mut source).unwrap()[0];
        let dest_page = page_refs(&mut dest).unwrap()[0];

        // Import source page 1 into dest as a Form XObject, then apply it as a
        // single overlay onto dest page 1.
        let imported = import_page_as_form_xobject(&mut dest, &mut source, source_page).unwrap();
        apply_overlays_to_page(
            &mut dest,
            dest_page,
            &[OverlaySource {
                kind: OverlayKind::Overlay,
                xobject_ref: imported,
            }],
        )
        .unwrap();

        // Write through the same recipe as `flpdf rewrite --static-id`.
        let opts = WriteOptions {
            full_rewrite: true,
            static_id: true,
            newline_before_endstream: NewlineBeforeEndstream::Never,
            ..Default::default()
        };

        let mut actual = Vec::new();
        write_pdf_with_options(&mut dest, &mut actual, &opts).unwrap();

        let expected = golden();
        if let Some(off) = first_diff(&actual, &expected) {
            let lo = off.saturating_sub(24);
            let g = expected.get(off).copied().unwrap_or(0);
            let f = actual.get(off).copied().unwrap_or(0);
            panic!(
                "overlay output not byte-identical to qpdf golden \
                 (flpdf={} bytes, golden={} bytes)\n\
                 first diff at offset {off} (golden=0x{g:02x} flpdf=0x{f:02x})\n\
                 golden[{lo}..]: {:?}\nflpdf [{lo}..]: {:?}",
                actual.len(),
                expected.len(),
                String::from_utf8_lossy(&expected[lo..(off + 24).min(expected.len())]),
                String::from_utf8_lossy(&actual[lo..(off + 24).min(actual.len())]),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- fmt_number -------------------------------------------------------

    #[test]
    fn fmt_number_matches_qpdf_double_to_string() {
        assert_eq!(fmt_number(1.0), "1");
        assert_eq!(fmt_number(0.0), "0");
        assert_eq!(fmt_number(155.5), "155.5");
        assert_eq!(fmt_number(0.181818_1818), "0.18182");
        assert_eq!(fmt_number(94.363_636_36), "94.36364");
    }

    #[test]
    fn fmt_number_normalizes_negative_zero() {
        assert_eq!(fmt_number(-0.0), "0");
        // A small negative value that rounds to zero at 5dp also normalizes.
        assert_eq!(fmt_number(-0.000001), "0");
    }

    // ---- place_form_xobject ----------------------------------------------

    #[test]
    fn place_identity_when_same_size() {
        // BBox == rect (612x792 at origin) -> identity, centred.
        let frag = place_form_xobject([0.0, 0.0, 612.0, 792.0], [0.0, 0.0, 612.0, 792.0], "Fx0");
        assert_eq!(frag, "q\n1 0 0 1 0 0 cm\n/Fx0 Do\nQ\n");
    }

    #[test]
    fn place_centers_smaller_bbox_without_scaling() {
        // 300x144 source into 612x792 dest: no scale-up; centred at
        // tx = 306 - 150 = 156, ty = 396 - 72 = 324.
        let frag = place_form_xobject([0.0, 0.0, 300.0, 144.0], [0.0, 0.0, 612.0, 792.0], "Fx1");
        assert_eq!(frag, "q\n1 0 0 1 156 324 cm\n/Fx1 Do\nQ\n");
    }

    #[test]
    fn place_shrinks_larger_bbox_to_fit() {
        // 612x792 source into 300x144 dest: scale = min(300/612, 144/792)
        // = min(0.490196, 0.181818) = 0.18182 (5dp). tx = 150 - 0.18182*306,
        // ty = 72 - 0.18182*396 -> "94.36364" and "0".
        let frag = place_form_xobject([0.0, 0.0, 612.0, 792.0], [0.0, 0.0, 300.0, 144.0], "Fx0");
        assert_eq!(frag, "q\n0.18182 0 0 0.18182 94.36364 0 cm\n/Fx0 Do\nQ\n");
    }

    #[test]
    fn place_fractional_center() {
        // 301x145 source into 612x792 dest: no scale; tx = 306 - 150.5 = 155.5,
        // ty = 396 - 72.5 = 323.5.
        let frag = place_form_xobject([0.0, 0.0, 301.0, 145.0], [0.0, 0.0, 612.0, 792.0], "Fx2");
        assert_eq!(frag, "q\n1 0 0 1 155.5 323.5 cm\n/Fx2 Do\nQ\n");
    }

    #[test]
    fn place_handles_zero_area_bbox() {
        // A degenerate /BBox (zero width) must not divide by zero: scale falls
        // back to 1, centred on the rect.
        let frag = place_form_xobject([0.0, 0.0, 0.0, 100.0], [0.0, 0.0, 200.0, 200.0], "Fx1");
        // scale 1; t_cx = 0, t_cy = 50; rect centre (100,100): tx=100, ty=50.
        assert_eq!(frag, "q\n1 0 0 1 100 50 cm\n/Fx1 Do\nQ\n");
    }

    #[test]
    fn place_uses_nonzero_bbox_origin_center() {
        // /BBox origin is non-zero: centre uses (llx+urx)/2, (lly+ury)/2.
        // BBox [10 10 510 610] -> w=500 h=600 into rect [0 0 612 792].
        // scale = min(612/500, 792/600) = min(1.224, 1.32) -> clamped to 1.
        // t_cx = (10+510)/2 = 260, rect_cx = 306 -> tx = 46.
        // t_cy = (10+610)/2 = 310, rect_cy = 396 -> ty = 86.
        let frag = place_form_xobject([10.0, 10.0, 510.0, 610.0], [0.0, 0.0, 612.0, 792.0], "Fx0");
        assert_eq!(frag, "q\n1 0 0 1 46 86 cm\n/Fx0 Do\nQ\n");
    }

    // ---- apply_overlays_to_page ------------------------------------------

    /// Build a valid single-object-table PDF from `(number, body)` definitions
    /// plus a `/Root` number, computing xref offsets so the bytes parse. Object
    /// numbers must be contiguous starting at 1.
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

    fn open(bytes: Vec<u8>) -> Pdf<std::io::Cursor<Vec<u8>>> {
        Pdf::open_mem_owned(bytes).unwrap()
    }

    /// A one-page document with a font resource and one content stream. The page
    /// is object 3; its MediaBox is 612x792 (TrimBox absent -> falls back to
    /// MediaBox).
    fn one_page_doc(content: &str) -> Vec<u8> {
        let content_body = format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        );
        build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                     /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R /Rotate 0 >>",
                ),
                (4, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
                (5, &content_body),
            ],
            1,
        )
    }

    /// Insert a pre-built Form XObject (Subtype /Form, given /BBox) into `pdf`
    /// and return its ref. Mimics an already-imported overlay/underlay source.
    fn insert_form_xobject<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        bbox: [i64; 4],
        content: &[u8],
    ) -> ObjectRef {
        let mut dict = Dictionary::new();
        dict.insert("Type", Object::Name(b"XObject".to_vec()));
        dict.insert("Subtype", Object::Name(b"Form".to_vec()));
        dict.insert(
            "BBox",
            Object::Array(bbox.iter().map(|v| Object::Integer(*v)).collect()),
        );
        let r = next_object_ref(pdf).unwrap();
        pdf.set_object(r, Object::Stream(Stream::new(dict, content.to_vec())));
        r
    }

    #[test]
    fn apply_single_overlay_rebuilds_resources_and_contents() {
        let mut pdf = open(one_page_doc("page content"));
        let page_ref = ObjectRef::new(3, 0);
        let overlay = insert_form_xobject(&mut pdf, [0, 0, 612, 792], b"overlay content");

        apply_overlays_to_page(
            &mut pdf,
            page_ref,
            &[OverlaySource {
                kind: OverlayKind::Overlay,
                xobject_ref: overlay,
            }],
        )
        .unwrap();

        // Page /Resources == { /XObject { /Fx0, /Fx1 } } only.
        let page = pdf.resolve(page_ref).unwrap();
        let page_dict = page.as_dict().unwrap();
        let res = page_dict.get("Resources").unwrap().as_dict().unwrap();
        let keys: Vec<&[u8]> = res.iter().map(|(k, _)| k).collect();
        assert_eq!(keys, vec![b"XObject".as_slice()]);
        let xobj = res.get("XObject").unwrap().as_dict().unwrap();
        let xkeys: std::collections::BTreeSet<Vec<u8>> =
            xobj.iter().map(|(k, _)| k.to_vec()).collect();
        let expected: std::collections::BTreeSet<Vec<u8>> =
            [b"Fx0".to_vec(), b"Fx1".to_vec()].into_iter().collect();
        assert_eq!(xkeys, expected);
        // The original /Font is gone from the page (moved inside /Fx0).
        assert!(res.get("Font").is_none());

        // /Contents replaced with a new single stream (identity placements).
        let contents_ref = match page_dict.get("Contents") {
            Some(Object::Reference(r)) => *r,
            other => panic!("Contents should be a reference, got {other:?}"), // cov:ignore: defensive — apply always writes /Contents as a reference
        };
        let stream = pdf
            .resolve(contents_ref)
            .unwrap()
            .into_stream()
            .expect("Contents must be a stream");
        assert_eq!(
            stream.data,
            b"q\n1 0 0 1 0 0 cm\n/Fx0 Do\nQ\nq\n1 0 0 1 0 0 cm\n/Fx1 Do\nQ\n".to_vec()
        );
        // The 54-byte identity content (two 27-byte fragments).
        assert_eq!(stream.data.len(), 54);

        // Other page keys preserved.
        assert_eq!(
            page_dict.get("Type").unwrap().as_name(),
            Some(b"Page".as_slice())
        );
        assert!(page_dict.get("MediaBox").is_some());
        assert!(page_dict.get("Rotate").is_some());

        // /Fx0 is a Form XObject carrying the original page resources (font ref).
        let fx0_ref = match xobj.get("Fx0") {
            Some(Object::Reference(r)) => *r,
            other => panic!("Fx0 should be a reference, got {other:?}"), // cov:ignore: defensive — apply always inserts /Fx0 as a reference
        };
        let fx0 = pdf.resolve(fx0_ref).unwrap().into_stream().unwrap();
        assert_eq!(
            fx0.dict.get("Subtype").unwrap().as_name(),
            Some(b"Form".as_slice())
        );
        let fx0_res = fx0.dict.get("Resources").unwrap().as_dict().unwrap();
        assert!(fx0_res.get("Font").is_some(), "Fx0 keeps the page's /Font");
    }

    #[test]
    fn apply_orders_underlays_then_overlays_in_naming_and_drawing() {
        let mut pdf = open(one_page_doc("page content"));
        let page_ref = ObjectRef::new(3, 0);
        // Declaration order is overlay, underlay; qpdf groups
        // underlay-then-overlay for BOTH naming and drawing.
        let overlay = insert_form_xobject(&mut pdf, [0, 0, 612, 792], b"over");
        let underlay = insert_form_xobject(&mut pdf, [0, 0, 612, 792], b"under");

        apply_overlays_to_page(
            &mut pdf,
            page_ref,
            &[
                OverlaySource {
                    kind: OverlayKind::Overlay,
                    xobject_ref: overlay,
                },
                OverlaySource {
                    kind: OverlayKind::Underlay,
                    xobject_ref: underlay,
                },
            ],
        )
        .unwrap();

        let page = pdf.resolve(page_ref).unwrap();
        let page_dict = page.as_dict().unwrap();
        let res = page_dict.get("Resources").unwrap().as_dict().unwrap();
        let xobj = res.get("XObject").unwrap().as_dict().unwrap();

        // Underlay is named /Fx1 (first non-page name), overlay /Fx2.
        let fx1 = match xobj.get("Fx1") {
            Some(Object::Reference(r)) => *r,
            other => panic!("Fx1 missing: {other:?}"), // cov:ignore: defensive — apply names the first source /Fx1
        };
        let fx2 = match xobj.get("Fx2") {
            Some(Object::Reference(r)) => *r,
            other => panic!("Fx2 missing: {other:?}"), // cov:ignore: defensive — apply names the second source /Fx2
        };
        assert_eq!(fx1, underlay, "underlay must be /Fx1");
        assert_eq!(fx2, overlay, "overlay must be /Fx2");

        // Draw order: underlay (/Fx1) -> /Fx0 -> overlay (/Fx2).
        let contents_ref = match page_dict.get("Contents") {
            Some(Object::Reference(r)) => *r,
            other => panic!("Contents ref: {other:?}"), // cov:ignore: defensive — apply always writes /Contents as a reference
        };
        let stream = pdf.resolve(contents_ref).unwrap().into_stream().unwrap();
        let text = String::from_utf8(stream.data).unwrap();
        let fx1_pos = text.find("/Fx1 Do").unwrap();
        let fx0_pos = text.find("/Fx0 Do").unwrap();
        let fx2_pos = text.find("/Fx2 Do").unwrap();
        assert!(
            fx1_pos < fx0_pos && fx0_pos < fx2_pos,
            "draw order must be Fx1 (under) -> Fx0 (page) -> Fx2 (over): {text:?}"
        );
    }

    #[test]
    fn apply_places_fx0_in_mediabox_and_source_in_trimbox() {
        // Crafted dest with TrimBox != MediaBox pins the box-selection wiring:
        // /Fx0 (the page) places into the dest MediaBox; the source places into
        // the dest TrimBox (qpdf doUnderOverlayForPage). Expected matrices come
        // from the oracle's crafted fixture:
        //   /Fx0  BBox = dest TrimBox [10 10 500 600], rect = dest MediaBox
        //         -> scale 1, tx = 306-255 = 51, ty = 396-305 = 91
        //   src   BBox = src  TrimBox [20 20 220 100], rect = dest TrimBox
        //         -> scale 1, tx = 255-120 = 135, ty = 305-60 = 245
        let content_body = "<< /Length 1 >>\nstream\nx\nendstream";
        let mut pdf = open(build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                     /CropBox [0 0 600 700] /TrimBox [10 10 500 600] \
                     /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>",
                ),
                (4, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
                (5, content_body),
            ],
            1,
        ));
        let page_ref = ObjectRef::new(3, 0);
        // Source XObject /BBox = the source page's TrimBox.
        let src = insert_form_xobject(&mut pdf, [20, 20, 220, 100], b"src");

        apply_overlays_to_page(
            &mut pdf,
            page_ref,
            &[OverlaySource {
                kind: OverlayKind::Overlay,
                xobject_ref: src,
            }],
        )
        .unwrap();

        let page = pdf.resolve(page_ref).unwrap();
        let contents_ref = match page.as_dict().unwrap().get("Contents") {
            Some(Object::Reference(r)) => *r,
            other => panic!("Contents ref: {other:?}"), // cov:ignore: defensive — apply always writes /Contents as a reference
        };
        let stream = pdf.resolve(contents_ref).unwrap().into_stream().unwrap();
        let text = String::from_utf8(stream.data).unwrap();
        assert!(
            text.contains("q\n1 0 0 1 51 91 cm\n/Fx0 Do\nQ\n"),
            "Fx0 must place into the dest MediaBox: {text:?}"
        );
        assert!(
            text.contains("q\n1 0 0 1 135 245 cm\n/Fx1 Do\nQ\n"),
            "source must place into the dest TrimBox: {text:?}"
        );
    }

    #[test]
    fn apply_rejects_non_page() {
        // Object 2 is /Type /Pages, not /Page -> /Fx0 conversion fails.
        let mut pdf = open(one_page_doc("x"));
        let err = apply_overlays_to_page(&mut pdf, ObjectRef::new(2, 0), &[]);
        assert!(matches!(err, Err(Error::Unsupported(_))));
    }

    #[test]
    fn page_box_or_err_errors_when_box_absent() {
        // A /Type /Page with no /MediaBox (or any inheritable box) must error
        // instead of returning a placement rectangle.
        let mut pdf = open(build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (3, "<< /Type /Page /Parent 2 0 R >>"),
            ],
            1,
        ));
        let err = page_box_or_err(&mut pdf, ObjectRef::new(3, 0), BoxKind::Media);
        assert!(matches!(err, Err(Error::Unsupported(_))));
    }

    #[test]
    fn xobject_bbox_reads_real_and_integer_elements() {
        let mut pdf = open(one_page_doc("x"));
        let mut dict = Dictionary::new();
        dict.insert("Subtype", Object::Name(b"Form".to_vec()));
        dict.insert(
            "BBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Real(1.5),
                Object::Integer(300),
                Object::Real(144.0),
            ]),
        );
        let r = next_object_ref(&pdf).unwrap();
        pdf.set_object(r, Object::Stream(Stream::new(dict, Vec::new())));
        let bbox = xobject_bbox(&mut pdf, r).unwrap();
        assert_eq!(bbox, [0.0, 1.5, 300.0, 144.0]);
    }

    #[test]
    fn xobject_bbox_rejects_missing_and_short_box() {
        let mut pdf = open(one_page_doc("x"));
        // Missing /BBox.
        let mut d1 = Dictionary::new();
        d1.insert("Subtype", Object::Name(b"Form".to_vec()));
        let r1 = next_object_ref(&pdf).unwrap();
        pdf.set_object(r1, Object::Stream(Stream::new(d1, Vec::new())));
        assert!(matches!(
            xobject_bbox(&mut pdf, r1),
            Err(Error::Unsupported(_))
        ));

        // /BBox too short.
        let mut d2 = Dictionary::new();
        d2.insert("BBox", Object::Array(vec![Object::Integer(0)]));
        let r2 = next_object_ref(&pdf).unwrap();
        pdf.set_object(r2, Object::Stream(Stream::new(d2, Vec::new())));
        assert!(matches!(
            xobject_bbox(&mut pdf, r2),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn xobject_bbox_rejects_non_stream_non_dict() {
        let mut pdf = open(one_page_doc("x"));
        let r = next_object_ref(&pdf).unwrap();
        pdf.set_object(r, Object::Integer(42));
        assert!(matches!(
            xobject_bbox(&mut pdf, r),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn xobject_bbox_reads_from_plain_dictionary() {
        // A Form XObject value that is a bare dictionary (not a stream) still
        // yields its /BBox.
        let mut pdf = open(one_page_doc("x"));
        let mut d = Dictionary::new();
        d.insert(
            "BBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(10),
                Object::Integer(20),
            ]),
        );
        let r = next_object_ref(&pdf).unwrap();
        pdf.set_object(r, Object::Dictionary(d));
        assert_eq!(xobject_bbox(&mut pdf, r).unwrap(), [0.0, 0.0, 10.0, 20.0]);
    }

    #[test]
    fn page_dictionary_rejects_non_dict() {
        let mut pdf = open(one_page_doc("x"));
        let r = next_object_ref(&pdf).unwrap();
        pdf.set_object(r, Object::Integer(7));
        assert!(matches!(
            page_dictionary(&mut pdf, r),
            Err(Error::Unsupported(_))
        ));
    }
}
