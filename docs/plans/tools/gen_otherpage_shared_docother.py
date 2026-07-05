#!/usr/bin/env python3
"""Generate a linearized generate-mode fixture where a document-`others` object
that ALSO reaches one other page is co-located by the even split with a
page-private member of a DIFFERENT page — the flpdf-w0vu part8-vs-part9 drift
between `route_objstm_containers` and `second_half_container_anchors`.

Usage: gen_otherpage_shared_docother.py N1 N2 [P9] [S8]  (default 48 53 1 1)

Shape (3 pages):
  * Page 0 is the FIRST page and is FONTLESS, so no ObjStm-eligible object is
    first-page-reachable; every compressible object lands in a SECOND-half
    container (none routes to part6).
  * Page 1 references N1 private fonts /A%04d (reach {page 1}).
  * Page 2 references N2 fonts /B%04d (reach {page 2}).
  * The catalog carries a CUSTOM key /Custom -> the FIRST page-2 font (B*).
    "/Custom" sorts before "/Pages", so the DFS visits B* right after the
    catalog, pulling it into the FIRST even-split window with the page-1 fonts.
    B* is therefore a document-`others` object (Catalog non-open-document key,
    ou_root_key "/Custom") AND reaches page 2.
  * With P9=1 the catalog also gains /Zzz -> a plain stream reached by nobody
    else => reach 0, others>0 => qpdf lc_other (part9), emitted UNCOMPRESSED
    (streams are ObjStm-ineligible). It is the object the buggy part9 anchor
    would target.
  * With S8=1 both page 1 and page 2 reference a shared /XObject stream =>
    reach 2 => qpdf lc_other_page_shared (part8), also emitted uncompressed.

Even split (qpdf ceil(N/100), N/streams chunking, DFS order; streams are
ineligible so they do not shift the boundary). With N1=48, N2=53 the 106
eligible objects split into two containers of 53:
  * C1 (drift container CX) = {B*, /Pages tree node, 48 page-1 fonts}.
      union other_pages == {1 (fonts), 2 (B*)} => 2
      => route_objstm_containers => lc_other_page_shared (part8).
      second_half_container_anchors sees pages=={1} (only the page-private
      fonts; B* and the /Pages node are in the rest set, not page-private) and
      rest==true, so its `!rest` gate demotes it to part9 (2,0). DRIFT.
  * C2 = {52 page-2 fonts}, other_pages == {2}, others == 0
      => lc_other_page_private (part7). Genuine part7 companion.

qpdf orders C2 (part7) before C1 (part8), then the plain part9 tail. flpdf is
byte-identical to qpdf 11.9.0 here: `route` (correct, part8) drives the member
numbering, and the part9-mislabeled container object lands at the same terminal
pre-container slot a part8 container would (part9 plain is post-container; part8
is the last pre-container part), so the anchor drift is NOT byte-observable.
This fixture pins that non-divergence as a regression guard (flpdf-w0vu); if a
future change to `place_objstm_members_per_half` or either classifier makes the
drift observable, the golden catches it.
"""
import sys


def build(n1: int, n2: int, add_p9: bool, add_s8: bool) -> bytes:
    f1_0 = 6                # first page-1 font object number (/A)
    f2_0 = f1_0 + n1        # first page-2 font object number (/B); B* == f2_0
    c0 = f2_0 + n2          # page 0 content stream
    c1 = c0 + 1             # page 1 content stream
    c2 = c1 + 1             # page 2 content stream
    nxt = c2 + 1
    s8 = nxt if add_s8 else 0
    if add_s8:
        nxt += 1
    p9 = nxt if add_p9 else 0
    if add_p9:
        nxt += 1
    max_obj = nxt - 1
    bstar = f2_0

    objs: dict[int, bytes] = {}
    # "/Custom" sorts before "/Pages" so B* is DFS-visited right after the catalog.
    cat = b"<< /Type /Catalog /Custom %d 0 R /Pages 2 0 R" % bstar
    if add_p9:
        cat += b" /Zzz %d 0 R" % p9
    cat += b" >>"
    objs[1] = cat
    objs[2] = b"<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>"
    # /A%04d and /B%04d resource keys sort lexically so the DFS font order is
    # deterministic and matches ascending object number.
    p1res = b" ".join(b"/A%04d %d 0 R" % (i + 1, f1_0 + i) for i in range(n1))
    p2res = b" ".join(b"/B%04d %d 0 R" % (i + 1, f2_0 + i) for i in range(n2))
    xobj = b" /XObject << /Xs %d 0 R >>" % s8 if add_s8 else b""
    objs[3] = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents %d 0 R >>" % c0
    objs[4] = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << %s >>%s >> /Contents %d 0 R >>" % (p1res, xobj, c1)
    objs[5] = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << %s >>%s >> /Contents %d 0 R >>" % (p2res, xobj, c2)
    for i in range(n1):
        objs[f1_0 + i] = b"<< /Type /Font /Subtype /Type1 /BaseFont /A%04d >>" % (i + 1)
    for i in range(n2):
        objs[f2_0 + i] = b"<< /Type /Font /Subtype /Type1 /BaseFont /B%04d >>" % (i + 1)
    cs0 = b"BT ET"
    cs1 = b"BT /A0001 12 Tf 72 720 Td (P1) Tj ET"
    cs2 = b"BT /B0001 12 Tf 72 720 Td (P2) Tj ET"
    objs[c0] = b"<< /Length %d >>\nstream\n%s\nendstream" % (len(cs0), cs0)
    objs[c1] = b"<< /Length %d >>\nstream\n%s\nendstream" % (len(cs1), cs1)
    objs[c2] = b"<< /Length %d >>\nstream\n%s\nendstream" % (len(cs2), cs2)
    if add_s8:
        sd = b"q Q"
        objs[s8] = b"<< /Type /XObject /Subtype /Form /BBox [0 0 1 1] /Length %d >>\nstream\n%s\nendstream" % (len(sd), sd)
    if add_p9:
        pd = b"custom-data"
        objs[p9] = b"<< /Length %d >>\nstream\n%s\nendstream" % (len(pd), pd)

    out = bytearray(b"%PDF-1.5\n%\xe2\xe3\xcf\xd3\n")
    offs: dict[int, int] = {}
    for n in range(1, max_obj + 1):
        offs[n] = len(out)
        out += b"%d 0 obj\n" % n + objs[n] + b"\nendobj\n"
    xo = len(out)
    size = max_obj + 1
    out += b"xref\n0 %d\n" % size + b"0000000000 65535 f \n"
    for n in range(1, size):
        out += b"%010d 00000 n \n" % offs[n]
    out += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (size, xo)
    return bytes(out)


if __name__ == "__main__":
    n1 = int(sys.argv[1]) if len(sys.argv) > 1 else 48
    n2 = int(sys.argv[2]) if len(sys.argv) > 2 else 53
    p9 = sys.argv[3] == "1" if len(sys.argv) > 3 else True
    s8 = sys.argv[4] == "1" if len(sys.argv) > 4 else True
    sys.stdout.buffer.write(build(n1, n2, p9, s8))
