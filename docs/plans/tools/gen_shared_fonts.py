import sys

# Build a 2-page PDF where BOTH pages reference the SAME N font dicts via their
# /Resources /Font. Each font dict is reachable from page 0 AND page 1, so it is
# a *first-page shared* object in qpdf's linearization hint model. With N >= 100
# this forces qpdf's generate-mode even-split to spread the first-page shared
# dicts across more than one object stream — the >cap scenario flpdf-g6hb.2 /
# flpdf-ihb.3 need ground truth for.
#
# Object layout (source numbering chosen so DFS/source order is recoverable):
#   1 = Catalog, 2 = Pages, 3 = Page0, 4 = Page1,
#   5..N+4 = shared font dicts (F1..FN),
#   N+5 = Page0 /Contents, N+6 = Page1 /Contents.
N = int(sys.argv[1]) if len(sys.argv) > 1 else 100

catalog, pages = 1, 2
page0, page1 = 3, 4
font0 = 5
font_nums = list(range(font0, font0 + N))
c0 = font0 + N
c1 = c0 + 1

objs = {}
objs[catalog] = b"<< /Type /Catalog /Pages %d 0 R >>" % pages
objs[pages] = b"<< /Type /Pages /Count 2 /Kids [ %d 0 R %d 0 R ] >>" % (page0, page1)

font_res = b" ".join(b"/F%d %d 0 R" % (i + 1, n) for i, n in enumerate(font_nums))
res = b"<< /Font << %s >> >>" % font_res
objs[page0] = (
    b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792] /Resources %s /Contents %d 0 R >>"
    % (pages, res, c0)
)
objs[page1] = (
    b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792] /Resources %s /Contents %d 0 R >>"
    % (pages, res, c1)
)
for i, n in enumerate(font_nums):
    # Distinct /BaseFont so the dicts are not deduplicated; all are eligible
    # (plain dicts, no streams).
    objs[n] = b"<< /Type /Font /Subtype /Type1 /BaseFont /F%d /Mark %d >>" % (i + 1, n)

for cnum, label in ((c0, b"Page0"), (c1, b"Page1")):
    stream = b"BT /F1 12 Tf 72 720 Td (%s) Tj ET" % label
    objs[cnum] = b"<< /Length %d >>\nstream\n%s\nendstream" % (len(stream), stream)

out = bytearray(b"%PDF-1.5\n%\xe2\xe3\xcf\xd3\n")
offsets = {}
for num in sorted(objs):
    offsets[num] = len(out)
    out += b"%d 0 obj\n" % num + objs[num] + b"\nendobj\n"

xref_start = len(out)
total = max(objs) + 1
out += b"xref\n0 %d\n" % total
out += b"0000000000 65535 f \n"
for num in range(1, total):
    out += b"%010d 00000 n \n" % offsets[num]
out += b"trailer\n<< /Size %d /Root %d 0 R >>\n" % (total, catalog)
out += b"startxref\n%d\n%%%%EOF\n" % xref_start

sys.stdout.buffer.write(out)
