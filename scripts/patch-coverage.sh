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
# Genuinely untestable lines can be excluded with a `//` comment + a reason.
#     if n < 0 { return 0; } // cov:ignore: unreachable defensive arm
#     // cov:ignore-start: <reason>
#     ...block...
#     // cov:ignore-end
#
# Usage:
#   scripts/patch-coverage.sh [--base <ref>] [--lcov <path>] [--allow-dirty]
#
#   --base <ref>   Compare against this ref's merge-base (default: origin/main).
#                  Stacked PRs pass the parent branch.
#   --lcov <path>  Reuse an existing lcov report instead of running coverage
#                  (skips the slow instrumented rebuild). Without it, the script
#                  runs `cargo llvm-cov --workspace --features qpdf-zlib-compat
#                  --ignore-run-fail --lcov`. The report MUST come from the
#                  current commit; a stale report can mismeasure.
#   --allow-dirty  Proceed on a dirty working tree (default: error). Coverage
#                  measures the tree but the gate diffs HEAD, so a dirty tree can
#                  produce a false pass — only override when you know it aligns.
set -euo pipefail

BASE="origin/main"
LCOV=""
ALLOW_DIRTY=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --base)         BASE="${2:?--base needs a ref}"; shift 2 ;;
    --lcov)         LCOV="${2:?--lcov needs a path}"; shift 2 ;;
    --allow-dirty)  ALLOW_DIRTY=1; shift ;;
    -h|--help)
      sed -n '2,29p' "$0" | sed 's/^# \{0,1\}//'
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

# Coverage instruments the working tree, but the gate diffs HEAD vs the merge
# base. On a dirty tree the two disagree (uncommitted lines measured but not
# gated; uncommitted cov:ignore markers affecting committed lines), which could
# print a false "OK". Fail before the expensive coverage run unless the caller
# opts out. Diffing HEAD (not the tree) is deliberate: a working-tree diff would
# instead silently drop untracked new files from the gate.
if ! git diff --quiet HEAD -- 2>/dev/null \
   || [[ -n "$(git ls-files --others --exclude-standard)" ]]; then
  if [[ "$ALLOW_DIRTY" == 1 ]]; then
    echo "[patch-coverage] warning: dirty working tree (--allow-dirty); coverage measures" >&2
    echo "                 the tree but the gate diffs HEAD — results may misalign." >&2
  else
    echo "patch-coverage: working tree has uncommitted or untracked changes." >&2
    echo "  The gate diffs HEAD vs '$BASE' while coverage measures the tree, so a dirty" >&2
    echo "  tree can produce a false pass. Commit your work first, or pass --allow-dirty." >&2
    exit 2
  fi
fi

if [[ -z "$LCOV" ]]; then
  # Fresh, whole-workspace measurement: a gated file absent from the report is
  # genuinely non-executable. A user-supplied --lcov may be stale, so that case
  # is checked (see LCOV_MODE below).
  #
  # We enable `qpdf-zlib-compat` because the byte-identical byte_gate tests are
  # gated on that feature; without it, most of the byte-parity plumbing (e.g.
  # `overlay::byte_gate`, `overlay_annotations`) is exercised by nothing that
  # runs under `cargo test`, and coverage under-reports catastrophically
  # (600+ false-positive uncovered lines on flpdf-9hc.34).
  #
  # We pass `--ignore-run-fail` as a safety net so a pre-existing test miss
  # never blocks the whole coverage run — the gate is the changed-line diff,
  # not the overall test outcome, and other workflows already run the full
  # test suite with a proper pass/fail gate. Concrete case today:
  # `compat_matrix_baseline`'s markdown snapshot was blessed against miniz
  # output, so the `flpdf-sha` and `byte-equal` columns drift under the
  # required `--features qpdf-zlib-compat` build. (The sibling
  # `compat_baseline_static_id` used to hit the same trap and has since
  # been cfg-gated on the feature and re-blessed against zlib output —
  # flpdf-qrg8.)
  LCOV_MODE="fresh"
  LCOV="target/patch-cov.lcov"
  echo "[patch-coverage] running 'cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail' (slow; pass --lcov <path> to reuse a report)..." >&2
  cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail \
    --lcov --output-path "$LCOV" >&2
