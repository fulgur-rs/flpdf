#!/usr/bin/env python3
"""Count production and test callers of flpdf symbols tracked by the route matrix.

``docs/qpdf-route-matrix/`` classifies flpdf routes as canonical / bridge /
mixed / unknown and, for every bridge or mixed row, records how many
production and test call sites still reach the non-canonical route. Those
numbers decide when a bridge can be deleted, so they must be re-measurable
after every cutover with one convention. This script is that convention:

* An occurrence is a whole-word match of the symbol's last ``::`` segment.
* Excluded occurrences (they are not callers): comment-only lines
  (``//``, ``///``, ``//!``), declaration lines (``fn``/``struct``/``enum``/
  ``trait``/``type``/``const``/``static``/``mod`` + the symbol), ``use`` lines
  including the continuation lines of a multi-line ``use {…}``, ``impl``
  header lines, and mentions inside string literals.
* Type-position references (argument, return, field types) are counted --
  they are real references that a cutover must migrate.
* ``test`` = files under ``tests/``, ``benches/`` or ``examples/``, files whose
  basename ends in ``_tests.rs``, and every line inside an item that is
  guarded by ``#[cfg(test)]`` (a ``mod … { … }`` block, a ``fn``/``impl``
  body, or a single-line item). Guarded blocks are tracked by brace depth,
  so a file that interleaves production code with several column-0
  ``#[cfg(test)] mod`` blocks (``object_handle.rs`` has 21) is split
  correctly. Everything else under ``src/`` is ``prod``.

The tracked symbol list lives in ``docs/qpdf-route-matrix/tracked-symbols.txt``
(one symbol per line, ``#`` comments). ``--symbol`` overrides it.
``--expect-zero`` turns the report into a gate: exit 1 if any listed symbol
still has a production caller -- the "bridge caller ゼロ" completion check.

The exclusions are line-based heuristics, adequate for a tracker; they are
not a Rust parser.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass, field
import json
from pathlib import Path
import re
import sys


DEFAULT_MANIFEST = Path("docs/qpdf-route-matrix/tracked-symbols.txt")
DECL_KEYWORDS = r"(?:fn|struct|enum|trait|type|const|static|mod|macro_rules!)"
TEST_DIR_PARTS = frozenset({"tests", "benches", "examples"})


@dataclass
class SymbolCount:
    prod: int = 0
    test: int = 0
    prod_files: dict[str, int] = field(default_factory=dict)
    test_files: dict[str, int] = field(default_factory=dict)


def mask_strings(line: str) -> str:
    """Replace the contents of string literals with spaces (same length)."""
    out: list[str] = []
    in_string = False
    escaped = False
    for ch in line:
        if in_string:
            if escaped:
                escaped = False
                out.append(" ")
            elif ch == "\\":
                escaped = True
                out.append(" ")
            elif ch == '"':
                in_string = False
                out.append(ch)
            else:
                out.append(" ")
        elif ch == '"':
            in_string = True
            out.append(ch)
        else:
            out.append(ch)
    return "".join(out)


def strip_line_comment(masked: str) -> str:
    index = masked.find("//")
    return masked if index < 0 else masked[:index]


def is_test_file(relative: Path) -> bool:
    if any(part in TEST_DIR_PARTS for part in relative.parts[:-1]):
        return True
    return relative.name.endswith("_tests.rs")


def cfg_test_lines(lines: list[str]) -> set[int]:
    """Return the 0-based line indexes that belong to a ``#[cfg(test)]`` item."""
    guarded: set[int] = set()
    i = 0
    while i < len(lines):
        if lines[i].strip() == "#[cfg(test)]":
            j = i + 1
            while j < len(lines) and lines[j].strip().startswith("#["):
                j += 1
            if j >= len(lines):
                break
            for k in range(i, j + 1):
                guarded.add(k)
            depth = 0
            opened = False
            k = j
            while k < len(lines):
                code = strip_line_comment(mask_strings(lines[k]))
                for ch in code:
                    if ch == "{":
                        depth += 1
                        opened = True
                    elif ch == "}":
                        depth -= 1
                guarded.add(k)
                if opened and depth <= 0:
                    break
                if not opened and code.rstrip().endswith(";"):
                    break
                k += 1
            i = k + 1
            continue
        i += 1
    return guarded


