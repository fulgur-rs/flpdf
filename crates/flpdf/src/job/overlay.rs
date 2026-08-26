//! qpdf correspondence: QPDFPageObjectHelper.cc placement and QPDFJob.cc overlay orchestration responsibilities.
//! Apply overlay/underlay content to a destination page, mirroring qpdf's
//! `QPDFPageObjectHelper::placeFormXObject` and `QPDFJob::doUnderOverlayForPage`
//! (qpdf 11.9.0).
//!
//! Each destination page that receives at least one overlay or underlay is
//! rewritten as follows (see [`get_form_xobject_for_page`](crate::page_form_xobject)):
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

use super::page_range::PageRange;
use crate::page_document_helper::PageDocumentHelper;
use crate::page_form_xobject::get_form_xobject_for_page;
use crate::page_object_helper::{rectangle_from_handle, PageBox, PageObjectHelper};
use crate::{
    Dictionary, Error, Matrix, Object, ObjectHandle, ObjectRef, Pdf, Rectangle, Result, Stream,
};

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
/// destination document, plus the source page identity retained for the
/// canonical foreign-document `PageObjectHelper::copy_annotations_from` route.
#[derive(Debug, Clone)]
pub(crate) struct OverlaySource {
    /// The source's kind (overlay or underlay).
    pub kind: OverlayKind,
    /// Reference to the imported Form XObject in the destination document.
    pub xobject_ref: ObjectRef,
    /// `(source document index, source page reference)` used for per-placement
    /// annotation copying. `None` is used by content-only unit fixtures.
    pub source_page: Option<(usize, ObjectRef)>,
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
#[cfg(test)]
fn get_matrix_for_form_xobject_placement(
    fo_bbox: Rectangle,
    fo_matrix: Matrix,
    rect: Rectangle,
    tmatrix: Matrix,
    allow_shrink: bool,
    allow_expand: bool,
) -> Option<Matrix> {
    // wmatrix = I.concat(tmatrix).concat(fmatrix). tmatrix is identity (a no-op)
    // when the dest page has no transform; fmatrix is identity when the fo has no
    // /Matrix — both still concatenated, matching qpdf.
    let mut wmatrix = Matrix::default();
    wmatrix.concat(tmatrix);
    wmatrix.concat(fo_matrix);
    let t = wmatrix.transform_rectangle(fo_bbox);
    if t.urx == t.llx || t.ury == t.lly {
        return None;
    }
    let rect_w = rect.urx - rect.llx;
    let rect_h = rect.ury - rect.lly;
    let t_w = t.urx - t.llx;
    let t_h = t.ury - t.lly;
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
    let mut wmatrix = Matrix::default();
    wmatrix.scale(scale, scale);
    wmatrix.concat(tmatrix);
    wmatrix.concat(fo_matrix);
    let t = wmatrix.transform_rectangle(fo_bbox);
    let t_cx = (t.llx + t.urx) / 2.0;
    let t_cy = (t.lly + t.ury) / 2.0;
    let r_cx = (rect.llx + rect.urx) / 2.0;
    let r_cy = (rect.lly + rect.ury) / 2.0;
    let tx = r_cx - t_cx;
    let ty = r_cy - t_cy;

    // cm = I.translate(tx, ty).scale(scale, scale).concat(tmatrix). The fmatrix is
    // deliberately absent: the PDF interpreter applies the fo's /Matrix itself.
    let mut cm = Matrix::default();
    cm.translate(tx, ty);
    cm.scale(scale, scale);
    cm.concat(tmatrix);
    Some(cm)
}

/// Build a `placeFormXObject` content fragment placing the Form XObject named
/// `name` into `rect`, mirroring qpdf's `QPDFPageObjectHelper::placeFormXObject`
/// (qpdf 11.9.0). A degenerate placement matrix collapses to the identity, as in
/// qpdf.
///
/// Returns `(fragment, cm)`: the fragment is exactly
/// `"q\n" + cm + " cm\n/" + name + " Do\nQ\n"` (with `cm` formatted by
/// [`Matrix::unparse`]); `cm` is the same six-component matrix used to place the
/// XObject. Callers that transform per-placement annotations (qpdf's
/// `copyAnnotations(from_page, cm, …)`) use the returned `cm` unchanged.
#[cfg(test)]
fn place_form_xobject(
    fo_bbox: Rectangle,
    fo_matrix: Matrix,
    rect: Rectangle,
    tmatrix: Matrix,
    allow_shrink: bool,
    allow_expand: bool,
    name: &str,
) -> (String, Matrix) {
    let cm = get_matrix_for_form_xobject_placement(
        fo_bbox,
        fo_matrix,
        rect,
        tmatrix,
        allow_shrink,
        allow_expand,
    )
    .unwrap_or_default();
    let fragment = format!("q\n{} cm\n/{} Do\nQ\n", cm.unparse(), name);
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
fn apply_overlays_to_page_with_sources<R: Read + Seek, RS: Read + Seek>(
    dest: &mut Pdf<R>,
    dest_page_ref: ObjectRef,
    sources: &[OverlaySource],
    source_documents: &mut [&mut Pdf<RS>],
) -> Result<()> {
    // qpdf orders sources underlays-then-overlays for BOTH naming and drawing.
    // Build the two typed Vecs in a single pass over `sources` in encounter
    // order — each kind is appended independently, so relative order within a
    // kind is preserved. The paint order (underlays first, then overlays) is
    // enforced below when we consume `underlays` before `overlays` while
    // building the /Fx1.. names and the new content stream.
    //
    type PlacementEntry = (ObjectRef, Option<(usize, ObjectRef)>);
    let mut underlays: Vec<PlacementEntry> = Vec::new();
    let mut overlays: Vec<PlacementEntry> = Vec::new();
    for src in sources {
        let entry = (src.xobject_ref, src.source_page);
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
    // Placement computes the destination inverse transform through the
    // canonical PageObjectHelper route below, after the destination page has
    // been wrapped as /Fx0.

    // 1. Convert the destination page itself to Form XObject /Fx0.
    let fx0_ref = get_form_xobject_for_page(dest, dest_page_ref)?;

    // 2. Name the sources /Fx1.. in underlays-then-overlays order and build the
    //    new page /Resources /XObject mapping. /Fx0 is the page; the unique-name
    //    counter continues from there (getUniqueResourceName).
    let mut xobject_entries: Vec<(Vec<u8>, ObjectHandle)> =
        Vec::with_capacity(1 + underlays.len() + overlays.len());
    xobject_entries.push((b"/Fx0".to_vec(), dest.get_object_handle(fx0_ref)));
    let mut next_index = 1u32;
    type NamedPlacement = (String, ObjectRef, Option<(usize, ObjectRef)>);
    let mut underlay_names: Vec<NamedPlacement> = Vec::new();
    let mut overlay_names: Vec<NamedPlacement> = Vec::new();
    for (xref, template) in &underlays {
        let name = format!("Fx{next_index}");
        xobject_entries.push((name.as_bytes().to_vec(), dest.get_object_handle(*xref)));
        underlay_names.push((name, *xref, *template));
        next_index += 1;
    }
    for (xref, template) in &overlays {
        let name = format!("Fx{next_index}");
        xobject_entries.push((name.as_bytes().to_vec(), dest.get_object_handle(*xref)));
        overlay_names.push((name, *xref, *template));
        next_index += 1;
    }

    // 3. Build the new page /Contents in draw order: underlays -> /Fx0 ->
    //    overlays. Underlays/overlays place into the page /TrimBox with
    //    allow_shrink=true; /Fx0 places into the page /MediaBox with
    //    allow_shrink=false (qpdf's doUnderOverlayForPage flag split). Every
    //    placement folds in the dest inverse transform `tmatrix`. Immediately
    //    after each source placement returns `cm`, copy the source page's
    //    annotations through the canonical PageObjectHelper foreign route.
    // Placement rects mirror qpdf's getTrimBox()/getMediaBox().getArrayAsRectangle()
    // in doUnderOverlayForPage: corners normalized before scaling/centring.
    let trim_rect = normalize_rectangle(trim_box);
    let media_rect = normalize_rectangle(media_box);
    let mut content = String::new();
    for (name, xref, source_page) in &underlay_names {
        // cov:ignore-start: the trailing `)?;` is the defensive error edge of
        // a multiline placement call; valid source and destination pages are
        // covered by the byte-identical overlay gates.
        let (fragment, cm) = place_form_xobject_canonical(
            dest,
            dest_page_ref,
            *xref,
            trim_rect,
            true,
            true,
            false,
            name,
        )?;
        // cov:ignore-end
        content.push_str(&fragment);
        if let Some((source_index, source_page_ref)) = source_page {
            let source = source_documents.get_mut(*source_index).ok_or_else(|| {
                Error::Unsupported(format!(
                    "overlay source document index {} is out of range",
                    source_index
                ))
            })?;
            let source_page = source.get_object_handle(*source_page_ref);
            let mut destination_page = PageObjectHelper::new(dest_page_ref, dest);
            destination_page.copy_annotations_from(source_page, cm, source)?;
        }
    }
    {
        // cov:ignore-start: the trailing `)?;` is the defensive error edge of
        // a multiline placement call; valid source and destination pages are
        // covered by the byte-identical overlay gates.
        let (fragment, _cm) = place_form_xobject_canonical(
            dest,
            dest_page_ref,
            fx0_ref,
            media_rect,
            true,
            false,
            false,
            "Fx0",
        )?;
        // cov:ignore-end
        content.push_str(&fragment);
    }
    for (name, xref, source_page) in &overlay_names {
        // cov:ignore-start: symmetric defensive error edge for the multiline
        // placement call; byte gates cover successful overlay placements.
        let (fragment, cm) = place_form_xobject_canonical(
            dest,
            dest_page_ref,
            *xref,
            trim_rect,
            true,
            true,
            false,
            name,
        )?;
        // cov:ignore-end
        content.push_str(&fragment);
        if let Some((source_index, source_page_ref)) = source_page {
            let source = source_documents.get_mut(*source_index).ok_or_else(|| {
                Error::Unsupported(format!(
                    "overlay source document index {} is out of range",
                    source_index
                ))
            })?;
            let source_page = source.get_object_handle(*source_page_ref);
            let mut destination_page = PageObjectHelper::new(dest_page_ref, dest);
            destination_page.copy_annotations_from(source_page, cm, source)?;
        }
    }

    // 4. Allocate the new /Contents stream (uncompressed, no /Filter; the writer
    //    compresses on output).
    let contents_ref = next_object_ref(dest)?;
    let contents_stream = Stream::new(Dictionary::new(), content.into_bytes());
    dest.set_object(contents_ref, Object::Stream(contents_stream));

    // 5. Rewrite only /Resources and /Contents on the live page handle. qpdf's
    // copyAnnotations has already appended to this same page's /Annots value;
    // retaining the page handle means that annotation state survives without a
    // raw page-dictionary snapshot or a second resolution route.
    let overlay_page = overlay_page_handle(dest, dest_page_ref)?;
    let resources = ObjectHandle::dictionary(vec![(
        b"/XObject".to_vec(),
        ObjectHandle::dictionary(xobject_entries),
    )]);
    overlay_page.replace_key(b"/Resources", resources)?;
    overlay_page.replace_key(b"/Contents", dest.get_object_handle(contents_ref))?;
    dest.mark_object_handle_dirty(&overlay_page)?;

    Ok(())
}

#[cfg(test)]
fn apply_overlays_to_page<R: Read + Seek>(
    dest: &mut Pdf<R>,
    dest_page_ref: ObjectRef,
    sources: &[OverlaySource],
    _dr_map: &mut crate::overlay_annotations::DrMap,
) -> Result<()> {
    apply_overlays_to_page_with_sources::<R, R>(dest, dest_page_ref, sources, &mut [])
}

/// Pair selected destination pages with source pages, mirroring qpdf's
/// `QPDFJob::handleUnderOverlay` page-mapping loop (qpdf 11.9.0).
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
/// **without applying them**, mirroring qpdf's `QPDFJob::handleUnderOverlay` source
/// preparation for one `--overlay`/`--underlay` group (qpdf 11.9.0).
///
/// `from`, `to`, and `repeat` are the spec's page ranges. `from` (default all
/// source pages) selects source pages; `to` (default all destination pages)
/// selects destination pages; `repeat` is `None` by default (no repetition) and,
/// when `Some`, selects source pages to cycle once `from` is exhausted. The
/// selected pages are paired by [`map_overlay_pages`].
///
/// The distinct source pages used by the mapping are converted to Form XObjects
/// through [`PageObjectHelper::get_form_xobject_for_page`] and imported with
/// `Pdf::copy_foreign_object`. The destination's per-source copier map is kept
/// alive for the later per-placement annotation facade, so shared page and
/// AcroForm resources retain one destination identity just as qpdf's
/// `doUnderOverlayForPage` route does. The result is a
/// `Vec<(dest_page, OverlaySource)>` in `to` order: each entry pairs a 1-based
/// destination page number with an [`OverlaySource`] carrying both the shared
/// imported XObject reference and the source page identity. No destination page
/// is rewritten here; the caller aggregates these across specs and applies them.
///
/// # Errors
///
/// - [`Error::Unsupported`] when a resolved page number falls outside `dest` or
///   `source` (the page lists and counts are read once up front, so this only
///   triggers on an internally inconsistent mapping), or any error propagated
///   from [`PageRange::resolve`] or
///   [`PageObjectHelper::get_form_xobject_for_page`] or
///   `Pdf::copy_foreign_object`.
#[allow(
    clippy::too_many_arguments,
    reason = "mirrors qpdf's per-spec page mapping inputs and retains source identity"
)]
fn spec_page_sources<RS, RT>(
    dest: &mut Pdf<RT>,
    source: &mut Pdf<RS>,
    kind: OverlayKind,
    from: &PageRange,
    to: &PageRange,
    repeat: Option<&PageRange>,
    n_dest: u32,
    source_document_index: usize,
) -> Result<Vec<(u32, OverlaySource)>>
where
    RS: Read + Seek,
    RT: Read + Seek,
{
    // Snapshot the source page list before mutating `dest`. The applied patches
    // change page dictionaries in place but never reorder or remove page
    // objects, so the 1-based page numbers stay valid.
    let source_pages = PageDocumentHelper::new(source).get_all_pages()?;
    let n_source = u32_len(source_pages.len());
    let pairs = resolve_spec_pairs(n_source, from, to, repeat, n_dest)?;

    // Collect distinct source pages in first-use order. Each source page is
    // copied once below; the destination's persistent per-source copier map
    // shares its graph with later annotation copies.
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

    // Import each source page's Form XObject through the live qpdf
    // `copyForeignObject` route. This is deliberately the same per-source
    // copier map later used by `PageObjectHelper::copy_annotations_from`: a
    // font/resource referenced by both the page Form and a copied annotation
    // must resolve to one destination object, just as qpdf's
    // `doUnderOverlayForPage` calls `pdf.copyForeignObject` before
    // `dest_page.copyAnnotations`.
    let mut imported_xobject_refs = Vec::with_capacity(source_refs.len());
    for &page_ref in &source_refs {
        let mut source_page = PageObjectHelper::new(page_ref, source);
        let source_form = source_page.get_form_xobject_for_page(true)?;
        let imported = dest.copy_foreign_object(&source_form)?;
        // cov:ignore-start: copy_foreign_object always returns an indirect destination object; this is an invariant guard for a malformed allocator result.
        imported_xobject_refs.push(imported.object_ref().ok_or_else(|| {
            Error::Unsupported("imported Form XObject is not indirect".to_string())
        })?);
        // cov:ignore-end
    }
    let imported: BTreeMap<u32, ObjectRef> = distinct_sources
        .iter()
        .copied()
        .zip(imported_xobject_refs)
        .collect();

    pairs
        .iter()
        .map(|&(dest_page, source_page)| {
            // `source_page` came from `pairs`, so it is one of `distinct_sources`
            // and is always present in the map; index directly.
            let xobject_ref = imported[&source_page];
            Ok((
                dest_page,
                OverlaySource {
                    kind,
                    xobject_ref,
                    source_page: Some((
                        source_document_index,
                        page_ref_for(&source_pages, source_page, "source")?,
                    )),
                },
            ))
        })
        .collect()
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
/// `QPDFJob::handleUnderOverlay` for one `--overlay`/`--underlay` group (qpdf 11.9.0).
///
/// A thin wrapper over [`spec_page_sources`] + [`apply_overlay_specs`]'s
/// aggregation: the spec's per-destination-page sources are mapped, grouped by
/// destination page, and each affected page is patched by the canonical
/// `PageObjectHelper::copy_annotations_from` route exactly once. Destination
/// pages not in the mapping are left untouched. See [`spec_page_sources`] for
/// the page-mapping and XObject-sharing semantics.
///
/// # Errors
///
/// Propagates any error from [`spec_page_sources`], [`page_ref_for`], the
/// placement facade, or the source document handles.
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
    let n_dest = u32_len(PageDocumentHelper::new(dest).get_all_pages()?.len());
    let sources = spec_page_sources(dest, source, kind, from, to, repeat, n_dest, 0)?;
    let mut source_documents = [source];
    apply_aggregated_sources(
        dest,
        group_sources_by_dest_page(&sources),
        &mut source_documents,
    )
}

/// A single overlay/underlay specification: a source document, its kind, and its
/// `--from`/`--to`/`--repeat` page ranges, as one `--overlay`/`--underlay` group
/// on the qpdf command line.
///
/// [`apply_overlay_specs`] imports source pages via
/// [`Pdf::copy_foreign_object`], which can leave a copied Form XObject's
/// stream data unread until `dest` is written. Keep every `source` here
/// alive at least until `dest` has been fully written (see
/// [`Pdf::copy_foreign_object`]'s own documented requirement).
pub struct OverlaySpec<RS: Read + Seek + 'static> {
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
/// and, within a page, preserves that encounter order (so the canonical
/// placement helper's kind grouping yields underlays-then-overlays with each
/// kind in declaration order).
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
/// qpdf orders overlay/underlay sources this way for both painting and
/// `--verbose` progress reporting.
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

/// Apply already-grouped overlay/underlay sources to `dest`, calling the
/// canonical placement helper **exactly once** per destination page (so each
/// page is converted to `/Fx0` only once). Pages are processed in ascending page
/// order; the per-page source order from `by_page` is preserved.
///
/// # Errors
///
/// Propagates any error from [`PageDocumentHelper::get_all_pages`], [`page_ref_for`], the placement
/// facade, or the source document handles.
fn apply_aggregated_sources<R: Read + Seek, RS: Read + Seek>(
    dest: &mut Pdf<R>,
    by_page: BTreeMap<u32, Vec<OverlaySource>>,
    source_documents: &mut [&mut Pdf<RS>],
) -> Result<()> {
    // Snapshot the repaired dest page refs once; the patches mutate page dicts
    // in place but never reorder or remove page objects, so 1-based numbers
    // stay valid. qpdf prepares all destination pages before it reads any
    // placement boxes or converts the page to a Form XObject.
    let dest_pages = PageDocumentHelper::new(dest).get_all_pages()?;
    for (dest_page, sources) in by_page {
        let dest_ref = page_ref_for(&dest_pages, dest_page, "destination")?;
        apply_overlays_to_page_with_sources(dest, dest_ref, &sources, source_documents)?;
    }
    Ok(())
}

/// Compose multiple overlay/underlay specs onto `dest`, mirroring qpdf's
/// `QPDFJob::handleUnderOverlay` handling of several `--overlay`/`--underlay` groups
/// (qpdf 11.9.0).
///
/// Each [`OverlaySpec`] is mapped independently against `dest`: its `from`/`to`/
/// `repeat` ranges select the source-to-destination page pairing, and each spec's
/// source pages are imported into `dest` once per source document through the
/// qpdf-shaped foreign copier (a source page used on several destination pages
/// is imported once and shared). The per-destination-page sources from all specs are
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
/// Imported Form XObjects are copied via [`Pdf::copy_foreign_object`] and may
/// still depend on their `source` document at write time (see that method's
/// doc). Keep every `spec.source` in `specs` alive until `dest` has been
/// fully written, not just until this function returns.
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
    // qpdf returns before touching the destination page tree at all when there
    // is nothing to overlay or underlay (QPDFJob.cc:1939-1941,
    // `if (m->underlay.empty() && m->overlay.empty()) { return; }`). Match that:
    // an empty `specs` slice must not trigger the repair pass below.
    if specs.is_empty() {
        return Ok(());
    }

