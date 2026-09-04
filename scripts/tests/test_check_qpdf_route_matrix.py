from __future__ import annotations

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
            "pub(crate) struct ObjectCache;\n",
            encoding="utf-8",
        )
        (root / "docs" / "qpdf-route-matrix").mkdir(parents=True)

    def write(self, name: str, body: str) -> None:
        (self.root / "docs" / "qpdf-route-matrix" / name).write_text(
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

    def test_missing_matrix_directory_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo = SyntheticRepository(Path(temporary_directory))
            result = repo.check("--matrix-dir", "docs/nowhere")
            self.assertNotEqual(0, result.returncode)
            self.assertIn("nowhere", result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
