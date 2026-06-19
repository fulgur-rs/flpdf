import sys

# >cap fixture exercising qpdf in_open_document PRECEDENCE over in_first_page.
#
# An AcroForm with W widget annotations appears in BOTH:
#   - /AcroForm /Fields (catalog key => closure_from_seeds => in_open_document)
#   - page 0 /Annots (first-page closure => in_first_page)
# qpdf precedence: in_open_document > in_first_page => widget goes to
# lc_open_document => part4 (FIRST half, before /O).
# flpdf-sjgv: without the fix, from_pdf Step 5 places widgets in part2
# (first-page exclusives), inflating page_hints[0].object_count and diverging
# hint tables vs qpdf's output.
#
# S shared fonts: page 0 and page 1 share them => Part 3 (first-half shared).
# DFS order (BTreeMap key order for Catalog: /AcroForm before /Pages before /Type):
#   Catalog, AcroForm, Widgets(W), Pages, Page0, SharedFonts(S), Contents0, Page1, Contents1

W = int(sys.argv[1]) if len(sys.argv) > 1 else 5
S = int(sys.argv[2]) if len(sys.argv) > 2 else 10

catalog, pages, page0, page1, acroform = 1, 2, 3, 4, 5
w0 = 6
w_nums = list(range(w0, w0 + W))
s0 = w0 + W
s_nums = list(range(s0, s0 + S))
c0 = s0 + S
c1 = c0 + 1

objs = {}
objs[catalog] = b"<< /Type /Catalog /AcroForm %d 0 R /Pages %d 0 R >>" % (acroform, pages)
objs[pages] = b"<< /Type /Pages /Count 2 /Kids [ %d 0 R %d 0 R ] >>" % (page0, page1)

fields = b" ".join(b"%d 0 R" % n for n in w_nums)
objs[acroform] = b"<< /Fields [ %s ] >>" % fields

for i, n in enumerate(w_nums):
    objs[n] = b"<< /Type /Annot /Subtype /Widget /Rect [0 0 100 20] /FT /Tx /T (F%d) /V () >>" % (i + 1)

shared_res = b" ".join(b"/SF%d %d 0 R" % (i + 1, n) for i, n in enumerate(s_nums))
res = b"<< /Font << %s >> >>" % shared_res
for i, n in enumerate(s_nums):
    objs[n] = b"<< /Type /Font /Subtype /Type1 /BaseFont /SF%d >>" % (i + 1)

annots = b" ".join(b"%d 0 R" % n for n in w_nums)
objs[page0] = (b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792]"
               b" /Resources %s /Annots [ %s ] /Contents %d 0 R >>"
               % (pages, res, annots, c0))
objs[page1] = (b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792]"
               b" /Resources %s /Contents %d 0 R >>"
               % (pages, res, c1))

for cnum, label in ((c0, b"Page0"), (c1, b"Page1")):
    stream = b"BT /SF1 12 Tf 72 720 Td (%s) Tj ET" % label
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
