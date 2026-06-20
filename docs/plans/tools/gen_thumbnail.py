import sys

# Build a 3-page PDF where other pages have /Thumb entries, exercising the
# qpdf lc_thumbnail_private and lc_thumbnail_shared linearization categories.
#
# Object layout:
#   1 = Catalog, 2 = Pages,
#   3 = Page0 (first page, no thumb),
#   4 = Page1 (has /Thumb -> thumb_priv, private to page1 only),
#   5 = Page2 (has /Thumb -> thumb_shared, shared with page3),
#   6 = Page3 (has /Thumb -> thumb_shared, same object as page2!),
#   7 = content0, 8 = content1, 9 = content2, 10 = content3,
#   11 = thumb_priv  (1x1 grayscale image, reachable only from page1's /Thumb),
#   12 = thumb_shared (1x1 grayscale image, reachable from page2 AND page3 /Thumb)
#
# lc_thumbnail_private: thumb_priv (reached by exactly 1 page's /Thumb)
# lc_thumbnail_shared:  thumb_shared (reached by 2 pages' /Thumb)
# Expected: both land in part9 (qpdf lc_thumbnail_* => second half).

catalog, pages = 1, 2
page0, page1, page2, page3 = 3, 4, 5, 6
c0, c1, c2, c3 = 7, 8, 9, 10
thumb_priv = 11
thumb_shared = 12

def make_thumb_stream(obj_num, pixel=0xFF):
    """Minimal 1x1 grayscale image stream."""
    data = bytes([pixel])
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
objs[pages] = b"<< /Type /Pages /Count 4 /Kids [ %d 0 R %d 0 R %d 0 R %d 0 R ] >>" % (
    page0, page1, page2, page3
)
objs[page0] = (
    b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792] /Contents %d 0 R >>" % (pages, c0)
)
objs[page1] = (
    b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792] /Contents %d 0 R "
    b"/Thumb %d 0 R >>" % (pages, c1, thumb_priv)
)
objs[page2] = (
    b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792] /Contents %d 0 R "
    b"/Thumb %d 0 R >>" % (pages, c2, thumb_shared)
)
objs[page3] = (
    b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792] /Contents %d 0 R "
    b"/Thumb %d 0 R >>" % (pages, c3, thumb_shared)
)
objs[c0] = make_content(b"Page0")
objs[c1] = make_content(b"Page1")
objs[c2] = make_content(b"Page2")
objs[c3] = make_content(b"Page3")
objs[thumb_priv] = make_thumb_stream(thumb_priv, 0xAA)
objs[thumb_shared] = make_thumb_stream(thumb_shared, 0x55)

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
