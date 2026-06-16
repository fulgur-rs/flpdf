import sys

# Build a 3-page PDF that forces a PURE part7 (other-page-private) ObjStm CONTAINER
# to coexist with a part8 (other-page-shared) UNCOMPRESSED object, so the
# linearization numbering discriminates qpdf's per-part interleave
# ([part7 incl. its containers][part8 incl. its plain objects] -> xref) from a
# naive "all plain, then all containers, then xref" ordering.
#
#   - page 0: P0 private fonts  -> small first-page closure (=> stream 0 = part6)
#   - page 1: B page-1-private fonts (enough that a WHOLE middle container is pure
#             page-1-private => part7 CONTAINER) + a shared Form XObject X
#   - page 2: C page-2-private fonts + the SAME shared Form XObject X
#   - X is a STREAM (never compressible) shared by pages 1 & 2 => part8 UNCOMPRESSED
#
# DFS (getCompressibleObjGens): Info, Catalog, Pages, Page0, page0-fonts, Page1,
#   [page1 /Font B-fonts, then /XObject X(stream, excluded)], Page2, page2-fonts.
# With B large the middle even-split container is entirely page-1-private fonts.
P0 = int(sys.argv[1]) if len(sys.argv) > 1 else 2
B = int(sys.argv[2]) if len(sys.argv) > 2 else 250
C = int(sys.argv[3]) if len(sys.argv) > 3 else 2

catalog, pages, page0, page1, page2, info = 1, 2, 3, 4, 5, 6
a0 = 7
a_nums = list(range(a0, a0 + P0))
b0 = a0 + P0
b_nums = list(range(b0, b0 + B))
c_font0 = b0 + B
c_nums = list(range(c_font0, c_font0 + C))
xobj = c_font0 + C          # shared Form XObject (stream)
c0 = xobj + 1
c1 = c0 + 1
c2 = c1 + 1

objs = {}
objs[catalog] = b"<< /Type /Catalog /Pages %d 0 R >>" % pages
objs[pages] = (
    b"<< /Type /Pages /Count 3 /Kids [ %d 0 R %d 0 R %d 0 R ] >>"
    % (page0, page1, page2)
)
objs[info] = b"<< /Producer (flpdf-g6hb part7/part8 discriminator) >>"

a_res = b"<< /Font << %s >> >>" % b" ".join(
    b"/A%d %d 0 R" % (i + 1, n) for i, n in enumerate(a_nums)
)
# page1: B private fonts + shared XObject X
b_font = b" ".join(b"/B%d %d 0 R" % (i + 1, n) for i, n in enumerate(b_nums))
b_res = b"<< /Font << %s >> /XObject << /Fm %d 0 R >> >>" % (b_font, xobj)
# page2: C private fonts + the SAME shared XObject X
c_font = b" ".join(b"/C%d %d 0 R" % (i + 1, n) for i, n in enumerate(c_nums))
c_res = b"<< /Font << %s >> /XObject << /Fm %d 0 R >> >>" % (c_font, xobj)

objs[page0] = (
    b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792] /Resources %s /Contents %d 0 R >>"
    % (pages, a_res, c0)
)
objs[page1] = (
    b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792] /Resources %s /Contents %d 0 R >>"
    % (pages, b_res, c1)
)
objs[page2] = (
    b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792] /Resources %s /Contents %d 0 R >>"
    % (pages, c_res, c2)
)
for i, n in enumerate(a_nums):
    objs[n] = b"<< /Type /Font /Subtype /Type1 /BaseFont /A%d /Mark %d >>" % (i + 1, n)
for i, n in enumerate(b_nums):
    objs[n] = b"<< /Type /Font /Subtype /Type1 /BaseFont /B%d /Mark %d >>" % (i + 1, n)
for i, n in enumerate(c_nums):
    objs[n] = b"<< /Type /Font /Subtype /Type1 /BaseFont /C%d /Mark %d >>" % (i + 1, n)
# Shared Form XObject stream (referenced by pages 1 and 2): never compressible.
xstream = b"q Q"
objs[xobj] = (
    b"<< /Type /XObject /Subtype /Form /BBox [0 0 100 100] /Length %d >>\nstream\n%s\nendstream"
    % (len(xstream), xstream)
)
for cnum, label in ((c0, b"Page0"), (c1, b"Page1"), (c2, b"Page2")):
    stream = b"BT /A1 12 Tf 72 720 Td (%s) Tj ET" % label
    objs[cnum] = b"<< /Length %d >>\nstream\n%s\nendstream" % (len(stream), stream)

out = bytearray(b"%PDF-1.5\n%\xe2\xe3\xcf\xd3\n")
offsets = {}
for num in sorted(objs):
    offsets[num] = len(out)
    out += b"%d 0 obj\n" % num + objs[num] + b"\nendobj\n"
xref_start = len(out)
total = max(objs) + 1
out += b"xref\n0 %d\n0000000000 65535 f \n" % total
for num in range(1, total):
    out += b"%010d 00000 n \n" % offsets[num]
out += b"trailer\n<< /Size %d /Root %d 0 R /Info %d 0 R >>\n" % (total, catalog, info)
out += b"startxref\n%d\n%%%%EOF\n" % xref_start
sys.stdout.buffer.write(out)
