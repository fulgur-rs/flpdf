#!/usr/bin/env bash
# Regenerate the PCLm writer fixtures and their qpdf 11.9.0 goldens.
#
# qpdf's CLI has no --pclm flag, so the goldens come from a small C++ oracle
# that drives QPDFWriter::setPCLm directly. The oracle links against the
# system libqpdf 11.9.0 shared object and compiles against the pinned headers
# from scripts/fetch-qpdf-source.sh, so its output is qpdf 11.9.0's output:
# running the oracle on qpdf's own qtest/qpdf/pclm-in.pdf reproduces
# qtest/qpdf/pclm-out.pdf byte for byte.
#
# Every golden uses setStaticID(true), matching PdfWriter::set_static_id.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

source_dir="$("$here/../../../scripts/fetch-qpdf-source.sh" --print-path)"

cat > "$workdir/oracle.cc" <<'CC'
#include <qpdf/QPDF.hh>
#include <qpdf/QPDFWriter.hh>
#include <cstring>
#include <iostream>

int main(int argc, char** argv)
{
    if (argc < 3) {
        std::cerr << "usage: oracle in.pdf out.pdf [pclm|qdf|objstm]...\n";
        return 2;
    }
    QPDF pdf;
    pdf.processFile(argv[1]);
    QPDFWriter w(pdf, argv[2]);
    w.setStaticID(true);
    for (int i = 3; i < argc; ++i) {
        if (!strcmp(argv[i], "pclm")) {
            w.setPCLm(true);
        } else if (!strcmp(argv[i], "qdf")) {
            w.setQDFMode(true);
        } else if (!strcmp(argv[i], "objstm")) {
            w.setObjectStreamMode(qpdf_o_generate);
        } else {
            std::cerr << "unknown flag " << argv[i] << "\n";
            return 2;
        }
    }
    w.write();
    return 0;
}
CC

g++ -std=c++17 -DPOINTERHOLDER_TRANSITION=4 -I"$source_dir/include" \
    "$workdir/oracle.cc" /usr/lib/x86_64-linux-gnu/libqpdf.so.29 \
    -o "$workdir/oracle"

# Self-check: the oracle must reproduce qpdf's own checked-in PCLm golden.
"$workdir/oracle" "$source_dir/qpdf/qtest/qpdf/pclm-in.pdf" "$workdir/selfcheck.pdf" pclm
cmp "$workdir/selfcheck.pdf" "$source_dir/qpdf/qtest/qpdf/pclm-out.pdf"

python3 "$here/make_input.py" "$here"

for variant in "mini-pclm-out.pdf:pclm" \
               "mini-pclm-qdf.pdf:pclm qdf" \
               "mini-pclm-objstm.pdf:pclm objstm"; do
    name="${variant%%:*}"
    flags="${variant#*:}"
    # shellcheck disable=SC2086
    "$workdir/oracle" "$here/mini-pclm-in.pdf" "$here/$name" $flags
done

"$workdir/oracle" "$here/mini-pclm-direct-root-in.pdf" \
    "$here/mini-pclm-direct-root-out.pdf" pclm

"$workdir/oracle" "$here/mini-pclm-ext-indirect-in.pdf" \
    "$here/mini-pclm-ext-indirect-objstm.pdf" pclm objstm

# Two shapes of a /Kids leaf that is not a dictionary. QPDF::getAllPagesInternal
# dispatches on kid.hasKey("/Kids"), not on /Type, so qpdf keeps the integer in
# the page list and enqueueObjectsPCLm numbers it as the first PCLm object. The
# -kid- input puts the integer directly in /Kids (qpdf promotes it to an
# indirect page object); the -page- input puts it in the object /Kids already
# points at, so the leaf is indirect from the start.
"$workdir/oracle" "$here/mini-pclm-nondict-kid-in.pdf" \
    "$here/mini-pclm-nondict-kid-out.pdf" pclm

"$workdir/oracle" "$here/mini-pclm-nondict-page-in.pdf" \
    "$here/mini-pclm-nondict-page-out.pdf" pclm

# A dictionary labeled /Page that also has /Kids is a /Pages subtree to qpdf.
"$workdir/oracle" "$here/mini-pclm-type-page-kids-in.pdf" \
    "$here/mini-pclm-type-page-kids-out.pdf" pclm
