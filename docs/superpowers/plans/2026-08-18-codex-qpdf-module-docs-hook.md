# Codex qpdf module docs hook Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Run the qpdf module correspondence checker after Codex edits and return the exact classification diagnostic to Codex when the check fails.

**Architecture:** A project-local `.codex/hooks.json` matches synchronous `PostToolUse` events for `Bash` and `apply_patch`. The command invokes a tracked Python adapter, which resolves the Git root from the hook payload, runs the existing `scripts/qpdf-module-docs.py --check`, and emits a Codex `decision: "block"` feedback object only when the checker fails.

**Tech Stack:** Python 3 standard library, Codex command hooks JSON, existing qpdf module documentation checker, `unittest`, temporary Git repositories.

## Global Constraints

- The checker remains the authority for classification syntax, source-tree coverage, and generated-index freshness.
- The hook must not auto-fix source annotations or rewrite `docs/qpdf-module-doc-index.md`.
- The hook is synchronous and uses a 30-second timeout.
- The matcher is exactly `^(Bash|apply_patch)$`; `PreToolUse` is not used for post-edit validation.
- The adapter is a no-op outside a repository containing `scripts/qpdf-module-docs.py` and `crates/flpdf/src`.
- Successful checks emit no stdout; failed checks emit compact JSON feedback containing checker diagnostics.
- The existing `scripts/tests/test_qpdf_module_docs.py` and CI commands remain unchanged.
- Do not modify or commit `main`; every commit is made only on `feature/flpdf-svta-codex-qpdf-hook`.
- Use only Python standard-library modules; do not add dependencies.

---

### Task 1: Build the hook adapter contract with failing integration tests

**Files:**
- Create: `scripts/tests/test_codex_qpdf_module_docs_hook.py`
- Read: `scripts/qpdf-module-docs.py`
- Read: `docs/superpowers/specs/2026-08-18-codex-qpdf-module-docs-hook-design.md`

**Interfaces:**
- Consumes: JSON on stdin with `hook_event_name`, `tool_name`, and `cwd` fields.
- Produces: a process exit status of `0`; empty stdout for a successful or irrelevant event, or a JSON object with `decision: "block"` and `reason` for a failed repository check.

- [ ] **Step 1: Write the failing adapter integration tests**

  Create unittest helpers that copy the real checker into a temporary repository, create
  `crates/flpdf/src/lib.rs`, initialize Git, and invoke the future adapter as a subprocess. Use a
  nested `cwd` so the test proves Git-root resolution rather than relying on the repository root.
  The core helper and assertions should follow this shape:

  ```python
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
      with self.synthetic_repository("//! qpdf correspondence: valid module.\n") as (root, cwd):
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
      with self.synthetic_repository("//! qpdf correspondence: missing terminal period\n") as (root, cwd):
          result = self.run_hook(root, cwd)

      self.assertEqual(0, result.returncode)
      feedback = json.loads(result.stdout)
      self.assertEqual("block", feedback["decision"])
      self.assertIn(
          "crates/flpdf/src/lib.rs: classification must end with a terminal period",
          feedback["reason"],
      )

  def test_non_repository_is_silent(self):
      with tempfile.TemporaryDirectory() as tmp:
          result = self.run_hook(Path(tmp), Path(tmp))

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
  ```

  The temporary repository helper must use `git init --quiet`, copy the real checker, and generate
  the valid index with the checker itself. It must not modify the real worktree.

- [ ] **Step 2: Run the focused tests and verify the RED failure**

  Run:

  ```bash
  python3 -m unittest scripts/tests/test_codex_qpdf_module_docs_hook.py
  ```

  Expected: the tests fail because `scripts/codex-hooks/qpdf_module_docs.py` does not exist yet.
  A missing-file subprocess error is the expected feature-missing failure; fix the test harness if
  any failure occurs before the adapter is invoked.

- [ ] **Step 3: Commit the failing contract tests on the feature branch**

  Confirm the branch before staging:

  ```bash
  git branch --show-current
  ```

  Expected output: `feature/flpdf-svta-codex-qpdf-hook`.

  Then run:

  ```bash
  git add scripts/tests/test_codex_qpdf_module_docs_hook.py
  git commit -m "test: define Codex qpdf module docs hook contract"
  ```

