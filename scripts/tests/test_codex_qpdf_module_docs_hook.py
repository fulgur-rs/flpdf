from __future__ import annotations

from contextlib import contextmanager
import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
CHECKER_PATH = ROOT / "scripts" / "qpdf-module-docs.py"
HOOK_PATH = ROOT / "scripts" / "codex-hooks" / "qpdf_module_docs.py"


class QpdfModuleDocsHookTests(unittest.TestCase):
    @contextmanager
    def synthetic_repository(self, classification: str):
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            source_path = root / "crates" / "flpdf" / "src" / "lib.rs"
            source_path.parent.mkdir(parents=True)
            source_path.write_text(classification + "\npub struct Synthetic;\n")

            checker_path = root / "scripts" / "qpdf-module-docs.py"
            checker_path.parent.mkdir()
            shutil.copy2(CHECKER_PATH, checker_path)
            subprocess.run(
                ["git", "init", "--quiet"],
                cwd=root,
                check=True,
                capture_output=True,
                text=True,
            )

            yield root, source_path.parent

    def run_hook(self, root: Path, cwd: Path) -> subprocess.CompletedProcess[str]:
        payload = {
            "hook_event_name": "PostToolUse",
            "tool_name": "apply_patch",
            "cwd": str(cwd),
        }
        return subprocess.run(
            [sys.executable, str(HOOK_PATH)],
            input=json.dumps(payload),
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=False,
        )

    def test_valid_module_is_silent(self):
        with self.synthetic_repository(
            "//! qpdf correspondence: valid module."
        ) as (root, cwd):
            subprocess.run(
                [sys.executable, str(CHECKER_PATH), "--root", str(root), "--write"],
                check=True,
                capture_output=True,
                text=True,
            )
            result = self.run_hook(root, cwd)

        self.assertEqual(0, result.returncode)
        self.assertEqual("", result.stdout)

    def test_invalid_terminal_period_returns_block_feedback(self):
        with self.synthetic_repository(
            "//! qpdf correspondence: missing terminal period"
        ) as (root, cwd):
            result = self.run_hook(root, cwd)

        self.assertEqual(0, result.returncode)
        feedback = json.loads(result.stdout)
        self.assertEqual("block", feedback["decision"])
        self.assertIn(
            "crates/flpdf/src/lib.rs: classification must end with a terminal period",
            feedback["reason"],
        )

    def test_non_repository_is_silent(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            result = self.run_hook(root, root)

        self.assertEqual(0, result.returncode)
        self.assertEqual("", result.stdout)

    def test_non_post_tool_event_is_silent(self):
        payload = {
            "hook_event_name": "SessionStart",
            "tool_name": "apply_patch",
            "cwd": str(ROOT),
        }
        result = subprocess.run(
            [sys.executable, str(HOOK_PATH)],
            input=json.dumps(payload),
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=False,
        )

        self.assertEqual(0, result.returncode)
        self.assertEqual("", result.stdout)


if __name__ == "__main__":
    unittest.main()
