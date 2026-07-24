# Release PR after publish design

## Problem

The release-plz quickstart runs two independent jobs on every push to `main`:

- `release-plz release` publishes unpublished workspace packages.
- `release-plz release-pr` prepares the next Release PR.

flpdf adds a GitHub Environment approval to the publish job. When a Release PR
is merged, that job can wait for approval while the independent `release-pr`
job reads the previous registry version and recreates the just-merged version.

## Design

Keep the official two-job release-plz structure and customize only the approval
boundary already required by flpdf:

1. `check-releases` decides whether this push should enter the `release`
   environment. Ordinary pushes do not request release approval.
2. `release` keeps the Environment approval, OIDC authentication, App token,
   and `release-plz release`.
3. `release-pr` depends on `check-releases` and `release`.
4. Its job condition implements the following truth table:

| Push | `release` result | `release-pr` |
| --- | --- | --- |
| Ordinary `main` push | skipped | run |
| Release PR merge | success | run after publish |
| Release PR merge | failure or cancellation | do not run |

The existing release-plz `release-pr` concurrency group remains unchanged.

## Deliberate non-goals

The workflow does not:

- inspect GitHub Actions queue ordering;
- serialize complete workflow runs;
- poll the crates.io REST API;
- reproduce Cargo registry/index readiness checks outside release-plz.

Those mechanisms duplicate release-plz responsibilities and introduce
additional scheduling or registry-state assumptions. This change fixes the
same-run race described by `flpdf-116l` while retaining the upstream
boilerplate's independent job model.

## Verification

The workflow contract test asserts:

- the two release-plz commands remain separate;
- only `release-pr` has the upstream-style concurrency group;
- no queue API or registry polling is introduced;
- ordinary pushes run `release-pr`;
- release pushes run it only after successful publication;
- publication does not depend on maintenance of the next Release PR.
