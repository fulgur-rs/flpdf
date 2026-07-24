#!/usr/bin/env python3
"""Generate a 1-page PDF whose open-document (OD) stream has an indirect /Length.

The catalog's /OpenAction points to a JavaScript action whose /JS stream carries
`/Length <holder> 0 R`. The holder object (an integer) is reachable ONLY through
that stream's /Length edge.

When a writer normalizes the stream's /Length to a direct integer (both qpdf and
flpdf do this for every stream), the holder becomes unreferenced. qpdf drops it
via reachability GC; flpdf historically emitted it anyway (flpdf-2vfg). This
fixture pins flpdf's linearized output byte-identical to qpdf, i.e. the orphaned
holder is dropped.

Usage:
    gen_od_indirect_length.py            # JS stream is uncompressed
    gen_od_indirect_length.py --flate    # JS stream is a lone /FlateDecode (the
                                         # writer's verbatim-preserve path)
    gen_od_indirect_length.py --null-holder
                                        # the indirect /Length resolves to null;
                                        # readers recover by scanning endstream
"""
import sys
import zlib

flate = "--flate" in sys.argv[1:]
null_holder = "--null-holder" in sys.argv[1:]

js_plain = b"app.alert('hi');"
content = b"BT ET"

# obj 6: the OD stream. Its /Length is INDIRECT (7 0 R). The holder (obj 7) is
# reachable only via this /Length edge, so it orphans once /Length is direct-ized.
if flate:
    js = zlib.compress(js_plain, 9)
    stream6 = b"<< /Length 7 0 R /Filter /FlateDecode >>\nstream\n" + js + b"\nendstream"
    holder_body = b"null" if null_holder else b"%d" % len(js)
else:
    stream6 = b"<< /Length 7 0 R >>\nstream\n" + js_plain + b"\nendstream"
    holder_body = b"null" if null_holder else b"%d" % len(js_plain)

objs = {
    1: b"<< /Type /Catalog /Pages 2 0 R /OpenAction 5 0 R >>",
    2: b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    3: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
       b"/Contents 4 0 R /Resources << >> >>",
    4: b"<< /Length %d >>\nstream\n%s\nendstream" % (len(content), content),
    5: b"<< /Type /Action /S /JavaScript /JS 6 0 R >>",
    6: stream6,
    7: holder_body,
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
