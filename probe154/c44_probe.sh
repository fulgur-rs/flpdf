#!/bin/bash
# C44 probe runner (flpdf-3yn9.48.154).
#
# Builds c44_probe.cc against the pinned qpdf 11.9.0 headers and the system
# libqpdf.so.29 (both 11.9.0) and prints the observations the C-U2 probe needs:
# provider call count, nullptr vs real `filtering_attempted`, filter / payload
# at two decode levels, live-source lifetime, and blob error propagation.
#
# Usage: c44_probe.sh [workdir]
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
WORK="${1:-/tmp/flpdf-c44-probe}"
mkdir -p "$WORK"

QPDF_SRC="$(cd "$ROOT" && scripts/fetch-qpdf-source.sh --print-path)"
if ! qpdf --version | grep -q '^qpdf version 11.9.0$'; then
    echo "c44_probe: qpdf --version is not 11.9.0" >&2
    qpdf --version >&2
    exit 1
fi

LIB=""
for candidate in /usr/lib/x86_64-linux-gnu/libqpdf.so.29 \
    /usr/lib/libqpdf.so.29 /usr/local/lib/libqpdf.so.29; do
    if [ -f "$candidate" ]; then
        LIB="$candidate"
        break
    fi
done
if [ -z "$LIB" ]; then
    echo "c44_probe: libqpdf.so.29 not found" >&2
    exit 1
fi

g++ -std=c++17 -I"$QPDF_SRC/include" "$HERE/c44_probe.cc" "$LIB" -o "$WORK/c44_probe"
"$WORK/c44_probe"
