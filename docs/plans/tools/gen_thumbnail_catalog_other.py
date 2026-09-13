#!/usr/bin/env python3
"""Generate a one-page thumbnail that is also a Catalog document-other object."""
import sys


def stream(data: bytes, extra: bytes = b"") -> bytes:
    return (
        b"<< "
        + extra
        + b"/Length %d >>\nstream\n" % len(data)
        + data
        + b"\nendstream"
    )


def build() -> bytes:
    # The image is reached first through page /Thumb and then independently
    # through Catalog /ZExtra. qpdf therefore records one thumbnail user and
    # one document-other user: thumbs==1, others>0 => lc_other, not
    # lc_thumbnail_private (QPDF_linearization.cc:1128-1137).
    objects = {
        1: b"<< /Type /Catalog /Pages 2 0 R /ZExtra 5 0 R >>",
        2: b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>",
        3: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Thumb 5 0 R >>",
        4: stream(b"BT (Page0) Tj ET"),
        5: stream(
            b"\x00",
            b"/Type /XObject /Subtype /Image /Width 1 /Height 1 "
            b"/ColorSpace /DeviceGray /BitsPerComponent 8 ",
        ),
    }

    output = bytearray(b"%PDF-1.5\n%\xe2\xe3\xcf\xd3\n")
    offsets: dict[int, int] = {}
    for number in sorted(objects):
        offsets[number] = len(output)
        output += b"%d 0 obj\n" % number + objects[number] + b"\nendobj\n"
    xref = len(output)
    size = max(objects) + 1
    output += b"xref\n0 %d\n0000000000 65535 f \n" % size
    for number in range(1, size):
        output += b"%010d 00000 n \n" % offsets[number]
    output += b"trailer\n<< /Size %d /Root 1 0 R >>\n" % size
    output += b"startxref\n%d\n%%%%EOF\n" % xref
    return bytes(output)


if __name__ == "__main__":
    sys.stdout.buffer.write(build())
