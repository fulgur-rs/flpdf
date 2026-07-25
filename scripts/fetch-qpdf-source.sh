#!/usr/bin/env bash
#
# fetch-qpdf-source.sh — materialise the pinned qpdf oracle source tree.
#
# flpdf's pre-v1.0 goal is byte-identical qpdf reproduction (CLAUDE.md), so the
# design docs and module docs cite qpdf by file and line
# (`libqpdf/QPDFWriter.cc:1491`, `//! Mirrors qpdf 11.9.0 libqpdf/X.cc`).
# Those citations only mean something against one exact tree. This script
# materialises that tree at a stable path so the references stay resolvable.
#
# Pinned to qpdf 11.9.0 by COMMIT, not by tag: tags are mutable, commit SHAs are
# content-addressed. `v11.9.0` is expected to point at the pinned commit and the
# script warns if upstream ever retags.
#
# 11.9.0 is not an arbitrary choice — it is the version packaged for the Ubuntu
# used in development, i.e. the `/usr/bin/qpdf` that serves as the behavioural
# oracle. Source and binary must agree or every observed-behaviour comparison is
# unsound, so the script warns when the installed qpdf reports another version.
# Moving the pin (e.g. to v12) is a three-constant edit below; the install path
# carries the version, so older citations keep resolving against the old tree.
#
# A full clone, not a tarball or a shallow clone: `git log`/`git blame` over
# libqpdf is what tells us *why* qpdf does something, and `git log v11.9.0..v12.0.0
# -- libqpdf/` is what a future oracle bump will be planned from. The v11.9.0
# tree here is byte-identical to the release tarball (verified 2026-07-25:
# libqpdf/ and include/qpdf/ diff clean), so the extra history costs ~34 MB and
# changes nothing about the sources being cited.
#
# Inspect other revisions with commands that leave HEAD alone — `git log`,
# `git show v12.0.0:libqpdf/QPDFWriter.cc`, `git diff v11.9.0..v12.0.0`. A
# `git checkout` moves the tree off the pin, which makes `--print-path` fail
# until this script is run again.
#
# Install location (first match wins):
#   $FLPDF_QPDF_SRC
#   ${XDG_CACHE_HOME:-$HOME/.cache}/flpdf/qpdf-11.9.0
#
# A user-level cache rather than a repo path: one tree serves every clone and
# every git worktree, and ~114 MB of C++ stays out of the working tree.
#
# Usage:
#   scripts/fetch-qpdf-source.sh               Clone, verify the pin, check out.
#                                              Idempotent: a tree already at the
#                                              pinned commit short-circuits.
#   scripts/fetch-qpdf-source.sh --print-path  Print the install path and exit.
#                                              Never clones; exits 1 if the tree
#                                              is missing or not at the pin.
#   scripts/fetch-qpdf-source.sh --force       Re-clone even when already present.
set -euo pipefail

QPDF_VERSION="11.9.0"
QPDF_TAG="v${QPDF_VERSION}"
QPDF_COMMIT="3b97c9bd266b7c32ea36d3536e22dab77412886d"
QPDF_REPO="https://github.com/qpdf/qpdf.git"

# Any file that must exist for the tree to be usable; also the guard against
# overwriting a directory that is not ours.
SENTINEL="libqpdf/QPDFWriter.cc"

DEST="${FLPDF_QPDF_SRC:-${XDG_CACHE_HOME:-$HOME/.cache}/flpdf/qpdf-${QPDF_VERSION}}"

PRINT_PATH=0
FORCE=0

# Reprint this file's header comment: drop the shebang, stop at the first line
# that is not a comment, strip the leading "# ".
usage() {
  sed -e '1d' -e '/^[^#]/,$d' "$0" | sed 's|^# \{0,1\}||'
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --print-path) PRINT_PATH=1; shift ;;
    --force)      FORCE=1; shift ;;
    -h|--help)    usage; exit 0 ;;
    *) echo "fetch-qpdf-source.sh: unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

# A tree counts as installed only when git reports the pinned commit AND the
# sentinel is there — so an interrupted clone is never mistaken for a good tree.
installed() {
  [[ -d "${DEST}/.git" && -f "${DEST}/${SENTINEL}" ]] || return 1
  [[ "$(git -C "$DEST" rev-parse HEAD 2>/dev/null)" == "$QPDF_COMMIT" ]]
}

