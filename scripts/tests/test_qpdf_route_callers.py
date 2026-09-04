from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "qpdf-route-callers.py"


def write(root: Path, relative: str, body: str) -> None:
    path = root / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body, encoding="utf-8")


def run(root: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(SCRIPT), "--root", str(root), *args],
        capture_output=True,
        text=True,
        check=False,
    )


def counts(root: Path, symbol: str) -> dict:
    result = run(root, "--symbol", symbol, "--json")
    assert result.returncode == 0, result.stdout + result.stderr
    return json.loads(result.stdout)["symbols"][symbol]


class ExclusionRules(unittest.TestCase):
    def test_comment_declaration_use_impl_and_string_are_excluded(self) -> None:
        with tempfile.TemporaryDirectory() as t:
            root = Path(t)
            write(
                root,
                "crates/flpdf/src/a.rs",
                "\n".join(
                    [
                        "// legacy_thing is described here",
                        "/// docs mention legacy_thing too",
                        "use crate::b::legacy_thing;",
                        "pub use crate::b::{",
                        "    legacy_thing,",
                        "    other,",
                        "};",
                        "pub(crate) fn legacy_thing() {}",
                        "impl LegacyThingHolder for legacy_thing {",
                        "}",
                        'let s = "legacy_thing";',
                        "let x = legacy_thing(); // real call",
                        "fn takes(v: legacy_thing) {} // type position counts",
                        "",
                    ]
                ),
            )
            c = counts(root, "legacy_thing")
            self.assertEqual(2, c["prod"], c)
            self.assertEqual(0, c["test"], c)
            self.assertEqual({"crates/flpdf/src/a.rs": 2}, c["prod_files"])

    def test_string_with_escaped_quote_does_not_hide_following_call(self) -> None:
        with tempfile.TemporaryDirectory() as t:
            root = Path(t)
            write(
                root,
                "crates/flpdf/src/a.rs",
                'let s = "a \\" quote"; legacy_thing();\n',
            )
            self.assertEqual(1, counts(root, "legacy_thing")["prod"])

    def test_qualified_symbol_matches_last_segment_only(self) -> None:
        with tempfile.TemporaryDirectory() as t:
            root = Path(t)
            write(root, "crates/flpdf/src/a.rs", "pdf.resolve(&h);\nother.resolve_all();\n")
            self.assertEqual(1, counts(root, "Pdf::resolve")["prod"])


class ProdTestSplit(unittest.TestCase):
    def test_tests_dir_and_suffix_are_test(self) -> None:
        with tempfile.TemporaryDirectory() as t:
            root = Path(t)
            write(root, "crates/flpdf/tests/x.rs", "legacy_thing();\n")
            write(root, "crates/flpdf/src/json/input_tests.rs", "legacy_thing();\n")
            write(root, "crates/flpdf/benches/b.rs", "legacy_thing();\n")
            c = counts(root, "legacy_thing")
            self.assertEqual(0, c["prod"])
            self.assertEqual(3, c["test"])

    def test_cfg_test_module_blocks_are_test_even_when_production_follows(self) -> None:
        with tempfile.TemporaryDirectory() as t:
            root = Path(t)
            write(
                root,
                "crates/flpdf/src/a.rs",
                "\n".join(
                    [
                        "fn one() { legacy_thing(); }",
                        "#[cfg(test)]",
                        "mod tests_a {",
                        "    fn t() { legacy_thing(); let s = \"}\"; }",
                        "}",
                        "fn two() { legacy_thing(); }",
                        "#[cfg(test)]",
                        "mod tests_b {",
                        "    fn t() { legacy_thing(); }",
                        "}",
                        "",
                    ]
                ),
            )
            c = counts(root, "legacy_thing")
            self.assertEqual(2, c["prod"], c)
            self.assertEqual(2, c["test"], c)

    def test_item_level_cfg_test_fn_is_test(self) -> None:
        with tempfile.TemporaryDirectory() as t:
            root = Path(t)
            write(
                root,
                "crates/flpdf/src/a.rs",
                "\n".join(
                    [
                        "#[cfg(test)]",
                        "#[allow(dead_code)]",
                        "pub(crate) fn helper() {",
                        "    legacy_thing();",
                        "}",
                        "fn prod() { legacy_thing(); }",
                        "#[cfg(test)]",
                        "use crate::legacy_thing;",
                        "",
                    ]
                ),
            )
            c = counts(root, "legacy_thing")
            self.assertEqual(1, c["prod"], c)
            self.assertEqual(1, c["test"], c)


class ManifestAndGate(unittest.TestCase):
    def test_manifest_and_expect_zero_gate(self) -> None:
        with tempfile.TemporaryDirectory() as t:
            root = Path(t)
            write(root, "crates/flpdf/src/a.rs", "alive_fn();\n")
            write(root, "crates/flpdf/tests/t.rs", "dead_fn();\n")
            write(
                root,
                "docs/qpdf-route-matrix/tracked-symbols.txt",
                "# comment\nalive_fn\ndead_fn\n\n",
            )
            report = run(root)
            self.assertEqual(0, report.returncode, report.stdout + report.stderr)
            self.assertIn("alive_fn", report.stdout)
            self.assertIn("dead_fn", report.stdout)

            gate = run(root, "--expect-zero")
            self.assertNotEqual(0, gate.returncode)
            self.assertIn("alive_fn", gate.stdout)
            self.assertNotIn("dead_fn: prod", gate.stdout.split("FAILED")[-1])

            only_dead = run(root, "--symbol", "dead_fn", "--expect-zero")
            self.assertEqual(0, only_dead.returncode, only_dead.stdout + only_dead.stderr)
            self.assertNotIn("no occurrence at all", only_dead.stdout)

            absent = run(root, "--symbol", "never_existed", "--expect-zero")
            self.assertEqual(0, absent.returncode, absent.stdout + absent.stderr)
            self.assertIn("no occurrence at all", absent.stdout)
            self.assertIn("never_existed", absent.stdout)

    def test_missing_manifest_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as t:
            root = Path(t)
            write(root, "crates/flpdf/src/a.rs", "x();\n")
            result = run(root)
            self.assertNotEqual(0, result.returncode)
            self.assertIn("tracked-symbols.txt", result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
