import sys

# Build a well-formed 1-page classic PDF whose ONLY malformation is a DIRECT
# (inline) /Page leaf inside the root /Pages node's /Kids array (rather than an
# indirect `N 0 R` reference), to exercise qpdf 11.9.0 getAllPagesInternal's
# direct-kid -> indirect repair (QPDF_pages.cc:113-118:
# `if (!kid.isIndirect()) { ... kid = makeIndirectObject(kid);
# kids.setArrayItem(i, kid); }`) (flpdf-nd38 repair 1):
#
#   1 Catalog  << /Type /Catalog /Pages 2 0 R >>
#   2 Pages    << /Type /Pages
#                 /Kids [<< /Type /Page /MediaBox [0 0 612 792]
#                           /Contents 3 0 R >>]                 % DIRECT leaf
#                 /Count 1 >>
#   3 content  << /Length N >> stream ... endstream
#
# The leaf is inline inside /Kids (NOT a `N 0 R` reference); its /Contents stays
# an indirect ref to obj 3. The leaf carries its OWN valid /MediaBox and correct
# /Type /Page, so the /MediaBox default (repair 3) and the /Type overrides
# (repair 2) are complete no-ops and the duplicate-leaf clone never fires — ONLY
# the direct-kid -> indirect repair is triggered. qpdf mints the now-indirect
# leaf from the same running object-number allocator the duplicate-clone uses.
# The xref offsets are computed programmatically, so qpdf --check reports ONLY
# the "kid 0 (from 0) is direct; converting to indirect" warning and NOT xref
# reconstruction / "file is damaged".

catalog, pages, content = 1, 2, 3

stream = b"BT /F1 12 Tf 72 720 Td (hi) Tj ET"

objs = {}
objs[catalog] = b"<< /Type /Catalog /Pages %d 0 R >>" % pages
# Root /Pages node: its single /Kids entry is a DIRECT (inline) /Page dict, not
# an indirect reference. The inline leaf carries its own valid /MediaBox and
# correct /Type /Page; /Contents is an indirect ref to obj 3.
objs[pages] = (
    b"<< /Type /Pages /Kids [<< /Type /Page /MediaBox [0 0 612 792] "
    b"/Contents %d 0 R >>] /Count 1 >>" % content
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
