import sys

# flpdf-7aek reproduction: an even-split ObjStm container that mixes /Outlines
# items (qpdf in_outlines => part9 / Rest, no /PageMode /UseOutlines) with
# other-page-shared fonts (referenced by pages 1 AND 2, NOT page 0 => part8 /
# lc_other_page_shared). route_objstm_containers gives OUTLINE priority, so the
# mixed container is routed to part9; the part8 fonts it carries must NOT appear
# as part8 Shared Object Hint Table entries (canonical_shared_hints must guard
# part9 containers in the input_idx >= first_page_input section too).
#
# Structure (3 pages):
#   - page 0 (first page): references P0 private fonts (small first-page closure)
#   - pages 1 AND 2: both reference the SAME G shared fonts (reach = {1,2}) => part8
#   - /Outlines -> outline dict -> K items reachable ONLY from /Outlines => part9
#
# DFS (getCompressibleObjGens; trailer ascending-key => /Info first; Catalog
# ascending-key => /Outlines before /Pages):
#   Info, Catalog, Outlines, items(K), Pages, Page0, page0-fonts, Page1,
#   G-fonts, Page2.
# The K outline items sit at the FRONT (DFS-early), so they share a container with
# the part8 G-fonts. Because the container holds outline items it routes to part9,
# yet it also carries part8 G-fonts -> the SOHT bug co-location (flpdf-7aek).
#
# Keep the compressible set under the 100-object cap (3 + K + P0 + G <= 100, the
# 4 erased page/Catalog dicts do not count) so the even split makes ONE container.
# A single container isolates the SOHT bug from the even-split page-dict-erasure
# boundary divergence (flpdf-g1eu), which only manifests across a 2-container
# split. The default 2/60/20 (eligible = 3 + 20 + 2 + 60 = 85) is single-container.
P0 = int(sys.argv[1]) if len(sys.argv) > 1 else 2
G = int(sys.argv[2]) if len(sys.argv) > 2 else 60
K = int(sys.argv[3]) if len(sys.argv) > 3 else 20
use_outlines = len(sys.argv) > 4 and sys.argv[4] == "--use-outlines"

if P0 < 0:
    raise SystemExit("P0 must be >= 0")
if G <= 0:
    raise SystemExit("G must be > 0")
if K <= 0:
    raise SystemExit("K must be > 0")

catalog, pages, page0, page1, page2, info, outlines = 1, 2, 3, 4, 5, 6, 7
o0 = 8
item_nums = list(range(o0, o0 + K))
p0_0 = o0 + K
p0_nums = list(range(p0_0, p0_0 + P0))
g0 = p0_0 + P0
g_nums = list(range(g0, g0 + G))
c0 = g0 + G
c1 = c0 + 1
c2 = c1 + 1

objs = {}
if use_outlines:
    objs[catalog] = (
        b"<< /Type /Catalog /PageMode /UseOutlines /Outlines %d 0 R /Pages %d 0 R >>"
        % (outlines, pages)
    )
else:
    objs[catalog] = b"<< /Type /Catalog /Outlines %d 0 R /Pages %d 0 R >>" % (
        outlines,
        pages,
    )
objs[pages] = (
    b"<< /Type /Pages /Count 3 /Kids [ %d 0 R %d 0 R %d 0 R ] >>"
    % (page0, page1, page2)
)
objs[info] = b"<< /Producer (flpdf-7aek outlines + other-page-shared fixture) >>"
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
    entry += b" >>"
    objs[n] = entry

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
