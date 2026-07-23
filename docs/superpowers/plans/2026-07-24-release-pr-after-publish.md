# Release PR Queue-Ordering Gate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Prevent a later active Release-plz run from updating a Release PR
while an older release run is still queued, without relying on GitHub's queued
run ordering.

**Architecture:** Retain the per-ref workflow queue and in-run publish
dependency. Add a fail-closed gate before `release-pr` that queries queued runs
for the same workflow and branch. The active run yields when any queued run has
a lower `run_number`; otherwise it may maintain the next Release PR.

**Tech Stack:** GitHub Actions YAML, GitHub Actions workflow-runs REST API,
`gh`, `jq`, Rust integration tests, Ruby/Psych YAML parsing

## Global Constraints

- Ordinary pushes to `main` must run `release-pr` when no older run is queued.
- A real release must publish successfully before its own `release-pr`.
- A run must skip `release-pr` when an older run is queued.
- API or JSON-processing failures must fail closed.
- The gate receives only `actions: read`.
- `release` continues to depend only on `check-releases`.
- Existing workflow-level and job-level concurrency blocks remain.
- Review thread 2 about `docs/superpowers/...` remains outside scope.

---

### Task 1: Add the queue-ordering contract

**Files:**
- Modify: `crates/flpdf-cli/tests/release_workflow_contract.rs`

- [x] Add a test requiring:
  - a `check-release-pr-turn` job;
  - dependencies on `check-releases` and `release`;
  - job-level `actions: read`;
  - a queued workflow-runs API query for the current branch;
  - comparison of queued `run_number` values with `github.run_number`;
  - a `should_run` job output;
  - `release-pr` dependence on the gate and a successful `should_run` result.
- [x] Run the focused test and verify RED because the gate is absent.

### Task 2: Implement the fail-closed gate

**Files:**
- Modify: `.github/workflows/release-plz.yml`

- [x] Add `check-release-pr-turn` after `check-releases`.
- [x] Reuse the existing release-state eligibility condition so ordinary pushes
  are eligible after a skipped `release`, while failed releases are not.
- [x] Query up to 100 queued runs for `release-plz.yml` and the current branch.
- [x] Emit `should_run=false` when a lower `run_number` exists; otherwise emit
  `should_run=true`.
- [x] Use `set -euo pipefail` and emit no permissive output after failures.
- [x] Make `release-pr` depend on the gate and require its successful true
  output.
- [x] Run the contract test and verify GREEN.

### Task 3: Verify and publish

- [x] Parse the workflow with Ruby/Psych.
- [x] Check for `actionlint` (not installed in this environment).
- [x] Run formatting, clippy, strict rustdoc, workspace tests, and
  `git diff --check`.
- [ ] Commit and push the workflow, contract test, and updated plan.
- [ ] Verify PR checks, then close `flpdf-116l` and push Beads state.
