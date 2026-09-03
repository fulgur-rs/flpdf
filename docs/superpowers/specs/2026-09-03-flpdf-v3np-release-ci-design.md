# flpdf-v3np Release CI Job Design

## Context

`flpdf-v3np` exists because the CI workflow currently exercises the Rust
workspace only in debug mode. The closed `flpdf-4rfl` bug demonstrated that a
side effect hidden inside `debug_assert!` can change behavior only in release
builds, while the existing CI remains green. The release check therefore needs
to be an actual required CI job, not merely a local recommendation or a
non-gating informational step.

## Goal

Add a required Ubuntu release-test job that runs the complete workspace test
suite with `--release`, while preserving the existing four-OS debug matrix and
the feature-specific qpdf byte-parity gates.

## Non-goals

- Do not add a release profile axis to the four-OS matrix.
- Do not run the release job on every OS.
- Do not enable `qpdf-zlib-compat` in the release job; the existing
  `bytes-identical zlib compat` step remains the owner of those feature-gated
  byte-parity tests.
- Do not change Rust production code or alter the existing debug test matrix.

## Design

Add a standalone `release` job to `.github/workflows/ci.yml`:

1. Depend on `quality`, so malformed workflow/configuration changes fail before
   the release compilation starts.
2. Run on `ubuntu-latest`, which is the existing canonical Linux environment
   for the qpdf-zlib byte gates and avoids multiplying CI cost across all four
   debug platforms.
3. Check out the repository with the same pinned checkout action as the other
   jobs.
4. Install stable Rust `1.97.1` and cache artifacts under a release-specific
   shared key (`ubuntu-latest-flpdf-release`).
5. Install and verify qpdf `11.9.0`, because the complete workspace test suite
   contains qpdf-oracle paths and must not accidentally skip them due to a
   missing executable.
6. Run a bash step with `set -euo pipefail` and exactly
   `cargo test --workspace --release` as the required release test command.

The job is intentionally independent of the debug `test` matrix. This keeps
the profile difference explicit, makes the release-only failure signal easy to
locate, and avoids re-running the qpdf-zlib compatibility gates under a feature
configuration that is not needed to reproduce the original bug class.

## Contract coverage

Extend `crates/flpdf-cli/tests/ci_workflow_contract.rs` with a contract for the
real workflow. The contract must verify that the `release` job:

- exists and is not conditionally disabled or allowed to fail;
- depends on `quality`;
- runs on the literal `ubuntu-latest` runner;
- has a gating bash step containing exactly one executable
  `cargo test --workspace --release` command;
- does not hide the command behind a conditional step, custom working
  directory, control flow, or `continue-on-error`.

The test should fail against the current workflow before the job is added, then
pass after the workflow change. Existing parser and shell-contract helpers
should be reused or generalized only as needed for this job-specific check.

## Verification

In the isolated worktree, run the contract test through RED/GREEN, then run:

- `cargo fmt --all -- --check`
- `cargo test -p flpdf-cli --test ci_workflow_contract`
- `cargo test --workspace --release`
- the relevant workspace checks required by the repository CI contract before
  handoff.

The final change must leave the main checkout's unrelated `a.pdf` untouched.