def count_file(path: Path, relative: Path, leafs: dict[str, str], totals: dict[str, SymbolCount]) -> None:
    text = path.read_text(encoding="utf-8", errors="replace")
    lines = text.split("\n")
    file_is_test = is_test_file(relative)
    guarded = set() if file_is_test else cfg_test_lines(lines)
    in_use_block = False
    patterns = {
        symbol: re.compile(rf"(?<![A-Za-z0-9_]){re.escape(leaf)}(?![A-Za-z0-9_])")
        for symbol, leaf in leafs.items()
    }
    decl_patterns = {
        symbol: re.compile(rf"^\s*(?:pub(?:\([a-z]+\))?\s+)?{DECL_KEYWORDS}\s+{re.escape(leaf)}(?![A-Za-z0-9_])")
        for symbol, leaf in leafs.items()
    }
    for index, raw in enumerate(lines):
        stripped = raw.strip()
        if in_use_block:
            if "}" in strip_line_comment(mask_strings(raw)):
                in_use_block = False
            continue
        if re.match(r"^\s*(?:pub(?:\([a-z]+\))?\s+)?use\b", raw):
            code = strip_line_comment(mask_strings(raw))
            if "{" in code and "}" not in code:
                in_use_block = True
            continue
        if stripped.startswith("//"):
            continue
        if re.match(r"^\s*impl\b", raw):
            continue
        masked = strip_line_comment(mask_strings(raw))
        is_test_line = file_is_test or index in guarded
        for symbol, pattern in patterns.items():
            if decl_patterns[symbol].match(raw):
                continue
            hits = len(pattern.findall(masked))
            if hits == 0:
                continue
            bucket = totals[symbol]
            key = relative.as_posix()
            if is_test_line:
                bucket.test += hits
                bucket.test_files[key] = bucket.test_files.get(key, 0) + hits
            else:
                bucket.prod += hits
                bucket.prod_files[key] = bucket.prod_files.get(key, 0) + hits


def load_manifest(path: Path) -> list[str]:
    symbols: list[str] = []
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.split("#", 1)[0].strip()
        if line:
            symbols.append(line)
    return symbols


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n", 1)[0])
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--symbol", action="append", default=[], help="symbol to count (repeatable; overrides the manifest)")
    parser.add_argument("--expect-zero", action="store_true", help="exit 1 if any symbol has a production caller")
    parser.add_argument("--json", action="store_true", help="emit machine-readable output")
    args = parser.parse_args(argv)

    root = args.root.resolve()
    if args.symbol:
        symbols = list(args.symbol)
    else:
        manifest = root / args.manifest
        if not manifest.is_file():
            print(f"error: manifest not found: {manifest} (pass --symbol or create it)")
            return 1
        symbols = load_manifest(manifest)
    if not symbols:
        print("error: no symbols to count")
        return 1

    leafs = {symbol: symbol.rsplit("::", 1)[-1] for symbol in symbols}
    totals = {symbol: SymbolCount() for symbol in symbols}
    crates = root / "crates"
    for path in sorted(crates.rglob("*.rs")):
        if any(part == "target" for part in path.parts):
            continue
        count_file(path, path.relative_to(root), leafs, totals)

    if args.json:
        payload = {
            "symbols": {
                symbol: {
                    "prod": c.prod,
                    "test": c.test,
                    "prod_files": dict(sorted(c.prod_files.items())),
                    "test_files": dict(sorted(c.test_files.items())),
                }
                for symbol, c in totals.items()
            }
        }
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        for symbol, c in totals.items():
            files = ", ".join(f"{k} {v}" for k, v in sorted(c.prod_files.items(), key=lambda kv: (-kv[1], kv[0])))
            print(f"{symbol}: prod {c.prod} ({len(c.prod_files)} files) / test {c.test}")
            if files:
                print(f"    {files}")

    if args.expect_zero:
        failing = [symbol for symbol, c in totals.items() if c.prod > 0]
        if failing:
            print("FAILED: production callers remain for: " + ", ".join(f"{s}: prod {totals[s].prod}" for s in failing))
            return 1
        print("OK: no production callers remain")
    return 0


if __name__ == "__main__":
    sys.exit(main())
