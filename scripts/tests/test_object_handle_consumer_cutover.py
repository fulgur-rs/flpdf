"""Contract tests for the check and filespec ObjectHandle cutover."""

from __future__ import annotations

import re
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]


def production_source(path: Path) -> str:
    source = path.read_text(encoding="utf-8")
    source = source.split("#[cfg(test)]", 1)[0]
    return re.sub(r"(?m)^\s*//.*(?:\n|$)", "", source)


class ObjectHandleConsumerCutoverTests(unittest.TestCase):
    def test_filespec_production_uses_only_the_canonical_handle_route(self):
        source = production_source(ROOT / "crates/flpdf/src/filespec_helper.rs")

        for legacy in (
            "crate::object::",
            "Object::",
            "Dictionary",
            ".materialize()",
            "decode_stream_data(",
            "pdf.resolve(",
            "pdf.set_object(",
        ):
            self.assertNotIn(legacy, source, f"filespec production still uses {legacy}")

        self.assertIn("decode_stream_data_from_handle", source)
        self.assertIn("ObjectHandle::dictionary", source)

    def test_check_production_uses_handle_content_streams(self):
        source = production_source(ROOT / "crates/flpdf/src/check.rs")

        for legacy in (
            "crate::{Diagnostic, Diagnostics, Dictionary, Error, Object, Pdf",
            "Object::",
            "page_content_stream_entries(",
            "decode_stream_data_with_limits_and_warnings(",
        ):
            self.assertNotIn(legacy, source, f"check production still uses {legacy}")

        self.assertIn("PageObjectHelper", source)
        self.assertIn("decode_stream_data_recovering_from_handle", source)


if __name__ == "__main__":
    unittest.main()
