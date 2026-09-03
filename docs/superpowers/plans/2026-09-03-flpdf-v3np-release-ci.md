# flpdf-v3np Release CI Job Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a required Ubuntu CI job that runs the complete Rust workspace test suite in release mode and catches release-only regressions such as `debug_assert!` side-effect bugs.

**Architecture:** Keep the existing four-OS debug `test` matrix unchanged and add a standalone `release` job that depends on `quality`. The job uses the pinned stable toolchain, its own cargo-cache key, qpdf 11.9.0, and one failure-gating bash step for `cargo test --workspace --release`; a YAML contract test enforces those properties.

**Tech Stack:** GitHub Actions YAML, Rust 1.97.1, qpdf 11.9.0, Rust integration contract tests, `yaml-rust2`.

---

### Task 1: Add the release-job contract test

**Files:**
- Modify: `crates/flpdf-cli/tests/ci_workflow_contract.rs`

- [ ] **Step 1: Add a focused failing test for the missing release job**

Add this test after `test_matrix_runs_default_workspace_suite`:

~~~rust
#[test]
fn release_job_is_present_in_ci() {
    let workflow = parse_workflow(CI_WORKFLOW).expect("ci workflow must be valid");
    let jobs = mapping_get(&workflow, "jobs").expect("ci workflow must define jobs");
    let jobs = require_mapping(jobs, "workflow.jobs").expect("workflow.jobs must be a mapping");

    assert!(
        mapping_get(jobs, "release").is_some(),
        "ci workflow must define the release job"
    );
}
~~~

- [ ] **Step 2: Run the focused test and verify the expected RED failure**

Run:

~~~bash
cargo test -p flpdf-cli --test ci_workflow_contract release_job_is_present_in_ci
~~~

Expected result: the test fails with `ci workflow must define the release job`, because the current workflow has no `release` job.

- [ ] **Step 3: Extend the test into the complete release-job contract**

Add these constants beside `TEST_JOB_RUNS_ON`:

~~~rust
const RELEASE_JOB_NAME: &str = "release";
const RELEASE_JOB_RUNS_ON: &str = "ubuntu-latest";
const RELEASE_TEST_COMMAND: &str = "cargo test --workspace --release";
~~~

Add this helper after `test_job_contains_test_command`:

~~~rust
fn release_job_contains_test_command(workflow: &str, command: &str) -> ContractResult<bool> {
    let workflow = parse_workflow(workflow)?;
    if has_default_run_override(&workflow, "workflow")? {
        return Ok(false);
    }

    let jobs = mapping_get(&workflow, "jobs")
        .ok_or_else(|| "ci workflow must define jobs".to_owned())?;
    let jobs = require_mapping(jobs, "workflow.jobs")?;
    let release_job = mapping_get(jobs, RELEASE_JOB_NAME)
        .ok_or_else(|| "ci workflow must define the release job".to_owned())?;
    let release_job = require_mapping(release_job, "release job")?;

    if has_default_run_override(release_job, "release job")?
        || mapping_contains_key(release_job, "if")
        || !continue_on_error_is_gating(release_job)
    {
        return Ok(false);
    }
    if mapping_get(release_job, "needs").and_then(Yaml::as_str) != Some("quality") {
        return Ok(false);
    }
    if mapping_get(release_job, "runs-on").and_then(Yaml::as_str)
        != Some(RELEASE_JOB_RUNS_ON)
    {
        return Ok(false);
    }

    let Some(steps) = mapping_get(release_job, "steps") else {
        return Ok(false);
    };
    let steps = steps
        .as_vec()
        .ok_or_else(|| "release job.steps must be a sequence".to_owned())?;

    let total_raw_command_occurrences = steps
        .iter()
        .map(|step| {
            mapping_get(step, "run")
                .and_then(Yaml::as_str)
                .map_or(0, |run| run_raw_command_occurrence_count(run, command))
        })
        .sum::<usize>();
    let total_exact_command_lines = steps
        .iter()
        .map(|step| {
            mapping_get(step, "run")
                .and_then(Yaml::as_str)
                .map_or(0, |run| run_exact_command_line_count(run, command))
        })
        .sum::<usize>();
    let executable_command_lines = steps
        .iter()
        .map(|step| test_job_step_exact_command_line_count(step, command))
        .sum::<usize>();

    Ok(total_raw_command_occurrences == 1
        && total_exact_command_lines == 1
        && executable_command_lines == 1)
}
~~~

Replace the existence-only test with:

~~~rust
#[test]
fn release_job_runs_gating_workspace_release_suite() {
    assert!(
        release_job_contains_test_command(CI_WORKFLOW, RELEASE_TEST_COMMAND)
            .expect("ci workflow must be valid and define the release job"),
        "release job must be an Ubuntu quality-dependent gating release test"
    );
}
~~~

- [ ] **Step 4: Add contract tests for the required release-job boundary**

Add this fixture helper and tests below the real-workflow test:

~~~rust
fn release_job_workflow(release_job_fields: &str) -> String {
    let release_job_fields = release_job_fields
        .lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "\
jobs:
  quality:
    runs-on: ubuntu-latest
    steps:
      - run: echo quality
  release:
{release_job_fields}
"
    )
}