else
  LCOV_MODE="reused"
  # Line-level staleness of an arbitrary report cannot be detected from lcov
  # alone (it carries no commit/line provenance); the authoritative gate is the
  # fresh default run. Surface the assumption rather than trust it silently.
  echo "[patch-coverage] note: reusing '$LCOV' as-is; a report not generated from the" >&2
  echo "                 current commit can mismeasure. The default (no --lcov) is authoritative." >&2
fi

if [[ ! -f "$LCOV" ]]; then
  echo "patch-coverage: lcov report not found: $LCOV" >&2
  exit 2
fi

DIFF_FILE="$(mktemp)"
trap 'rm -f "$DIFF_FILE"' EXIT
# Harden the diff against user git config: -c diff.external= and --no-ext-diff
# disable an external differ (non-patch output); explicit --src-prefix/--dst-prefix
# force the a/ b/ prefixes the parser expects, overriding diff.mnemonicPrefix
# (w/ i/ c/ ...) and diff.noprefix; --no-color avoids color.diff=always.
git -c diff.external= -c diff.noprefix=false -c diff.mnemonicPrefix=false \
  diff --no-ext-diff --no-color --unified=0 \
  --src-prefix=a/ --dst-prefix=b/ \
  "$MERGE_BASE" HEAD > "$DIFF_FILE"

python3 - "$LCOV" "$DIFF_FILE" "$REPO_ROOT" "$LCOV_MODE" <<'PYEOF'
import os
import re
import sys

lcov_path, diff_path, repo_root = sys.argv[1], sys.argv[2], sys.argv[3]
lcov_mode = sys.argv[4] if len(sys.argv) > 4 else "fresh"

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

# 3. // cov:ignore markers, read strictly from source.
#    A marker must be a real `//` line comment whose text is exactly
#    `cov:ignore: <reason>`, `cov:ignore-start: <reason>`, or `cov:ignore-end`.
#    Substring matching would let a string literal ("cov:ignore") or a
#    reason-less marker silently drop changed lines and bypass the 100% gate,
#    so anything that mentions the token but is not a well-formed marker is a
#    marker error (fails the run), never a silent exclusion. Likewise an
#    unterminated block, a stray -end, or a nested -start are errors.
_MARKER_RE = re.compile(r"\s*cov:ignore(-start|-end)?\b\s*(:?)\s*(.*?)\s*$")

def _find_line_comment(src):
    """Return the index where a real `//` line comment starts, or None.

    Tracks double-quoted strings (with backslash escapes) so a `//` inside a
    string literal — a URL, or a literal "// ..." — is not read as a comment.
    Single quotes are left untracked so Rust lifetimes (`'a`) don't confuse it;
    a char literal containing a quote is not handled (put the marker on its own
    line in that rare case). Shared by `_comment_text` (cov:ignore marker
    parsing) and `_code_before_comment` (declaration-line classification).
    """
    in_str = False
    esc = False
    i = 0
    while i < len(src):
        ch = src[i]
        if in_str:
            if esc:
                esc = False
            elif ch == "\\":
                esc = True
            elif ch == '"':
                in_str = False
        elif ch == '"':
            in_str = True
        elif ch == "/" and src[i + 1:i + 2] == "/":
            return i
        i += 1
    return None

def _comment_text(src):
    """Return the text after the first real `//` line comment, or None."""
    idx = _find_line_comment(src)
    return None if idx is None else src[idx + 2:]

def _code_before_comment(src):
    """Return the code portion of a line, with any trailing `//` comment removed."""
    idx = _find_line_comment(src)
    return src if idx is None else src[:idx]

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
            has_token = "cov:ignore" in src
            comment = _comment_text(src) if has_token else None
            m = _MARKER_RE.match(comment) if comment is not None else None
            if m:
                kind, colon, rest = m.group(1), m.group(2), m.group(3).strip()
                if kind == "-start":
                    if not rest:
                        errors.append((i, "cov:ignore-start requires a reason"))
                    else:
                        if in_block:
                            errors.append((i, "nested cov:ignore-start"))
                        in_block = True
                        start_line = i
                        excl.add(i)
                elif kind == "-end":
                    if rest:
                        errors.append((i, "cov:ignore-end takes no text"))
                    elif not in_block:
                        errors.append((i, "cov:ignore-end without matching start"))
                    else:
                        in_block = False
                        excl.add(i)
                elif colon and rest:
                    excl.add(i)
                else:
                    errors.append((i, "cov:ignore requires ': <reason>'"))
            elif has_token:
                errors.append((i, "cov:ignore must be a `// cov:ignore[-start|-end]` comment"))
            elif in_block:
                excl.add(i)
    if in_block:
        errors.append((start_line, "cov:ignore-start without matching end"))
    return excl, errors

