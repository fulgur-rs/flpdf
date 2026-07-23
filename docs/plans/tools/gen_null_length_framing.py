#!/usr/bin/env python3
"""Generate streams with null /Length and LF, CR, or CRLF framing."""

import pathlib
import sys


def indirect(number: int, body: bytes) -> bytes:
    return b"%d 0 obj\n" % number + body + b"\nendobj\n"


def stream(length: bytes, payload: bytes, eol: bytes) -> bytes:
    return b"<< /Length " + length + b" >>\nstream\n" + payload + eol + b"endstream"


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit("usage: gen_null_length_framing.py OUTPUT.pdf")

    content = b"BT ET"
    objects = {
        1: b"<< /Type /Catalog /Pages 2 0 R >>",
        2: b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        3: (
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
            b"/Contents 4 0 R /Resources << /XObject << "
            b"/DLF 5 0 R /DCR 6 0 R /DCRLF 7 0 R "
            b"/ILF 8 0 R /ICR 9 0 R /ICRLF 10 0 R >> >> >>"
        ),
        4: stream(str(len(content)).encode(), content, b"\n"),
        5: stream(b"null", b"direct-lf", b"\n"),
        6: stream(b"null", b"direct-cr", b"\r"),
        7: stream(b"null", b"direct-crlf", b"\r\n"),
        8: stream(b"11 0 R", b"indirect-lf", b"\n"),
        9: stream(b"12 0 R", b"indirect-cr", b"\r"),
        10: stream(b"13 0 R", b"indirect-crlf", b"\r\n"),
        11: b"null",
        12: b"null",
        13: b"null",
    }

    output = bytearray(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n")
    offsets = {}
    for number in sorted(objects):
        offsets[number] = len(output)
        output += indirect(number, objects[number])

    xref_offset = len(output)
    output += b"xref\n0 14\n0000000000 65535 f \n"
    for number in range(1, 14):
        output += b"%010d 00000 n \n" % offsets[number]
    output += b"trailer\n<< /Size 14 /Root 1 0 R >>\n"
    output += b"startxref\n%d\n%%%%EOF\n" % xref_offset
    pathlib.Path(sys.argv[1]).write_bytes(output)


if __name__ == "__main__":
    main()
