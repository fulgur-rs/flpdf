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
#
# Layout: one local mirror of the qpdf repository, plus a checked-out worktree
# per pinned version.
#
#   ${XDG_CACHE_HOME:-$HOME/.cache}/flpdf/qpdf.git       mirror (history, ~43 MB)
#   ${XDG_CACHE_HOME:-$HOME/.cache}/flpdf/qpdf-11.9.0    worktree ($FLPDF_QPDF_SRC
#                                                        overrides this path)
#
# Three properties follow from that split, and each one matters here:
#
#   * History is kept OUT of the tree that gets replaced. A worktree is
#     regenerable from the mirror in ~0.2 s, so re-installing never risks
#     anything irreplaceable.
#   * A worktree's `.git` is a FILE naming its mirror, where an ordinary clone
#     has a `.git` DIRECTORY. That is an unambiguous ownership signal: the
#     script replaces only worktrees of its own mirror, and refuses to touch
#     someone else's qpdf checkout handed to it via $FLPDF_QPDF_SRC.
#   * Worktrees have independent HEADs, so a future v12 tree can sit alongside
#     this one — ~61 MB and ~0.2 s each, against ~103 MB and ~10 s for a
#     separate clone — and today's line-number citations keep resolving.
#
# Moving the pin (e.g. to v12) is a three-constant edit below; a re-run fetches
# the new commit into the existing mirror and adds a second worktree.
#
# `git log`/`git blame` over libqpdf are the point of keeping full history: they
# are what tells us *why* qpdf does something. Inspect other revisions with
# commands that leave HEAD alone — `git log`, `git show v12.0.0:libqpdf/X.cc`,
# `git diff v11.9.0..v12.0.0` — or add another worktree. A `git checkout` inside
# this worktree moves it off the pin, which makes `--print-path` fail until the
# script is run again.
#
# Usage:
#   scripts/fetch-qpdf-source.sh               Install the pinned worktree.
#                                              Idempotent: a tree already at the
#                                              pinned commit short-circuits.
#   scripts/fetch-qpdf-source.sh --print-path  Print the worktree path and exit.
#                                              Never clones; exits 1 if the tree
#                                              is missing or not at the pin.
#   scripts/fetch-qpdf-source.sh --force       Re-create the worktree even when
#                                              already present.
set -euo pipefail

QPDF_VERSION="11.9.0"
QPDF_TAG="v${QPDF_VERSION}"
QPDF_COMMIT="3b97c9bd266b7c32ea36d3536e22dab77412886d"
QPDF_REPO="https://github.com/qpdf/qpdf.git"

# Any file that must exist for the tree to be usable.
SENTINEL="libqpdf/QPDFWriter.cc"

CACHE_ROOT="${XDG_CACHE_HOME:-$HOME/.cache}/flpdf"
# The mirror stays in the cache even when $FLPDF_QPDF_SRC redirects the
# worktree: it is shared by every pinned version.
MIRROR="${CACHE_ROOT}/qpdf.git"
DEST="${FLPDF_QPDF_SRC:-${CACHE_ROOT}/qpdf-${QPDF_VERSION}}"

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
# sentinel is there — so an interrupted install is never mistaken for a good
# tree. Deliberately not restricted to worktrees we own: if someone points
# $FLPDF_QPDF_SRC at their own checkout that already sits on the pinned commit,
# using it as-is is correct and costs nothing.
installed() {
  [[ -e "${DEST}/.git" && -f "${DEST}/${SENTINEL}" ]] || return 1
  [[ "$(git -C "$DEST" rev-parse HEAD 2>/dev/null)" == "$QPDF_COMMIT" ]]
}