if (( PRINT_PATH )); then
  if ! installed; then
    echo "fetch-qpdf-source.sh: qpdf ${QPDF_VERSION} source not installed at ${DEST}" >&2
    echo "fetch-qpdf-source.sh: run scripts/fetch-qpdf-source.sh first" >&2
    exit 1
  fi
  printf '%s\n' "$DEST"
  exit 0
fi

# Only ever replace something we recognise as a qpdf source tree (or a leftover
# of a previous run). Checked before the clone so a misdirected FLPDF_QPDF_SRC
# fails fast, and again right before the move.
assert_dest_replaceable() {
  if [[ -e "$DEST" && ! -f "${DEST}/${SENTINEL}" && ! -d "${DEST}/.git" ]]; then
    echo "fetch-qpdf-source.sh: ${DEST} exists but is not a qpdf source tree; refusing to replace it" >&2
    exit 1
  fi
}

if (( ! FORCE )) && installed; then
  echo "qpdf ${QPDF_VERSION} source already present: ${DEST}"
  exit 0
fi

assert_dest_replaceable

if ! command -v git >/dev/null 2>&1; then
  echo "fetch-qpdf-source.sh: git is required to clone ${QPDF_REPO}" >&2
  exit 1
fi

# Scratch space next to the destination, not under $TMPDIR: same filesystem by
# construction, so the install below is a rename(2) rather than a ~114 MB
# copy+unlink that could be interrupted half-way.
mkdir -p "$(dirname "$DEST")"
TMP="$(mktemp -d "$(dirname "$DEST")/.flpdf-qpdf-src.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

echo "Cloning ${QPDF_REPO} (full history)"
git clone --quiet "$QPDF_REPO" "${TMP}/tree"

# The pin is the commit. If upstream ever rewrote history so that it is gone, we
# want a hard failure, not a silently different tree.
if ! git -C "${TMP}/tree" rev-parse --verify --quiet "${QPDF_COMMIT}^{commit}" >/dev/null; then
  echo "fetch-qpdf-source.sh: pinned commit ${QPDF_COMMIT} not present in ${QPDF_REPO}" >&2
  exit 1
fi

TAG_COMMIT="$(git -C "${TMP}/tree" rev-parse --verify --quiet "${QPDF_TAG}^{commit}" || true)"
if [[ "$TAG_COMMIT" != "$QPDF_COMMIT" ]]; then
  echo "fetch-qpdf-source.sh: warning: ${QPDF_TAG} no longer points at the pinned commit" >&2
  echo "                      pinned ${QPDF_COMMIT}, ${QPDF_TAG} -> ${TAG_COMMIT:-<missing>}" >&2
  echo "                      installing the pinned commit; re-verify the pin before trusting it" >&2
fi

git -C "${TMP}/tree" -c advice.detachedHead=false checkout --quiet "$QPDF_COMMIT"

if [[ ! -f "${TMP}/tree/${SENTINEL}" ]]; then
  echo "fetch-qpdf-source.sh: checked-out tree has no ${SENTINEL}; refusing to install" >&2
  exit 1
fi

assert_dest_replaceable
if [[ -e "$DEST" ]]; then
  mv -f "$DEST" "${TMP}/previous"
fi

mv -f "${TMP}/tree" "$DEST"

echo "qpdf ${QPDF_VERSION} source installed: ${DEST} (${QPDF_COMMIT})"

# The oracle binary and the oracle source have to be the same version, or the
# observed-behaviour comparisons the docs rely on are comparing two qpdfs.
if command -v qpdf >/dev/null 2>&1; then
  BIN_VERSION="$(qpdf --version 2>/dev/null | head -1 | awk '{print $3}')"
  if [[ "$BIN_VERSION" != "$QPDF_VERSION" ]]; then
    echo "fetch-qpdf-source.sh: warning: installed qpdf is ${BIN_VERSION:-unknown}, pinned source is ${QPDF_VERSION}" >&2
    echo "                      behavioural comparisons against \$(command -v qpdf) will not match this tree" >&2
  fi
else
  echo "fetch-qpdf-source.sh: note: no qpdf on PATH; the binary oracle is unavailable" >&2
fi
