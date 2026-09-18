"""Build the mini PCLm writer inputs.

The document is deliberately small (10 objects, two pages, two image strips
per page) and writes each page's ``/Resources /XObject`` dictionary with its
keys in descending order. qpdf's ``enqueueObjectsPCLm`` walks the strips in
``getKeys()`` order, which is ascending, so a writer that seeds strips in
source insertion order produces a different object numbering than qpdf and is
caught by the goldens.
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


def build(root_entry, with_extensions=False, first_page=None):
    """Build one input.

    ``first_page`` replaces the body of object 3, the first ``/Kids`` leaf.
    Passing a non-dictionary object such as ``b"42"`` exercises qpdf's
    ``QPDF::getAllPagesInternal`` leaf arm, which dispatches on
    ``kid.hasKey("/Kids")`` rather than on ``/Type`` and therefore keeps a
    non-dictionary leaf in the page list.
    """
    objects = {
        1: b"<< /Type /Catalog /Pages 2 0 R"
        + (b" /Extensions 11 0 R" if with_extensions else b"")
        + b" >>",
        2: b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>",
        3: first_page
        if first_page is not None
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
    if with_extensions:
        objects[11] = b"<< /Custom 1 >>"

    out = bytearray(b"%PDF-1.3\n%\xbf\xf7\xa2\xfe\n")
    offsets = {}
    for number in sorted(objects):
        offsets[number] = len(out)
        out += b"%d 0 obj\n" % number + objects[number] + b"\nendobj\n"
    xref = len(out)
    out += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objects) + 1)
    for number in sorted(objects):
        out += b"%010d 00000 n \n" % offsets[number]
    out += b"trailer << /Size %d /Root %s >>\nstartxref\n%d\n%%%%EOF\n" % (
        len(objects) + 1,
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
    with open(directory + "/mini-pclm-nondict-page-in.pdf", "wb") as handle:
        handle.write(build(b"1 0 R", first_page=b"42"))


if __name__ == "__main__":
    main(sys.argv[1])
