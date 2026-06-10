#!/usr/bin/env bash
#
# patch-coverage.sh — pre-PR changed-line ("patch") coverage check.
#
# Lists the lines added on this branch that no test executes, and GATES the
# `flpdf` library crate: any uncovered changed line under crates/flpdf/src
# fails (exit 1). crates/flpdf-cli/src is reported only (best-effort).
#
# Coverage is measured over the WHOLE workspace (flpdf-cli tests drive flpdf,
# so a crate-scoped run would under-count flpdf); only the diff/gate is scoped.
#
# Genuinely untestable lines can be excluded with markers that carry a reason:
#     let _ = unreachable_defensive_arm(); // cov:ignore: defensive, see ISO ...
#     // cov:ignore-start  (reason)
#     ...block...
#     // cov:ignore-end
#
# Usage:
#   scripts/patch-coverage.sh [--base <ref>] [--lcov <path>]
#
#   --base <ref>   Compare against this ref's merge-base (default: origin/main).
#                  Stacked PRs pass the parent branch.
#   --lcov <path>  Reuse an existing lcov report instead of running coverage
#                  (skips the slow instrumented rebuild). Without it, the script
#                  runs `cargo llvm-cov --workspace --lcov`.
set -euo pipefail

BASE="origin/main"
LCOV=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --base)  BASE="${2:?--base needs a ref}"; shift 2 ;;
    --lcov)  LCOV="${2:?--lcov needs a path}"; shift 2 ;;
    -h|--help)
      sed -n '2,25p' "$0" | sed 's/^# \{0,1\}//'
      exit 0 ;;
    *) echo "patch-coverage: unknown argument: $1" >&2; exit 2 ;;
  esac
done

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

if ! MERGE_BASE="$(git merge-base "$BASE" HEAD 2>/dev/null)"; then
  echo "patch-coverage: cannot find merge-base with '$BASE'." >&2
  echo "  Fetch it first, e.g.: git fetch origin main" >&2
  exit 2
fi

if [[ -z "$LCOV" ]]; then
  LCOV="target/patch-cov.lcov"
  echo "[patch-coverage] running 'cargo llvm-cov --workspace' (slow; pass --lcov <path> to reuse a report)..." >&2
  cargo llvm-cov --workspace --lcov --output-path "$LCOV" >&2
fi

if [[ ! -f "$LCOV" ]]; then
  echo "patch-coverage: lcov report not found: $LCOV" >&2
  exit 2
fi

# Coverage instruments the working tree, but the diff is HEAD vs merge-base —
# so the gate must run on a committed tree to stay aligned (see CLAUDE.md).
# Diffing HEAD (not the working tree) is deliberate: a working-tree diff would
# silently drop untracked new files from the gate. Warn instead when dirty.
if ! git diff --quiet HEAD -- 2>/dev/null \
   || [[ -n "$(git ls-files --others --exclude-standard)" ]]; then
  echo "[patch-coverage] warning: working tree has uncommitted or untracked changes." >&2
  echo "                 The gate compares HEAD against '$BASE'; commit your work first" >&2
  echo "                 so coverage and the diff line up." >&2
fi

DIFF_FILE="$(mktemp)"
trap 'rm -f "$DIFF_FILE"' EXIT
git diff --unified=0 "$MERGE_BASE" HEAD > "$DIFF_FILE"

python3 - "$LCOV" "$DIFF_FILE" "$REPO_ROOT" <<'PYEOF'
import os
import re
import sys

lcov_path, diff_path, repo_root = sys.argv[1], sys.argv[2], sys.argv[3]

GATE_PREFIX = "crates/flpdf/src/"        # 100% gate
REPORT_PREFIX = "crates/flpdf-cli/src/"  # report-only

# 1. lcov -> {relpath: {line: hits}}.  Only lines with a DA record are
#    executable; a changed line without one (blank, comment, brace, decl) is
#    not counted as uncovered.
cov = {}
cur = None
with open(lcov_path, encoding="utf-8", errors="replace") as fh:
    for line in fh:
        if line.startswith("SF:"):
            cur = os.path.relpath(line[3:].strip(), repo_root)
            cov.setdefault(cur, {})
        elif line.startswith("DA:") and cur is not None:
            num, hits = line[3:].strip().split(",")[:2]
            cov[cur][int(num)] = int(hits)
        elif line.startswith("end_of_record"):
            cur = None