    // Map every spec first, collecting its per-dest-page sources in declaration
    // order. Each spec gets its own source-document foreign copier (separate
    // documents => separate qpdf identity maps).
    // The dest page count is invariant while specs are mapped (sources are
    // applied only after the loop), so query the page tree once up front
    // instead of re-walking it per spec.
    // qpdf's overlay job obtains the repaired destination page list before
    // resolving any source ranges or performing placement.
    let n_dest = u32_len(PageDocumentHelper::new(dest).get_all_pages()?.len());
    let mut entries: Vec<(u32, OverlaySource)> = Vec::new();
    for (spec_index, spec) in specs.iter_mut().enumerate() {
        let sources = spec_page_sources(
            dest,
            &mut spec.source,
            spec.kind,
            &spec.from,
            &spec.to,
            spec.repeat.as_ref(),
            n_dest,
            spec_index,
        )?;
        entries.extend(sources);
    }
    let mut source_documents: Vec<&mut Pdf<RS>> =
        specs.iter_mut().map(|spec| &mut spec.source).collect();
    apply_aggregated_sources(
        dest,
        group_sources_by_dest_page(&entries),
        &mut source_documents,
    )
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
/// source page or drawing on the destination.
///
/// The returned vector covers every destination page in ascending order
/// (`1..=n_dest`). Per-page sources are ordered underlays first (in declaration
/// order across `specs`), then overlays (also in declaration order), matching
/// the order [`apply_overlay_specs`] uses to paint the same page. Destination
/// pages that no spec targets appear in the result with an empty `sources`.
///
/// The source documents are taken by `&mut` because [`PageRange::resolve`]
/// reads their page trees; the destination is taken by `&mut` for the same
/// reason, and because [`PageDocumentHelper::get_all_pages`] repairs any page
/// lacking an effective `/MediaBox` in place, matching qpdf's own
/// `QPDFPageDocumentHelper::getAllPages` (qpdf 11.9.0). No source page is
/// imported and no destination content stream is drawn on. Calling this before
/// [`apply_overlay_specs`] on the same specs yields the paint plan that will be
/// applied.
///
/// # Errors
///
/// - [`Error::Parse`] when a `--from`/`--to`/`--repeat` range references a
///   page number outside its document (propagated from
///   [`PageRange::resolve`]).
/// - Any error propagated from [`PageDocumentHelper::get_all_pages`]
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
    let n_dest = u32_len(PageDocumentHelper::new(dest).get_all_pages()?.len());
    // Flatten every spec's (dest_page, source) pairs in declaration order.
    let mut flat: Vec<(u32, OverlayVerboseSource)> = Vec::new();
    for (spec_index, spec) in specs.iter_mut().enumerate() {
        let n_source = u32_len(
            PageDocumentHelper::new(&mut spec.source)
                .get_all_pages()?
                .len(),
        );
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
/// and fallback resolved by [`PageObjectHelper`]). A present but malformed box
/// maps to qpdf's zero rectangle from `getArrayAsRectangle`; only an absent
/// effective box is an error.
fn page_box_or_err<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    kind: BoxKind,
) -> Result<PageBox> {
    let mut helper = PageObjectHelper::new(page_ref, pdf);
    let value = match kind {
        BoxKind::Media => helper.get_media_box(false)?,
        BoxKind::Trim => helper.get_trim_box(false, false)?,
    };
    if value.is_null() {
        return Err(Error::Unsupported(format!(
            "destination page {page_ref} has no usable placement box"
        )));
    }
    // qpdf's getArrayAsRectangle returns the zero rectangle for a present but
    // malformed value; the destination Form-XObject conversion owns the
    // warning that makes the overlay operation observable as repaired.
    let rectangle = rectangle_from_handle(pdf, &value)?.unwrap_or_default();
    Ok(PageBox::new(
        rectangle.llx,
        rectangle.lly,
        rectangle.urx,
        rectangle.ury,
    ))
}

/// Build a placement fragment through the canonical page/Form handle route.
///
/// This is the consumer-side counterpart of
/// [`PageObjectHelper::get_matrix_for_form_xobject_placement`].  Keeping the
/// content fragment here preserves overlay's naming and annotation ordering,
/// while all page-tree inheritance, Form `/BBox`/`/Matrix` resolution, and
/// transformation handling live in the qpdf-shaped helper API.
#[allow(
    clippy::too_many_arguments,
    reason = "mirrors qpdf placement inputs while retaining overlay naming and ordering"
)]
fn place_form_xobject_canonical<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dest_page_ref: ObjectRef,
    form_ref: ObjectRef,
    rect: Rectangle,
    invert_transformations: bool,
    allow_shrink: bool,
    allow_expand: bool,
    name: &str,
) -> Result<(String, Matrix)> {
    let form = pdf.get_object_handle(form_ref);
    let mut helper = PageObjectHelper::new(dest_page_ref, pdf);
    let resource_name = format!("/{name}");
    helper.place_form_xobject(
        form,
        &resource_name,
        rect,
        invert_transformations,
        allow_shrink,
        allow_expand,
    )
}

