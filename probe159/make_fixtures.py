#!/usr/bin/env python3
"""Build the fixtures used by the flpdf-3yn9.48.159 D16 / E-16 parity probe.

Each fixture is a hand-assembled classic-xref PDF so that the exact shape the
probe needs (direct /Kids entries, shared indirect /Resources, an unreferenced
/Font) survives without a writer normalising it away.
"""
import os
import sys


def build(objects, root=1, extra_trailer=""):
    """objects: list of (number, body_bytes). Returns the PDF bytes."""
    out = bytearray(b"%PDF-1.4\n")
    offsets = {}
    maxn = max(n for n, _ in objects)
    for n, body in objects:
        offsets[n] = len(out)
        out += b"%d 0 obj\n" % n + body + b"\nendobj\n"
    xref = len(out)
    out += b"xref\n0 %d\n" % (maxn + 1)
    out += b"0000000000 65535 f \n"
    for n in range(1, maxn + 1):
        if n in offsets:
            out += b"%010d 00000 n \n" % offsets[n]
        else:
            out += b"0000000000 65535 f \n"
    out += b"trailer\n<< /Size %d /Root %d 0 R %s>>\nstartxref\n%d\n%%%%EOF\n" % (
        maxn + 1, root, extra_trailer.encode(), xref)
    return bytes(out)


def stream_obj(data, extra=b""):
    return b"<< /Length %d %s>>\nstream\n" % (len(data), extra) + data + b"\nendstream"


CONTENT = b"BT /F1 12 Tf 72 720 Td (hi) Tj ET"


def direct_kids_shared_resources():
    """Two DIRECT page dicts in /Kids that share one indirect /Resources (5 0 R).

    This was written on the premise that qpdf's shouldRemoveUnreferencedResources
    keys nodes_seen on QPDFObjGen, so both direct kids would collapse to `0 0`
    and the second would be skipped, while an implementation keying on canonical
    handle identity would visit both.  That premise is wrong about qpdf:
    `QPDFObjGen::set::add` ignores attempts to insert `QPDFObjGen(0, 0)` and
    returns true (include/qpdf/QPDFObjGen.hh:108-121), so direct nodes are never
    deduplicated and qpdf visits both kids too.  Measured on qpdf 11.9.0: the
    --split-pages cells print `found shared resources in leaf node 0 0: 5 0`,
    i.e. qpdf reaches the shared /Resources through a direct node.

    What the fixture does exercise depends on the cell.  Through `--pages .`
    the CLI promotes each direct /Kids entry to its own numbered page object
    first, so the heuristic sees numbered leaves (qpdf prints `leaf node 11 0`).
    Through `--split-pages` it does not: two direct `0 0` nodes reach the
    heuristic in both tools.  Those cells do guard the distinction -- keying
    nodes_seen on the object number instead of handle identity (so the second
    `0 0` node is skipped) makes e16_probe.sh report DIFFs on
    e16-direct-kids-shared.auto.split-1.pdf and .split-2.pdf -- they just do not
    show a qpdf/flpdf divergence, because qpdf does not collapse them either.
    """
    page = (b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
            b"/Resources 5 0 R /Contents %d 0 R >>")
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Count 2 /Kids [ " + (page % 6) + b" " + (page % 7) + b" ] >>"),
        (5, b"<< /Font << /F1 8 0 R /F2 9 0 R >> >>"),
        (6, stream_obj(CONTENT)),
        (7, stream_obj(CONTENT)),
        (8, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
        (9, b"<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>"),
    ]
    return build(objects)


def indirect_kids_shared_resources():
    """Control: the same document with INDIRECT page kids.

    Both tools must agree that the shared indirect /Resources triggers pruning.
    """
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Count 2 /Kids [ 3 0 R 4 0 R ] >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
            b"/Resources 5 0 R /Contents 6 0 R >>"),
        (4, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
            b"/Resources 5 0 R /Contents 7 0 R >>"),
        (5, b"<< /Font << /F1 8 0 R /F2 9 0 R >> >>"),
        (6, stream_obj(CONTENT)),
        (7, stream_obj(CONTENT)),
        (8, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
        (9, b"<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>"),
    ]
    return build(objects)


def two_pages_unshared():
    """No sharing at all: per-page direct /Resources, one unreferenced font."""
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Count 2 /Kids [ 3 0 R 4 0 R ] >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
            b"/Resources << /Font << /F1 8 0 R /F2 9 0 R >> >> /Contents 6 0 R >>"),
        (4, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
            b"/Resources << /Font << /F1 8 0 R >> >> /Contents 7 0 R >>"),
        (6, stream_obj(CONTENT)),
        (7, stream_obj(CONTENT)),
        (8, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
        (9, b"<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>"),
    ]
    return build(objects)


def nonleaf_resources():
    """Inherited /Resources on the non-leaf node: both tools must return true."""
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Count 2 /Kids [ 3 0 R 4 0 R ] "
            b"/Resources << /Font << /F1 8 0 R /F2 9 0 R >> >> >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R >>"),
        (4, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 7 0 R >>"),
        (6, stream_obj(CONTENT)),
        (7, stream_obj(CONTENT)),
        (8, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
        (9, b"<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>"),
    ]
    return build(objects)


def shared_xobject_as_resources():
    """One indirect object used BOTH as page 1's /Resources and as page 2's
    /Resources /XObject value.

    qpdf feeds both the /Resources check and the /XObject check from a single
    `resources_seen` set, so the second use is a hit.  A probe of whether flpdf
    also uses one set rather than two.
    """
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Count 2 /Kids [ 3 0 R 4 0 R ] >>"),
        # page 1's /Resources IS object 5.
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
            b"/Resources 5 0 R /Contents 6 0 R >>"),
        # page 2's /Resources is direct but its /XObject value is object 5.
        (4, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
            b"/Resources << /Font << /F1 8 0 R >> /XObject 5 0 R >> /Contents 7 0 R >>"),
        (5, b"<< /Font << /F1 8 0 R /F2 9 0 R >> >>"),
        (6, stream_obj(CONTENT)),
        (7, stream_obj(CONTENT)),
        (8, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
        (9, b"<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>"),
    ]
    return build(objects)


FIXTURES = {
    "e16-direct-kids-shared.pdf": direct_kids_shared_resources,
    "e16-indirect-kids-shared.pdf": indirect_kids_shared_resources,
    "e16-unshared.pdf": two_pages_unshared,
    "e16-nonleaf-resources.pdf": nonleaf_resources,
    "e16-shared-xobject-crosskind.pdf": shared_xobject_as_resources,
}


def main():
    outdir = sys.argv[1] if len(sys.argv) > 1 else "."
    os.makedirs(outdir, exist_ok=True)
    for name, fn in FIXTURES.items():
        path = os.path.join(outdir, name)
        with open(path, "wb") as fh:
            fh.write(fn())
        print("wrote", path)


if __name__ == "__main__":
    main()
