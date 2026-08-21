"""Contract checks for the qpdf JSON input correspondence row."""

from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
CHECKER_PATH = ROOT / "scripts" / "qpdf-module-docs.py"


def _load_checker():
    spec = importlib.util.spec_from_file_location("qpdf_module_docs", CHECKER_PATH)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"unable to load {CHECKER_PATH}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class JsonInputCorrespondenceTests(unittest.TestCase):
    def test_category_b_substitutions_are_recorded_in_both_documents(self):
        checker = _load_checker()
        module_path = ROOT / "crates/flpdf/src/json/input.rs"
        module_doc = "\n".join(
            checker._leading_comment_lines(module_path.read_text())
        )
        correspondence = (ROOT / "docs/qpdf-correspondence.md").read_text()
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
