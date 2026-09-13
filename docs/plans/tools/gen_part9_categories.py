#!/usr/bin/env python3
"""Generate a linearized Generate fixture with distinct part-9 categories.

Usage: gen_part9_categories.py [N_OUTLINES] [N_REST] [SHARED_THUMB]

The catalog exposes three root children in qpdf's traversal order:

  * /Outlines: an indirect outline chain (``lc_outlines``)
  * /Pages: a two-page tree; the Pages node itself is ``lc_other``
  * /Zzz: a chain of ordinary dictionaries (``lc_other``)

The outline chain is visited before the Pages tree and /Zzz by qpdf's sorted
depth-first eligible-object walk. With the checked-in counts (74, 225), the
even split creates four containers: a Pages/rest container, an outline
container, and two remaining-rest containers after qpdf's linearization
ordering. Linearization must emit Pages/rest before outlines in part 9
(QPDF_linearization.cc:1279-1337), so this fixture exercises a real within-part
sub-order rather than only stable order inside one category.

When SHARED_THUMB is ``1``, both pages reference one image thumbnail whose
dictionary also reaches the /Zzz chain. The chain therefore has two thumbnail
users plus a document-other user. qpdf still classifies it as shared-thumbnail
(``thumbs > 1``), exercising the ordering condition at
QPDF_linearization.cc:1128-1137. When it is ``2``, the same shared thumbnail is
used without the /Meta edge, separating the Pages container from the plain
shared-thumbnail object.
"""
import sys


def build(n_outlines: int, n_rest: int, thumbnail_mode: int) -> bytes:
    if n_outlines <= 0 or n_rest <= 0:
        raise SystemExit("N_OUTLINES and N_REST must both be > 0")

    catalog, outlines, pages, page0, page1 = 1, 2, 3, 4, 5
    outline0 = 6
    outline_nums = list(range(outline0, outline0 + n_outlines))
    rest0 = outline0 + n_outlines
    rest_nums = list(range(rest0, rest0 + n_rest))
    contents0 = rest0 + n_rest
    contents1 = contents0 + 1
    thumbnail = contents1 + 1 if thumbnail_mode in (1, 2) else None
    max_obj = thumbnail if thumbnail is not None else contents1

    objs: dict[int, bytes] = {}
    objs[catalog] = (
        b"<< /Type /Catalog /Outlines %d 0 R /Pages %d 0 R /Zzz %d 0 R >>"
        % (outlines, pages, rest_nums[0])
    )
    objs[outlines] = (
        b"<< /Type /Outlines /First %d 0 R /Last %d 0 R /Count %d >>"
        % (outline_nums[0], outline_nums[-1], n_outlines)
    )
    for i, number in enumerate(outline_nums):
        item = b"<< /Title (Item%d) /Parent %d 0 R" % (i + 1, outlines)
        if i > 0:
            item += b" /Prev %d 0 R" % outline_nums[i - 1]
        if i + 1 < n_outlines:
            item += b" /Next %d 0 R" % outline_nums[i + 1]
        objs[number] = item + b" >>"

    objs[pages] = b"<< /Type /Pages /Kids [ %d 0 R %d 0 R ] /Count 2 >>" % (
        page0,
        page1,
    )
    thumb_ref = b" /Thumb %d 0 R" % thumbnail if thumbnail is not None else b""
    objs[page0] = (
        b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792] /Contents %d 0 R%s >>"
        % (pages, contents0, thumb_ref)
    )
    objs[page1] = (
        b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792] /Contents %d 0 R%s >>"
        % (pages, contents1, thumb_ref)
    )

    for i, number in enumerate(rest_nums):
        next_ref = b" /Next %d 0 R" % rest_nums[i + 1] if i + 1 < n_rest else b""
        objs[number] = b"<< /Mark %d%s >>" % (i + 1, next_ref)

    for number, label in ((contents0, b"Page0"), (contents1, b"Page1")):
        stream = b"BT 72 720 Td (%s) Tj ET" % label
        objs[number] = b"<< /Length %d >>\nstream\n%s\nendstream" % (
            len(stream),
            stream,
        )
    if thumbnail is not None:
        pixel = b"\x00"
        meta_ref = b" /Meta %d 0 R" % rest_nums[0] if thumbnail_mode == 1 else b""
        objs[thumbnail] = (
            b"<< /Type /XObject /Subtype /Image /Width 1 /Height 1 "
            b"/ColorSpace /DeviceGray /BitsPerComponent 8%s /Length %d >>\n"
            b"stream\n%s\nendstream" % (meta_ref, len(pixel), pixel)
        )

    out = bytearray(b"%PDF-1.5\n%\xe2\xe3\xcf\xd3\n")
    offsets: dict[int, int] = {}
    for number in range(1, max_obj + 1):
        offsets[number] = len(out)
        out += b"%d 0 obj\n" % number + objs[number] + b"\nendobj\n"
    xref_start = len(out)
    size = max_obj + 1
    out += b"xref\n0 %d\n0000000000 65535 f \n" % size
    for number in range(1, size):
        out += b"%010d 00000 n \n" % offsets[number]
    out += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (
        size,
        xref_start,
    )
    return bytes(out)


if __name__ == "__main__":
    n_outlines = int(sys.argv[1]) if len(sys.argv) > 1 else 100
    n_rest = int(sys.argv[2]) if len(sys.argv) > 2 else 100
    thumbnail_mode = int(sys.argv[3]) if len(sys.argv) > 3 else 0
    sys.stdout.buffer.write(build(n_outlines, n_rest, thumbnail_mode))
