#!/usr/bin/env python3
"""Generate a 1-page PDF whose page /Contents stream has an indirect /Length.

The holder (obj 5) is reachable ONLY through the content stream's /Length edge.
Unlike gen_od_indirect_length.py (where the orphan holder sits on an
open-document stream and is dropped via the Part-4 universe filter), here the
holder is PAGE-reachable: it enters the first-page closure because
compute_closure follows the stream dict's /Length. The linearization planner
must drop it from the page closures too, or it leaks into Part 2/3 and inflates
page object counts (flpdf-2vfg, Codex review on PR #400).

Usage:
    gen_page_contents_indirect_length.py            # content stream uncompressed
    gen_page_contents_indirect_length.py --flate    # content stream lone /FlateDecode
"""
import sys
import zlib

flate = "--flate" in sys.argv[1:]

content_plain = b"BT /F1 12 Tf 72 720 Td (hello) Tj ET"
if flate:
    content = zlib.compress(content_plain, 9)
    stream4 = b"<< /Length 5 0 R /Filter /FlateDecode >>\nstream\n" + content + b"\nendstream"
    holder_value = len(content)
else:
    stream4 = b"<< /Length 5 0 R >>\nstream\n" + content_plain + b"\nendstream"
    holder_value = len(content_plain)

objs = {
    1: b"<< /Type /Catalog /Pages 2 0 R >>",
    2: b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    3: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
       b"/Contents 4 0 R /Resources << >> >>",
    # obj 4: the page content stream with an INDIRECT /Length (5 0 R).
    4: stream4,
    # obj 5: the holder, reachable only through obj 4's /Length.
    5: b"%d" % holder_value,
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