#[test]
fn release_job_contract_rejects_wrong_runner() {
    let workflow = release_job_workflow(
        "\
    needs: quality
    runs-on: macos-latest
    steps:
      - shell: bash
        run: |
          set -euo pipefail
          cargo test --workspace --release
",
    );

    assert!(!release_job_contains_test_command(&workflow, RELEASE_TEST_COMMAND)
        .expect("synthetic release workflow must be valid"));
}

#[test]
fn release_job_contract_rejects_missing_quality_dependency() {
    let workflow = release_job_workflow(
        "\
    runs-on: ubuntu-latest
    steps:
      - shell: bash
        run: |
          set -euo pipefail
          cargo test --workspace --release
",
    );

    assert!(!release_job_contains_test_command(&workflow, RELEASE_TEST_COMMAND)
        .expect("synthetic release workflow must be valid"));
}

#[test]
fn release_job_contract_rejects_conditional_or_allowed_failure_execution() {
    let workflow = release_job_workflow(
        "\
    needs: quality
    runs-on: ubuntu-latest
    continue-on-error: true
    steps:
      - if: runner.os == 'Linux'
        shell: bash
        run: |
          set -euo pipefail
          cargo test --workspace --release
",
    );

    assert!(!release_job_contains_test_command(&workflow, RELEASE_TEST_COMMAND)
        .expect("synthetic release workflow must be valid"));
}
~~~

- [ ] **Step 5: Run the contract suite and confirm the expected intermediate state**

Run:

~~~bash
cargo test -p flpdf-cli --test ci_workflow_contract
~~~

Expected result before changing the production workflow: the three synthetic rejection tests pass, while `release_job_runs_gating_workspace_release_suite` fails because `.github/workflows/ci.yml` still has no release job.

### Task 2: Add the standalone Ubuntu release job

**Files:**
- Modify: `.github/workflows/ci.yml` between the existing `test` and `coverage` jobs

- [ ] **Step 1: Add the pinned job setup and release test command**

Insert this top-level job under `jobs:`:

~~~yaml
  release:
    name: Release
    needs: quality
    runs-on: ubuntu-latest
    steps:
      - name: Checkout
        uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1

      - name: Set up Rust
        uses: dtolnay/rust-toolchain@29eef336d9b2848a0b548edc03f92a220660cdb8
        with:
          toolchain: 1.97.1

      - name: Cache cargo artifacts
        uses: Swatinem/rust-cache@f0d9c3887740aee45f6153b24b3a6b815192ec16
        with:
          shared-key: ubuntu-latest-flpdf-release

      - name: Install qpdf 11.9.0
        run: bash scripts/ci-apt-install.sh qpdf

      - name: Verify qpdf 11.9.0 oracle
        shell: bash
        run: |
          set -euo pipefail

          qpdf --version
          test "$(qpdf --version | sed -n '1p' | tr -d '\r')" = "qpdf version ${QPDF_VERSION}"

      - name: Run workspace release test suite
        shell: bash
        run: |
          set -euo pipefail

          cargo test --workspace --release
~~~

The job must keep the default feature set. The existing debug matrix remains responsible for explicit `qpdf-zlib-compat` byte-parity gates.

- [ ] **Step 2: Run the focused contract suite and verify GREEN**

Run:

~~~bash
cargo test -p flpdf-cli --test ci_workflow_contract
~~~

Expected result: every contract test passes, including the real-workflow release-job contract and all synthetic rejection tests.

- [ ] **Step 3: Inspect the workflow diff**

Run:

~~~bash
git diff --check
git diff -- .github/workflows/ci.yml crates/flpdf-cli/tests/ci_workflow_contract.rs
~~~

Confirm that the existing four-OS `test` matrix and its `bytes-identical zlib compat` step are unchanged, and that the new job is top-level under `jobs`.

### Task 3: Verify, commit, and hand off

**Files:**
- Verify: `.github/workflows/ci.yml`
- Verify: `crates/flpdf-cli/tests/ci_workflow_contract.rs`

- [ ] **Step 1: Check formatting**

Run:

~~~bash
cargo fmt --all -- --check
~~~

Expected result: exit code 0 with no formatting changes requested.

- [ ] **Step 2: Run the contract suite**

Run:

~~~bash
cargo test -p flpdf-cli --test ci_workflow_contract
~~~

Expected result: the full contract suite passes.

- [ ] **Step 3: Run the exact release-job command locally**

Run:

~~~bash
cargo test --workspace --release
~~~

Expected result: exit code 0 and no failed tests. Local qpdf 11.9.0 is available, matching the CI setup.

- [ ] **Step 4: Run the repository quality gates relevant to the changed files**

Run:

~~~bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
~~~

Expected result: both commands exit 0.

- [ ] **Step 5: Review status and diff**

Run:

~~~bash
git status --short --branch
git diff --check
~~~

Confirm that only the planned workflow and contract-test changes remain beyond the already committed design and plan documents. The main checkout's unrelated `a.pdf` must remain untouched.

- [ ] **Step 6: Commit the implementation**

Run:

~~~bash
git add .github/workflows/ci.yml crates/flpdf-cli/tests/ci_workflow_contract.rs
git commit -m "ci: add release workspace test job"
~~~

Expected result: one implementation commit containing only the release CI job and its contract tests.

