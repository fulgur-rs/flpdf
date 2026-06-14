# qpdf overlay/underlay byte-identical oracle (qpdf 11.9.0)

Empirical (qpdf 11.9.0 on tests/fixtures/compat) + source (deepwiki qpdf/qpdf:
QPDFJob::doUnderOverlayForPage, QPDFPageObjectHelper::placeFormXObject /
getFormXObjectForPage / getMatrixForFormXObjectPlacement, QPDFMatrix::unparse).

## Per destination page (only pages that receive >=1 overlay/underlay)

1. Convert the **destination page itself** to a Form XObject named **/Fx0**
   (getFormXObjectForPage):
   - /Type /XObject, /Subtype /Form
   - /BBox = source page TrimBox (fallback CropBox -> MediaBox)
   - /Matrix = getMatrixForTransformations (encodes /Rotate + /UserUnit;
     identity [1 0 0 1 0 0] when neither present)
   - /Resources = shallowCopy of the page's /Resources (NO name prefixing)
   - /Group = shallowCopy of page's /Group if present
   - stream = page content streams concatenated
2. For each **underlay** source page (declaration order), THEN each **overlay**
   source page (declaration order): convert to Form XObject, name via
   getUniqueResourceName("/Fx", suffix) -> /Fx1, /Fx2, ... (page is /Fx0 first).
   NAMING is grouped underlays-then-overlays, NOT raw CLI order.
3. Page /Resources is REBUILT as `<< /XObject << /Fx0 .. /FxN >> >>` only
   (original page resources now live inside Fx0; /Font /ProcSet etc. are gone
   from the page dict). Other page keys (/MediaBox /Parent /Rotate /Trans /Type)
   preserved.
4. Page /Contents REPLACED by a single new stream = concatenation of
   placeFormXObject fragments in **draw order**:
       underlays (declaration order)  -> placed into dest **TrimBox**
       /Fx0 (the page)                -> placed into dest **MediaBox**
       overlays (declaration order)   -> placed into dest **TrimBox**

## placeFormXObject fragment bytes (EXACT)

    q\n<a b c d e f> cm\n/<Name> Do\nQ\n

Each fragment is exactly: "q\n" + matrix + " cm\n/" + name + " Do\nQ\n".
Two identity fragments = 54 bytes (27 each: 2+11+4+7+3).

## cm matrix (getMatrixForFormXObjectPlacement)

    xscale = rect_w / bbox_w
    yscale = rect_h / bbox_h
    scale  = min(xscale, yscale)
    if scale > 1.0 && !allow_expand { scale = 1.0 }   // allow_expand defaults false
    if scale < 1.0 && !allow_shrink { scale = 1.0 }   // allow_shrink defaults true (kept)
    // transformed bbox center -> rect center
    tx = rect_cx - tbbox_cx
    ty = rect_cy - tbbox_cy
    cm = [scale 0 0 scale tx ty]

Defaults: allow_shrink=true, allow_expand=false (never scale up; shrink to fit).
Fx0 (page into its own MediaBox) => identity 1 0 0 1 0 0.

## Number formatting (QPDFMatrix::unparse -> QUtil::double_to_string)

%.5f then strip trailing zeros and trailing '.'. Examples observed:
  1.0       -> "1"
  0.0       -> "0"
  155.5     -> "155.5"
  0.181818..-> "0.18182"   (5 dp, rounded)
  94.3636.. -> "94.36364"  (5 dp, rounded)

## Empirical confirmations (tests/fixtures/compat, qpdf 11.9.0)

- three-page <- overlay one-page (same size): p1 content =
  `q\n1 0 0 1 0 0 cm\n/Fx0 Do\nQ\nq\n1 0 0 1 0 0 cm\n/Fx1 Do\nQ\n` (Len 54);
  pages 2,3 untouched (source exhausted, no --repeat).
- underlay: order Fx1(under) then Fx0(page).
- multi --overlay: Fx0,Fx1,Fx2 all drawn in order (page bottom).
- mixed `--overlay one -- --underlay small --`: Fx1=underlay(small) cm
  `1 0 0 1 156 324`, Fx0 identity, Fx2=overlay(one) -> naming under-then-over.
- 300x144 onto 612x792: `1 0 0 1 156 324` (center, no scale up).
- 612x792 onto 300x144: `0.18182 0 0 0.18182 94.36364 0` (shrink to fit, center).
- 301x145 onto 612x792: `1 0 0 1 155.5 323.5` (fractional center).

## Box selection — EMPIRICALLY CONFIRMED (crafted TrimBox!=MediaBox fixture)

dest: MediaBox[0 0 612 792] CropBox[0 0 600 700] TrimBox[10 10 500 600]
src : MediaBox[0 0 300 144] TrimBox[20 20 220 100]
Result:
  Fx0 /BBox = [10 10 500 600] (dest TrimBox); placement rect = dest MediaBox
      -> cm 1 0 0 1 51 91  (scale=1; tx=306-255=51, ty=396-305=91)
  Fx1 /BBox = [20 20 220 100] (src TrimBox); placement rect = dest TrimBox
      -> cm 1 0 0 1 135 245 (scale=1; tx=255-120=135, ty=305-60=245)
CropBox is NOT used when TrimBox present. tx/ty use BBox CENTERS, not just
w/h (BBox origin may be non-zero): T=scale*bbox; tx=rect_cx-T_cx; ty=rect_cy-T_cy.
Fallback chain remains TrimBox -> CropBox -> MediaBox when TrimBox absent.

## Page mapping (--from/--to/--repeat) -- pin precisely at .16.4 impl

Defaults: --from=1-z (all source pages), --to=1-z (all dest pages), --repeat=z.
Pair i-th selected source page with i-th selected dest page; when source pages
run out, if --repeat range given, cycle those; extra dest pages with no source
get nothing. (qpdf manual help=overlay-underlay + QPDFJob doUnderOverlay loop.)
Re-derive the exact loop from qpdf source at impl time with a correct per-page
inspector (the quick awk counter used during oracle was buggy).

## Byte-identity dependencies (beyond content bytes)

- Content streams are Flate-compressed by qpdf by default -> byte-identical
  comparison requires the `qpdf-zlib-compat` feature (CLAUDE.md DEFLATE carve-out).
- Object numbering / foreign-object copy order must match qpdf's import order
  (object_copy.rs). This is the hardest byte-identity surface for this epic.
- Encrypted source (--password): source decrypted on import; verify object order.

## Implications for the original subtask designs (now superseded by byte-identical)

- .16.2: "import source resources into destination, prefixed to avoid name
  collisions" is WRONG vs qpdf. qpdf keeps each source page's /Resources inside
  its own Fx XObject (shallowCopy, no prefixing). No page-level resource merge.
- .16.2: must also convert the DEST page to /Fx0 (the design omitted this).
- .16.3: NOT "append/prepend q cm Do Q to existing content". qpdf REPLACES the
  page content with a single new stream of fragments (the original content is
  inside /Fx0). Float formatting IS achievable byte-identical (5dp trim).
- .16.5: multi-compose = naming under-then-over, draw under->Fx0->over.
- .16.7: byte parity is now in-scope (feature-gated qpdf-zlib-compat), not a caveat.
