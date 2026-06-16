import sys

# Build a 2-page PDF that forces a single generate-mode object stream to STRADDLE
# the linearization first-page / second-half boundary, so we can measure qpdf's
# part6/part8 placement rule for a section-spanning container.
#
#   - page 0 references S "shared" font dicts (also referenced by page 1)
#   - page 1 references those S shared dicts PLUS P "page1-only" font dicts
#   - the trailer carries an /Info dict
#
# DFS order (getCompressibleObjGens, trailer children ascending-key => /Info
# before /Root): Info, Catalog, Pages, Page0, shared(S), Page1, page1-only(P).
# Total eligible = 4 + S + P + 1 (Info). With S=60, P=70 => 135 eligible =>
# ceil/2 = 68 per stream, so stream 0 spans shared + Page1 + a few page1-only.
S = int(sys.argv[1]) if len(sys.argv) > 1 else 60
P = int(sys.argv[2]) if len(sys.argv) > 2 else 70

catalog, pages, page0, page1, info = 1, 2, 3, 4, 5
shared0 = 6
shared_nums = list(range(shared0, shared0 + S))
p1only0 = shared0 + S
p1only_nums = list(range(p1only0, p1only0 + P))
c0 = p1only0 + P
c1 = c0 + 1

objs = {}
objs[catalog] = b"<< /Type /Catalog /Pages %d 0 R >>" % pages
objs[pages] = b"<< /Type /Pages /Count 2 /Kids [ %d 0 R %d 0 R ] >>" % (page0, page1)
objs[info] = b"<< /Producer (flpdf-g6hb mixed fixture) >>"

shared_res = b" ".join(b"/S%d %d 0 R" % (i + 1, n) for i, n in enumerate(shared_nums))
p0_res = b"<< /Font << %s >> >>" % shared_res
p1_font = shared_res + b" " + b" ".join(
    b"/P%d %d 0 R" % (i + 1, n) for i, n in enumerate(p1only_nums)
)
p1_res = b"<< /Font << %s >> >>" % p1_font

objs[page0] = (
    b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792] /Resources %s /Contents %d 0 R >>"
    % (pages, p0_res, c0)
)
objs[page1] = (
    b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792] /Resources %s /Contents %d 0 R >>"
    % (pages, p1_res, c1)
)
for i, n in enumerate(shared_nums):
    objs[n] = b"<< /Type /Font /Subtype /Type1 /BaseFont /S%d /Mark %d >>" % (i + 1, n)
for i, n in enumerate(p1only_nums):
    objs[n] = b"<< /Type /Font /Subtype /Type1 /BaseFont /P%d /Mark %d >>" % (i + 1, n)
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
out += b"trailer\n<< /Size %d /Root %d 0 R /Info %d 0 R >>\n" % (total, catalog, info)
out += b"startxref\n%d\n%%%%EOF\n" % xref_start
sys.stdout.buffer.write(out)
