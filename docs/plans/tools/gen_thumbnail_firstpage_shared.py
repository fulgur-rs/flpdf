import sys

# Build a 2-page PDF where a first-page object is ALSO another page's /Thumb.
#
# Object layout:
#   1 = Catalog, 2 = Pages,
#   3 = Page0 (first page): /Resources /XObject /Im0 -> X (obj 5)
#   4 = Page1: /Contents c1, /Thumb -> X (obj 5)  <-- same object as page0's XObject
#   5 = X (1x1 image): reached by page0 /Resources (ou_page 0) AND page1 /Thumb (ou_thumb 1)
#   6 = content0, 7 = content1
#
# qpdf classification for X: in_first_page=true (ou_page 0), thumbs=1 (ou_thumb 1),
# other_pages=0, others=0. QPDF_linearization.cc:1124-1127: thumbs>0 -> lc_first_page_shared.
# X (obj 5) is numbered BELOW content0 (obj 6) so private-vs-shared changes object order:
#   shared (correct): page0, content0, X ; private (bug): page0, X, content0.

catalog, pages = 1, 2
page0, page1 = 3, 4
X = 5
c0, c1 = 6, 7

def make_thumb_stream():
    data = bytes([0xAA])
    return (
        b"<< /Type /XObject /Subtype /Image "
        b"/Width 1 /Height 1 /ColorSpace /DeviceGray "
        b"/BitsPerComponent 8 /Length %d >>\nstream\n%s\nendstream" % (len(data), data)
    )

def make_content(label):
    stream = b"BT /F1 12 Tf 72 720 Td (%s) Tj ET" % label
    return b"<< /Length %d >>\nstream\n%s\nendstream" % (len(stream), stream)

objs = {}
objs[catalog] = b"<< /Type /Catalog /Pages %d 0 R >>" % pages
objs[pages] = b"<< /Type /Pages /Count 2 /Kids [ %d 0 R %d 0 R ] >>" % (page0, page1)
objs[page0] = (
    b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792] "
    b"/Resources << /XObject << /Im0 %d 0 R >> >> /Contents %d 0 R >>"
    % (pages, X, c0)
)
objs[page1] = (
    b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792] /Contents %d 0 R "
    b"/Thumb %d 0 R >>" % (pages, c1, X)
)
objs[X] = make_thumb_stream()
objs[c0] = make_content(b"Page0")
objs[c1] = make_content(b"Page1")

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
