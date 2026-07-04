#!/usr/bin/env python3
"""Generate a linearized generate-mode fixture that exercises the `others` gate
on the part7 (lc_other_page_private) arm of BOTH `route_objstm_containers` and
`second_half_container_anchors` (flpdf-pn7h).

Shape (3 pages; usage: gen_otherpage_others_private.py N1 N2):
  * Page 0 is the FIRST page and is intentionally FONTLESS, so no ObjStm-eligible
    object is first-page-reachable. Every compressible object therefore lands in a
    SECOND-half container (none is routed to part6).
  * Page 1 references N1 private fonts (/A%04d), reached ONLY by page 1.
  * Page 2 references N2 private fonts (/B%04d), reached ONLY by page 2.

qpdf's `generateObjectStreams` even-splits the eligible set (page dicts + root
still counted, erased afterwards) into ceil(eligible/100) streams of <=100 in DFS
order. With N1 ~= N2 and >100 total eligible this yields exactly TWO second-half
containers, split at the page-1/page-2 font boundary:
  * C1 = {Pages tree node} + the N1 page-1 fonts.
      union: other_pages == {page 1}; others > 0 (the /Pages tree node is reached
      via ou_root_key "/Pages", which is neither an open-document key nor
      /Outlines, so it counts as `others`). qpdf -> lc_other (part9).
  * C2 = the N2 page-2 fonts.
      union: other_pages == {page 2}; others == 0. qpdf -> lc_other_page_private
      (part7).

Before the flpdf-pn7h fix, `route_objstm_containers` routed C1 to part7 by
other_pages.len()==1 alone (ignoring `others`), and `second_half_container_anchors`
classified C1 as part7 because it holds a page-private member — both diverging from
qpdf, which orders the containers part7 (C2) before part9 (C1). This fixture pins
the two-container part7/part9 ordering AND numbering byte-identical to qpdf.
"""
import sys


def build(n1: int, n2: int) -> bytes:
    f1_0 = 6                # first page-1 font object number
    f2_0 = f1_0 + n1        # first page-2 font object number
    c0 = f2_0 + n2          # page 0 content stream
    c1 = c0 + 1             # page 1 content stream
    c2 = c1 + 1             # page 2 content stream
    max_obj = c2

    objs: dict[int, bytes] = {}
    objs[1] = b"<< /Type /Catalog /Pages 2 0 R >>"
    objs[2] = b"<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>"
    # /A%04d and /B%04d resource keys sort lexically so the DFS font order is
    # deterministic and matches ascending object number.
    p1res = b" ".join(b"/A%04d %d 0 R" % (i + 1, f1_0 + i) for i in range(n1))
    p2res = b" ".join(b"/B%04d %d 0 R" % (i + 1, f2_0 + i) for i in range(n2))
    objs[3] = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents %d 0 R >>" % c0
    objs[4] = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << %s >> >> /Contents %d 0 R >>" % (p1res, c1)
    objs[5] = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << %s >> >> /Contents %d 0 R >>" % (p2res, c2)
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
    n2 = int(sys.argv[2]) if len(sys.argv) > 2 else 50
    sys.stdout.buffer.write(build(n1, n2))