# 3b. A file is "declaration-only" if every line is a module/use declaration
#     (optionally multi-line, e.g. `pub use foo::{\n  a,\n  b,\n};`), a `//`
#     comment (incl. `//!`/`///`, and a trailing `// ...` after a declaration),
#     a module-level attribute, or blank. Such a file compiles to zero
#     executable regions, so llvm-cov never emits an SF: record for it — in a
#     fresh run just as much as a reused one. Used only to exempt these files
#     from the missing-coverage check below; a single non-matching line (a
#     real function, expression, or braced inline `mod name { ... }`) makes
#     the whole file ineligible, falling back to the safe default of still
#     flagging it. `pub(...)` accepts any restricted-visibility path (`pub(crate)`,
#     `pub(super)`, `pub(in crate::foo)`), not just a single identifier.
_DECL_MOD_RE = re.compile(r"^(pub(\([^)]+\))?\s+)?mod\s+\w+\s*;\s*$")
_DECL_USE_RE = re.compile(r"^(pub(\([^)]+\))?\s+)?use\s+")

def is_declaration_only_file(relpath):
    full = os.path.join(repo_root, relpath)
    if not os.path.isfile(full):
        return False
    in_multiline_use = False
    with open(full, encoding="utf-8", errors="replace") as fh:
        for src in fh:
            stripped = _code_before_comment(src).strip()
            if in_multiline_use:
                if stripped.endswith(";"):
                    in_multiline_use = False
                continue
            if not stripped or stripped.startswith("#"):
                continue
            if _DECL_MOD_RE.match(stripped):
                continue
            if _DECL_USE_RE.match(stripped):
                if not stripped.endswith(";"):
                    in_multiline_use = True
                continue
            return False
    return not in_multiline_use

# 4. Classify changed lines per crate group.
groups = {"gate": {}, "report": {}}
marker_errors = {}
missing_cov = []  # gated files with (non-excluded) added lines never measured
for relpath, lines in added.items():
    if relpath.startswith(GATE_PREFIX):
        grp = "gate"
    elif relpath.startswith(REPORT_PREFIX):
        grp = "report"
    else:
        continue
    excl, errs = excluded_lines(relpath)
    if errs:
        marker_errors[relpath] = errs
    # A reused --lcov report may be stale or incomplete; if a gated file has
    # (non-excluded) added lines but no coverage entry at all, the new code
    # may have never been measured, which would let the 100% gate pass
    # vacuously. Exempt declaration-only files: llvm-cov never emits an SF:
    # record for one regardless of report freshness, so its total absence
    # from cov is not evidence of staleness (unlike a file that does contain
    # executable code, where total absence is always suspicious).
    if (
        grp == "gate"
        and lcov_mode == "reused"
        and relpath not in cov
        and (lines - excl)
        and not is_declaration_only_file(relpath)
    ):
        missing_cov.append(relpath)
    file_cov = cov.get(relpath, {})
    changed_exec = [n for n in lines if n in file_cov and n not in excl]
    uncovered = sorted(n for n in changed_exec if file_cov[n] == 0)
    if changed_exec:
        groups[grp][relpath] = (len(changed_exec), uncovered)

# Malformed markers or unmeasured gated files corrupt the gate — fail loudly.
if marker_errors or missing_cov:
    print("== patch coverage ==")
    if marker_errors:
        print("ERROR: malformed // cov:ignore markers (each -start needs an -end):")
        for relpath in sorted(marker_errors):
            for ln, msg in marker_errors[relpath]:
                print(f"  {relpath}:{ln}: {msg}")
    if missing_cov:
        print("ERROR: no coverage data for changed flpdf files (stale/incomplete --lcov?):")
        for relpath in sorted(missing_cov):
            print(f"  {relpath}")
        print("  Regenerate coverage (omit --lcov to measure a fresh workspace run).")
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
