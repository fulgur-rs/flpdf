from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
CHECKER_PATH = ROOT / "scripts" / "check-qpdf-route-matrix.py"

HEADER = (
    "| # | qpdf responsibility owner | qpdf evidence | flpdf current entrypoint "
    "| callers (prod / test) | classification | canonical owner "
    "| remaining bridge callers / notes |\n"
    "|---|---|---|---|---|---|---|---|\n"
)

DETAIL_HEADER = (
    "| case | qpdf test fn | flpdf owner fn | classification "
    "| A-D/E owner refs / notes |\n"
    "|---|---|---|---|---|\n"
)


def area_row(row_id: str, classification: str) -> str:
    return (
        f"| {row_id} | x | `libqpdf/QPDF.cc:1` | y | z | {classification} | w | - |\n"
    )


def detail_row(row_id: str, classification: str) -> str:
    return f"| {row_id} | a | b | {classification} | n |\n"


# The synthetic matrix below is the smallest tree that exercises every
# aggregate kind: two area documents (one of which also carries a `case`
# detail table whose `0/1` row counts as two logical cases) plus a README
# holding the three repository-wide tables. Tests override exactly the cell
# they probe with `str.replace`.
A_DOCUMENT = (
    "# A\n\n"
    + HEADER
    + area_row("A1", "canonical")
    + area_row("A2", "mixed")
    + area_row("A3", "canonical")
    + "\n### 分類集計\n\n"
    "<!-- route-matrix-aggregate: document-tally unit=area-physical file=a-x.md -->\n\n"
    "| 分類 | 件数 | 行 |\n"
    "|---|---|---|\n"
    "| canonical | 2 | A1, A3 |\n"
    "| bridge | 0 | — |\n"
    "| mixed | 1 | A2 |\n"
    "| unknown | 0 | — |\n"
)

B_DOCUMENT = (
    "# B\n\n"
    + HEADER
    + area_row("B1", "mixed")
    + "\n## qtest exceptions\n\n"
    + DETAIL_HEADER
    + detail_row("0/1", "canonical")
    + detail_row("2", "mixed")
    + detail_row("3", "canonical")
    + "\n**range 別サマリ**:\n\n"
    "<!-- route-matrix-aggregate: range-summary unit=physical detail-table=B1 -->\n\n"
    "| range | 物理行 | canonical | mixed | bridge | unknown |\n"
    "|---|---|---|---|---|---|\n"
    "| 0/1, 2 | 2 | 1（0/1） | 1（2） | 0（—） | 0（—） |\n"
    "| 3 | 1 | 1（3） | 0（—） | 0（—） | 0（—） |\n"
    "| **合計（物理行）** | **3** | **2** | **1** | **0** | **0** |\n"
    "| **合計（論理ケース）** | **4** | **3** | **1** | **0** | **0** |\n"
    "\n### 分類集計\n\n"
    "<!-- route-matrix-aggregate: document-tally unit=area-physical file=b-x.md -->\n\n"
    "| 分類 | 件数 | 行 |\n"
    "|---|---|---|\n"
    "| canonical | 0 | — |\n"
    "| bridge | 0 | — |\n"
    "| mixed | 1 | B1 |\n"
    "| unknown | 0 | — |\n"
)

README_DOCUMENT = (
    "# route matrix\n\n"
    "<!-- route-matrix-aggregate: area-total unit=area-physical -->\n\n"
    "| canonical | bridge | mixed | unknown | 合計 |\n"
    "|---|---|---|---|---|\n"
    "| 2 | 0 | 2 | 0 | 4 |\n\n"
    "<!-- route-matrix-aggregate: logical-total unit=logical -->\n\n"
    "| canonical | bridge | mixed | unknown | 合計 |\n"
    "|---|---|---|---|---|\n"
    "| 5 | 0 | 3 | 0 | 8 |\n\n"
    "## 領域別 matrix\n\n"
    "<!-- route-matrix-aggregate: per-file unit=area-physical -->\n\n"
    "| ファイル | 行数 | canonical | bridge | mixed | unknown |\n"
    "|---|---|---|---|---|---|\n"
    "| [A. ObjectHandle](a-x.md) | 3 | 2 | 0 | 1 | 0 |\n"
    "| [B. parser](b-x.md) | 1 | 0 | 0 | 1 | 0 |\n"
)


class SyntheticRepository:
    def __init__(self, root: Path) -> None:
        self.root = root
        self.qpdf = root / "qpdf-src"
        (self.qpdf / "libqpdf").mkdir(parents=True)
        (self.qpdf / "include" / "qpdf").mkdir(parents=True)
        (self.qpdf / "libqpdf" / "QPDF.cc").write_text("a\nb\nc\n", encoding="utf-8")
        (self.qpdf / "include" / "qpdf" / "QPDF.hh").write_text(
            "\n".join(str(i) for i in range(10)) + "\n", encoding="utf-8"
        )
        src = root / "crates" / "flpdf" / "src"
        src.mkdir(parents=True)
        (src / "reader.rs").write_text(
            "impl<R> Pdf<R> {\n    pub fn resolve(&mut self) {}\n}\n"
            "pub(crate) struct ObjectCache;\n"
            "pub struct Pdf {\n    pub(crate) legacy_state_synced: bool,\n}\n"
            "pub enum CacheEntry {\n    Resolved(u8),\n    Deleted,\n}\n"
            "fn gate() {\n    let qpdf_gate_flag = true;\n    let _ = qpdf_gate_flag;\n}\n",
            encoding="utf-8",
        )
        (root / "docs" / "qpdf-route-matrix").mkdir(parents=True)
        # The checker requires this document, so every synthetic repository
        # starts with an empty one; tests that care about its contents call
        # write_correspondence, and the missing-file case removes it.
        (root / "docs" / "qpdf-correspondence.md").write_text("", encoding="utf-8")

    def write(self, name: str, body: str) -> None:
        (self.root / "docs" / "qpdf-route-matrix" / name).write_text(
            body, encoding="utf-8"
        )

    def write_matrix(self) -> None:
        """Write the synthetic matrix whose aggregate tables all agree."""
        self.write("README.md", README_DOCUMENT)
        self.write("a-x.md", A_DOCUMENT)
        self.write("b-x.md", B_DOCUMENT)

    def write_correspondence(self, body: str) -> None:
        (self.root / "docs" / "qpdf-correspondence.md").write_text(
            body, encoding="utf-8"
        )

    def check(self, *extra: str) -> subprocess.CompletedProcess[str]:
        args = [
            sys.executable,
            str(CHECKER_PATH),
            "--check",
            "--root",
            str(self.root),
            "--qpdf-root",
            str(self.qpdf),
            *extra,
        ]
        return subprocess.run(args, capture_output=True, text=True, check=False)


