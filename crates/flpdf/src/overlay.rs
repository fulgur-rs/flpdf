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

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

use crate::page_form_xobject::{page_to_form_xobject, read_page_transform, transformation_matrix};
use crate::page_object_helper::{PageBox, PageObjectHelper};
use crate::page_range::PageRange;
use crate::pages::page_refs;
use crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result, Stream};

/// Whether a source page is drawn beneath (`Underlay`) or above (`Overlay`) the
/// destination page's own content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayKind {
    /// Drawn beneath the destination page content (before `/Fx0`).
    Underlay,
    /// Drawn above the destination page content (after `/Fx0`).
    Overlay,
}

/// A single overlay/underlay source: a Form XObject already imported into the
/// destination document, plus whether it is an overlay or an underlay and an
/// optional `AnnotationCopyTemplate` that names the dest-side annotation refs
/// to duplicate at placement time (populated only when the source page has
/// `/Annots`; `None` otherwise so annotation-less sources cost nothing).
#[derive(Debug, Clone)]
pub(crate) struct OverlaySource {
    /// The source's kind (overlay or underlay).
    pub kind: OverlayKind,
    /// Reference to the imported Form XObject in the destination document.
    pub xobject_ref: ObjectRef,
    /// Template refs for per-placement annotation duplication (qpdf's
    /// `dest_page.copyAnnotations(from_page, cm, ...)` step). `None` when the
    /// source page has no `/Annots` and the placement should skip the
    /// annotation phase entirely.
    pub annot_template: Option<crate::overlay_annotations::AnnotationCopyTemplate>,
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

/// The identity transformation matrix `[1 0 0 1 0 0]`.
const IDENTITY_MATRIX: [f64; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// Multiply two transformation matrices, mirroring qpdf's `QPDFMatrix::concat`
/// (`this.concat(other)`) byte-for-byte so the floating-point result matches
/// qpdf's exactly.
fn qpdf_concat(this: [f64; 6], other: [f64; 6]) -> [f64; 6] {
    let [a, b, c, d, e, f] = this;
    let [oa, ob, oc, od, oe, of] = other;
    [
        a * oa + c * ob,
        b * oa + d * ob,
        a * oc + c * od,
        b * oc + d * od,
        a * oe + c * of + e,
        b * oe + d * of + f,
    ]
}

/// `this.scale(sx, sy)` — qpdf concatenates a scaling matrix on the right.
fn qpdf_scale(this: [f64; 6], sx: f64, sy: f64) -> [f64; 6] {
    qpdf_concat(this, [sx, 0.0, 0.0, sy, 0.0, 0.0])
}

/// `this.translate(tx, ty)` — qpdf concatenates a translation matrix on the right.
fn qpdf_translate(this: [f64; 6], tx: f64, ty: f64) -> [f64; 6] {
    qpdf_concat(this, [1.0, 0.0, 0.0, 1.0, tx, ty])
}

/// Round a matrix component the way qpdf's `QPDFMatrix::unparse` does before
/// formatting: values in `(-0.00001, 0.00001)` collapse to `0.0`.
fn fix_rounding(d: f64) -> f64 {
    if d > -0.00001 && d < 0.00001 {
        0.0
    } else {
        d
    }
}

/// Serialize a transformation matrix the way qpdf's `QPDFMatrix::unparse` does:
/// `fix_rounding` each of the six components, then format with [`fmt_number`]
/// (qpdf's `QUtil::double_to_string(..., 5)`), space-separated.
fn matrix_unparse(m: [f64; 6]) -> String {
    let parts: Vec<String> = m.iter().map(|&x| fmt_number(fix_rounding(x))).collect();
    parts.join(" ")
}

/// Compute the placement matrix that lands the Form XObject (`/BBox` `fo_bbox`,
/// `/Matrix` `fo_matrix`) inside `rect`, mirroring qpdf's
/// `getMatrixForFormXObjectPlacement` (qpdf 11.9.0) exactly.
///
/// `tmatrix` is the destination page's inverse transform
/// (`getMatrixForTransformations(true)`, the identity when the dest page has no
/// `/Rotate`/`/UserUnit`); it is always concatenated, matching qpdf's
/// `invert_transformations=true` call sites. `allow_shrink`/`allow_expand` gate
/// whether the scale-to-fit factor may drop below or rise above 1.0.
///
/// Returns `None` when the matrix-transformed `/BBox` is degenerate (zero width
/// or height); the caller substitutes the identity, matching qpdf's `{}`.
fn matrix_for_form_xobject_placement(
    fo_bbox: [f64; 4],
    fo_matrix: [f64; 6],
    rect: [f64; 4],
    tmatrix: [f64; 6],
    allow_shrink: bool,
    allow_expand: bool,
) -> Option<[f64; 6]> {
    // wmatrix = I.concat(tmatrix).concat(fmatrix). tmatrix is identity (a no-op)
    // when the dest page has no transform; fmatrix is identity when the fo has no
    // /Matrix — both still concatenated, matching qpdf.
    let wmatrix = qpdf_concat(qpdf_concat(IDENTITY_MATRIX, tmatrix), fo_matrix);
    let t = transform_bbox(fo_bbox, wmatrix);
    let [t_llx, t_lly, t_urx, t_ury] = t;
    if t_urx == t_llx || t_ury == t_lly {
        return None;
    }
    let [rllx, rlly, rurx, rury] = rect;
    let rect_w = rurx - rllx;
    let rect_h = rury - rlly;
    let t_w = t_urx - t_llx;
    let t_h = t_ury - t_lly;
    let xscale = rect_w / t_w;
    let yscale = rect_h / t_h;
    let mut scale = if xscale < yscale { xscale } else { yscale };
    if scale > 1.0 {
        if !allow_expand {
            scale = 1.0;
        }
    } else if scale < 1.0 && !allow_shrink {
        scale = 1.0;
    }

    // Re-measure the scaled box to find the centring translation.
    let wmatrix = qpdf_concat(
        qpdf_concat(qpdf_scale(IDENTITY_MATRIX, scale, scale), tmatrix),
        fo_matrix,
    );
    let t = transform_bbox(fo_bbox, wmatrix);
    let [t_llx, t_lly, t_urx, t_ury] = t;
    let t_cx = (t_llx + t_urx) / 2.0;
    let t_cy = (t_lly + t_ury) / 2.0;
    let r_cx = (rllx + rurx) / 2.0;
    let r_cy = (rlly + rury) / 2.0;
    let tx = r_cx - t_cx;
    let ty = r_cy - t_cy;

    // cm = I.translate(tx, ty).scale(scale, scale).concat(tmatrix). The fmatrix is
    // deliberately absent: the PDF interpreter applies the fo's /Matrix itself.
    let cm = qpdf_concat(
        qpdf_scale(qpdf_translate(IDENTITY_MATRIX, tx, ty), scale, scale),
        tmatrix,
    );
    Some(cm)
}

/// Build a `placeFormXObject` content fragment placing the Form XObject named
/// `name` into `rect`, mirroring qpdf's `QPDFPageObjectHelper::placeFormXObject`
/// (qpdf 11.9.0). A degenerate placement matrix collapses to the identity, as in
/// qpdf.
///
/// Returns `(fragment, cm)`: the fragment is exactly
/// `"q\n" + cm + " cm\n/" + name + " Do\nQ\n"` (with `cm` formatted by
/// [`matrix_unparse`]); `cm` is the same six-component matrix used to place the
/// XObject. Callers that transform per-placement annotations (qpdf's
/// `copyAnnotations(from_page, cm, …)`) use the returned `cm` unchanged.
fn place_form_xobject(
    fo_bbox: [f64; 4],
    fo_matrix: [f64; 6],
    rect: [f64; 4],
    tmatrix: [f64; 6],
    allow_shrink: bool,
    allow_expand: bool,
    name: &str,
) -> (String, [f64; 6]) {
    let cm = matrix_for_form_xobject_placement(
        fo_bbox,
        fo_matrix,
        rect,
        tmatrix,
        allow_shrink,
        allow_expand,
    )
    .unwrap_or(IDENTITY_MATRIX);
    let fragment = format!("q\n{} cm\n/{} Do\nQ\n", matrix_unparse(cm), name);
    (fragment, cm)
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
    // Build the two typed Vecs in a single pass over `sources` in encounter
    // order — each kind is appended independently, so relative order within a
    // kind is preserved. The paint order (underlays first, then overlays) is
    // enforced below when we consume `underlays` before `overlays` while
    // building the /Fx1.. names and the new content stream.
    //
    // Each entry carries the source's `annot_template` alongside the imported
    // XObject ref so that per placement we can call
    // `overlay_annotations::apply_placement(dest, dest_page_ref, template, cm, dr)`
    // right after `place_form_xobject` returns `cm`, mirroring qpdf's
    // `dest_page.copyAnnotations(from_page, cm, dest_afdh, from_afdh)` call at
    // `QPDFJob::doUnderOverlayForPage` line 1899.
    type PlacementEntry = (
        ObjectRef,
        Option<crate::overlay_annotations::AnnotationCopyTemplate>,
    );
    let mut underlays: Vec<PlacementEntry> = Vec::new();
    let mut overlays: Vec<PlacementEntry> = Vec::new();
    for src in sources {
        let entry = (src.xobject_ref, src.annot_template.clone());
        match src.kind {
            OverlayKind::Underlay => underlays.push(entry),
            OverlayKind::Overlay => overlays.push(entry),
        }
    }

    // Destination placement rectangles, read before /Fx0 conversion mutates the
    // page dict (it does not touch the boxes, but reading first keeps the box
    // accessors operating on the original /Type /Page dictionary).
    let media_box = page_box_or_err(dest, dest_page_ref, BoxKind::Media)?;
    let trim_box = page_box_or_err(dest, dest_page_ref, BoxKind::Trim)?;

    // The destination page's inverse transform, folded into every placement
    // (qpdf's placeFormXObject is called with invert_transformations=true for both
    // /Fx0 and the sources). Width/height come from the dest /TrimBox, matching
    // qpdf's getMatrixForTransformations(true). Identity when the dest page has no
    // /Rotate or /UserUnit, so a non-rotated page is unaffected.
    let dest_transform = read_page_transform(dest, dest_page_ref)?;
    // qpdf's getMatrixForTransformations reads the box through getArrayAsRectangle
    // (libqpdf/QPDFPageObjectHelper.cc), so the width/height are the normalized
    // (non-negative) extents. These dims feed ONLY the tmatrix translation column
    // (transformation_matrix puts width*scale/height*scale in positions e/f, never
    // the a/b/c/d rotation part), and the placement centring (tx = r_cx - t_cx)
    // absorbs that translation -- so for a reversed box this normalization is an
    // output no-op that no byte-gate can isolate. It is kept to reproduce qpdf's
    // computation faithfully, NOT for an observable byte difference (do not "dead
    // code" it away).
    let [n_llx, n_lly, n_urx, n_ury] = normalize_rectangle(page_box_array(&trim_box));
    let trim_w = n_urx - n_llx;
    let trim_h = n_ury - n_lly;
    let tmatrix = transformation_matrix(&dest_transform, trim_w, trim_h, true);

    // 1. Convert the destination page itself to Form XObject /Fx0.
    let fx0_ref = page_to_form_xobject(dest, dest_page_ref)?;

    // 2. Name the sources /Fx1.. in underlays-then-overlays order and build the
    //    new page /Resources /XObject mapping. /Fx0 is the page; the unique-name
    //    counter continues from there (getUniqueResourceName).
    let mut xobject_dict = Dictionary::new();
    xobject_dict.insert("Fx0", Object::Reference(fx0_ref));
    let mut next_index = 1u32;
    type NamedPlacement = (
        String,
        ObjectRef,
        Option<crate::overlay_annotations::AnnotationCopyTemplate>,
    );
    let mut underlay_names: Vec<NamedPlacement> = Vec::new();
    let mut overlay_names: Vec<NamedPlacement> = Vec::new();
    for (xref, template) in &underlays {
        let name = format!("Fx{next_index}");
        xobject_dict.insert(name.as_bytes(), Object::Reference(*xref));
        underlay_names.push((name, *xref, template.clone()));
        next_index += 1;
    }
    for (xref, template) in &overlays {
        let name = format!("Fx{next_index}");
        xobject_dict.insert(name.as_bytes(), Object::Reference(*xref));
        overlay_names.push((name, *xref, template.clone()));
        next_index += 1;
    }

    // 3. Build the new page /Contents in draw order: underlays -> /Fx0 ->
    //    overlays. Underlays/overlays place into the page /TrimBox with
    //    allow_shrink=true; /Fx0 places into the page /MediaBox with
    //    allow_shrink=false (qpdf's doUnderOverlayForPage flag split). Every
    //    placement folds in the dest inverse transform `tmatrix`. Immediately
    //    after each source placement returns `cm`, the source's annotation
    //    template (if any) is applied through
    //    [`crate::overlay_annotations::apply_placement`], mirroring qpdf's
    //    `dest_page.copyAnnotations(from_page, cm, dest_afdh, from_afdh)` at
    //    `QPDFJob::doUnderOverlayForPage` line 1899. New top-level fields
    //    accumulate across placements and are appended to /AcroForm/Fields
    //    (with +N rename on FQN collision) once at the end.
    // Placement rects mirror qpdf's getTrimBox()/getMediaBox().getArrayAsRectangle()
    // in doUnderOverlayForPage: corners normalized before scaling/centring.
    let trim_rect = normalize_rectangle(page_box_array(&trim_box));
    let media_rect = normalize_rectangle(page_box_array(&media_box));
    let mut content = String::new();
    let mut new_top_fields: Vec<ObjectRef> = Vec::new();
    let mut dest_acroform_dr: Option<ObjectRef> = None;
    for (name, xref, template) in &underlay_names {
        let (bbox, fmatrix) = fo_bbox_and_matrix(dest, *xref)?;
        let (fragment, cm) =
            place_form_xobject(bbox, fmatrix, trim_rect, tmatrix, true, false, name);
        content.push_str(&fragment);
        if let Some(tpl) = template {
            let mut added = crate::overlay_annotations::apply_placement(
                dest,
                dest_page_ref,
                tpl,
                cm,
                &mut dest_acroform_dr,
            )?;
            new_top_fields.append(&mut added);
        }
    }
    {
        let (bbox, fmatrix) = fo_bbox_and_matrix(dest, fx0_ref)?;
        let (fragment, _cm) =
            place_form_xobject(bbox, fmatrix, media_rect, tmatrix, false, false, "Fx0");
        content.push_str(&fragment);
    }
    for (name, xref, template) in &overlay_names {
        let (bbox, fmatrix) = fo_bbox_and_matrix(dest, *xref)?;
        let (fragment, cm) =
            place_form_xobject(bbox, fmatrix, trim_rect, tmatrix, true, false, name);
        content.push_str(&fragment);
        if let Some(tpl) = template {
            let mut added = crate::overlay_annotations::apply_placement(
                dest,
                dest_page_ref,
                tpl,
                cm,
                &mut dest_acroform_dr,
            )?;
            new_top_fields.append(&mut added);
        }
    }

    // 4. Allocate the new /Contents stream (uncompressed, no /Filter; the writer
    //    compresses on output).
    let contents_ref = next_object_ref(dest)?;
    let contents_stream = Stream::new(Dictionary::new(), content.into_bytes());
    dest.set_object(contents_ref, Object::Stream(contents_stream));

    // 5. Rewrite the page dictionary: replace /Resources and /Contents, keep all
    //    other keys (in particular, /Annots — which apply_placement above may
    //    have already extended in place — must survive this step).
    let mut page_dict = page_dictionary(dest, dest_page_ref)?;
    // Fetch the /Annots value we just installed (if any) so it is carried onto
    // the rewritten page dict below (apply_placement wrote to the pre-rewrite
    // dict). If page_dictionary returns a fresh clone that already includes
    // /Annots, this is a no-op; if it does not, we re-install it.
    let live_annots = {
        let obj = dest.resolve_borrowed(dest_page_ref)?;
        obj.as_dict().and_then(|d| d.get("Annots").cloned())
    };
    let mut resources = Dictionary::new();
    resources.insert("XObject", Object::Dictionary(xobject_dict));
    page_dict.insert("Resources", Object::Dictionary(resources));
    page_dict.insert("Contents", Object::Reference(contents_ref));
    if let Some(annots) = live_annots {
        page_dict.insert("Annots", annots);
    }
    dest.set_object(dest_page_ref, Object::Dictionary(page_dict));

    // 6. Finalize: append the accumulated new top-level fields to
    //    /AcroForm/Fields, renaming /T on FQN collision with existing dest
    //    fields (qpdf's addAndRenameFormFields).
    crate::overlay_annotations::add_and_rename_form_fields(dest, new_top_fields)?;

    Ok(())
}

/// Pair selected destination pages with source pages, mirroring qpdf's
/// `QPDFJob::doUnderOverlay` page-mapping loop (qpdf 11.9.0).
///
/// `from_pages`, `to_pages`, and `repeat_pages` are 1-based page numbers already
/// resolved from the `--from`, `--to`, and `--repeat` page ranges. The `i`-th
/// selected destination page (`to_pages[i]`) is paired with:
///
/// - `from_pages[i]` while `i < from_pages.len()`;
/// - otherwise, when `repeat_pages` is non-empty,
///   `repeat_pages[(i - from_pages.len()) % repeat_pages.len()]` (the repeat
///   pages cycle);
/// - otherwise that destination page is skipped (it receives no overlay).
///
/// The result is a `Vec<(dest_page, source_page)>` in `to_pages` order, omitting
/// the skipped destination pages.
fn map_overlay_pages(
    from_pages: &[u32],
    to_pages: &[u32],
    repeat_pages: &[u32],
) -> Vec<(u32, u32)> {
    let mut pairs = Vec::new();
    for (i, &dest) in to_pages.iter().enumerate() {
        let source = if i < from_pages.len() {
            from_pages[i]
        } else if !repeat_pages.is_empty() {
            repeat_pages[(i - from_pages.len()) % repeat_pages.len()]
        } else {
            // Source pages exhausted and no --repeat: this dest page gets nothing.
            continue;
        };
        pairs.push((dest, source));
    }
    pairs
}

/// Map a single overlay/underlay spec to its per-destination-page sources
/// **without applying them**, mirroring qpdf's `QPDFJob::doUnderOverlay` source
/// preparation for one `--overlay`/`--underlay` group (qpdf 11.9.0).
///
/// `from`, `to`, and `repeat` are the spec's page ranges. `from` (default all
/// source pages) selects source pages; `to` (default all destination pages)
/// selects destination pages; `repeat` is `None` by default (no repetition) and,
/// when `Some`, selects source pages to cycle once `from` is exhausted. The
/// selected pages are paired by [`map_overlay_pages`].
///
/// The distinct source pages used by the mapping are imported into `dest` in a
/// single cross-document copy via [`import_pages_as_form_xobjects`] (so an object
/// shared by several source pages is copied once), and the imported Form XObject
/// reference is shared across every destination page that uses that source page
/// (qpdf imports each source page once and reuses the object). The result is a
/// `Vec<(dest_page, OverlaySource)>` in `to` order: each entry pairs a 1-based
/// destination page number with an [`OverlaySource`] of the given `kind` carrying
/// the shared imported XObject reference. No destination page is modified here;
/// the caller aggregates these across specs and applies them.
///
/// # Errors
///
/// - [`Error::Unsupported`] when a resolved page number falls outside `dest` or
///   `source` (the page lists and counts are read once up front, so this only
///   triggers on an internally inconsistent mapping), or any error propagated
///   from [`PageRange::resolve`] or [`import_pages_as_form_xobjects`].
fn spec_page_sources<RS, RT>(
    dest: &mut Pdf<RT>,
    source: &mut Pdf<RS>,
    kind: OverlayKind,
    from: &PageRange,
    to: &PageRange,
    repeat: Option<&PageRange>,
    n_dest: u32,
) -> Result<Vec<(u32, OverlaySource)>>
where
    RS: Read + Seek,
    RT: Read + Seek,
{
    // Snapshot the source page list before mutating `dest`. The applied patches
    // change page dictionaries in place but never reorder or remove page
    // objects, so the 1-based page numbers stay valid.
    let source_pages = page_refs(source)?;
    let n_source = u32_len(source_pages.len());
    let pairs = resolve_spec_pairs(n_source, from, to, repeat, n_dest)?;

    // Collect the distinct source pages in first-use order, then import them all
    // in a SINGLE cross-document copy. One copy shares any indirect object used
    // by more than one source page (a `/Font`, `/ProcSet`, …) instead of
    // duplicating it, matching qpdf's per-document foreign→local map. The same
    // imported XObject ref is then reused on every dest page that uses that
    // source page (qpdf imports each source page once).
    let mut distinct_sources: Vec<u32> = Vec::new();
    let mut seen = BTreeSet::new();
    for &(_dest_page, source_page) in &pairs {
        if seen.insert(source_page) {
            distinct_sources.push(source_page);
        }
    }
    let source_refs: Vec<ObjectRef> = distinct_sources
        .iter()
        .map(|&p| page_ref_for(&source_pages, p, "source"))
        .collect::<Result<_>>()?;

    // Inline the two-phase import so the annotation closure (annots + fields
    // + AP streams + /DR fonts) is unioned into the SAME copy_objects call as
    // the Form XObject closure — advisor #2: one shared foreign→local map per
    // source document, so a font used by both page /Resources and a widget's
    // /AP is copied exactly once.
    let mut xobject_seeds: Vec<ObjectRef> = Vec::with_capacity(source_refs.len());
    let mut union: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut surveys: Vec<Option<crate::overlay_annotations::AnnotationSurvey>> =
        Vec::with_capacity(source_refs.len());
    for &page_ref in &source_refs {
        let xobject_ref = crate::page_form_xobject::page_to_form_xobject(source, page_ref)?;
        union.extend(crate::page_form_xobject::xobject_object_closure(
            source,
            xobject_ref,
        )?);
        xobject_seeds.push(xobject_ref);
        match crate::overlay_annotations::survey_source_annotations(source, page_ref)? {
            Some((survey, annot_closure)) => {
                union.extend(annot_closure);
                surveys.push(Some(survey));
            }
            None => surveys.push(None),
        }
    }
    let map = crate::object_copy::copy_objects(source, dest, &union)?;
    let imported_xobject_refs: Vec<ObjectRef> = xobject_seeds
        .iter()
        .map(|xref| {
            map.get(xref).copied().ok_or_else(|| {
                Error::Unsupported(
                    "imported Form XObject reference missing from copy map".to_string(),
                )
            })
        })
        .collect::<Result<_>>()?;
    let imported: BTreeMap<
        u32,
        (
            ObjectRef,
            Option<crate::overlay_annotations::AnnotationCopyTemplate>,
        ),
    > = distinct_sources
        .iter()
        .copied()
        .zip(
            imported_xobject_refs
                .into_iter()
                .zip(surveys.into_iter())
                .map(|(xr, sv)| {
                    (
                        xr,
                        sv.map(|s| crate::overlay_annotations::template_from_survey(&s, &map)),
                    )
                }),
        )
        .collect();

    Ok(pairs
        .iter()
        .map(|&(dest_page, source_page)| {
            // `source_page` came from `pairs`, so it is one of `distinct_sources`
            // and is always present in the map; index directly.
            let (xobject_ref, template) = imported[&source_page].clone();
            (
                dest_page,
                OverlaySource {
                    kind,
                    xobject_ref,
                    annot_template: template,
                },
            )
        })
        .collect())
}

/// Resolve a single overlay/underlay spec's `--from`/`--to`/`--repeat` ranges
/// into `(dest_page, source_page)` pairs, without touching either document.
///
/// This is the range-math half of [`spec_page_sources`]: it resolves the three
/// ranges against the caller-supplied page counts and calls
/// [`map_overlay_pages`] to produce the pairing. No pages are imported and no
/// destination pages are modified; the caller decides what to do with the
/// pairs (import + apply, or report). `n_source` and `n_dest` are the source
/// and destination page counts, computed once by the caller.
///
/// # Errors
///
/// Any error propagated from [`PageRange::resolve`].
pub(crate) fn resolve_spec_pairs(
    n_source: u32,
    from: &PageRange,
    to: &PageRange,
    repeat: Option<&PageRange>,
    n_dest: u32,
) -> Result<Vec<(u32, u32)>> {
    let from_pages = from.resolve(n_source)?;
    let to_pages = to.resolve(n_dest)?;
    let repeat_pages = match repeat {
        Some(pr) => pr.resolve(n_source)?,
        None => Vec::new(),
    };

    Ok(map_overlay_pages(&from_pages, &to_pages, &repeat_pages))
}

/// Apply a single overlay/underlay spec to `dest`, mirroring qpdf's
/// `QPDFJob::doUnderOverlay` for one `--overlay`/`--underlay` group (qpdf 11.9.0).
///
/// A thin wrapper over [`spec_page_sources`] + [`apply_overlay_specs`]'s
/// aggregation: the spec's per-destination-page sources are mapped, grouped by
/// destination page, and each affected page is patched by
/// [`apply_overlays_to_page`] exactly once. Destination pages not in the mapping
/// are left untouched. See [`spec_page_sources`] for the page-mapping and
/// XObject-sharing semantics.
///
/// # Errors
///
/// Propagates any error from [`spec_page_sources`], [`page_ref_for`], or
/// [`apply_overlays_to_page`].
// Single-spec convenience wrapper used only by the feature-gated byte gate;
// the CLI and `apply_overlay_specs` map specs directly via `spec_page_sources`.
#[allow(dead_code)]
fn apply_overlay_spec<RS, RT>(
    dest: &mut Pdf<RT>,
    source: &mut Pdf<RS>,
    kind: OverlayKind,
    from: &PageRange,
    to: &PageRange,
    repeat: Option<&PageRange>,
) -> Result<()>
where
    RS: Read + Seek,
    RT: Read + Seek,
{
    let n_dest = u32_len(page_refs(dest)?.len());
    let sources = spec_page_sources(dest, source, kind, from, to, repeat, n_dest)?;
    apply_aggregated_sources(dest, group_sources_by_dest_page(&sources))
}

/// A single overlay/underlay specification: a source document, its kind, and its
/// `--from`/`--to`/`--repeat` page ranges, as one `--overlay`/`--underlay` group
/// on the qpdf command line.
pub struct OverlaySpec<RS: Read + Seek> {
    /// The source document supplying the overlay/underlay pages.
    pub source: Pdf<RS>,
    /// Whether the source is drawn beneath or above the destination content.
    pub kind: OverlayKind,
    /// `--from`: which source pages are used (default all source pages).
    pub from: PageRange,
    /// `--to`: which destination pages receive the source (default all).
    pub to: PageRange,
    /// `--repeat`: source pages cycled once `from` is exhausted (default none).
    pub repeat: Option<PageRange>,
}

/// Group per-spec `(dest_page, source)` entries by destination page, preserving
/// each entry's encounter order within its page.
///
/// `entries` must already be in the order the sources should be drawn/named on a
/// page: across specs in declaration order, and within a spec in `--to` order.
/// The returned [`BTreeMap`] iterates destination pages in ascending page order
/// and, within a page, preserves that encounter order (so
/// [`apply_overlays_to_page`]'s kind grouping yields underlays-then-overlays with
/// each kind in declaration order).
fn group_sources_by_dest_page(
    entries: &[(u32, OverlaySource)],
) -> BTreeMap<u32, Vec<OverlaySource>> {
    let mut by_page: BTreeMap<u32, Vec<OverlaySource>> = BTreeMap::new();
    for (dest_page, source) in entries {
        by_page.entry(*dest_page).or_default().push(source.clone());
    }
    by_page
}

/// Stable-partition `entries` into (underlays first, overlays second),
/// preserving each source's original relative order within its kind.
///
/// qpdf orders overlay/underlay sources this way for both painting
/// (see [`apply_overlays_to_page`]) and `--verbose` progress reporting.
/// Sharing one implementation here prevents drift between painting and
/// progress reporting.
pub(crate) fn kind_stable_partition<T, F>(entries: Vec<T>, kind_of: F) -> Vec<T>
where
    F: Fn(&T) -> OverlayKind,
{
    let (underlays, overlays): (Vec<T>, Vec<T>) = entries
        .into_iter()
        .partition(|e| matches!(kind_of(e), OverlayKind::Underlay));
    let mut out = underlays;
    out.extend(overlays);
    out
}

/// Apply already-grouped overlay/underlay sources to `dest`, calling
/// [`apply_overlays_to_page`] **exactly once** per destination page (so each page
/// is converted to `/Fx0` only once). Pages are processed in ascending page
/// order; the per-page source order from `by_page` is preserved.
///
/// # Errors
///
/// Propagates any error from [`page_refs`], [`page_ref_for`], or
/// [`apply_overlays_to_page`].
fn apply_aggregated_sources<R: Read + Seek>(
    dest: &mut Pdf<R>,
    by_page: BTreeMap<u32, Vec<OverlaySource>>,
) -> Result<()> {
    // Snapshot the dest page refs once; the patches mutate page dicts in place
    // but never reorder or remove page objects, so 1-based numbers stay valid.
    let dest_pages = page_refs(dest)?;
    for (dest_page, sources) in by_page {
        let dest_ref = page_ref_for(&dest_pages, dest_page, "destination")?;
        apply_overlays_to_page(dest, dest_ref, &sources)?;
    }
    Ok(())
}

/// Compose multiple overlay/underlay specs onto `dest`, mirroring qpdf's
/// `QPDFJob::doUnderOverlay` handling of several `--overlay`/`--underlay` groups
/// (qpdf 11.9.0).
///
/// Each [`OverlaySpec`] is mapped independently against `dest`: its `from`/`to`/
/// `repeat` ranges select the source-to-destination page pairing, and each spec's
/// source pages are imported into `dest` as Form XObjects in a single
/// cross-document copy (a source page used on several destination pages is
/// imported once and shared). The per-destination-page sources from all specs are
/// then aggregated **in declaration order** and each affected destination page is
/// rewritten exactly once: the page itself becomes Form XObject `/Fx0`, and the
/// sources are named `/Fx1…/FxN` and drawn in qpdf order — underlays (across
/// specs, declaration order), then `/Fx0` (the page), then overlays (across specs,
/// declaration order).
///
/// Destination pages not selected by any spec are left untouched. The specs'
/// source documents are taken by `&mut` because importing reads (and may seek)
/// them.
///
/// # Errors
///
/// - [`Error::Unsupported`] when a page number resolves outside its document, a
///   page lacks a usable placement box, or the object-number space is exhausted.
/// - Any error propagated from page-range resolution, the cross-document copy, or
///   [`Pdf::resolve`].
pub fn apply_overlay_specs<RS, RT>(dest: &mut Pdf<RT>, specs: &mut [OverlaySpec<RS>]) -> Result<()>
where
    RS: Read + Seek,
    RT: Read + Seek,
{
    // Map every spec first, collecting its per-dest-page sources in declaration
    // order. Each spec gets its own batch import into `dest` (separate documents
    // => one foreign→local copy per source doc).
    // The dest page count is invariant while specs are mapped (sources are
    // applied only after the loop), so query the page tree once up front
    // instead of re-walking it per spec.
    let n_dest = u32_len(page_refs(dest)?.len());
    let mut entries: Vec<(u32, OverlaySource)> = Vec::new();
    for spec in specs.iter_mut() {
        let sources = spec_page_sources(
            dest,
            &mut spec.source,
            spec.kind,
            &spec.from,
            &spec.to,
            spec.repeat.as_ref(),
            n_dest,
        )?;
        entries.extend(sources);
    }
    apply_aggregated_sources(dest, group_sources_by_dest_page(&entries))
}

/// A single overlay/underlay source contributing to one destination page, as
/// reported by [`overlay_verbose_report`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayVerboseSource {
    /// Zero-based index of the source's spec in the `specs` slice passed to
    /// [`overlay_verbose_report`].
    pub spec_index: usize,
    /// Whether the source is drawn beneath or above the destination content.
    pub kind: OverlayKind,
    /// One-based source page number contributing to this destination page.
    pub src_page: u32,
}

/// One destination page's overlay/underlay plan, as reported by
/// [`overlay_verbose_report`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayVerbosePage {
    /// One-based destination page number.
    pub dest_page: u32,
    /// Sources drawn on this page, ordered underlays-first (declaration order
    /// across specs) then overlays. Empty when no spec targets this page.
    pub sources: Vec<OverlayVerboseSource>,
}

