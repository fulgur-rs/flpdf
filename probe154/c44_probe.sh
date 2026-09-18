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

# Link the library that the validated executable actually loads. A hard-coded
# candidate list can pick an unrelated ABI-compatible libqpdf on a host with
# more than one qpdf installed, which would attribute the observations below to
# 11.9.0 even though another build produced them.
QPDF_BIN="$(command -v qpdf)"
LIB="$(ldd "$QPDF_BIN" | awk '/libqpdf\.so/ { print $3; exit }')"
if [ -z "$LIB" ] || [ ! -f "$LIB" ]; then
    echo "c44_probe: cannot resolve the libqpdf loaded by $QPDF_BIN" >&2
    ldd "$QPDF_BIN" >&2
    exit 1
fi

# libqpdf carries an SONAME, so naming the file on the link line only records a
# `DT_NEEDED: libqpdf.so.29` entry -- the loader would still search the default
# paths and could pick a different copy. Pin the directory with an RPATH and
# then confirm what the probe itself resolves before trusting its output.
g++ -std=c++17 -I"$QPDF_SRC/include" "$HERE/c44_probe.cc" "$LIB" \
    -Wl,-rpath,"$(dirname "$LIB")" -o "$WORK/c44_probe"

PROBE_LIB="$(ldd "$WORK/c44_probe" | awk '/libqpdf\.so/ { print $3; exit }')"
if [ -z "$PROBE_LIB" ] || [ "$(readlink -f "$PROBE_LIB")" != "$(readlink -f "$LIB")" ]; then
    echo "c44_probe: the probe loads $PROBE_LIB, not the validated $LIB" >&2
    ldd "$WORK/c44_probe" >&2
    exit 1
fi

"$WORK/c44_probe"
