#!/usr/bin/env python3
"""Generate minimal qpdf thumbnail-user traversal parity fixtures."""

from pathlib import Path
import argparse


def stream(data: bytes, extra: bytes = b"") -> bytes:
    return (
        b"<< "
        + extra
        + b"/Length %d >>\nstream\n" % len(data)
        + data
        + b"\nendstream"
    )


def write_pdf(path: Path, objects: dict[int, bytes]) -> None:
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
    path.write_bytes(output)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "case",
        choices=["direct-descendant", "first-edge-wins", "null-first-edge-wins"],
    )
    parser.add_argument("output", type=Path)
    args = parser.parse_args()

    image = stream(
        b"\x00",
        b"/Type /XObject /Subtype /Image /Width 1 /Height 1 "
        b"/ColorSpace /DeviceGray /BitsPerComponent 8 ",
    )
    content0 = stream(b"BT (Page0) Tj ET")

    if args.case == "direct-descendant":
        write_pdf(
            args.output,
            {
                1: b"<< /Type /Catalog /Pages 2 0 R >>",
                2: b"<< /Type /Pages /Count 2 /Kids [3 0 R 4 0 R] >>",
                3: (
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
                    b"/Resources << /XObject << /Im0 5 0 R >> >> "
                    b"/Contents 6 0 R >>"
                ),
                4: (
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
                    b"/Contents 7 0 R /Thumb << /Image 5 0 R >> >>"
                ),
                5: image,
                6: content0,
                7: stream(b"BT (Page1) Tj ET"),
            },
        )
    elif args.case == "first-edge-wins":
        write_pdf(
            args.output,
            {
                1: b"<< /Type /Catalog /Pages 2 0 R >>",
                2: b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>",
                3: (
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
                    b"/Contents 4 0 R /Thumb 5 0 R /Zzz 5 0 R >>"
                ),
                4: content0,
                5: image,
            },
        )
    else:
        # `/Thumb` sorts before `/Zzz`, so qpdf assigns object 5 exclusively to
        # the thumbnail user even though the same REAL-null identity is reached
        # again from the ordinary page user. The arrays keep the indirect null
        # identity visible; shared `visited` provides first-edge-wins.
        write_pdf(
            args.output,
            {
                1: b"<< /Type /Catalog /Pages 2 0 R >>",
                2: b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>",
                3: (
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
                    b"/Contents 4 0 R /Thumb [5 0 R] /Zzz [5 0 R] >>"
                ),
                4: content0,
                5: b"null",
            },
        )


if __name__ == "__main__":
    main()
