import sys

# Build a well-formed 1-page classic PDF whose /Page leaf has a /MediaBox that is
# a DIRECT array with an ELEMENT that is an indirect reference to a number
# ([0 0 612 4 0 R], where obj 4 is the integer 792), and no ancestor /MediaBox.
# qpdf 11.9.0's isRectangle() (QPDFObjectHandle.cc:789-800) tests each item with
# isNumber(), which DEREFERENCES the indirect element, so it treats this as a
# valid rectangle and does NOT apply the letter/ANSI-A default — it keeps the box
# (verified: `qpdf --check` emits no "MediaBox is undefined" warning, and the
# linearized output keeps `/MediaBox [0 0 612 <ref>]`). This pins that flpdf's
# repair (3) resolves each /MediaBox element before deciding to default (codex
# review r3522482671 on PR #453 / flpdf-nd38):
#
#   1 Catalog << /Type /Catalog /Pages 2 0 R >>
#   2 root Pages << /Type /Pages /Kids [3 0 R] /Count 1 >>   % NO /MediaBox
#   3 leaf     << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 4 0 R] /Contents 5 0 R >>
#   4 792                                                    % the indirect element
#   5 content  << /Length N >> stream ... endstream
#
# Every /Type is correct and every kid is indirect, so the /Type, direct->indirect
# and duplicate-leaf repairs are no-ops; only the /MediaBox rectangle check (with
# an indirect element) is exercised. Offsets are computed programmatically so
# qpdf --check reports NO xref reconstruction / "file is damaged".

catalog, pages, leaf, mbox_elem, content = 1, 2, 3, 4, 5

stream = b"BT /F1 12 Tf 72 720 Td (hi) Tj ET"

objs = {}
objs[catalog] = b"<< /Type /Catalog /Pages %d 0 R >>" % pages
objs[pages] = b"<< /Type /Pages /Kids [%d 0 R] /Count 1 >>" % leaf
# Leaf: /MediaBox is a direct array whose last element is an indirect number.
objs[leaf] = (
    b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 %d 0 R] /Contents %d 0 R >>"
    % (pages, mbox_elem, content)
)
objs[mbox_elem] = b"792"
objs[content] = b"<< /Length %d >>\nstream\n%s\nendstream" % (len(stream), stream)

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
