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
#                                              already present. The only way to
#                                              discard local edits to the tree.
#
# Both of the first two forms refuse to hand back a tree whose tracked files
# have been edited, and warn when the qpdf on PATH is a different version.
# Installation takes a lock under the cache root so that concurrent runs — one
# per agent or checkout on a shared $HOME — queue instead of colliding.
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
# `git -C "$MIRROR" worktree add "$DEST"` runs as if started in the mirror, so a
# relative $FLPDF_QPDF_SRC would land the worktree inside the mirror while every
# other check here reads it relative to the caller. Anchor it once, up front.
case "$DEST" in
  /*) ;;
  *)  DEST="${PWD}/${DEST}" ;;
esac

# Serialises mirror creation and worktree replacement against a concurrent run
# sharing this cache (parallel agents, several checkouts on one machine). mkdir
# is the portable atomic test-and-set; flock is not on every platform.
LOCK="${CACHE_ROOT}/.lock"
LOCK_HELD=0
SCRATCH=""

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

# A matching HEAD is not enough: editing a tracked file leaves HEAD alone while
# changing the very bytes the citations address, so the tree would still look
# pinned while `libqpdf/X.cc:NNN` had quietly moved. Untracked files (an in-tree
# build directory, say) shift no line numbers and are ignored.
#
# Prints clean / dirty / unknown. The third state is not pedantry: with the
# mirror deleted, `git status` inside the worktree fails, and folding that into
# "clean" would let the replacement path delete a tree full of edits without
# --force. Not being able to check is a reason to stop, not to proceed.
tracked_state() {
  local out
  if out="$(git -C "$DEST" status --porcelain --untracked-files=no 2>/dev/null)"; then
    if [[ -n "$out" ]]; then printf 'dirty'; else printf 'clean'; fi
  else
    printf 'unknown'
  fi
}

refuse_unverifiable() {
  echo "fetch-qpdf-source.sh: cannot determine whether ${DEST} has local edits" >&2
  echo "                      (git could not read it — its mirror is missing or damaged)" >&2
  echo "                      refusing to replace a tree that might hold uncommitted work" >&2
  echo "                      discard it explicitly: scripts/fetch-qpdf-source.sh --force" >&2
  exit 1
}

refuse_modified() {
  echo "fetch-qpdf-source.sh: ${DEST} has local edits to tracked files:" >&2
  git -C "$DEST" status --porcelain --untracked-files=no >&2
  echo "                      it no longer matches the commit it is checked out at," >&2
  echo "                      so file/line citations against it are unreliable" >&2
  if owned; then
    echo "                      restore it:    git -C ${DEST} checkout -- ." >&2
    echo "                      or re-create:  scripts/fetch-qpdf-source.sh --force" >&2
  else
    # Never suggest discarding work in a tree we did not create. The edits are
    # most likely a deliberate experiment in someone's own qpdf checkout.
    echo "                      this tree was not created by this script, so the edits are left alone" >&2
    echo "                      commit or stash them, or point \$FLPDF_QPDF_SRC elsewhere" >&2
    echo "                      (unset to use ${CACHE_ROOT}/qpdf-${QPDF_VERSION})" >&2
  fi
  exit 1
}

# The oracle binary and the oracle source have to be the same version, or the
# observed-behaviour comparisons the docs rely on are comparing two qpdfs. This
# runs on every path, not just a fresh install: qpdf can be upgraded, or PATH
# changed, long after the tree was put in place.
warn_on_binary_drift() {
  if ! command -v qpdf >/dev/null 2>&1; then
    echo "fetch-qpdf-source.sh: note: no qpdf on PATH; the binary oracle is unavailable" >&2
    return 0
  fi
  # Capture the whole output and split it in the shell rather than piping into
  # `head`/`awk`. `qpdf --version` prints two lines, so a `| head -1` lets the
  # reader exit first and the writer take SIGPIPE: measured at status 141 on
  # roughly 1 in 100 runs with a perfectly healthy qpdf, which under `pipefail`
  # would either kill the script (`set -e`) or silently blank the version. A
  # binary that genuinely cannot run (a missing shared library, say) lands in
  # the same empty-value branch.
  local raw first bin_version
  local -a fields
  raw="$(qpdf --version 2>/dev/null)" || raw=""
  first="${raw%%$'\n'*}"
  read -r -a fields <<<"$first" || true
  bin_version="${fields[2]:-}"
  if [[ -z "$bin_version" ]]; then
    echo "fetch-qpdf-source.sh: note: qpdf on PATH did not report a version; the binary oracle is unavailable" >&2
    return 0
  fi
  if [[ "$bin_version" != "$QPDF_VERSION" ]]; then
    echo "fetch-qpdf-source.sh: warning: installed qpdf is ${bin_version:-unknown}, pinned source is ${QPDF_VERSION}" >&2
    echo "                      behavioural comparisons against \$(command -v qpdf) will not match this tree" >&2
  fi
}

# Resolve symlinks without depending on GNU realpath.
resolve_path() {
  if [[ -d "$1" ]]; then
    (cd "$1" && pwd -P) 2>/dev/null || printf '%s' "$1"
  else
    printf '%s' "$1"
  fi
}

# True only for a worktree of OUR mirror. An ordinary clone has a `.git`
# directory, so the file test alone already excludes one.
#
# Ask git for the mirror this worktree belongs to and compare resolved paths:
# worktree bookkeeping records the mirror's real path, while $MIRROR may name
# the same directory through a symlink (a symlinked $HOME or $XDG_CACHE_HOME),
# and a plain string compare would then disown our own worktree. Fall back to
# reading the pointer as text so a worktree whose mirror has been deleted is
# still recognisable as ours.
owned() {
  [[ -f "${DEST}/.git" ]] || return 1
  local common gitdir
  if common="$(git -C "$DEST" rev-parse --git-common-dir 2>/dev/null)" && [[ -n "$common" ]]; then
    [[ "$(resolve_path "$common")" == "$(resolve_path "$MIRROR")" ]] && return 0
  fi
  gitdir="$(head -n 1 "${DEST}/.git")"
  [[ "$gitdir" == "gitdir: ${MIRROR}/worktrees/"* ]] && return 0
  # git records the mirror's real path while $MIRROR may be spelled through a
  # symlink, and with the mirror gone `resolve_path "$MIRROR"` cannot bridge the
  # two. Its parent is still resolvable, so rebuild the canonical spelling from
  # there — otherwise the deleted-mirror recovery this fallback exists for is
  # exactly what breaks under a symlinked cache root.
  local canonical_parent
  canonical_parent="$(resolve_path "$(dirname "$MIRROR")")"
  [[ "$gitdir" == "gitdir: ${canonical_parent}/$(basename "$MIRROR")/worktrees/"* ]]
}

if (( PRINT_PATH )); then
  if ! installed; then
    echo "fetch-qpdf-source.sh: qpdf ${QPDF_VERSION} source not installed at ${DEST}" >&2
    echo "fetch-qpdf-source.sh: run scripts/fetch-qpdf-source.sh first" >&2
    exit 1
  fi
  if [[ "$(tracked_state)" == "dirty" ]]; then
    refuse_modified
  fi
  warn_on_binary_drift
  printf '%s\n' "$DEST"
  exit 0
fi

if (( ! FORCE )) && installed; then
  if [[ "$(tracked_state)" == "dirty" ]]; then
    refuse_modified
  fi
  echo "qpdf ${QPDF_VERSION} source already present: ${DEST}"
  warn_on_binary_drift
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

# Reaching here means $DEST is about to be replaced by `worktree remove --force`
# (or `rm -rf`), which is irreversible. The guards above only cover a dirty tree
# that still satisfies `installed`; an owned worktree checked out elsewhere, or
# with its sentinel deleted, is dirty too and would otherwise be wiped silently.
# Discard only when explicitly asked.
if (( ! FORCE )) && [[ -e "$DEST" ]]; then
  case "$(tracked_state)" in
    dirty)   refuse_modified ;;
    unknown) refuse_unverifiable ;;
  esac
fi

if ! command -v git >/dev/null 2>&1; then
  echo "fetch-qpdf-source.sh: git is required to clone ${QPDF_REPO}" >&2
  exit 1
fi

mkdir -p "$CACHE_ROOT"

cleanup() {
  [[ -n "$SCRATCH" ]] && rm -rf "$SCRATCH"
  (( LOCK_HELD )) && rmdir "$LOCK" 2>/dev/null
  :
}
trap cleanup EXIT

waited=0
until mkdir "$LOCK" 2>/dev/null; do
  if (( waited == 0 )); then
    echo "Waiting for another fetch-qpdf-source.sh to release ${LOCK}" >&2
  fi
  if (( waited >= 600 )); then
    echo "fetch-qpdf-source.sh: timed out waiting for ${LOCK}" >&2
    echo "                      if no other run is active it is stale: rmdir ${LOCK}" >&2
    exit 1
  fi
  sleep 1
  waited=$((waited + 1))
done
LOCK_HELD=1

# Another run may have completed the install while we queued for the lock.
if (( ! FORCE )) && installed && [[ "$(tracked_state)" == "clean" ]]; then
  echo "qpdf ${QPDF_VERSION} source already present: ${DEST}"
  warn_on_binary_drift
  exit 0
fi

SCRATCH="$(mktemp -d "${CACHE_ROOT}/.flpdf-qpdf-src.XXXXXX")"

if [[ -e "$MIRROR" ]]; then
  if ! git -C "$MIRROR" rev-parse --git-dir >/dev/null 2>&1; then
    echo "fetch-qpdf-source.sh: ${MIRROR} exists but is not a git repository" >&2
    echo "                      move it aside and re-run" >&2
    exit 1
  fi
  # A non-bare checkout here would still accept `worktree add`, but its worktrees
  # point at ${MIRROR}/.git — a path `owned()` does not recognise — so the very
  # next run would disown the tree this one just created.
  if [[ "$(git -C "$MIRROR" rev-parse --is-bare-repository 2>/dev/null)" != "true" ]]; then
    echo "fetch-qpdf-source.sh: ${MIRROR} is a git repository but not a bare mirror" >&2
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
warn_on_binary_drift
