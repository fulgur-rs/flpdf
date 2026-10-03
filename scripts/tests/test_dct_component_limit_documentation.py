import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]


class DctComponentLimitDocumentationTests(unittest.TestCase):
    def test_default_backend_two_component_parity_is_documented(self):
        dct_source = (ROOT / "crates/flpdf/src/pipeline/dct.rs").read_text(
            encoding="utf-8"
        )
        module_doc = dct_source.split("\nuse ", 1)[0]
        correspondence = (ROOT / "docs/qpdf-correspondence.md").read_text(
            encoding="utf-8"
        )
        self.assertNotIn("恒久的な能力制限として扱う", correspondence)
        self.assertIn("default Rust backend も2成分JPEGでは", correspondence)
        dct_section = correspondence[
            correspondence.index("| `Pl_DCT.cc` (buffer/decode)") : correspondence.index(
                "`/ID` が qpdf と非 parity"
            )
        ]

        self.assertNotIn("Known component-count limitation (`flpdf-twm6`)", module_doc)
        self.assertIn("two-component JPEG", module_doc)
        self.assertIn("decode_raw", module_doc)
        self.assertIn("libjpeg's frame", module_doc)

        self.assertIn("`libjpeg-turbo-rs = 0.8.0`", dct_section)
        self.assertIn("two_component_dct", dct_section)
        self.assertIn(
            "show_object_two_component_dct_matches_qpdf_11_9_output_components",
            dct_section,
        )
        self.assertIn("libqpdf/Pl_DCT.cc:297-326", dct_section)


if __name__ == "__main__":
    unittest.main()
