#!/usr/bin/env python3
"""Count production and test callers of flpdf symbols tracked by the route matrix.

``docs/qpdf-route-matrix/`` classifies flpdf routes as canonical / bridge /
mixed / unknown and, for every bridge or mixed row, records how many
production and test call sites still reach the non-canonical route. Those
numbers decide when a bridge can be deleted, so they must be re-measurable
after every cutover with one convention. This script is that convention:

* An occurrence is a whole-word match of the symbol's last ``::`` segment.
* Excluded occurrences (they are not callers): comment-only lines
  (``//``, ``///``, ``//!``), item declaration lines (``fn``/``struct``/
  ``enum``/``trait``/``type``/``const``/``static``/``mod`` + the symbol),
  struct field declaration lines, ``use`` lines including the continuation
  lines of a multi-line ``use {…}``, ``impl`` header lines, and mentions inside
  normal or raw string literals.
* Type-position references (argument, return, field types) are counted --
  they are real references that a cutover must migrate.
* ``test`` = files under ``tests/``, ``benches/`` or ``examples/``, files whose
  basename ends in ``_tests.rs``, and every line inside an item that is
  guarded by ``#[cfg(test)]`` or a test-only compound cfg such as
  ``#[cfg(all(test, feature = "qtest-driver"))]`` (a ``mod … { … }`` block, a ``fn``/``impl``
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


def mask_rust_source(text: str) -> str:
    """Blank Rust comments and normal/raw string literals, preserving lines."""
    masked = list(text)
    length = len(text)
    index = 0

    def blank(start: int, end: int) -> None:
        for position in range(start, end):
            if masked[position] != "\n":
                masked[position] = " "

    while index < length:
        if text.startswith("//", index):
            end = text.find("\n", index)
            if end < 0:
                end = length
            blank(index, end)
            index = end
            continue
        if text.startswith("/*", index):
            end = index + 2
            depth = 1
            while end < length and depth:
                if text.startswith("/*", end):
                    depth += 1
                    end += 2
                elif text.startswith("*/", end):
                    depth -= 1
                    end += 2
                else:
                    end += 1
            blank(index, end)
            index = end
            continue

        raw_marker = index
        if text.startswith("br", index):
            raw_marker = index + 1
        if text[raw_marker : raw_marker + 1] == "r":
            cursor = raw_marker + 1
            while cursor < length and text[cursor] == "#":
                cursor += 1
            hashes = text[raw_marker + 1 : cursor]
            if cursor < length and text[cursor] == '"':
                delimiter = '"' + hashes
                close = text.find(delimiter, cursor + 1)
                end = length if close < 0 else close + len(delimiter)
                blank(index, end)
                index = end
                continue

        if text[index] == '"':
            end = index + 1
            escaped = False
            while end < length:
                character = text[end]
                if escaped:
                    escaped = False
                elif character == "\\":
                    escaped = True
                elif character == '"':
                    end += 1
                    break
                end += 1
            blank(index, end)
            index = end
            continue

        index += 1

    return "".join(masked)


def is_test_file(relative: Path) -> bool:
    if any(part in TEST_DIR_PARTS for part in relative.parts[:-1]):
        return True
    return relative.name.endswith("_tests.rs")


def cfg_test_lines(lines: list[str], masked_lines: list[str]) -> set[int]:
    """Return the 0-based line indexes that belong to a ``#[cfg(test)]`` item."""
    guarded: set[int] = set()
    i = 0
    while i < len(lines):
        attribute = masked_lines[i].strip()
        is_test_cfg = bool(
            re.fullmatch(r"#\[cfg\(test\)\]", attribute)
            or re.fullmatch(r"#\[cfg\(all\([^)]*\btest\b[^)]*\)\)\]", attribute)
        )
        if is_test_cfg:
            j = i + 1
            while j < len(lines) and masked_lines[j].strip().startswith("#["):
                j += 1
            if j >= len(lines):
                break
            for k in range(i, j + 1):
                guarded.add(k)
            depth = 0
            opened = False
            k = j
            while k < len(lines):
                code = masked_lines[k]
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


def struct_body_lines(masked_lines: list[str]) -> set[int]:
    """Return line indexes inside Rust struct bodies."""
    body_lines: set[int] = set()
    body_depths: list[int] = []
    brace_depth = 0
    pending_struct = False

    for index, line in enumerate(masked_lines):
        if body_depths:
            body_lines.add(index)
        if re.search(r"\bstruct\b", line):
            pending_struct = True
        opened_struct = False
        for character in line:
            if character == "{":
                brace_depth += 1
                if pending_struct and not opened_struct:
                    body_depths.append(brace_depth)
                    opened_struct = True
                    pending_struct = False
            elif character == "}":
                brace_depth -= 1
                while body_depths and brace_depth < body_depths[-1]:
                    body_depths.pop()
        if pending_struct and not opened_struct and ";" in line:
            pending_struct = False
    return body_lines


def count_file(path: Path, relative: Path, leafs: dict[str, str], totals: dict[str, SymbolCount]) -> None:
    text = path.read_text(encoding="utf-8", errors="replace")
    lines = text.split("\n")
    masked_lines = mask_rust_source(text).split("\n")
    file_is_test = is_test_file(relative)
    guarded = set() if file_is_test else cfg_test_lines(lines, masked_lines)
    struct_lines = struct_body_lines(masked_lines)
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
        masked = masked_lines[index]
        stripped = masked.strip()
        if in_use_block:
            if "}" in masked:
                in_use_block = False
            continue
        if re.match(r"^\s*(?:pub(?:\([^)]*\))?\s+)?use\b", masked):
            code = masked
            if "{" in code and "}" not in code:
                in_use_block = True
            continue
        if not stripped:
            continue
        if re.match(r"^\s*impl\b", masked):
            continue
        is_test_line = file_is_test or index in guarded
        for symbol, pattern in patterns.items():
            leaf = leafs[symbol]
            if decl_patterns[symbol].match(masked):
                continue
            if index in struct_lines and re.match(
                rf"^\s*(?:pub(?:\([^)]*\))?\s+)?{re.escape(leaf)}\s*:",
                masked,
            ):
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

    failing = [symbol for symbol, c in totals.items() if c.prod > 0]
    absent = [symbol for symbol, c in totals.items() if c.prod == 0 and c.test == 0]

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
        if args.expect_zero:
            payload["expect_zero"] = {
                "ok": not failing,
                "failing": failing,
                "absent": absent,
            }
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        for symbol, c in totals.items():
            files = ", ".join(f"{k} {v}" for k, v in sorted(c.prod_files.items(), key=lambda kv: (-kv[1], kv[0])))
            print(f"{symbol}: prod {c.prod} ({len(c.prod_files)} files) / test {c.test}")
            if files:
                print(f"    {files}")

    if args.expect_zero:
        if failing:
            if args.json:
                return 1
            print("FAILED: production callers remain for: " + ", ".join(f"{s}: prod {totals[s].prod}" for s in failing))
            return 1
        # A symbol with no occurrence at all passes vacuously: it may have been
        # deleted (the intended end state) or misspelled. Say so, so the gate
        # cannot be mistaken for evidence that the route was ever tracked.
        if absent:
            if args.json:
                return 0
            print("note: no occurrence at all (deleted, or misspelled?): " + ", ".join(absent))
        if args.json:
            return 0
        print("OK: no production callers remain")
    return 0


if __name__ == "__main__":
    sys.exit(main())
