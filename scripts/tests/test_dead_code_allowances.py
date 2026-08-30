import re
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
ALLOW_RE = re.compile(r"#\[allow\([^]]*dead_code")
STALE_MARKERS = (
    "production callers land",
    "consumer cutover",
    "deferred consumers",
    "after this prerequisite lands",
    "future ObjectHandle writer route",
    "not-yet-wired",
    "flpdf-25kg.3.5",
    "flpdf-25kg.3.6",
    "flpdf-25kg.3.6.3",
    "flpdf-25kg.3.12",
    "flpdf-25kg.3.25",
    "flpdf-egzr.3.2.5",
)


class DeadCodeAllowanceTests(unittest.TestCase):
    def test_no_dead_code_allowance_uses_completed_cutover_rationale(self):
        offenders = []
        for path in sorted((ROOT / "crates").rglob("*.rs")):
            lines = path.read_text(encoding="utf-8").splitlines()
            for index, line in enumerate(lines):
                if not ALLOW_RE.search(line):
                    continue
                context = "\n".join(lines[index : index + 3]).lower()
                for marker in STALE_MARKERS:
                    if marker.lower() in context:
                        offenders.append(f"{path.relative_to(ROOT)}:{index + 1}: {marker}")
        self.assertEqual(
            [],
            offenders,
            "stale dead_code rationale(s):\n" + "\n".join(offenders),
        )


if __name__ == "__main__":
    unittest.main()