/// Normalize a rectangle's corners the way qpdf's
/// `QPDFObjectHandle::getArrayAsRectangle` does: `llx = min(x0, x2)`,
/// `lly = min(x1, x3)`, `urx = max(x0, x2)`, `ury = max(x1, x3)`. qpdf reads all
/// box geometry for placement through this accessor, so a page with a reversed box
/// (`llx > urx` or `lly > ury`) still yields a non-negative width/height and places
/// identically to its ordered form.
fn normalize_rectangle(rectangle: PageBox) -> Rectangle {
    Rectangle::new(
        rectangle.llx.min(rectangle.urx),
        rectangle.lly.min(rectangle.ury),
        rectangle.llx.max(rectangle.urx),
        rectangle.lly.max(rectangle.ury),
    )
}

/// Coerce a PDF numeric object to `f64`, matching qpdf's numeric coercion
/// (non-numeric values, including indirect references, contribute `0.0`).
#[cfg(test)]
fn as_f64(o: &Object) -> f64 {
    o.as_integer()
        .map(|i| i as f64)
        .or_else(|| o.as_real())
        .unwrap_or(0.0)
}

/// Read a Form XObject dictionary's `/Matrix` as `[a b c d e f]`, defaulting to
/// the identity when `/Matrix` is absent or not a 6+ element array. The Form
/// XObjects built by [`get_form_xobject_for_page`] always carry a direct `/Matrix`
/// array, so no indirect-reference resolution is needed here.
#[cfg(test)]
fn matrix_or_identity(dict: &Dictionary) -> Matrix {
    match dict.get("Matrix").and_then(Object::as_array) {
        Some(m) if m.len() >= 6 => Matrix::new(
            as_f64(&m[0]),
            as_f64(&m[1]),
            as_f64(&m[2]),
            as_f64(&m[3]),
            as_f64(&m[4]),
            as_f64(&m[5]),
        ),
        _ => Matrix::default(),
    }
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
#[cfg(test)]
fn fo_bbox_and_matrix<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    xobject_ref: ObjectRef,
) -> Result<(Rectangle, Matrix)> {
    let obj = pdf.resolve_object(xobject_ref)?;
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
        Object::Reference(r) => pdf.resolve_object(*r)?,
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
    let bbox = Rectangle::new(
        as_f64(&arr[0]),
        as_f64(&arr[1]),
        as_f64(&arr[2]),
        as_f64(&arr[3]),
    );
    Ok((bbox, matrix))
}

