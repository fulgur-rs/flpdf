import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]


class DctBackendDocumentationTests(unittest.TestCase):
    def test_default_reserved_marker_diagnostic_matches_qpdf(self):
        dct_source = (ROOT / "crates/flpdf/src/pipeline/dct.rs").read_text(
            encoding="utf-8"
        )
        module_doc = dct_source.split("\nuse ", 1)[0]
        correspondence = (ROOT / "docs/qpdf-correspondence.md").read_text(
            encoding="utf-8"
        )

        self.assertIn("formats reserved marker bytes as qpdf's exact", module_doc)
        self.assertIn("Unsupported marker type 0xNN", module_doc)
        self.assertIn("the default path now scans marker segments", module_doc)
        self.assertIn("before the Rust decoder starts", module_doc)
        self.assertNotIn("flpdf-specific marker error", module_doc)
        self.assertIn("`qpdf-libjpeg-compat`", module_doc)

        dct_section = correspondence[
            correspondence.index("| `Pl_DCT.cc` (buffer/decode)") : correspondence.index(
                "`/ID` が qpdf と非 parity"
            )
        ]
        self.assertIn("libqpdf/Pl_DCT.cc:24-31,83-142", dct_section)
        self.assertIn("JERR_UNKNOWN_MARKER", dct_section)
        self.assertIn("first_reserved_marker_before_sos", dct_section)
        self.assertIn("Unsupported marker type 0xNN", dct_section)
        self.assertIn("show_object_default_dct_reserved_marker_diagnostic_matches_qpdf_11_9", dct_section)
        self.assertNotIn("恒久的なbackend limitation", dct_section)


if __name__ == "__main__":
    unittest.main()