### Task 2: Implement the repository-aware checker adapter

**Files:**
- Create: `scripts/codex-hooks/qpdf_module_docs.py`
- Test: `scripts/tests/test_codex_qpdf_module_docs_hook.py`

**Interfaces:**
- Consumes: one JSON object from stdin; `cwd` is the session working directory.
- Produces: `main(stdin=sys.stdin, stdout=sys.stdout) -> int`; no output on success or irrelevant input, and a JSON `decision: "block"` object on checker failure.
- Internal boundary: `_repository_root(cwd: str | None) -> Path | None`, `_run_checker(root: Path) -> subprocess.CompletedProcess[str]`, and `_feedback(result: subprocess.CompletedProcess[str]) -> dict[str, object]`.

- [ ] **Step 1: Implement root detection and no-op behavior**

  Resolve the payload's `cwd` with `git -C "$cwd" rev-parse --show-toplevel`, catching both invalid
  JSON/paths and `OSError` without emitting feedback. Treat a missing `cwd`, a non-Git directory,
  and a Git root without both required repository paths as successful no-ops.

  The adapter must verify these paths before running the checker:

  ```python
  checker = root / "scripts/qpdf-module-docs.py"
  source_root = root / "crates/flpdf/src"
  if not checker.is_file() or not source_root.is_dir():
      return 0
  ```

- [ ] **Step 2: Implement the checker subprocess without a shell**

  Run the existing checker with an argument list, the adapter's `sys.executable`, and `cwd=root`:

  ```python
  result = subprocess.run(
      [sys.executable, str(checker), "--check"],
      cwd=root,
      capture_output=True,
      text=True,
      check=False,
  )
  ```

  Return `0` and no stdout when `result.returncode == 0`. Do not pass `--write` and do not modify
  any file.

- [ ] **Step 3: Implement bounded Codex feedback for checker failures**

  Prefer non-empty stderr, then stdout, then `qpdf module documentation check failed` as the
  diagnostic. Limit the diagnostic to 4000 characters and append `...` when truncating. Print one
  JSON object with `ensure_ascii=False` and a final newline:

  ```python
  {
      "decision": "block",
      "reason": "qpdf module documentation check failed:\n" + diagnostic,
      "hookSpecificOutput": {
          "hookEventName": "PostToolUse",
          "additionalContext": "Fix the qpdf correspondence annotation before continuing.",
      },
  }
  ```

  Keep the adapter's own process exit status at `0` after emitting this JSON so Codex consumes the
  structured PostToolUse feedback rather than treating the hook itself as a crashed command.

- [ ] **Step 4: Run the focused tests and verify GREEN**

  Run:

  ```bash
  python3 -m unittest scripts/tests/test_codex_qpdf_module_docs_hook.py
  ```

  Expected: all adapter integration tests pass, including the exact terminal-period diagnostic.

- [ ] **Step 5: Commit the adapter implementation**

  Confirm `git branch --show-current` still prints `feature/flpdf-svta-codex-qpdf-hook`, then run:

  ```bash
  git add scripts/codex-hooks/qpdf_module_docs.py scripts/tests/test_codex_qpdf_module_docs_hook.py
  git commit -m "feat: add Codex qpdf module docs checker hook"
  ```

### Task 3: Register the project-local PostToolUse hook

**Files:**
- Create: `.codex/hooks.json`
- Modify: `scripts/tests/test_codex_qpdf_module_docs_hook.py`

**Interfaces:**
- Consumes: Codex lifecycle events matching `Bash` or `apply_patch`.
- Produces: synchronous invocation of `scripts/codex-hooks/qpdf_module_docs.py` with the session working directory available to the adapter.

- [ ] **Step 1: Add the failing hooks.json contract test**

  Extend the focused unittest to load `.codex/hooks.json` with `json.loads` and assert the exact
  registration contract:

  ```python
  def test_project_hook_registers_synchronous_post_tool_use_checker(self):
      config = json.loads((ROOT / ".codex/hooks.json").read_text(encoding="utf-8"))
      group = config["hooks"]["PostToolUse"][0]
      command_hook = group["hooks"][0]

      self.assertEqual("^(Bash|apply_patch)$", group["matcher"])
      self.assertEqual("command", command_hook["type"])
      self.assertIn("git rev-parse --show-toplevel", command_hook["command"])
      self.assertIn("scripts/codex-hooks/qpdf_module_docs.py", command_hook["command"])
      self.assertEqual(30, command_hook["timeout"])
      self.assertNotIn("async", command_hook)
  ```

