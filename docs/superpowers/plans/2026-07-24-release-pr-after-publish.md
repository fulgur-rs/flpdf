# Release PR Registry Readiness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prevent release-plz from recreating an unpublished merged version
while ensuring later `main` changes enter the next Release PR without another
push.

**Architecture:** Remove workflow-run queue inference and validate the exact
latest-`main` snapshot against crates.io before invoking `release-plz
release-pr`. Keep publishing and Release PR maintenance in independent job
concurrency groups, retaining all pending Release PR jobs with `queue: max`.

**Tech Stack:** GitHub Actions YAML, Bash, `cargo metadata`, `jq`, crates.io
HTTP API, Rust workflow contract tests, Ruby/Psych

## Global Constraints

- `release-pr` waits for successful `release` in a real release run.
- Ordinary pushes remain eligible after their `release` job is skipped.
- `release` continues to depend only on `check-releases`.
- The validated checkout is a fixed latest-`main` snapshot.
- Every publishable workspace crate's exact version must exist on crates.io.
- Ordinary runs skip safely on HTTP 404; real release runs retry for 5 minutes
  and then fail.
- Network errors, unexpected HTTP status, malformed metadata, and an empty
  publishable package set fail closed.
- Review thread 2 about `docs/superpowers/...` remains untouched.

---

### Task 1: Replace the queue-order contract with a registry-readiness contract

**Files:**
- Modify: `crates/flpdf-cli/tests/release_workflow_contract.rs`

**Interfaces:**
- Consumes: `.github/workflows/release-plz.yml` as `RELEASE_WORKFLOW`.
- Produces: text-level regression checks for the workflow safety contract.

- [ ] **Step 1: Write the failing contract tests**

Replace `workflow_runs_are_serialized_without_dropping_pending_pushes` and
`release_pr_yields_to_older_queued_workflow_runs` with:

```rust
#[test]
fn workflow_does_not_infer_release_safety_from_run_queue_order() {
    let preamble = workflow_preamble().replace("\r\n", "\n");

    assert!(!preamble.contains("\nconcurrency:"));
    assert!(!RELEASE_WORKFLOW.contains("check-release-pr-turn:"));
    assert!(!RELEASE_WORKFLOW.contains("actions/workflows/release-plz.yml/runs"));
}

#[test]
fn release_pr_validates_latest_main_versions_against_crates_io() {
    let release_pr = job_block("release-pr");

    assert!(release_pr.contains("needs: [check-releases, release]"));
    assert!(release_pr.contains(
        "group: release-plz-pr-${{ github.ref }}\n      cancel-in-progress: false\n      queue: max"
    ));
    assert!(release_pr.contains("ref: refs/heads/main"));
    assert!(release_pr.contains("cargo metadata --format-version 1 --no-deps"));
    assert!(release_pr.contains(".workspace_members as $members"));
    assert!(release_pr.contains("https://crates.io/api/v1/crates/$name/$version"));
    assert!(release_pr.contains("IS_RELEASE: ${{ needs.check-releases.outputs.has_releases }}"));
    assert!(release_pr.contains("max_attempts=30"));
    assert!(release_pr.contains("sleep 10"));
    assert!(release_pr.contains("echo \"ready=false\" >> \"$GITHUB_OUTPUT\""));
    assert!(release_pr.contains("echo \"ready=true\" >> \"$GITHUB_OUTPUT\""));
    assert!(release_pr.contains("if: steps.registry.outputs.ready == 'true'"));
}
```

Restore the direct dependency assertion in
`release_pr_waits_for_publish_when_a_release_is_detected`:

```rust
assert!(block.contains("needs: [check-releases, release]"));
```

- [ ] **Step 2: Run the new tests and verify RED**

Run:

```bash
cargo test -p flpdf-cli --test release_workflow_contract
```

Expected: FAIL because top-level concurrency and `check-release-pr-turn` still
exist, and the registry readiness contract is absent.

- [ ] **Step 3: Commit the RED contract**

```bash
git add crates/flpdf-cli/tests/release_workflow_contract.rs
git commit -m "test(release): require registry readiness gate (flpdf-116l)"
```

Expected: one test-only commit with the focused suite still failing.

---

### Task 2: Validate the latest main snapshot before Release PR maintenance

**Files:**
- Modify: `.github/workflows/release-plz.yml`
- Test: `crates/flpdf-cli/tests/release_workflow_contract.rs`

**Interfaces:**
- Consumes: `needs.check-releases.outputs.has_releases`, latest
  `refs/heads/main`, crates.io exact-version endpoints.
- Produces: `steps.registry.outputs.ready` with string value `true` or `false`.

- [ ] **Step 1: Remove workflow-level run queue inference**

Delete the top-level `concurrency` block and the complete
`check-release-pr-turn` job. Restore the `release-pr` dependency and condition:

```yaml
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

- [ ] **Step 2: Retain all Release PR jobs and checkout latest main**

Add `queue: max` to the existing `release-pr` concurrency block:

```yaml
    concurrency:
      group: release-plz-pr-${{ github.ref }}
      cancel-in-progress: false
      queue: max
```

Add an explicit ref to the checkout step:

```yaml
        with:
          ref: refs/heads/main
          fetch-depth: 0
          persist-credentials: false
