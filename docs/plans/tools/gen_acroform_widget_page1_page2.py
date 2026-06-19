#!/usr/bin/env python3
"""Generate a 3-page PDF with an AcroForm widget shared by pages 1 AND 2.

This fixture exercises the part8_container_nums OD filter (r3443001371).

The widget (6) is in open_document_set via AcroForm /Fields, and in BOTH
page 1's and page 2's /Annots. So page_reach[widget] == 2.

OD routing sends widget to part4_rest (eligible for ObjStm packing). The
even-split packs widget into an OD ObjStm. That ObjStm's container_pages
spans {1, 2} (from all_referenced_pages[widget]), so part8_container_nums
includes it. Without the fix, canonical_shared_hints appends this OD container
as a Part-8 SOHT entry. With the fix, the open_document_container_nums filter
skips it.

Object layout:
  1 = Catalog  (/AcroForm 5, /Pages 2)
  2 = Pages    (/Kids [3, 4, 10])
  3 = Page 0   (no widget)
  4 = Page 1   (/Annots [6])
  5 = AcroForm (/Fields [6])
  6 = Widget   (/T (F1)) -- eligible, in open_document_set, reach=2
  7 = Font     (shared by all pages -- Part 3)
  8 = Contents0
  9 = Contents1
 10 = Page 2   (/Annots [6])  -- same widget
 11 = Contents2
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
page2 = 10
contents2 = 11

objs = {}

objs[catalog] = (
    b"<< /Type /Catalog /AcroForm %d 0 R /Pages %d 0 R >>" % (acroform, pages)
)
objs[pages] = (
    b"<< /Type /Pages /Count 3 /Kids [ %d 0 R %d 0 R %d 0 R ] >>"
    % (page0, page1, page2)
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
objs[page2] = (
    b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792]"
    b" /Resources %s /Annots [ %d 0 R ] /Contents %d 0 R >>"
    % (pages, res, widget, contents2)
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

s2 = b"BT /F1 12 Tf 72 720 Td (Page2) Tj ET"
objs[contents2] = b"<< /Length %d >>\nstream\n%s\nendstream" % (len(s2), s2)

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