# True only for a worktree of OUR mirror: `.git` is a file whose gitdir points
# into ${MIRROR}/worktrees/. Read as text rather than asked of git, so a mirror
# that has gone missing still leaves the worktree recognisable as ours.
owned() {
  [[ -f "${DEST}/.git" ]] || return 1
  local gitdir
  gitdir="$(head -n 1 "${DEST}/.git")"
  [[ "$gitdir" == "gitdir: ${MIRROR}/worktrees/"* ]]
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

if (( ! FORCE )) && installed; then
  echo "qpdf ${QPDF_VERSION} source already present: ${DEST}"
  exit 0
fi

# Anything at $DEST that is not our own worktree is someone else's data — most
# plausibly a personal qpdf clone handed over through $FLPDF_QPDF_SRC, complete
# with local branches and edits. Never replace it; say what to do instead.
if [[ -e "$DEST" ]] && ! owned; then
  echo "fetch-qpdf-source.sh: ${DEST} is not a worktree created by this script; refusing to replace it" >&2
  echo "                      it is not at the pinned commit ${QPDF_COMMIT} either," >&2
  echo "                      so it cannot be used as the oracle as-is" >&2
  echo "                      move it aside, or unset/redirect \$FLPDF_QPDF_SRC" >&2
  exit 1
fi

if ! command -v git >/dev/null 2>&1; then
  echo "fetch-qpdf-source.sh: git is required to clone ${QPDF_REPO}" >&2
  exit 1
fi

mkdir -p "$CACHE_ROOT"
SCRATCH="$(mktemp -d "${CACHE_ROOT}/.flpdf-qpdf-src.XXXXXX")"
trap 'rm -rf "$SCRATCH"' EXIT

if [[ -e "$MIRROR" ]]; then
  if ! git -C "$MIRROR" rev-parse --git-dir >/dev/null 2>&1; then
    echo "fetch-qpdf-source.sh: ${MIRROR} exists but is not a git repository" >&2
    echo "                      move it aside and re-run" >&2
    exit 1
  fi
else
  # Clone into scratch on the same filesystem, then rename: a mirror only ever
  # appears at $MIRROR complete.
  echo "Cloning ${QPDF_REPO} (mirror, full history)"
  git clone --quiet --mirror "$QPDF_REPO" "${SCRATCH}/qpdf.git"
  mv "${SCRATCH}/qpdf.git" "$MIRROR"
fi

have_pinned_commit() {
  git -C "$MIRROR" rev-parse --verify --quiet "${QPDF_COMMIT}^{commit}" >/dev/null
}

# An existing mirror predating a pin bump will not have the commit yet.
if ! have_pinned_commit; then
  echo "Fetching ${QPDF_REPO}"
  git -C "$MIRROR" fetch --quiet origin
fi

# The pin is the commit. If it is not upstream at all, fail hard rather than
# silently installing a different tree.
if ! have_pinned_commit; then
  echo "fetch-qpdf-source.sh: pinned commit ${QPDF_COMMIT} not present in ${QPDF_REPO}" >&2
  exit 1
fi

TAG_COMMIT="$(git -C "$MIRROR" rev-parse --verify --quiet "${QPDF_TAG}^{commit}" || true)"
if [[ "$TAG_COMMIT" != "$QPDF_COMMIT" ]]; then
  echo "fetch-qpdf-source.sh: warning: ${QPDF_TAG} no longer points at the pinned commit" >&2
  echo "                      pinned ${QPDF_COMMIT}, ${QPDF_TAG} -> ${TAG_COMMIT:-<missing>}" >&2
  echo "                      installing the pinned commit; re-verify the pin before trusting it" >&2
fi

# Drop stale bookkeeping for worktrees deleted behind git's back, then clear the
# old tree. Only ever reached for a worktree we own, and the history it was
# checked out from lives in the mirror, so nothing here is irreplaceable.
git -C "$MIRROR" worktree prune
if [[ -e "$DEST" ]]; then
  git -C "$MIRROR" worktree remove --force "$DEST" 2>/dev/null || rm -rf "$DEST"
fi

mkdir -p "$(dirname "$DEST")"
git -C "$MIRROR" -c advice.detachedHead=false \
  worktree add --quiet --detach "$DEST" "$QPDF_COMMIT"

if [[ ! -f "${DEST}/${SENTINEL}" ]]; then
  echo "fetch-qpdf-source.sh: installed tree has no ${SENTINEL}" >&2
  exit 1
fi

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
