#!/usr/bin/env bash
# Regenerate the qpdf-test-compare end-to-end fixture set from flpdf-authored
# sources. All output PDFs in this directory are flpdf-authored derivative
# works and inherit the flpdf repository's Apache-2.0/MIT license. NO file
# in this directory is copied from qpdf-qtest/vendor/qpdf-qtest/ (Artistic
# 2.0). See ../encrypted/README.md for the same disclaimer applied to the
# encrypted fixture set.
#
# Usage:
#   bash tests/fixtures/compare_for_test/generate.sh            # regenerate
#   bash tests/fixtures/compare_for_test/generate.sh --check    # verify only
#
# Dependencies: python3 (stdlib zlib) and qpdf (for the --check verification
# step; not strictly required for regeneration).

set -euo pipefail

usage() {
    printf 'Usage: %s [--generate|--check]\n' "$0" >&2
}

mode=${1:---generate}
case "$mode" in
    --generate | --check) ;;
    *) usage; exit 2 ;;
esac

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cd "$script_dir"

need_python3() {
    if ! command -v python3 >/dev/null 2>&1; then
        printf 'python3 is required to generate compare_for_test fixtures\n' >&2
        exit 1
    fi
}

need_qpdf() {
    if ! command -v qpdf >/dev/null 2>&1; then
        printf 'qpdf is required to --check compare_for_test fixtures\n' >&2
        exit 1
    fi
}