- [ ] **Step 2: Run the config test and verify the RED failure**

  Run:

  ```bash
  python3 -m unittest scripts/tests/test_codex_qpdf_module_docs_hook.py
  ```

  Expected: only the new config assertion fails because `.codex/hooks.json` is not present.

- [ ] **Step 3: Add the project-local hooks.json**

  Add exactly one `PostToolUse` matcher group. The command must resolve from Git root so sessions
  started below the repository root still locate the adapter:

  ```json
  {
    "description": "Validate qpdf module correspondence after repository edits.",
    "hooks": {
      "PostToolUse": [
        {
          "matcher": "^(Bash|apply_patch)$",
          "hooks": [
            {
              "type": "command",
              "command": "python3 \"$(git rev-parse --show-toplevel)/scripts/codex-hooks/qpdf_module_docs.py\"",
              "timeout": 30,
              "statusMessage": "Checking qpdf module documentation"
            }
          ]
        }
      ]
    }
  }
  ```

  If the environment rejects writes to the existing read-only `.codex` directory, stop and use
  the approved elevated file-edit path; do not change directory permissions or write to
  `~/.codex/config.toml`.

- [ ] **Step 4: Run the focused tests and verify GREEN**

  Run:

  ```bash
  python3 -m unittest scripts/tests/test_codex_qpdf_module_docs_hook.py
  ```

  Expected: all adapter and configuration tests pass.

- [ ] **Step 5: Commit only the project hook configuration and test update**

  Confirm the feature branch, then run:

  ```bash
  git add .codex/hooks.json scripts/tests/test_codex_qpdf_module_docs_hook.py
  git commit -m "chore: register qpdf module docs PostToolUse hook"
  ```

### Task 4: Run repository verification and prepare hook activation

**Files:**
- Verify: `.codex/hooks.json`
- Verify: `scripts/codex-hooks/qpdf_module_docs.py`
- Verify: `scripts/tests/test_codex_qpdf_module_docs_hook.py`
- Verify: `scripts/tests/test_qpdf_module_docs.py`

**Interfaces:**
- Consumes: the committed feature-branch hook and existing checker/test suite.
- Produces: verified local behavior and a clean feature-branch handoff; no changes to `main`.

- [ ] **Step 1: Run all qpdf module documentation tests**

  Run:

  ```bash
  python3 -m unittest scripts/tests/test_codex_qpdf_module_docs_hook.py scripts/tests/test_qpdf_module_docs.py
  ```

  Expected: all tests pass, including the existing 55-test checker suite and the new hook tests.

- [ ] **Step 2: Run the production checker and formatting gate**

  Run:

  ```bash
  python3 scripts/qpdf-module-docs.py --check
  cargo fmt -- --check
  git diff --check
  ```

  Expected: the checker exits 0, Rust formatting is clean, and Git reports no whitespace errors.

- [ ] **Step 3: Inspect branch isolation and changed files**

  Run:

  ```bash
  git branch --show-current
  git status --short --branch
  git log --oneline --decorate -4
  git diff --stat main...HEAD
  ```

  Expected: the current branch is `feature/flpdf-svta-codex-qpdf-hook`, `main` remains at the
  pre-task/origin commit, and the diff contains only the design/plan plus hook implementation and
  focused tests.

- [ ] **Step 4: Review and trust the hook in Codex**

  In a Codex session rooted at this repository, open `/hooks`, review the command definition, and
  trust the project-local hook. Use a real `apply_patch` edit that temporarily removes the period
  from a synthetic or disposable module only if manual protocol verification is needed; restore
  the file and rerun the checker. Do not intentionally leave the repository invalid.

- [ ] **Step 5: Close and push only the feature-branch work**

  After the user accepts the verified result, run:

  ```bash
  bd close flpdf-svta
  bd dolt push
  git push -u origin feature/flpdf-svta-codex-qpdf-hook
  ```

  Do not push `main`, force-update `main`, or merge the feature branch as part of this task.
