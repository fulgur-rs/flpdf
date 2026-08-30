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
use std::rc::Rc;

use super::page_range::PageRange;
use crate::page_document_helper::PageDocumentHelper;
use crate::page_form_xobject::get_form_xobject_for_page;
use crate::page_object_helper::{rectangle_from_handle, PageBox, PageObjectHelper};
use crate::{Error, Matrix, ObjectHandle, ObjectRef, Pdf, Rectangle, Result};

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
    let contents_stream = dest.new_stream_with_data(Rc::new(content.into_bytes()))?;

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
    overlay_page.replace_key(b"/Contents", contents_stream)?;
    dest.mark_object_handle_dirty(&overlay_page)?;

    Ok(())
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
