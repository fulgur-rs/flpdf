# Pre-PR Patch-Coverage Gate — Design

- Date: 2026-06-10
- Issue: flpdf-i22l
- Status: accepted

## Problem

CI already measures workspace coverage with `cargo-llvm-cov` and uploads it to
Codecov (`.github/workflows/ci.yml` `coverage` job), but coverage is **measured,
not enforced** — `fail_ci_if_error: false` and there is no `codecov.yml`
threshold. The development workflow therefore has no step that verifies, *before
a PR is opened*, that the changed code is actually exercised by tests. Codecov
reports only after the PR exists.

Goal: add a step to the agent workflow that verifies changed-line ("patch")
coverage is sufficient before `gh pr create`, with `flpdf` (the library crate)
held to 100%.

## Decisions

| Lever | Decision |
| --- | --- |
| Mechanism | **Agent procedure** — wired into `CLAUDE.md`, backed by a committed helper script. No CI/hook changes this round. |
| Criterion | **Numeric (changed-line coverage) + qualitative** (edge / error-path reasoning). |
| Numeric automation | **Committed helper script** that intersects `git diff` added lines with `cargo-llvm-cov` uncovered lines. |
| 100% scope | `flpdf/src` changed lines = **hard gate** (must be 100%). `flpdf-cli/src` = **report-only** (best-effort). |
| Escape hatch | `// cov:ignore` (line) / `// cov:ignore-start` … `// cov:ignore-end` (block) markers, each with a reason comment, are subtracted from the gate so it stays reproducible. |

Explicitly **out of scope** (YAGNI): CI changes, `codecov.yml` thresholds,
`PreToolUse` hooks. The script is shaped so a future CI job can call it
unchanged.

## Component 1 — `scripts/patch-coverage.sh`

Lists the changed lines in the PR that are not executed by any test, and gates
`flpdf`.

Processing:
1. **Coverage**: by default run
   `cargo llvm-cov --workspace --lcov --output-path target/patch-cov.lcov`
   (LCOV `DA:<line>,<hits>` records = per-file `line → hit count`; matches the
   format CI already produces). `--lcov <path>` reuses an existing report to skip
   the expensive instrumented rebuild.
2. **Changed lines**: `git diff --unified=0 <base>...HEAD` added-line ranges.
   Base defaults to `origin/main`; `--base <ref>` overrides it (stacked PRs pass
   the parent branch).
3. **Intersect** added lines × uncovered lines. Exclude `tests/` files and lines
   covered by `// cov:ignore` markers (read from source).

Per-crate handling:
- `flpdf/src/**`: any uncovered changed line → **FAIL (exit ≠ 0)**. Target 100%.
- `flpdf-cli/src/**`: uncovered changed lines are **reported only** (no effect on
  exit code).

Why measure the **whole workspace** but gate only `flpdf`: `flpdf-cli` tests
drive `flpdf` code, so a crate-scoped coverage run would under-count `flpdf` and
produce false "uncovered" hits. The diff/gate is scoped to `flpdf` while the
measurement stays workspace-wide.

Example output:

```
flpdf      : changed 42, uncovered 0   -> PASS (100%)
flpdf-cli  : changed 18, uncovered 3   -> report-only
  crates/flpdf-cli/src/args.rs: 88, 89, 140
FAIL: flpdf has uncovered changed lines   (only when flpdf has gaps)
```

## Component 2 — Workflow integration

### A. `CLAUDE.md` Session Completion quality gate

Insert a pre-PR step:

```
2.5. Verify patch test coverage (before opening a PR)
     - Run: scripts/patch-coverage.sh [--base <parent-branch>]
     - flpdf: changed lines MUST be 100% covered (gate). Add tests for any
       uncovered changed line, OR mark genuinely untestable lines with
       `// cov:ignore` + a reason comment.
     - flpdf-cli: review the report; add tests where reasonable (best-effort).
     - Then apply the qualitative check below before `gh pr create`.
```

### B. Qualitative check (after the numeric pass)

100% line execution only proves a line *ran*. The procedure also requires
confirming that new/changed public behavior has tests for its **error arms,
boundary values, and empty/extreme inputs** — a weak assertion over a covered
line is not enough. Any `// cov:ignore` added must be justified in one line in
the PR description.

### C. Placement

The authoritative step lives in always-loaded `CLAUDE.md`. The script is
self-contained, so stacked-PR / `stacked-epic-impl` skills can reference it with
a single line (`run scripts/patch-coverage.sh`).

## Verification

- Script runs against a synthetic diff and a real branch; confirm uncovered
  changed lines are correctly listed and that `flpdf` gaps fail while `flpdf-cli`
  gaps only report.
- Confirm `// cov:ignore` line and block markers are subtracted.
- Confirm `--base` and `--lcov` options behave as specified.
