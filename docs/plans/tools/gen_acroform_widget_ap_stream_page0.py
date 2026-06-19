#!/usr/bin/env python3
"""Generate a 2-page PDF with an AcroForm widget that has an /AP /N appearance stream.

The widget's /AP /N is a Form XObject (Object::Stream), which is ineligible for
ObjStm packing.  This exercises the `part4_open_document_plain` path: qpdf emits
the Form XObject as a plain indirect object between the Catalog and the OD ObjStm
containers (pre-/O region), not inside the ObjStm.

Object layout:
  1 = Catalog  (/AcroForm 5 0 R, /Pages 2 0 R)
  2 = Pages    (/Kids [3 0 R 4 0 R])
  3 = Page 0   (/Annots [6 0 R])
  4 = Page 1
  5 = AcroForm (/Fields [6 0 R])
  6 = Widget   (/T (F1), /AP 7 0 R)
  7 = AP dict  (/N 8 0 R)
  8 = Form XObject (appearance stream, Object::Stream → ineligible for ObjStm)
  9 = Content stream for page 0
 10 = Content stream for page 1
 11 = Font shared by both pages (Part 3)
"""

catalog = 1
pages = 2
page0 = 3
page1 = 4
acroform = 5
widget = 6
ap_dict = 7
ap_stream = 8
contents0 = 9
contents1 = 10
font = 11

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
    b" /Resources %s /Annots [ %d 0 R ] /Contents %d 0 R >>"
    % (pages, res, widget, contents0)
)
objs[page1] = (
    b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792]"
    b" /Resources %s /Contents %d 0 R >>"
    % (pages, res, contents1)
)

objs[acroform] = b"<< /Fields [ %d 0 R ] >>" % widget

objs[widget] = (
    b"<< /Type /Annot /Subtype /Widget /Rect [50 700 200 720]"
    b" /FT /Tx /T (F1) /V () /AP %d 0 R >>" % ap_dict
)

objs[ap_dict] = b"<< /N %d 0 R >>" % ap_stream

# Form XObject: this is an Object::Stream, so is_eligible_for_objstm returns false.
ap_content = b"q Q"
objs[ap_stream] = (
    b"<< /Type /XObject /Subtype /Form /BBox [0 0 150 20] /Length %d >>"
    b"\nstream\n%s\nendstream" % (len(ap_content), ap_content)
)

s0 = b"BT /F1 12 Tf 72 720 Td (Page0) Tj ET"
objs[contents0] = b"<< /Length %d >>\nstream\n%s\nendstream" % (len(s0), s0)

s1 = b"BT /F1 12 Tf 72 720 Td (Page1) Tj ET"
objs[contents1] = b"<< /Length %d >>\nstream\n%s\nendstream" % (len(s1), s1)

objs[font] = b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"

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
