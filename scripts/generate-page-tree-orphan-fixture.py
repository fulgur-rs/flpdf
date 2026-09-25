#!/usr/bin/env python3
"""Generate a page-tree fixture with one reachable page and one body orphan."""

from pathlib import Path
import sys


def page_tree_orphan() -> bytes:
    objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 5 0 R >>",
        (
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] "
            + b"/Marker (ORPHAN-PAGE) >>"
        ),
        b"<< /Length 0 >>\nstream\n\nendstream",
    ]
    pdf = bytearray(b"%PDF-1.7\n")
    offsets = [0]
    for object_number, body in enumerate(objects, start=1):
        offsets.append(len(pdf))
        pdf.extend(f"{object_number} 0 obj\n".encode())
        pdf.extend(body)
        pdf.extend(b"\nendobj\n")

    xref_offset = len(pdf)
    pdf.extend(f"xref\n0 {len(offsets)}\n".encode())
    pdf.extend(b"0000000000 65535 f \n")
    for offset in offsets[1:]:
        pdf.extend(f"{offset:010d} 00000 n \n".encode())
    pdf.extend(
        f"trailer\n<< /Size {len(offsets)} /Root 1 0 R >>\n"
        f"startxref\n{xref_offset}\n%%EOF\n".encode()
    )
    return bytes(pdf)


def main() -> None:
    if len(sys.argv) > 2:
        raise SystemExit(f"usage: {sys.argv[0]} [FIXTURE_DIRECTORY]")
    directory = (
        Path(sys.argv[1])
        if len(sys.argv) == 2
        else Path(__file__).resolve().parents[1] / "tests" / "fixtures" / "compat"
    )
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / "page-tree-orphan.pdf"
    data = page_tree_orphan()
    path.write_bytes(data)
    print(f"Generated {path} ({len(data)} bytes)")


if __name__ == "__main__":
    main()
