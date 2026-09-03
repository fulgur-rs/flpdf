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

        self.assertIn("Known diagnostic limitation (`flpdf-69n1`)", module_doc)
        self.assertIn(
            "`libjpeg-turbo-rs` 0.8.0 parser does not expose the reserved marker byte",
            module_doc,
        )
        self.assertIn("Unsupported marker type 0xNN", module_doc)
        self.assertIn("Do not\n//! fabricate that byte", module_doc)
        self.assertIn("`qpdf-libjpeg-compat` feature", module_doc)
        self.assertIn("Correctness fix (`flpdf-401z`)", module_doc)
        self.assertIn("scans marker segments", module_doc)
        self.assertIn("rejects reserved marker codes", module_doc)
        self.assertIn("accept/reject gap", module_doc)

        dct_section = correspondence[
            correspondence.index("| `Pl_DCT.cc` (buffer/decode)") : correspondence.index(
                "`/ID` が qpdf と非 parity"
            )
        ]
        self.assertIn("libqpdf/Pl_DCT.cc:24-31,83-142", dct_section)
        self.assertIn("JERR_UNKNOWN_MARKER", dct_section)
        self.assertIn("InvalidMarker", dct_section)
        self.assertIn("`flpdf-69n1`", dct_section)
        self.assertIn("`flpdf-401z`", dct_section)
        self.assertIn("accept/reject", dct_section)
        self.assertIn("pre-pass", dct_section)
        self.assertIn("qpdf-libjpeg-compat", dct_section)


if __name__ == "__main__":
    unittest.main()
