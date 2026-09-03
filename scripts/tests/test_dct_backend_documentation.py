import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]


class DctBackendDocumentationTests(unittest.TestCase):
    def test_default_marker_limitation_and_compat_route_are_documented(self):
        dct_source = (ROOT / "crates/flpdf/src/pipeline/dct.rs").read_text(
            encoding="utf-8"
        )
        module_doc = dct_source.split("\nuse ", 1)[0]
        correspondence = (ROOT / "docs/qpdf-correspondence.md").read_text(
            encoding="utf-8"
        )

        for text in (
            "flpdf-69n1",
            "Unsupported marker type 0xNN",
            "qpdf-libjpeg-compat",
        ):
            self.assertIn(text, module_doc)
            self.assertIn(text, correspondence)


if __name__ == "__main__":
    unittest.main()
