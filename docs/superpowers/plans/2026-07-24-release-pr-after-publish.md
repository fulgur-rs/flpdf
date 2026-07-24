# Release PR after publish implementation plan

**Goal:** Prevent the Release PR merge workflow from recreating the same
version before its approved publish job completes.

**Architecture:** Preserve release-plz's separate `release` and `release-pr`
jobs. Add only a dependency and result-aware condition to `release-pr`, using
flpdf's existing release detector to avoid approval prompts on ordinary pushes.

## Task 1: Specify the boilerplate contract

Update `crates/flpdf-cli/tests/release_workflow_contract.rs` to require:

- separate `release` and `release-pr` commands;
- the standard `release-pr` concurrency group;
- no workflow-level concurrency or queue-inspection job;
- no custom crates.io readiness polling;
- result-aware ordering after `release`.

Run:

```bash
cargo test -p flpdf-cli --test release_workflow_contract
```

Expected before the workflow change: failures for the custom queue machinery
and the old `release-pr` dependency list.

## Task 2: Restore the two-job workflow

In `.github/workflows/release-plz.yml`:

1. Remove workflow-level concurrency.
2. Remove `check-release-pr-turn`.
3. Set `release-pr` to `needs: [check-releases, release]`.
4. Run `release-pr` when the detector succeeds and either:
   - the push is ordinary, so `release` was intentionally skipped; or
   - the push is a release and `release` succeeded.
5. Keep the existing release-plz commands, job-level PR concurrency,
   authentication, and Environment approval unchanged.

Run the focused contract test again and expect all tests to pass.

## Task 3: Verify and publish

Run:

```bash
cargo fmt --all -- --check
cargo test -p flpdf-cli --test release_workflow_contract
cargo test -p flpdf-cli
cargo test
actionlint
git diff --check
```

Then commit, push the PR branch, inspect GitHub checks, and reply only to
review threads directly addressed by the implementation. Leave the explicitly
skipped documentation thread unchanged.
