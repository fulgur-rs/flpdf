import sys

# Build a minimal N-page PDF. Object numbering is controllable so we can later
# make DFS-traversal order differ from numeric order. For the first measurement
# we use natural order: 1=Catalog, 2=Pages, 3..N+2=Page dicts.
N = int(sys.argv[1]) if len(sys.argv) > 1 else 120
order = sys.argv[2] if len(sys.argv) > 2 else "natural"  # natural | reverse

catalog_num = 1
pages_num = 2
page_nums = list(range(3, 3 + N))

# /Kids order = the DFS traversal order over pages. For "reverse", list kids in
# descending object number so DFS order != numeric order.
kids_order = list(page_nums)
if order == "reverse":
    kids_order = list(reversed(page_nums))

objs = {}
objs[catalog_num] = b"<< /Type /Catalog /Pages %d 0 R >>" % pages_num
kids = b" ".join(b"%d 0 R" % n for n in kids_order)
objs[pages_num] = b"<< /Type /Pages /Count %d /Kids [ %s ] >>" % (N, kids)
for n in page_nums:
    # Minimal page dict; no content stream (keeps every object a non-stream =
    # eligible). MediaBox inline. Parent points back to pages tree. /PageMark
    # carries the source object number so the page stays identifiable after
    # qpdf renumbers it — lets us recover DFS stream grouping from output.
    objs[n] = (
        b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792] /PageMark %d >>"
        % (pages_num, n)
    )

# Serialize as a classic xref-table PDF.
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
out += b"trailer\n<< /Size %d /Root %d 0 R >>\n" % (total, catalog_num)
out += b"startxref\n%d\n%%%%EOF\n" % xref_start

sys.stdout.buffer.write(out)
