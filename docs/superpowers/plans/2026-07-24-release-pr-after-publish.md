# Release PR After Publish Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prevent release-plz from recreating the just-merged version while preserving immediate next-Release-PR maintenance after a successful publish.

**Architecture:** Keep publishing dependent only on release detection. Make the Release PR job observe both detection and publishing results, using an explicit GitHub Actions condition that runs after successful publishing or after a deliberately skipped publish on ordinary pushes.

**Tech Stack:** GitHub Actions YAML, Rust integration tests, Ruby/Psych YAML parsing

## Global Constraints

- Ordinary pushes to `main` must run `release-pr` without requesting release-environment approval.
- A real release must publish successfully before `release-pr` reads crates.io.
- A failed, rejected, or cancelled release must not update the next Release PR.
- The existing publish and Release PR concurrency groups must remain unchanged.
- `release` must continue to depend only on `check-releases`.

---

### Task 1: Encode and implement the release job-ordering contract

**Files:**
- Create: `crates/flpdf-cli/tests/release_workflow_contract.rs`
- Modify: `.github/workflows/release-plz.yml:121-200`

**Interfaces:**
- Consumes: GitHub Actions `needs.<job>.result` and `check-releases.outputs.has_releases`.
- Produces: A `release-pr` job that waits for `release` only when a release was detected, plus a workspace test that pins this dependency contract.

- [ ] **Step 1: Write the failing workflow contract test**

Create `crates/flpdf-cli/tests/release_workflow_contract.rs`:

```rust
const RELEASE_WORKFLOW: &str =
    include_str!("../../../.github/workflows/release-plz.yml");

fn job_block(name: &str) -> String {
    let marker = format!("  {name}:");
    let mut found = false;
    let mut block = Vec::new();

    for line in RELEASE_WORKFLOW.lines() {
        if line == marker {
            found = true;
        } else if found
            && line.starts_with("  ")
            && !line.starts_with("    ")
            && line.ends_with(':')
        {
            break;
        }

        if found {
            block.push(line);
        }
    }

    assert!(found, "job {name:?} is absent from release-plz.yml");
    block.join("\n")
}

#[test]
fn release_pr_waits_for_publish_when_a_release_is_detected() {
    let block = job_block("release-pr");

    assert!(block.contains("needs: [check-releases, release]"));
    assert!(block.contains("always()"));
    assert!(block.contains("!cancelled()"));
    assert!(block.contains("needs.check-releases.result == 'success'"));
    assert!(block.contains(
        "needs.check-releases.outputs.has_releases != 'true'"
    ));
    assert!(block.contains("needs.release.result == 'success'"));
}

#[test]
fn publishing_remains_independent_of_next_release_pr_maintenance() {
    let block = job_block("release");

    assert!(block.contains("needs: [check-releases]"));
    assert!(!block.contains("needs: [check-releases, release-pr]"));
}
```

- [ ] **Step 2: Run the focused test and verify RED**

Run:

```bash
cargo test -p flpdf-cli --test release_workflow_contract
```

Expected: FAIL in
`release_pr_waits_for_publish_when_a_release_is_detected` because the current
`release-pr` block has no `needs: [check-releases, release]`.

- [ ] **Step 3: Implement the minimal workflow ordering**

In `.github/workflows/release-plz.yml`, replace the existing `release-pr` job
guard with:

```yaml
    # release-pr compares main's version with crates.io. On a real release push,
    # wait for publishing so it cannot recreate the just-merged version while
    # the registry is still behind. On ordinary pushes `release` is skipped;
    # always() lets this condition inspect that skipped result and run normally.
    needs: [check-releases, release]
    if: >-
      ${{
        always() &&
        !cancelled() &&
        github.ref == 'refs/heads/main' &&
        needs.check-releases.result == 'success' &&
        (
          needs.check-releases.outputs.has_releases != 'true' ||
          needs.release.result == 'success'
        )
      }}
```

Update the `release` job comment to state that publishing remains independent,
while `release-pr` is ordered after publishing to avoid reading stale registry
state. Do not change `release` job's existing:

```yaml
    needs: [check-releases]
```

- [ ] **Step 4: Run the focused test and verify GREEN**

Run:

```bash
cargo test -p flpdf-cli --test release_workflow_contract
```

Expected: PASS, 2 passed and 0 failed.

- [ ] **Step 5: Validate YAML and repository gates**

Run:

```bash
ruby -e 'require "yaml"; YAML.load_file(".github/workflows/release-plz.yml")'
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
cargo test
git diff --check
```

Expected: every command exits 0 with no warnings promoted to errors and all
tests passing.

- [ ] **Step 6: Commit the implementation**

```bash
git add .github/workflows/release-plz.yml \
  crates/flpdf-cli/tests/release_workflow_contract.rs \
  docs/superpowers/plans/2026-07-24-release-pr-after-publish.md
git commit -m "fix(release): order release PR after publish (flpdf-116l)"
```

- [ ] **Step 7: Close and publish tracker and Git state**

```bash
bd close flpdf-116l --reason="release-pr now waits for successful publishing on release pushes; workflow contract test added"
bd dolt push
git pull --rebase
git push -u origin fix/flpdf-116l-release-pr-after-publish
```

Expected: Bead push and Git push both succeed; the worktree is clean and the
remote branch points at local `HEAD`.

