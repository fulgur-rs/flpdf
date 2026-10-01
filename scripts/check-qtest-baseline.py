#!/usr/bin/env python3
"""Check that every non-pass qtest baseline row has an actionable rationale.

The pinned qtest survey action preserves only ``bead`` and ``rationale`` when
it rewrites the baseline. Keep the classification and exact Rust test selector
in the rationale instead of adding fields that the action would discard.

Rationale forms:

    represented: test=<package>/<cargo-test-target>::<test_fn>; <reason>
    excluded: <reason>                    # bead must be null
    applicable: open-qpdf-upgrade=<bead>; <reason>

GitHub CI has no Beads connection, so it verifies that all applicable rows
carry the same declared open qpdf-upgrade Bead. The current live Bead status is
verified with ``bd show`` when this baseline metadata is changed.
"""

from __future__ import annotations

import argparse
from collections import Counter
import json
from pathlib import Path
import re
import sys


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_BASELINE = ROOT / ".github" / "qtest-baseline.jsonl"
EXPECTED_APPLICABLE_IDS = {
    "split-pages 14",
    "split-pages 15",
    "split-pages 16",
}
RATIONALE_CLASSIFICATIONS = {"represented", "excluded", "applicable"}
REPRESENTED_RE = re.compile(
    r"^represented: test=(?P<package>[a-z0-9-]+)/(?P<target>lib|[a-z0-9_]+)::"
    r"(?P<test>[A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*); (?P<reason>\S.*)$"
)
APPLICABLE_RE = re.compile(
    r"^applicable: open-qpdf-upgrade=(?P<bead>flpdf-[a-z0-9]+); (?P<reason>\S.*)$"
)
TEST_DECLARATION_RE = re.compile(r"^(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)\b")


def test_functions(source: str) -> set[str]:
    """Return top-level test function names declared in one Rust test target."""
    found: set[str] = set()
    pending_test_attribute = False

    for line in source.splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("//"):
            continue
        if stripped.startswith("#["):
            if stripped in {"#[test]", "#[tokio::test]"}:
                pending_test_attribute = True
            continue

        declaration = TEST_DECLARATION_RE.match(stripped)
        if declaration is not None:
            if pending_test_attribute:
                found.add(declaration.group(1))
            pending_test_attribute = False
            continue

        pending_test_attribute = False

    return found


def _error(errors: list[str], baseline: Path, line: int, message: str) -> None:
    errors.append(f"{baseline}:{line}: {message}")


def _load_records(baseline: Path, errors: list[str]) -> list[tuple[int, object]]:
    try:
        lines = baseline.read_text(encoding="utf-8").splitlines()
    except OSError as exc:
        errors.append(f"{baseline}: cannot read baseline: {exc}")
        return []

    records: list[tuple[int, object]] = []
    for number, line in enumerate(lines, start=1):
        if not line.strip():
            continue
        try:
            records.append((number, json.loads(line)))
        except json.JSONDecodeError as exc:
            _error(errors, baseline, number, f"invalid JSON: {exc}")
    return records


def _check_header(
    baseline: Path,
    number: int,
    header: object,
    errors: list[str],
) -> None:
    if not isinstance(header, dict):
        _error(errors, baseline, number, "first record must be a qtest-baseline header")
        return
    if header.get("kind") != "qtest-baseline" or header.get("schema") != 1:
        _error(errors, baseline, number, "header must use qtest-baseline schema 1")

    total = header.get("total")
    suites = header.get("suites")
    if not isinstance(total, int) or isinstance(total, bool) or total < 0:
        _error(errors, baseline, number, "header total must be a non-negative integer")
    if not isinstance(suites, dict) or any(
        not isinstance(count, int) or isinstance(count, bool) or count < 0
        for count in (suites.values() if isinstance(suites, dict) else ())
    ):
        _error(errors, baseline, number, "header suites must map names to non-negative counts")
    elif isinstance(total, int) and not isinstance(total, bool) and sum(suites.values()) != total:
        _error(errors, baseline, number, "header suite counts must add up to total")


