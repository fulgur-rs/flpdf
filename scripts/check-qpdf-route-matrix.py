#!/usr/bin/env python3
"""Validate citations in docs/qpdf-route-matrix and docs/qpdf-correspondence.md.

The route matrix records, per qpdf responsibility, which flpdf entry points
implement it and how they are classified (canonical / bridge / mixed /
unknown). Every claim there is anchored to a citation, and a citation that
does not resolve is worse than none: it looks verified. This checker makes the
anchors machine-checked.

Checked forms (all inside backticks, anywhere in the document -- tables and
prose alike):

* ``libqpdf/X.cc:N``, ``include/qpdf/X.hh:N-M``, ``qpdf/X.cc:N``, and bare
  basenames such as ``QPDF.cc:N`` (including C sources such as
  ``qpdf-ctest.c:N``), with optional comma-separated extra ranges after one
  path (``libqpdf/X.cc:10-20,45``): the file must exist under the pinned qpdf
  source and every range must satisfy ``1 <= N <= M <= line count``. Bare
  basenames are resolved recursively under ``libqpdf/``, ``include/qpdf/``,
  and ``qpdf/``; zero or multiple matches are errors.
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
* Every Markdown table introduced by an aggregate marker comment,
  ``<!-- route-matrix-aggregate: KIND [key=value ...] -->``, is recomputed
  from the validated classification rows and must agree with them: counts and
  enumerated row ids alike. ``KIND`` is one of ``area-total``,
  ``logical-total``, ``per-file``, ``document-tally`` and ``range-summary``.
  Every ``[a-e]-*.md`` area document must carry exactly one
  ``document-tally``, and -- when the matrix directory has a ``README.md`` --
  the three repository-wide kinds must each appear exactly once, so deleting
  an aggregate table cannot escape the check. An aggregate row that is itself
  consumed as a classification row is an error, because the recomputed view
  would then be counting itself.

``--stats`` prints the classification breakdown that those aggregate tables
are checked against (``--stats-format text|json``) and never changes the exit
code. The aggregate checks read Markdown only, so they run unchanged under
``--no-qpdf``.

The pinned qpdf tree is optional at the call site: ``--qpdf-root`` names it,
otherwise ``scripts/fetch-qpdf-source.sh --print-path`` is consulted, and
``--no-qpdf`` skips the qpdf range checks entirely (syntax is still validated)
for environments that deliberately do not fetch the source, such as CI.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass, field
import json
from pathlib import Path
import re
import subprocess
import sys


CLASSIFICATION_ORDER = ("canonical", "bridge", "mixed", "unknown")
CLASSIFICATIONS = frozenset(CLASSIFICATION_ORDER)

# An aggregate table opts into the recomputation check with a marker comment on
# a line of its own. Header shape alone cannot identify these tables: README's
# area-total and logical-total tables have byte-identical headers over
# different denominators, and the historical snapshot prose repeats the same
# numbers without being a live aggregate.
AGGREGATE_MARKER_RE = re.compile(
    r"^\s*<!--\s*route-matrix-aggregate:\s*([^>]*?)\s*-->\s*$"
)
AGGREGATE_ATTRIBUTE_RE = re.compile(r"^([a-z][a-z0-9-]*)=(\S+)$")
AGGREGATE_KIND_UNITS: dict[str, tuple[str, ...]] = {
    "area-total": ("area-physical",),
    "logical-total": ("logical",),
    "per-file": ("area-physical",),
    "document-tally": ("area-physical",),
    "range-summary": ("physical",),
}
AGGREGATE_KIND_ATTRIBUTES: dict[str, tuple[str, ...]] = {
    "area-total": ("unit",),
    "logical-total": ("unit",),
    "per-file": ("unit",),
    "document-tally": ("unit", "file"),
    "range-summary": ("unit", "detail-table"),
}
# Kinds that describe the whole matrix directory rather than one document.
REPOSITORY_WIDE_KINDS = ("area-total", "logical-total", "per-file")
AREA_DOCUMENT_GLOB = "[a-e]-*.md"

# A classification table's kind follows from the first header cell: the per-area
# route tables start with `#` and the qtest exception detail table with `case`.
TABLE_KINDS = {"#": "area", "case": "detail"}

CLASS_LABEL_HEADERS = frozenset({"分類", "classification"})
COUNT_HEADERS = frozenset({"件数", "count"})
ID_LIST_HEADERS = frozenset({"行", "row ids"})
TOTAL_HEADERS = frozenset({"合計", "行数", "物理行", "total"})
FILE_HEADERS = frozenset({"ファイル", "file"})
RANGE_HEADERS = frozenset({"range", "範囲"})
TOTAL_ROW_PREFIXES = ("合計", "total")
ROW_ID_RE = re.compile(r"[A-Za-z]+-?\d+|\d+(?:/\d+)*")
# A cell may say "no rows" with a dash, an em dash, or a Japanese word; none of
# those contain an ASCII alphanumeric, so one class covers every spelling.
ID_PLACEHOLDER_RE = re.compile(r"[^0-9A-Za-z]+")
COUNT_PREFIX_RE = re.compile(r"(\d+)\s*(.*)$", re.DOTALL)
ENUMERATION_RE = re.compile(r"[（(](.*)[）)]", re.DOTALL)
CASE_RANGE_RE = re.compile(r"(\d+)-(\d+)")
CASE_LIST_RE = re.compile(r"\d+(?:/\d+)*")

QPDF_CITATION_PATH_RE = (
    r"(?:"
    r"(?:libqpdf|include/qpdf|qpdf)/[A-Za-z0-9_./+-]+\.(?:cc|hh|h|c)"
    r"|[A-Za-z0-9_.+-]+\.(?:cc|hh|h|c)"
    r")"
)
QPDF_CITATION_RE = re.compile(
    rf"`({QPDF_CITATION_PATH_RE}):(\d+(?:-\d+)?(?:,\d+(?:-\d+)?)*)`"
)
QPDF_CITATION_TOKEN_RE = re.compile(
    rf"`({QPDF_CITATION_PATH_RE}):([^`\n]*)`"
)
QPDF_CITATION_PLACEHOLDER_RE = re.compile(r"(?:N(?:-M)?|NNN)")
FLPDF_SYMBOL_RE = re.compile(
    r"`(crates/[A-Za-z0-9_./-]+\.rs)::([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*)`"
)
FLPDF_RANGE_RE = re.compile(
    r"`(crates/[A-Za-z0-9_./-]+\.rs):(\d+(?:-\d+)?(?:,\d+(?:-\d+)?)*)`"
)
MANIFEST_SYMBOL_RE = re.compile(
    r"^\s*(crates/[A-Za-z0-9_./-]+\.rs)::([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*)\s*(?:#.*)?$"
)
DECLARATION_KEYWORDS = r"(?:fn|struct|enum|trait|type|const|static|mod|macro_rules!)"


@dataclass(frozen=True)
class ClassificationRow:
    """One data row of a classification table that passed validation.

    ``table_kind`` follows from the table header's first cell (see
    ``TABLE_KINDS``), ``row_id`` is the first cell of the row itself (``A7``,
    ``E-28``, ``0/1``), and ``weight`` is the row's logical case count as
    defined by :func:`classification_row_weight`.
    """

    doc: Path
    line_number: int
    table_index: int
    table_kind: str
    row_id: str
    classification: str
    weight: int


@dataclass
class AggregateTable:
    """A Markdown table introduced by a ``route-matrix-aggregate`` marker."""

    doc: Path
    marker_line: int
    kind: str
    attributes: dict[str, str]
    header_line: int
    header: list[str]
    rows: list[tuple[int, list[str]]]


@dataclass
class Report:
    errors: list[str] = field(default_factory=list)
    qpdf_citations: int = 0
    flpdf_citations: int = 0
    rows: int = 0
    # Rows counted into `rows` but rejected before they could be recorded, so
    # that `rows == recorded weight + rejected weight` stays an invariant the
    # aggregate check can assert against.
    rejected_row_weight: int = 0
    classification_rows: list[ClassificationRow] = field(default_factory=list)
    aggregate_tables: list[AggregateTable] = field(default_factory=list)

    def error(self, path: Path, line_number: int, message: str) -> None:
        self.errors.append(f"{path}:{line_number}: {message}")

    def note(self, location: Path, message: str) -> None:
        self.errors.append(f"{location}: {message}")


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


def mask_rust_source(text: str) -> str:
    """Blank Rust comments and string literals while preserving line layout."""
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


def split_markdown_row(row: str) -> list[str]:
    """Split a Markdown table row without treating an escaped pipe as a cell boundary."""
    stripped = row.strip()
    if stripped.startswith("|"):
        stripped = stripped[1:]
    if stripped.endswith("|"):
        stripped = stripped[:-1]

    cells: list[str] = []
    current: list[str] = []
    escaped = False
    for character in stripped:
        if character == "|" and not escaped:
            cells.append("".join(current).strip())
            current = []
            continue
        current.append(character)
        if character == "\\" and not escaped:
            escaped = True
        else:
            escaped = False
    cells.append("".join(current).strip())
    return cells


def is_markdown_separator(cells: list[str]) -> bool:
    return bool(cells) and all(re.fullmatch(r":?-{3,}:?", cell) for cell in cells)


def classification_row_weight(cells: list[str]) -> int:
    # The qtest exception matrix intentionally combines cases 0 and 1 into
    # one physical row. Keep the aggregate denominator in logical-case units,
    # as recorded by docs/qpdf-route-matrix/README.md and flpdf-335in.
    return 2 if cells and cells[0].strip() == "0/1" else 1


def normalize_cell(cell: str) -> str:
    """Strip Markdown emphasis and code markers from a table cell.

    Uses the same strip set as the classification state machine, so an
    aggregate row id normalizes exactly like a matrix row id.
    """
    return cell.strip("*` \t")


def header_key(cell: str) -> str:
    return normalize_cell(cell).lower()


def row_id_sort_key(row_id: str) -> tuple[int, str, int, int]:
    match = re.fullmatch(r"([A-Za-z]*)-?(\d+)(?:/(\d+))?", row_id)
    if match is None:
        return (1, row_id, 0, 0)
    return (0, match.group(1).upper(), int(match.group(2)), int(match.group(3) or 0))


def format_row_ids(row_ids: set[str]) -> str:
    return ", ".join(sorted(row_ids, key=row_id_sort_key)) or "(none)"


def parse_count(cell: str) -> tuple[int | None, str | None]:
    """Split a count cell into its leading integer and the remaining text."""
    match = COUNT_PREFIX_RE.match(normalize_cell(cell))
    if match is None:
        return None, None
    return int(match.group(1)), match.group(2).strip()


def parse_row_ids(text: str) -> tuple[list[str] | None, str | None]:
    """Parse a comma-separated row id enumeration.

    Returns ``(ids, None)`` on success and ``(None, reason)`` when a token is
    neither a row id nor a "no rows" placeholder. An enumeration that holds
    only placeholders parses as the empty list.
    """
    row_ids: list[str] = []
    for part in re.split(r"[,、]", normalize_cell(text)):
        token = normalize_cell(part)
        if not token:
            continue
        if ROW_ID_RE.fullmatch(token):
            row_ids.append(token)
            continue
        if ID_PLACEHOLDER_RE.fullmatch(token):
            continue
        return None, f"`{token}` is not a row id"
    return row_ids, None


def parse_enumeration(text: str) -> str | None:
    """Extract the parenthesized row id enumeration of a count cell, if any."""
    match = ENUMERATION_RE.fullmatch(text.strip())
    return None if match is None else match.group(1)


def parse_case_spec(text: str) -> tuple[set[int] | None, str | None]:
    """Parse a range bucket spec such as ``0/1, 2-25`` into a set of cases."""
    cases: set[int] = set()
    for part in re.split(r"[,、]", normalize_cell(text)):
        token = normalize_cell(part)
        if not token:
            continue
        span = CASE_RANGE_RE.fullmatch(token)
        if span is not None:
            start, end = int(span.group(1)), int(span.group(2))
            if end < start:
                return None, f"`{token}` is an inverted case range"
            cases.update(range(start, end + 1))
            continue
        if CASE_LIST_RE.fullmatch(token):
            cases.update(int(piece) for piece in token.split("/"))
            continue
        return None, f"`{token}` is not a case range"
    return cases, None


def row_id_cases(row_id: str) -> set[int] | None:
    if not CASE_LIST_RE.fullmatch(row_id):
        return None
    return {int(piece) for piece in row_id.split("/")}


def tally(rows: list[ClassificationRow], *, weighted: bool) -> dict[str, int]:
    counts = {name: 0 for name in CLASSIFICATION_ORDER}
    for row in rows:
        counts[row.classification] += row.weight if weighted else 1
    counts["total"] = sum(counts[name] for name in CLASSIFICATION_ORDER)
    return counts


class Checker:
    def __init__(self, root: Path, qpdf_root: Path | None, report: Report) -> None:
        self.root = root
        self.qpdf_root = qpdf_root
        self.report = report
        self._line_counts: dict[Path, int] = {}
        self._file_texts: dict[Path, str] = {}
        self._qpdf_basename_matches: dict[str, tuple[Path, ...]] = {}

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
        if self.qpdf_root is None:
            target = None
        elif "/" in relative:
            target = self.qpdf_root / relative
        else:
            matches = self._qpdf_basename_matches_for(relative)
            display = f"{relative}:{spec}"
            if not matches:
                self.report.error(
                    doc,
                    line_number,
                    f"`{display}`: qpdf basename not found under {self.qpdf_root}",
                )
                return
            if len(matches) > 1:
                paths = ", ".join(
                    str(path.relative_to(self.qpdf_root)) for path in matches
                )
                self.report.error(
                    doc,
                    line_number,
                    f"`{display}`: qpdf basename is ambiguous; matches {paths}",
                )
                return
            target = matches[0]
        self._check_ranges(doc, line_number, f"{relative}:{spec}", target, spec)

    def _qpdf_basename_matches_for(self, basename: str) -> tuple[Path, ...]:
        matches = self._qpdf_basename_matches.get(basename)
        if matches is not None:
            return matches
        if self.qpdf_root is None:
            return ()
        found: list[Path] = []
        for directory in ("libqpdf", "include/qpdf", "qpdf"):
            source_root = self.qpdf_root / directory
            if source_root.is_dir():
                found.extend(sorted(source_root.rglob(basename)))
        matches = tuple(path for path in found if path.is_file())
        self._qpdf_basename_matches[basename] = matches
        return matches

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
        if pattern.search(mask_rust_source(self._text(target))) is None:
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
        classification_width: int | None = None
        table_index = 0
        table_kind = "other"
        lines = doc.read_text(encoding="utf-8").splitlines()
        for line_number, raw_line in enumerate(
            lines, start=1
        ):
            for match in QPDF_CITATION_TOKEN_RE.finditer(raw_line):
                # The checker documentation itself contains illustrative
                # forms such as `libqpdf/X.cc:N-M`; only numeric-looking
                # tokens are citations that can be validated.
                if QPDF_CITATION_PLACEHOLDER_RE.fullmatch(match.group(2)):
                    continue
                if QPDF_CITATION_RE.fullmatch(match.group(0)):
                    self.check_qpdf_citation(doc, line_number, match)
                else:
                    self.report.error(
                        doc,
                        line_number,
                        f"`{match.group(1)}:{match.group(2)}`: malformed qpdf citation",
                    )
            for match in FLPDF_SYMBOL_RE.finditer(raw_line):
                self.check_flpdf_symbol(doc, line_number, match)
            for match in FLPDF_RANGE_RE.finditer(raw_line):
                self.check_flpdf_range(doc, line_number, match)

            stripped = raw_line.strip()
            if not stripped.startswith("|"):
                continue
            cells = split_markdown_row(stripped)
            lowered = [cell.lower() for cell in cells]
            if "classification" in lowered:
                classification_column = lowered.index("classification")
                classification_width = len(cells)
                table_index += 1
                table_kind = TABLE_KINDS.get(header_key(cells[0]), "other")
                continue
            if classification_column is None:
                continue
            if is_markdown_separator(cells):
                continue
            next_line = lines[line_number] if line_number < len(lines) else ""
            next_cells = (
                split_markdown_row(next_line.strip())
                if next_line.strip().startswith("|")
                else []
            )
            # Only a proven header boundary ends the table: a header row is
            # followed by a Markdown separator. A width mismatch without that
            # boundary is a malformed data row, and silently ending the table
            # there would understate the count exactly like the prose reset
            # this check replaced.
            if (
                next_cells
                and is_markdown_separator(next_cells)
                and "classification" not in lowered
            ):
                classification_column = None
                classification_width = None
                continue
            if len(cells) != classification_width:
                self.report.error(
                    doc,
                    line_number,
                    f"classification row has {len(cells)} cell(s), "
                    f"expected {classification_width}",
                )
                continue
            weight = classification_row_weight(cells)
            self.report.rows += weight
            if classification_column >= len(cells):
                self.report.rejected_row_weight += weight
                self.report.error(
                    doc, line_number, "row has no classification column"
                )
                continue
            value = cells[classification_column].strip("*` ").lower()
            if value not in CLASSIFICATIONS:
                self.report.rejected_row_weight += weight
                allowed = ", ".join(sorted(CLASSIFICATIONS))
                self.report.error(
                    doc,
                    line_number,
                    f"classification `{value}` is not one of: {allowed}",
                )
                continue
            # The aggregate tables are recomputed from these rows, so record
            # every row the checks above accepted.
            self.report.classification_rows.append(
                ClassificationRow(
                    doc=doc,
                    line_number=line_number,
                    table_index=table_index,
                    table_kind=table_kind,
                    row_id=normalize_cell(cells[0]),
                    classification=value,
                    weight=weight,
                )
            )


    # ------------------------------------------------------------------
    # Aggregate tables
    #
    # The classification cells are the source of truth; the aggregate tables
    # restate them. Restating by hand is what let four independent copies of
    # the same breakdown drift apart, so every table that opts in with a
    # `route-matrix-aggregate` marker is recomputed from the rows recorded by
    # check_document and must agree with them.
    # ------------------------------------------------------------------

    def collect_aggregate_tables(self, doc: Path) -> None:
        """Record every ``route-matrix-aggregate`` table in ``doc``."""
        lines = self._text(doc).splitlines()
        index = 0
        while index < len(lines):
            match = AGGREGATE_MARKER_RE.match(lines[index])
            if match is None:
                index += 1
                continue
            marker_line = index + 1
            kind, attributes = self._parse_aggregate_marker(
                doc, marker_line, match.group(1)
            )
            cursor = index + 1
            while cursor < len(lines) and not lines[cursor].strip():
                cursor += 1
            header = (
                split_markdown_row(lines[cursor].strip())
                if cursor < len(lines) and lines[cursor].strip().startswith("|")
                else []
            )
            separator = (
                split_markdown_row(lines[cursor + 1].strip())
                if cursor + 1 < len(lines)
                and lines[cursor + 1].strip().startswith("|")
                else []
            )
            if not header or not separator or not is_markdown_separator(separator):
                self.report.error(
                    doc,
                    marker_line,
                    f"`route-matrix-aggregate: {kind or '?'}` marker is not followed "
                    "by a Markdown table",
                )
                index += 1
                continue
            rows: list[tuple[int, list[str]]] = []
            row_index = cursor + 2
            while row_index < len(lines) and lines[row_index].strip().startswith("|"):
                cells = split_markdown_row(lines[row_index].strip())
                if is_markdown_separator(cells):
                    break
                rows.append((row_index + 1, cells))
                row_index += 1
            if kind is not None:
                self.report.aggregate_tables.append(
                    AggregateTable(
                        doc=doc,
                        marker_line=marker_line,
                        kind=kind,
                        attributes=attributes,
                        header_line=cursor + 1,
                        header=header,
                        rows=rows,
                    )
                )
            index = max(row_index, index + 1)

    def _parse_aggregate_marker(
        self, doc: Path, line_number: int, payload: str
    ) -> tuple[str | None, dict[str, str]]:
        tokens = payload.split()
        if not tokens:
            self.report.error(
                doc, line_number, "`route-matrix-aggregate` marker names no kind"
            )
            return None, {}
        kind = tokens[0]
        if kind not in AGGREGATE_KIND_UNITS:
            allowed = ", ".join(sorted(AGGREGATE_KIND_UNITS))
            self.report.error(
                doc,
                line_number,
                f"`route-matrix-aggregate: {kind}` is not one of: {allowed}",
            )
            return None, {}
        attributes: dict[str, str] = {}
        for token in tokens[1:]:
            attribute = AGGREGATE_ATTRIBUTE_RE.fullmatch(token)
            if attribute is None:
                self.report.error(
                    doc,
                    line_number,
                    f"`route-matrix-aggregate: {kind}`: `{token}` is not a "
                    "`key=value` attribute",
                )
                continue
            attributes[attribute.group(1)] = attribute.group(2)
        expected = AGGREGATE_KIND_ATTRIBUTES[kind]
        for name in expected:
            if name not in attributes:
                self.report.error(
                    doc,
                    line_number,
                    f"`route-matrix-aggregate: {kind}` is missing the `{name}` attribute",
                )
        for name in sorted(attributes):
            if name not in expected:
                self.report.error(
                    doc,
                    line_number,
                    f"`route-matrix-aggregate: {kind}`: unknown attribute `{name}`",
                )
        unit = attributes.get("unit")
        if unit is not None and unit not in AGGREGATE_KIND_UNITS[kind]:
            allowed = ", ".join(AGGREGATE_KIND_UNITS[kind])
            self.report.error(
                doc,
                line_number,
                f"`route-matrix-aggregate: {kind}`: unit `{unit}` is not one of: {allowed}",
            )
        return kind, attributes

    def check_aggregate_tables(self, matrix_dir: Path) -> None:
        self._check_recording_invariant(matrix_dir)
        self._check_classification_row_id_collisions()
        self._check_aggregate_inventory(matrix_dir)
        self._check_aggregate_row_collisions()
        handlers = {
            "area-total": self._check_area_total,
            "logical-total": self._check_logical_total,
            "per-file": self._check_per_file,
            "document-tally": self._check_document_tally,
            "range-summary": self._check_range_summary,
        }
        for table in self.report.aggregate_tables:
            handlers[table.kind](table, matrix_dir)

    def _check_recording_invariant(self, matrix_dir: Path) -> None:
        # `rows` is the counter the checker has always printed. Recomputing it
        # from the recorded rows proves the recording point still sees exactly
        # the rows the classification state machine counts.
        recorded = sum(row.weight for row in self.report.classification_rows)
        total = recorded + self.report.rejected_row_weight
        if total != self.report.rows:
            self.report.note(
                matrix_dir,
                f"internal error: recorded {total} logical row(s) but counted "
                f"{self.report.rows}",
            )

    def _area_documents(self, matrix_dir: Path) -> set[Path]:
        return set(matrix_dir.glob(AREA_DOCUMENT_GLOB))

    def _area_rows(self, matrix_dir: Path) -> list[ClassificationRow]:
        documents = self._area_documents(matrix_dir)
        return [
            row
            for row in self.report.classification_rows
            if row.table_kind == "area" and row.doc in documents
        ]

    def _document_rows(self, doc: Path, table_kind: str) -> list[ClassificationRow]:
        return [
            row
            for row in self.report.classification_rows
            if row.doc == doc and row.table_kind == table_kind
        ]

    def _tables_of(self, kind: str, doc: Path | None = None) -> list[AggregateTable]:
        return [
            table
            for table in self.report.aggregate_tables
            if table.kind == kind and (doc is None or table.doc == doc)
        ]

    def _check_aggregate_inventory(self, matrix_dir: Path) -> None:
        # Derive what must exist from the tree instead of hardcoding it, so a
        # deleted aggregate table fails rather than silently opting out.
        for document in sorted(self._area_documents(matrix_dir)):
            found = len(self._tables_of("document-tally", document))
            if found != 1:
                self.report.note(
                    document,
                    "expected exactly 1 `route-matrix-aggregate: document-tally` "
                    f"table, found {found}",
                )
        readme = matrix_dir / "README.md"
        readme_exists = readme.is_file()
        for kind in REPOSITORY_WIDE_KINDS:
            found_tables = self._tables_of(kind)
            if readme_exists:
                if len(found_tables) != 1:
                    self.report.note(
                        matrix_dir,
                        f"expected exactly 1 `route-matrix-aggregate: {kind}` "
                        f"table, found {len(found_tables)}",
                    )
                for table in found_tables:
                    if table.doc != readme:
                        self.report.error(
                            table.doc,
                            table.marker_line,
                            f"`route-matrix-aggregate: {kind}` must live in "
                            f"`README.md`, not `{table.doc.name}`",
                        )
            else:
                # No README.md to hold these tables. A matrix directory that
                # also has none of this kind anywhere has nothing to check
                # (this is the state most test fixtures are in). One that
                # does have `kind` tables elsewhere is the bug this branch
                # exists for: those tables have no legitimate home to be
                # placed in, so flag every one of them.
                for table in found_tables:
                    self.report.error(
                        table.doc,
                        table.marker_line,
                        f"`route-matrix-aggregate: {kind}` must live in "
                        f"`README.md`, but `{matrix_dir}` has no `README.md`",
                    )
        detail_documents = {
            row.doc
            for row in self.report.classification_rows
            if row.table_kind == "detail"
        }
        for document in sorted(detail_documents):
            found = len(self._tables_of("range-summary", document))
            if found != 1:
                self.report.note(
                    document,
                    "expected exactly 1 `route-matrix-aggregate: range-summary` table "
                    f"for its `case` detail table, found {found}",
                )
        for table in self._tables_of("range-summary"):
            if table.doc not in detail_documents:
                self.report.error(
                    table.doc,
                    table.marker_line,
                    "`route-matrix-aggregate: range-summary` has no `case` detail "
                    "table to summarize",
                )

    def _check_aggregate_row_collisions(self) -> None:
        recorded = {
            (row.doc, row.line_number) for row in self.report.classification_rows
        }
        for table in self.report.aggregate_tables:
            for line_number, _ in table.rows:
                if (table.doc, line_number) in recorded:
                    self.report.error(
                        table.doc,
                        line_number,
                        f"`route-matrix-aggregate: {table.kind}` row is also counted "
                        "as a classification row; the aggregate table must not sit "
                        "inside a classification table's scope",
                    )

    def _check_classification_row_id_collisions(self) -> None:
        # Keyed by (doc, table_kind, row_id) rather than including
        # table_index: a document that splits its classification rows across
        # more than one table of the same kind (e.g. a table per section)
        # must still keep row ids unique across all of them, not just within
        # a single physical table.
        rows_by_table: dict[tuple[Path, str, str], list[ClassificationRow]] = {}
        for row in self.report.classification_rows:
            key = (row.doc, row.table_kind, row.row_id)
            rows_by_table.setdefault(key, []).append(row)
        for rows in rows_by_table.values():
            if len(rows) < 2:
                continue
            first = rows[0]
            for duplicate in rows[1:]:
                self.report.error(
                    duplicate.doc,
                    duplicate.line_number,
                    f"classification {first.table_kind} row id `{first.row_id}` "
                    "appears more than once in the same table",
                )

    def _classification_columns(self, table: AggregateTable) -> dict[str, int] | None:
        keys = [header_key(cell) for cell in table.header]
        columns: dict[str, int] = {}
        for name in CLASSIFICATION_ORDER:
            found = [index for index, key in enumerate(keys) if key == name]
            if len(found) != 1:
                self.report.error(
                    table.doc,
                    table.header_line,
                    f"`route-matrix-aggregate: {table.kind}` header must have exactly "
                    f"one `{name}` column, found {len(found)}",
                )
                return None
            columns[name] = found[0]
        return columns

    def _named_column(
        self, table: AggregateTable, label: str, aliases: frozenset[str]
    ) -> int | None:
        keys = [header_key(cell) for cell in table.header]
        found = [index for index, key in enumerate(keys) if key in aliases]
        if len(found) != 1:
            self.report.error(
                table.doc,
                table.header_line,
                f"`route-matrix-aggregate: {table.kind}` header must have exactly one "
                f"{label} column, found {len(found)}",
            )
            return None
        return found[0]

    def _compare_row_ids(
        self,
        table: AggregateTable,
        line_number: int,
        context: str,
        declared: list[str],
        actual: set[str],
    ) -> None:
        unique = set(declared)
        if len(declared) != len(unique):
            duplicates = {value for value in unique if declared.count(value) > 1}
            self.report.error(
                table.doc,
                line_number,
                f"{context}: lists {format_row_ids(duplicates)} more than once",
            )
        if unique == actual:
            return
        details = []
        missing = actual - unique
        if missing:
            details.append(f"missing {format_row_ids(missing)}")
        unexpected = unique - actual
        if unexpected:
            details.append(f"unexpected {format_row_ids(unexpected)}")
        self.report.error(
            table.doc,
            line_number,
            f"{context}: enumerated row ids do not match the matrix; "
            + "; ".join(details),
        )

    def _compare_counts(
        self,
        table: AggregateTable,
        line_number: int,
        context: str,
        cells: list[str],
        columns: dict[str, int],
        actual: dict[str, int],
        *,
        actual_ids: dict[str, set[str]] | None,
        require_enumeration: bool,
    ) -> None:
        for name in CLASSIFICATION_ORDER:
            index = columns[name]
            if index >= len(cells):
                self.report.error(
                    table.doc, line_number, f"{context}: row has no `{name}` cell"
                )
                continue
            declared, remainder = parse_count(cells[index])
            if declared is None:
                self.report.error(
                    table.doc,
                    line_number,
                    f"{context}: `{name}` cell "
                    f"`{normalize_cell(cells[index])}` does not start with a count",
                )
                continue
            if declared != actual[name]:
                self.report.error(
                    table.doc,
                    line_number,
                    f"{context}: `{name}` says {declared} but the matrix has "
                    f"{actual[name]}",
                )
            # A zero cell carries no enumeration to compare, which also leaves
            # room for explanatory prose. If it does contain a parseable row
            # id, reject it rather than letting a zero-count escape route hide
            # an aggregate classification.
            if actual_ids is None:
                continue
            enumeration = parse_enumeration(remainder or "")
            if enumeration is None:
                # A zero count has nothing to enumerate, so a bare `0` with
                # no parenthesized text (not even a `(—)` placeholder) is a
                # legitimate way to write it -- only a non-zero count that
                # omits its enumeration is an error under `require_enumeration`.
                if require_enumeration and declared != 0:
                    self.report.error(
                        table.doc,
                        line_number,
                        f"{context}: `{name}` says {declared} but enumerates no row ids",
                    )
                continue
            row_ids, reason = parse_row_ids(enumeration)
            if row_ids is None:
                self.report.error(
                    table.doc, line_number, f"{context}: `{name}`: {reason}"
                )
                continue
            if declared == 0:
                if row_ids:
                    self.report.error(
                        table.doc,
                        line_number,
                        f"{context}: `{name}` is zero but enumerates row ids "
                        f"{format_row_ids(set(row_ids))}",
                    )
                continue
            if not row_ids:
                self.report.error(
                    table.doc,
                    line_number,
                    f"{context}: `{name}` says {declared} but enumerates no row ids",
                )
                continue
            self._compare_row_ids(
                table, line_number, f"{context}: `{name}`", row_ids, actual_ids[name]
            )

    def _compare_total(
        self,
        table: AggregateTable,
        line_number: int,
        context: str,
        cells: list[str],
        column: int,
        expected: int,
    ) -> None:
        if column >= len(cells):
            self.report.error(
                table.doc, line_number, f"{context}: row has no total cell"
            )
            return
        declared, _ = parse_count(cells[column])
        if declared is None:
            self.report.error(
                table.doc,
                line_number,
                f"{context}: total cell `{normalize_cell(cells[column])}` does not "
                "start with a count",
            )
            return
        if declared != expected:
            self.report.error(
                table.doc,
                line_number,
                f"{context}: total says {declared} but the matrix has {expected}",
            )

    def _check_single_total(
        self, table: AggregateTable, actual: dict[str, int], context: str
    ) -> None:
        columns = self._classification_columns(table)
        total_column = self._named_column(table, "total", TOTAL_HEADERS)
        if columns is None or total_column is None:
            return
        if len(table.rows) != 1:
            self.report.error(
                table.doc,
                table.header_line,
                f"`route-matrix-aggregate: {table.kind}` must have exactly one data "
                f"row, found {len(table.rows)}",
            )
            return
        line_number, cells = table.rows[0]
        self._compare_counts(
            table,
            line_number,
            context,
            cells,
            columns,
            actual,
            actual_ids=None,
            require_enumeration=False,
        )
        self._compare_total(
            table, line_number, context, cells, total_column, actual["total"]
        )

    def _check_area_total(self, table: AggregateTable, matrix_dir: Path) -> None:
        actual = tally(self._area_rows(matrix_dir), weighted=False)
        self._check_single_total(table, actual, "area-total")

    def _check_logical_total(self, table: AggregateTable, matrix_dir: Path) -> None:
        # Weighted over every classification row the checker validated, which
        # `_check_recording_invariant` ties back to the printed row counter.
        actual = tally(self.report.classification_rows, weighted=True)
        self._check_single_total(table, actual, "logical-total")

    def _check_per_file(self, table: AggregateTable, matrix_dir: Path) -> None:
        columns = self._classification_columns(table)
        total_column = self._named_column(table, "row count", TOTAL_HEADERS)
        file_column = self._named_column(table, "file", FILE_HEADERS)
        if columns is None or total_column is None or file_column is None:
            return
        listed: list[str] = []
        for line_number, cells in table.rows:
            if file_column >= len(cells):
                self.report.error(
                    table.doc, line_number, "per-file: row has no file cell"
                )
                continue
            link = re.search(r"\]\(([^)\s]+)\)", cells[file_column])
            if link is None:
                self.report.error(
                    table.doc,
                    line_number,
                    f"per-file: `{normalize_cell(cells[file_column])}` is not a "
                    "`[text](file.md)` link",
                )
                continue
            name = link.group(1)
            listed.append(name)
            target = matrix_dir / name
            if not target.is_file():
                self.report.error(
                    table.doc,
                    line_number,
                    f"per-file: `{name}` does not exist under {matrix_dir}",
                )
                continue
            if target not in self._area_documents(matrix_dir):
                self.report.error(
                    table.doc,
                    line_number,
                    f"per-file: `{name}` is not an area document",
                )
                continue
            actual = tally(self._document_rows(target, "area"), weighted=False)
            context = f"per-file `{name}`"
            self._compare_counts(
                table,
                line_number,
                context,
                cells,
                columns,
                actual,
                actual_ids=None,
                require_enumeration=False,
            )
            self._compare_total(
                table, line_number, context, cells, total_column, actual["total"]
            )
        for name in sorted(
            document.name
            for document in self._area_documents(matrix_dir)
            if document.name not in listed
        ):
            self.report.error(
                table.doc, table.header_line, f"per-file: `{name}` is not listed"
            )
        for name in sorted({name for name in listed if listed.count(name) > 1}):
            self.report.error(
                table.doc,
                table.header_line,
                f"per-file: `{name}` is listed {listed.count(name)} times",
            )

    def _check_document_tally(self, table: AggregateTable, matrix_dir: Path) -> None:
        label_column = self._named_column(table, "classification", CLASS_LABEL_HEADERS)
        count_column = self._named_column(table, "count", COUNT_HEADERS)
        ids_column = self._named_column(table, "row id", ID_LIST_HEADERS)
        if label_column is None or count_column is None or ids_column is None:
            return
        declared_file = table.attributes.get("file")
        if declared_file is not None and declared_file != table.doc.name:
            self.report.error(
                table.doc,
                table.marker_line,
                "`route-matrix-aggregate: document-tally` declares "
                f"file=`{declared_file}` but lives in `{table.doc.name}`",
            )
        rows = self._document_rows(table.doc, "area")
        actual = tally(rows, weighted=False)
        actual_ids = {
            name: {row.row_id for row in rows if row.classification == name}
            for name in CLASSIFICATION_ORDER
        }
        seen: dict[str, int] = {}
        for line_number, cells in table.rows:
            if max(label_column, count_column, ids_column) >= len(cells):
                self.report.error(
                    table.doc, line_number, "document-tally: row is missing cells"
                )
                continue
            name = header_key(cells[label_column])
            if name not in CLASSIFICATIONS:
                allowed = ", ".join(CLASSIFICATION_ORDER)
                self.report.error(
                    table.doc,
                    line_number,
                    f"document-tally: `{name}` is not one of: {allowed}",
                )
                continue
            seen[name] = seen.get(name, 0) + 1
            context = f"document-tally `{name}`"
            declared, _ = parse_count(cells[count_column])
            if declared is None:
                self.report.error(
                    table.doc,
                    line_number,
                    f"{context}: count `{normalize_cell(cells[count_column])}` does "
                    "not start with a number",
                )
                continue
            if declared != actual[name]:
                self.report.error(
                    table.doc,
                    line_number,
                    f"{context}: says {declared} but the matrix has {actual[name]}",
                )
            if declared == 0:
                # A zero-count cell may carry free-form prose explaining the
                # count (e.g. "none (A2 already resolved)"), so this does not
                # require the whole cell to parse as `parse_row_ids` would.
                # But a comma-separated list that mixes real-looking row ids
                # with other tokens -- `A1, note` -- must not let the hidden
                # `A1` escape unnoticed just because the list as a whole
                # fails to parse cleanly.
                hidden_ids = [
                    token
                    for part in re.split(r"[,、]", normalize_cell(cells[ids_column]))
                    if (token := normalize_cell(part)) and ROW_ID_RE.fullmatch(token)
                ]
                if hidden_ids:
                    self.report.error(
                        table.doc,
                        line_number,
                        f"{context}: says 0 but enumerates row ids "
                        f"{format_row_ids(set(hidden_ids))}",
                    )
                continue
            row_ids, reason = parse_row_ids(cells[ids_column])
            if row_ids is None:
                self.report.error(table.doc, line_number, f"{context}: {reason}")
                continue
            if not row_ids:
                self.report.error(
                    table.doc,
                    line_number,
                    f"{context}: says {declared} but enumerates no row ids",
                )
                continue
            self._compare_row_ids(
                table, line_number, context, row_ids, actual_ids[name]
            )
        for name in CLASSIFICATION_ORDER:
            if seen.get(name, 0) != 1:
                self.report.error(
                    table.doc,
                    table.header_line,
                    f"document-tally must list `{name}` exactly once, found "
                    f"{seen.get(name, 0)}",
                )

    def _check_range_summary(self, table: AggregateTable, matrix_dir: Path) -> None:
        columns = self._classification_columns(table)
        total_column = self._named_column(table, "physical row count", TOTAL_HEADERS)
        range_column = self._named_column(table, "range", RANGE_HEADERS)
        if columns is None or total_column is None or range_column is None:
            return
        detail_table = table.attributes.get("detail-table")
        if detail_table is not None:
            area_ids = {
                row.row_id.upper() for row in self._document_rows(table.doc, "area")
            }
            if detail_table.upper() not in area_ids:
                self.report.error(
                    table.doc,
                    table.marker_line,
                    "`route-matrix-aggregate: range-summary` names "
                    f"detail-table=`{detail_table}`, which is not a row of "
                    f"`{table.doc.name}`",
                )
        rows = self._document_rows(table.doc, "detail")
        by_row_id = {row.row_id: row for row in rows}
        cases_of: dict[str, set[int]] = {}
        case_owners: dict[int, str] = {}
        for row in rows:
            cases = row_id_cases(row.row_id)
            if cases is None:
                self.report.error(
                    table.doc,
                    row.line_number,
                    f"range-summary: detail row `{row.row_id}` is not a case number, "
                    "so it cannot be assigned to a range bucket",
                )
                continue
            # `row_id_cases` folds the row id's `/`-separated parts into a
            # set, which silently drops a repeated case number within the
            # same row id (e.g. `3/3` collapses to `{3}`) before it ever
            # reaches the cross-row overlap check below. Catch that here,
            # without skipping the overlap check itself: the deduplicated
            # `cases` set is still meaningful, and a row can be both
            # internally repetitive and overlap an earlier row.
            raw_parts = row.row_id.split("/")
            if len(raw_parts) != len(cases):
                self.report.error(
                    table.doc,
                    row.line_number,
                    f"range-summary: detail row `{row.row_id}` repeats the same "
                    "case number within itself",
                )
            overlap_owners: dict[int, str] = {}
            for case in cases:
                previous = case_owners.get(case)
                if previous is not None and previous != row.row_id:
                    overlap_owners[case] = previous
                else:
                    case_owners[case] = row.row_id
            if overlap_owners:
                owners = ", ".join(
                    f"{owner} ({case})"
                    for case, owner in sorted(overlap_owners.items())
                )
                self.report.error(
                    table.doc,
                    row.line_number,
                    f"range-summary: detail row `{row.row_id}` case set overlaps "
                    f"earlier row(s) {owners}",
                )
            cases_of[row.row_id] = cases

        buckets: list[tuple[int, list[str], set[int]]] = []
        totals: dict[str, tuple[int, list[str]]] = {}
        for line_number, cells in table.rows:
            if range_column >= len(cells):
                self.report.error(
                    table.doc, line_number, "range-summary: row has no range cell"
                )
                continue
            label = normalize_cell(cells[range_column])
            if label.lower().startswith(TOTAL_ROW_PREFIXES):
                lowered = label.lower()
                if "物理" in label or "physical" in lowered:
                    key = "physical"
                elif "論理" in label or "logical" in lowered:
                    key = "logical"
                else:
                    self.report.error(
                        table.doc,
                        line_number,
                        f"range-summary: total row `{label}` must say whether it "
                        "counts 物理行 (physical rows) or 論理ケース (logical cases)",
                    )
                    continue
                if key in totals:
                    self.report.error(
                        table.doc,
                        line_number,
                        f"range-summary: duplicate {key} total row",
                    )
                    continue
                totals[key] = (line_number, cells)
                continue
            cases, reason = parse_case_spec(label)
            if cases is None:
                self.report.error(table.doc, line_number, f"range-summary: {reason}")
                continue
            buckets.append((line_number, cells, cases))

        for first in range(len(buckets)):
            for second in range(first + 1, len(buckets)):
                overlap = buckets[first][2] & buckets[second][2]
                if overlap:
                    self.report.error(
                        table.doc,
                        buckets[second][0],
                        "range-summary: bucket overlaps an earlier bucket on case(s) "
                        + ", ".join(str(case) for case in sorted(overlap)),
                    )
        covered: set[int] = set()
        for _, _, cases in buckets:
            covered |= cases
        present: set[int] = set()
        for cases in cases_of.values():
            present |= cases
        uncovered = present - covered
        if uncovered:
            self.report.error(
                table.doc,
                table.header_line,
                "range-summary: case(s) "
                + ", ".join(str(case) for case in sorted(uncovered))
                + " are in the detail table but in no range bucket",
            )
        surplus = covered - present
        if surplus:
            self.report.error(
                table.doc,
                table.header_line,
                "range-summary: range bucket(s) cover case(s) "
                + ", ".join(str(case) for case in sorted(surplus))
                + " that the detail table does not have",
            )

        members: dict[int, list[str]] = {}
        for row_id, cases in sorted(
            cases_of.items(), key=lambda item: row_id_sort_key(item[0])
        ):
            holders = [
                index for index, bucket in enumerate(buckets) if bucket[2] & cases
            ]
            if len(holders) != 1:
                self.report.error(
                    table.doc,
                    table.header_line,
                    f"range-summary: detail row `{row_id}` is covered by "
                    f"{len(holders)} range bucket(s), expected exactly 1",
                )
                continue
            if not cases <= buckets[holders[0]][2]:
                self.report.error(
                    table.doc,
                    buckets[holders[0]][0],
                    f"range-summary: detail row `{row_id}` is only partly covered by "
                    "this range bucket",
                )
                continue
            members.setdefault(holders[0], []).append(row_id)

        assigned = sum(len(member_ids) for member_ids in members.values())
        if buckets and assigned != len(rows):
            # The buckets must partition the detail rows, not merely cover the
            # case numbers: a duplicated row id covers its cases while leaving
            # one physical row unassigned.
            self.report.error(
                table.doc,
                table.header_line,
                f"range-summary: the range buckets hold {assigned} of the "
                f"{len(rows)} detail row(s)",
            )

        for index, (line_number, cells, _) in enumerate(buckets):
            bucket_rows = [by_row_id[row_id] for row_id in members.get(index, [])]
            actual = tally(bucket_rows, weighted=False)
            actual_ids = {
                name: {row.row_id for row in bucket_rows if row.classification == name}
                for name in CLASSIFICATION_ORDER
            }
            context = f"range-summary bucket `{normalize_cell(cells[range_column])}`"
            self._compare_counts(
                table,
                line_number,
                context,
                cells,
                columns,
                actual,
                actual_ids=actual_ids,
                require_enumeration=True,
            )
            self._compare_total(
                table, line_number, context, cells, total_column, actual["total"]
            )

        for key, weighted, label in (
            ("physical", False, "合計（物理行）"),
            ("logical", True, "合計（論理ケース）"),
        ):
            entry = totals.get(key)
            if entry is None:
                self.report.error(
                    table.doc,
                    table.header_line,
                    f"range-summary: the `{label}` total row is missing",
                )
                continue
            line_number, cells = entry
            actual = tally(rows, weighted=weighted)
            context = f"range-summary `{label}`"
            self._compare_counts(
                table,
                line_number,
                context,
                cells,
                columns,
                actual,
                actual_ids=None,
                require_enumeration=False,
            )
            self._compare_total(
                table, line_number, context, cells, total_column, actual["total"]
            )


def build_stats(rows: list[ClassificationRow]) -> dict[str, object]:
    """Summarize the recorded classification rows for ``--stats``."""
    documents: dict[str, dict[str, dict[str, dict[str, int]]]] = {}
    for name in sorted({row.doc.name for row in rows}):
        document_rows = [row for row in rows if row.doc.name == name]
        entry: dict[str, dict[str, dict[str, int]]] = {}
        for table_kind in ("area", "detail", "other"):
            subset = [row for row in document_rows if row.table_kind == table_kind]
            if not subset:
                continue
            entry[table_kind] = {
                "physical": tally(subset, weighted=False),
                "logical": tally(subset, weighted=True),
            }
        documents[name] = entry
    return {
        "logical_total": tally(rows, weighted=True),
        "physical_total": tally(rows, weighted=False),
        "area_physical_total": tally(
            [row for row in rows if row.table_kind == "area"], weighted=False
        ),
        "documents": documents,
    }


def format_counts(counts: dict[str, int]) -> str:
    parts = " / ".join(f"{name} {counts[name]}" for name in CLASSIFICATION_ORDER)
    return f"{parts} = {counts['total']}"


def format_stats_text(stats: dict[str, object]) -> list[str]:
    lines = ["classification stats:"]
    for key, label in (
        ("logical_total", "logical total"),
        ("physical_total", "physical total"),
        ("area_physical_total", "area physical total"),
    ):
        lines.append(f"  {label}: {format_counts(stats[key])}")
    for name, entry in stats["documents"].items():
        lines.append(f"  {name}:")
        for table_kind, values in entry.items():
            lines.append(
                f"    {table_kind} physical: {format_counts(values['physical'])}"
            )
            if values["logical"] != values["physical"]:
                lines.append(
                    f"    {table_kind} logical: {format_counts(values['logical'])}"
                )
    return lines


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
    parser.add_argument(
        "--stats",
        action="store_true",
        help="print the classification breakdown (never changes the exit code)",
    )
    parser.add_argument(
        "--stats-format",
        choices=("text", "json"),
        default=None,
        help="format for --stats output (default: text)",
    )
    args = parser.parse_args(argv)
    if args.stats_format is not None and not args.stats:
        parser.error("--stats-format requires --stats")

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
    documents = set(matrix_dir.glob("*.md"))
    correspondence_doc = root / "docs" / "qpdf-correspondence.md"
    if not correspondence_doc.is_file():
        # Skipping it silently would let a delete or rename pass CI while
        # leaving every citation in it unchecked.
        print(f"{correspondence_doc}: required citation document not found")
        return 1
    documents.add(correspondence_doc)
    for doc in sorted(documents):
        checker.check_document(doc)
        checker.collect_aggregate_tables(doc)
    for manifest in sorted(matrix_dir.glob("*.txt")):
        checker.check_manifest(manifest)
    checker.check_aggregate_tables(matrix_dir)

    if args.stats:
        stats = build_stats(report.classification_rows)
        if (args.stats_format or "text") == "json":
            print(json.dumps(stats, ensure_ascii=False, indent=2))
        else:
            for line in format_stats_text(stats):
                print(line)

    for error in report.errors:
        print(error)
    if report.errors:
        print(f"FAILED: {len(report.errors)} error(s)")
        return 1
    qpdf_note = "skipped (--no-qpdf)" if qpdf_root is None else f"checked against {qpdf_root}"
    print(
        f"OK: {report.qpdf_citations} qpdf citation(s) {qpdf_note}, "
        f"{report.flpdf_citations} flpdf citation(s), {report.rows} matrix row(s), "
        f"{len(report.aggregate_tables)} aggregate table(s)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
