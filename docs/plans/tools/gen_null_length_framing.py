#!/usr/bin/env python3
"""Generate malformed-length streams with LF, CR, or CRLF framing."""

import pathlib
import sys
import zlib


def indirect(number: int, body: bytes) -> bytes:
    return b"%d 0 obj\n" % number + body + b"\nendobj\n"


def stream(length_entry: bytes, payload: bytes, eol: bytes) -> bytes:
    return b"<< " + length_entry + b" >>\nstream\n" + payload + eol + b"endstream"


def fixture_objects() -> dict[int, bytes]:
    content = b"BT ET"
    objects: dict[int, bytes] = {
        1: b"<< /Type /Catalog /Pages 2 0 R >>",
        2: b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        3: (
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
            b"/Contents 4 0 R /Resources << /XObject << "
            b"/MLF 5 0 R /MCR 6 0 R /MCRLF 7 0 R "
            b"/VLF 8 0 R /VCR 9 0 R /VCRLF 10 0 R "
            b"/DLF 11 0 R /DCR 12 0 R /DCRLF 13 0 R "
            b"/ILF 14 0 R /ICR 15 0 R /ICRLF 16 0 R >> >> >>"
        ),
        4: stream(b"/Length " + str(len(content)).encode(), content, b"\n"),
        5: stream(b"", b"missing-lf", b"\n"),
        6: stream(b"", b"missing-cr", b"\r"),
        7: stream(b"", b"missing-crlf", b"\r\n"),
        8: stream(b"/Length /Bad", b"invalid-lf", b"\n"),
        9: stream(b"/Length /Bad", b"invalid-cr", b"\r"),
        10: stream(b"/Length /Bad", b"invalid-crlf", b"\r\n"),
        11: stream(b"/Length null", b"direct-null-lf", b"\n"),
        12: stream(b"/Length null", b"direct-null-cr", b"\r"),
        13: stream(b"/Length null", b"direct-null-crlf", b"\r\n"),
        14: stream(b"/Length 17 0 R", b"indirect-null-lf", b"\n"),
        15: stream(b"/Length 18 0 R", b"indirect-null-cr", b"\r"),
        16: stream(b"/Length 19 0 R", b"indirect-null-crlf", b"\r\n"),
        17: b"null",
        18: b"null",
        19: b"null",
    }
    return objects


def write_classic(path: str, objects: dict[int, bytes]) -> None:
    output = bytearray(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n")
    offsets = {}
    for number in sorted(objects):
        offsets[number] = len(output)
        output += indirect(number, objects[number])

    size = max(objects) + 1
    xref_offset = len(output)
    output += b"xref\n0 %d\n0000000000 65535 f \n" % size
    for number in range(1, size):
        output += b"%010d 00000 n \n" % offsets[number]
    output += b"trailer\n<< /Size %d /Root 1 0 R >>\n" % size
    output += b"startxref\n%d\n%%%%EOF\n" % xref_offset
    pathlib.Path(path).write_bytes(output)


def append_xref_entry(entries: bytearray, entry_type: int, field1: int, field2: int) -> None:
    entries.append(entry_type)
    entries.extend(field1.to_bytes(4, "big"))
    entries.extend(field2.to_bytes(2, "big"))


def write_objstm(path: str, objects: dict[int, bytes]) -> None:
    member_numbers = [1, 2, 3, 17, 18, 19]
    pair_table = b""
    member_body = b""
    for index, number in enumerate(member_numbers):
        pair_table += b"%d %d " % (number, len(member_body))
        member_body += objects[number]
        if index + 1 < len(member_numbers):
            member_body += b"\n"
    objstm_data = zlib.compress(pair_table + member_body)

    container_number = 20
    xref_number = 21
    size = 22
    output = bytearray(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n")
    offsets = {}
    for number in range(4, 17):
        offsets[number] = len(output)
        output += indirect(number, objects[number])
    offsets[container_number] = len(output)
    output += (
        b"20 0 obj\n<< /Type /ObjStm /N 6 /First %d /Length %d "
        b"/Filter /FlateDecode >>\nstream\n" % (len(pair_table), len(objstm_data))
    )
    output += objstm_data + b"\nendstream\nendobj\n"
    offsets[xref_number] = len(output)

    member_indices = {number: index for index, number in enumerate(member_numbers)}
    xref_entries = bytearray()
    for number in range(size):
        if number == 0:
            append_xref_entry(xref_entries, 0, 0, 65535)
        elif number in member_indices:
            append_xref_entry(xref_entries, 2, container_number, member_indices[number])
        elif number in offsets:
            append_xref_entry(xref_entries, 1, offsets[number], 0)
        else:
            append_xref_entry(xref_entries, 0, 0, 0)
    xref_data = zlib.compress(bytes(xref_entries))
    output += (
        b"21 0 obj\n<< /Type /XRef /W [1 4 2] /Index [0 22] /Size 22 "
        b"/Root 1 0 R /Length %d /Filter /FlateDecode >>\nstream\n" % len(xref_data)
    )
    output += xref_data + b"\nendstream\nendobj\n"
    output += b"startxref\n%d\n%%%%EOF\n" % offsets[xref_number]
    pathlib.Path(path).write_bytes(output)


def main() -> None:
    if len(sys.argv) not in (2, 3):
        raise SystemExit(
            "usage: gen_null_length_framing.py CLASSIC.pdf [OBJSTM.pdf]"
        )
    objects = fixture_objects()
    write_classic(sys.argv[1], objects)
    if len(sys.argv) == 3:
        write_objstm(sys.argv[2], objects)


if __name__ == "__main__":
    main()
