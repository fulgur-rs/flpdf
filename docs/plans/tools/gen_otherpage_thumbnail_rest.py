#!/usr/bin/env python3
"""Generate an ObjStm route fixture with one page user plus one thumbnail user."""

from pathlib import Path
import sys


def indirect(number: int, body: bytes) -> bytes:
    return f"{number} 0 obj\n".encode() + body + b"\nendobj\n"


def main() -> None:
    output = Path(sys.argv[1])
    first_font = 6
    font_count = 110
    thumbnail_font = first_font + font_count - 1
    font_entries = b" ".join(
        f"/F{i:03d} {first_font + i} 0 R".encode() for i in range(font_count)
    )

    objects = [
        indirect(1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        indirect(2, b"<< /Type /Pages /Count 3 /Kids [3 0 R 4 0 R 5 0 R] >>"),
        indirect(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
        ),
        indirect(
            4,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
            + b"/Resources << /Font << "
            + font_entries
            + b" >> >> >>",
        ),
        indirect(
            5,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
            + f"/Thumb << /Image {thumbnail_font} 0 R >> >>".encode(),
        ),
    ]
    objects.extend(
        indirect(
            first_font + i,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        )
        for i in range(font_count)
    )

    pdf = bytearray(b"%PDF-1.5\n%\xe2\xe3\xcf\xd3\n")
    offsets = [0]
    for obj in objects:
        offsets.append(len(pdf))
        pdf.extend(obj)
    xref = len(pdf)
    pdf.extend(f"xref\n0 {len(offsets)}\n".encode())
    pdf.extend(b"0000000000 65535 f \n")
    for offset in offsets[1:]:
        pdf.extend(f"{offset:010d} 00000 n \n".encode())
    pdf.extend(
        b"trailer\n"
        + f"<< /Size {len(offsets)} /Root 1 0 R >>\n".encode()
        + f"startxref\n{xref}\n%%EOF\n".encode()
    )
    output.write_bytes(pdf)


if __name__ == "__main__":
    main()
