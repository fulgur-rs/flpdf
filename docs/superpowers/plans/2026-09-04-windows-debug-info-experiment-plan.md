# Windows debug-info build experiment Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans or superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Measure three Cargo dev-profile debug-info settings on the Windows CI runner without changing required CI behavior.

**Architecture:** Add one branch-scoped `.github/workflows/build-experiment.yml` with `push` restricted to `feature/flpdf-w2mi-build-experiment` and `workflow_dispatch` for reruns. Three parallel `windows-latest` jobs set only `CARGO_PROFILE_DEV_DEBUG`, warm dependencies, clean the four workspace packages, time the existing `cargo build --workspace --all-targets` test-binary build, and verify `cargo test --workspace --no-run` reuses those artifacts. Experimental caches use variant-specific keys and `save-if: false`.

**Tech Stack:** GitHub Actions, Windows Git Bash, Cargo, Rust 1.97.1, pinned checkout/toolchain/cache actions, Ruby/Python checks, `gh` CLI.

---

### Task 1: Record the design

**Files:**
- Create: `docs/superpowers/specs/2026-09-04-windows-debug-info-experiment-design.md`

- [x] **Step 1: Write and self-review the design**

Record the 272 second build versus 51 second test baseline, the fact that
`[profile.test]` forks the dependency graph, and the three profile values
`true`, `line-tables-only`, and `0`. Compare required-matrix, local, and manual
workflow approaches. Fix the experiment to the branch-scoped manual workflow,
variant-specific no-save caches, four-package clean, timed all-target build,
test no-run reuse check, and GitHub job summary output. Explicitly prohibit
changes to `ci.yml`, Cargo profiles, required matrix, and source code.

### Task 2: Add the isolated workflow

**Files:**
- Create: `.github/workflows/build-experiment.yml`

- [x] **Step 1: Create the exact workflow**

Create `.github/workflows/build-experiment.yml` with:

    name: Windows Build Experiment

    on:
      push:
        branches:
          - feature/flpdf-w2mi-build-experiment
      workflow_dispatch:

    permissions:
      contents: read

    env:
      CARGO_TERM_COLOR: always

    jobs:
      measure:
        name: debug=${{ matrix.label }}
        runs-on: windows-latest
        strategy:
          fail-fast: false
          matrix:
            include:
              - label: full
                debug: "true"
              - label: line-tables-only
                debug: line-tables-only
              - label: none
                debug: "0"
        env:
          CARGO_PROFILE_DEV_DEBUG: ${{ matrix.debug }}
        steps:
          - name: Checkout
            uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1

          - name: Set up Rust
            uses: dtolnay/rust-toolchain@29eef336d9b2848a0b548edc03f92a220660cdb8
            with:
              toolchain: 1.97.1

          - name: Cache dependencies without saving experiment artifacts
            uses: Swatinem/rust-cache@f0d9c3887740aee45f6153b24b3a6b815192ec16
            with:
              shared-key: windows-profile-dev-debug-${{ matrix.label }}
              save-if: false

          - name: Warm dependency and workspace artifacts
            shell: bash
            run: |
              set -euo pipefail
              cargo build --workspace --all-targets

          - name: Clean workspace package artifacts
            shell: bash
            run: |
              set -euo pipefail
              cargo clean \
                --package flpdf \
                --package flpdf-cli \
                --package flpdf-libjpeg-compat \
                --package flpdf-qtest-tools

          - name: Measure clean workspace test-binary build
            shell: bash
            run: |
              set -euo pipefail
              start_seconds=$SECONDS
              cargo build --workspace --all-targets
              rebuild_seconds=$((SECONDS - start_seconds))
              {
                echo "## Windows debug-info build experiment"
                echo "- variant: ${{ matrix.label }}"
                echo "- CARGO_PROFILE_DEV_DEBUG: ${CARGO_PROFILE_DEV_DEBUG}"
                echo "- clean rebuild seconds: ${rebuild_seconds}"
              } >> "$GITHUB_STEP_SUMMARY"

          - name: Verify test build reuse
            shell: bash
            run: |
              set -euo pipefail
              log_path="${RUNNER_TEMP}/flpdf-w2mi-${{ matrix.label }}-test-no-run.log"
              cargo test --workspace --no-run 2>&1 | tee "$log_path"
              if grep -Eq 'Compiling.*(flpdf|flpdf-cli|flpdf-libjpeg-compat|flpdf-qtest-tools) v' "$log_path"; then
                echo "::error::cargo test --workspace --no-run recompiled a workspace package"
                exit 1
              fi
              echo "test --no-run reuse: pass"
              echo "- test --no-run reuse: pass" >> "$GITHUB_STEP_SUMMARY"

- [x] **Step 2: Verify scope and commit**

Run:

    git diff -- .github/workflows/ci.yml Cargo.toml
    rg -n "CARGO_PROFILE_DEV_DEBUG|windows-profile-dev-debug|save-if: false|cargo clean|cargo build --workspace --all-targets|cargo test --workspace --no-run" .github/workflows/build-experiment.yml

