import sys

# Build a well-formed 1-page classic PDF whose ONLY malformation is that the
# catalog's /Pages points INTO the page tree (at the first-page LEAF) instead of
# at the true page-tree root, to exercise qpdf 11.9.0 getAllPages's root->/Pages
# /Parent-chain correction (QPDF_pages.cc:50-67:
#   pages = getRoot().getKey("/Pages")
#   while (pages.isDictionary() && pages.hasKey("/Parent")) {
#       if (!seen.add(pages)) break;         // loop guard
#       changed_pages = true;
#       pages = pages.getKey("/Parent");     // walk UP toward the true root
#   }
#   if (changed_pages) getRoot().replaceKey("/Pages", pages);
# ) (flpdf-nd38 repair 6):
#
#   1 Catalog << /Type /Catalog /Pages 3 0 R >>     % /Pages -> LEAF (obj 3, WRONG)
#   2 Pages   << /Type /Pages /Kids [3 0 R] /Count 1 >>   % the TRUE root (no /Parent)
#   3 Page    << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]
#               /Contents 4 0 R >>                  % leaf; /Parent = the true root
#   4 content << /Length N >> stream ... endstream
#
# The catalog's /Pages points at obj 3 (a /Page leaf), whose /Parent chain
# reaches the true root obj 2 in one hop. Every OTHER aspect is well-formed: the
# leaf has correct /Type /Page, a valid /MediaBox, and an indirect /Contents, and
# the true root obj 2 has correct /Type /Pages and no /Parent — so the /Type
# overrides (repair 2), /MediaBox default (repair 3), direct->indirect (repair 1)
# and duplicate-leaf clone are all complete no-ops and ONLY the root->/Pages
# correction fires. qpdf walks obj 3's /Parent up to obj 2 and rewrites the
# catalog /Pages to point at obj 2 (the true root). The xref offsets are computed
# programmatically, so qpdf --check reports ONLY the "document page tree root
# (root -> /Pages) doesn't point to the root of the page tree; attempting to
# correct" warning and NOT xref reconstruction / "file is damaged".

catalog, pages, leaf, content = 1, 2, 3, 4

stream = b"BT /F1 12 Tf 72 720 Td (hi) Tj ET"

objs = {}
# Catalog /Pages points at the LEAF (obj 3), NOT the true root (obj 2).
objs[catalog] = b"<< /Type /Catalog /Pages %d 0 R >>" % leaf
# The TRUE page-tree root: a /Pages node with no /Parent.
objs[pages] = b"<< /Type /Pages /Kids [%d 0 R] /Count 1 >>" % leaf
# The leaf: a well-formed /Page whose /Parent points back at the true root.
objs[leaf] = (
    b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792] "
    b"/Contents %d 0 R >>" % (pages, content)
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
