import sys

# Build a well-formed classic PDF whose ONLY malformation is a /Page leaf shared
# by TWO /Pages parents where ONLY parent A carries a /MediaBox — the fixture that
# makes qpdf 11.9.0 getAllPagesInternal's MediaBox-default-BEFORE-clone ordering
# observable (flpdf-nd38 repair 3 regression guard):
#
#   1 Catalog  << /Type /Catalog /Pages 2 0 R >>
#   2 Pages    << /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>
#   3 A Pages  << /Type /Pages /Parent 2 0 R /Kids [5 0 R] /Count 1 /MediaBox [0 0 200 300] >>
#   4 B Pages  << /Type /Pages /Parent 2 0 R /Kids [5 0 R] /Count 1 >>   % NO /MediaBox
#   5 Page     << /Type /Page /Parent 3 0 R /Resources << >> /Contents 6 0 R >>  % NO /MediaBox
#   6 content  << /Length N >> stream ... endstream
#
# The shared leaf (obj 5) is visited first via A (media_box=true from A's
# /MediaBox -> the default is SUPPRESSED) and second via B (media_box=false ->
# the default [0 0 612 792] fires, and because the default runs BEFORE the
# duplicate-clone at QPDF_pages.cc:104-112 vs :119-130, the minted clone inherits
# it). A same-parent duplicate cannot observe this ordering (the first occurrence
# always defaults the original first); only this cross-parent shape can. Every
# /Type is correct and every kid is indirect, so only the shared-leaf clone
# (flpdf-52md) and the MediaBox default (repair 3) are exercised. Offsets are
# computed programmatically, so qpdf --check reports ONLY the duplicate-page
# warning and NOT xref reconstruction / "file is damaged".

catalog, root_pages, a_pages, b_pages, leaf, content = 1, 2, 3, 4, 5, 6

stream = b"BT /F1 12 Tf 72 720 Td (hi) Tj ET"

objs = {}
objs[catalog] = b"<< /Type /Catalog /Pages %d 0 R >>" % root_pages
objs[root_pages] = (
    b"<< /Type /Pages /Kids [%d 0 R %d 0 R] /Count 2 >>" % (a_pages, b_pages)
)
# Parent A carries a /MediaBox; the shared leaf's first visit (via A) therefore
# sees media_box=true and the default is suppressed.
objs[a_pages] = (
    b"<< /Type /Pages /Parent %d 0 R /Kids [%d 0 R] /Count 1 /MediaBox [0 0 200 300] >>"
    % (root_pages, leaf)
)
# Parent B has NO /MediaBox; the leaf's second visit (via B) sees media_box=false.
objs[b_pages] = (
    b"<< /Type /Pages /Parent %d 0 R /Kids [%d 0 R] /Count 1 >>"
    % (root_pages, leaf)
)
# Shared leaf: correctly typed /Page, NO local /MediaBox.
objs[leaf] = (
    b"<< /Type /Page /Parent %d 0 R /Resources << >> /Contents %d 0 R >>"
    % (a_pages, content)
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
