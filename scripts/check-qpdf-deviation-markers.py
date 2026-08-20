#!/usr/bin/env python3
"""Validate `qpdf-deviation` markers across flpdf source files.

A `qpdf-deviation` marker records a flpdf behavior that intentionally
diverges from qpdf because qpdf has no counterpart for it at all (CLAUDE.md's
"qpdf に対応物が一切ない flpdf 固有の挙動" category -- distinct from both
deviation class (A), which changes output bytes, and class (B), which
replaces a qpdf-counterpart concept's container with a permanent, widely used
Rust idiom such as `InputSource` -> `Read + Seek`). Unlike `// cov:ignore`
(patch-coverage.sh), which excludes changed lines from the coverage gate,
this marker exists so the deviation itself can be found by grep and is not
silently reintroduced as a "regression" during a later refactor that folds
two existing implementations into one shared primitive.

Marker forms, mirroring the `// cov:ignore` grammar in
scripts/patch-coverage.sh:

    // qpdf-deviation: <reason>
    // qpdf-deviation-start: <reason>
    ...block...
    // qpdf-deviation-end

A marker must be a real `//` line comment. A reason is required on
`qpdf-deviation` and `qpdf-deviation-start`; `qpdf-deviation-end` takes no
text. Blocks must not nest and every `-start` needs a matching `-end`.
Within the scanned tree (`crates/*/src/**/*.rs`), anything that mentions the
token but is not a well-formed marker is an error, never a silent no-op --
a malformed marker that fails open would defeat the point of a grep-able
record. Files outside that tree (`tests/`, `build.rs`, `benches/`, `fuzz/`)
are not scanned at all, matching `scripts/qpdf-module-docs.py`'s published-
source-only scope.

Known limitations, shared with `// cov:ignore`'s `_find_line_comment`
(scripts/patch-coverage.sh), whose exact scanning algorithm this mirrors:

- Comment and string detection resets at every line, so a `//` sequence
  written inside a `/* ... */` block comment or a multi-line string literal
  is misread as a real line comment. This repository's style does not use
  block comments, so the risk is accepted rather than adding full lexical
  (multi-line) state tracking.
- Single quotes are not tracked (so Rust lifetimes like `'a` do not confuse
  the scanner), which means a char/byte literal containing a double quote,
  such as `b'"'`, is misread as opening a string and can hide a real `//`
  marker later on the same line. Put the marker on its own line in that
  case.
- Raw string delimiters (`r"..."`, `r#"..."#`, `br#"..."#`, ...) are not
  tracked, so an embedded `"` inside a raw string (this repository already
  has one at crates/flpdf-qtest-tools/src/main.rs) can close the scanner's
  naive string-tracking early and let a later `//` sequence still inside
  the raw string be misread as a real comment -- accepting text that is not
  actually a marker. This is the higher-severity direction of the same
  quote-tracking gap (a false accept rather than a false reject); avoided
  in practice by never following an embedded-quote raw string with
  anything resembling `// qpdf-deviation` on the same line.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path


_MARKER_RE = re.compile(r"\s*qpdf-deviation(-start|-end)?\b\s*(:?)\s*(.*?)\s*$")


def _find_line_comment(src: str) -> int | None:
    """Return the index where a real `//` line comment starts, or None.

    Tracks double-quoted strings (with backslash escapes) so a `//` inside a
    string literal is not read as a comment. Mirrors
    scripts/patch-coverage.sh's `_find_line_comment`.
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
        elif ch == "/" and src[i + 1 : i + 2] == "/":
            return i
        i += 1
    return None


def _comment_text(src: str) -> str | None:
    idx = _find_line_comment(src)
    return None if idx is None else src[idx + 2 :]


def scan_source(source: str) -> list[tuple[int, str]]:
    """Return a list of (line_number, error_message) for malformed markers."""
    errors: list[tuple[int, str]] = []
    in_block = False
    start_line: int | None = None
    for i, src in enumerate(source.splitlines(keepends=True), start=1):
        has_token = "qpdf-deviation" in src
        comment = _comment_text(src) if has_token else None
        m = _MARKER_RE.match(comment) if comment is not None else None
        if m:
            kind, colon, rest = m.group(1), m.group(2), m.group(3).strip()
            if kind == "-start":
                if not (colon and rest):
                    errors.append((i, "qpdf-deviation-start requires ': <reason>'"))
                else:
                    if in_block:
                        errors.append((i, "nested qpdf-deviation-start"))
                    in_block = True
                    start_line = i
            elif kind == "-end":
                if colon or rest:
                    errors.append((i, "qpdf-deviation-end takes no text"))
                elif not in_block:
                    errors.append((i, "qpdf-deviation-end without matching start"))
                else:
                    in_block = False
            elif colon and rest:
                pass  # well-formed single-line marker
            else:
                errors.append((i, "qpdf-deviation requires ': <reason>'"))
        elif has_token:
            errors.append(
                (i, "qpdf-deviation must be a `// qpdf-deviation[-start|-end]` comment")
            )
    if in_block:
        errors.append((start_line, "qpdf-deviation-start without matching end"))
    return errors


def check(root: Path) -> int:
    errors_by_file: dict[str, list[tuple[int, str]]] = {}
    for path in sorted(root.glob("crates/*/src/**/*.rs")):
        source = path.read_text(encoding="utf-8", errors="replace")
        errors = scan_source(source)
        if errors:
            errors_by_file[str(path.relative_to(root))] = errors
    if not errors_by_file:
        print("OK: no malformed qpdf-deviation markers.")
        return 0
    print("ERROR: malformed // qpdf-deviation markers (each -start needs an -end):")
    for relpath in sorted(errors_by_file):
        for line, msg in errors_by_file[relpath]:
            print(f"  {relpath}:{line}: {msg}")
    return 1


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
        help="scan crates/*/src for malformed qpdf-deviation markers",
    )
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parents[1],
        help="repository root (default: parent of scripts/)",
    )
    args = parser.parse_args(argv)
    if not args.check:
        parser.print_help()
        return 2
    return check(args.root)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
