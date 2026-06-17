import sys

# >cap fixture exercising the qpdf in_open_document linearization category.
#
# Catalog /OpenAction -> action dict D -> /Refs array of K "od-only" marker
# dicts. Those od objects are reachable ONLY from the root /OpenAction key, so
# qpdf marks them ou_root_key "/OpenAction" => in_open_document => lc_open_document
# => qpdf part4 (FIRST half, right after the Catalog, before the first page).
# flpdf's page-closure-only model sees page_reach 0 => part4_rest (qpdf part9,
# SECOND half), so first_page_object / offsets diverge.
#
# Two pages share S fonts (first-page-shared). DFS order (getCompressibleObjGens;
# root children ascending key => /OpenAction before /Pages before /Type):
#   Catalog, D(openaction), od-fonts(K), Pages, Page0, shared(S), Page1
S = int(sys.argv[1]) if len(sys.argv) > 1 else 80
K = int(sys.argv[2]) if len(sys.argv) > 2 else 80

catalog, pages, page0, page1, action = 1, 2, 3, 4, 5
od0 = 6
od_nums = list(range(od0, od0 + K))
shared0 = od0 + K
shared_nums = list(range(shared0, shared0 + S))
c0 = shared0 + S
c1 = c0 + 1

objs = {}
objs[catalog] = b"<< /Type /Catalog /OpenAction %d 0 R /Pages %d 0 R >>" % (action, pages)
objs[pages] = b"<< /Type /Pages /Count 2 /Kids [ %d 0 R %d 0 R ] >>" % (page0, page1)
refs = b" ".join(b"%d 0 R" % n for n in od_nums)
objs[action] = b"<< /S /GoTo /Refs [ %s ] >>" % refs
for i, n in enumerate(od_nums):
    objs[n] = b"<< /Type /Font /Subtype /Type1 /BaseFont /OD%d /Mark %d >>" % (i + 1, n)

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
