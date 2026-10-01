from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "check-qtest-baseline.py"
UPGRADE_BEAD = "flpdf-nhula"


def write(root: Path, relative: str, body: str) -> None:
    path = root / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body, encoding="utf-8")


def baseline_records() -> list[dict]:
    return [
        {
            "kind": "qtest-baseline",
            "schema": 1,
            "total": 106,
            "suites": {"c-api": 7, "c-api-check": 2, "split-pages": 97},
        },
        {
            "id": "c-api 3",
            "suite": "c-api",
            "category": "c-api",
            "ordinal": 3,
            "description": "normalized content",
            "outcome": "fail",
            "bead": None,
            "rationale": (
                "represented: test=flpdf/metadata_fixture::covers_supported_behavior; "
                "Portable behavior has a direct regression test."
            ),
        },
        {
            "id": "c-api-check 1",
            "suite": "c-api-check",
            "category": "c-api-check",
            "ordinal": 1,
            "description": "C check warn",
            "outcome": "fail",
            "bead": None,
            "rationale": "excluded: qpdf C ABI callback behavior is out of scope.",
        },
        *[
            {
                "id": f"split-pages {ordinal}",
                "suite": "split-pages",
                "category": "split-pages",
                "ordinal": ordinal,
                "description": f"check output chunk {ordinal}",
                "outcome": "expected-fail",
                "bead": UPGRADE_BEAD,
                "rationale": (
                    f"applicable: open-qpdf-upgrade={UPGRADE_BEAD}; "
                    "Revalidate byte identity when the pinned qpdf version changes."
                ),
            }
            for ordinal in (14, 15, 16)
        ],
    ]


def write_baseline(root: Path, records: list[dict]) -> Path:
    baseline = root / ".github" / "qtest-baseline.jsonl"
    baseline.parent.mkdir(parents=True, exist_ok=True)
    baseline.write_text(
        "".join(json.dumps(record, sort_keys=True) + "\n" for record in records),
        encoding="utf-8",
    )
    return baseline


def run(root: Path, baseline: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            "--root",
            str(root),
            "--baseline",
            str(baseline),
        ],
        capture_output=True,
        text=True,
        check=False,
    )


class QtestBaselineMetadata(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        write(
            self.root,
            "crates/flpdf/tests/metadata_fixture.rs",
            "#[test]\nfn covers_supported_behavior() {}\n",
        )
        self.records = baseline_records()
        self.baseline = write_baseline(self.root, self.records)

    def tearDown(self) -> None:
        self.temp.cleanup()

    def test_accepts_complete_metadata_and_existing_test_target(self) -> None:
        result = run(self.root, self.baseline)
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertIn("1 represented, 1 excluded, 3 applicable", result.stdout)

    def test_accepts_a_library_unit_test_target(self) -> None:
        write(
            self.root,
            "crates/flpdf/src/metadata.rs",
            "#[cfg(test)]\nmod tests {\n"
            "    #[test]\n    fn covers_library_behavior() {}\n}\n",
        )
        self.records[1]["rationale"] = (
            "represented: test=flpdf/lib::metadata::tests::covers_library_behavior; "
            "Portable behavior has a direct regression test."
        )
        baseline = write_baseline(self.root, self.records)
        result = run(self.root, baseline)
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)

    def test_checked_in_baseline_is_fully_classified(self) -> None:
        result = run(ROOT, ROOT / ".github" / "qtest-baseline.jsonl")
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertIn("49 rows; 33 represented, 13 excluded, 3 applicable", result.stdout)

    def test_rejects_a_represented_row_without_exact_test_reference(self) -> None:
        self.records[1]["rationale"] = "represented: Portable behavior is covered."
        baseline = write_baseline(self.root, self.records)
        result = run(self.root, baseline)
        self.assertNotEqual(0, result.returncode)
        self.assertIn("exact Rust test target and function", result.stderr)

    def test_rejects_an_unrecognized_classification(self) -> None:
        self.records[1]["rationale"] = "untracked: Portable behavior is covered."
        baseline = write_baseline(self.root, self.records)
        result = run(self.root, baseline)
        self.assertNotEqual(0, result.returncode)
        self.assertIn("unknown or missing machine-readable classification", result.stderr)

    def test_rejects_a_test_function_missing_from_the_target(self) -> None:
        self.records[1]["rationale"] = (
            "represented: test=flpdf/metadata_fixture::missing_test; "
            "Portable behavior has a direct regression test."
        )
        baseline = write_baseline(self.root, self.records)
        result = run(self.root, baseline)
        self.assertNotEqual(0, result.returncode)
        self.assertIn("does not declare test function", result.stderr)

    def test_rejects_an_excluded_row_linked_to_a_bead(self) -> None:
        self.records[2]["bead"] = "flpdf-something"
        baseline = write_baseline(self.root, self.records)
        result = run(self.root, baseline)
        self.assertNotEqual(0, result.returncode)
        self.assertIn("excluded rows must keep bead null", result.stderr)

    def test_rejects_applicable_rows_that_do_not_share_the_revalidation_bead(self) -> None:
        self.records[-1]["bead"] = "flpdf-other"
        self.records[-1]["rationale"] = (
            "applicable: open-qpdf-upgrade=flpdf-other; "
            "Revalidate byte identity when the pinned qpdf version changes."
        )
        baseline = write_baseline(self.root, self.records)
        result = run(self.root, baseline)
        self.assertNotEqual(0, result.returncode)
        self.assertIn("must all reference one open qpdf-upgrade Bead", result.stderr)

    def test_rejects_missing_split_pages_revalidation_rows(self) -> None:
        self.records.pop()
        baseline = write_baseline(self.root, self.records)
        result = run(self.root, baseline)
        self.assertNotEqual(0, result.returncode)
        self.assertIn("split-pages 14, 15, and 16", result.stderr)


if __name__ == "__main__":
    unittest.main()
