import sys

# Build a well-formed 1-page classic PDF whose ONLY malformation is a /Page
# leaf with no /MediaBox and no ancestor supplying one, to exercise qpdf
# 11.9.0 getAllPagesInternal's letter/ANSI-A default (QPDF_pages.cc:104-112:
# `if (!media_box && !kid.getKey("/MediaBox").isRectangle())` -> replaceKey
# /MediaBox = newArray(Rectangle(0, 0, 612, 792))) (flpdf-nd38 repair 3):
#
#   1 Catalog    << /Type /Catalog /Pages 2 0 R >>
#   2 root Pages  << /Type /Pages /Kids [3 0 R] /Count 1 >>   % NO /MediaBox
#   3 leaf        << /Type /Page /Parent 2 0 R /Resources << >>
#                    /Contents 4 0 R >>                        % NO /MediaBox
#   4 content     << /Length N >> stream ... endstream
#
# Every /Type is correct and every kid is indirect, so the /Type override and
# duplicate-leaf repairs are complete no-ops; ONLY the missing /MediaBox (with
# no ancestor /MediaBox anywhere) triggers the default. The xref offsets are
# computed programmatically, so qpdf --check reports ONLY the MediaBox-undefined
# warning and NOT xref reconstruction / "file is damaged".

catalog, pages, leaf, content = 1, 2, 3, 4

stream = b"BT /F1 12 Tf 72 720 Td (hi) Tj ET"

objs = {}
objs[catalog] = b"<< /Type /Catalog /Pages %d 0 R >>" % pages
# Root /Pages node: NO /MediaBox, so no ancestor supplies one.
objs[pages] = b"<< /Type /Pages /Kids [%d 0 R] /Count 1 >>" % leaf
# Leaf: correctly typed /Page but with NO /MediaBox.
objs[leaf] = (
    b"<< /Type /Page /Parent %d 0 R /Resources << >> /Contents %d 0 R >>"
    % (pages, content)
)
objs[content] = b"<< /Length %d >>\nstream\n%s\nendstream" % (len(stream), stream)

# Serialize as a classic %PDF-1.4 xref-table PDF with a binary comment line.
out = bytearray(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n")
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