/// Return the per-destination-page overlay/underlay plan without importing any
/// source page or mutating the destination graph.
///
/// The returned vector covers every destination page in ascending order
/// (`1..=n_dest`). Per-page sources are ordered underlays first (in declaration
/// order across `specs`), then overlays (also in declaration order), matching
/// the order [`apply_overlay_specs`] uses to paint the same page. Destination
/// pages that no spec targets appear in the result with an empty `sources`.
///
/// The source documents are taken by `&mut` because [`PageRange::resolve`]
/// reads their page trees; the destination is taken by `&mut` for the same
/// reason. Neither document's on-disk content is modified. Calling this before
/// [`apply_overlay_specs`] on the same specs yields the paint plan that will be
/// applied.
///
/// # Errors
///
/// - [`Error::Parse`] when a `--from`/`--to`/`--repeat` range references a
///   page number outside its document (propagated from
///   [`PageRange::resolve`]).
/// - Any error propagated from [`pages::page_refs`](crate::pages::page_refs)
///   — typically [`Error::Missing`] for a missing `/Root`/`/Pages`, or
///   [`Error::Unsupported`] for a malformed page tree.
pub fn overlay_verbose_report<RS, RT>(
    dest: &mut Pdf<RT>,
    specs: &mut [OverlaySpec<RS>],
) -> Result<Vec<OverlayVerbosePage>>
where
    RS: Read + Seek,
    RT: Read + Seek,
{
    let n_dest = u32_len(page_refs(dest)?.len());
    // Flatten every spec's (dest_page, source) pairs in declaration order.
    let mut flat: Vec<(u32, OverlayVerboseSource)> = Vec::new();
    for (spec_index, spec) in specs.iter_mut().enumerate() {
        let n_source = u32_len(page_refs(&mut spec.source)?.len());
        let pairs =
            resolve_spec_pairs(n_source, &spec.from, &spec.to, spec.repeat.as_ref(), n_dest)?;
        for (dest_page, src_page) in pairs {
            flat.push((
                dest_page,
                OverlayVerboseSource {
                    spec_index,
                    kind: spec.kind,
                    src_page,
                },
            ));
        }
    }
    // Group by destination page (ascending order via BTreeMap).
    let mut by_page: BTreeMap<u32, Vec<OverlayVerboseSource>> = BTreeMap::new();
    for (dest_page, src) in flat {
        by_page.entry(dest_page).or_default().push(src);
    }
    // Emit one entry per dest page in 1..=n_dest, with underlays-then-overlays
    // per page (shared with the paint path via kind_stable_partition).
    let mut out = Vec::with_capacity(n_dest as usize);
    for dest_page in 1..=n_dest {
        let sources = by_page.remove(&dest_page).unwrap_or_default();
        let sources = kind_stable_partition(sources, |s| s.kind);
        out.push(OverlayVerbosePage { dest_page, sources });
    }
    Ok(out)
}

