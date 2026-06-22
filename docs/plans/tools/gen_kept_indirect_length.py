#!/usr/bin/env python3
"""Generate a 1-page PDF whose image XObject has a KEPT indirect /Length holder.

The image XObject (obj 5) declares /Filter /DCTDecode — an image codec flpdf
cannot decode, so the stream is passed through verbatim (a decode-failure /
passthrough stream) — and carries `/Length 6 0 R`. The holder (obj 6, an
integer) is referenced BOTH by that /Length edge AND by the catalog
(/KeepHolder 6 0 R), so it stays live; it is NOT an orphan (contrast
gen_od_indirect_length.py, where the holder is reachable only via /Length and
is garbage-collected once /Length is direct-ized).

qpdf writes every emitted stream's /Length as a direct integer, never an
indirect reference, while keeping the holder (it has another live reference).
flpdf historically left the indirect /Length on passthrough streams whose
holder is kept (flpdf-q1j2). This fixture pins flpdf's plain rewrite
byte-identical to qpdf for that kept-holder passthrough case.

Usage:
    gen_kept_indirect_length.py        # to stdout
"""
import sys

# Fake JPEG bytes (a valid SOI + APP0 prefix, then junk): not decodable, so
# flpdf's DCTDecode path fails and the stream is passed through verbatim — its
# /Length must still be direct-ized.
fake_jpeg = bytes([0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0xAA, 0xBB, 0xCC, 0xDD])
content = b"BT ET"

objs = {
    # /KeepHolder gives obj 6 a live reference independent of the image's
    # /Length, so direct-izing /Length must NOT drop it.
    1: b"<< /Type /Catalog /Pages 2 0 R /KeepHolder 6 0 R >>",
    2: b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    3: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
       b"/Contents 4 0 R /Resources << /XObject << /Im0 5 0 R >> >> >>",
    4: b"<< /Length %d >>\nstream\n%s\nendstream" % (len(content), content),
    5: b"<< /Type /XObject /Subtype /Image /Width 1 /Height 1 "
       b"/BitsPerComponent 8 /ColorSpace /DeviceRGB /Filter /DCTDecode "
       b"/Length 6 0 R >>\nstream\n%s\nendstream" % fake_jpeg,
    6: b"%d" % len(fake_jpeg),
}

out = bytearray(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n")
offsets = {}
for num in sorted(objs):
    offsets[num] = len(out)
    out += b"%d 0 obj\n" % num + objs[num] + b"\nendobj\n"
xref_start = len(out)
total = max(objs) + 1
out += b"xref\n0 %d\n0000000000 65535 f \n" % total
for num in range(1, total):
    out += b"%010d 00000 n \n" % offsets[num]
out += b"trailer\n<< /Size %d /Root 1 0 R >>\n" % total
out += b"startxref\n%d\n%%%%EOF\n" % xref_start
sys.stdout.buffer.write(out)
