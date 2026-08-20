"""Contract checks for the qpdf JSON input correspondence row."""

from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]


class JsonInputCorrespondenceTests(unittest.TestCase):
    def test_category_b_substitutions_are_recorded_in_both_documents(self):
        correspondence = (ROOT / "docs/qpdf-correspondence.md").read_text()
        module_doc = (ROOT / "crates/flpdf/src/json/input.rs").read_text()
        row = next(
            line
            for line in correspondence.splitlines()
            if line.startswith("| `QPDF_json.cc` 入力側")
        )

        for name in ("validate_pdf_version", "JsonDescription"):
            self.assertIn(name, row)
            self.assertIn(name, module_doc)
        self.assertIn("⚪ (B)", row)
        self.assertIn("category (B)", module_doc)


if __name__ == "__main__":
    unittest.main()
