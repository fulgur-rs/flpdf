# Release PR After Publish Design

## Problem

When a release-plz Release PR is merged, the `release-pr` and `release` jobs in
`.github/workflows/release-plz.yml` run concurrently. `release-pr` compares the
version in `main` with crates.io before `release` has published that version, so
it immediately opens a new Release PR for the version that was just merged.

Ordering those jobs within one workflow run is necessary but not sufficient.
While that release run is waiting for environment approval or publishing, a
later ordinary push starts another workflow run. Its `release` job is skipped,
so its `release-pr` job can still read the stale crates.io version. Job `needs`
edges do not order jobs across workflow runs.

The v0.3.0 release run demonstrated the race:

- `release-pr` observed local v0.3.0 and registry v0.2.1.
- It opened PR #514 for v0.3.0.
- The parallel `release` job published v0.3.0 about 90 seconds later.
- A subsequent push to `main` updated the existing PR to the real next version.

## Considered Approaches

### 1. Serialize complete workflow runs and preserve the in-run dependency

Keep `release-pr` dependent on both `check-releases` and `release`, and add
workflow-level concurrency keyed by ref. Set `queue: max` so later pushes wait
instead of replacing an already-pending release run.

This is the selected approach. It serializes detection, approval, publishing,
and next-PR maintenance across runs without dropping queued pushes.

### 2. Put only `release` and `release-pr` in a shared job concurrency group

This avoids blocking `check-releases`, but job readiness can differ across runs.
A later ordinary run's `release-pr` may enter the group before an earlier
release run's publish job, so it does not provide the required ordering.

### 3. Poll workflow or deployment state before `release-pr`

An API-based wait can detect an active publish, but requires extra permissions,
runner time while approvals are pending, timeout policy, and protection against
check-then-act races. Native workflow concurrency is smaller and more reliable.

## Selected Workflow Semantics

The workflow uses one concurrency group per ref:

```yaml
concurrency:
  group: release-plz-${{ github.ref }}
  cancel-in-progress: false
  queue: max
```

`queue: max` is required. The default single pending slot replaces an existing
pending run when another push arrives, which could discard the run responsible
for publishing a release. Main pushes and main `workflow_dispatch` runs share a
group; a non-main manual dispatch uses a different group and remains unable to
publish or create a Release PR because of the existing main-ref guards.

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

Within a run, the release environment approval still occurs only for a real
release. At the workflow level, later ordinary pushes intentionally wait behind
that real release so none can inspect crates.io while publication is pending.

## Failure Handling

If `check-releases` fails, `release-pr` must not run because its release-state
input is unknown. If a real release fails or is rejected, `release-pr` must not
run because crates.io may still be behind `main`. Queued workflow runs proceed
after the failed run completes; persistent recovery from a failed publication
remains an operator concern and is outside the pending-publication race fixed
here. The existing job-level `release` and `release-pr` concurrency groups
remain unchanged as defense in depth.

## Verification

Add a repository test that parses the workflow as text and asserts the
dependency and condition contract:

- `release-pr` needs `check-releases` and `release`.
- Its condition permits an ordinary push with a skipped release.
- Its condition requires a successful release when `has_releases` is true.
- `release` still needs only `check-releases`.
- The complete workflow is serialized per ref without cancelling running work.
- Multiple pending runs are retained with `queue: max`.

Also run a YAML parser/action linter if one is already available in the
repository, then run formatting and the workspace tests required by the
repository gates.
