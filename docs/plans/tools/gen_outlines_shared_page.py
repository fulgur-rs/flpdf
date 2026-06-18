import sys

# >cap fixture exercising the outline∩page object-overlap scenario (flpdf-vvjr.4 part A).
#
# Same structure as gen_outlines_gap.py (S shared fonts between two pages, K outline
# items reachable only from /Outlines) but outline item 1 gains an extra key:
#   /Extra <shared_nums[0]> 0 R
# making shared_nums[0] (font F1) reachable from BOTH /Outlines (via closure) AND the
# two pages' /Resources.  qpdf categorizes F1 as in_outlines (higher precedence than
# in_first_page), removing it from the first-page shared set; flpdf must do the same.
#
# DFS order (getCompressibleObjGens):
#   Catalog, Outlines, item0_with_extra, F1, items(K-1), Pages, Page0, F2..FS, Page1
S = int(sys.argv[1]) if len(sys.argv) > 1 else 80
K = int(sys.argv[2]) if len(sys.argv) > 2 else 80
use_outlines = len(sys.argv) > 3 and sys.argv[3] == "--use-outlines"

catalog, pages, page0, page1, outlines = 1, 2, 3, 4, 5
o0 = 6
item_nums = list(range(o0, o0 + K))
shared0 = o0 + K
shared_nums = list(range(shared0, shared0 + S))
c0 = shared0 + S
c1 = c0 + 1

objs = {}
if use_outlines:
    objs[catalog] = b"<< /Type /Catalog /PageMode /UseOutlines /Outlines %d 0 R /Pages %d 0 R >>" % (outlines, pages)
else:
    objs[catalog] = b"<< /Type /Catalog /Outlines %d 0 R /Pages %d 0 R >>" % (outlines, pages)
objs[pages] = b"<< /Type /Pages /Count 2 /Kids [ %d 0 R %d 0 R ] >>" % (page0, page1)
objs[outlines] = b"<< /Type /Outlines /First %d 0 R /Last %d 0 R /Count %d >>" % (
    item_nums[0],
    item_nums[-1],
    K,
)
for i, n in enumerate(item_nums):
    entry = b"<< /Title (Item%d) /Parent %d 0 R" % (i + 1, outlines)
    if i > 0:
        entry += b" /Prev %d 0 R" % item_nums[i - 1]
    if i < K - 1:
        entry += b" /Next %d 0 R" % item_nums[i + 1]
    # Item 0 has an extra key pointing to F1 (shared_nums[0]).
    # This makes F1 reachable from /Outlines, creating the overlap with pages.
    if i == 0 and S > 0:
        entry += b" /Extra %d 0 R" % shared_nums[0]
    entry += b" >>"
    objs[n] = entry

shared_res = b" ".join(b"/S%d %d 0 R" % (i + 1, n) for i, n in enumerate(shared_nums))
res = b"<< /Font << %s >> >>" % shared_res
objs[page0] = b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792] /Resources %s /Contents %d 0 R >>" % (pages, res, c0)
objs[page1] = b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792] /Resources %s /Contents %d 0 R >>" % (pages, res, c1)
for i, n in enumerate(shared_nums):
    objs[n] = b"<< /Type /Font /Subtype /Type1 /BaseFont /S%d /Mark %d >>" % (i + 1, n)
for cnum, label in ((c0, b"Page0"), (c1, b"Page1")):
    stream = b"BT /S1 12 Tf 72 720 Td (%s) Tj ET" % label
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
out += b"trailer\n<< /Size %d /Root %d 0 R >>\n" % (total, catalog)
out += b"startxref\n%d\n%%%%EOF\n" % xref_start
sys.stdout.buffer.write(out)
