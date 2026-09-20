#!/bin/bash
# E-16 probe (flpdf-3yn9.48.159): does the `should_remove_unreferenced_resources`
# mode-dispatch / verbose-frame duplication produce any output difference
# against qpdf 11.9.0?
#
# Usage: e16_probe.sh <workdir> [path-to-flpdf]
#
# The only expected stderr/stdout difference is the progname (`qpdf:` vs
# `flpdf:`), which argv[0] forces; the script normalises it away.  Any other
# reported difference is a real E-16 parity finding.
#
# Result on 2026-09-18 (qpdf 11.9.0, flpdf @ da153c06a, --features
# qpdf-zlib-compat): zero differences across every cell below.
set -eu
# -m so a workdir whose parent does not exist yet is still accepted -- the
# `mkdir -p` below creates it. Plain `realpath` fails on a missing component,
# and under `set -e` that ends the run before the directory is ever made.
P="$(realpath -m "${1:?workdir}")"
FL="${2:-flpdf}"
case "$FL" in */*) FL="$(realpath "$FL")" ;; esac
export FLPDF_STATIC_ID_QUIET=1
rm -rf "$P/fix" "$P/q" "$P/f"; mkdir -p "$P/fix" "$P/q" "$P/f"
python3 "$(dirname "$0")/make_fixtures.py" "$P/fix" >/dev/null
cp "$P/fix/e16-indirect-kids-shared.pdf" "$P/fix/a.pdf"
cp "$P/fix/e16-unshared.pdf"             "$P/fix/b.pdf"
cp "$P/fix/e16-nonleaf-resources.pdf"    "$P/fix/c has space.pdf"
cd "$P/fix" || exit 1
# Setup (fixture generation above) must not fail silently into a partial
# fixture set that then produces spurious DIFFs below -- `set -e` covers it.
# The probe cells themselves are not expected to fail here (unlike D16's),
# but turning -e off before the run loop keeps both probes' shape identical
# and avoids `set -e` masking a later `run` invocation's own exit status.
set +e
run() { tag="$1"; shift
  /usr/bin/qpdf "${@//@OUT@/$P/q/$tag.pdf}" >"$P/q/$tag.out" 2>"$P/q/$tag.err"; echo $? >"$P/q/$tag.rc"
  "$FL"         "${@//@OUT@/$P/f/$tag.pdf}" >"$P/f/$tag.out" 2>"$P/f/$tag.err"; echo $? >"$P/f/$tag.rc"
}
for mode in auto yes no; do
  C=(--static-id --verbose --remove-unreferenced-resources=$mode)
  for f in e16-direct-kids-shared e16-indirect-kids-shared e16-unshared \
           e16-nonleaf-resources e16-shared-xobject-crosskind; do
    # QPDFJob::handlePageSpecs (single source, self-selection)
    run "$f.$mode.pages" "${C[@]}" "$f.pdf" --pages . -- @OUT@
    # QPDFJob::doSplitPages
    run "$f.$mode.split" "${C[@]}" --split-pages=1 "$f.pdf" @OUT@
  done
  # multi-source handlePageSpecs: distinct sources, repeated spelling,
  # aliased spelling (two QPDF objects for one file), primary `.`, a name
  # with a space, and reversed command-line order (qpdf keys the scan map on
  # the filename, so the scan order is sorted, not argv order).
  run "m2.$mode"    "${C[@]}" --empty --pages a.pdf 1-z b.pdf 1-z -- @OUT@
  run "same.$mode"  "${C[@]}" --empty --pages a.pdf 1-z a.pdf 1-z -- @OUT@
  run "alias.$mode" "${C[@]}" --empty --pages a.pdf 1-z ./a.pdf 1-z -- @OUT@
  run "prim.$mode"  "${C[@]}" b.pdf --pages . 1-z a.pdf 1-z -- @OUT@
  run "space.$mode" "${C[@]}" --empty --pages "c has space.pdf" 1-z a.pdf 1-z -- @OUT@
  run "order.$mode" "${C[@]}" --empty --pages b.pdf 1-z a.pdf 1-z -- @OUT@
done
for x in "$P"/f/*.out "$P"/f/*.err; do sed -i "s/^flpdf:/qpdf:/; s#$P/f/#OUTDIR/#g" "$x"; done
for x in "$P"/q/*.out "$P"/q/*.err; do sed -i "s#$P/q/#OUTDIR/#g" "$x"; done
rc=0
cd "$P" || exit 1
for x in q/*; do n=$(basename "$x"); diff -q "q/$n" "f/$n" >/dev/null 2>&1 || { echo "DIFF $n"; rc=1; }; done
comm -3 <(ls q) <(ls f) | sed 's/^/ONE-SIDED /' | grep . && rc=1
[ "$rc" = 0 ] && echo "E-16: no observable difference"
exit $rc