def _check_represented(
    root: Path,
    baseline: Path,
    number: int,
    rationale: str,
    errors: list[str],
) -> None:
    match = REPRESENTED_RE.fullmatch(rationale)
    if match is None:
        _error(
            errors,
            baseline,
            number,
            "represented rows must name an exact Rust test target and function as "
            "'represented: test=<package>/<target>::<function>; <reason>'",
        )
        return

    package = match.group("package")
    target = match.group("target")
    test_path = match.group("test")
    test_name = test_path.rsplit("::", 1)[-1]
    crate = root / "crates" / package
    if target == "lib":
        modules = [part for part in test_path.split("::")[:-1] if part != "tests"]
        source_root = crate / "src"
        candidates = (
            [source_root / "lib.rs", source_root / "main.rs"]
            if not modules
            else [
                source_root.joinpath(*modules).with_suffix(".rs"),
                source_root.joinpath(*modules, "mod.rs"),
            ]
        )
        target_path = next((path for path in candidates if path.is_file()), None)
        if target_path is None:
            _error(
                errors,
                baseline,
                number,
                f"Rust library test module {package}::{test_path.rsplit('::', 1)[0]} does not exist",
            )
            return
    else:
        target_path = crate / "tests" / f"{target}.rs"
        if not target_path.is_file():
            _error(
                errors,
                baseline,
                number,
                f"Rust test target {package}/{target} does not exist",
            )
            return

    try:
        declared = test_functions(target_path.read_text(encoding="utf-8"))
    except OSError as exc:
        _error(errors, baseline, number, f"cannot read Rust test target {target_path}: {exc}")
        return
    if test_name not in declared:
        _error(
            errors,
            baseline,
            number,
            f"Rust test target {package}/{target} does not declare test function {test_name}",
        )


def validate(root: Path, baseline: Path) -> tuple[list[str], Counter[str], int]:
    errors: list[str] = []
    records = _load_records(baseline, errors)
    if not records:
        if not errors:
            errors.append(f"{baseline}: baseline is empty")
        return errors, Counter(), 0

    _check_header(baseline, records[0][0], records[0][1], errors)
    rows = records[1:]
    classifications: Counter[str] = Counter()
    identities: set[str] = set()
    applicable_rows: list[dict[str, object]] = []

    for number, record in rows:
        if not isinstance(record, dict):
            _error(errors, baseline, number, "baseline row must be a JSON object")
            continue
        identity = record.get("id")
        if not isinstance(identity, str) or not identity:
            _error(errors, baseline, number, "baseline row id must be a non-empty string")
        elif identity in identities:
            _error(errors, baseline, number, f"duplicate baseline row id {identity!r}")
        else:
            identities.add(identity)

        if record.get("outcome") == "pass":
            _error(errors, baseline, number, "baseline rows must remain non-pass outcomes")

        rationale = record.get("rationale")
        if not isinstance(rationale, str) or ":" not in rationale:
            _error(errors, baseline, number, "every non-pass row needs a classified rationale")
            continue
        classification = rationale.split(":", 1)[0]
        if classification not in RATIONALE_CLASSIFICATIONS:
            _error(
                errors,
                baseline,
                number,
                f"unknown or missing machine-readable classification {classification!r}",
            )
            continue
        classifications[classification] += 1

        bead = record.get("bead")
        if bead is not None and not isinstance(bead, str):
            _error(errors, baseline, number, "bead must be a string or null")

        if classification == "represented":
            _check_represented(root, baseline, number, rationale, errors)
        elif classification == "excluded":
            if not rationale.startswith("excluded: ") or not rationale[10:].strip():
                _error(errors, baseline, number, "excluded rationale must include a reason")
            if bead is not None:
                _error(errors, baseline, number, "excluded rows must keep bead null")
        else:
            match = APPLICABLE_RE.fullmatch(rationale)
            if match is None:
                _error(
                    errors,
                    baseline,
                    number,
                    "applicable rows must name an open qpdf-upgrade Bead and a reason",
                )
                continue
            if bead != match.group("bead"):
                _error(
                    errors,
                    baseline,
                    number,
                    "applicable row bead must match its open qpdf-upgrade rationale",
                )
            applicable_rows.append(record)

    applicable_ids = {row.get("id") for row in applicable_rows}
    if applicable_ids != EXPECTED_APPLICABLE_IDS:
        _error(
            errors,
            baseline,
            records[0][0],
            "applicable rows must be split-pages 14, 15, and 16 exactly",
        )
    applicable_beads = {row.get("bead") for row in applicable_rows}
    if len(applicable_rows) != 3 or len(applicable_beads) != 1 or None in applicable_beads:
        _error(
            errors,
            baseline,
            records[0][0],
            "applicable rows must all reference one open qpdf-upgrade Bead",
        )

    return errors, classifications, len(rows)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT, help="repository root")
    parser.add_argument(
        "--baseline",
        type=Path,
        default=DEFAULT_BASELINE,
        help="qtest baseline JSONL path",
    )
    args = parser.parse_args(argv)

    root = args.root.resolve()
    baseline = args.baseline if args.baseline.is_absolute() else root / args.baseline
    errors, classifications, row_count = validate(root, baseline)
    if errors:
        print("ERROR: qtest baseline metadata is incomplete:", file=sys.stderr)
        for error in errors:
            print(f"  {error}", file=sys.stderr)
        return 1

    print(
        "OK: qtest baseline metadata: "
        f"{row_count} rows; "
        f"{classifications['represented']} represented, "
        f"{classifications['excluded']} excluded, "
        f"{classifications['applicable']} applicable"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
