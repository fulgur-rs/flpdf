# Release PR After Publish Design

## Problem

When a release-plz Release PR is merged, the `release-pr` and `release` jobs in
`.github/workflows/release-plz.yml` run concurrently. `release-pr` compares the
version in `main` with crates.io before `release` has published that version, so
it immediately opens a new Release PR for the version that was just merged.

The v0.3.0 release run demonstrated the race:

- `release-pr` observed local v0.3.0 and registry v0.2.1.
- It opened PR #514 for v0.3.0.
- The parallel `release` job published v0.3.0 about 90 seconds later.
- A subsequent push to `main` updated the existing PR to the real next version.

## Considered Approaches

### 1. Run `release-pr` after `release` when publishing

Make `release-pr` depend on both `check-releases` and `release`. Use an explicit
job condition so ordinary pushes still run `release-pr` when `release` is
skipped, while release pushes wait for a successful publish.

This is the selected approach. It keeps the next Release PR available
immediately after a successful release and changes only job orchestration.

### 2. Skip `release-pr` on release pushes

This removes the race, but leaves no next Release PR until another commit lands
on `main`. It is simpler but unnecessarily delays release preparation.

### 3. Trigger a separate workflow after publishing

A `release: published` or `workflow_run` workflow can create the next Release
PR, but introduces another workflow, token-trigger behavior, and cross-workflow
failure handling for a dependency that can be expressed within the current run.

## Selected Job Semantics

`release` continues to depend only on `check-releases`; publishing must never
depend on maintenance of the next Release PR.

`release-pr` directly depends on both `check-releases` and `release` and uses an
`always()`-guarded condition with these outcomes:

| Event state | `release` result | `release-pr` |
| --- | --- | --- |
| Ordinary `main` push | `skipped` | Run |
| Successful Release PR merge | `success` | Run after publish |
| Failed or rejected release | `failure` or `cancelled` | Skip |
| Non-`main` manual dispatch | any | Skip |

The release environment approval therefore blocks only a real release and the
dependent next-PR maintenance. Ordinary pushes do not wait for an approval.

## Failure Handling

If `check-releases` fails, `release-pr` must not run because its release-state
input is unknown. If a real release fails or is rejected, `release-pr` must not
run because crates.io may still be behind `main`. The existing `release`
concurrency group and `release-pr` concurrency group remain unchanged.

## Verification

Add a repository test that parses the workflow as text and asserts the
dependency and condition contract:

- `release-pr` needs `check-releases` and `release`.
- Its condition permits an ordinary push with a skipped release.
- Its condition requires a successful release when `has_releases` is true.
- `release` still needs only `check-releases`.

Also run a YAML parser/action linter if one is already available in the
repository, then run formatting and the workspace tests required by the
repository gates.

