import sys

# Build a 3-page PDF that forces a PURE other-page-shared object stream so we can
# measure qpdf's part8 (lc_other_page_shared) routing for a generate-mode ObjStm
# container whose members are referenced by 2+ NON-first pages.
#
#   - page 0 references a tiny set (P0 private fonts) -> small first-page closure
#   - pages 1 AND 2 both reference the SAME G shared fonts (reach = {1,2}, NOT 0)
#
# DFS (getCompressibleObjGens, trailer children ascending-key => /Info first):
#   Info, Catalog, Pages, Page0, page0-fonts, Page1, G-fonts, Page2.
# With G large enough, the even-split puts a LATER container entirely inside the
# G-fonts run => that container's union = {pages 1,2} => other_pages>1 => part8.
P0 = int(sys.argv[1]) if len(sys.argv) > 1 else 2
G = int(sys.argv[2]) if len(sys.argv) > 2 else 120

catalog, pages, page0, page1, page2, info = 1, 2, 3, 4, 5, 6
p0_0 = 7
p0_nums = list(range(p0_0, p0_0 + P0))
g0 = p0_0 + P0
g_nums = list(range(g0, g0 + G))
c0 = g0 + G
c1 = c0 + 1
c2 = c1 + 1

objs = {}
objs[catalog] = b"<< /Type /Catalog /Pages %d 0 R >>" % pages
objs[pages] = (
    b"<< /Type /Pages /Count 3 /Kids [ %d 0 R %d 0 R %d 0 R ] >>"
    % (page0, page1, page2)
)
objs[info] = b"<< /Producer (flpdf-g6hb three-page shared fixture) >>"

p0_res = b"<< /Font << %s >> >>" % b" ".join(
    b"/A%d %d 0 R" % (i + 1, n) for i, n in enumerate(p0_nums)
)
g_res = b"<< /Font << %s >> >>" % b" ".join(
    b"/G%d %d 0 R" % (i + 1, n) for i, n in enumerate(g_nums)
)
objs[page0] = (
    b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792] /Resources %s /Contents %d 0 R >>"
    % (pages, p0_res, c0)
)
objs[page1] = (
    b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792] /Resources %s /Contents %d 0 R >>"
    % (pages, g_res, c1)
)
objs[page2] = (
    b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792] /Resources %s /Contents %d 0 R >>"
    % (pages, g_res, c2)
)
for i, n in enumerate(p0_nums):
    objs[n] = b"<< /Type /Font /Subtype /Type1 /BaseFont /A%d /Mark %d >>" % (i + 1, n)
for i, n in enumerate(g_nums):
    objs[n] = b"<< /Type /Font /Subtype /Type1 /BaseFont /G%d /Mark %d >>" % (i + 1, n)
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