generate_all() {
    # A single Python invocation writes every fixture. Keeping the layout in
    # one place makes byte offsets easier to keep consistent across the pair
    # files and lets us reuse zlib.compress for the FlateDecode fixture.
    python3 - "$script_dir" <<'PYEOF'
import os
import sys
import zlib

out_dir = sys.argv[1]


def write(name: str, data: bytes) -> None:
    path = os.path.join(out_dir, name)
    with open(path, "wb") as fh:
        fh.write(data)


# --------------------------------------------------------------------------
# Fixture 1: id_differs_{a,b}.pdf
#
# Purpose: exercise the compare tool's clean_trailer path that blanks the
# second half of trailer /ID before comparing. Both files share /ID[0]; only
# /ID[1] differs. After cleaning, both trailers collapse to the same bytes.
# The rest of the file — objects, xref layout, startxref — is byte-identical
# across the pair, so the per-object walk and xref-derived comparisons all
# match trivially.
#
# The two files are literally identical except for the 32-hex-char /ID[1]
# blob (same length so xref offsets and startxref stay valid on both sides).
# --------------------------------------------------------------------------

def build_id_fixture(id1_hex: bytes) -> bytes:
    assert len(id1_hex) == 32
    # Body objects — same byte layout as tests/fixtures/minimal.pdf so the
    # xref offsets 9 / 58 and startxref 110 all stay correct.
    body = (
        b"%PDF-1.7\n"
        b"1 0 obj\n"
        b"<< /Type /Catalog /Pages 2 0 R >>\n"
        b"endobj\n"
        b"2 0 obj\n"
        b"<< /Type /Pages /Count 0 /Kids [] >>\n"
        b"endobj\n"
    )
    assert len(body) == 110, f"body should be 110 bytes, got {len(body)}"
    trailer = (
        b"xref\n"
        b"0 3\n"
        b"0000000000 65535 f \n"
        b"0000000009 00000 n \n"
        b"0000000058 00000 n \n"
        b"trailer\n"
        b"<< /Size 3 /Root 1 0 R /ID [<00112233445566778899AABBCCDDEEFF> <"
        + id1_hex
        + b">] >>\n"
        b"startxref\n"
        b"110\n"
        b"%%EOF\n"
    )
    return body + trailer


write("id_differs_a.pdf", build_id_fixture(b"1" * 32))
write("id_differs_b.pdf", build_id_fixture(b"2" * 32))

# --------------------------------------------------------------------------
# Fixture 2: flate_miniz.pdf, flate_zlib.pdf
#
# Purpose: exercise the compare tool's FlateDecode branch — two content
# streams whose compressed bytes differ but whose decoded bytes are equal
# must compare as matching. Both files have identical Catalog + Pages
# objects; object 3 is an unreferenced FlateDecode stream that carries the
# variant compression.
#
# We use Python's zlib at compression levels 1 and 9 on the same source.
# Both zlib runs produce different compressed bytes AND different /Length
# values, so this single pair covers both the "/Length stripped" and the
# "/Filter /FlateDecode → decode-and-compare" branches of compare_streams.
#
# The naming "_miniz" vs "_zlib" is a nod to flpdf's default (miniz) vs
# qpdf-zlib-compat (zlib) backends — the test itself is gated on the
# qpdf-zlib-compat feature and asserts stdout == expected file bytes.
# --------------------------------------------------------------------------

FLATE_SOURCE = b"BT\n/F1 12 Tf\n72 720 Td\n(Hello, PDF!) Tj\nET\n" * 20


def build_flate_fixture(compressed: bytes) -> bytes:
    # Body: Catalog + Pages + FlateDecode stream. Same shape on both sides;
    # only the stream's /Length value and payload bytes differ across the
    # pair — those differences are exactly what the compare tool must
    # tolerate.
    prefix = (
        b"%PDF-1.7\n"
        b"1 0 obj\n"
        b"<< /Type /Catalog /Pages 2 0 R >>\n"
        b"endobj\n"
        b"2 0 obj\n"
        b"<< /Type /Pages /Count 0 /Kids [] >>\n"
        b"endobj\n"
    )
    obj1_offset = prefix.index(b"1 0 obj")
    obj2_offset = prefix.index(b"2 0 obj")
    obj3_offset = len(prefix)
    length_bytes = str(len(compressed)).encode("ascii")
    stream_object = (
        b"3 0 obj\n"
        b"<< /Filter /FlateDecode /Length "
        + length_bytes
        + b" >>\n"
        b"stream\n"
        + compressed
        + b"\nendstream\n"
        b"endobj\n"
    )
    body = prefix + stream_object
    xref_offset = len(body)
    xref = (
        b"xref\n"
        b"0 4\n"
        b"0000000000 65535 f \n"
        + f"{obj1_offset:010d} 00000 n \n".encode("ascii")
        + f"{obj2_offset:010d} 00000 n \n".encode("ascii")
        + f"{obj3_offset:010d} 00000 n \n".encode("ascii")
        + b"trailer\n"
        b"<< /Size 4 /Root 1 0 R >>\n"
        b"startxref\n"
        + f"{xref_offset}\n".encode("ascii")
        + b"%%EOF\n"
    )
    return body + xref


compressed_low = zlib.compress(FLATE_SOURCE, 1)
compressed_high = zlib.compress(FLATE_SOURCE, 9)
assert compressed_low != compressed_high, "premise: compressed bytes must differ"
assert zlib.decompress(compressed_low) == FLATE_SOURCE
assert zlib.decompress(compressed_high) == FLATE_SOURCE
write("flate_miniz.pdf", build_flate_fixture(compressed_low))
write("flate_zlib.pdf", build_flate_fixture(compressed_high))

# --------------------------------------------------------------------------
# Fixture 3: differ_body_{a,b}.pdf
#
# Purpose: negative control for the match path. Two PDFs identical in shape
# but with a one-byte body difference (object 2's /Count changes from 0 to
# 1). Both files share xref layout, startxref, trailer, and object 1 — so
# the trailer compare passes and the per-object walk surfaces the diff on
# obj 2 as "2 0: object contents differ".
#
# This is a byte-copy of the MINIMAL_PDF / MINIMAL_PDF_COUNT1 constants in
# crates/qpdf-test-compare/tests/orchestrator.rs, promoted to a fixture
# file so the CLI test can prove the "diff -> cat actual" branch dumps the
# actual file byte-verbatim.
# --------------------------------------------------------------------------

def build_differ_body(count: bytes) -> bytes:
    assert len(count) == 1, "single-digit /Count so xref offsets stay valid"
    return (
        b"%PDF-1.7\n"
        b"1 0 obj\n"
        b"<< /Type /Catalog /Pages 2 0 R >>\n"
        b"endobj\n"
        b"2 0 obj\n"
        b"<< /Type /Pages /Count "
        + count
        + b" /Kids [] >>\n"
        b"endobj\n"
        b"xref\n"
        b"0 3\n"
        b"0000000000 65535 f \n"
        b"0000000009 00000 n \n"
        b"0000000058 00000 n \n"
        b"trailer\n"
        b"<< /Size 3 /Root 1 0 R >>\n"
        b"startxref\n"
        b"110\n"
        b"%%EOF\n"
    )


write("differ_body_a.pdf", build_differ_body(b"0"))
write("differ_body_b.pdf", build_differ_body(b"1"))

print("generated compare_for_test fixtures in", out_dir)
PYEOF
}

check_fixture_bytes() {
    # Each pair is checked with qpdf --check to confirm the file parses. The
    # semantic pass/fail assertions (exit 0 vs exit 2) live in the Rust e2e
    # test suite, which exercises the actual qpdf-test-compare binary.
    for f in \
        id_differs_a.pdf id_differs_b.pdf \
        flate_miniz.pdf flate_zlib.pdf \
        differ_body_a.pdf differ_body_b.pdf; do
        if [[ ! -f $f ]]; then
            printf 'missing fixture: %s\n' "$f" >&2
            return 1
        fi
        qpdf --check "$f" >/dev/null 2>&1 || {
            printf '%s: qpdf --check failed\n' "$f" >&2
            return 1
        }
    done
    printf 'compare_for_test fixture bytes verified\n'
}

case "$mode" in
    --generate)
        need_python3
        generate_all
        ;;
    --check)
        need_qpdf
        check_fixture_bytes
        ;;
esac
