"""Build the mini PCLm writer inputs.

The default document is deliberately small (10 objects, two pages, two image
strips per page) and writes each page's ``/Resources /XObject`` dictionary with
its keys in descending order. qpdf's ``enqueueObjectsPCLm`` walks the strips in
``getKeys()`` order, which is ascending, so a writer that seeds strips in
source insertion order produces a different object numbering than qpdf and is
caught by the goldens. Malformed page-tree variants add or replace only the
objects needed to isolate a qpdf repair boundary.
"""

import sys


def stream(dict_extra, data):
    header = "<< " + dict_extra + " /Length %d >>\nstream\n" % len(data)
    return header.encode() + data + b"\nendstream"


def image_object():
    return stream(
        "/Type /XObject /Subtype /Image /Width 1 /Height 1"
        " /ColorSpace /DeviceGray /BitsPerComponent 8",
        b"\x80",
    )


def build(
    root_entry,
    with_extensions=False,
    first_kid=None,
    first_page_body=None,
    first_page_type_with_kids=False,
):
    """Build one input.

    The malformed-tree options exercise qpdf's ``QPDF::getAllPagesInternal``
    behavior:

    ``first_kid`` replaces the first ``/Kids`` *entry* itself, so the leaf is a
    direct object and qpdf promotes it to an indirect page object.

    ``first_page_body`` replaces the body of object 3, the object the first
    ``/Kids`` entry already points at, so the leaf is already indirect.

    ``first_page_type_with_kids`` puts ``/Type /Page`` on object 3 while giving
    it a ``/Kids`` child in object 12. qpdf treats object 3 as a subtree based
    on ``/Kids``, repairs its type to ``/Pages``, and enumerates object 12 first.
    """
    objects = {
        1: b"<< /Type /Catalog /Pages 2 0 R"
        + (b" /Extensions 11 0 R" if with_extensions else b"")
        + b" >>",
        2: b"<< /Type /Pages /Kids ["
        + (first_kid if first_kid is not None else b"3 0 R")
        + b" 4 0 R] /Count 2 >>",
        3: first_page_body
        if first_page_body is not None
        else b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 12 12] /Contents 5 0 R"
        b" /Resources << /XObject << /Sb 7 0 R /Sa 8 0 R >> >> >>",
        4: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 12 12] /Contents 6 0 R"
        b" /Resources << /XObject << /Sd 9 0 R /Sc 10 0 R >> >> >>",
        5: stream("", b"q 1 0 0 1 0 0 cm /Sa Do Q\n"),
        6: stream("", b"q 1 0 0 1 0 0 cm /Sc Do Q\n"),
        7: image_object(),
        8: image_object(),
        9: image_object(),
        10: image_object(),
    }
    if first_page_type_with_kids:
        if first_page_body is not None:
            raise ValueError("first_page_body and first_page_type_with_kids are exclusive")
        objects[3] = b"<< /Type /Page /Parent 2 0 R /Kids [12 0 R] /Count 1 >>"
        objects[12] = (
            b"<< /Type /Page /Parent 3 0 R /MediaBox [0 0 12 12] /Contents 5 0 R"
            b" /Resources << /XObject << /Sb 7 0 R /Sa 8 0 R >> >> >>"
        )
    if with_extensions:
        objects[11] = b"<< /Custom 1 >>"

    out = bytearray(b"%PDF-1.3\n%\xbf\xf7\xa2\xfe\n")
    offsets = {}
    for number in sorted(objects):
        offsets[number] = len(out)
        out += b"%d 0 obj\n" % number + objects[number] + b"\nendobj\n"
    xref = len(out)
    size = max(objects) + 1
    out += b"xref\n0 %d\n0000000000 65535 f \n" % size
    for number in range(1, size):
        if number in offsets:
            out += b"%010d 00000 n \n" % offsets[number]
        else:
            out += b"0000000000 00000 f \n"
    out += b"trailer << /Size %d /Root %s >>\nstartxref\n%d\n%%%%EOF\n" % (
        size,
        root_entry,
        xref,
    )
    return bytes(out)


def main(directory):
    with open(directory + "/mini-pclm-in.pdf", "wb") as handle:
        handle.write(build(b"1 0 R"))
    with open(directory + "/mini-pclm-direct-root-in.pdf", "wb") as handle:
        handle.write(build(b"<< /Type /Catalog /Pages 2 0 R >>"))
    with open(directory + "/mini-pclm-ext-indirect-in.pdf", "wb") as handle:
        handle.write(build(b"1 0 R", with_extensions=True))
    with open(directory + "/mini-pclm-nondict-kid-in.pdf", "wb") as handle:
        handle.write(build(b"1 0 R", first_kid=b"42"))
    with open(directory + "/mini-pclm-nondict-page-in.pdf", "wb") as handle:
        handle.write(build(b"1 0 R", first_page_body=b"42"))
    with open(directory + "/mini-pclm-type-page-kids-in.pdf", "wb") as handle:
        handle.write(build(b"1 0 R", first_page_type_with_kids=True))


if __name__ == "__main__":
    main(sys.argv[1])