Expected: `ci.yml` and `Cargo.toml` have no diff; the new workflow contains all
three profile values, the four package names, both build commands, the no-run
command, variant cache keys, and `save-if: false`.

Then run:

    git add .github/workflows/build-experiment.yml docs/superpowers/specs/2026-09-04-windows-debug-info-experiment-design.md docs/superpowers/plans/2026-09-04-windows-debug-info-experiment-plan.md
    git commit -m "ci: add Windows debug-info build experiment"

### Task 3: Validate and measure

**Files:** Verify the new workflow only; do not modify `ci.yml` or Cargo profiles.

- [x] **Step 1: Run local checks**

Run:

    ruby -e 'require "yaml"; YAML.load_file(".github/workflows/build-experiment.yml")'
    python3 -m unittest scripts/tests/test_qpdf_module_docs.py
    python3 scripts/qpdf-module-docs.py --check
    python3 scripts/check-qpdf-deviation-markers.py --check
    git diff --check
    git diff -- .github/workflows/ci.yml Cargo.toml

Expected: YAML and repository checks pass, the diff is clean, and required CI
and Cargo profiles are unchanged. If an existing actionlint is available,
run it against the new workflow; do not install new tooling for this issue.

- [x] **Step 2: Push and locate the workflow run**

Run:

    git push -u origin feature/flpdf-w2mi-build-experiment
    gh run list --repo fulgur-rs/flpdf --workflow build-experiment.yml --branch feature/flpdf-w2mi-build-experiment --limit 5 --json databaseId,status,conclusion,headSha,event,url

Require the push-triggered run to use the current branch head. Before this
workflow is present on the default branch, the push-triggered run is the
available pre-merge execution path; `workflow_dispatch` can be used for
manual reruns after the workflow is present on the default branch.

- [x] **Step 3: Watch and record the experiment**

Run:

    RUN_ID="$(gh run list --repo fulgur-rs/flpdf --workflow build-experiment.yml --branch feature/flpdf-w2mi-build-experiment --limit 1 --json databaseId --jq '.[0].databaseId')"
    test -n "$RUN_ID"
    gh run watch "$RUN_ID" --repo fulgur-rs/flpdf --exit-status

Require all three matrix jobs to pass. Record each job summary's debug setting,
clean rebuild seconds, and `test --no-run reuse: pass`. Treat the timings as
data; do not change profile settings based on this issue.

Recorded successful run `33797921540` at commit `cf04b876`:

- `CARGO_PROFILE_DEV_DEBUG=true`: 256 seconds; reuse passed.
- `CARGO_PROFILE_DEV_DEBUG=line-tables-only`: 168 seconds; reuse passed.
- `CARGO_PROFILE_DEV_DEBUG=0`: 97 seconds; reuse passed.

### Task 4: Run repository gates and create a Draft PR

**Files:** Verify the complete worktree; no required CI/source changes.

- [ ] **Step 1: Run gates**

Run:

    cargo fmt --all -- --check
    RUSTDOCFLAGS='-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags' cargo doc --workspace --no-deps --document-private-items
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo test --workspace
    python3 -m unittest scripts/tests/test_qpdf_module_docs.py
    python3 scripts/qpdf-module-docs.py --check
    python3 scripts/check-qpdf-deviation-markers.py --check
    git diff --check

Expected: every command exits 0, the workflow contract tests remain green, and
only the new experiment workflow plus its design/plan files are changed.

- [ ] **Step 2: Create Draft PR and wait for required CI**

Run `gh pr create --repo fulgur-rs/flpdf --draft --base main --head feature/flpdf-w2mi-build-experiment --title "ci: measure Windows debug-info build variants" --body-file /tmp/flpdf-w2mi-pr-body.md` with a body containing the three timings, reuse results, unchanged `ci.yml`/Cargo-profile evidence, and local gates. Watch all required Analyze, Quality, Coverage, codecov/patch, Fuzz, Release, platform, label, and release-approval checks. The experiment workflow is informational.

- [ ] **Step 3: Freshly verify and mark Ready**

Run:

    PR_NUMBER="$(gh pr list --repo fulgur-rs/flpdf --head feature/flpdf-w2mi-build-experiment --state open --json number --jq '.[0].number')"
    test -n "$PR_NUMBER"
    gh pr checks "$PR_NUMBER" --repo fulgur-rs/flpdf
    gh pr view "$PR_NUMBER" --repo fulgur-rs/flpdf --json number,state,isDraft,baseRefName,headRefName,headRefOid,body

Verify base `main`, current head, all checks, and the timing body, then run
`gh pr ready "$PR_NUMBER"`. Leave the PR open and unmerged.

### Task 5: Persist evidence and continue the loop

- [ ] **Step 1: Append Beads evidence**

Read back `flpdf-w2mi`, append the commit, workflow run URL, all three timing
values, reuse results, local/CI gates, PR URL/state, and unchanged required CI
evidence. Keep the issue `in_progress` for integration.

- [ ] **Step 2: Push state**

Run `bd dep cycles`, `bd dolt push`, and `git push`; require
`No dependency cycles detected`, `Push complete.`, and a successful git push.
Then inspect the next ready issue without closing or merging this one.