class CheckQpdfRouteMatrixTests(unittest.TestCase):
    def test_valid_document_passes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write(
                "a.md",
                HEADER
                + "| 1 | `QPDF::resolve` | `libqpdf/QPDF.cc:1-3`; "
                "`include/qpdf/QPDF.hh:2-4,7` | "
                "`crates/flpdf/src/reader.rs::Pdf::resolve` (`pub`) | "
                "prod: 1 (x.rs) / test: 0 | canonical | "
                "`crates/flpdf/src/reader.rs::Pdf::resolve` | - |\n"
                "| 2 | `QPDF::obj_cache` | `libqpdf/QPDF.cc:2` | "
                "`crates/flpdf/src/reader.rs::ObjectCache` (`pub(crate)`) | "
                "prod: 0 / test: 0 | **bridge** | absent | none |\n",
            )
            result = repo.check()
            self.assertEqual(0, result.returncode, result.stdout + result.stderr)
            self.assertIn("OK", result.stdout)

    def test_correspondence_document_citations_are_checked(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write("a.md", HEADER)
            repo.write_correspondence("See `qpdf/Missing.cc:1` for details.\n")
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn("qpdf-correspondence.md", result.stdout)
            self.assertIn("qpdf/Missing.cc", result.stdout)

    def test_bare_basename_citations_are_resolved_and_counted(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            qpdf_programs = repo.qpdf / "qpdf"
            qpdf_programs.mkdir()
            (qpdf_programs / "qpdf-ctest.c").write_text("a\nb\n", encoding="utf-8")
            repo.write("a.md", HEADER)
            repo.write_correspondence(
                "See `QPDF.cc:1-3` and `qpdf-ctest.c:1-2` for details.\n"
            )
            result = repo.check()
            self.assertEqual(0, result.returncode, result.stdout + result.stderr)
            self.assertIn("OK: 2 qpdf citation(s)", result.stdout)

    def test_bare_basename_range_is_checked_against_the_resolved_file(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write("a.md", HEADER)
            repo.write_correspondence("See `QPDF.cc:1-4` for details.\n")
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn("QPDF.cc:1-4", result.stdout)

    def test_missing_bare_basename_is_an_error(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write("a.md", HEADER)
            repo.write_correspondence("See `Missing.cc:1` for details.\n")
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn("Missing.cc:1", result.stdout)
            self.assertIn("basename", result.stdout)

    def test_ambiguous_bare_basename_is_an_error(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            (repo.qpdf / "include" / "qpdf" / "QPDF.cc").write_text(
                "a\nb\nc\n", encoding="utf-8"
            )
            repo.write("a.md", HEADER)
            repo.write_correspondence("See `QPDF.cc:1` for details.\n")
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn("QPDF.cc:1", result.stdout)
            self.assertIn("ambiguous", result.stdout)

    def test_no_qpdf_still_checks_correspondence_citation_syntax(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write("a.md", HEADER)
            repo.write_correspondence("See `qpdf/QPDF.cc:bogus` for details.\n")
            result = repo.check("--no-qpdf")
            self.assertNotEqual(0, result.returncode)
            self.assertIn("qpdf-correspondence.md", result.stdout)
            self.assertIn("malformed qpdf citation", result.stdout)

    def test_no_qpdf_still_checks_bare_citation_syntax(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write("a.md", HEADER)
            repo.write_correspondence("See `QPDF.cc:bogus` for details.\n")
            result = repo.check("--no-qpdf")
            self.assertNotEqual(0, result.returncode)
            self.assertIn("QPDF.cc:bogus", result.stdout)
            self.assertIn("malformed qpdf citation", result.stdout)

    def test_line_range_past_end_of_file_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write("a.md", HEADER + "| 1 | x | `libqpdf/QPDF.cc:2-9` | y | z | canonical | w | - |\n")
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn("a.md:3:", result.stdout)
            self.assertIn("libqpdf/QPDF.cc:2-9", result.stdout)

    def test_missing_qpdf_file_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write("a.md", HEADER + "| 1 | x | `libqpdf/Nope.cc:1` | y | z | canonical | w | - |\n")
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn("libqpdf/Nope.cc", result.stdout)

    def test_inverted_range_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write("a.md", HEADER + "| 1 | x | `libqpdf/QPDF.cc:3-1` | y | z | canonical | w | - |\n")
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn("QPDF.cc:3-1", result.stdout)

    def test_missing_flpdf_symbol_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write(
                "a.md",
                HEADER + "| 1 | x | `libqpdf/QPDF.cc:1` | "
                "`crates/flpdf/src/reader.rs::Pdf::nope` | z | canonical | w | - |\n",
            )
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn("nope", result.stdout)

    def test_missing_flpdf_file_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write(
                "a.md",
                HEADER + "| 1 | x | `libqpdf/QPDF.cc:1` | "
                "`crates/flpdf/src/gone.rs::Pdf::resolve` | z | canonical | w | - |\n",
            )
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn("gone.rs", result.stdout)

    def test_flpdf_line_range_form_is_validated(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write("a.md", HEADER + "| 1 | x | `libqpdf/QPDF.cc:1` | `crates/flpdf/src/reader.rs:2-99` | z | canonical | w | - |\n")
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn("reader.rs:2-99", result.stdout)

    def test_bad_classification_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write("a.md", HEADER + "| 1 | x | `libqpdf/QPDF.cc:1` | y | z | legacy | w | - |\n")
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn("classification", result.stdout)
            self.assertIn("legacy", result.stdout)

    def test_prose_between_classification_rows_is_not_table_end(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write(
                "a.md",
                HEADER
                + "| 1 | x | `libqpdf/QPDF.cc:1` | y | z | canonical | w | - |\n"
                + "\n"
                + "The table continues after this explanatory note.\n"
                + "\n"
                + "| 2 | x | `libqpdf/QPDF.cc:2` | y | z | mixed | w | - |\n",
            )
            result = repo.check()
            self.assertEqual(0, result.returncode, result.stdout + result.stderr)
            self.assertIn("2 matrix row(s)", result.stdout)

    def test_nonclassification_table_after_prose_is_not_counted(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write(
                "a.md",
                HEADER
                + "| 1 | x | `libqpdf/QPDF.cc:1` | y | z | canonical | w | - |\n"
                + "\n"
                + "A different table follows.\n"
                + "\n"
                + "| name | value |\n"
                + "|---|---|\n"
                + "| stale | mixed |\n",
            )
            result = repo.check()
            self.assertEqual(0, result.returncode, result.stdout + result.stderr)
            self.assertIn("1 matrix row(s)", result.stdout)

    def test_combined_zero_one_row_counts_as_two_logical_cases(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write(
                "a.md",
                HEADER
                + "| 0/1 | x | `libqpdf/QPDF.cc:1` | y | z | bridge | w | - |\n",
            )
            result = repo.check()
            self.assertEqual(0, result.returncode, result.stdout + result.stderr)
            self.assertIn("2 matrix row(s)", result.stdout)

    def test_prose_citation_outside_table_is_also_checked(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write("a.md", "See `libqpdf/QPDF.cc:1-400` for details.\n")
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn("QPDF.cc:1-400", result.stdout)

    def test_missing_qpdf_root_is_error_unless_no_qpdf(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write("a.md", HEADER + "| 1 | x | `libqpdf/QPDF.cc:2-9` | y | z | canonical | w | - |\n")
            missing = str(repo.root / "absent")
            result = repo.check("--qpdf-root", missing)
            self.assertNotEqual(0, result.returncode)
            self.assertIn("qpdf source", result.stdout + result.stderr)

            skipped = repo.check("--no-qpdf")
            self.assertEqual(0, skipped.returncode, skipped.stdout + skipped.stderr)
            self.assertIn("skipped", skipped.stdout)

    def test_no_qpdf_still_rejects_malformed_qpdf_citation(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write("a.md", HEADER + "| 1 | x | `libqpdf/QPDF.cc:0` | y | z | canonical | w | - |\n")
            result = repo.check("--no-qpdf")
            self.assertNotEqual(0, result.returncode)
            self.assertIn("QPDF.cc:0", result.stdout)

    def test_malformed_qpdf_citation_is_not_silently_accepted(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write(
                "a.md",
                HEADER + "| 1 | x | `libqpdf/QPDF.cc:1--2` | y | z | canonical | w | - |\n",
            )
            result = repo.check("--no-qpdf")
            self.assertNotEqual(0, result.returncode)
            self.assertIn("QPDF.cc:1--2", result.stdout)

    def test_non_numeric_qpdf_citation_is_not_a_placeholder(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write(
                "a.md",
                HEADER + "| 1 | x | `libqpdf/QPDF.cc:bogus` | y | z | canonical | w | - |\n",
            )
            result = repo.check("--no-qpdf")
            self.assertNotEqual(0, result.returncode)
            self.assertIn("QPDF.cc:bogus", result.stdout)

    def test_comma_separated_flpdf_ranges_are_validated(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write("a.md", "See `crates/flpdf/src/reader.rs:1,99` for details.\n")
            result = repo.check("--no-qpdf")
            self.assertNotEqual(0, result.returncode)
            self.assertIn("reader.rs:1,99", result.stdout)

    def test_escaped_pipe_in_a_matrix_cell_does_not_shift_columns(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write(
                "a.md",
                HEADER + "| 1 | x | `libqpdf/QPDF.cc:1` | y \\| z | x | canonical | w | - |\n",
            )
            result = repo.check()
            self.assertEqual(0, result.returncode, result.stdout + result.stderr)

    def test_flpdf_symbol_in_comment_or_string_is_not_a_declaration(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            (repo.root / "crates" / "flpdf" / "src" / "reader.rs").write_text(
                '// fn gone() {}\nlet text = "fn gone()";\n', encoding="utf-8"
            )
            repo.write("a.md", "See `crates/flpdf/src/reader.rs::gone`.\n")
            result = repo.check("--no-qpdf")
            self.assertNotEqual(0, result.returncode)
            self.assertIn("gone", result.stdout)

    def test_field_and_variant_declarations_are_accepted(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write(
                "a.md",
                HEADER
                + "| 1 | x | `libqpdf/QPDF.cc:1` | `crates/flpdf/src/reader.rs::Pdf::legacy_state_synced` | z | bridge | w | - |\n"
                "| 2 | x | `libqpdf/QPDF.cc:1` | `crates/flpdf/src/reader.rs::CacheEntry::Deleted` | z | mixed | w | - |\n"
                "| 3 | x | `libqpdf/QPDF.cc:1` | `crates/flpdf/src/reader.rs::CacheEntry::Resolved` | z | mixed | w | - |\n"
                "| 4 | x | `libqpdf/QPDF.cc:1` | `crates/flpdf/src/reader.rs::qpdf_gate_flag` | z | mixed | w | - |\n",
            )
            result = repo.check()
            self.assertEqual(0, result.returncode, result.stdout + result.stderr)

    def test_manifest_symbols_are_validated(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write("a.md", HEADER)
            repo.write(
                "tracked-symbols.txt",
                "# comment\n"
                "crates/flpdf/src/reader.rs::Pdf::resolve   # ok\n"
                "bare_leaf_only\n"
                "crates/flpdf/src/reader.rs::Pdf::nope\n"
                "crates/flpdf/src/gone.rs::resolve\n"
                "crates/flpdf/src/reader.rs:12   # not a symbol form\n",
            )
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn("tracked-symbols.txt:4:", result.stdout)
            self.assertIn("nope", result.stdout)
            self.assertIn("tracked-symbols.txt:5:", result.stdout)
            self.assertIn("gone.rs", result.stdout)
            self.assertIn("tracked-symbols.txt:6:", result.stdout)
            self.assertNotIn("tracked-symbols.txt:2:", result.stdout)
            self.assertNotIn("tracked-symbols.txt:3:", result.stdout)

    def test_malformed_row_width_is_error_not_a_silent_table_end(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write(
                "a.md",
                HEADER
                + "| 1 | x | `libqpdf/QPDF.cc:1` | y | z | canonical | w | - | extra |\n"
                "| 2 | x | `libqpdf/QPDF.cc:1` | y | z | bogus | w | - |\n",
            )
            result = repo.check()
            self.assertNotEqual(0, result.returncode, result.stdout + result.stderr)
            self.assertIn("a.md:3:", result.stdout)
            self.assertIn("classification row has 9 cell(s), expected 8", result.stdout)
            # The table must keep going: the row after the malformed one is
            # still validated, so a stray pipe cannot silently drop the rest
            # of the table from both the count and the classification check.
            self.assertIn("a.md:4:", result.stdout)
            self.assertIn("classification `bogus` is not one of", result.stdout)

    def test_missing_correspondence_document_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_correspondence("See `libqpdf/QPDF.cc:1` for details.\n")
            (repo.root / "docs" / "qpdf-correspondence.md").unlink()
            result = repo.check()
            # Skipping it silently would let a delete or rename pass CI with
            # every citation in that document unchecked.
            self.assertNotEqual(0, result.returncode)
            self.assertIn("qpdf-correspondence.md", result.stdout + result.stderr)

    def test_missing_matrix_directory_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            result = repo.check("--matrix-dir", "docs/nowhere")
            self.assertNotEqual(0, result.returncode)
            self.assertIn("nowhere", result.stdout + result.stderr)



class AggregateTableTests(unittest.TestCase):
    """The aggregate tables must be recomputable from the classification rows."""

    def test_agreeing_aggregate_tables_pass(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            result = repo.check()
            self.assertEqual(0, result.returncode, result.stdout + result.stderr)
            self.assertIn("8 matrix row(s)", result.stdout)
            self.assertIn("6 aggregate table(s)", result.stdout)

    def test_document_tally_count_mismatch_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "a-x.md",
                A_DOCUMENT.replace(
                    "| canonical | 2 | A1, A3 |", "| canonical | 3 | A1, A3 |"
                ),
            )
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn(
                "document-tally `canonical`: says 3 but the matrix has 2",
                result.stdout,
            )

    def test_document_tally_row_ids_are_compared_when_the_count_matches(self) -> None:
        # The drift that motivated this check kept the totals right and moved
        # rows between the enumerations, so counting alone would pass.
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "a-x.md",
                A_DOCUMENT.replace(
                    "| canonical | 2 | A1, A3 |", "| canonical | 2 | A1, A2 |"
                ).replace("| mixed | 1 | A2 |", "| mixed | 1 | A3 |"),
            )
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn("enumerated row ids do not match the matrix", result.stdout)
            self.assertIn("missing A3", result.stdout)
            self.assertIn("unexpected A2", result.stdout)

    def test_document_tally_duplicate_row_id_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "a-x.md",
                A_DOCUMENT.replace(
                    "| canonical | 2 | A1, A3 |", "| canonical | 2 | A1, A1, A3 |"
                ),
            )
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn("lists A1 more than once", result.stdout)

    def test_document_tally_nonzero_count_without_row_ids_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "a-x.md",
                A_DOCUMENT.replace(
                    "| canonical | 2 | A1, A3 |", "| canonical | 2 | — |"
                ),
            )
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn("says 2 but enumerates no row ids", result.stdout)

    def test_zero_count_row_id_cell_may_carry_prose(self) -> None:
        # The real c-stream tally explains a resolved `unknown` in that cell,
        # so a zero count must not be read as an enumeration.
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "a-x.md",
                A_DOCUMENT.replace(
                    "| unknown | 0 | — |", "| unknown | 0 | なし（A2 は解決済み） |"
                ),
            )
            result = repo.check()
            self.assertEqual(0, result.returncode, result.stdout + result.stderr)

    def test_zero_count_row_id_cell_does_not_hide_a_real_id_in_a_mixed_list(
        self,
    ) -> None:
        # Free-form prose is allowed (see the prose test above), but a
        # comma-separated list that mixes a real-looking row id with other
        # tokens must not let the row id escape unnoticed just because the
        # list as a whole does not parse cleanly.
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "a-x.md",
                A_DOCUMENT.replace("| unknown | 0 | — |", "| unknown | 0 | A1, note |"),
            )
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn("says 0 but enumerates row ids", result.stdout)
            self.assertIn("A1", result.stdout)

    def test_missing_document_tally_marker_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "a-x.md",
                A_DOCUMENT.replace(
                    "<!-- route-matrix-aggregate: document-tally "
                    "unit=area-physical file=a-x.md -->\n",
                    "",
                ),
            )
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn("a-x.md: expected exactly 1", result.stdout)
            self.assertIn("document-tally` table, found 0", result.stdout)

    def test_duplicate_document_tally_marker_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write("a-x.md", A_DOCUMENT + "\n" + A_DOCUMENT.split("### 分類集計")[1])
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn("document-tally` table, found 2", result.stdout)

    def test_document_tally_file_attribute_must_name_its_own_document(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write("a-x.md", A_DOCUMENT.replace("file=a-x.md", "file=b-x.md"))
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn("declares file=`b-x.md` but lives in `a-x.md`", result.stdout)

    def test_document_tally_must_list_every_classification(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write("a-x.md", A_DOCUMENT.replace("| unknown | 0 | — |\n", ""))
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn(
                "document-tally must list `unknown` exactly once, found 0",
                result.stdout,
            )

    def test_area_total_mismatch_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "README.md",
                README_DOCUMENT.replace(
                    "| 2 | 0 | 2 | 0 | 4 |", "| 3 | 0 | 1 | 0 | 4 |"
                ),
            )
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn(
                "area-total: `canonical` says 3 but the matrix has 2", result.stdout
            )

    def test_area_total_sum_cell_is_checked(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "README.md",
                README_DOCUMENT.replace(
                    "| 2 | 0 | 2 | 0 | 4 |", "| 2 | 0 | 2 | 0 | 5 |"
                ),
            )
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn("area-total: total says 5 but the matrix has 4", result.stdout)

    def test_logical_total_counts_the_combined_row_as_two_cases(self) -> None:
        # Physical numbers for the same rows are canonical 4 / mixed 3 = 7;
        # the logical table must reject them because `0/1` is two cases.
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "README.md",
                README_DOCUMENT.replace(
                    "| 5 | 0 | 3 | 0 | 8 |", "| 4 | 0 | 3 | 0 | 7 |"
                ),
            )
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn(
                "logical-total: `canonical` says 4 but the matrix has 5", result.stdout
            )
            self.assertIn(
                "logical-total: total says 7 but the matrix has 8", result.stdout
            )

    def test_per_file_mismatch_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "README.md",
                README_DOCUMENT.replace(
                    "| [A. ObjectHandle](a-x.md) | 3 | 2 | 0 | 1 | 0 |",
                    "| [A. ObjectHandle](a-x.md) | 3 | 1 | 0 | 2 | 0 |",
                ),
            )
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn(
                "per-file `a-x.md`: `canonical` says 1 but the matrix has 2",
                result.stdout,
            )

    def test_per_file_must_list_every_area_document(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "README.md",
                README_DOCUMENT.replace(
                    "| [B. parser](b-x.md) | 1 | 0 | 0 | 1 | 0 |\n", ""
                ),
            )
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn("per-file: `b-x.md` is not listed", result.stdout)

    def test_per_file_link_must_resolve(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "README.md", README_DOCUMENT.replace("(b-x.md)", "(b-renamed.md)")
            )
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn("per-file: `b-renamed.md` does not exist", result.stdout)

    def test_missing_repository_wide_marker_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "README.md",
                README_DOCUMENT.replace(
                    "<!-- route-matrix-aggregate: per-file unit=area-physical -->\n", ""
                ),
            )
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn("per-file` table, found 0", result.stdout)

    def test_range_summary_bucket_misplacement_is_error(self) -> None:
        # The e-job range summary listed case 34 under `mixed` while its
        # detail row said `canonical`; both cells must fail.
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "b-x.md",
                B_DOCUMENT.replace(
                    "| 0/1, 2 | 2 | 1（0/1） | 1（2） | 0（—） | 0（—） |",
                    "| 0/1, 2 | 2 | 0（—） | 2（0/1, 2） | 0（—） | 0（—） |",
                ),
            )
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn(
                "range-summary bucket `0/1, 2`: `canonical` says 0 but the matrix has 1",
                result.stdout,
            )
            self.assertIn("unexpected 0/1", result.stdout)

    def test_range_summary_physical_and_logical_totals_are_separate(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "b-x.md",
                B_DOCUMENT.replace(
                    "| **合計（物理行）** | **3** | **2** | **1** | **0** | **0** |",
                    "| **合計（物理行）** | **4** | **3** | **1** | **0** | **0** |",
                ),
            )
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn(
                "range-summary `合計（物理行）`: `canonical` says 3 but the matrix has 2",
                result.stdout,
            )
            self.assertIn(
                "range-summary `合計（物理行）`: total says 4 but the matrix has 3",
                result.stdout,
            )

    def test_range_summary_missing_logical_total_row_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "b-x.md",
                B_DOCUMENT.replace(
                    "| **合計（論理ケース）** | **4** | **3** | **1** | **0** | **0** |\n",
                    "",
                ),
            )
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn(
                "range-summary: the `合計（論理ケース）` total row is missing",
                result.stdout,
            )

    def test_range_summary_buckets_must_cover_every_detail_row(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "b-x.md",
                B_DOCUMENT.replace(
                    "| 3 | 1 | 1（3） | 0（—） | 0（—） | 0（—） |\n", ""
                ),
            )
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn(
                "range-summary: case(s) 3 are in the detail table but in no range bucket",
                result.stdout,
            )

    def test_range_summary_buckets_must_not_cover_absent_cases(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "b-x.md",
                B_DOCUMENT.replace(
                    "| 3 | 1 | 1（3） | 0（—） | 0（—） | 0（—） |",
                    "| 3-4 | 1 | 1（3） | 0（—） | 0（—） | 0（—） |",
                ),
            )
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn(
                "range bucket(s) cover case(s) 4 that the detail table does not have",
                result.stdout,
            )

    def test_range_summary_buckets_must_not_overlap(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "b-x.md",
                B_DOCUMENT.replace(
                    "| 3 | 1 | 1（3） | 0（—） | 0（—） | 0（—） |",
                    "| 2-3 | 1 | 1（3） | 0（—） | 0（—） | 0（—） |",
                ),
            )
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn(
                "bucket overlaps an earlier bucket on case(s) 2", result.stdout
            )

    def test_range_summary_bucket_must_not_split_a_combined_row(self) -> None:
        # Splitting `0/1` across buckets would make the physical count look
        # plausible while no bucket actually contains the row.
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "b-x.md",
                B_DOCUMENT.replace(
                    "| 0/1, 2 | 2 | 1（0/1） | 1（2） | 0（—） | 0（—） |",
                    "| 0, 2 | 2 | 1（0/1） | 1（2） | 0（—） | 0（—） |",
                ),
            )
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn(
                "detail row `0/1` is only partly covered by this range bucket",
                result.stdout,
            )

    def test_repository_wide_aggregate_must_live_in_readme(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            area_total = (
                "<!-- route-matrix-aggregate: area-total unit=area-physical -->\n\n"
                "| canonical | bridge | mixed | unknown | 合計 |\n"
                "|---|---|---|---|---|\n"
                "| 2 | 0 | 2 | 0 | 4 |\n"
            )
            repo.write("README.md", README_DOCUMENT.replace(area_total, ""))
            repo.write("a-x.md", A_DOCUMENT + "\n" + area_total)

            result = repo.check()

            self.assertNotEqual(0, result.returncode)
            self.assertIn("area-total", result.stdout)
            self.assertIn("README.md", result.stdout)

    def test_repository_wide_aggregate_with_no_readme_at_all_is_still_error(
        self,
    ) -> None:
        # A prior version of this check only validated placement/inventory
        # of area-total/logical-total/per-file inside an `if README.md
        # exists` guard, so deleting README.md entirely -- not just moving
        # one table out of it -- let these tables escape both checks no
        # matter where they lived.
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            area_total = (
                "<!-- route-matrix-aggregate: area-total unit=area-physical -->\n\n"
                "| canonical | bridge | mixed | unknown | 合計 |\n"
                "|---|---|---|---|---|\n"
                "| 2 | 0 | 2 | 0 | 4 |\n"
            )
            (repo.root / "docs" / "qpdf-route-matrix" / "README.md").unlink()
            repo.write("a-x.md", A_DOCUMENT + "\n" + area_total)

            result = repo.check()

            self.assertNotEqual(0, result.returncode)
            self.assertIn("area-total", result.stdout)
            self.assertIn("README.md", result.stdout)

    def test_no_repository_wide_aggregate_tables_and_no_readme_is_not_flagged(
        self,
    ) -> None:
        # A matrix directory with neither README.md nor any
        # area-total/logical-total/per-file table anywhere has nothing for
        # this check to validate -- it must stay silent rather than treat
        # the absence of README.md itself as an error.
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write("a.md", HEADER + area_row("A1", "canonical"))

            result = repo.check()

            self.assertEqual(0, result.returncode, result.stdout + result.stderr)

    def test_zero_document_tally_cell_must_not_enumerate_a_row_id(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "a-x.md",
                A_DOCUMENT.replace("| bridge | 0 | — |", "| bridge | 0 | A1 |")
            )

            result = repo.check()

            self.assertNotEqual(0, result.returncode)
            self.assertIn("document-tally `bridge`", result.stdout)
            self.assertIn("says 0", result.stdout)

    def test_zero_range_summary_cell_must_not_enumerate_a_row_id(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "b-x.md",
                B_DOCUMENT.replace(
                    "| 0/1, 2 | 2 | 1（0/1） | 1（2） | 0（—） | 0（—） |",
                    "| 0/1, 2 | 2 | 1（0/1） | 1（2） | 0（0/1） | 0（—） |",
                ),
            )

            result = repo.check()

            self.assertNotEqual(0, result.returncode)
            self.assertIn("range-summary", result.stdout)
            self.assertIn("zero", result.stdout)

    def test_bare_zero_range_summary_cell_without_an_enumeration_is_accepted(
        self,
    ) -> None:
        # A zero count has nothing to enumerate, so a bare `0` (no
        # parenthesized text at all, not even the `(—)` placeholder) must be
        # accepted -- only a non-zero count omitting its enumeration is an
        # error under `require_enumeration`.
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "b-x.md",
                B_DOCUMENT.replace(
                    "| 0/1, 2 | 2 | 1（0/1） | 1（2） | 0（—） | 0（—） |",
                    "| 0/1, 2 | 2 | 1（0/1） | 1（2） | 0 | 0 |",
                ),
            )

            result = repo.check()

            self.assertEqual(0, result.returncode, result.stdout + result.stderr)

    def test_per_file_link_must_target_an_area_document(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "README.md",
                README_DOCUMENT
                + "| [outside](README.md) | 0 | 0 | 0 | 0 | 0 |\n",
            )

            result = repo.check()

            self.assertNotEqual(0, result.returncode)
            self.assertIn("per-file", result.stdout)
            self.assertIn("area document", result.stdout)

    def test_duplicate_area_row_id_is_error_even_when_aggregate_counts_match(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            duplicate_a = A_DOCUMENT.replace(
                area_row("A3", "canonical"),
                area_row("A3", "canonical") + area_row("A1", "canonical"),
            ).replace("| canonical | 2 | A1, A3 |", "| canonical | 3 | A1, A3 |")
            repo.write("a-x.md", duplicate_a)
            repo.write(
                "README.md",
                README_DOCUMENT.replace("| 2 | 0 | 2 | 0 | 4 |", "| 3 | 0 | 2 | 0 | 5 |")
                .replace("| 5 | 0 | 3 | 0 | 8 |", "| 6 | 0 | 3 | 0 | 9 |")
                .replace(
                    "| [A. ObjectHandle](a-x.md) | 3 | 2 | 0 | 1 | 0 |",
                    "| [A. ObjectHandle](a-x.md) | 4 | 3 | 0 | 1 | 0 |",
                ),
            )

            result = repo.check()

            self.assertNotEqual(0, result.returncode)
            self.assertIn("row id `A1` appears more than once", result.stdout)

    def test_duplicate_area_row_id_across_split_tables_in_one_document_is_error(
        self,
    ) -> None:
        # A document that splits its `area` classification rows across more
        # than one physical table (e.g. one table per section) must still
        # keep row ids unique across all of them, not just within a single
        # table -- the uniqueness key used to key `table_index` alone, which
        # let the same row id reappear once per table.
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            split_a = A_DOCUMENT + "\n## second section\n\n" + HEADER + area_row(
                "A1", "canonical"
            )
            repo.write("a-x.md", split_a)

            result = repo.check()

            self.assertNotEqual(0, result.returncode)
            self.assertIn("row id `A1` appears more than once", result.stdout)

    def test_overlapping_detail_case_sets_are_error_even_when_range_counts_match(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            duplicate_b = B_DOCUMENT.replace(
                detail_row("2", "mixed"),
                detail_row("2", "mixed") + detail_row("2/2", "mixed"),
            )
            duplicate_b = duplicate_b.replace(
                "| 0/1, 2 | 2 | 1（0/1） | 1（2） | 0（—） | 0（—） |",
                "| 0/1, 2 | 3 | 1（0/1） | 2（2, 2/2） | 0（—） | 0（—） |",
            ).replace(
                "| **合計（物理行）** | **3** | **2** | **1** | **0** | **0** |",
                "| **合計（物理行）** | **4** | **2** | **2** | **0** | **0** |",
            ).replace(
                "| **合計（論理ケース）** | **4** | **3** | **1** | **0** | **0** |",
                "| **合計（論理ケース）** | **5** | **3** | **2** | **0** | **0** |",
            )
            repo.write("b-x.md", duplicate_b)
            repo.write(
                "README.md",
                README_DOCUMENT.replace("| 5 | 0 | 3 | 0 | 8 |", "| 5 | 0 | 4 | 0 | 9 |"),
            )

            result = repo.check()

            self.assertNotEqual(0, result.returncode)
            self.assertIn("case set", result.stdout)

    def test_detail_row_id_repeating_its_own_case_number_is_error(self) -> None:
        # `row_id_cases` folds a row id's `/`-separated parts into a set, so
        # `3/3` collapses to `{3}` -- the same set a plain `3` would produce.
        # Because it is the only row covering case 3, it never collides with
        # an earlier row's case set, so the cross-row overlap check alone
        # cannot catch it; the row id's self-repetition must be flagged
        # directly.
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            duplicate_b = B_DOCUMENT.replace(
                detail_row("3", "canonical"), detail_row("3/3", "canonical")
            ).replace(
                "| 3 | 1 | 1（3） | 0（—） | 0（—） | 0（—） |",
                "| 3 | 1 | 1（3/3） | 0（—） | 0（—） | 0（—） |",
            )
            repo.write("b-x.md", duplicate_b)

            result = repo.check()

            self.assertNotEqual(0, result.returncode)
            self.assertIn(
                "detail row `3/3` repeats the same case number within itself",
                result.stdout,
            )

    def test_range_summary_buckets_must_partition_the_detail_rows(self) -> None:
        # Two detail rows with the same case number are covered by the bucket's
        # case set while leaving one physical row unaccounted for.
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "b-x.md",
                B_DOCUMENT.replace(
                    detail_row("3", "canonical"),
                    detail_row("3", "canonical") + detail_row("3", "canonical"),
                ),
            )
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn(
                "range-summary: the range buckets hold 3 of the 4 detail row(s)",
                result.stdout,
            )

    def test_range_summary_nonzero_bucket_cell_must_enumerate_row_ids(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "b-x.md",
                B_DOCUMENT.replace(
                    "| 3 | 1 | 1（3） | 0（—） | 0（—） | 0（—） |",
                    "| 3 | 1 | 1 | 0（—） | 0（—） | 0（—） |",
                ),
            )
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn(
                "`canonical` says 1 but enumerates no row ids", result.stdout
            )

    def test_range_summary_detail_table_attribute_must_name_a_row(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write("b-x.md", B_DOCUMENT.replace("detail-table=B1", "detail-table=B9"))
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn(
                "names detail-table=`B9`, which is not a row of `b-x.md`", result.stdout
            )

    def test_missing_range_summary_for_a_detail_table_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "b-x.md",
                B_DOCUMENT.replace(
                    "<!-- route-matrix-aggregate: range-summary "
                    "unit=physical detail-table=B1 -->\n",
                    "",
                ),
            )
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn("range-summary` table for its `case` detail table", result.stdout)

    def test_aggregate_marker_without_a_table_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "a-x.md",
                A_DOCUMENT.replace("| 分類 | 件数 | 行 |\n|---|---|---|\n", "prose\n\n"),
            )
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn(
                "marker is not followed by a Markdown table", result.stdout
            )

    def test_aggregate_row_that_is_also_a_classification_row_is_error(self) -> None:
        # A tally whose own header names the `classification` column re-arms
        # the classification state machine, so its rows would be counted
        # twice: once as matrix rows and once as the aggregate of them.
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write(
                "a-x.md",
                A_DOCUMENT.replace("| 分類 | 件数 | 行 |", "| classification | 件数 | 行 |"),
            )
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn(
                "row is also counted as a classification row", result.stdout
            )
            # The first data row of the colliding tally table.
            self.assertIn("a-x.md:15:", result.stdout)

    def test_unmarked_aggregate_table_is_not_checked(self) -> None:
        # Detection is opt-in: an unmarked table with the same header shape
        # and deliberately stale numbers must not fail, because the historical
        # snapshots in the real documents repeat those very numbers.
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "README.md",
                README_DOCUMENT
                + "\n## 6.3 履歴スナップショット（2026-01-01、再計測値ではない）\n\n"
                "| canonical | bridge | mixed | unknown | 合計 |\n"
                "|---|---|---|---|---|\n"
                "| 99 | 7 | 42 | 3 | 151 |\n",
            )
            result = repo.check()
            self.assertEqual(0, result.returncode, result.stdout + result.stderr)

    def test_unknown_aggregate_kind_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "README.md", README_DOCUMENT.replace("per-file unit=", "per-flie unit=")
            )
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn("`route-matrix-aggregate: per-flie` is not one of", result.stdout)

    def test_unknown_aggregate_unit_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "README.md",
                README_DOCUMENT.replace("per-file unit=area-physical", "per-file unit=logical"),
            )
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn("unit `logical` is not one of: area-physical", result.stdout)

    def test_missing_aggregate_attribute_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "a-x.md", A_DOCUMENT.replace(" unit=area-physical file=a-x.md", "")
            )
            result = repo.check()
            self.assertNotEqual(0, result.returncode)
            self.assertIn("is missing the `unit` attribute", result.stdout)
            self.assertIn("is missing the `file` attribute", result.stdout)

    def test_aggregate_checks_run_without_the_qpdf_source(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "a-x.md",
                A_DOCUMENT.replace(
                    "| canonical | 2 | A1, A3 |", "| canonical | 3 | A1, A3 |"
                ),
            )
            result = repo.check("--no-qpdf")
            self.assertNotEqual(0, result.returncode)
            self.assertIn("document-tally `canonical`", result.stdout)


class StatsTests(unittest.TestCase):
    def test_stats_reports_the_breakdown(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            result = repo.check("--stats")
            self.assertEqual(0, result.returncode, result.stdout + result.stderr)
            self.assertIn(
                "logical total: canonical 5 / bridge 0 / mixed 3 / unknown 0 = 8",
                result.stdout,
            )
            self.assertIn(
                "physical total: canonical 4 / bridge 0 / mixed 3 / unknown 0 = 7",
                result.stdout,
            )
            self.assertIn(
                "area physical total: canonical 2 / bridge 0 / mixed 2 / unknown 0 = 4",
                result.stdout,
            )
            self.assertIn(
                "detail logical: canonical 3 / bridge 0 / mixed 1 / unknown 0 = 4",
                result.stdout,
            )
            explicit = repo.check("--stats", "--stats-format", "text")
            self.assertEqual(0, explicit.returncode, explicit.stdout + explicit.stderr)
            self.assertEqual(result.stdout, explicit.stdout)

    def test_stats_does_not_change_the_exit_code(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            repo.write(
                "a-x.md",
                A_DOCUMENT.replace(
                    "| canonical | 2 | A1, A3 |", "| canonical | 3 | A1, A3 |"
                ),
            )
            result = repo.check("--stats")
            self.assertNotEqual(0, result.returncode)
            self.assertIn("classification stats:", result.stdout)
            self.assertIn("document-tally `canonical`", result.stdout)

    def test_stats_json_format(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            result = repo.check("--stats", "--stats-format", "json")
            self.assertEqual(0, result.returncode, result.stdout + result.stderr)
            payload = json.loads(
                result.stdout[result.stdout.index("{") : result.stdout.rindex("}") + 1]
            )
            self.assertEqual(
                {"canonical": 5, "bridge": 0, "mixed": 3, "unknown": 0, "total": 8},
                payload["logical_total"],
            )
            self.assertEqual(
                {"canonical": 2, "bridge": 0, "mixed": 2, "unknown": 0, "total": 4},
                payload["area_physical_total"],
            )
            self.assertEqual(
                {"canonical": 3, "bridge": 0, "mixed": 1, "unknown": 0, "total": 4},
                payload["documents"]["b-x.md"]["detail"]["logical"],
            )

    def test_stats_format_requires_stats(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            repo.write_matrix()
            result = repo.check("--stats-format", "json")
            self.assertEqual(2, result.returncode, result.stdout + result.stderr)
            self.assertIn("--stats-format requires --stats", result.stderr)


if __name__ == "__main__":
    unittest.main()
