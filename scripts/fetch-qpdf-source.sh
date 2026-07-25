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
# Pinned to the qpdf 11.9.0 release tarball (tag v11.9.0). The SHA-256 below is
# the value upstream publishes in the release's own `qpdf-11.9.0.sha256`
# manifest (verified 2026-07-25). The release tarball is preferred over the
# GitHub tag archive precisely because it carries that published checksum.
#
# Install location (first match wins):
#   $FLPDF_QPDF_SRC
#   ${XDG_CACHE_HOME:-$HOME/.cache}/flpdf/qpdf-11.9.0
#
# A user-level cache rather than a repo path: one tree serves every clone and
# every git worktree, and ~80 MB of C++ stays out of the working tree. The
# version is part of the directory name, so a future oracle bump can coexist
# and today's line-number citations keep resolving.
#
# Usage:
#   scripts/fetch-qpdf-source.sh               Fetch, verify, extract. Idempotent:
#                                              a matching tree short-circuits.
#   scripts/fetch-qpdf-source.sh --print-path  Print the install path and exit.
#                                              Never downloads; exits 1 if the
#                                              tree is missing or incomplete.
#   scripts/fetch-qpdf-source.sh --force       Re-fetch even when already present.
set -euo pipefail

QPDF_VERSION="11.9.0"
QPDF_TAG="v${QPDF_VERSION}"
QPDF_SHA256="9f5d6335bb7292cc24a7194d281fc77be2bbf86873e8807b85aeccfbff66082f"
QPDF_URL="https://github.com/qpdf/qpdf/releases/download/${QPDF_TAG}/qpdf-${QPDF_VERSION}.tar.gz"

# Any file that must exist for the tree to be usable; also the guard against
# overwriting a directory that is not ours.
SENTINEL="libqpdf/QPDFWriter.cc"

DEST="${FLPDF_QPDF_SRC:-${XDG_CACHE_HOME:-$HOME/.cache}/flpdf/qpdf-${QPDF_VERSION}}"
STAMP="${DEST}/.flpdf-qpdf-src-sha256"

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

# sha256sum on Linux, shasum -a 256 on macOS.
sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d' ' -f1
  else
    echo "fetch-qpdf-source.sh: need sha256sum or shasum to verify the download" >&2
    exit 1
  fi
}

# A tree counts as installed only when the stamp records the pinned checksum AND
# the sentinel file is there — so a half-extracted tree is never mistaken for a
# good one.
installed() {
  [[ -f "$STAMP" && -f "${DEST}/${SENTINEL}" ]] || return 1
  [[ "$(cat "$STAMP")" == "$QPDF_SHA256" ]]
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
# of a previous run). Checked before the download so a misdirected
# FLPDF_QPDF_SRC fails fast, and again right before the move.
assert_dest_replaceable() {
  if [[ -e "$DEST" && ! -f "${DEST}/${SENTINEL}" && ! -f "$STAMP" ]]; then
    echo "fetch-qpdf-source.sh: ${DEST} exists but is not a qpdf source tree; refusing to replace it" >&2
    exit 1
  fi
}

if (( ! FORCE )) && installed; then
  echo "qpdf ${QPDF_VERSION} source already present: ${DEST}"
  exit 0
fi

assert_dest_replaceable

if ! command -v curl >/dev/null 2>&1; then
  echo "fetch-qpdf-source.sh: curl is required to download ${QPDF_URL}" >&2
  exit 1
fi

TMP="$(mktemp -d "${TMPDIR:-/tmp}/flpdf-qpdf-src.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

TARBALL="${TMP}/qpdf-${QPDF_VERSION}.tar.gz"
echo "Downloading ${QPDF_URL}"
curl -fsSL --retry 3 --retry-delay 2 -o "$TARBALL" "$QPDF_URL"

ACTUAL="$(sha256_of "$TARBALL")"
if [[ "$ACTUAL" != "$QPDF_SHA256" ]]; then
  echo "fetch-qpdf-source.sh: checksum mismatch for qpdf-${QPDF_VERSION}.tar.gz" >&2
  echo "  expected ${QPDF_SHA256}" >&2
  echo "  actual   ${ACTUAL}" >&2
  exit 1
fi
echo "SHA-256 verified: ${QPDF_SHA256}"

# Extract to a scratch tree first; the destination is only touched once the
# extraction has succeeded, and the stamp is written last.
mkdir -p "${TMP}/tree"
tar xzf "$TARBALL" -C "${TMP}/tree" --strip-components=1

if [[ ! -f "${TMP}/tree/${SENTINEL}" ]]; then
  echo "fetch-qpdf-source.sh: extracted tree has no ${SENTINEL}; refusing to install" >&2
  exit 1
fi

assert_dest_replaceable
if [[ -e "$DEST" ]]; then
  mv -f "$DEST" "${TMP}/previous"
fi

mkdir -p "$(dirname "$DEST")"
mv -f "${TMP}/tree" "$DEST"
printf '%s\n' "$QPDF_SHA256" > "$STAMP"

echo "qpdf ${QPDF_VERSION} source installed: ${DEST}"
