"""Discriminating fixture for multi-container in_open_document ordering (flpdf-699x).

Two pages share S fonts (first-page-shared).  The catalog /OpenAction -> action
dict carries two reference arrays:
  /ARef -> K_a "a-OD" font dicts (obj nums A0..A0+K_a-1, visited FIRST in DFS)
  /BRef -> K_b "b-OD" font dicts (obj nums B0..B0+K_b-1, visited SECOND in DFS)

DFS visits /ARef before /BRef (A < B lexically), so the even-split produces:
  C0: [action, a_od[0..K_a-1], b_od[0..some]] — all OD, min = action.num (5)
  C1: [b_od[rest...], pages, shared]          — has OD members, min = pages.num (2)

Since pages.num (2) < action.num (5), C1.min < C0.min.
DFS order = [C0, C1] is NOT ascending by min member.
qpdf correct order: also [C0, C1] (ObjGen-ascending = DFS order = even-split order).

This fixture DISCRIMINATES between "DFS order" (correct) and "sort by min member"
hypotheses.  Object numbers are contiguous (no gaps) to avoid free-object
xref entries that would inflate the renumbered output.

Object layout (no gaps in [1..c1]):
  1=catalog, 2=pages, 3=page0, 4=page1, 5=action
  6..5+K_a    = a-OD font dicts  (visited first via /ARef)
  6+K_a..5+K_a+K_b = b-OD font dicts (visited second via /BRef)
  6+K_a+K_b..5+K_a+K_b+S = shared font dicts
  6+K_a+K_b+S, 7+K_a+K_b+S = content streams c0, c1

Usage: python3 gen_openaction_multi_od.py [S [K_a [K_b]]]
Default: S=5, K_a=50, K_b=50
"""
import sys

S = int(sys.argv[1]) if len(sys.argv) > 1 else 5
K_a = int(sys.argv[2]) if len(sys.argv) > 2 else 50
K_b = int(sys.argv[3]) if len(sys.argv) > 3 else 50

catalog, pages, page0, page1, action = 1, 2, 3, 4, 5

a0 = 6
a_nums = list(range(a0, a0 + K_a))

b0 = a0 + K_a
b_nums = list(range(b0, b0 + K_b))

shared0 = b0 + K_b
shared_nums = list(range(shared0, shared0 + S))

c0 = shared0 + S
c1 = c0 + 1

objs = {}
objs[catalog] = (
    b"<< /Type /Catalog /OpenAction %d 0 R /Pages %d 0 R >>" % (action, pages)
)
objs[pages] = (
    b"<< /Type /Pages /Count 2 /Kids [ %d 0 R %d 0 R ] >>" % (page0, page1)
)
a_refs = b" ".join(b"%d 0 R" % n for n in a_nums)
b_refs = b" ".join(b"%d 0 R" % n for n in b_nums)
# /ARef sorts before /BRef (A < B lexically), so DFS visits a_nums first.
objs[action] = b"<< /ARef [ %s ] /BRef [ %s ] /S /GoTo >>" % (a_refs, b_refs)
for i, n in enumerate(a_nums):
    objs[n] = b"<< /Type /Font /Subtype /Type1 /BaseFont /A%d /Mark %d >>" % (i + 1, n)
for i, n in enumerate(b_nums):
    objs[n] = b"<< /Type /Font /Subtype /Type1 /BaseFont /B%d /Mark %d >>" % (i + 1, n)

shared_res = b" ".join(b"/S%d %d 0 R" % (i + 1, n) for i, n in enumerate(shared_nums))
res = b"<< /Font << %s >> >>" % shared_res
objs[page0] = (
    b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792] /Resources %s"
    b" /Contents %d 0 R >>" % (pages, res, c0)
)
objs[page1] = (
    b"<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792] /Resources %s"
    b" /Contents %d 0 R >>" % (pages, res, c1)
)
for i, n in enumerate(shared_nums):
    objs[n] = b"<< /Type /Font /Subtype /Type1 /BaseFont /S%d /Mark %d >>" % (i + 1, n)
for cnum, label in ((c0, b"Page0"), (c1, b"Page1")):
    stream = b"BT /S1 12 Tf 72 720 Td (%s) Tj ET" % label
    objs[cnum] = b"<< /Length %d >>\nstream\n%s\nendstream" % (len(stream), stream)

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