```

- [ ] **Step 3: Add the fail-closed registry readiness step**

After the Rust toolchain step and before the Rust cache step, add:

```yaml
      - name: Confirm checked-out versions exist on crates.io
        id: registry
        env:
          IS_RELEASE: ${{ needs.check-releases.outputs.has_releases }}
        run: |
          set -euo pipefail

          packages="$RUNNER_TEMP/publishable-workspace-packages.tsv"
          cargo metadata --format-version 1 --no-deps |
            jq -r '
              .workspace_members as $members
              | .packages[]
              | select(.id as $id | $members | index($id))
              | select(.publish == null or (.publish | length > 0))
              | [.name, .version]
              | @tsv
            ' > "$packages"

          if [ ! -s "$packages" ]; then
            echo "No publishable workspace packages found" >&2
            exit 1
          fi

          check_registry() {
            local missing=0
            local name version http_code

            while IFS=$'\t' read -r name version; do
              http_code=$(
                curl --silent --show-error --location \
                  --output /dev/null \
                  --write-out '%{http_code}' \
                  --header 'User-Agent: flpdf-release-workflow (https://github.com/fulgur-rs/flpdf)' \
                  "https://crates.io/api/v1/crates/$name/$version"
              )
              case "$http_code" in
                200)
                  echo "$name $version is available on crates.io"
                  ;;
                404)
                  echo "$name $version is not yet available on crates.io"
                  missing=1
                  ;;
                *)
                  echo "Unexpected crates.io response for $name $version: HTTP $http_code" >&2
                  return 2
                  ;;
              esac
            done < "$packages"

            return "$missing"
          }

          max_attempts=30
          attempt=1
          while true; do
            if check_registry; then
              echo "ready=true" >> "$GITHUB_OUTPUT"
              exit 0
            else
              status=$?
            fi

            if [ "$status" -ne 1 ]; then
              exit "$status"
            fi

            if [ "$IS_RELEASE" != "true" ]; then
              echo "ready=false" >> "$GITHUB_OUTPUT"
              echo "Skipping release-pr until the release run publishes this snapshot"
              exit 0
            fi

            if [ "$attempt" -ge "$max_attempts" ]; then
              echo "Published versions did not become visible within 5 minutes" >&2
              exit 1
            fi

            attempt=$((attempt + 1))
            sleep 10
          done
```

- [ ] **Step 4: Guard the release-plz action**

Add the readiness condition only to the action that mutates the Release PR:

```yaml
      - name: Run release-plz release-pr
        if: steps.registry.outputs.ready == 'true'
        uses: release-plz/action@e8792575c7f2366cf6ff3ccc33ead9ace5b691c7
```

- [ ] **Step 5: Run the focused contract and verify GREEN**

Run:

```bash
cargo test -p flpdf-cli --test release_workflow_contract
```

Expected: 4 passed, 0 failed.

- [ ] **Step 6: Validate workflow syntax**

Run:

```bash
ruby -e 'require "yaml"; YAML.load_file(".github/workflows/release-plz.yml")'
git diff --check
```

If installed, also run:

```bash
actionlint .github/workflows/release-plz.yml
```

Expected: all available checks exit 0.

- [ ] **Step 7: Commit the implementation**

```bash
git add .github/workflows/release-plz.yml
git commit -m "fix(release): gate PR updates on registry state (flpdf-116l)"
```

Expected: implementation commit follows the RED contract commit.

---

### Task 3: Verify, publish, and resolve review threads

**Files:**
- Verify: `.github/workflows/release-plz.yml`
- Verify: `crates/flpdf-cli/tests/release_workflow_contract.rs`
- Verify: `docs/superpowers/specs/2026-07-24-release-pr-after-publish-design.md`
- Verify: `docs/superpowers/plans/2026-07-24-release-pr-after-publish.md`

**Interfaces:**
- Consumes: committed implementation from Tasks 1 and 2.
- Produces: pushed PR head with green checks and resolved actionable threads.

- [ ] **Step 1: Run repository quality gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" \
  cargo doc --workspace --no-deps --document-private-items
cargo test
git diff --check
```

Expected: every command exits 0.

- [ ] **Step 2: Commit the updated implementation plan**

```bash
git add docs/superpowers/plans/2026-07-24-release-pr-after-publish.md
git commit -m "docs(release): plan registry readiness gate (flpdf-116l)"
```

- [ ] **Step 3: Run committed-HEAD patch coverage**

```bash
scripts/patch-coverage.sh --base origin/main
```

Expected: exit 0 and all executable changed lines covered.

- [ ] **Step 4: Push and monitor PR checks**

```bash
git push
gh pr checks 525 --watch
```

Expected: every required PR check succeeds.

- [ ] **Step 5: Reply in the four actionable review threads**

Use `addPullRequestReviewThreadReply` for:

- `PRRT_kwDOSYPosM6TU0kR` — cross-run publish race;
- `PRRT_kwDOSYPosM6TVQGl` — queue ordering;
- `PRRT_kwDOSYPosM6TVs6U` — pending versus queued status;
- `PRRT_kwDOSYPosM6TVs6c` — preserving later main changes.

Each reply must name the implementation commit and verification:

```text
Fixed in `<commit>`: removed workflow-run queue inference. `release-pr` now
checks out a fixed latest-main snapshot and runs only when every publishable
workspace crate's exact version exists on crates.io. Its job queue retains the
post-publish run, so later main changes are included without relying on run
status or ordering.

Verified with:
- cargo test -p flpdf-cli --test release_workflow_contract
- cargo test
- scripts/patch-coverage.sh --base origin/main
- PR checks on #525
```

- [ ] **Step 6: Resolve only those four threads**

Call `resolveReviewThread` for the four thread IDs after their replies succeed.
Do not reply to or resolve `PRRT_kwDOSYPosM6TU0ka` (the user explicitly skipped
the documentation-artifact comment).

- [ ] **Step 7: Close and sync the tracker**

```bash
bd close flpdf-116l --reason="Registry readiness gate prevents stale Release PRs, preserves later main changes, and all PR checks pass"
bd dolt push
git status -sb
```

Expected: `flpdf-116l` is closed, tracker push succeeds, the worktree is clean,
and local/remote PR heads match.
