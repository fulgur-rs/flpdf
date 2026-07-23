# Release PR Cross-Run Serialization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prevent any later `main` push from running `release-pr` while an earlier release workflow is awaiting approval or publishing.

**Architecture:** Preserve the existing in-run dependency from `release-pr` to `release`, and serialize complete Release-plz workflow runs per ref. Use GitHub Actions `queue: max` so subsequent pushes wait without replacing a pending release run.

**Tech Stack:** GitHub Actions YAML, Rust integration tests, Ruby/Psych YAML parsing

## Global Constraints

- Ordinary pushes to `main` must run `release-pr` without requesting release-environment approval.
- A real release must publish successfully before any current or later run reads crates.io.
- A failed, rejected, or cancelled release must not update the next Release PR in that run.
- Workflow concurrency must retain multiple pending pushes instead of replacing them.
- The existing job-level publish and Release PR concurrency groups must remain unchanged.
- `release` must continue to depend only on `check-releases`.
- Review thread 2 about `docs/superpowers/...` is intentionally outside this implementation scope.

---

### Task 1: Serialize Release-plz workflow runs

**Files:**
- Modify: `crates/flpdf-cli/tests/release_workflow_contract.rs`
- Modify: `.github/workflows/release-plz.yml:21-33`

**Interfaces:**
- Consumes: GitHub Actions workflow-level `concurrency`, `github.ref`, and the existing in-run `needs` contract.
- Produces: A per-ref workflow queue that runs at most one Release-plz workflow at a time and retains up to GitHub's `queue: max` limit.

- [ ] **Step 1: Write the failing cross-run workflow contract test**

Add this helper and test to
`crates/flpdf-cli/tests/release_workflow_contract.rs`:

```rust
fn workflow_preamble() -> &str {
    RELEASE_WORKFLOW
        .split_once("\njobs:")
        .map(|(preamble, _)| preamble)
        .expect("release-plz.yml must contain a top-level jobs mapping")
}

#[test]
fn workflow_runs_are_serialized_without_dropping_pending_pushes() {
    let preamble = workflow_preamble();

    assert!(preamble.contains(
        "\nconcurrency:\n  group: release-plz-${{ github.ref }}\n  cancel-in-progress: false\n  queue: max"
    ));
}
```

- [ ] **Step 2: Run the focused test and verify RED**

Run:

```bash
cargo test -p flpdf-cli --test release_workflow_contract \
  workflow_runs_are_serialized_without_dropping_pending_pushes
```

Expected: FAIL at the `preamble.contains(...)` assertion because the workflow
does not yet define top-level concurrency.

- [ ] **Step 3: Implement the minimal workflow-level queue**

Insert this block after the top-level `on` mapping and before `env` in
`.github/workflows/release-plz.yml`:

```yaml
# Serialize complete runs so a later ordinary main push cannot run release-pr
# while an earlier release is waiting for approval or publishing. `queue: max`
# retains pending runs; the default single pending slot could replace the run
# responsible for publishing.
concurrency:
  group: release-plz-${{ github.ref }}
  cancel-in-progress: false
  queue: max
```

Do not alter either existing job-level concurrency block:

```yaml
    concurrency:
      group: release-plz-pr-${{ github.ref }}
      cancel-in-progress: false
```

```yaml
    concurrency:
      group: release-plz-publish
      cancel-in-progress: false
```

- [ ] **Step 4: Run the focused contract suite and verify GREEN**

Run:

```bash
cargo test -p flpdf-cli --test release_workflow_contract
```

Expected: PASS, 3 passed and 0 failed.

- [ ] **Step 5: Validate workflow syntax and repository quality gates**

Run:

```bash
ruby -e 'require "yaml"; YAML.load_file(".github/workflows/release-plz.yml")'
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
cargo test
git diff --check
```

If `actionlint` is installed, also run:

```bash
actionlint .github/workflows/release-plz.yml
```

Expected: every available command exits 0, all tests pass, and no warnings are
promoted to errors.

- [ ] **Step 6: Commit and publish the review fix**

Run:

```bash
git add .github/workflows/release-plz.yml \
  crates/flpdf-cli/tests/release_workflow_contract.rs \
  docs/superpowers/plans/2026-07-24-release-pr-after-publish.md
git commit -m "fix(release): serialize release workflow runs (flpdf-116l)"
git push
```

Expected: the remote PR branch advances to the new local `HEAD`.

- [ ] **Step 7: Verify PR and tracker state**

Run:

```bash
gh pr checks 525
git status -sb
bd close flpdf-116l --reason="Release-plz workflow runs are serialized across main pushes and the cross-run contract is tested"
bd dolt push
```

Expected: local and remote branches agree, the worktree is clean, and
`flpdf-116l` is closed after the review fix is published.