/// Convert a page-list length to `u32`, the width [`PageRange::resolve`] expects.
///
/// A document with more than `u32::MAX` pages is not representable; clamp to
/// `u32::MAX` so a pathological count cannot wrap (qpdf's page index is `int`,
/// far below this bound, so real documents never reach the clamp).
fn u32_len(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

/// Look up the [`ObjectRef`] of a 1-based `page` number in `pages`, erroring when
/// it is out of range. `which` names the document (`"source"`/`"destination"`)
/// for the error message.
fn page_ref_for(pages: &[ObjectRef], page: u32, which: &str) -> Result<ObjectRef> {
    let idx = (page as usize)
        .checked_sub(1)
        .filter(|&i| i < pages.len())
        .ok_or_else(|| {
            Error::Unsupported(format!(
                "{which} page {page} is out of range (document has {} page(s))",
                pages.len()
            ))
        })?;
    Ok(pages[idx])
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

/// Normalize a rectangle's corners the way qpdf's
/// `QPDFObjectHandle::getArrayAsRectangle` does: `llx = min(x0, x2)`,
/// `lly = min(x1, x3)`, `urx = max(x0, x2)`, `ury = max(x1, x3)`. qpdf reads all
/// box geometry for placement through this accessor, so a page with a reversed box
/// (`llx > urx` or `lly > ury`) still yields a non-negative width/height and places
/// identically to its ordered form.
fn normalize_rectangle([x0, x1, x2, x3]: [f64; 4]) -> [f64; 4] {
    [x0.min(x2), x1.min(x3), x0.max(x2), x1.max(x3)]
}

/// Coerce a PDF numeric object to `f64`, matching qpdf's numeric coercion
/// (non-numeric values, including indirect references, contribute `0.0`).
fn as_f64(o: &Object) -> f64 {
    o.as_integer()
        .map(|i| i as f64)
        .or_else(|| o.as_real())
        .unwrap_or(0.0)
}

/// Read a Form XObject dictionary's `/Matrix` as `[a b c d e f]`, defaulting to
/// the identity when `/Matrix` is absent or not a 6+ element array. The Form
/// XObjects built by [`page_to_form_xobject`] always carry a direct `/Matrix`
/// array, so no indirect-reference resolution is needed here.
fn matrix_or_identity(dict: &Dictionary) -> [f64; 6] {
    match dict.get("Matrix").and_then(Object::as_array) {
        Some(m) if m.len() >= 6 => [
            as_f64(&m[0]),
            as_f64(&m[1]),
            as_f64(&m[2]),
            as_f64(&m[3]),
            as_f64(&m[4]),
            as_f64(&m[5]),
        ],
        _ => [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
    }
}

/// Apply PDF matrix `m = [a b c d e f]` to the four corners of `bbox` and return
/// the axis-aligned bounding rectangle `[llx lly urx ury]` of the result. A
/// point `(x, y)` maps to `(a*x + c*y + e, b*x + d*y + f)`.
fn transform_bbox(bbox: [f64; 4], m: [f64; 6]) -> [f64; 4] {
    let [llx, lly, urx, ury] = bbox;
    let [a, b, c, d, e, f] = m;
    let pt = |x: f64, y: f64| (a * x + c * y + e, b * x + d * y + f);
    let corners = [pt(llx, lly), pt(urx, lly), pt(urx, ury), pt(llx, ury)];
    let mut min_x = corners[0].0;
    let mut max_x = corners[0].0;
    let mut min_y = corners[0].1;
    let mut max_y = corners[0].1;
    for &(x, y) in &corners[1..] {
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    [min_x, min_y, max_x, max_y]
}

/// Read an imported Form XObject's raw `/BBox` (`[llx lly urx ury]`) and `/Matrix`
/// (`[a b c d e f]`), the inputs qpdf's `getMatrixForFormXObjectPlacement`
/// consumes.
///
/// The `/Matrix` is returned verbatim (not pre-applied to the `/BBox`): qpdf folds
/// it into the placement computation alongside the destination page's inverse
/// transform. Non-numeric `/BBox` elements coerce to `0.0` (matching qpdf); a
/// `/BBox` shorter than four elements is an error; an absent or malformed
/// `/Matrix` is treated as the identity.
fn fo_bbox_and_matrix<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    xobject_ref: ObjectRef,
) -> Result<([f64; 4], [f64; 6])> {
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
    let matrix = matrix_or_identity(dict);
    // /BBox may be stored as an indirect reference; resolve it before reading
    // (qpdf dereferences here, so a reference must not fall through as "no array").
    let bbox_entry = dict.get("BBox").ok_or_else(|| {
        Error::Unsupported(format!("Form XObject {xobject_ref} has no /BBox array"))
    })?;
    let resolved_bbox = match bbox_entry {
        Object::Reference(r) => pdf.resolve(*r)?,
        other => other.clone(),
    };
    let arr = resolved_bbox.as_array().ok_or_else(|| {
        Error::Unsupported(format!("Form XObject {xobject_ref} has no /BBox array"))
    })?;
    if arr.len() < 4 {
        return Err(Error::Unsupported(format!(
            "Form XObject {xobject_ref} /BBox has {} elements, expected 4",
            arr.len()
        )));
    }
    let bbox = [
        as_f64(&arr[0]),
        as_f64(&arr[1]),
        as_f64(&arr[2]),
        as_f64(&arr[3]),
    ];
    Ok((bbox, matrix))
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
//
// Overlay/underlay byte-identity matrix (flpdf-9hc.16.7). Each row is a
// `qpdf 11.9.0 --static-id` invocation reproduced byte-for-byte at the library
// layer; the golden recipes live in tests/golden/regenerate.sh. Goldens under
// tests/golden/references/overlay/.
//
//   case                | kind     | dest          | source        | --from | --to  | --repeat
//   --------------------|----------|---------------|---------------|--------|-------|---------
//   one-page (.16.3)    | overlay  | three-page    | one-page      | -      | -     | -
//   two-page default    | overlay  | three-page    | two-page      | -      | -     | -
//   one-page repeat1    | overlay  | three-page    | one-page      | -      | -     | 1
//   two-page to=2-3     | overlay  | three-page    | two-page      | -      | 2-3   | -
//   two overlays (.16.5)| overlay×2| three-page    | one + two     | -      | -     | -
//   overlay+underlay    | over+und | three-page    | one + two     | -      | -     | -
//   two-page from=2     | overlay  | three-page    | two-page      | 2      | -     | -
//   two-page from= rpt2 | overlay  | three-page    | two-page      | (empty)| -     | 2
//   underlay two-page   | underlay | three-page    | two-page      | -      | -     | -
//   rotated source mtx  | overlay  | three-page    | one-page-r90  | -      | -     | -
//   one-page to=1-3 rpt1| overlay  | three-page    | one-page      | -      | 1-3   | 1
//   multi-stream (.16.10)| overlay | three-page    | multi-stream  | -      | -     | -
//   rotated dest (.16.10)| overlay | one-page-r90  | one-page      | -      | -     | -
//   userunit (.16.10)   | overlay  | three-page    | userunit      | -      | -     | -
//   swapped box (lkk7)  | overlay  | swapped-box   | one-page      | -      | -     | -
//   swapped+r90 (lkk7)  | overlay  | swapped-r90   | swapped-r90   | -      | -     | -
//
// The flpdf-lkk7 rows cover reversed page boxes (llx>urx AND lly>ury): qpdf reads
// all placement geometry through getArrayAsRectangle (min/max normalized). The
// swapped-box row proves the placement-rect normalization (a raw rect would reflect
// the source cm). The swapped+r90 row (overlaid onto itself) additionally proves the
// source/dest Form /Matrix dims normalize -- the /Matrix array is serialized into
// the output, so a raw width flips its sign. (The dest tmatrix dims are ALSO
// normalized in code, but that is an output no-op here: their only effect is the
// tmatrix translation, which the placement centring absorbs -- see
// apply_overlays_to_page. So no gate isolates it.) Both fixtures are pinned to 1.3.
//
// The rotated-source row is the matrix-transformed placement check: the source
// page carries /Rotate 90, so its imported Form XObject gets a non-identity
// /Matrix. The flpdf-9hc.16.10 rows widen the gate to the four byte-parity gaps
// the narrow fixtures had masked: multi-stream exercises the conditional /Matrix
// omission (no /Rotate) and qpdf's newline content coalescing; rotated dest
// exercises the destination inverse transform folded into every placement cm;
// userunit exercises the /UserUnit scale folded into the Form /Matrix. The
// .16.10 source fixtures are pinned to PDF 1.3 (== the three-page dest) so the
// orthogonal source version-floor limitation does not perturb the bytes.
//
// Source version-floor + Adobe extension_level propagation (pure header
// bump AND AES-256 /Extensions/ADBE injection) is now covered here by
// `overlay_pure_source_version_floor_bytes` and
// `overlay_encrypted_source_extension_level_bytes` (below).
//
// Explicit deferrals (NOT covered here, by design):
//   - CLI-level overlay byte-identity: these gates write through the library
//     entry points with `NewlineBeforeEndstream::Never` to keep the byte
//     comparison surgical. The CLI now also defaults to `Never` and can be
//     wired up separately for CLI-level overlay byte-identity coverage.
#[cfg(all(test, feature = "qpdf-zlib-compat"))]
mod byte_gate {
    use super::{
        apply_overlay_spec, apply_overlay_specs, apply_overlays_to_page, OverlayKind,
        OverlaySource, OverlaySpec,
    };
    use crate::page_form_xobject::import_page_as_form_xobject;
    use crate::page_range::PageRange;
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

    fn golden(name: &str) -> Vec<u8> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/golden/references/overlay")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("read golden {path:?}: {e}"))
    }

    /// Parse a page-range string, panicking with context on error.
    fn pr(input: &str) -> PageRange {
        PageRange::parse(input).unwrap_or_else(|e| panic!("parse {input:?}: {e}"))
    }

    /// Write `dest` through the `flpdf rewrite --static-id` recipe.
    fn write_static_id<R: std::io::Read + std::io::Seek>(dest: &mut Pdf<R>) -> Vec<u8> {
        let opts = WriteOptions {
            full_rewrite: true,
            static_id: true,
            newline_before_endstream: NewlineBeforeEndstream::Never,
            ..Default::default()
        };
        let mut out = Vec::new();
        write_pdf_with_options(dest, &mut out, &opts).unwrap();
        out
    }

    /// Write `dest` through the `flpdf rewrite --static-id --qdf --no-original-object-ids`
    /// recipe. QDF uses the caller's `newline_before_endstream` policy but
    /// promotes only `Never` to `No` internally (so `endstream` stays
    /// line-anchored) — we leave `newline_before_endstream` at its default
    /// (`Never`) and rely on that promotion.
    fn write_qdf_nooid<R: std::io::Read + std::io::Seek>(dest: &mut Pdf<R>) -> Vec<u8> {
        let opts = WriteOptions {
            full_rewrite: true,
            static_id: true,
            qdf: true,
            no_original_object_ids: true,
            ..Default::default()
        };
        let mut out = Vec::new();
        write_pdf_with_options(dest, &mut out, &opts).unwrap();
        out
    }

    /// Report the first differing byte offset for a readable failure message.
    fn first_diff(a: &[u8], b: &[u8]) -> Option<usize> {
        if a == b {
            return None;
        }
        let common = a.len().min(b.len());
        (0..common).find(|&i| a[i] != b[i]).or(Some(common))
    }

    /// Assert `actual` is byte-identical to the golden named `golden_name`,
    /// reporting the first diff offset and surrounding bytes on mismatch.
    fn assert_byte_identical(actual: &[u8], golden_name: &str) {
        let expected = golden(golden_name);
        if let Some(off) = first_diff(actual, &expected) {
            let lo = off.saturating_sub(24);
            let g = expected.get(off).copied().unwrap_or(0);
            let f = actual.get(off).copied().unwrap_or(0);
            panic!(
                "overlay output not byte-identical to qpdf golden {golden_name} \
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
                annot_template: None,
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

        let expected = golden("three-page-overlay-one-page.pdf");
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

    #[test]
    fn three_page_overlay_one_page_qdf_is_byte_identical() {
        // Same as three_page_overlay_one_page_is_byte_identical but written
        // through the QDF + --no-original-object-ids recipe.
        let mut dest = fixture("three-page.pdf");
        let mut source = fixture("one-page.pdf");
        apply_overlay_spec(
            &mut dest,
            &mut source,
            OverlayKind::Overlay,
            &pr(""),
            &pr(""),
            None,
        )
        .unwrap();
        let actual = write_qdf_nooid(&mut dest);
        assert_byte_identical(&actual, "three-page-overlay-one-page-qdf.pdf");
    }

    #[test]
    fn three_page_two_overlays_qdf_is_byte_identical() {
        // QDF recipe of two_overlays_compose_byte_identical:
        // `qpdf --overlay one-page.pdf -- --overlay two-page.pdf --`. Two
        // overlays compose left-to-right in declaration order (Fx0/Fx1 for the
        // first spec, Fx2 for the second on page 1; Fx0/Fx1 on page 2; page 3
        // untouched). Verifies the Fx0/Fx1 declaration-order convention under
        // QDF.
        let mut dest = fixture("three-page.pdf");
        let mut specs = vec![
            spec("one-page.pdf", OverlayKind::Overlay),
            spec("two-page.pdf", OverlayKind::Overlay),
        ];
        apply_overlay_specs(&mut dest, &mut specs).unwrap();
        let actual = write_qdf_nooid(&mut dest);
        assert_byte_identical(&actual, "three-page-two-overlays-qdf.pdf");
    }

    #[test]
    fn three_page_overlay_and_underlay_qdf_is_byte_identical() {
        // QDF recipe of overlay_and_underlay_compose_byte_identical:
        // `qpdf --overlay one-page.pdf -- --underlay two-page.pdf --`, which
        // apply_overlay_specs batches together so Form XObject naming follows
        // the under-then-over cross-spec convention (Fx0, Fx1, Fx2 on page 1;
        // Fx0, Fx1 on page 2; page 3 untouched). Verifies Form XObject
        // naming/order preservation under QDF.
        let mut dest = fixture("three-page.pdf");
        let mut specs = vec![
            spec("one-page.pdf", OverlayKind::Overlay),
            spec("two-page.pdf", OverlayKind::Underlay),
        ];
        apply_overlay_specs(&mut dest, &mut specs).unwrap();
        let actual = write_qdf_nooid(&mut dest);
        assert_byte_identical(&actual, "three-page-overlay-and-underlay-qdf.pdf");
    }

    #[test]
    fn overlay_two_page_default_is_byte_identical() {
        // dest=three-page, overlay source=two-page, defaults: p1<-s1, p2<-s2,
        // p3 untouched (source exhausted, no --repeat).
        let mut dest = fixture("three-page.pdf");
        let mut source = fixture("two-page.pdf");
        apply_overlay_spec(
            &mut dest,
            &mut source,
            OverlayKind::Overlay,
            &pr(""),
            &pr(""),
            None,
        )
        .unwrap();
        let actual = write_static_id(&mut dest);
        assert_byte_identical(&actual, "three-page-overlay-two-page.pdf");
    }

    #[test]
    fn overlay_one_page_repeat1_is_byte_identical() {
        // dest=three-page, overlay source=one-page, --repeat=1: every dest page
        // shares the SAME imported XObject (obj9 in the golden).
        let mut dest = fixture("three-page.pdf");
        let mut source = fixture("one-page.pdf");
        apply_overlay_spec(
            &mut dest,
            &mut source,
            OverlayKind::Overlay,
            &pr(""),
            &pr(""),
            Some(&pr("1")),
        )
        .unwrap();
        let actual = write_static_id(&mut dest);
        assert_byte_identical(&actual, "three-page-overlay-one-page-repeat1.pdf");
    }

    #[test]
    fn overlay_two_page_to2_3_is_byte_identical() {
        // dest=three-page, overlay source=two-page, --to=2-3: p1 untouched,
        // p2<-s1, p3<-s2.
        let mut dest = fixture("three-page.pdf");
        let mut source = fixture("two-page.pdf");
        apply_overlay_spec(
            &mut dest,
            &mut source,
            OverlayKind::Overlay,
            &pr(""),
            &pr("2-3"),
            None,
        )
        .unwrap();
        let actual = write_static_id(&mut dest);
        assert_byte_identical(&actual, "three-page-overlay-two-page-to2-3.pdf");
    }

    #[test]
    fn overlay_two_page_from2_is_byte_identical() {
        // dest=three-page, overlay source=two-page, --from=2: the source range
        // starts at page 2, so p1<-s2 and then the source is exhausted (p2, p3
        // untouched).
        let mut dest = fixture("three-page.pdf");
        let mut source = fixture("two-page.pdf");
        apply_overlay_spec(
            &mut dest,
            &mut source,
            OverlayKind::Overlay,
            &pr("2"),
            &pr(""),
            None,
        )
        .unwrap();
        let actual = write_static_id(&mut dest);
        assert_byte_identical(&actual, "three-page-overlay-two-page-from2.pdf");
    }

    #[test]
    fn overlay_two_page_from_empty_repeat2_is_byte_identical() {
        // dest=three-page, overlay source=two-page, explicit empty --from= with
        // --repeat=2: an empty `from` set means `--repeat` cycles from the first
        // dest page, so every dest page receives source page 2. This pins the
        // empty-from path that `PageRange::empty()` enables (distinct from an
        // absent `--from`, which would map p1<-s1, p2<-s2, p3 untouched).
        let mut dest = fixture("three-page.pdf");
        let mut source = fixture("two-page.pdf");
        apply_overlay_spec(
            &mut dest,
            &mut source,
            OverlayKind::Overlay,
            &PageRange::empty(),
            &pr(""),
            Some(&pr("2")),
        )
        .unwrap();
        let actual = write_static_id(&mut dest);
        assert_byte_identical(
            &actual,
            "three-page-overlay-two-page-from-empty-repeat2.pdf",
        );
    }

    #[test]
    fn underlay_two_page_default_is_byte_identical() {
        // dest=three-page, single --underlay source=two-page, defaults: p1<-s1,
        // p2<-s2, p3 untouched. The source is drawn BENEATH the page (Fx1 placed
        // before Fx0 in the new content stream).
        let mut dest = fixture("three-page.pdf");
        let mut source = fixture("two-page.pdf");
        apply_overlay_spec(
            &mut dest,
            &mut source,
            OverlayKind::Underlay,
            &pr(""),
            &pr(""),
            None,
        )
        .unwrap();
        let actual = write_static_id(&mut dest);
        assert_byte_identical(&actual, "three-page-underlay-two-page.pdf");
    }

    #[test]
    fn overlay_rotated_source_is_byte_identical() {
        // dest=three-page, overlay source=one-page-r90 (a +90-rotated page). The
        // imported Form XObject carries a non-identity /Matrix encoding the
        // rotation, and the placement `cm` must fit the matrix-TRANSFORMED bbox
        // (the visual extent), not the raw /BBox. A whole-file byte match proves
        // both the /Matrix import and the matrix-transformed cm fragment.
        let mut dest = fixture("three-page.pdf");
        let mut source = fixture("one-page-r90.pdf");
        apply_overlay_spec(
            &mut dest,
            &mut source,
            OverlayKind::Overlay,
            &pr(""),
            &pr(""),
            None,
        )
        .unwrap();
        let actual = write_static_id(&mut dest);
        assert_byte_identical(&actual, "three-page-overlay-rotated.pdf");
    }

    #[test]
    // ---- copy-annotations parity (flpdf-9hc.34) -------------------------
    //
    // Primary target for the overlay/underlay copyAnnotations parity work:
    // qpdf 11.9.0's `qpdf/qtest/copy-annotations.test` line 19-28.
    // fxo-red.pdf (16-page dest, no /AcroForm) --overlay
    // form-fields-and-annotations.pdf --repeat=1 (1-page source with 5 widget
    // annots over 3 top-level fields including a radio group). Because the
    // single source page is repeated onto every dest page, the +N rename path
    // fires from placement 2 onward (r1..r1+15, "Text Box 1"..+15, etc.).
    #[test]
    fn overlay_copy_annotations_fxo_red_repeat1_is_byte_identical_qdf() {
        let mut dest = fixture("fxo-red.pdf");
        let mut src = fixture("form-fields-and-annotations.pdf");
        // qpdf floors the output at max(dest, all sources) — form-fields-and-
        // annotations.pdf is PDF 1.6, fxo-red.pdf is PDF 1.3, so the output
        // header must be 1.6.
        let ((maj, min), max_ext) = accumulate_max(&mut dest, &mut src);
        let mut specs = vec![OverlaySpec {
            source: src,
            kind: OverlayKind::Overlay,
            from: pr(""),
            to: pr(""),
            repeat: Some(pr("1")),
        }];
        apply_overlay_specs(&mut dest, &mut specs).unwrap();
        let opts = WriteOptions {
            full_rewrite: true,
            static_id: true,
            qdf: true,
            no_original_object_ids: true,
            min_version: Some(format!("{maj}.{min}")),
            min_extension_level: (max_ext > 0).then_some(max_ext),
            ..Default::default()
        };
        let mut actual = Vec::new();
        write_pdf_with_options(&mut dest, &mut actual, &opts).unwrap();
        assert_byte_identical(&actual, "overlay-copy-annotations.pdf");
    }

    /// Overlay a source that mixes two edge shapes into its page's
    /// `/Annots` array:
    /// - one widget (obj 3, "Text Box 1") carries an explicit `/P`
    ///   pointing at the source page — after copy that ref goes stale
    ///   and gets Null'd by rewrite_refs, so `apply_placement`'s
    ///   `set_annot_page_ref_if_null` must repoint it at dest_page_ref;
    /// - one entry is a DIRECT annot dictionary (an inline
    ///   `<< /Subtype /FreeText ... >>` where an indirect ref would
    ///   normally live) — `survey_source_annotations` must materialize
    ///   it into a fresh source-doc indirect object (qpdf
    ///   transformAnnotations line 954-956).
    ///
    /// Fixture: `form-fields-and-annotations-p-and-inline.pdf` is the
    /// primary source with `/P 17 0 R` added to Text Box 1 and one
    /// FreeText annot inlined into the page's `/Annots`.
    #[test]
    fn overlay_copy_annotations_source_p_and_inline_is_byte_identical_qdf() {
        let mut dest = fixture("fxo-red.pdf");
        let mut src = fixture("form-fields-and-annotations-p-and-inline.pdf");
        let ((maj, min), max_ext) = accumulate_max(&mut dest, &mut src);
        let mut specs = vec![OverlaySpec {
            source: src,
            kind: OverlayKind::Overlay,
            from: pr(""),
            to: pr(""),
            repeat: Some(pr("1")),
        }];
        apply_overlay_specs(&mut dest, &mut specs).unwrap();
        let opts = WriteOptions {
            full_rewrite: true,
            static_id: true,
            qdf: true,
            no_original_object_ids: true,
            min_version: Some(format!("{maj}.{min}")),
            min_extension_level: (max_ext > 0).then_some(max_ext),
            ..Default::default()
        };
        let mut actual = Vec::new();
        write_pdf_with_options(&mut dest, &mut actual, &opts).unwrap();
        assert_byte_identical(&actual, "overlay-source-p-and-inline.pdf");
    }

    /// Overlay a source whose `/AcroForm/DR` is stored inline as a direct
    /// dictionary (rather than the usual indirect ref). Exercises
    /// `read_source_acroform_defaults`' direct-`/DR` materialize path
    /// (allocate a fresh source-doc indirect object, register the direct
    /// dict on it, and return that ref for downstream copy).
    #[test]
    fn overlay_copy_annotations_source_direct_dr_is_byte_identical_qdf() {
        let mut dest = fixture("fxo-red.pdf");
        let mut src = fixture("form-fields-and-annotations-direct-dr.pdf");
        let ((maj, min), max_ext) = accumulate_max(&mut dest, &mut src);
        let mut specs = vec![OverlaySpec {
            source: src,
            kind: OverlayKind::Overlay,
            from: pr(""),
            to: pr(""),
            repeat: Some(pr("1")),
        }];
        apply_overlay_specs(&mut dest, &mut specs).unwrap();
        let opts = WriteOptions {
            full_rewrite: true,
            static_id: true,
            qdf: true,
            no_original_object_ids: true,
            min_version: Some(format!("{maj}.{min}")),
            min_extension_level: (max_ext > 0).then_some(max_ext),
            ..Default::default()
        };
        let mut actual = Vec::new();
        write_pdf_with_options(&mut dest, &mut actual, &opts).unwrap();
        assert_byte_identical(&actual, "overlay-source-direct-dr.pdf");
    }

    /// Overlay `form-fields-and-annotations.pdf` onto a dest that already
    /// carries an `/AcroForm` with a pre-existing `/Fields` entry named
    /// "Text Box 1" — the same partial name as one of the source's
    /// top-level fields, so the +N collision rename must fire once for
    /// every placement (the source page is repeated onto all 16 dest
    /// pages, so the rename runs 16 times: "Text Box 1+1", "Text Box 1+2",
    /// ...). Also exercises `ensure_dest_acroform_dr`'s existing-`/DR`
    /// short-circuit, `add_and_rename_form_fields`'s reference-`/AcroForm`
    /// / reference-`/Fields` paths, `collect_fully_qualified_names` over
    /// the pre-existing field, and the tail of `duplicate_field_tree` that
    /// leaves an existing dest `/DR` untouched.
    ///
    /// Fixture: `fxo-red-with-existing-acroform.pdf` is fxo-red with a
    /// small hand-added `/AcroForm { /DR ... /Fields [<field>] }` whose
    /// field has `/T (Text Box 1)`.
    #[test]
    fn overlay_copy_annotations_onto_existing_acroform_is_byte_identical_qdf() {
        let mut dest = fixture("fxo-red-with-existing-acroform.pdf");
        let mut src = fixture("form-fields-and-annotations.pdf");
        let ((maj, min), max_ext) = accumulate_max(&mut dest, &mut src);
        let mut specs = vec![OverlaySpec {
            source: src,
            kind: OverlayKind::Overlay,
            from: pr(""),
            to: pr(""),
            repeat: Some(pr("1")),
        }];
        apply_overlay_specs(&mut dest, &mut specs).unwrap();
        let opts = WriteOptions {
            full_rewrite: true,
            static_id: true,
            qdf: true,
            no_original_object_ids: true,
            min_version: Some(format!("{maj}.{min}")),
            min_extension_level: (max_ext > 0).then_some(max_ext),
            ..Default::default()
        };
        let mut actual = Vec::new();
        write_pdf_with_options(&mut dest, &mut actual, &opts).unwrap();
        assert_byte_identical(&actual, "overlay-onto-existing-acroform.pdf");
    }

    /// Overlay a source whose `/AcroForm` supplies `/DA` and `/Q` defaults
    /// onto a dest with no `/AcroForm`. Exercises qpdf's
    /// `adjustInheritedFields` (line 442-484, called from
    /// transformAnnotations line 914-917) — a copied field that inherits
    /// its default appearance / quadding from the source `/AcroForm` gets
    /// the value pinned on the field itself so the (different / absent)
    /// dest default is not silently inherited.
    ///
    /// Fixture: `form-fields-and-annotations-with-defaults.pdf` is
    /// `form-fields-and-annotations.pdf` with `/DA (/ZaDi 0 Tf 0 g)` and
    /// `/Q 1` added at the `/AcroForm` level (nothing else changed). Dest
    /// remains fxo-red (no `/AcroForm`), so `override_da` and
    /// `override_q` both fire and every copied field runs through
    /// `adjust_inherited_field` + `ancestor_has_key`.
    #[test]
    fn overlay_copy_annotations_with_da_q_defaults_is_byte_identical_qdf() {
        let mut dest = fixture("fxo-red.pdf");
        let mut src = fixture("form-fields-and-annotations-with-defaults.pdf");
        let ((maj, min), max_ext) = accumulate_max(&mut dest, &mut src);
        let mut specs = vec![OverlaySpec {
            source: src,
            kind: OverlayKind::Overlay,
            from: pr(""),
            to: pr(""),
            repeat: Some(pr("1")),
        }];
        apply_overlay_specs(&mut dest, &mut specs).unwrap();
        let opts = WriteOptions {
            full_rewrite: true,
            static_id: true,
            qdf: true,
            no_original_object_ids: true,
            min_version: Some(format!("{maj}.{min}")),
            min_extension_level: (max_ext > 0).then_some(max_ext),
            ..Default::default()
        };
        let mut actual = Vec::new();
        write_pdf_with_options(&mut dest, &mut actual, &opts).unwrap();
        assert_byte_identical(&actual, "overlay-copy-annotations-with-defaults.pdf");
    }

    /// Underlay counterpart of the primary copy-annotations byte gate.
    /// Same fixture (fxo-red + form-fields-and-annotations, --repeat=1),
    /// same expected annotation copy behaviour (qpdf's
    /// `doUnderOverlayForPage` shares the codepath for both kinds and
    /// differs only in the content-stream placement order), but exercises
    /// [`apply_overlay_specs`]'s underlay branch and the accompanying
    /// [`apply_placement`] call inside it — the mirror of the overlay
    /// branch already covered above.
    #[test]
    fn underlay_copy_annotations_fxo_red_repeat1_is_byte_identical_qdf() {
        let mut dest = fixture("fxo-red.pdf");
        let mut src = fixture("form-fields-and-annotations.pdf");
        let ((maj, min), max_ext) = accumulate_max(&mut dest, &mut src);
        let mut specs = vec![OverlaySpec {
            source: src,
            kind: OverlayKind::Underlay,
            from: pr(""),
            to: pr(""),
            repeat: Some(pr("1")),
        }];
        apply_overlay_specs(&mut dest, &mut specs).unwrap();
        let opts = WriteOptions {
            full_rewrite: true,
            static_id: true,
            qdf: true,
            no_original_object_ids: true,
            min_version: Some(format!("{maj}.{min}")),
            min_extension_level: (max_ext > 0).then_some(max_ext),
            ..Default::default()
        };
        let mut actual = Vec::new();
        write_pdf_with_options(&mut dest, &mut actual, &opts).unwrap();
        assert_byte_identical(&actual, "underlay-copy-annotations.pdf");
    }

    fn overlay_one_page_to1_3_repeat1_is_byte_identical() {
        // dest=three-page, overlay source=one-page, --to=1-3 --repeat=1: every
        // dest page is selected and the single source page cycles via --repeat,
        // so all three pages share the SAME imported XObject.
        let mut dest = fixture("three-page.pdf");
        let mut source = fixture("one-page.pdf");
        apply_overlay_spec(
            &mut dest,
            &mut source,
            OverlayKind::Overlay,
            &pr(""),
            &pr("1-3"),
            Some(&pr("1")),
        )
        .unwrap();
        let actual = write_static_id(&mut dest);
        assert_byte_identical(&actual, "three-page-overlay-to-repeat.pdf");
    }

    #[test]
    fn overlay_multi_stream_source_is_byte_identical() {
        // dest=three-page, overlay source=multi-stream-one-page (no /Rotate, a
        // two-element /Contents array whose first stream does not end in a
        // newline). The imported Form XObject must OMIT /Matrix (no /Rotate or
        // /UserUnit) and coalesce the two content streams with qpdf's newline rule
        // (a single '\n' between them). A whole-file match proves the
        // /Matrix-omission (gap 1) and newline coalescing (gap 2). The source is
        // pinned to PDF 1.3 (== dest) so the orthogonal source version-floor
        // limitation does not perturb the bytes.
        let mut dest = fixture("three-page.pdf");
        let mut source = fixture("multi-stream-one-page.pdf");
        apply_overlay_spec(
            &mut dest,
            &mut source,
            OverlayKind::Overlay,
            &pr(""),
            &pr(""),
            None,
        )
        .unwrap();
        let actual = write_static_id(&mut dest);
        assert_byte_identical(&actual, "three-page-overlay-multi-stream.pdf");
    }

    #[test]
    fn overlay_onto_rotated_dest_is_byte_identical() {
        // dest=one-page-r90 (a +90-rotated page), overlay source=one-page. The
        // destination's inverse transform is folded into BOTH the /Fx0 placement
        // (cm "0 1 -1 0 612 0") and the source placement
        // ("0 0.77273 -0.77273 0 612 159.54545") — the nonzero b/c prove the dest
        // inverse transform is applied (gap 3).
        let mut dest = fixture("one-page-r90.pdf");
        let mut source = fixture("one-page.pdf");
        apply_overlay_spec(
            &mut dest,
            &mut source,
            OverlayKind::Overlay,
            &pr(""),
            &pr(""),
            None,
        )
        .unwrap();
        let actual = write_static_id(&mut dest);
        assert_byte_identical(&actual, "r90-dest-overlay-one-page.pdf");
    }

    #[test]
    fn overlay_userunit_source_is_byte_identical() {
        // dest=three-page, overlay source=userunit-one-page (/UserUnit 2, no
        // /Rotate, pinned to PDF 1.3 == dest). The imported Form XObject's /Matrix
        // folds the unit scale in ([2 0 0 2 0 0]); a whole-file match proves the
        // /UserUnit scale (gap 4).
        let mut dest = fixture("three-page.pdf");
        let mut source = fixture("userunit-one-page.pdf");
        apply_overlay_spec(
            &mut dest,
            &mut source,
            OverlayKind::Overlay,
            &pr(""),
            &pr(""),
            None,
        )
        .unwrap();
        let actual = write_static_id(&mut dest);
        assert_byte_identical(&actual, "three-page-overlay-userunit.pdf");
    }

    /// Build a default-range [`OverlaySpec`] over a fixture document.
    fn spec(name: &str, kind: OverlayKind) -> OverlaySpec<std::io::BufReader<std::fs::File>> {
        OverlaySpec {
            source: fixture(name),
            kind,
            from: pr(""),
            to: pr(""),
            repeat: None,
        }
    }

    #[test]
    fn two_overlays_compose_byte_identical() {
        // dest=three-page, --overlay one-page -- --overlay two-page --.
        // Page 1: Fx0, Fx1(overlay one s1), Fx2(overlay two s1); page 2: Fx0,
        // Fx1(overlay two s2); page 3 untouched.
        let mut dest = fixture("three-page.pdf");
        let mut specs = vec![
            spec("one-page.pdf", OverlayKind::Overlay),
            spec("two-page.pdf", OverlayKind::Overlay),
        ];
        apply_overlay_specs(&mut dest, &mut specs).unwrap();
        let actual = write_static_id(&mut dest);
        assert_byte_identical(&actual, "three-page-two-overlays.pdf");
    }

    #[test]
    fn overlay_and_underlay_compose_byte_identical() {
        // dest=three-page, --overlay one-page -- --underlay two-page --.
        // Page 1: Fx1(underlay two s1) drawn before Fx0, Fx2(overlay one s1)
        // after; page 2: Fx1(underlay two s2) before Fx0; page 3 untouched.
        // Naming is under-then-over across specs even though overlay is declared
        // first.
        let mut dest = fixture("three-page.pdf");
        let mut specs = vec![
            spec("one-page.pdf", OverlayKind::Overlay),
            spec("two-page.pdf", OverlayKind::Underlay),
        ];
        apply_overlay_specs(&mut dest, &mut specs).unwrap();
        let actual = write_static_id(&mut dest);
        assert_byte_identical(&actual, "three-page-overlay-and-underlay.pdf");
    }

    #[test]
    fn swapped_box_overlay_one_page_is_byte_identical() {
        // dest = swapped-box-one-page (reversed /MediaBox [612 792 0 0]),
        // source = one-page. The placement rect is read like qpdf
        // getArrayAsRectangle, so it normalizes to [0 0 612 792] and the source
        // places at identity; a raw rect would yield the reflected cm
        // "-1 0 0 -1 612 792". Proves the placement-rect normalization (Edit C).
        let mut dest = fixture("swapped-box-one-page.pdf");
        let mut source = fixture("one-page.pdf");
        apply_overlay_spec(
            &mut dest,
            &mut source,
            OverlayKind::Overlay,
            &pr(""),
            &pr(""),
            None,
        )
        .unwrap();
        let actual = write_static_id(&mut dest);
        assert_byte_identical(&actual, "swapped-box-overlay-one-page.pdf");
    }

    #[test]
    fn swapped_box_r90_overlay_self_is_byte_identical() {
        // dest = source = swapped-box-r90-one-page (reversed box + /Rotate 90),
        // overlaid onto itself. The /Rotate makes the source/dest Form /Matrix
        // depend on the box width/height, and that /Matrix array is serialized, so
        // this proves the /Matrix-dim normalization (Edit A) on top of the placement
        // rects (Edit C). (The dest tmatrix dims are normalized too, but their effect
        // -- the tmatrix translation -- is absorbed by the placement centring, so it
        // is an output no-op this gate cannot isolate.)
        let mut dest = fixture("swapped-box-r90-one-page.pdf");
        let mut source = fixture("swapped-box-r90-one-page.pdf");
        apply_overlay_spec(
            &mut dest,
            &mut source,
            OverlayKind::Overlay,
            &pr(""),
            &pr(""),
            None,
        )
        .unwrap();
        let actual = write_static_id(&mut dest);
        assert_byte_identical(&actual, "swapped-box-r90-overlay-self.pdf");
    }

    // ---- source version-floor propagation --------------------------------
    //
    // These gates prove the writer half of qpdf's cross-source version rule
    // in isolation from the CLI. The CLI wires the same accumulation into
    // its overlay/underlay pipeline; here the test mirrors it explicitly so
    // WriteOptions.min_version / min_extension_level are the sole inputs
    // exercised at the library boundary.
    use std::io::{Read, Seek};

    /// Return `(max_pdf_version, max_extension_level)` over two open PDFs
    /// using qpdf's pairwise rule: the higher version wins outright, and a
    /// higher version RESETS the extension level (only equal versions merge
    /// via `max`). Mirrors what `flpdf rewrite` needs to accumulate across
    /// dest + all overlay/underlay sources.
    fn accumulate_max<R1: Read + Seek, R2: Read + Seek>(
        a: &mut Pdf<R1>,
        b: &mut Pdf<R2>,
    ) -> ((u8, u8), i64) {
        let va = crate::writer::parse_pdf_version(a.version()).unwrap_or((1, 0));
        let vb = crate::writer::parse_pdf_version(b.version()).unwrap_or((1, 0));
        let ea = a.adobe_extension_level().unwrap_or(0);
        let eb = b.adobe_extension_level().unwrap_or(0);
        match va.cmp(&vb) {
            std::cmp::Ordering::Greater => (va, ea),
            std::cmp::Ordering::Less => (vb, eb),
            std::cmp::Ordering::Equal => (va, ea.max(eb)),
        }
    }

    /// Resolve a workspace-relative path (from the repo root) to an absolute
    /// path so `cargo test` works from any cwd. Matches the neighbouring
    /// `fixture` / `golden` helpers' use of `CARGO_MANIFEST_DIR`.
    fn fixture_path(rel: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(rel)
    }

    #[test]
    fn overlay_pure_source_version_floor_bytes() {
        use std::fs;

        let dest_bytes = fs::read(fixture_path("tests/fixtures/compat/three-page.pdf"))
            .expect("read dest fixture");
        let source_bytes = fs::read(fixture_path("tests/fixtures/compat/one-page-v17.pdf"))
            .expect("read source fixture");
        let golden = fs::read(fixture_path(
            "tests/golden/references/overlay/three-page-overlay-v17-source.pdf",
        ))
        .expect("read golden");

        let mut dest = Pdf::open(std::io::Cursor::new(dest_bytes)).expect("open dest");
        let mut src = Pdf::open(std::io::Cursor::new(source_bytes)).expect("open source");

        // Mirror flpdf-cli accumulation manually: the CLI walks dest and
        // every overlay/underlay source; here there is exactly one source.
        let ((maj, min), max_ext) = accumulate_max(&mut dest, &mut src);

        let mut specs = vec![OverlaySpec {
            source: src,
            kind: OverlayKind::Overlay,
            from: pr(""),
            to: pr(""),
            repeat: None,
        }];
        apply_overlay_specs(&mut dest, &mut specs).expect("apply overlay");

        let opts = WriteOptions {
            full_rewrite: true,
            static_id: true,
            min_version: Some(format!("{maj}.{min}")),
            min_extension_level: (max_ext > 0).then_some(max_ext),
            newline_before_endstream: NewlineBeforeEndstream::Never,
            ..Default::default()
        };
        let mut out = Vec::new();
        write_pdf_with_options(&mut dest, &mut out, &opts).expect("write");

        if let Some(off) = first_diff(&out, &golden) {
            let lo = off.saturating_sub(24);
            let g = golden.get(off).copied().unwrap_or(0);
            let f = out.get(off).copied().unwrap_or(0);
            panic!(
                "overlay output not byte-identical to qpdf golden \
                 three-page-overlay-v17-source.pdf \
                 (flpdf={} bytes, golden={} bytes)\n\
                 first diff at offset {off} (golden=0x{g:02x} flpdf=0x{f:02x})\n\
                 golden[{lo}..]: {:?}\nflpdf [{lo}..]: {:?}",
                out.len(),
                golden.len(),
                String::from_utf8_lossy(&golden[lo..(off + 24).min(golden.len())]),
                String::from_utf8_lossy(&out[lo..(off + 24).min(out.len())]),
            );
        }
    }

    #[test]
    fn overlay_encrypted_source_extension_level_bytes() {
        use std::fs;

        let dest_bytes = fs::read(fixture_path("tests/fixtures/compat/three-page.pdf"))
            .expect("read dest fixture");
        let source_bytes = fs::read(fixture_path("tests/fixtures/compat/one-page-enc-u.pdf"))
            .expect("read encrypted source fixture");
        let golden = fs::read(fixture_path(
            "tests/golden/references/overlay/three-page-overlay-encrypted-source.pdf",
        ))
        .expect("read encrypted-source golden");

        let mut dest = Pdf::open(std::io::Cursor::new(dest_bytes)).expect("open dest");
        let src_open_opts = crate::PdfOpenOptions {
            password: b"u".to_vec(),
            ..crate::PdfOpenOptions::default()
        };
        let mut src = Pdf::open_with_options(std::io::Cursor::new(source_bytes), src_open_opts)
            .expect("open encrypted source");

        // Mirror flpdf-cli accumulation manually: the CLI walks dest and
        // every overlay/underlay source; here there is exactly one source.
        let ((maj, min), max_ext) = accumulate_max(&mut dest, &mut src);

        let mut specs = vec![OverlaySpec {
            source: src,
            kind: OverlayKind::Overlay,
            from: pr(""),
            to: pr(""),
            repeat: None,
        }];
        apply_overlay_specs(&mut dest, &mut specs).expect("apply overlay");

        let opts = WriteOptions {
            full_rewrite: true,
            static_id: true,
            min_version: Some(format!("{maj}.{min}")),
            min_extension_level: (max_ext > 0).then_some(max_ext),
            newline_before_endstream: NewlineBeforeEndstream::Never,
            ..Default::default()
        };
        let mut out = Vec::new();
        write_pdf_with_options(&mut dest, &mut out, &opts).expect("write");

        if let Some(off) = first_diff(&out, &golden) {
            let lo = off.saturating_sub(24);
            let g = golden.get(off).copied().unwrap_or(0);
            let f = out.get(off).copied().unwrap_or(0);
            panic!(
                "overlay output not byte-identical to qpdf golden \
                 three-page-overlay-encrypted-source.pdf \
                 (flpdf={} bytes, golden={} bytes)\n\
                 first diff at offset {off} (golden=0x{g:02x} flpdf=0x{f:02x})\n\
                 golden[{lo}..]: {:?}\nflpdf [{lo}..]: {:?}",
                out.len(),
                golden.len(),
                String::from_utf8_lossy(&golden[lo..(off + 24).min(golden.len())]),
                String::from_utf8_lossy(&out[lo..(off + 24).min(out.len())]),
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

    // ---- qpdf matrix primitives ------------------------------------------

    #[test]
    fn qpdf_concat_matches_qpdf_arithmetic() {
        // this=[2 0 0 2 0 0] (scale 2) concat other=[1 0 0 1 5 7] (translate):
        // ap=2; bp=0; cp=0; dp=2; ep=2*5+0*7+0=10; fp=0*5+2*7+0=14.
        assert_eq!(
            qpdf_concat(
                [2.0, 0.0, 0.0, 2.0, 0.0, 0.0],
                [1.0, 0.0, 0.0, 1.0, 5.0, 7.0]
            ),
            [2.0, 0.0, 0.0, 2.0, 10.0, 14.0]
        );
    }

    #[test]
    fn matrix_unparse_applies_fix_rounding_and_trims() {
        // fix_rounding zeroes a sub-0.00001 component before formatting.
        assert_eq!(
            matrix_unparse([0.000_004, 1.0, 0.0, 1.0, 0.0, 0.0]),
            "0 1 0 1 0 0"
        );
        assert_eq!(
            matrix_unparse([0.772_73, 0.0, 0.0, 0.772_73, 0.0, 159.545_45]),
            "0.77273 0 0 0.77273 0 159.54545"
        );
    }

    // ---- place_form_xobject ----------------------------------------------

    /// The identity transformation matrix `[1 0 0 1 0 0]`.
    const ID: [f64; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

    #[test]
    fn place_identity_when_same_size() {
        // BBox == rect (612x792 at origin), no fo/dest transform -> identity, centred.
        let (frag, _cm) = place_form_xobject(
            [0.0, 0.0, 612.0, 792.0],
            ID,
            [0.0, 0.0, 612.0, 792.0],
            ID,
            true,
            false,
            "Fx0",
        );
        assert_eq!(frag, "q\n1 0 0 1 0 0 cm\n/Fx0 Do\nQ\n");
    }

    #[test]
    fn place_centers_smaller_bbox_without_scaling() {
        // 300x144 source into 612x792 dest: no scale-up; centred at
        // tx = 306 - 150 = 156, ty = 396 - 72 = 324.
        let (frag, _cm) = place_form_xobject(
            [0.0, 0.0, 300.0, 144.0],
            ID,
            [0.0, 0.0, 612.0, 792.0],
            ID,
            true,
            false,
            "Fx1",
        );
        assert_eq!(frag, "q\n1 0 0 1 156 324 cm\n/Fx1 Do\nQ\n");
    }

    #[test]
    fn place_shrinks_larger_bbox_to_fit() {
        // 612x792 source into 300x144 dest with allow_shrink: scale = min(300/612,
        // 144/792) = 0.18182 (5dp). tx -> "94.36364", ty -> "0".
        let (frag, _cm) = place_form_xobject(
            [0.0, 0.0, 612.0, 792.0],
            ID,
            [0.0, 0.0, 300.0, 144.0],
            ID,
            true,
            false,
            "Fx0",
        );
        assert_eq!(frag, "q\n0.18182 0 0 0.18182 94.36364 0 cm\n/Fx0 Do\nQ\n");
    }

    #[test]
    fn place_allow_shrink_false_clamps_scale_to_one() {
        // Same oversize source, but allow_shrink=false (the /Fx0 flags): the
        // would-be <1 scale is clamped to 1 and the bbox is centred unscaled.
        // scale 1; t_cx=306, t_cy=396; r_cx=150, r_cy=72; tx=-156, ty=-324.
        let (frag, _cm) = place_form_xobject(
            [0.0, 0.0, 612.0, 792.0],
            ID,
            [0.0, 0.0, 300.0, 144.0],
            ID,
            false,
            false,
            "Fx0",
        );
        assert_eq!(frag, "q\n1 0 0 1 -156 -324 cm\n/Fx0 Do\nQ\n");
    }

    #[test]
    fn place_fractional_center() {
        // 301x145 source into 612x792 dest: no scale; tx = 306 - 150.5 = 155.5,
        // ty = 396 - 72.5 = 323.5.
        let (frag, _cm) = place_form_xobject(
            [0.0, 0.0, 301.0, 145.0],
            ID,
            [0.0, 0.0, 612.0, 792.0],
            ID,
            true,
            false,
            "Fx2",
        );
        assert_eq!(frag, "q\n1 0 0 1 155.5 323.5 cm\n/Fx2 Do\nQ\n");
    }

    #[test]
    fn place_handles_zero_area_bbox_as_identity() {
        // A degenerate /BBox (zero width) gives qpdf a degenerate transformed
        // rectangle, so getMatrixForFormXObjectPlacement returns the identity
        // (NOT a centred scale-1 placement). Mirrors qpdf 11.9.0.
        let (frag, _cm) = place_form_xobject(
            [0.0, 0.0, 0.0, 100.0],
            ID,
            [0.0, 0.0, 200.0, 200.0],
            ID,
            true,
            false,
            "Fx1",
        );
        assert_eq!(frag, "q\n1 0 0 1 0 0 cm\n/Fx1 Do\nQ\n");
    }

    #[test]
    fn place_uses_nonzero_bbox_origin_center() {
        // /BBox origin is non-zero: centre uses (llx+urx)/2, (lly+ury)/2.
        // BBox [10 10 510 610] -> w=500 h=600 into rect [0 0 612 792].
        // scale = min(612/500, 792/600) -> clamped to 1; tx=46, ty=86.
        let (frag, _cm) = place_form_xobject(
            [10.0, 10.0, 510.0, 610.0],
            ID,
            [0.0, 0.0, 612.0, 792.0],
            ID,
            true,
            false,
            "Fx0",
        );
        assert_eq!(frag, "q\n1 0 0 1 46 86 cm\n/Fx0 Do\nQ\n");
    }

    #[test]
    fn place_fx0_into_rotated_dest_uses_inverse_transform() {
        // /Fx0 placement onto a +90-rotated 612x792 dest page. The dest inverse
        // transform tmatrix = getMatrixForTransformations(true) = [0 1 -1 0 792 0];
        // the page-as-XObject carries /Matrix [0 -1 1 0 0 612] and /BBox
        // [0 0 612 792]; rect = MediaBox [0 0 612 792]; allow_shrink=false.
        // The resulting cm un-rotates the page: [0 1 -1 0 612 0]. The nonzero b/c
        // (impossible for the old axis-aligned placement) prove the dest inverse
        // transform is folded in.
        let tmatrix = [0.0, 1.0, -1.0, 0.0, 792.0, 0.0];
        let (frag, _cm) = place_form_xobject(
            [0.0, 0.0, 612.0, 792.0],
            [0.0, -1.0, 1.0, 0.0, 0.0, 612.0],
            [0.0, 0.0, 612.0, 792.0],
            tmatrix,
            false,
            false,
            "Fx0",
        );
        assert_eq!(frag, "q\n0 1 -1 0 612 0 cm\n/Fx0 Do\nQ\n");
    }

    #[test]
    fn normalize_rectangle_orders_swapped_corners() {
        // Reversed box [612 792 0 0] -> [0 0 612 792]; an already-ordered box is
        // unchanged (qpdf getArrayAsRectangle = min/max of paired corners).
        assert_eq!(
            normalize_rectangle([612.0, 792.0, 0.0, 0.0]),
            [0.0, 0.0, 612.0, 792.0]
        );
        assert_eq!(
            normalize_rectangle([0.0, 0.0, 612.0, 792.0]),
            [0.0, 0.0, 612.0, 792.0]
        );
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
                annot_template: None,
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
                    annot_template: None,
                },
                OverlaySource {
                    kind: OverlayKind::Underlay,
                    xobject_ref: underlay,
                    annot_template: None,
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
                annot_template: None,
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

    /// Overlay a fixed 100x100 source onto a one-page dest with the given
    /// `/MediaBox` array literal and optional `/Rotate` entry, returning the
    /// rewritten page `/Contents` bytes. Used to prove a reversed box places
    /// identically to its ordered (normalized) form.
    fn overlay_contents(media_box: &str, rotate: &str) -> Vec<u8> {
        let page = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox {media_box} {rotate} \
             /Resources << >> /Contents 4 0 R >>"
        );
        let mut pdf = open(build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (3, &page),
                (4, "<< /Length 1 >>\nstream\nx\nendstream"),
            ],
            1,
        ));
        let page_ref = ObjectRef::new(3, 0);
        let src = insert_form_xobject(&mut pdf, [0, 0, 100, 100], b"src");
        apply_overlays_to_page(
            &mut pdf,
            page_ref,
            &[OverlaySource {
                kind: OverlayKind::Overlay,
                xobject_ref: src,
                annot_template: None,
            }],
        )
        .unwrap();
        let contents_ref = match pdf
            .resolve(page_ref)
            .unwrap()
            .as_dict()
            .unwrap()
            .get("Contents")
        {
            Some(Object::Reference(r)) => *r,
            other => panic!("Contents ref: {other:?}"), // cov:ignore: defensive — apply always writes /Contents as a reference
        };
        pdf.resolve(contents_ref)
            .unwrap()
            .into_stream()
            .unwrap()
            .data
    }

    #[test]
    fn apply_swapped_mediabox_normalizes_placement_rect() {
        // Dest /MediaBox is reversed ([612 792 0 0]); qpdf reads it through
        // getArrayAsRectangle, so the placement rect normalizes to [0 0 612 792].
        // A 100x100 source then centres into the normalized rect: scale clamps to 1
        // (no expand), tx = 306-50 = 256, ty = 396-50 = 346. A raw (un-normalized)
        // rect would yield a negative width and a wildly different cm.
        let text = String::from_utf8(overlay_contents("[612 792 0 0]", "")).unwrap();
        assert!(
            text.contains("q\n1 0 0 1 256 346 cm\n/Fx1 Do\nQ\n"),
            "source must place into the normalized MediaBox: {text:?}"
        );
    }

    #[test]
    fn apply_swapped_box_with_rotate_matches_normalized() {
        // With /Rotate 90 the dest inverse tmatrix and the page-as-/Fx0 /Matrix both
        // depend on the box width/height. Reading the box through getArrayAsRectangle
        // makes a reversed box place identically to its ordered form, so every cm in
        // the rewritten /Contents is byte-identical between the two.
        let swapped = overlay_contents("[612 792 0 0]", "/Rotate 90");
        let normalized = overlay_contents("[0 0 612 792]", "/Rotate 90");
        assert_eq!(swapped, normalized);
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
        let (bbox, _matrix) = fo_bbox_and_matrix(&mut pdf, r).unwrap();
        assert_eq!(bbox, [0.0, 1.5, 300.0, 144.0]);
    }

    #[test]
    fn fo_bbox_rejects_missing_and_short_box() {
        let mut pdf = open(one_page_doc("x"));
        // Missing /BBox.
        let mut d1 = Dictionary::new();
        d1.insert("Subtype", Object::Name(b"Form".to_vec()));
        let r1 = next_object_ref(&pdf).unwrap();
        pdf.set_object(r1, Object::Stream(Stream::new(d1, Vec::new())));
        assert!(matches!(
            fo_bbox_and_matrix(&mut pdf, r1),
            Err(Error::Unsupported(_))
        ));

        // /BBox too short.
        let mut d2 = Dictionary::new();
        d2.insert("BBox", Object::Array(vec![Object::Integer(0)]));
        let r2 = next_object_ref(&pdf).unwrap();
        pdf.set_object(r2, Object::Stream(Stream::new(d2, Vec::new())));
        assert!(matches!(
            fo_bbox_and_matrix(&mut pdf, r2),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn fo_bbox_rejects_non_stream_non_dict() {
        let mut pdf = open(one_page_doc("x"));
        let r = next_object_ref(&pdf).unwrap();
        pdf.set_object(r, Object::Integer(42));
        assert!(matches!(
            fo_bbox_and_matrix(&mut pdf, r),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn fo_bbox_reads_from_plain_dictionary() {
        // A Form XObject value that is a bare dictionary (not a stream) still
        // yields its /BBox. With no /Matrix the matrix defaults to identity.
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
        let (bbox, matrix) = fo_bbox_and_matrix(&mut pdf, r).unwrap();
        assert_eq!(bbox, [0.0, 0.0, 10.0, 20.0]);
        assert_eq!(matrix, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    }

    #[test]
    fn fo_bbox_resolves_indirect_reference() {
        // /BBox stored as an indirect reference to the array object must be
        // dereferenced, not rejected as "no array".
        let mut pdf = open(one_page_doc("x"));
        let bbox_ref = next_object_ref(&pdf).unwrap();
        pdf.set_object(
            bbox_ref,
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(10),
                Object::Integer(20),
            ]),
        );
        let mut d = Dictionary::new();
        d.insert("BBox", Object::Reference(bbox_ref));
        let r = next_object_ref(&pdf).unwrap();
        pdf.set_object(r, Object::Dictionary(d));
        let (bbox, _matrix) = fo_bbox_and_matrix(&mut pdf, r).unwrap();
        assert_eq!(bbox, [0.0, 0.0, 10.0, 20.0]);
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

    #[test]
    fn transform_bbox_identity_is_unchanged() {
        let id = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        assert_eq!(
            transform_bbox([10.0, 20.0, 300.0, 400.0], id),
            [10.0, 20.0, 300.0, 400.0]
        );
    }

    #[test]
    fn transform_bbox_rotate_90_swaps_extent() {
        // qpdf's getMatrixForTransformations for a +90 page is [0 -1 1 0 0 w].
        // Mapping (x,y) -> (y, w - x) turns a 612x792 box into a 792x612 box.
        let m90 = [0.0, -1.0, 1.0, 0.0, 0.0, 612.0];
        assert_eq!(
            transform_bbox([0.0, 0.0, 612.0, 792.0], m90),
            [0.0, 0.0, 792.0, 612.0]
        );
    }

    #[test]
    fn matrix_or_identity_reads_present_absent_and_short() {
        // Present 6-element /Matrix is read verbatim.
        let mut present = Dictionary::new();
        present.insert(
            "Matrix",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(-1),
                Object::Integer(1),
                Object::Integer(0),
                Object::Integer(0),
                Object::Real(612.0),
            ]),
        );
        assert_eq!(
            matrix_or_identity(&present),
            [0.0, -1.0, 1.0, 0.0, 0.0, 612.0]
        );
        // Absent /Matrix falls back to the identity.
        assert_eq!(
            matrix_or_identity(&Dictionary::new()),
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]
        );
        // A /Matrix with fewer than six elements falls back to the identity.
        let mut short = Dictionary::new();
        short.insert(
            "Matrix",
            Object::Array(vec![Object::Integer(1), Object::Integer(0)]),
        );
        assert_eq!(matrix_or_identity(&short), [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    }

    #[test]
    fn place_uses_matrix_transformed_bbox_for_rotated_form() {
        // A +90-rotated 612x792 source page (Form /Matrix [0 -1 1 0 0 612], /BBox
        // [0 0 612 792]) presents a 792x612 visual box. With an identity dest
        // transform it shrinks to fit a 612x792 rect exactly as qpdf 11.9.0 emits:
        //   0.77273 0 0 0.77273 0 159.54545
        // The fo /Matrix affects scale/translation but does NOT appear in the cm
        // (the PDF interpreter applies it automatically), so b/c stay 0.
        let (frag, _cm) = place_form_xobject(
            [0.0, 0.0, 612.0, 792.0],
            [0.0, -1.0, 1.0, 0.0, 0.0, 612.0],
            [0.0, 0.0, 612.0, 792.0],
            ID,
            true,
            false,
            "Fx1",
        );
        assert_eq!(frag, "q\n0.77273 0 0 0.77273 0 159.54545 cm\n/Fx1 Do\nQ\n");
    }

    #[test]
    fn fo_bbox_and_matrix_reads_bbox_and_matrix() {
        // A Form XObject dict's /BBox and /Matrix are read verbatim (the matrix is
        // applied later inside the placement math, not pre-multiplied here).
        let mut pdf = open(one_page_doc("x"));
        let mut d = Dictionary::new();
        d.insert(
            "BBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(612),
                Object::Integer(792),
            ]),
        );
        d.insert(
            "Matrix",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(-1),
                Object::Integer(1),
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(612),
            ]),
        );
        let r = next_object_ref(&pdf).unwrap();
        pdf.set_object(r, Object::Dictionary(d));
        let (bbox, matrix) = fo_bbox_and_matrix(&mut pdf, r).unwrap();
        assert_eq!(bbox, [0.0, 0.0, 612.0, 792.0]);
        assert_eq!(matrix, [0.0, -1.0, 1.0, 0.0, 0.0, 612.0]);
    }

    // ---- map_overlay_pages (pure) ----------------------------------------

    /// Resolve a page-range string and pin a panic message on failure.
    fn pr(input: &str) -> PageRange {
        PageRange::parse(input).unwrap_or_else(|e| panic!("parse {input:?}: {e}"))
    }

    #[test]
    fn map_two_page_default_pairs_in_order_and_skips_extra() {
        // dest=three-page, source=two-page, defaults: p1<-s1, p2<-s2, p3 none.
        let from = pr("").resolve(2).unwrap();
        let to = pr("").resolve(3).unwrap();
        assert_eq!(map_overlay_pages(&from, &to, &[]), vec![(1, 1), (2, 2)]);
    }

    #[test]
    fn map_one_page_repeat1_cycles_single_source_over_all_dest() {
        // source=one-page, --repeat=1: p1,p2,p3 all <- s1.
        let from = pr("").resolve(1).unwrap();
        let to = pr("").resolve(3).unwrap();
        let repeat = pr("1").resolve(1).unwrap();
        assert_eq!(
            map_overlay_pages(&from, &to, &repeat),
            vec![(1, 1), (2, 1), (3, 1)]
        );
    }

    #[test]
    fn map_to_2_3_pairs_against_the_to_list() {
        // source=two-page, --to=2-3: p2<-s1, p3<-s2 (p1 untouched). Pairing is
        // positional against the --to LIST, not the absolute page numbers.
        let from = pr("").resolve(2).unwrap();
        let to = pr("2-3").resolve(3).unwrap();
        assert_eq!(map_overlay_pages(&from, &to, &[]), vec![(2, 1), (3, 2)]);
    }

    #[test]
    fn map_from_2_uses_offset_source_then_exhausts() {
        // source=two-page, --from=2: p1<-s2, then from exhausted -> p2,p3 none.
        let from = pr("2").resolve(2).unwrap();
        let to = pr("").resolve(3).unwrap();
        assert_eq!(map_overlay_pages(&from, &to, &[]), vec![(1, 2)]);
    }

    #[test]
    fn map_to_1_3_skips_unpaired_dest_when_source_exhausted() {
        // source=one-page, --to=1,3: p1<-s1; p3 is in --to but the single source
        // is exhausted and no --repeat -> p3 gets nothing.
        let from = pr("").resolve(1).unwrap();
        let to = pr("1,3").resolve(3).unwrap();
        assert_eq!(map_overlay_pages(&from, &to, &[]), vec![(1, 1)]);
    }

    #[test]
    fn map_repeat_2_cycles_last_source_past_exhaustion() {
        // source=two-page, --repeat=2: p1<-s1, p2<-s2, then from exhausted ->
        // p3<-repeat[(2-2)%1]=s2.
        let from = pr("").resolve(2).unwrap();
        let to = pr("").resolve(3).unwrap();
        let repeat = pr("2").resolve(2).unwrap();
        assert_eq!(
            map_overlay_pages(&from, &to, &repeat),
            vec![(1, 1), (2, 2), (3, 2)]
        );
    }

    #[test]
    fn map_repeat_cycles_when_more_dest_than_repeat_pages() {
        // Drive the modulo wrap: from exhausted at index 0, repeat=[3,4] cycles
        // 3,4,3,4 across four dest pages.
        let from: Vec<u32> = Vec::new();
        let to = vec![1, 2, 3, 4];
        let repeat = vec![3, 4];
        assert_eq!(
            map_overlay_pages(&from, &to, &repeat),
            vec![(1, 3), (2, 4), (3, 3), (4, 4)]
        );
    }

    // ---- resolve_spec_pairs (composed with PageRange::resolve) ------------

    #[test]
    fn spec_pairs_repeated_to_slots_yield_one_pair_per_slot() {
        // uo-6 pattern: `--overlay --to=1,1,1,1 --from=1-4` on a 1-page dest
        // and a 4-page source. PageRange::resolve preserves the four repeated
        // 1s (qpdf-parity), so map_overlay_pages pairs each slot with the
        // i-th --from source page. The bug this pins (flpdf-9x9o): dedup on
        // --to collapsed the four slots to `[1]` and only one overlay was
        // applied.
        let from = pr("1-4");
        let to = pr("1,1,1,1");
        let pairs = resolve_spec_pairs(4, &from, &to, None, 1).unwrap();
        assert_eq!(pairs, vec![(1, 1), (1, 2), (1, 3), (1, 4)]);
    }

    // ---- apply_overlay_spec (driving function, end-to-end in memory) ------

    /// Build a `count`-page document. Every page is object `2 + i` (page 1 is
    /// object 3), each with a 612x792 MediaBox, a shared font, and its own
    /// content stream. Returns parseable PDF bytes.
    fn multi_page_doc(count: u32) -> Vec<u8> {
        assert!(count >= 1);
        let mut objs: Vec<(u32, String)> = Vec::new();
        objs.push((1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()));
        // Page objects are 3..3+count; content streams follow them.
        let kids: Vec<String> = (0..count).map(|i| format!("{} 0 R", 3 + i)).collect();
        objs.push((
            2,
            format!(
                "<< /Type /Pages /Kids [{}] /Count {count} >>",
                kids.join(" ")
            ),
        ));
        // Shared font object placed after the pages + their content streams.
        let font_obj = 3 + count * 2;
        for i in 0..count {
            let page_num = i + 1;
            let page_obj = 3 + i;
            let content_obj = 3 + count + i;
            objs.push((
                page_obj,
                format!(
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                     /Resources << /Font << /F1 {font_obj} 0 R >> >> /Contents {content_obj} 0 R >>"
                ),
            ));
            let content = format!("page {page_num} content");
            let body = format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            );
            objs.push((content_obj, body));
        }
        objs.push((
            font_obj,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        ));
        let borrowed: Vec<(u32, &str)> = objs.iter().map(|(n, b)| (*n, b.as_str())).collect();
        build_pdf(&borrowed, 1)
    }

    /// The imported overlay XObject ref (`/Fx1`) referenced by a patched page.
    fn fx1_ref<R: Read + Seek>(pdf: &mut Pdf<R>, page_ref: ObjectRef) -> ObjectRef {
        let page = pdf.resolve(page_ref).unwrap();
        let res = page
            .as_dict()
            .unwrap()
            .get("Resources")
            .unwrap()
            .as_dict()
            .unwrap();
        let xobj = res.get("XObject").unwrap().as_dict().unwrap();
        match xobj.get("Fx1") {
            Some(Object::Reference(r)) => *r,
            other => panic!("Fx1 should be a reference, got {other:?}"), // cov:ignore: defensive — apply always inserts /Fx1 as a reference
        }
    }

    /// Whether a page has been patched into an overlay page (its /Resources is
    /// just `<< /XObject << /Fx0 ... >> >>`, so /Font is gone and /XObject present).
    fn is_patched<R: Read + Seek>(pdf: &mut Pdf<R>, page_ref: ObjectRef) -> bool {
        let page = pdf.resolve(page_ref).unwrap();
        let res = page
            .as_dict()
            .unwrap()
            .get("Resources")
            .unwrap()
            .as_dict()
            .unwrap();
        res.get("XObject").is_some() && res.get("Font").is_none()
    }

    #[test]
    fn apply_overlay_spec_two_page_default_shares_nothing_and_skips_third() {
        // dest=3 pages, source=2 pages, defaults. p1<-s1, p2<-s2, p3 untouched.
        let mut dest = open(multi_page_doc(3));
        let mut source = open(multi_page_doc(2));
        let dest_pages = page_refs(&mut dest).unwrap();

        apply_overlay_spec(
            &mut dest,
            &mut source,
            OverlayKind::Overlay,
            &pr(""),
            &pr(""),
            None,
        )
        .unwrap();

        assert!(is_patched(&mut dest, dest_pages[0]), "p1 patched");
        assert!(is_patched(&mut dest, dest_pages[1]), "p2 patched");
        assert!(!is_patched(&mut dest, dest_pages[2]), "p3 untouched");
        // Distinct sources -> distinct imported XObjects.
        let fx1_p1 = fx1_ref(&mut dest, dest_pages[0]);
        let fx1_p2 = fx1_ref(&mut dest, dest_pages[1]);
        assert_ne!(
            fx1_p1, fx1_p2,
            "distinct source pages import distinct XObjects"
        );
    }

    #[test]
    fn apply_overlay_spec_repeat_shares_single_source_xobject() {
        // dest=3, source=1, --repeat=1: every dest page shares the SAME imported
        // XObject ref (qpdf imports the source page once and reuses it).
        let mut dest = open(multi_page_doc(3));
        let mut source = open(multi_page_doc(1));
        let dest_pages = page_refs(&mut dest).unwrap();

        apply_overlay_spec(
            &mut dest,
            &mut source,
            OverlayKind::Overlay,
            &pr(""),
            &pr(""),
            Some(&pr("1")),
        )
        .unwrap();

        let fx1_p1 = fx1_ref(&mut dest, dest_pages[0]);
        let fx1_p2 = fx1_ref(&mut dest, dest_pages[1]);
        let fx1_p3 = fx1_ref(&mut dest, dest_pages[2]);
        assert_eq!(fx1_p1, fx1_p2, "same source -> shared XObject ref");
        assert_eq!(fx1_p2, fx1_p3, "same source -> shared XObject ref");

        // /Fx0 (the page itself) differs per page (each page's own content).
        let fx0 = |pdf: &mut Pdf<_>, page_ref: ObjectRef| -> ObjectRef {
            let page = pdf.resolve(page_ref).unwrap();
            let xobj = page
                .as_dict()
                .unwrap()
                .get("Resources")
                .unwrap()
                .as_dict()
                .unwrap()
                .get("XObject")
                .unwrap()
                .as_dict()
                .unwrap();
            match xobj.get("Fx0") {
                Some(Object::Reference(r)) => *r,
                other => panic!("Fx0 ref: {other:?}"), // cov:ignore: defensive — apply always inserts /Fx0 as a reference
            }
        };
        let fx0_p1 = fx0(&mut dest, dest_pages[0]);
        let fx0_p2 = fx0(&mut dest, dest_pages[1]);
        assert_ne!(fx0_p1, fx0_p2, "each page's own Fx0 is distinct");
    }

    #[test]
    fn apply_overlay_spec_to_range_leaves_unselected_dest_untouched() {
        // dest=3, source=2, --to=2-3: p1 untouched, p2<-s1, p3<-s2.
        let mut dest = open(multi_page_doc(3));
        let mut source = open(multi_page_doc(2));
        let dest_pages = page_refs(&mut dest).unwrap();

        apply_overlay_spec(
            &mut dest,
            &mut source,
            OverlayKind::Overlay,
            &pr(""),
            &pr("2-3"),
            None,
        )
        .unwrap();

        assert!(!is_patched(&mut dest, dest_pages[0]), "p1 untouched");
        assert!(is_patched(&mut dest, dest_pages[1]), "p2 patched");
        assert!(is_patched(&mut dest, dest_pages[2]), "p3 patched");
    }

    #[test]
    fn apply_overlay_spec_underlay_kind_is_threaded_through() {
        // A single underlay: the source is named /Fx1 and drawn BEFORE /Fx0.
        let mut dest = open(multi_page_doc(1));
        let mut source = open(multi_page_doc(1));
        let dest_pages = page_refs(&mut dest).unwrap();

        apply_overlay_spec(
            &mut dest,
            &mut source,
            OverlayKind::Underlay,
            &pr(""),
            &pr(""),
            None,
        )
        .unwrap();

        let page = dest.resolve(dest_pages[0]).unwrap();
        let contents_ref = match page.as_dict().unwrap().get("Contents") {
            Some(Object::Reference(r)) => *r,
            other => panic!("Contents ref: {other:?}"), // cov:ignore: defensive — apply always writes /Contents as a reference
        };
        let stream = dest.resolve(contents_ref).unwrap().into_stream().unwrap();
        let text = String::from_utf8(stream.data).unwrap();
        let fx1 = text.find("/Fx1 Do").unwrap();
        let fx0 = text.find("/Fx0 Do").unwrap();
        assert!(fx1 < fx0, "underlay /Fx1 must draw before /Fx0: {text:?}");
    }

    #[test]
    fn apply_overlay_spec_errors_on_out_of_range_from() {
        // --from=5 against a 2-page source resolves out of range and errors.
        let mut dest = open(multi_page_doc(2));
        let mut source = open(multi_page_doc(2));
        let err = apply_overlay_spec(
            &mut dest,
            &mut source,
            OverlayKind::Overlay,
            &pr("5"),
            &pr(""),
            None,
        );
        assert!(matches!(err, Err(Error::Parse { .. })));
    }

    #[test]
    fn page_ref_for_errors_when_out_of_range() {
        // A 1-based page number past the end is rejected (defensive guard).
        let pages = vec![ObjectRef::new(3, 0), ObjectRef::new(4, 0)];
        assert!(matches!(
            page_ref_for(&pages, 3, "source"),
            Err(Error::Unsupported(_))
        ));
        // Page 0 (would underflow) is also rejected.
        assert!(matches!(
            page_ref_for(&pages, 0, "destination"),
            Err(Error::Unsupported(_))
        ));
        // In-range lookups return the right ref.
        assert_eq!(page_ref_for(&pages, 1, "source").unwrap(), pages[0]);
        assert_eq!(page_ref_for(&pages, 2, "source").unwrap(), pages[1]);
    }

    #[test]
    fn u32_len_clamps_oversized_lengths() {
        assert_eq!(u32_len(0), 0);
        assert_eq!(u32_len(5), 5);
        // A length above u32::MAX clamps instead of wrapping.
        assert_eq!(u32_len(usize::MAX), u32::MAX);
    }

    // ---- group_sources_by_dest_page (pure) -------------------------------

    /// A synthetic [`OverlaySource`] of `kind` referencing object `n`.
    fn src(kind: OverlayKind, n: u32) -> OverlaySource {
        OverlaySource {
            kind,
            xobject_ref: ObjectRef::new(n, 0),
            annot_template: None,
        }
    }

    #[test]
    fn group_sources_buckets_by_page_in_ascending_order() {
        // Out-of-order dest pages bucket correctly; BTreeMap iterates ascending.
        let entries = vec![
            (3, src(OverlayKind::Overlay, 10)),
            (1, src(OverlayKind::Overlay, 11)),
            (3, src(OverlayKind::Overlay, 12)),
        ];
        let grouped = group_sources_by_dest_page(&entries);
        let pages: Vec<u32> = grouped.keys().copied().collect();
        assert_eq!(pages, vec![1, 3], "pages iterate in ascending order");
        // Page 3 keeps both its sources in encounter order (10 before 12).
        let p3: Vec<u32> = grouped[&3].iter().map(|s| s.xobject_ref.number).collect();
        assert_eq!(p3, vec![10, 12]);
    }

    #[test]
    fn group_sources_preserves_cross_spec_declaration_order_within_page() {
        // Mirrors the overlay-and-underlay golden's page 1: spec1 contributes an
        // OVERLAY (one, ref 11), spec2 an UNDERLAY (two, ref 19), both onto page
        // 1, in that declaration order. The grouping must keep that order so
        // apply_overlays_to_page can re-group by kind (under-then-over).
        let entries = vec![
            (1, src(OverlayKind::Overlay, 11)),
            (1, src(OverlayKind::Underlay, 19)),
        ];
        let grouped = group_sources_by_dest_page(&entries);
        let p1 = &grouped[&1];
        assert_eq!(p1.len(), 2);
        assert_eq!(p1[0].kind, OverlayKind::Overlay);
        assert_eq!(p1[0].xobject_ref.number, 11);
        assert_eq!(p1[1].kind, OverlayKind::Underlay);
        assert_eq!(p1[1].xobject_ref.number, 19);
    }

    #[test]
    fn group_sources_empty_is_empty() {
        assert!(group_sources_by_dest_page(&[]).is_empty());
    }

    // ---- kind_stable_partition (pure) -------------------------------------

    #[test]
    fn kind_stable_partition_underlays_first_stable_within_group() {
        #[derive(Debug, PartialEq)]
        struct E(u32, OverlayKind);
        let out = kind_stable_partition(
            vec![
                E(1, OverlayKind::Overlay),
                E(2, OverlayKind::Underlay),
                E(3, OverlayKind::Overlay),
                E(4, OverlayKind::Underlay),
            ],
            |e| e.1,
        );
        assert_eq!(
            out,
            vec![
                E(2, OverlayKind::Underlay),
                E(4, OverlayKind::Underlay),
                E(1, OverlayKind::Overlay),
                E(3, OverlayKind::Overlay),
            ],
            "underlays first (order preserved), then overlays (order preserved)"
        );

        // Empty input → empty output.
        let empty: Vec<E> = kind_stable_partition(Vec::new(), |e| e.1);
        assert!(empty.is_empty(), "empty input yields empty output");

        // All-underlays input → identity (order preserved).
        let all_u = kind_stable_partition(
            vec![
                E(10, OverlayKind::Underlay),
                E(11, OverlayKind::Underlay),
                E(12, OverlayKind::Underlay),
            ],
            |e| e.1,
        );
        assert_eq!(
            all_u,
            vec![
                E(10, OverlayKind::Underlay),
                E(11, OverlayKind::Underlay),
                E(12, OverlayKind::Underlay),
            ],
            "all-underlays input preserves order (identity)"
        );

        // All-overlays input → identity (order preserved).
        let all_o = kind_stable_partition(
            vec![
                E(20, OverlayKind::Overlay),
                E(21, OverlayKind::Overlay),
                E(22, OverlayKind::Overlay),
            ],
            |e| e.1,
        );
        assert_eq!(
            all_o,
            vec![
                E(20, OverlayKind::Overlay),
                E(21, OverlayKind::Overlay),
                E(22, OverlayKind::Overlay),
            ],
            "all-overlays input preserves order (identity)"
        );
    }

    // ---- apply_overlay_specs (multi-spec driver, end-to-end in memory) ----

    /// The full /Fx name → imported ref map and decoded content text of a patched
    /// page, for asserting cross-spec naming and draw order.
    fn page_fx_and_content<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        page_ref: ObjectRef,
    ) -> (BTreeMap<String, ObjectRef>, String) {
        let page = pdf.resolve(page_ref).unwrap();
        let page_dict = page.as_dict().unwrap();
        let xobj = page_dict
            .get("Resources")
            .unwrap()
            .as_dict()
            .unwrap()
            .get("XObject")
            .unwrap()
            .as_dict()
            .unwrap();
        let mut names = BTreeMap::new();
        for (k, v) in xobj.iter() {
            if let Object::Reference(r) = v {
                names.insert(String::from_utf8(k.to_vec()).unwrap(), *r);
            }
        }
        let contents_ref = match page_dict.get("Contents") {
            Some(Object::Reference(r)) => *r,
            other => panic!("Contents ref: {other:?}"), // cov:ignore: defensive — apply always writes /Contents as a reference
        };
        let stream = pdf.resolve(contents_ref).unwrap().into_stream().unwrap();
        (names, String::from_utf8(stream.data).unwrap())
    }

    /// Build an [`OverlaySpec`] with default ranges (`--from`/`--to` all, no
    /// `--repeat`) over a freshly opened `source` document.
    fn spec(
        source: Pdf<std::io::Cursor<Vec<u8>>>,
        kind: OverlayKind,
    ) -> OverlaySpec<std::io::Cursor<Vec<u8>>> {
        OverlaySpec {
            source,
            kind,
            from: pr(""),
            to: pr(""),
            repeat: None,
        }
    }

    #[test]
    fn apply_overlay_specs_two_overlays_name_in_declaration_order() {
        // Mirrors the three-page-two-overlays golden: dest=3 pages,
        // spec1=overlay(one-page), spec2=overlay(two-page). Page 1 gets BOTH:
        // Fx1=overlay-one(s1), Fx2=overlay-two(s1); page 2 gets only
        // overlay-two(s2) as Fx1; page 3 untouched. apply_overlays_to_page must
        // run exactly once per page (one /Fx0).
        let mut dest = open(multi_page_doc(3));
        let dest_pages = page_refs(&mut dest).unwrap();
        let mut specs = vec![
            spec(open(multi_page_doc(1)), OverlayKind::Overlay),
            spec(open(multi_page_doc(2)), OverlayKind::Overlay),
        ];
        apply_overlay_specs(&mut dest, &mut specs).unwrap();

        // Page 1: Fx0 + two overlays. Draw order Fx0 -> Fx1 -> Fx2.
        let (names1, text1) = page_fx_and_content(&mut dest, dest_pages[0]);
        let keys1: Vec<&str> = {
            let mut k: Vec<&str> = names1.keys().map(String::as_str).collect();
            k.sort();
            k
        };
        assert_eq!(keys1, vec!["Fx0", "Fx1", "Fx2"], "page 1 has Fx0..Fx2");
        let p0 = text1.find("/Fx0 Do").unwrap();
        let p1 = text1.find("/Fx1 Do").unwrap();
        let p2 = text1.find("/Fx2 Do").unwrap();
        assert!(p0 < p1 && p1 < p2, "overlays draw after /Fx0: {text1:?}");
        // The two overlays come from DIFFERENT source documents -> distinct refs.
        assert_ne!(names1["Fx1"], names1["Fx2"]);

        // Page 2: only spec2's second source page (Fx1), single /Fx0.
        let (names2, _text2) = page_fx_and_content(&mut dest, dest_pages[1]);
        let mut keys2: Vec<&str> = names2.keys().map(String::as_str).collect();
        keys2.sort();
        assert_eq!(keys2, vec!["Fx0", "Fx1"], "page 2 has only one source");

        // Page 3 untouched (both sources exhausted, no --repeat).
        assert!(!is_patched(&mut dest, dest_pages[2]), "page 3 untouched");
    }

    #[test]
    fn apply_overlay_specs_overlay_then_underlay_names_under_first() {
        // Mirrors the three-page-overlay-and-underlay golden: spec1=overlay(one),
        // spec2=underlay(two). On page 1 the UNDERLAY must be /Fx1 (drawn before
        // /Fx0) and the OVERLAY /Fx2 (drawn after /Fx0), even though the overlay
        // was declared first — apply_overlays_to_page groups under-then-over.
        let mut dest = open(multi_page_doc(3));
        let dest_pages = page_refs(&mut dest).unwrap();
        let mut specs = vec![
            spec(open(multi_page_doc(1)), OverlayKind::Overlay),
            spec(open(multi_page_doc(2)), OverlayKind::Underlay),
        ];
        apply_overlay_specs(&mut dest, &mut specs).unwrap();

        let (names1, text1) = page_fx_and_content(&mut dest, dest_pages[0]);
        let mut keys1: Vec<&str> = names1.keys().map(String::as_str).collect();
        keys1.sort();
        assert_eq!(keys1, vec!["Fx0", "Fx1", "Fx2"]);
        // Draw order: Fx1 (underlay) -> Fx0 (page) -> Fx2 (overlay).
        let f1 = text1.find("/Fx1 Do").unwrap();
        let f0 = text1.find("/Fx0 Do").unwrap();
        let f2 = text1.find("/Fx2 Do").unwrap();
        assert!(
            f1 < f0 && f0 < f2,
            "under(Fx1) -> page(Fx0) -> over(Fx2): {text1:?}"
        );

        // Page 2: only the underlay's second source page, drawn before /Fx0.
        let (names2, text2) = page_fx_and_content(&mut dest, dest_pages[1]);
        let mut keys2: Vec<&str> = names2.keys().map(String::as_str).collect();
        keys2.sort();
        assert_eq!(keys2, vec!["Fx0", "Fx1"]);
        assert!(
            text2.find("/Fx1 Do").unwrap() < text2.find("/Fx0 Do").unwrap(),
            "page 2 underlay draws before /Fx0: {text2:?}"
        );
    }

    #[test]
    fn apply_overlay_specs_applies_each_page_once() {
        // Two overlay specs both targeting page 1 (each a single source page) must
        // share ONE /Fx0 (the page is wrapped exactly once). Distinct Fx0 per call
        // would indicate a double apply.
        let mut dest = open(multi_page_doc(1));
        let dest_pages = page_refs(&mut dest).unwrap();
        let mut specs = vec![
            spec(open(multi_page_doc(1)), OverlayKind::Overlay),
            spec(open(multi_page_doc(1)), OverlayKind::Overlay),
        ];
        apply_overlay_specs(&mut dest, &mut specs).unwrap();

        let (names, text) = page_fx_and_content(&mut dest, dest_pages[0]);
        let mut keys: Vec<&str> = names.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, vec!["Fx0", "Fx1", "Fx2"], "one /Fx0, two overlays");
        // Exactly one "/Fx0 Do" — the page was converted to /Fx0 once.
        assert_eq!(text.matches("/Fx0 Do").count(), 1, "single /Fx0 draw");
    }

    #[test]
    fn apply_overlay_specs_empty_is_noop() {
        // No specs leaves every dest page untouched.
        let mut dest = open(multi_page_doc(2));
        let dest_pages = page_refs(&mut dest).unwrap();
        let mut specs: Vec<OverlaySpec<std::io::Cursor<Vec<u8>>>> = Vec::new();
        apply_overlay_specs(&mut dest, &mut specs).unwrap();
        assert!(!is_patched(&mut dest, dest_pages[0]));
        assert!(!is_patched(&mut dest, dest_pages[1]));
    }

    #[test]
    fn apply_overlay_specs_propagates_spec_error() {
        // An out-of-range --from in any spec surfaces as an error from the driver.
        let mut dest = open(multi_page_doc(2));
        let mut specs = vec![OverlaySpec {
            source: open(multi_page_doc(2)),
            kind: OverlayKind::Overlay,
            from: pr("5"),
            to: pr(""),
            repeat: None,
        }];
        let err = apply_overlay_specs(&mut dest, &mut specs);
        assert!(matches!(err, Err(Error::Parse { .. })));
    }

    // ---- overlay_verbose_report (public inspection API) -------------------

    /// A minimally-valid N-page document (empty content streams; MediaBox
    /// only). Object numbers: 1 = Catalog, 2 = Pages, 3..(2+n) = /Page dicts.
    fn n_page_doc(n: u32) -> Vec<u8> {
        assert!(n >= 1);
        let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 3 + i)).collect();
        let mut objects: Vec<(u32, String)> = Vec::new();
        objects.push((1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()));
        objects.push((
            2,
            format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), n),
        ));
        for i in 0..n {
            objects.push((
                3 + i,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> >>"
                    .to_string(),
            ));
        }
        let refs: Vec<(u32, &str)> = objects.iter().map(|(n, s)| (*n, s.as_str())).collect();
        build_pdf(&refs, 1)
    }

    #[test]
    fn overlay_verbose_report_orders_underlays_then_overlays_across_specs() {
        // 4 specs on the same 3-page dest with 1-page sources, all targeting page 1
        // via --to=1. Declaration order: overlay-A, overlay-B, underlay-C, underlay-D.
        // Expected page-1 sources spec_index order: [2, 3, 0, 1] (underlays first).
        let mut dest = open(n_page_doc(3));
        let spec_a = OverlaySpec {
            source: open(n_page_doc(1)),
            kind: OverlayKind::Overlay,
            from: PageRange::parse("").unwrap(),
            to: PageRange::parse("1").unwrap(),
            repeat: None,
        };
        let spec_b = OverlaySpec {
            source: open(n_page_doc(1)),
            kind: OverlayKind::Overlay,
            from: PageRange::parse("").unwrap(),
            to: PageRange::parse("1").unwrap(),
            repeat: None,
        };
        let spec_c = OverlaySpec {
            source: open(n_page_doc(1)),
            kind: OverlayKind::Underlay,
            from: PageRange::parse("").unwrap(),
            to: PageRange::parse("1").unwrap(),
            repeat: None,
        };
        let spec_d = OverlaySpec {
            source: open(n_page_doc(1)),
            kind: OverlayKind::Underlay,
            from: PageRange::parse("").unwrap(),
            to: PageRange::parse("1").unwrap(),
            repeat: None,
        };
        let mut specs = [spec_a, spec_b, spec_c, spec_d];
        let report = overlay_verbose_report(&mut dest, &mut specs).unwrap();
        assert_eq!(report.len(), 3, "3-page dest -> 3 report entries");
        assert_eq!(report[0].dest_page, 1);
        let idx: Vec<usize> = report[0].sources.iter().map(|s| s.spec_index).collect();
        assert_eq!(
            idx,
            vec![2, 3, 0, 1],
            "underlays first (specs 2,3), then overlays (0,1)"
        );
        let kinds: Vec<OverlayKind> = report[0].sources.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            vec![
                OverlayKind::Underlay,
                OverlayKind::Underlay,
                OverlayKind::Overlay,
                OverlayKind::Overlay,
            ],
            "sources[0..2] must be underlays, [2..4] overlays"
        );
        let srcs: Vec<u32> = report[0].sources.iter().map(|s| s.src_page).collect();
        assert_eq!(
            srcs,
            vec![1, 1, 1, 1],
            "every spec targets --to=1, src=1 for a 1-page source"
        );
        // Pages 2 and 3 unaffected (--to=1 only).
        assert!(report[1].sources.is_empty());
        assert!(report[2].sources.is_empty());
    }

    #[test]
    fn overlay_verbose_report_includes_dest_pages_with_no_sources() {
        // 3-page dest, 2-page source, single --overlay --to=1-2:
        //   page 1 <- src 1 (overlay), page 2 <- src 2 (overlay), page 3 empty.
        let mut dest = open(n_page_doc(3));
        let spec = OverlaySpec {
            source: open(n_page_doc(2)),
            kind: OverlayKind::Overlay,
            from: PageRange::parse("").unwrap(),
            to: PageRange::parse("1-2").unwrap(),
            repeat: None,
        };
        let mut specs = [spec];
        let report = overlay_verbose_report(&mut dest, &mut specs).unwrap();
        assert_eq!(report.len(), 3);
        assert_eq!(report[0].dest_page, 1);
        assert_eq!(report[0].sources.len(), 1);
        assert_eq!(report[0].sources[0].src_page, 1);
        assert_eq!(report[1].dest_page, 2);
        assert_eq!(report[1].sources.len(), 1);
        assert_eq!(report[1].sources[0].src_page, 2);
        assert_eq!(report[2].dest_page, 3);
        assert!(report[2].sources.is_empty());
    }

    #[test]
    fn overlay_verbose_report_pins_source_page_under_repeat() {
        // 5-page dest, 2-page source, single --overlay --repeat=1-2:
        //   from defaults to all source pages (1,2), applied to dest 1-2 in order.
        //   Once from is exhausted, repeat=[1,2] cycles across the remaining dest
        //   pages -> dest 3<-1, 4<-2, 5<-1.
        let mut dest = open(n_page_doc(5));
        let spec = OverlaySpec {
            source: open(n_page_doc(2)),
            kind: OverlayKind::Overlay,
            from: PageRange::parse("").unwrap(),
            to: PageRange::parse("").unwrap(),
            repeat: Some(PageRange::parse("1-2").unwrap()),
        };
        let mut specs = [spec];
        let report = overlay_verbose_report(&mut dest, &mut specs).unwrap();
        let src_pages: Vec<u32> = report.iter().map(|p| p.sources[0].src_page).collect();
        assert_eq!(src_pages, vec![1, 2, 1, 2, 1]);
    }

    #[test]
    fn overlay_verbose_report_repeated_to_slot_yields_one_source_per_slot() {
        // uo-6 pattern: 1-page dest, 4-page source, single --overlay with
        // --to=1,1,1,1 and --from=1-4. The four repeated dest-slots each pair
        // with a distinct source page (from 1..4), so dest page 1 accumulates
        // four sources — matching qpdf's uo-6 golden which emits
        // `fxo-blue.pdf overlay 1..4` on page 1.
        let mut dest = open(n_page_doc(1));
        let spec = OverlaySpec {
            source: open(n_page_doc(4)),
            kind: OverlayKind::Overlay,
            from: PageRange::parse("1-4").unwrap(),
            to: PageRange::parse("1,1,1,1").unwrap(),
            repeat: None,
        };
        let mut specs = [spec];
        let report = overlay_verbose_report(&mut dest, &mut specs).unwrap();
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].dest_page, 1);
        let src_pages: Vec<u32> = report[0].sources.iter().map(|s| s.src_page).collect();
        assert_eq!(src_pages, vec![1, 2, 3, 4]);
    }

    #[test]
    fn overlay_verbose_report_empty_to_yields_all_empty_entries() {
        // 3-page dest, 2-page source, single spec with an explicitly empty --to:
        // no dest pages are selected, so every report entry has empty sources.
        let mut dest = open(n_page_doc(3));
        let spec = OverlaySpec {
            source: open(n_page_doc(2)),
            kind: OverlayKind::Overlay,
            from: PageRange::parse("").unwrap(),
            to: PageRange::empty(),
            repeat: None,
        };
        let mut specs = [spec];
        let report = overlay_verbose_report(&mut dest, &mut specs).unwrap();
        assert_eq!(report.len(), 3);
        for (i, page) in report.iter().enumerate() {
            assert_eq!(page.dest_page, (i + 1) as u32);
            assert!(page.sources.is_empty());
        }
    }

    #[test]
    fn overlay_verbose_report_does_not_mutate_dest() {
        // Read-only inspection: page refs stay identical, and each page's
        // /Contents and /Resources references are unchanged after the call.
        let mut dest = open(n_page_doc(3));
        let page_refs_before = page_refs(&mut dest).unwrap();
        assert_eq!(page_refs_before.len(), 3);
        let page1_ref = page_refs_before[0];
        let dict_before = page_dictionary(&mut dest, page1_ref).unwrap();
        let spec = OverlaySpec {
            source: open(n_page_doc(1)),
            kind: OverlayKind::Overlay,
            from: PageRange::parse("").unwrap(),
            to: PageRange::parse("").unwrap(),
            repeat: None,
        };
        let mut specs = [spec];
        let _ = overlay_verbose_report(&mut dest, &mut specs).unwrap();
        let page_refs_after = page_refs(&mut dest).unwrap();
        assert_eq!(page_refs_before, page_refs_after);
        let dict_after = page_dictionary(&mut dest, page1_ref).unwrap();
        assert_eq!(dict_before.get("Contents"), dict_after.get("Contents"));
        assert_eq!(dict_before.get("Resources"), dict_after.get("Resources"));
    }

    #[test]
    fn overlay_verbose_report_propagates_spec_page_resolution_error() {
        // Source has 1 page but --from=2 references a nonexistent source page,
        // so PageRange::resolve inside resolve_spec_pairs returns Err. Verifies
        // the `?` on the resolve_spec_pairs call propagates the error.
        let mut dest = open(n_page_doc(2));
        let mut specs = [OverlaySpec {
            source: open(n_page_doc(1)),
            kind: OverlayKind::Overlay,
            from: PageRange::parse("2").unwrap(),
            to: PageRange::parse("").unwrap(),
            repeat: None,
        }];
        let result = overlay_verbose_report(&mut dest, &mut specs);
        assert!(
            matches!(result, Err(Error::Parse { .. })),
            "out-of-range --from should propagate as Err(Parse), got {result:?}"
        );
    }
}
