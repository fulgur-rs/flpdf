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

This is necessary but not sufficient. GitHub Actions does not guarantee that
queued runs enter the concurrency group in creation order, so a later ordinary
push can become active before an earlier release run.

### 2. Put only `release` and `release-pr` in a shared job concurrency group

This avoids blocking `check-releases`, but job readiness can differ across runs.
A later ordinary run's `release-pr` may enter the group before an earlier
release run's publish job, so it does not provide the required ordering.

### 3. Wait for workflow or deployment state before `release-pr`

An API-based wait can detect an active publish, but requires extra permissions,
runner time while approvals are pending, timeout policy, and protection against
check-then-act races.

### 4. Serialize runs and skip a run when an older run is queued

Keep the workflow queue from approach 1, then gate `release-pr` by querying the
workflow-runs API. If the active run sees any queued run with a lower
`run_number`, it skips `release-pr` and completes, allowing the older run to
become active.

This is the selected approach. It does not assume an ordering among queued runs:
whichever run becomes active either yields to an older queued run or proceeds
when no older queued run exists.

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

After `check-releases` and `release`, an eligible run executes a
`check-release-pr-turn` gate with job-level `actions: read` permission. The gate
requests queued runs for this workflow and branch, then compares their
`run_number` values with `github.run_number`:

- If any queued run has a lower number, output `should_run=false`.
- Otherwise, output `should_run=true`.
- If the API request or comparison fails, the job fails and `release-pr` stays
  skipped.

The query requests up to 100 queued runs, matching the workflow's `queue: max`
capacity. The top-level concurrency group ensures that a queued run cannot
change to active between the gate and `release-pr`, so the decision does not
introduce a check-then-act race.

`release-pr` directly depends on `check-releases`, `release`, and the gate. Its
`always()`-guarded condition requires `should_run=true` in addition to the
existing release-state checks:

| Event state | `release` result | Older queued run | `release-pr` |
| --- | --- | --- | --- |
| Ordinary `main` push | `skipped` | No | Run |
| Successful Release PR merge | `success` | No | Run after publish |
| Any eligible run | `skipped` or `success` | Yes | Skip and yield |
| Failed or rejected release | `failure` or `cancelled` | any | Skip |
| Non-`main` manual dispatch | any | any | Skip |

Within a run, the release environment approval still occurs only for a real
release. Across runs, repeatedly yielding to lower run numbers causes the oldest
queued run to proceed before any later run can inspect crates.io.

## Failure Handling

If `check-releases` fails, `release-pr` must not run because its release-state
input is unknown. If a real release fails or is rejected, `release-pr` must not
run because crates.io may still be behind `main`. Queued workflow runs proceed
after the failed run completes; persistent recovery from a failed publication
remains an operator concern and is outside the pending-publication race fixed
here.

If GitHub's workflow-runs API is unavailable, the gate fails closed rather than
allowing a possibly stale Release PR update. The next queued or manually
rerun workflow can retry. The existing job-level `release` and `release-pr`
concurrency groups remain unchanged as defense in depth.

## Verification

Add a repository test that parses the workflow as text and asserts the
dependency and condition contract:

- `release-pr` needs `check-releases` and `release`.
- `release-pr` also needs the turn-check gate and requires its `should_run`
  output.
- Its condition permits an ordinary push with a skipped release.
- Its condition requires a successful release when `has_releases` is true.
- `release` still needs only `check-releases`.
- The complete workflow is serialized per ref without cancelling running work.
- Multiple pending runs are retained with `queue: max`.
- The gate has only `actions: read`, queries queued runs on the current branch,
  and detects lower `run_number` values.
- API or comparison failure prevents `release-pr` from running.

Also run a YAML parser/action linter if one is already available in the
repository, then run formatting and the workspace tests required by the
repository gates.
