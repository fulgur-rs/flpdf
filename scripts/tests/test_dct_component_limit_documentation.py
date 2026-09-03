import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]


class DctComponentLimitDocumentationTests(unittest.TestCase):
    def test_default_component_limit_and_compat_route_are_documented(self):
        dct_source = (ROOT / "crates/flpdf/src/pipeline/dct.rs").read_text(
            encoding="utf-8"
        )
        module_doc = dct_source.split("\nuse ", 1)[0]
        correspondence = (ROOT / "docs/qpdf-correspondence.md").read_text(
            encoding="utf-8"
        )
        dct_section = correspondence[
            correspondence.index("| `Pl_DCT.cc` (buffer/decode)") : correspondence.index(
                "`/ID` が qpdf と非 parity"
            )
        ]

        self.assertIn("Known component-count limitation (`flpdf-twm6`)", module_doc)
        self.assertIn("`libjpeg-turbo-rs` 0.8.0", module_doc)
        self.assertIn("1/3/4-component", module_doc)
        self.assertIn("N components not yet supported", module_doc)
        self.assertIn("`qpdf-libjpeg-compat`", module_doc)
        self.assertIn("2-component", module_doc)

        self.assertIn("`flpdf-twm6`", dct_section)
        self.assertIn("`libjpeg-turbo-rs = 0.8.0`", dct_section)
        self.assertIn("1/3/4", dct_section)
        self.assertIn("2-component", dct_section)
        self.assertIn("qpdf-libjpeg-compat", dct_section)
        self.assertIn("libqpdf/Pl_DCT.cc:297-326", dct_section)


if __name__ == "__main__":
    unittest.main()
