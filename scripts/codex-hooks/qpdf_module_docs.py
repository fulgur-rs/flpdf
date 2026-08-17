#!/usr/bin/env python3
"""Expose qpdf module documentation check failures to Codex hooks."""

from __future__ import annotations

import json
from pathlib import Path
import subprocess
import sys
from typing import TextIO


_CHECKER_PATH = Path("scripts/qpdf-module-docs.py")
_SOURCE_ROOT = Path("crates/flpdf/src")
_GENERIC_DIAGNOSTIC = "qpdf module documentation check failed"
_MAX_DIAGNOSTIC_LENGTH = 4000
_TRUNCATION_SUFFIX = "..."


def _repository_root(cwd: str | None) -> Path | None:
    """Return the Git root for *cwd*, or ``None`` when it cannot be resolved."""
    if not isinstance(cwd, str):
        return None

    try:
        result = subprocess.run(
            ["git", "-C", cwd, "rev-parse", "--show-toplevel"],
            capture_output=True,
            text=True,
            check=False,
        )
    except (OSError, ValueError):
        return None

    if result.returncode != 0:
        return None

    root_text = result.stdout.strip()
    if not root_text:
        return None
    return Path(root_text)


def _run_checker(root: Path) -> subprocess.CompletedProcess[str]:
    """Run the repository's existing checker without modifying any files."""
    checker = root / _CHECKER_PATH
    return subprocess.run(
        [sys.executable, str(checker), "--check"],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )


def _feedback(result: subprocess.CompletedProcess[str]) -> dict[str, object]:
    """Translate a checker failure into the Codex PostToolUse response format."""
    diagnostic = result.stderr or result.stdout or _GENERIC_DIAGNOSTIC
    if len(diagnostic) > _MAX_DIAGNOSTIC_LENGTH:
        diagnostic = (
            diagnostic[: _MAX_DIAGNOSTIC_LENGTH - len(_TRUNCATION_SUFFIX)]
            + _TRUNCATION_SUFFIX
        )

    return {
        "decision": "block",
        "reason": f"qpdf module documentation check failed:\n{diagnostic}",
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "additionalContext": (
                "Fix the qpdf correspondence annotation before continuing."
            ),
        },
    }


def main(stdin: TextIO = sys.stdin, stdout: TextIO = sys.stdout) -> int:
    """Run the checker for PostToolUse events and emit feedback on failure."""
    try:
        payload = json.load(stdin)
    except (json.JSONDecodeError, OSError, TypeError, ValueError):
        return 0

    if not isinstance(payload, dict) or payload.get("hook_event_name") != "PostToolUse":
        return 0

    root = _repository_root(payload.get("cwd"))
    if root is None:
        return 0

    checker = root / _CHECKER_PATH
    source_root = root / _SOURCE_ROOT
    if not checker.is_file() or not source_root.is_dir():
        return 0

    try:
        result = _run_checker(root)
    except OSError:
        return 0

    if result.returncode == 0:
        return 0

    print(json.dumps(_feedback(result), ensure_ascii=False), file=stdout)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