/// Resolve the destination page through the canonical handle registry and
/// validate its dictionary shape before mutating it.
fn overlay_page_handle<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<ObjectHandle> {
    let page = pdf.get_object_handle(page_ref);
    pdf.resolve(&page)?;
    if page.try_as_dictionary()?.is_none() {
        return Err(Error::Unsupported(format!(
            "page {page_ref} is not a dictionary"
        )));
    }
    Ok(page)
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
// CLI-level overlay byte-identity coverage lives in
// `crates/flpdf-cli/tests/cli_byte_identical_overlay.rs` (gated on
// `qpdf-zlib-compat`, same policy as the linearize `cli_byte_identical`
// gate). Those tests run the actual `flpdf` binary with `--static-id`
// [`--qdf --no-original-object-ids`] against a subset of the overlay
// goldens used here (the version-floor / encrypted-source / annotation-
// copy families remain library-only for now), catching CLI-layer wiring
// divergences (argv parsing, PdfWriter setting assembly, defaults) that
// library-only gates cannot see.
#[cfg(all(test, feature = "qpdf-zlib-compat"))]
mod byte_gate {
    use super::{
        apply_overlay_spec, apply_overlay_specs, apply_overlays_to_page, OverlayKind,
        OverlaySource, OverlaySpec,
    };
    use crate::page_form_xobject::import_page_as_form_xobject;
    use crate::pages::page_refs;
    use crate::PageRange;
    use crate::{Object, ObjectRef, Pdf, PdfWriter};
    use std::io::{Read, Seek};
    use std::path::Path;

    fn fixture(name: &str) -> Pdf<std::io::BufReader<std::fs::File>> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat")
            .join(name);
        let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
        Pdf::open(std::io::BufReader::new(file)).unwrap()
    }

    fn write_qpdf<R, F>(dest: &mut Pdf<R>, configure: F) -> Vec<u8>
    where
        R: Read + Seek + 'static,
        F: FnOnce(&mut PdfWriter<'_, R>),
    {
        let mut writer = PdfWriter::new(dest);
        configure(&mut writer);
        writer.set_output_memory().unwrap();
        writer.write().unwrap();
        writer.get_buffer().unwrap()
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
    fn write_static_id<R: Read + Seek + 'static>(dest: &mut Pdf<R>) -> Vec<u8> {
        write_qpdf(dest, |writer| writer.set_static_id(true))
    }

    /// Write `dest` through the `flpdf rewrite --static-id --qdf --no-original-object-ids`
    /// recipe. QDF applies qpdf's conditional `last_char != '\n'` framing rule
    /// so `endstream` stays line-anchored — we leave
    /// `newline_before_endstream` at its default (`Never`).
    fn write_qdf_nooid<R: Read + Seek + 'static>(dest: &mut Pdf<R>) -> Vec<u8> {
        write_qpdf(dest, |writer| {
            writer.set_static_id(true);
            writer.set_qdf_mode(true);
            writer.set_suppress_original_object_ids(true);
        })
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

    /// Return one object body from a QDF output. The hidden-collision gate
    /// deliberately inspects dictionaries rather than searching the whole
    /// file, since the same resource operand appears in both copied `/DA`
    /// strings and appearance-stream content.
    fn qdf_object(output: &[u8], object_number: u32) -> &[u8] {
        let marker = format!("{object_number} 0 obj\n");
        let start = output
            .windows(marker.len())
            .position(|window| window == marker.as_bytes())
            .expect("QDF object marker must exist");
        let body = &output[start + marker.len()..];
        let end_marker = b"\nendobj";
        let end = body
            .windows(end_marker.len())
            .position(|window| window == end_marker)
            .expect("QDF object terminator must exist");
        &body[..end]
    }

    fn qdf_object_contains(object: &[u8], needle: &[u8], context: &str) {
        let needle_text = String::from_utf8_lossy(needle);
        assert!(
            object.windows(needle.len()).any(|window| window == needle),
            "{context}: QDF object is missing {:?}",
            needle_text
        );
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
        let mut dr_map = crate::overlay_annotations::DrMap::new();
        apply_overlays_to_page(
            &mut dest,
            dest_page,
            &[OverlaySource {
                kind: OverlayKind::Overlay,
                xobject_ref: imported,
                source_page: None,
            }],
            &mut dr_map,
        )
        .unwrap();

        // Write through the same recipe as `flpdf rewrite --static-id`.
        let actual = write_static_id(&mut dest);

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
        let (version, max_ext) = accumulate_max(&mut dest, &mut src).get_version();
        let mut specs = vec![OverlaySpec {
            source: src,
            kind: OverlayKind::Overlay,
            from: pr(""),
            to: pr(""),
            repeat: Some(pr("1")),
        }];
        apply_overlay_specs(&mut dest, &mut specs).unwrap();
        let actual = write_qpdf(&mut dest, |writer| {
            writer.set_static_id(true);
            writer.set_qdf_mode(true);
            writer.set_suppress_original_object_ids(true);
            writer.set_minimum_pdf_version(version, max_ext);
        });
        assert_byte_identical(&actual, "overlay-copy-annotations.pdf");
    }

    /// Two `--overlay` specs from the same source, both targeting
    /// destination page 1 — the distinguishing shape between per-placement
    /// and per-page finalization of `addAndRenameFormFields`. The qpdf
    /// 11.9.0 real-CLI probe `qpdf fxo-red.pdf --overlay
    /// form-fields-and-annotations.pdf --to=1 -- --overlay
    /// form-fields-and-annotations.pdf --to=1 -- --qdf --static-id
    /// --no-original-object-ids out.pdf` renames the second placement's
    /// fields "Text Box 1+1"/"Text Box 2+1"/"r1+1": qpdf's `copyAnnotations`
    /// (`QPDFPageObjectHelper.cc:1030`) calls `addAndRenameFormFields` once
    /// per placement, and the trailing `addFormField` walk
    /// (`QPDFAcroFormDocumentHelper.cc:105-108`) updates the qualified-name
    /// cache before the second placement's BFS rename pass reads it. The
    /// sibling `..._fxo_red_repeat1_...` test above repeats one placement
    /// across 16 distinct destination pages and does not exercise this: each
    /// page gets its own fresh `AcroFormDocumentHelper::new(dest).analyze()`
    /// regardless of whether finalization is per-placement or per-page, so
    /// only two placements landing on the *same* page tell them apart.
    #[test]
    fn overlay_copy_annotations_two_specs_same_page_is_byte_identical_qdf() {
        let mut dest = fixture("fxo-red.pdf");
        let mut src1 = fixture("form-fields-and-annotations.pdf");
        let src2 = fixture("form-fields-and-annotations.pdf");
        let (version, max_ext) = accumulate_max(&mut dest, &mut src1).get_version();
        let mut specs = vec![
            OverlaySpec {
                source: src1,
                kind: OverlayKind::Overlay,
                from: pr(""),
                to: pr("1"),
                repeat: None,
            },
            OverlaySpec {
                source: src2,
                kind: OverlayKind::Overlay,
                from: pr(""),
                to: pr("1"),
                repeat: None,
            },
        ];
        apply_overlay_specs(&mut dest, &mut specs).unwrap();
        let actual = write_qpdf(&mut dest, |writer| {
            writer.set_static_id(true);
            writer.set_qdf_mode(true);
            writer.set_suppress_original_object_ids(true);
            writer.set_minimum_pdf_version(version, max_ext);
        });
        assert_byte_identical(&actual, "overlay-copy-annotations-two-specs-same-page.pdf");
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
        let (version, max_ext) = accumulate_max(&mut dest, &mut src).get_version();
        let mut specs = vec![OverlaySpec {
            source: src,
            kind: OverlayKind::Overlay,
            from: pr(""),
            to: pr(""),
            repeat: Some(pr("1")),
        }];
        apply_overlay_specs(&mut dest, &mut specs).unwrap();
        let actual = write_qpdf(&mut dest, |writer| {
            writer.set_static_id(true);
            writer.set_qdf_mode(true);
            writer.set_suppress_original_object_ids(true);
            writer.set_minimum_pdf_version(version, max_ext);
        });
        assert_byte_identical(&actual, "overlay-source-p-and-inline.pdf");
    }

    /// Overlay a source that has annotations (a `/Link` annot) but NO
    /// `/AcroForm` on the catalog — a valid, common shape (only widget
    /// annots require an /AcroForm; link, stamp, freetext, ... do not).
    /// Exercises the "no /AcroForm" branch of
    /// `read_source_acroform_defaults` (`source_dr`, `source_default_da`,
    /// `source_default_q` all return None) that the primary target does
    /// not hit because its source PDF carries an /AcroForm.
    #[test]
    fn overlay_copy_annotations_source_no_acroform_is_byte_identical_qdf() {
        let mut dest = fixture("fxo-red.pdf");
        let mut src = fixture("link-annot-no-acroform.pdf");
        let (version, max_ext) = accumulate_max(&mut dest, &mut src).get_version();
        let mut specs = vec![OverlaySpec {
            source: src,
            kind: OverlayKind::Overlay,
            from: pr(""),
            to: pr(""),
            repeat: Some(pr("1")),
        }];
        apply_overlay_specs(&mut dest, &mut specs).unwrap();
        let actual = write_qpdf(&mut dest, |writer| {
            writer.set_static_id(true);
            writer.set_qdf_mode(true);
            writer.set_suppress_original_object_ids(true);
            writer.set_minimum_pdf_version(version, max_ext);
        });
        assert_byte_identical(&actual, "overlay-link-annot-no-acroform.pdf");
    }

    #[test]
    fn overlay_destination_existing_annotation_is_byte_identical_qdf() {
        let mut dest = fixture("link-annot-no-acroform.pdf");
        let mut source = fixture("one-page.pdf");
        let dest_page = page_refs(&mut dest).unwrap()[0];
        let before = dest
            .resolve_object(dest_page)
            .unwrap()
            .into_dict()
            .unwrap()
            .get("Annots")
            .and_then(Object::as_array)
            .expect("destination fixture must have an annotation array")
            .to_vec();

        apply_overlay_spec(
            &mut dest,
            &mut source,
            OverlayKind::Overlay,
            &pr(""),
            &pr("1"),
            None,
        )
        .unwrap();

        let after = dest
            .resolve_object(dest_page)
            .unwrap()
            .into_dict()
            .unwrap()
            .get("Annots")
            .and_then(Object::as_array)
            .expect("destination annotations must survive the overlay rewrite")
            .to_vec();
        assert_eq!(
            after, before,
            "overlay must preserve destination annotations"
        );

        let actual = write_qdf_nooid(&mut dest);
        assert_byte_identical(&actual, "overlay-destination-existing-annotation.pdf");
    }

    /// Overlay onto a dest whose `/AcroForm/Fields` is stored as an
    /// indirect reference (`/Fields 5 0 R`) instead of a direct array —
    /// a valid PDF shape. Exercises
    /// the canonical helper's indirect `/Fields` append (updates the array
    /// object in place rather than storing a new direct array on the
    /// AcroForm).
    ///
    /// Fixture: `fxo-red-indirect-fields.pdf` is fxo-red with a hand-added
    /// `/AcroForm { /Fields <ref> }` whose Fields ref points at a
    /// standalone array object containing one widget "Text Box 1"; the
    /// source's Text Box 1 must therefore rename to +N on every placement.
    #[test]
    fn overlay_copy_annotations_onto_indirect_fields_is_byte_identical_qdf() {
        let mut dest = fixture("fxo-red-indirect-fields.pdf");
        let mut src = fixture("form-fields-and-annotations.pdf");
        let (version, max_ext) = accumulate_max(&mut dest, &mut src).get_version();
        let mut specs = vec![OverlaySpec {
            source: src,
            kind: OverlayKind::Overlay,
            from: pr(""),
            to: pr(""),
            repeat: Some(pr("1")),
        }];
        apply_overlay_specs(&mut dest, &mut specs).unwrap();
        let actual = write_qpdf(&mut dest, |writer| {
            writer.set_static_id(true);
            writer.set_qdf_mode(true);
            writer.set_suppress_original_object_ids(true);
            writer.set_minimum_pdf_version(version, max_ext);
        });
        assert_byte_identical(&actual, "overlay-onto-indirect-fields.pdf");
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
        let (version, max_ext) = accumulate_max(&mut dest, &mut src).get_version();
        let mut specs = vec![OverlaySpec {
            source: src,
            kind: OverlayKind::Overlay,
            from: pr(""),
            to: pr(""),
            repeat: Some(pr("1")),
        }];
        apply_overlay_specs(&mut dest, &mut specs).unwrap();
        let actual = write_qpdf(&mut dest, |writer| {
            writer.set_static_id(true);
            writer.set_qdf_mode(true);
            writer.set_suppress_original_object_ids(true);
            writer.set_minimum_pdf_version(version, max_ext);
        });
        assert_byte_identical(&actual, "overlay-source-direct-dr.pdf");
    }

    /// Overlay `form-fields-and-annotations.pdf` onto a dest that already
    /// carries an `/AcroForm` with a pre-existing `/Fields` entry named
    /// "Text Box 1" — the same partial name as one of the source's
    /// top-level fields, so the +N collision rename must fire once for
    /// every placement (the source page is repeated onto all 16 dest
    /// pages, so the rename runs 16 times: "Text Box 1+1", "Text Box 1+2",
    /// ...). Also exercises `ensure_dest_acroform_dr`'s existing-`/DR`
    /// short-circuit, the canonical helper's reference-`/AcroForm` and
    /// reference-`/Fields` paths over the pre-existing field, and the tail of
    /// `duplicate_field_tree` that
    /// leaves an existing dest `/DR` untouched.
    ///
    /// Fixture: `fxo-red-with-existing-acroform.pdf` is fxo-red with a
    /// small hand-added `/AcroForm { /DR ... /Fields [<field>] }` whose
    /// field has `/T (Text Box 1)`.
    #[test]
    fn overlay_copy_annotations_onto_existing_acroform_is_byte_identical_qdf() {
        let mut dest = fixture("fxo-red-with-existing-acroform.pdf");
        let mut src = fixture("form-fields-and-annotations.pdf");
        let (version, max_ext) = accumulate_max(&mut dest, &mut src).get_version();
        let mut specs = vec![OverlaySpec {
            source: src,
            kind: OverlayKind::Overlay,
            from: pr(""),
            to: pr(""),
            repeat: Some(pr("1")),
        }];
        apply_overlay_specs(&mut dest, &mut specs).unwrap();
        let actual = write_qpdf(&mut dest, |writer| {
            writer.set_static_id(true);
            writer.set_qdf_mode(true);
            writer.set_suppress_original_object_ids(true);
            writer.set_minimum_pdf_version(version, max_ext);
        });
        assert_byte_identical(&actual, "overlay-onto-existing-acroform.pdf");
    }

    /// Overlay onto a dest whose `/AcroForm/DR` is already populated with a
    /// `/Font /F1` that collides with the source's own `/DR/Font/F1` (dest
    /// `/F1` is Helvetica, source `/F1` is Courier — different refs).
    /// Exercises four qpdf helpers not reached by
    /// `overlay_copy_annotations_onto_existing_acroform_is_byte_identical_qdf`
    /// (whose dest `/AcroForm` has no `/DR` at all):
    ///   1. `QPDFObjectHandle::mergeResources` — rename source `/F1` to
    ///      `/F1_1` on merge into the existing dest `/DR/Font`.
    ///   2. `init_dr_map` — populate `dr_map = {Font: {F1: F1_1}}`.
    ///   3. `adjustDefaultAppearances` — rewrite each copied field's `/DA`
    ///      to reference `/F1_1` instead of `/F1`.
    ///   4. `adjustAppearanceStream` (`ResourceReplacer`) — rewrite the
    ///      `/F1` operand inside each copied field's AP stream content to
    ///      `/F1_1`.
    ///
    /// Fixture: `fxo-red-with-existing-acroform-dr.pdf` is
    /// `fxo-red-with-existing-acroform.pdf` with an indirect `/AcroForm/DR`
    /// added (`/Font << /F1 <ref-to-Helvetica> >>`).
    ///
    /// Golden generated with (qpdf 11.9.0):
    /// ```text
    /// qpdf --qdf --static-id --no-original-object-ids --min-version=1.6 \
    ///   tests/fixtures/compat/fxo-red-with-existing-acroform-dr.pdf \
    ///   --overlay tests/fixtures/compat/form-fields-and-annotations.pdf --repeat=1 \
    ///   -- tests/golden/references/overlay/overlay-onto-existing-acroform-dr.pdf
    /// ```
    ///
    /// Golden inspection confirms all three of:
    ///   - dest `/AcroForm/DR/Font` has both `/F1 -> Helvetica` and
    ///     `/F1_1 -> Courier`.
    ///   - every copied field `/DA` string reads `/F1_1 ... Tf`.
    ///   - at least one copied AP stream content is
    ///     `/Tx BMC q BT /F1_1 18 Tf ... ET Q EMC` with its own
    ///     `/Resources/Font/F1_1` pointing at the Courier font — proving
    ///     `ResourceReplacer` fired on the stream, not just the `/DA` string.
    // cov:ignore-start: the test body is instrumented by llvm-cov but never
    // executes on this branch because it is `#[ignore]`d until Layer 4 wires
    // up `adjust_appearance_stream`. The body IS exercised (and byte-identical
    // against the qpdf 11.9.0 golden) on the top of the stack; keeping it
    // here means the golden and its test doc-comment land alongside the
    // fixture that defines them, rather than being deferred to a later PR.
    #[test]
    fn overlay_copy_annotations_onto_existing_acroform_dr_is_byte_identical_qdf() {
        let mut dest = fixture("fxo-red-with-existing-acroform-dr.pdf");
        let mut src = fixture("form-fields-and-annotations.pdf");
        let (version, max_ext) = accumulate_max(&mut dest, &mut src).get_version();
        let mut specs = vec![OverlaySpec {
            source: src,
            kind: OverlayKind::Overlay,
            from: pr(""),
            to: pr(""),
            repeat: Some(pr("1")),
        }];
        apply_overlay_specs(&mut dest, &mut specs).unwrap();
        let actual = write_qpdf(&mut dest, |writer| {
            writer.set_static_id(true);
            writer.set_qdf_mode(true);
            writer.set_suppress_original_object_ids(true);
            writer.set_minimum_pdf_version(version, max_ext);
        });
        assert_byte_identical(&actual, "overlay-onto-existing-acroform-dr.pdf");
    }
    // cov:ignore-end

    /// Overlay a destination `/DR/Font` whose direct category dictionary
    /// contains an indirect nested dictionary and an existing `/F1_1` key.
    /// qpdf's `getResourceNames` sees only keys inside the nested dictionary
    /// (`QPDFObjectHandle.cc:1155-1172`), so its `mergeResources` collision
    /// path chooses `/F1_1` and overwrites the existing direct key.
    #[test]
    fn overlay_copy_annotations_indirect_font_hidden_collision_is_byte_identical_qdf() {
        let mut dest = fixture("overlay-dr-merge-hidden-collision.pdf");
        let mut src = fixture("form-fields-and-annotations.pdf");
        let (version, max_ext) = accumulate_max(&mut dest, &mut src).get_version();
        let mut specs = vec![OverlaySpec {
            source: src,
            kind: OverlayKind::Overlay,
            from: pr(""),
            to: pr(""),
            repeat: Some(pr("1")),
        }];
        apply_overlay_specs(&mut dest, &mut specs).unwrap();
        let actual = write_qpdf(&mut dest, |writer| {
            writer.set_static_id(true);
            writer.set_qdf_mode(true);
            writer.set_suppress_original_object_ids(true);
            writer.set_minimum_pdf_version(version, max_ext);
        });
        let expected = golden("overlay-dr-merge-hidden-collision.pdf");

        assert_byte_identical(&actual, "overlay-dr-merge-hidden-collision.pdf");
        // Inspect the copied field dictionaries themselves. A whole-file
        // marker search is insufficient because the same operand also occurs
        // in copied AP stream content and could mask a broken `/DA` rewrite.
        for field_ref in [5, 6] {
            let qpdf_field = qdf_object(&expected, field_ref);
            let flpdf_field = qdf_object(&actual, field_ref);
            qdf_object_contains(
                qpdf_field,
                b"/DA (0 0.4 0 rg /F1_1 18 Tf)",
                "qpdf copied field /DA",
            );
            assert!(
                !qpdf_field
                    .windows(b"/F1_2 18 Tf".len())
                    .any(|window| window == b"/F1_2 18 Tf"),
                "qpdf copied field must not use /F1_2 in /DA"
            );
            qdf_object_contains(
                flpdf_field,
                b"/DA (0 0.4 0 rg /F1_1 18 Tf)",
                "flpdf copied field /DA",
            );
            assert!(
                !flpdf_field
                    .windows(b"/F1_2 18 Tf".len())
                    .any(|window| window == b"/F1_2 18 Tf"),
                "flpdf copied field must not use /F1_2 in /DA"
            );
        }

        // Inspect the resource dictionaries behind those operands. Object 4
        // is the copied field's `/DR`; object 31 is the `/Resources` of its
        // `/AP/N` stream (object 12). Operand-only assertions would not catch
        // either dictionary mapping being wrong while the bytes still contain
        // the expected marker.
        let qpdf_dr = qdf_object(&expected, 4);
        qdf_object_contains(qpdf_dr, b"/F1 10 0 R", "qpdf /DR Helvetica mapping");
        qdf_object_contains(qpdf_dr, b"/F1_1 11 0 R", "qpdf /DR Courier mapping");
        assert!(
            !qpdf_dr
                .windows(b"/F1_2 11 0 R".len())
                .any(|window| window == b"/F1_2 11 0 R"),
            "qpdf /DR must not map Courier through /F1_2"
        );

        let flpdf_dr = qdf_object(&actual, 4);
        qdf_object_contains(flpdf_dr, b"/F1 10 0 R", "flpdf /DR Helvetica mapping");
        qdf_object_contains(flpdf_dr, b"/F1_1 11 0 R", "flpdf /DR Courier mapping");
        assert!(
            !flpdf_dr
                .windows(b"/F1_2 11 0 R".len())
                .any(|window| window == b"/F1_2 11 0 R"),
            "flpdf /DR must not map Courier through /F1_2"
        );

        let qpdf_ap_resources = qdf_object(&expected, 31);
        qdf_object_contains(
            qpdf_ap_resources,
            b"/F1_1 11 0 R",
            "qpdf AP `/Resources` Courier mapping",
        );
        assert!(
            !qpdf_ap_resources
                .windows(b"/F1_2 11 0 R".len())
                .any(|window| window == b"/F1_2 11 0 R"),
            "qpdf AP `/Resources` must not use /F1_2"
        );

        let flpdf_ap_resources = qdf_object(&actual, 31);
        qdf_object_contains(
            flpdf_ap_resources,
            b"/F1_1 11 0 R",
            "flpdf AP `/Resources` Courier mapping",
        );
        assert!(
            !flpdf_ap_resources
                .windows(b"/F1_2 11 0 R".len())
                .any(|window| window == b"/F1_2 11 0 R"),
            "flpdf AP `/Resources` must not use /F1_2"
        );
    }

    /// Keep the hidden-collision fixture's empty Flate stream as a valid
    /// initialized zlib stream rather than as zero raw bytes. qpdf 11.9.0
    /// constructs the filter pipeline before writing stream data
    /// (`libqpdf/QPDF_Stream.cc:548-574`), but `Pl_Flate` initializes its
    /// codec only when `handleData` receives bytes
    /// (`libqpdf/Pl_Flate.cc:81-98,112-131`). Its `finish` path consequently
    /// has no initialized codec to flush for a zero-byte input
    /// (`libqpdf/Pl_Flate.cc:188-205`). The fixture must therefore carry a
    /// complete encoded empty stream so readers can exercise the Flate path.
    #[test]
    fn overlay_hidden_collision_fixture_uses_initialized_empty_flate_stream() {
        let mut dest = fixture("overlay-dr-merge-hidden-collision.pdf");
        let stream = dest
            .resolve_object(ObjectRef::new(8, 0))
            .unwrap()
            .into_stream()
            .unwrap();

        assert!(matches!(
            stream.dict.get("Filter"),
            Some(Object::Name(name)) if name.as_slice() == b"FlateDecode"
        ));
        assert!(
            !stream.data.is_empty(),
            "empty Flate stream must contain encoded zlib bytes"
        );
        assert_eq!(
            crate::filters::test_dictionary_api::decode_stream_data(&stream.dict, &stream.data)
                .unwrap(),
            Vec::<u8>::new()
        );
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
        let (version, max_ext) = accumulate_max(&mut dest, &mut src).get_version();
        let mut specs = vec![OverlaySpec {
            source: src,
            kind: OverlayKind::Overlay,
            from: pr(""),
            to: pr(""),
            repeat: Some(pr("1")),
        }];
        apply_overlay_specs(&mut dest, &mut specs).unwrap();
        let actual = write_qpdf(&mut dest, |writer| {
            writer.set_static_id(true);
            writer.set_qdf_mode(true);
            writer.set_suppress_original_object_ids(true);
            writer.set_minimum_pdf_version(version, max_ext);
        });
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
        let (version, max_ext) = accumulate_max(&mut dest, &mut src).get_version();
        let mut specs = vec![OverlaySpec {
            source: src,
            kind: OverlayKind::Underlay,
            from: pr(""),
            to: pr(""),
            repeat: Some(pr("1")),
        }];
        apply_overlay_specs(&mut dest, &mut specs).unwrap();
        let actual = write_qpdf(&mut dest, |writer| {
            writer.set_static_id(true);
            writer.set_qdf_mode(true);
            writer.set_suppress_original_object_ids(true);
            writer.set_minimum_pdf_version(version, max_ext);
        });
        assert_byte_identical(&actual, "underlay-copy-annotations.pdf");
    }

    #[test]
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
    // PdfWriter minimum-version and extension-level setters are the sole inputs
    // exercised at the library boundary.
    /// Return `(max_pdf_version, max_extension_level)` over two open PDFs
    /// using qpdf's pairwise rule: the higher version wins outright, and a
    /// higher version RESETS the extension level (only equal versions merge
    /// via `max`). Mirrors what `flpdf rewrite` needs to accumulate across
    /// dest + all overlay/underlay sources.
    fn accumulate_max<R1: Read + Seek, R2: Read + Seek>(
        a: &mut Pdf<R1>,
        b: &mut Pdf<R2>,
    ) -> crate::PdfVersion {
        let a_version =
            crate::parse_pdf_version(a.version()).unwrap_or(crate::PdfVersion::new(1, 0, 0));
        let b_version =
            crate::parse_pdf_version(b.version()).unwrap_or(crate::PdfVersion::new(1, 0, 0));
        let mut best = crate::PdfVersion::new(
            a_version.major(),
            a_version.minor(),
            a.adobe_extension_level().unwrap_or(0),
        );
        best.update_if_greater(crate::PdfVersion::new(
            b_version.major(),
            b_version.minor(),
            b.adobe_extension_level().unwrap_or(0),
        ));
        best
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
        let (version, max_ext) = accumulate_max(&mut dest, &mut src).get_version();

        let mut specs = vec![OverlaySpec {
            source: src,
            kind: OverlayKind::Overlay,
            from: pr(""),
            to: pr(""),
            repeat: None,
        }];
        apply_overlay_specs(&mut dest, &mut specs).expect("apply overlay");

        let out = write_qpdf(&mut dest, |writer| {
            writer.set_static_id(true);
            writer.set_minimum_pdf_version(version, max_ext);
        });

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
        let (version, max_ext) = accumulate_max(&mut dest, &mut src).get_version();

        let mut specs = vec![OverlaySpec {
            source: src,
            kind: OverlayKind::Overlay,
            from: pr(""),
            to: pr(""),
            repeat: None,
        }];
        apply_overlay_specs(&mut dest, &mut specs).expect("apply overlay");

        let out = write_qpdf(&mut dest, |writer| {
            writer.set_static_id(true);
            writer.set_minimum_pdf_version(version, max_ext);
        });

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
    use crate::pages::page_refs;

    // ---- place_form_xobject ----------------------------------------------

    /// The identity transformation matrix `[1 0 0 1 0 0]`.
    const ID: Matrix = Matrix::new(1.0, 0.0, 0.0, 1.0, 0.0, 0.0);

    #[test]
    fn place_identity_when_same_size() {
        // BBox == rect (612x792 at origin), no fo/dest transform -> identity, centred.
        let (frag, _cm) = place_form_xobject(
            [0.0, 0.0, 612.0, 792.0].into(),
            ID,
            [0.0, 0.0, 612.0, 792.0].into(),
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
            [0.0, 0.0, 300.0, 144.0].into(),
            ID,
            [0.0, 0.0, 612.0, 792.0].into(),
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
            [0.0, 0.0, 612.0, 792.0].into(),
            ID,
            [0.0, 0.0, 300.0, 144.0].into(),
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
            [0.0, 0.0, 612.0, 792.0].into(),
            ID,
            [0.0, 0.0, 300.0, 144.0].into(),
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
            [0.0, 0.0, 301.0, 145.0].into(),
            ID,
            [0.0, 0.0, 612.0, 792.0].into(),
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
            [0.0, 0.0, 0.0, 100.0].into(),
            ID,
            [0.0, 0.0, 200.0, 200.0].into(),
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
            [10.0, 10.0, 510.0, 610.0].into(),
            ID,
            [0.0, 0.0, 612.0, 792.0].into(),
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
        let tmatrix = Matrix::new(0.0, 1.0, -1.0, 0.0, 792.0, 0.0);
        let (frag, _cm) = place_form_xobject(
            [0.0, 0.0, 612.0, 792.0].into(),
            Matrix::new(0.0, -1.0, 1.0, 0.0, 0.0, 612.0),
            [0.0, 0.0, 612.0, 792.0].into(),
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
            normalize_rectangle(PageBox::new(612.0, 792.0, 0.0, 0.0)),
            Rectangle::new(0.0, 0.0, 612.0, 792.0)
        );
        assert_eq!(
            normalize_rectangle(PageBox::new(0.0, 0.0, 612.0, 792.0)),
            Rectangle::new(0.0, 0.0, 612.0, 792.0)
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

        let mut dr_map = crate::overlay_annotations::DrMap::new();
        apply_overlays_to_page(
            &mut pdf,
            page_ref,
            &[OverlaySource {
                kind: OverlayKind::Overlay,
                xobject_ref: overlay,
                source_page: None,
            }],
            &mut dr_map,
        )
        .unwrap();

        // Page /Resources == { /XObject { /Fx0, /Fx1 } } only.
        let page = pdf.resolve_object(page_ref).unwrap();
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
            .resolve_object(contents_ref)
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
        let fx0 = pdf.resolve_object(fx0_ref).unwrap().into_stream().unwrap();
        assert_eq!(
            fx0.dict.get("Subtype").unwrap().as_name(),
            Some(b"Form".as_slice())
        );
        let fx0_res = fx0.dict.get("Resources").unwrap().as_dict().unwrap();
        assert!(fx0_res.get("Font").is_some(), "Fx0 keeps the page's /Font");
    }

    #[test]
    fn apply_underlay_rejects_an_out_of_range_source_document_index() {
        let mut dest = open(one_page_doc("page content"));
        let dest_page_ref = ObjectRef::new(3, 0);
        let source_form = insert_form_xobject(&mut dest, [0, 0, 612, 792], b"underlay");
        let sources = [OverlaySource {
            kind: OverlayKind::Underlay,
            xobject_ref: source_form,
            source_page: Some((0, ObjectRef::new(99, 0))),
        }];
        let mut source_documents: Vec<&mut Pdf<std::io::Cursor<Vec<u8>>>> = Vec::new();

        let error = apply_overlays_to_page_with_sources(
            &mut dest,
            dest_page_ref,
            &sources,
            &mut source_documents,
        )
        .expect_err("an absent source document must be rejected");
        assert!(matches!(error, Error::Unsupported(message) if message.contains("out of range")));
    }

    #[test]
    fn apply_overlay_rejects_an_out_of_range_source_document_index() {
        let mut dest = open(one_page_doc("page content"));
        let dest_page_ref = ObjectRef::new(3, 0);
        let source_form = insert_form_xobject(&mut dest, [0, 0, 612, 792], b"overlay");
        let sources = [OverlaySource {
            kind: OverlayKind::Overlay,
            xobject_ref: source_form,
            source_page: Some((0, ObjectRef::new(99, 0))),
        }];
        let mut source_documents: Vec<&mut Pdf<std::io::Cursor<Vec<u8>>>> = Vec::new();

        let error = apply_overlays_to_page_with_sources(
            &mut dest,
            dest_page_ref,
            &sources,
            &mut source_documents,
        )
        .expect_err("an absent source document must be rejected");
        assert!(matches!(error, Error::Unsupported(message) if message.contains("out of range")));
    }

    #[test]
    fn apply_orders_underlays_then_overlays_in_naming_and_drawing() {
        let mut pdf = open(one_page_doc("page content"));
        let page_ref = ObjectRef::new(3, 0);
        // Declaration order is overlay, underlay; qpdf groups
        // underlay-then-overlay for BOTH naming and drawing.
        let overlay = insert_form_xobject(&mut pdf, [0, 0, 612, 792], b"over");
        let underlay = insert_form_xobject(&mut pdf, [0, 0, 612, 792], b"under");

        let mut dr_map = crate::overlay_annotations::DrMap::new();
        apply_overlays_to_page(
            &mut pdf,
            page_ref,
            &[
                OverlaySource {
                    kind: OverlayKind::Overlay,
                    xobject_ref: overlay,
                    source_page: None,
                },
                OverlaySource {
                    kind: OverlayKind::Underlay,
                    xobject_ref: underlay,
                    source_page: None,
                },
            ],
            &mut dr_map,
        )
        .unwrap();

        let page = pdf.resolve_object(page_ref).unwrap();
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
        let stream = pdf
            .resolve_object(contents_ref)
            .unwrap()
            .into_stream()
            .unwrap();
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

        let mut dr_map = crate::overlay_annotations::DrMap::new();
        apply_overlays_to_page(
            &mut pdf,
            page_ref,
            &[OverlaySource {
                kind: OverlayKind::Overlay,
                xobject_ref: src,
                source_page: None,
            }],
            &mut dr_map,
        )
        .unwrap();

        let page = pdf.resolve_object(page_ref).unwrap();
        let contents_ref = match page.as_dict().unwrap().get("Contents") {
            Some(Object::Reference(r)) => *r,
            other => panic!("Contents ref: {other:?}"), // cov:ignore: defensive — apply always writes /Contents as a reference
        };
        let stream = pdf
            .resolve_object(contents_ref)
            .unwrap()
            .into_stream()
            .unwrap();
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
        let mut dr_map = crate::overlay_annotations::DrMap::new();
        apply_overlays_to_page(
            &mut pdf,
            page_ref,
            &[OverlaySource {
                kind: OverlayKind::Overlay,
                xobject_ref: src,
                source_page: None,
            }],
            &mut dr_map,
        )
        .unwrap();
        let contents_ref = match pdf
            .resolve_object(page_ref)
            .unwrap()
            .as_dict()
            .unwrap()
            .get("Contents")
        {
            Some(Object::Reference(r)) => *r,
            other => panic!("Contents ref: {other:?}"), // cov:ignore: defensive — apply always writes /Contents as a reference
        };
        pdf.resolve_object(contents_ref)
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
        let mut dr_map = crate::overlay_annotations::DrMap::new();
        let err = apply_overlays_to_page(&mut pdf, ObjectRef::new(2, 0), &[], &mut dr_map);
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
    fn overlay_accepts_malformed_destination_trim_box_like_qpdf() {
        // qpdf's getArrayAsRectangle returns the zero rectangle for a malformed
        // destination box, so doUnderOverlayForPage reaches the destination
        // Form-XObject warning and still completes the operation. The previous
        // page_box_or_err path converted the same malformed /TrimBox into a
        // hard Error::Unsupported before that warning could be emitted.
        let content_body = "<< /Length 1 >>\nstream\nx\nendstream";
        let mut pdf = open(build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                     /TrimBox [0 0 5] /Resources << >> /Contents 4 0 R >>",
                ),
                (4, content_body),
            ],
            1,
        ));
        let page_ref = ObjectRef::new(3, 0);
        let source = insert_form_xobject(&mut pdf, [0, 0, 100, 100], b"source");
        let mut dr_map = crate::overlay_annotations::DrMap::new();

        apply_overlays_to_page(
            &mut pdf,
            page_ref,
            &[OverlaySource {
                kind: OverlayKind::Overlay,
                xobject_ref: source,
                source_page: None,
            }],
            &mut dr_map,
        )
        .expect("malformed destination TrimBox must warn and continue like qpdf");

        assert!(
            pdf.repair_diagnostics()
                .entries()
                .iter()
                .any(|diagnostic| diagnostic.message.contains("bounding box is invalid")),
            "destination Form-XObject conversion must retain qpdf's warning"
        );
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
        assert_eq!(bbox, Rectangle::new(0.0, 1.5, 300.0, 144.0));
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
        assert_eq!(bbox, Rectangle::new(0.0, 0.0, 10.0, 20.0));
        assert_eq!(matrix, Matrix::default());
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
        assert_eq!(bbox, Rectangle::new(0.0, 0.0, 10.0, 20.0));
    }

    #[test]
    fn overlay_page_handle_rejects_non_dict() {
        let mut pdf = open(one_page_doc("x"));
        let r = next_object_ref(&pdf).unwrap();
        pdf.set_object(r, Object::Integer(7));
        assert!(matches!(
            overlay_page_handle(&mut pdf, r),
            Err(Error::Unsupported(_))
        ));
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
            Matrix::new(0.0, -1.0, 1.0, 0.0, 0.0, 612.0)
        );
        // Absent /Matrix falls back to the identity.
        assert_eq!(matrix_or_identity(&Dictionary::new()), Matrix::default());
        // A /Matrix with fewer than six elements falls back to the identity.
        let mut short = Dictionary::new();
        short.insert(
            "Matrix",
            Object::Array(vec![Object::Integer(1), Object::Integer(0)]),
        );
        assert_eq!(matrix_or_identity(&short), Matrix::default());
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
            [0.0, 0.0, 612.0, 792.0].into(),
            Matrix::new(0.0, -1.0, 1.0, 0.0, 0.0, 612.0),
            [0.0, 0.0, 612.0, 792.0].into(),
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
        assert_eq!(bbox, Rectangle::new(0.0, 0.0, 612.0, 792.0));
        assert_eq!(matrix, Matrix::new(0.0, -1.0, 1.0, 0.0, 0.0, 612.0));
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
        let page = pdf.resolve_object(page_ref).unwrap();
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
        let page = pdf.resolve_object(page_ref).unwrap();
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
            let page = pdf.resolve_object(page_ref).unwrap();
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

        let page = dest.resolve_object(dest_pages[0]).unwrap();
        let contents_ref = match page.as_dict().unwrap().get("Contents") {
            Some(Object::Reference(r)) => *r,
            other => panic!("Contents ref: {other:?}"), // cov:ignore: defensive — apply always writes /Contents as a reference
        };
        let stream = dest
            .resolve_object(contents_ref)
            .unwrap()
            .into_stream()
            .unwrap();
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
            source_page: None,
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
        let page = pdf.resolve_object(page_ref).unwrap();
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
        let stream = pdf
            .resolve_object(contents_ref)
            .unwrap()
            .into_stream()
            .unwrap();
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

    // An empty `specs` slice must return before touching the destination page
    // tree at all (QPDFJob.cc:1939-1941, `if (m->underlay.empty() &&
    // m->overlay.empty()) { return; }`), so a page lacking an effective
    // `/MediaBox` must NOT be repaired by the get_all_pages() call this
    // function would otherwise make to compute `n_dest`.
    #[test]
    fn apply_overlay_specs_empty_skips_boxless_page_repair() {
        let mut dest = open(build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (3, "<< /Type /Page /Parent 2 0 R >>"),
            ],
            1,
        ));
        let mut specs: Vec<OverlaySpec<std::io::Cursor<Vec<u8>>>> = Vec::new();
        apply_overlay_specs(&mut dest, &mut specs).unwrap();

        let page_ref = ObjectRef {
            number: 3,
            generation: 0,
        };
        let page_dict = dest.resolve_object(page_ref).unwrap().into_dict().unwrap();
        assert!(
            page_dict.get("MediaBox").is_none(),
            "empty specs must not trigger the page-tree repair pass"
        );
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

    #[test]
    fn apply_overlay_specs_source_must_outlive_the_write() {
        // Documents the constraint on `OverlaySpec`/`apply_overlay_specs`
        // (mirroring qpdf's own `copyForeignObject` contract,
        // `include/qpdf/QPDF.hh:401-410`): a copied Form XObject's stream
        // data can still be dispatched from the source `Pdf` when `dest` is
        // written, so dropping the source first is a real, reproducible
        // failure, not merely a hypothetical one.
        use crate::writer::write_qpdf_to_memory;

        let mut dest = open(multi_page_doc(1));
        let mut specs = vec![spec(open(multi_page_doc(1)), OverlayKind::Overlay)];
        apply_overlay_specs(&mut dest, &mut specs).unwrap();
        drop(specs);

        let err = write_qpdf_to_memory(&mut dest, |_| {});
        assert!(
            matches!(&err, Err(Error::Internal(message))
                if message == "pipeStreamData called for non-stream"),
            "dropping the source before write must fail, not silently omit the stream: {err:?}"
        );
    }

    #[test]
    fn apply_overlay_specs_source_kept_alive_writes_successfully() {
        // The documented-safe counterpart to the test above: keeping the
        // source alive until after the write succeeds.
        use crate::writer::write_qpdf_to_memory;

        let mut dest = open(multi_page_doc(1));
        let mut specs = vec![spec(open(multi_page_doc(1)), OverlayKind::Overlay)];
        apply_overlay_specs(&mut dest, &mut specs).unwrap();

        let out = write_qpdf_to_memory(&mut dest, |_| {}).unwrap();
        assert!(!out.is_empty());
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
        let page_before = dest.get_object_handle(page1_ref);
        dest.resolve(&page_before).unwrap();
        let contents_before = page_before.get_key(b"/Contents").unparse();
        let resources_before = page_before.get_key(b"/Resources").unparse();
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
        let page_after = dest.get_object_handle(page1_ref);
        dest.resolve(&page_after).unwrap();
        assert_eq!(contents_before, page_after.get_key(b"/Contents").unparse());
        assert_eq!(
            resources_before,
            page_after.get_key(b"/Resources").unparse()
        );
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
