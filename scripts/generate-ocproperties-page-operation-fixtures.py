#!/usr/bin/env python3
"""Generate license-safe optional-content rewrite and page-copy fixtures."""

from pathlib import Path
import sys


def stream(data: bytes) -> bytes:
    return b"<< /Length " + str(len(data)).encode() + b" >>\nstream\n" + data + b"endstream"


def make_pdf(objects: dict[int, bytes], *, info_object: int) -> bytes:
    object_numbers = sorted(objects)
    if object_numbers != list(range(1, max(object_numbers) + 1)):
        raise ValueError(f"object numbers must be contiguous from 1: {object_numbers}")

    pdf = bytearray(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n")
    offsets = [0]
    for object_number in object_numbers:
        offsets.append(len(pdf))
        pdf.extend(f"{object_number} 0 obj\n".encode())
        pdf.extend(objects[object_number])
        pdf.extend(b"\nendobj\n")

    xref_offset = len(pdf)
    pdf.extend(f"xref\n0 {len(objects) + 1}\n".encode())
    pdf.extend(b"0000000000 65535 f \n")
    for offset in offsets[1:]:
        pdf.extend(f"{offset:010d} 00000 n \n".encode())
    pdf.extend(
        f"trailer\n<< /Size {len(objects) + 1} /Root 1 0 R /Info {info_object} 0 R >>\n"
        f"startxref\n{xref_offset}\n%%EOF\n".encode()
    )
    return bytes(pdf)


def primary_with_used_and_unused_ocgs() -> bytes:
    content = b"/OC /Used BDC\n0 0 50 50 re f\nEMC\n"
    objects = {
        1: b"<< /Type /Catalog /Pages 2 0 R /OCProperties 3 0 R >>",
        2: b"<< /Type /Pages /Kids [4 0 R] /Count 1 >>",
        3: b"<< /OCGs [7 0 R 8 0 R] /D 5 0 R /Configs [6 0 R] >>",
        4: (
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] "
            b"/Resources << /Properties << /Used 7 0 R >> >> /Contents 9 0 R >>"
        ),
        5: (
            b"<< /BaseState /ON /Order [7 0 R [8 0 R]] "
            b"/ON [7 0 R] /OFF [8 0 R] >>"
        ),
        6: (
            b"<< /Name (Print) /BaseState /OFF /Order [8 0 R 7 0 R] "
            b"/ON [] /OFF [7 0 R 8 0 R] >>"
        ),
        7: b"<< /Type /OCG /Name (Primary used) >>",
        8: b"<< /Type /OCG /Name (Primary unused) >>",
        9: stream(content),
        10: b"<< /Producer (flpdf optional-content fixture generator) >>",
    }
    return make_pdf(objects, info_object=10)


def secondary_page_graph() -> bytes:
    content = b"/OC /OC_B BDC\n/Fm Do\nEMC\n"
    form = b"q 0 0 1 rg 0 0 25 25 re f Q\n"
    objects = {
        1: b"<< /Type /Catalog /Pages 2 0 R /OCProperties 3 0 R >>",
        2: b"<< /Type /Pages /Kids [4 0 R] /Count 1 >>",
        3: b"<< /OCGs [6 0 R] /D 7 0 R >>",
        4: (
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] "
            b"/Resources << /Properties << /OC_B 6 0 R >> /XObject << /Fm 9 0 R >> >> "
            b"/Contents 5 0 R /Annots [8 0 R] >>"
        ),
        5: stream(content),
        6: b"<< /Type /OCG /Name (Secondary layer) >>",
        7: b"<< /BaseState /ON /Order [6 0 R] >>",
        8: b"<< /Type /Annot /Subtype /Text /Rect [10 10 20 20] /OC 10 0 R >>",
        9: (
            b"<< /Type /XObject /Subtype /Form /BBox [0 0 100 100] "
            b"/Resources << >> /OC 11 0 R /Length "
            + str(len(form)).encode()
            + b" >>\nstream\n"
            + form
            + b"endstream"
        ),
        10: b"<< /Type /OCMD /OCGs [6 0 R] /VE [/And 6 0 R [/Not 6 0 R]] >>",
        11: b"<< /Type /OCMD /OCGs [6 0 R] /VE [/Not 6 0 R] >>",
        12: b"<< /Producer (flpdf optional-content fixture generator) >>",
    }
    return make_pdf(objects, info_object=12)


def main() -> None:
    if len(sys.argv) > 2:
        raise SystemExit(f"usage: {sys.argv[0]} [FIXTURE_DIRECTORY]")
    fixture_directory = (
        Path(sys.argv[1])
        if len(sys.argv) == 2
        else Path(__file__).resolve().parents[1] / "tests" / "fixtures" / "compat"
    )
    fixture_directory.mkdir(parents=True, exist_ok=True)
    fixtures = {
        "ocproperties-primary-used-unused.pdf": primary_with_used_and_unused_ocgs(),
        "ocproperties-secondary-page-graph.pdf": secondary_page_graph(),
    }
    for name, data in fixtures.items():
        path = fixture_directory / name
        path.write_bytes(data)
        print(f"Generated {path} ({len(data)} bytes)")


if __name__ == "__main__":
    main()
