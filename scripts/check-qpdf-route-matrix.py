#!/usr/bin/env python3
"""Validate the citations in ``docs/qpdf-route-matrix/*.md``.

The route matrix records, per qpdf responsibility, which flpdf entry points
implement it and how they are classified (canonical / bridge / mixed /
unknown). Every claim there is anchored to a citation, and a citation that
does not resolve is worse than none: it looks verified. This checker makes the
anchors machine-checked.

Checked forms (all inside backticks, anywhere in the document -- tables and
prose alike):

* ``libqpdf/X.cc:N``, ``libqpdf/X.cc:N-M``, ``include/qpdf/X.hh:N-M``,
  ``qpdf/X.cc:N``, with optional comma-separated extra ranges after one path
  (``libqpdf/X.cc:10-20,45``): the file must exist under the pinned qpdf
  source and every range must satisfy ``1 <= N <= M <= line count``.
* ``crates/<crate>/src/<path>.rs::Sym`` / ``...rs::Type::method``: the file
  must exist under the repository root and the last path segment must be
  declared there (``fn``/``struct``/``enum``/``trait``/``type``/``const``/
  ``static``/``mod``/``macro_rules!``).
* ``crates/<crate>/src/<path>.rs:N[-M]``: the file must exist and the range
  must be inside it.
* In any table whose header has a ``classification`` column, every data row's
  cell in that column must be exactly one of the four classifications.
* In ``docs/qpdf-route-matrix/*.txt`` (the caller-tracker symbol manifests),
  every non-comment line that names a ``crates/<crate>/src/<path>.rs::Sym``
  symbol must resolve exactly like the backticked form above, so a stale
  manifest entry fails the same way a stale citation does.

The pinned qpdf tree is optional at the call site: ``--qpdf-root`` names it,
otherwise ``scripts/fetch-qpdf-source.sh --print-path`` is consulted, and
``--no-qpdf`` skips the qpdf range checks entirely (syntax is still validated)
for environments that deliberately do not fetch the source, such as CI.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass, field
from pathlib import Path
import re
import subprocess
import sys


CLASSIFICATIONS = frozenset({"canonical", "bridge", "mixed", "unknown"})

QPDF_CITATION_RE = re.compile(
    r"`((?:libqpdf|include/qpdf|qpdf)/[A-Za-z0-9_./+-]+\.(?:cc|hh|h))"
    r":(\d+(?:-\d+)?(?:,\d+(?:-\d+)?)*)`"
)
FLPDF_SYMBOL_RE = re.compile(
    r"`(crates/[A-Za-z0-9_./-]+\.rs)::([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*)`"
)
FLPDF_RANGE_RE = re.compile(r"`(crates/[A-Za-z0-9_./-]+\.rs):(\d+(?:-\d+)?)`")
MANIFEST_SYMBOL_RE = re.compile(
    r"^\s*(crates/[A-Za-z0-9_./-]+\.rs)::([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*)\s*(?:#.*)?$"
)
DECLARATION_KEYWORDS = r"(?:fn|struct|enum|trait|type|const|static|mod|macro_rules!)"


@dataclass
class Report:
    errors: list[str] = field(default_factory=list)
    qpdf_citations: int = 0
    flpdf_citations: int = 0
    rows: int = 0

    def error(self, path: Path, line_number: int, message: str) -> None:
        self.errors.append(f"{path}:{line_number}: {message}")


def _parse_ranges(spec: str) -> list[tuple[int, int]]:
    ranges: list[tuple[int, int]] = []
    for part in spec.split(","):
        if "-" in part:
            start_text, end_text = part.split("-", 1)
            ranges.append((int(start_text), int(end_text)))
        else:
            value = int(part)
            ranges.append((value, value))
    return ranges


def _line_count(path: Path) -> int:
    with path.open("rb") as handle:
        return sum(1 for _ in handle)


class Checker:
    def __init__(self, root: Path, qpdf_root: Path | None, report: Report) -> None:
        self.root = root
        self.qpdf_root = qpdf_root
        self.report = report
        self._line_counts: dict[Path, int] = {}
        self._file_texts: dict[Path, str] = {}

    def _lines(self, path: Path) -> int:
        if path not in self._line_counts:
            self._line_counts[path] = _line_count(path)
        return self._line_counts[path]

    def _text(self, path: Path) -> str:
        if path not in self._file_texts:
            self._file_texts[path] = path.read_text(encoding="utf-8", errors="replace")
        return self._file_texts[path]

    def _check_ranges(
        self,
        doc: Path,
        line_number: int,
        display: str,
        target: Path | None,
        spec: str,
    ) -> None:
        ranges = _parse_ranges(spec)
        for start, end in ranges:
            if start < 1 or end < start:
                self.report.error(doc, line_number, f"`{display}`: invalid line range")
                return
        if target is None:
            return
        if not target.is_file():
            self.report.error(doc, line_number, f"`{display}`: file not found")
            return
        total = self._lines(target)
        for start, end in ranges:
            if end > total:
                self.report.error(
                    doc,
                    line_number,
                    f"`{display}`: range {start}-{end} exceeds {total} lines",
                )

    def check_qpdf_citation(self, doc: Path, line_number: int, match: re.Match[str]) -> None:
        self.report.qpdf_citations += 1
        relative, spec = match.group(1), match.group(2)
        target = self.qpdf_root / relative if self.qpdf_root is not None else None
        self._check_ranges(doc, line_number, f"{relative}:{spec}", target, spec)

    def check_flpdf_symbol(self, doc: Path, line_number: int, match: re.Match[str]) -> None:
        self.report.flpdf_citations += 1
        relative, symbol = match.group(1), match.group(2)
        target = self.root / relative
        if not target.is_file():
            self.report.error(doc, line_number, f"`{relative}::{symbol}`: file not found")
            return
        leaf = symbol.rsplit("::", 1)[-1]
        escaped = re.escape(leaf)
        # Item declarations (`fn x`, `struct X`, …), struct fields
        # (`pub(crate) x: T,`), and enum variants (`X,` / `X(` / `X {`) all
        # count as a declaration of the leaf: the route matrix tracks fields
        # (`legacy_resolution_state_synced`) and variants as routes too.
        pattern = re.compile(
            rf"\b{DECLARATION_KEYWORDS}\s+{escaped}\b"
            rf"|^\s*(?:pub(?:\([a-z]+\))?\s+)?{escaped}\s*:"
            rf"|^\s*{escaped}\s*(?:[,({{]|$)"
            rf"|\blet\s+(?:mut\s+)?{escaped}\b",
            re.MULTILINE,
        )
        if pattern.search(self._text(target)) is None:
            self.report.error(
                doc,
                line_number,
                f"`{relative}::{symbol}`: no declaration of `{leaf}` in {relative}",
            )

    def check_flpdf_range(self, doc: Path, line_number: int, match: re.Match[str]) -> None:
        self.report.flpdf_citations += 1
        relative, spec = match.group(1), match.group(2)
        self._check_ranges(doc, line_number, f"{relative}:{spec}", self.root / relative, spec)

    def check_manifest(self, manifest: Path) -> None:
        for line_number, raw_line in enumerate(
            manifest.read_text(encoding="utf-8").splitlines(), start=1
        ):
            stripped = raw_line.strip()
            if not stripped or stripped.startswith("#"):
                continue
            match = MANIFEST_SYMBOL_RE.match(raw_line)
            if match is None:
                if stripped.split("#", 1)[0].strip().startswith("crates/"):
                    self.report.error(
                        manifest, line_number, "manifest entry is not `crates/<path>.rs::Symbol`"
                    )
                continue
            self.check_flpdf_symbol(manifest, line_number, match)

    def check_document(self, doc: Path) -> None:
        classification_column: int | None = None
        for line_number, raw_line in enumerate(
            doc.read_text(encoding="utf-8").splitlines(), start=1
        ):
            for match in QPDF_CITATION_RE.finditer(raw_line):
                self.check_qpdf_citation(doc, line_number, match)
            for match in FLPDF_SYMBOL_RE.finditer(raw_line):
                self.check_flpdf_symbol(doc, line_number, match)
            for match in FLPDF_RANGE_RE.finditer(raw_line):
                self.check_flpdf_range(doc, line_number, match)

            stripped = raw_line.strip()
            if not stripped.startswith("|"):
                classification_column = None
                continue
            cells = [cell.strip() for cell in stripped.strip("|").split("|")]
            lowered = [cell.lower() for cell in cells]
            if "classification" in lowered:
                classification_column = lowered.index("classification")
                continue
            if classification_column is None:
                continue
            if all(re.fullmatch(r":?-{3,}:?", cell) for cell in cells):
                continue
            self.report.rows += 1
            if classification_column >= len(cells):
                self.report.error(
                    doc, line_number, "row has no classification column"
                )
                continue
            value = cells[classification_column].strip("*` ").lower()
            if value not in CLASSIFICATIONS:
                allowed = ", ".join(sorted(CLASSIFICATIONS))
                self.report.error(
                    doc,
                    line_number,
                    f"classification `{value}` is not one of: {allowed}",
                )


def _default_qpdf_root(root: Path) -> Path | None:
    script = root / "scripts" / "fetch-qpdf-source.sh"
    if not script.is_file():
        return None
    completed = subprocess.run(
        ["bash", str(script), "--print-path"],
        capture_output=True,
        text=True,
        check=False,
    )
    if completed.returncode != 0:
        return None
    candidate = completed.stdout.strip().splitlines()
    return Path(candidate[-1]) if candidate else None


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n", 1)[0])
    parser.add_argument("--check", action="store_true", help="validate and exit non-zero on any error")
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--matrix-dir", type=Path, default=Path("docs/qpdf-route-matrix"))
    parser.add_argument("--qpdf-root", type=Path, default=None)
    parser.add_argument(
        "--no-qpdf",
        action="store_true",
        help="skip qpdf line-range checks (citation syntax is still validated)",
    )
    args = parser.parse_args(argv)

    root = args.root.resolve()
    matrix_dir = root / args.matrix_dir
    if not matrix_dir.is_dir():
        print(f"error: matrix directory not found: {matrix_dir}")
        return 1

    qpdf_root: Path | None
    if args.no_qpdf:
        qpdf_root = None
    else:
        qpdf_root = args.qpdf_root or _default_qpdf_root(root)
        if qpdf_root is None or not (qpdf_root / "libqpdf").is_dir():
            print(
                "error: pinned qpdf source not found; run scripts/fetch-qpdf-source.sh, "
                "pass --qpdf-root, or pass --no-qpdf to skip range checks"
            )
            return 1

    report = Report()
    checker = Checker(root, qpdf_root, report)
    for doc in sorted(matrix_dir.glob("*.md")):
        checker.check_document(doc)
    for manifest in sorted(matrix_dir.glob("*.txt")):
        checker.check_manifest(manifest)

    for error in report.errors:
        print(error)
    if report.errors:
        print(f"FAILED: {len(report.errors)} error(s)")
        return 1
    qpdf_note = "skipped (--no-qpdf)" if qpdf_root is None else f"checked against {qpdf_root}"
    print(
        f"OK: {report.qpdf_citations} qpdf citation(s) {qpdf_note}, "
        f"{report.flpdf_citations} flpdf citation(s), {report.rows} matrix row(s)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
