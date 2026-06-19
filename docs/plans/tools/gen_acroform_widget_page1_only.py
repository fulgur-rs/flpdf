#!/usr/bin/env python3
"""Generate a 2-page PDF with an AcroForm widget ONLY on page 1 (not page 0).

This fixture exercises the part7 pre-pass OD guard (r3443001374).

The widget (6) is in open_document_set via AcroForm /Fields, and also in
page 1's /Annots. So page_reach[widget] == 1 (page 1 only). Without the fix,
the part7 pre-pass places widget in part4_other_pages_private, bypassing OD
routing. With the fix, it flows through OD routing and lands in part4_rest.

Object layout:
  1 = Catalog  (/AcroForm 5, /Pages 2)
  2 = Pages    (/Kids [3, 4])
  3 = Page 0   (no widget)
  4 = Page 1   (/Annots [6])
  5 = AcroForm (/Fields [6])
  6 = Widget   (/T (F1)) -- eligible for ObjStm, in open_document_set
  7 = Font     (shared by both pages -- Part 3)
  8 = Contents0
  9 = Contents1
"""

catalog = 1
pages = 2
page0 = 3
page1 = 4
acroform = 5
widget = 6
font = 7
contents0 = 8
contents1 = 9

objs = {}

objs[catalog] = (
    b"<< /Type /Catalog /AcroForm %d 0 R /Pages %d 0 R >>" % (acroform, pages)
)
objs[pages] = (
    b"<< /Type /Pages /Count 2 /Kids [ %d 0 R %d 0 R ] >>" % (page0, page1)
)

res = b"<< /Font << /F1 %d 0 R >> >>" % font
objs[page0] = (
    b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792]"
    b" /Resources %s /Contents %d 0 R >>"
    % (pages, res, contents0)
)
objs[page1] = (
    b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792]"
    b" /Resources %s /Annots [ %d 0 R ] /Contents %d 0 R >>"
    % (pages, res, widget, contents1)
)

objs[acroform] = b"<< /Fields [ %d 0 R ] >>" % widget
objs[widget] = (
    b"<< /Type /Annot /Subtype /Widget /Rect [50 700 200 720]"
    b" /FT /Tx /T (F1) /V () >>"
)

objs[font] = b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"

s0 = b"BT /F1 12 Tf 72 720 Td (Page0) Tj ET"
objs[contents0] = b"<< /Length %d >>\nstream\n%s\nendstream" % (len(s0), s0)

s1 = b"BT /F1 12 Tf 72 720 Td (Page1) Tj ET"
objs[contents1] = b"<< /Length %d >>\nstream\n%s\nendstream" % (len(s1), s1)

import sys

out = bytearray(b"%PDF-1.5\n%\xe2\xe3\xcf\xd3\n")
offsets = {}
for num in sorted(objs):
    offsets[num] = len(out)
    out += b"%d 0 obj\n" % num + objs[num] + b"\nendobj\n"

xref_start = len(out)
total = max(objs) + 1
out += b"xref\n0 %d\n0000000000 65535 f \n" % total
for num in range(1, total):
    out += b"%010d 00000 n \n" % offsets[num]
out += b"trailer\n<< /Size %d /Root %d 0 R >>\n" % (total, catalog)
out += b"startxref\n%d\n%%%%EOF\n" % xref_start

sys.stdout.buffer.write(out)
