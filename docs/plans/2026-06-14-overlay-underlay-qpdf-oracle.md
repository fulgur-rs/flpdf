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

Defaults: --from=1-z (all source pages), --to=1-z (all dest pages),
--repeat=EMPTY (no repeat). NOTE the issue's "--repeat=z" acceptance text is
WRONG vs qpdf observed and must NOT be used: with no --repeat, when source pages
run out the extra destination pages get NOTHING (confirmed empirically above;
qpdf observed wins per the byte-identical policy).
Pair i-th selected source page with i-th selected dest page; when source pages
run out, ONLY if a --repeat range is given do those repeat pages cycle; with no
--repeat the extra dest pages with no source get nothing. (qpdf manual
help=overlay-underlay + QPDFJob doUnderOverlay loop.)
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

## flpdf infrastructure map (for implementation; verified by code survey)

- Foreign deep copy (qpdf copyForeignObject): `object_copy::copy_objects(&mut
  source, &mut target, &BTreeSet<ObjectRef>) -> BTreeMap<ObjectRef,ObjectRef>`
  (object_copy.rs:76). Pre-allocates target numbers as max(target)+1 in
  **BTreeSet sorted order of source refs**. THIS ORDER may differ from qpdf's
  copyForeignObject traversal order -> the prime object-numbering byte-identity
  risk; verify at .16.3 with the compat baseline.
- Page object closure: `page_closure::extend_page_object_closure(pdf, page_ref,
  &mut BTreeSet)` (boundary-respecting; used by page_merge::merge_documents).
- Raw content streams (undecoded): `pages::page_content_stream_entries(pdf,
  page_ref) -> Vec<(Option<ObjectRef>, Stream)>` (pages.rs:166).
  Decoded+coalesced content: `pages::page_content_bytes` (pages.rs:92). qpdf's
  form XObject stream holds DECODED page content (re-compressed on write);
  verify qpdf's inter-stream separator with multi-contents-one-page.pdf at .16.3.
- Box accessors with inheritance + fallback: `PageObjectHelper::{media_box,
  crop_box,trim_box,...}` (page_object_helper.rs); `rotate()`, `resources()`
  (walks /Parent). trim_box already falls back CropBox->MediaBox.
- /Group and /UserUnit: NO existing helper (zero grep hits). Read /Group from
  the page dict directly and copy as-is when present. UserUnit unsupported in
  flpdf and absent in fixtures -> defer (identity); note as edge case.
- Object construction + allocation: appearance.rs:119-171 +
  `next_object_ref()` (max(refs)+1, then set_object). NOTE qpdf
  getFormXObjectForPage does NOT add /FormType; flpdf appearance.rs DOES — for
  overlay byte-identity OMIT /FormType.
- Compat baseline byte test template: crates/flpdf-cli/tests/
  compat_baseline_static_id.rs (BLESS=1 to bless golden, ByteComparator,
  first-diff). golden under tests/golden/. Run with qpdf-zlib-compat feature.

## Form XObject dict (EXACT, from QDF) — keys in sorted order, NO /FormType

  /BBox /Matrix /Resources /Subtype(/Form) /Type(/XObject) [+/Group if present]
  (+ /Length added by writer). qpdf dicts are std::map => keys WRITTEN SORTED.
  /Matrix always emitted (identity [1 0 0 1 0 0] when no rotate/userunit).

## CRITICAL: object numbering is set by the writer, not at creation time

flpdf's writer renumbers every object via `rewrite_renumber::CatalogFirstRenumber`
(writer.rs:2520) reproducing qpdf's BFS write order: trailer /Root first then
trailer indirect entries in lexicographic key order (/Info => obj 2); each
dequeued object enqueues its references descending dict entries in LEXICOGRAPHIC
byte order of keys and array elements in order; streams walk only the dict.
=> The new objects' creation-time numbers (next_object_ref / copy_objects order)
DO NOT affect final bytes. Final numbering follows the post-overlay GRAPH. Verified
against the golden: obj4=page1, 5=page2, 6=page3, 7=page1 new /Contents, 8=Fx0,
9=Fx1, 10=page2 content, 11=page2 /Font dict, 12/13 page3, 14/15 fonts — exactly
the BFS-by-lexicographic-keys order. So .16.3 only needs the GRAPH + bytes to
match; numbering follows automatically.

## Golden for the .16.3 byte gate (simplest case)

qpdf --static-id --warning-exit-0 three-page.pdf --overlay one-page.pdf -- OUT
Real numbering above. obj7 (new /Contents) = "q\n1 0 0 1 0 0 cm\n/Fx0 Do\nQ\nq\n
1 0 0 1 0 0 cm\n/Fx1 Do\nQ\n" (54 bytes) FlateDecode-compressed to Length 35.

## placeFormXObject uses the MATRIX-TRANSFORMED bbox (rotated pages)

getMatrixForFormXObjectPlacement fits the form's /BBox AFTER applying the form's
/Matrix (the visual extent), not the raw /BBox. So a +90 page (Form /Matrix
[0 -1 1 0 0 w], /BBox [0 0 612 792]) presents a 792x612 box. Empirical
(qpdf 11.9.0, rotated source overlaid onto 612x792):
  cm = 0.77273 0 0 0.77273 0 159.54545
  (transformed bbox [0 0 792 612]; scale=min(612/792,792/612)=0.77273;
   tx=306-0.77273*396=0; ty=396-0.77273*306=159.54545)
=> placement = transform the /BBox 4 corners by /Matrix, take the bounding rect,
then scale-to-fit+centre THAT. Identity /Matrix => transformed == raw (the
simple gate is unaffected). A byte-level rotated golden is deferred to .16.7.

## Page mapping (--from/--to/--repeat) — CONFIRMED algorithm + XObject sharing

from_pages   = --from applied to source (default all source pages, in range order)
to_pages     = --to   applied to dest   (default all dest pages, in range order)
repeat_pages = --repeat applied to source (default EMPTY)
  for i, dest in enumerate(to_pages):
      src = from_pages[i]                         if i < len(from_pages)
          = repeat_pages[(i-len(from_pages)) % len(repeat_pages)]  elif repeat non-empty
          = (skip: this dest page gets NO overlay) else
Confirmed (qpdf 11.9.0, dest=three-page):
  two-page default -> p1<-s1,p2<-s2,p3 none
  one-page --repeat=1 -> p1,p2,p3 all <- s1
  two-page --to=2-3 -> p2<-s1,p3<-s2 (p1 none; pairing is against the --to LIST)
  two-page --from=2 -> p1<-s2 (then exhausted, p2,p3 none)
  one-page --to=1,3 -> p1<-s1 (p3 in --to but source exhausted+no repeat -> none)
  two-page --repeat=2 -> p1<-s1,p2<-s2,p3<-s2

XObject SHARING (byte-identity critical): a source page used on multiple dest
pages is imported ONCE and the SAME XObject ref is shared. repeat1 golden:
page1 Fx1=obj9, page2 Fx1=obj9, page3 Fx1=obj9 (shared); Fx0 differs per page
(8/11/13, each page's own content). Content streams are per-page distinct objects
even when byte-identical (no dedupe). => import distinct source pages once, cache
by source-page index, reuse the ref across dest pages. Only dest pages that
actually receive a source are touched (others left fully untouched, no Fx0).
