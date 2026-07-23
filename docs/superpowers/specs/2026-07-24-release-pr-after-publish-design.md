# Release PR After Publish Design

## Problem

When a release-plz Release PR is merged, `main` contains a version that is not
yet available on crates.io. Any `release-pr` invocation during that interval
compares the new local version with the old registry version and can recreate a
Release PR for the version that was just merged.

Ordering `release-pr` after `release` within the merge workflow run fixes only
that run. An ordinary push can start another run while publication is awaiting
environment approval or in progress.

The previous cross-run queue design is not a safe foundation:

- GitHub concurrency waiters have `pending` status, distinct from `queued`.
- Their execution order is not guaranteed by workflow dispatch order.
- Skipping a later run permanently can omit its changes from the next Release
  PR when no further push occurs.
- Waiting inside the active workflow would deadlock because that workflow holds
  the concurrency group needed by the pending release run.

## Required Semantics

- A Release PR merge publishes successfully before its own `release-pr`.
- An ordinary push never runs `release-pr` against an unpublished version from
  `main`.
- Changes pushed while publication is pending are included in the next Release
  PR without requiring another push.
- A failed or rejected publication cannot update the next Release PR.
- Publishing remains independent from maintenance of the next Release PR.
- API and registry failures fail closed.

## Considered Approaches

### 1. Validate the checked-out version against crates.io

Remove workflow-level serialization. Serialize only `release-pr` jobs, retain
all pending jobs, check out the latest `main`, and run release-plz only when
every publishable workspace crate at that snapshot's version exists on
crates.io.

This is the selected approach. It validates the actual safety condition instead
of inferring it from workflow scheduling state.

### 2. Split publishing and Release PR maintenance into separate workflows

A completion-triggered maintenance workflow could run after publishing. This
would be robust but changes manual-dispatch behavior, permissions, workflow
ownership, and release documentation more broadly than this bug requires.

### 3. Query pending runs and redispatch yielded runs

The workflow could query both `pending` and `queued` runs and redispatch itself
after yielding. This needs Actions write permission, recursive-run controls,
and starvation protection while still depending on externally scheduled state.

## Selected Workflow Semantics

### Workflow and job concurrency

Remove the top-level Release-plz concurrency group and the
`check-release-pr-turn` job. Release jobs remain serialized by the existing
`release-plz-publish` job concurrency group, so only one publication changes
crates.io at a time.

The `release-pr` job retains its existing concurrency group and adds
`queue: max`. This prevents an eligible post-publish `release-pr` job from being
replaced by a later pending job. Queue order is irrelevant because every job
validates its own checked-out snapshot before invoking release-plz.

### In-run release ordering

`release-pr` directly needs both `check-releases` and `release`:

| Current run | `release` result | `release-pr` eligibility |
| --- | --- | --- |
| Ordinary `main` push | `skipped` | Eligible for registry guard |
| Successful Release PR merge | `success` | Eligible after publish |
| Failed or rejected release | `failure` or `cancelled` | Skip |
| Non-`main` manual dispatch | any | Skip |

`release` continues to need only `check-releases`.

### Stable main snapshot

When an eligible `release-pr` job starts, checkout explicitly resolves the
current `main` branch rather than the workflow event SHA. That snapshot includes
ordinary changes pushed while a release was waiting for approval.

The registry guard validates the versions from this checked-out snapshot. A
push arriving after checkout is not part of the snapshot and is handled by its
own workflow run, avoiding a check-then-act race.

### Registry readiness guard

After checkout and Rust toolchain setup:

1. Run `cargo metadata --format-version 1 --no-deps`.
2. Select every publishable workspace package and its exact version.
3. Request `https://crates.io/api/v1/crates/{name}/{version}` with an explicit
   User-Agent.
4. Run `release-plz release-pr` only if every request confirms that exact
   version exists.

For an ordinary push, a missing version means publication is still pending, so
the job records `ready=false` and skips the release-plz action. The retained
Release PR merge run becomes eligible only after its publication succeeds and
will process the latest `main` snapshot.

For a real release run, publication has already reported success. To tolerate
short registry-read propagation delay, retry missing versions every 10 seconds
for up to 5 minutes. If any version is still missing, fail the job instead of
creating a stale Release PR.

HTTP failures other than a confirmed missing version, malformed metadata, an
empty publishable package set, or command failures fail the job immediately.

## Race Analysis

- **Ordinary run starts before release approval:** it sees the unpublished
  version and skips; the release job is in another concurrency group and can
  proceed.
- **Ordinary run starts during publish:** it sees at least one missing exact
  version and skips; the successful release run later maintains the PR.
- **Ordinary changes land while release waits:** the post-publish release run
  checks out latest `main`, so those changes are included.
- **Another push lands after checkout:** it is excluded from the validated
  snapshot and handled by its own run.
- **Multiple `release-pr` jobs become eligible:** job concurrency serializes
  them; each validates its snapshot, so execution order does not affect safety.
- **A publication fails or is rejected:** that release run's `release-pr` is
  ineligible, and ordinary runs fail closed on the missing registry version.

## Verification

Update the repository workflow contract test to assert:

- no workflow-level concurrency or queue-order API gate remains;
- `release-pr` needs `check-releases` and `release`;
- ordinary pushes remain eligible after a skipped release;
- real releases require successful publication;
- `release` remains independent of `release-pr`;
- `release-pr` uses `queue: max`;
- checkout explicitly targets `main`;
- the registry guard derives exact publishable package versions from
  `cargo metadata`;
- both ordinary-skip and real-release retry/failure paths are present;
- the release-plz action requires a successful `ready=true` result.

Run YAML parsing, formatting, clippy, strict rustdoc, focused contract tests,
workspace tests, and committed-HEAD patch coverage. The PR is ready to resolve
review threads only after all checks pass.
