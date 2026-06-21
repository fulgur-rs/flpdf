#!/usr/bin/env python3
"""Generate a 2-page PDF whose /JS action stream is shared by /OpenAction and an
outline item's /A.

The shared stream is reachable from BOTH the catalog's /OpenAction subtree
(qpdf in_open_document) AND the catalog's /Outlines subtree (qpdf in_outlines).
qpdf's canonical classification orders in_outlines ABOVE in_open_document
(QPDF_linearization.cc:1368-1387: lc_outlines before lc_open_document), so the
shared object is categorized as an OUTLINE, not an open-document object.

Because the shared object is an Object::Stream it is ineligible for ObjStm
packing, so qpdf emits it as a PLAIN stream with a part9 (second-half) object
number — after /E, alongside the other outline objects.

This fixture discriminates flpdf's step-6b routing: the buggy order checked
open_document_set before outline membership, so the ineligible OD+outline stream
fell into part4_open_document_plain (pre-/O, first half) instead of the outline
section, diverging from qpdf.

Object layout (source numbering is arbitrary; qpdf renumbers on linearize):
  1  = Catalog   (/OpenAction 5 0 R, /Outlines 7 0 R, /Pages 2 0 R)
  2  = Pages     (/Kids [3 0 R 4 0 R], /Count 2)
  3  = Page 0    (/Contents 10 0 R, shared font 12)
  4  = Page 1    (/Contents 11 0 R, shared font 12)
  5  = OD action (/S /JavaScript /JS 6 0 R)        -> in_open_document, eligible
  6  = JS stream (shared by 5 and 9)               -> in_outlines (wins), STREAM
  7  = Outlines  (/First 8 0 R /Last 8 0 R)        -> in_outlines, eligible
  8  = Item      (/Parent 7 0 R /A 9 0 R)          -> in_outlines, eligible
  9  = OL action (/S /JavaScript /JS 6 0 R)        -> in_outlines, eligible
  10 = Contents page 0 (stream)                    -> in_first_page private (part2)
  11 = Contents page 1 (stream)                    -> other-page private (part7)
  12 = Font shared by both pages                   -> first-page shared (part3)

The intersection open_document_set ∩ outlines_set is exactly {6}: object 6 is the
only OD+outline object, isolating the ineligible-stream routing under test. Two
pages share font 12 so a first-page (part6/part3) ObjStm container coexists.
"""

import sys

# /PageMode /UseOutlines routes the outline objects (and the ineligible OD+outline
# stream) into the first-page section (qpdf part6 / lc_outlines), i.e. BEFORE /E,
# instead of part9 (after /E). Mirrors gen_outlines_gap.py's --use-outlines flag.
use_outlines = "--use-outlines" in sys.argv[1:]

catalog = 1
pages = 2
page0 = 3
page1 = 4
od_action = 5
js_stream = 6
outlines = 7
item = 8
ol_action = 9
contents0 = 10
contents1 = 11
font = 12

objs = {}

page_mode = b" /PageMode /UseOutlines" if use_outlines else b""
objs[catalog] = (
    b"<< /Type /Catalog%s /OpenAction %d 0 R /Outlines %d 0 R /Pages %d 0 R >>"
    % (page_mode, od_action, outlines, pages)
)
objs[pages] = (
    b"<< /Type /Pages /Count 2 /Kids [ %d 0 R %d 0 R ] >>" % (page0, page1)
)

res = b"<< /Font << /F1 %d 0 R >> >>" % font
objs[page0] = (
    b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792]"
    b" /Resources %s /Contents %d 0 R >>" % (pages, res, contents0)
)
objs[page1] = (
    b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792]"
    b" /Resources %s /Contents %d 0 R >>" % (pages, res, contents1)
)

# Open-document action: reachable only from catalog /OpenAction (a dict, so
# eligible for ObjStm). Its /JS points at the shared stream.
objs[od_action] = b"<< /S /JavaScript /JS %d 0 R >>" % js_stream

# Shared JavaScript stream: reachable from BOTH /OpenAction (via 5) and
# /Outlines (via 7 -> 8 -> 9). A stream, hence ineligible for ObjStm.
js_code = b"app.alert('shared');"
objs[js_stream] = (
    b"<< /Length %d >>\nstream\n%s\nendstream" % (len(js_code), js_code)
)

# Outline subtree: reachable only from catalog /Outlines (all dicts, eligible).
objs[outlines] = (
    b"<< /Type /Outlines /First %d 0 R /Last %d 0 R /Count 1 >>" % (item, item)
)
objs[item] = (
    b"<< /Title (Item) /Parent %d 0 R /A %d 0 R >>" % (outlines, ol_action)
)
objs[ol_action] = b"<< /S /JavaScript /JS %d 0 R >>" % js_stream

s0 = b"BT /F1 12 Tf 72 720 Td (Page0) Tj ET"
objs[contents0] = b"<< /Length %d >>\nstream\n%s\nendstream" % (len(s0), s0)

s1 = b"BT /F1 12 Tf 72 720 Td (Page1) Tj ET"
objs[contents1] = b"<< /Length %d >>\nstream\n%s\nendstream" % (len(s1), s1)

objs[font] = b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"

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
