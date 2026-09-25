#!/usr/bin/env python3
"""Generate small, license-safe graph-preservation fixtures.

These PDFs carry the PDF/A-related dictionary and stream shapes exercised by
the preservation tests. They are not used to validate PDF/A conformance.
"""

from pathlib import Path
import sys
import zlib


def make_fixture(part: int, version: str) -> bytes:
    xmp = (
        b'<?xpacket begin="\xef\xbb\xbf"?><x:xmpmeta xmlns:x="adobe:ns:meta/">'
        b'<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">'
        b'<rdf:Description rdf:about="" xmlns:pdfaid="http://www.aiim.org/pdfa/ns/id/" '
        + f'pdfaid:part="{part}" pdfaid:conformance="B"/>'.encode()
        + b'</rdf:RDF></x:xmpmeta><?xpacket end="w"?>'
    )
    compressed_xmp = zlib.compress(xmp)
    icc = b'SYNTHETIC-ICC-PROFILE-NOT-FOR-CONFORMANCE-VALIDATION-' + bytes([part]) * 64
    content = b'q Q\n'

    objects = [
        b'<< /Type /Catalog /Pages 2 0 R /Metadata 5 0 R /OutputIntents [6 0 R] '
        + b'/MarkInfo << /Marked true /Suspects false >> /StructTreeRoot 8 0 R /Lang (en-US)'
        + (b' /AF [11 0 R]' if part == 3 else b'')
        + b' >>',
        b'<< /Type /Pages /Count 1 /Kids [3 0 R] >>',
        b'<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>',
        b'<< /Length ' + str(len(content)).encode() + b' >>\nstream\n' + content + b'endstream',
        b'<< /Type /Metadata /Subtype /XML /Filter /FlateDecode /Length '
        + str(len(compressed_xmp)).encode()
        + b' >>\nstream\n'
        + compressed_xmp
        + b'\nendstream',
        b'<< /Type /OutputIntent /S /GTS_PDFA1 /OutputConditionIdentifier '
        + b'(Synthetic sRGB) /DestOutputProfile 7 0 R >>',
        b'<< /N 3 /Length ' + str(len(icc)).encode() + b' >>\nstream\n' + icc + b'\nendstream',
        b'<< /Type /StructTreeRoot /K 9 0 R /ParentTree 10 0 R >>',
        b'<< /Type /StructElem /S /Document /P 8 0 R /Pg 3 0 R /K 0 >>',
        b'<< /Nums [0 [9 0 R]] >>',
    ]
    if part == 3:
        embedded = b'PDF/A-3 associated file payload\n'
        objects.extend(
            [
                b'<< /Type /Filespec /F (attachment.txt) /UF (attachment.txt) '
                + b'/AFRelationship /Data /EF << /F 12 0 R >> >>',
                b'<< /Type /EmbeddedFile /Subtype /text#2Fplain /Length '
                + str(len(embedded)).encode()
                + b' >>\nstream\n'
                + embedded
                + b'endstream',
            ]
        )

    pdf = bytearray(f'%PDF-{version}\n'.encode())
    offsets = []
    for number, body in enumerate(objects, start=1):
        offsets.append(len(pdf))
        pdf.extend(f'{number} 0 obj\n'.encode())
        pdf.extend(body)
        pdf.extend(b'\nendobj\n')
    xref_offset = len(pdf)
    pdf.extend(f'xref\n0 {len(objects) + 1}\n'.encode())
    pdf.extend(b'0000000000 65535 f \n')
    for offset in offsets:
        pdf.extend(f'{offset:010d} 00000 n \n'.encode())
    pdf.extend(
        f'trailer\n<< /Size {len(objects) + 1} /Root 1 0 R >>\n'
        f'startxref\n{xref_offset}\n%%EOF\n'.encode()
    )
    return bytes(pdf)


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit(f"usage: {sys.argv[0]} FIXTURE_DIRECTORY")
    fixture_directory = Path(sys.argv[1])
    for part, version in ((1, "1.4"), (2, "1.7"), (3, "1.7")):
        path = fixture_directory / f"pdfa-{part}b.pdf"
        if path.exists():
            print(f"Skipping {path.name} (already exists)")
            continue
        path.write_bytes(make_fixture(part, version))
        print(f"Generated {path.name}")


if __name__ == "__main__":
    main()