# 2. diff -> {relpath: set(added new-file line numbers)}.
added = {}
cur = None
new_ln = None
hunk_re = re.compile(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@")
with open(diff_path, encoding="utf-8", errors="replace") as fh:
    for line in fh:
        if line.startswith("diff --git "):
            # New file block: reset so a later "+++ " is read as the header.
            cur = None
            new_ln = None
        elif line.startswith("+++ ") and new_ln is None:
            # Only a header before any hunk; inside a hunk new_ln is set, so an
            # added line whose content starts with "++ " (diff: "+++ ...") falls
            # through to the added-line branch below instead of being misread.
            path = line[4:].strip()
            if path == "/dev/null":
                cur = None
            else:
                cur = path[2:] if path[:2] in ("a/", "b/") else path
                added.setdefault(cur, set())
        elif line.startswith("@@"):
            m = hunk_re.match(line)
            new_ln = int(m.group(1)) if m else None
        elif cur is not None and new_ln is not None:
            if line.startswith("+"):
                added[cur].add(new_ln)
                new_ln += 1
            elif line.startswith("-"):
                pass  # removed line: new-file cursor does not advance
            elif line.startswith(" "):
                new_ln += 1  # context (only with -U>0); keep cursor honest
            # other lines (e.g. "\ No newline at end of file") do not advance

# 3. // cov:ignore markers (line + start/end block) read from source.
#    Returns (excluded_line_set, marker_errors).  An unterminated block — or a
#    stray -end — is an error, not a silent exclusion: leaving in_block true to
#    EOF would drop every later changed line and let the gate pass over untested
#    code.
def excluded_lines(relpath):
    excl = set()
    errors = []
    full = os.path.join(repo_root, relpath)
    if not os.path.isfile(full):
        return excl, errors
    in_block = False
    start_line = None
    with open(full, encoding="utf-8", errors="replace") as fh:
        for i, src in enumerate(fh, start=1):
            if "cov:ignore-start" in src:
                if in_block:
                    errors.append((i, "nested cov:ignore-start"))
                in_block = True
                start_line = i
                excl.add(i)
            elif "cov:ignore-end" in src:
                if not in_block:
                    errors.append((i, "cov:ignore-end without matching start"))
                in_block = False
                excl.add(i)
            elif in_block or "cov:ignore" in src:
                excl.add(i)
    if in_block:
        errors.append((start_line, "cov:ignore-start without matching end"))
    return excl, errors

# 4. Classify changed lines per crate group.
groups = {"gate": {}, "report": {}}
marker_errors = {}
for relpath, lines in added.items():
    if relpath.startswith(GATE_PREFIX):
        grp = "gate"
    elif relpath.startswith(REPORT_PREFIX):
        grp = "report"
    else:
        continue
    file_cov = cov.get(relpath, {})
    excl, errs = excluded_lines(relpath)
    if errs:
        marker_errors[relpath] = errs
    changed_exec = [n for n in lines if n in file_cov and n not in excl]
    uncovered = sorted(n for n in changed_exec if file_cov[n] == 0)
    if changed_exec:
        groups[grp][relpath] = (len(changed_exec), uncovered)

# Malformed markers corrupt the gate — fail loudly before reporting coverage.
if marker_errors:
    print("== patch coverage ==")
    print("ERROR: malformed // cov:ignore markers (each -start needs an -end):")
    for relpath in sorted(marker_errors):
        for ln, msg in marker_errors[relpath]:
            print(f"  {relpath}:{ln}: {msg}")
    sys.exit(2)

def render(label, grp, gated):
    files = groups[grp]
    changed = sum(c for c, _ in files.values())
    uncov = sum(len(u) for _, u in files.values())
    if changed == 0:
        verdict = "PASS (no executable changed lines)"
    elif uncov == 0:
        verdict = "PASS (100%)"
    elif gated:
        verdict = "FAIL"
    else:
        verdict = "report-only"
    print(f"{label:<10}: changed {changed}, uncovered {uncov}   -> {verdict}")
    for relpath in sorted(files):
        _, uncovered = files[relpath]
        if uncovered:
            nums = ", ".join(str(n) for n in uncovered)
            print(f"  {relpath}: {nums}")
    return uncov

print("== patch coverage (changed lines vs tests) ==")
gate_uncov = render("flpdf", "gate", gated=True)
render("flpdf-cli", "report", gated=False)

if gate_uncov:
    print()
    print("FAIL: flpdf has uncovered changed lines. Add tests, or mark genuinely")
    print("      untestable lines with `// cov:ignore: <reason>` and note it in the PR.")
    sys.exit(1)
print()
print("OK: flpdf changed lines fully covered.")
PYEOF
