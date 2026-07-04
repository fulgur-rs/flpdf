import sys

# Build a well-formed 1-page classic PDF EXCEPT for two mistyped page-tree
# /Type keys, to exercise qpdf 11.9.0 getAllPagesInternal's /Type overrides
# (QPDF_pages.cc:89-92 interior -> /Pages, :131-134 leaf -> /Page) and the
# push-dispatch alignment they enable (flpdf-nd38 repair 2):
#
#   1 Catalog    << /Type /Catalog /Pages 2 0 R >>
#   2 root Pages  << /Type /Pages /Kids [3 0 R] /Count 1 >>
#   3 interior    << /Type /Foo /Parent 2 0 R /Kids [4 0 R] /Count 1 /Rotate 90 >>
#                 % /Type WRONG (should be /Pages); carries an inheritable /Rotate
#                 % so the interior override demonstrably matters to the push.
#   4 leaf        << /Type /Bar /Parent 3 0 R /MediaBox [0 0 612 792]
#                    /Resources << >> /Contents 5 0 R >>   % /Type WRONG (/Page)
#   5 content     << /Length N >> stream ... endstream
#
# Everything else is well-formed: all kids indirect, valid xref table (offsets
# computed programmatically), so qpdf --check reports ONLY the two /Type
# override warnings and NOT xref reconstruction / "file is damaged".

catalog, pages, interior, leaf, content = 1, 2, 3, 4, 5

stream = b"BT /F1 12 Tf 72 720 Td (hi) Tj ET"

objs = {}
objs[catalog] = b"<< /Type /Catalog /Pages %d 0 R >>" % pages
objs[pages] = b"<< /Type /Pages /Kids [%d 0 R] /Count 1 >>" % interior
# Interior node: /Type is /Foo (wrong) and it carries an inheritable /Rotate.
objs[interior] = (
    b"<< /Type /Foo /Parent %d 0 R /Kids [%d 0 R] /Count 1 /Rotate 90 >>"
    % (pages, leaf)
)
# Leaf: /Type is /Bar (wrong).
objs[leaf] = (
    b"<< /Type /Bar /Parent %d 0 R /MediaBox [0 0 612 792] "
    b"/Resources << >> /Contents %d 0 R >>" % (interior, content)
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
