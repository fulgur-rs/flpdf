import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]


class DctComponentLimitDocumentationTests(unittest.TestCase):
    def test_default_backend_unknown_component_parity_is_documented(self):
        dct_source = (ROOT / "crates/flpdf/src/pipeline/dct.rs").read_text(
            encoding="utf-8"
        )
        module_doc = dct_source.split("\nuse ", 1)[0]
        correspondence = (ROOT / "docs/qpdf-correspondence.md").read_text(
            encoding="utf-8"
        )
        module_text = " ".join(
            line.lstrip().removeprefix("//!").strip() for line in module_doc.splitlines()
        )
        self.assertNotIn("恒久的な能力制限として扱う", correspondence)
        self.assertIn("default Rust backendは2成分および5-10成分JPEGを", correspondence)
        dct_section = correspondence[
            correspondence.index("| `Pl_DCT.cc` (buffer/decode)") : correspondence.index(
                "`/ID` が qpdf と非 parity"
            )
        ]

        self.assertNotIn("Known component-count limitation (`flpdf-twm6`)", module_doc)
        self.assertIn("unknown-color-space JPEGs", module_text)
        self.assertIn("five to ten components", module_text)
        self.assertIn("decode_raw", module_text)
        self.assertIn("frame order", module_text)

        self.assertIn("`libjpeg-turbo-rs = 0.8.0`", dct_section)
        self.assertIn("2成分および5-10成分", dct_section)
        self.assertIn("dct_component_count_qpdf", dct_section)
        self.assertIn("Too many color components", dct_section)
        self.assertIn("Bogus marker length", dct_section)
        self.assertIn("Invalid component ID", dct_section)
        self.assertIn("Windows同梱qpdf runtime", dct_section)
        self.assertIn("libqpdf/Pl_DCT.cc:297-326", dct_section)


if __name__ == "__main__":
    unittest.main()
