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

use crate::page_form_xobject::{import_pages_as_form_xobjects, page_to_form_xobject};
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
        let bbox = xobject_placement_box(dest, *xref)?;
        content.push_str(&place_form_xobject(bbox, page_box_array(&trim_box), name));
    }
    {
        let bbox = xobject_placement_box(dest, fx0_ref)?;
        content.push_str(&place_form_xobject(bbox, page_box_array(&media_box), "Fx0"));
    }
    for (name, xref) in &overlay_names {
        let bbox = xobject_placement_box(dest, *xref)?;
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
    // objects, so the 1-based page numbers stay valid. `n_dest` is the
    // destination page count, computed once by the caller (it does not change
    // while sources are being mapped).
    let source_pages = page_refs(source)?;
    let n_source = u32_len(source_pages.len());

    let from_pages = from.resolve(n_source)?;
    let to_pages = to.resolve(n_dest)?;
    let repeat_pages = match repeat {
        Some(pr) => pr.resolve(n_source)?,
        None => Vec::new(),
    };

    let pairs = map_overlay_pages(&from_pages, &to_pages, &repeat_pages);

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
    let imported_refs = import_pages_as_form_xobjects(dest, source, &source_refs)?;
    let imported: BTreeMap<u32, ObjectRef> = distinct_sources
        .iter()
        .copied()
        .zip(imported_refs)
        .collect();

    Ok(pairs
        .iter()
        .map(|&(dest_page, source_page)| {
            // `source_page` came from `pairs`, so it is one of `distinct_sources`
            // and is always present in the map; index directly.
            let xobject_ref = imported[&source_page];
            (dest_page, OverlaySource { kind, xobject_ref })
        })
        .collect())
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
    for &(dest_page, source) in entries {
        by_page.entry(dest_page).or_default().push(source);
    }
    by_page
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

/// Read the imported Form XObject's placement box: its `/BBox` transformed by the
/// XObject's `/Matrix`, returned as the bounding rectangle `[llx lly urx ury]`.
///
/// qpdf's `getMatrixForFormXObjectPlacement` fits the matrix-transformed `/BBox`
/// (not the raw `/BBox`) into the destination rectangle, so a rotated page —
/// whose `/Matrix` swaps width and height — is scaled and centred by its visual
/// extent. Non-numeric `/BBox` elements coerce to `0.0` (matching qpdf); a
/// `/BBox` shorter than four elements is an error; an absent or malformed
/// `/Matrix` is treated as the identity.
fn xobject_placement_box<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    xobject_ref: ObjectRef,
) -> Result<[f64; 4]> {
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
    Ok(transform_bbox(bbox, matrix_or_identity(dict)))
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
//   case                | kind     | source        | --from | --to  | --repeat
//   --------------------|----------|---------------|--------|-------|---------
//   one-page (.16.3)    | overlay  | one-page      | -      | -     | -
//   two-page default    | overlay  | two-page      | -      | -     | -
//   one-page repeat1    | overlay  | one-page      | -      | -     | 1
//   two-page to=2-3     | overlay  | two-page      | -      | 2-3   | -
//   two overlays (.16.5)| overlay×2| one + two     | -      | -     | -
//   overlay+underlay    | over+und | one + two     | -      | -     | -
//   two-page from=2     | overlay  | two-page      | 2      | -     | -
//   underlay two-page   | underlay | two-page      | -      | -     | -
//   rotated (.16.3 mtx) | overlay  | one-page-r90  | -      | -     | -
//   one-page to=1-3 rpt1| overlay  | one-page      | -      | 1-3   | 1
//
// The rotated row is the matrix-transformed placement check: the source page
// carries /Rotate 90, so its imported Form XObject gets a non-identity /Matrix
// and the placement `cm` is fitted to the matrix-transformed bbox (a whole-file
// byte match proves both the /Matrix import and the cm fragment).
//
// Explicit deferrals (NOT covered here, by design):
//   - Encrypted-source --password byte-identity: deferred to flpdf-9hc.16.8
//     (source version-floor propagation). qpdf raises the output version to
//     max(dest, sources) for AES-256 sources; flpdf keeps the dest version, so
//     those bytes diverge. The behavioral --password path is covered in
//     crates/flpdf-cli/tests/cli_overlay.rs.
//   - CLI-level byte-identity: deferred to flpdf-9hc.33. The flpdf CLI emits
//     NewlineBeforeEndstream::Yes and exposes no ::Never (qpdf's default), so
//     every CLI-written stream diverges. These gates write through the library
//     entry points with NewlineBeforeEndstream::Never instead.
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
        let bbox = xobject_placement_box(&mut pdf, r).unwrap();
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
            xobject_placement_box(&mut pdf, r1),
            Err(Error::Unsupported(_))
        ));

        // /BBox too short.
        let mut d2 = Dictionary::new();
        d2.insert("BBox", Object::Array(vec![Object::Integer(0)]));
        let r2 = next_object_ref(&pdf).unwrap();
        pdf.set_object(r2, Object::Stream(Stream::new(d2, Vec::new())));
        assert!(matches!(
            xobject_placement_box(&mut pdf, r2),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn xobject_bbox_rejects_non_stream_non_dict() {
        let mut pdf = open(one_page_doc("x"));
        let r = next_object_ref(&pdf).unwrap();
        pdf.set_object(r, Object::Integer(42));
        assert!(matches!(
            xobject_placement_box(&mut pdf, r),
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
        assert_eq!(
            xobject_placement_box(&mut pdf, r).unwrap(),
            [0.0, 0.0, 10.0, 20.0]
        );
    }

    #[test]
    fn xobject_bbox_resolves_indirect_reference() {
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
        assert_eq!(
            xobject_placement_box(&mut pdf, r).unwrap(),
            [0.0, 0.0, 10.0, 20.0]
        );
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
        // A +90-rotated 612x792 page (Form /Matrix [0 -1 1 0 0 612], /BBox
        // [0 0 612 792]) presents a 792x612 visual box. Placed into a 612x792
        // rect it shrinks to fit and centres exactly as qpdf 11.9.0 emits:
        //   0.77273 0 0 0.77273 0 159.54545
        // (scale=min(612/792,792/612)=0.77273; tx=306-0.77273*396=0;
        //  ty=396-0.77273*306=159.54545). Verified against qpdf --overlay output.
        let transformed =
            transform_bbox([0.0, 0.0, 612.0, 792.0], [0.0, -1.0, 1.0, 0.0, 0.0, 612.0]);
        let frag = place_form_xobject(transformed, [0.0, 0.0, 612.0, 792.0], "Fx1");
        assert_eq!(frag, "q\n0.77273 0 0 0.77273 0 159.54545 cm\n/Fx1 Do\nQ\n");
    }

    #[test]
    fn xobject_placement_box_applies_form_matrix() {
        // A Form XObject carrying a +90 /Matrix reports its matrix-transformed
        // (visual) bounding box, not the raw /BBox.
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
        assert_eq!(
            xobject_placement_box(&mut pdf, r).unwrap(),
            [0.0, 0.0, 792.0, 612.0]
        );
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
}
