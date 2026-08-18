# Codex PostToolUse hook for qpdf module documentation design

**Issue:** `flpdf-svta`

**Date:** 2026-08-18

## Goal

Detect qpdf module correspondence annotation violations immediately after Codex edits the
repository. In particular, an edit such as a missing terminal period in
`crates/flpdf/src/json/input.rs` must surface the existing checker diagnostic:

```text
crates/flpdf/src/json/input.rs: classification must end with a terminal period
```

The hook must provide actionable feedback to Codex without modifying the source or generated
index automatically.

## Scope

The change adds:

- a project-local `.codex/hooks.json` configuration;
- a tracked Python hook adapter under `scripts/codex-hooks/`;
- focused tests for hook input handling, successful validation, and checker failure feedback.

The existing `scripts/tests/test_qpdf_module_docs.py` unittest and the CI commands remain in
place. The hook invokes the production checker directly because it is the fast path that emits
the exact repository diagnostic; the unittest remains the regression suite for the checker and
its parser/generator behavior.

The change does not:

- auto-fix annotations or rewrite `docs/qpdf-module-doc-index.md`;
- replace the CI quality checks;
- install or modify the user's global `~/.codex` configuration;
- add a `PreToolUse` validator that attempts to infer the post-edit file contents.

## Alternatives considered

### Project-local hook and adapter — selected

Store the hook definition in `.codex/hooks.json` and keep the implementation in the repository.
The command resolves the Git root at runtime, so it works when Codex starts in a subdirectory and
when a linked worktree has a different absolute path. The adapter reads the standard hook JSON
from stdin and invokes the existing checker without a shell-built command line.

This is versioned, reviewable, and portable across worktrees. It requires the user to review and
trust the project-local command hook through Codex's `/hooks` UI before it runs.

### User-global hook

Put a hook in `~/.codex/hooks.json` that points directly at `/home/ubuntu/flpdf`. This is useful
for a single machine, but it is not part of the repository, does not travel with the project, and
would require a separate configuration for each checkout or worktree.

### Inline checker command

Call `python3 scripts/qpdf-module-docs.py --check` directly from `hooks.json`. This has fewer
files, but it leaves JSON parsing, Git-root resolution, repository detection, diagnostic shaping,
and non-repository no-op behavior in a shell command. The adapter keeps those boundaries testable.

## Lifecycle and data flow

1. Codex runs the synchronous `PostToolUse` hook after `Bash` or `apply_patch`.
2. The adapter reads the hook event JSON from stdin and uses its `cwd` field as the starting point
   for `git -C "$cwd" rev-parse --show-toplevel`.
3. If the resolved root does not contain this repository's checker and flpdf source tree, the
   adapter exits successfully without output.
4. For this repository, it runs `python3 scripts/qpdf-module-docs.py --check` with the Git root as
   the subprocess working directory.
5. A successful check exits with no stdout, so Codex proceeds normally.
6. A failed check emits a compact JSON `decision: "block"` response whose reason contains the
   checker's stderr. Codex replaces the completed tool result with that feedback and continues the
   model loop; the already-applied edit is not undone.

The hook is deliberately synchronous. An asynchronous hook could report the error after Codex
has already started the next model step, weakening the immediate feedback loop.

## Hook configuration

The project-local configuration uses one matcher group:

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

The root-based command path avoids dependence on the session's current subdirectory. Codex's
project trust and hook review flow remains the activation gate; the repository cannot silently
enable an unreviewed command on another user's machine.

## Adapter behavior and error boundary

The adapter will:

- accept the standard JSON stdin object and ignore non-`PostToolUse` events when invoked directly;
- no-op successfully when `cwd` is absent, is not inside a Git worktree, or resolves to a
  different repository;
- invoke the checker with an argument list and `sys.executable`, preserving paths with spaces;
- return no output for a zero exit status;
- return a JSON block feedback object for a checker failure, preferring stderr and falling back to
  stdout or a generic message;
- bound the returned diagnostic text so a malformed repository cannot flood the model context;
- avoid writing files, staging changes, or changing the generated index.

The checker remains the authority for classification syntax, source-tree coverage, and generated
index freshness. The adapter only transports its result into the Codex hook protocol.

## Testing

The focused Python tests will exercise the real adapter subprocess against temporary Git roots:

- a non-repository or unrelated repository is a successful no-op;
- a valid synthetic qpdf module and generated index produce empty stdout and exit 0;
- a missing terminal period produces exit 0 with parseable JSON `decision: "block"` feedback and
  the exact `classification must end with a terminal period` diagnostic;
- the hook configuration has the expected `PostToolUse` matcher and synchronous command handler.

Existing tests continue to run unchanged, including the repository policy test that scans all
real flpdf modules. Verification will include:

```text
python3 -m unittest scripts/tests/test_codex_qpdf_module_docs_hook.py
python3 -m unittest scripts/tests/test_qpdf_module_docs.py
python3 scripts/qpdf-module-docs.py --check
```

The normal repository quality gates remain applicable after implementation.

## Operational activation

After the project-local hook is added, open Codex's `/hooks` view, review the new command, and
trust it. Until that review is completed, Codex may list the hook but skip running it. This is an
intentional safety property of command hooks.
