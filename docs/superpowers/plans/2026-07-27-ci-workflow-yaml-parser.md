# CI Workflow YAML Parser Replacement Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `ci_workflow_contract.rs`'s hand-written YAML interpretation
with `yaml-rust2`, preserve both CI contracts, and rebuild PR #555 so no commit
contains the discarded parser.

**Architecture:** Parse each workflow into one `yaml_rust2::Yaml` mapping, then
traverse `jobs`, `steps`, and decoded `run` scalars. Apply strict gating policy
only to the qpdf module-correspondence commands; the platform-specific
qpdf-zlib compatibility contract remains a presence check over parsed `run`
values.

**Tech Stack:** Rust 1.87 workspace, `yaml-rust2` 0.11 with default features
disabled, Cargo integration tests, Git history reconstruction.

## Global Constraints

- YAML syntax, quoting, flow collections, anchors, aliases, comments, block
  scalars, whitespace, and CRLF handling belong to `yaml-rust2`.
- The workflow source is trusted UTF-8 from `include_str!`; do not enable the
  optional `encoding` feature.
- A module-correspondence match must be in the `quality` job and must not have
  job/step `if`, an allowed-failure setting, or a custom/default shell.
- A qpdf-zlib command may remain in its existing Linux-amd64 conditional step,
  but it must occur in a parsed `run` scalar at a complete command boundary.
- Unexpected root, `jobs`, `quality`, `defaults`, or `steps` shapes produce
  contextual contract errors; non-mapping step items and non-string `run`
  values do not match.
- `ci_workflow_contract.rs` remains excluded from the published
  `flpdf-cli` archive.
- Final history must preserve the verified final tree while replacing the
  review-wave history with four logical commits. The CI commit must introduce
  the `yaml-rust2` implementation directly, with no hand-written parser
  snapshot.

---

### Task 1: Add the YAML dependency and single-document boundary

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/flpdf-cli/Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `crates/flpdf-cli/tests/ci_workflow_contract.rs`

**Interfaces:**
- Consumes: `yaml_rust2::{Yaml, YamlLoader}`
- Produces: `type ContractResult<T> = Result<T, String>` and
  `fn parse_workflow(&str) -> ContractResult<Yaml>`

- [ ] **Step 1: Write failing parser-boundary tests**

Add these tests before adding `parse_workflow`:

```rust
#[test]
fn yaml_parser_rejects_multiple_documents() {
    let error = parse_workflow("{}\n---\n{}").expect_err("multiple documents must fail");

    assert_eq!(
        error,
        "ci workflow must contain exactly one YAML document, found 2"
    );
}

#[test]
fn yaml_parser_requires_mapping_root() {
    let error = parse_workflow("- one\n- two").expect_err("sequence root must fail");

    assert_eq!(error, "ci workflow root must be a mapping");
}

#[test]
fn yaml_parser_reports_malformed_yaml() {
    let error = parse_workflow("jobs: [").expect_err("malformed YAML must fail");

    assert!(error.starts_with("ci workflow is not valid YAML:"));
}
```

- [ ] **Step 2: Run the boundary tests and observe the expected compile failure**

Run:

```bash
cargo test -p flpdf-cli --test ci_workflow_contract yaml_parser_
```

Expected: compilation fails because `parse_workflow` does not exist.

- [ ] **Step 3: Declare `yaml-rust2` as a UTF-8-only dev dependency**

Add to `[workspace.dependencies]`:

```toml
yaml-rust2 = { version = "0.11", default-features = false }
```

Add to `crates/flpdf-cli/Cargo.toml` under `[dev-dependencies]`:

```toml
yaml-rust2.workspace = true
```

Let the next Cargo test update `Cargo.lock`.

- [ ] **Step 4: Implement the single-document parser**

Add the import, result alias, and parser next to the workflow constants:

```rust
use yaml_rust2::{Yaml, YamlLoader};

type ContractResult<T> = Result<T, String>;

fn parse_workflow(workflow: &str) -> ContractResult<Yaml> {
    let mut documents = YamlLoader::load_from_str(workflow)
        .map_err(|error| format!("ci workflow is not valid YAML: {error}"))?;
    if documents.len() != 1 {
        return Err(format!(
            "ci workflow must contain exactly one YAML document, found {}",
            documents.len()
        ));
    }

    let document = documents
        .pop()
        .expect("one parsed YAML document must be available");
    if document.as_hash().is_none() {
        return Err("ci workflow root must be a mapping".to_owned());
    }
    Ok(document)
}
```

- [ ] **Step 5: Run the parser-boundary tests**

Run:

```bash
cargo test -p flpdf-cli --test ci_workflow_contract yaml_parser_
```

Expected: all three parser-boundary tests pass.

- [ ] **Step 6: Commit the dependency and parser boundary**

```bash
git add Cargo.toml Cargo.lock crates/flpdf-cli/Cargo.toml crates/flpdf-cli/tests/ci_workflow_contract.rs
git commit -m "test: parse CI workflow YAML with yaml-rust2"
```

---

### Task 2: Replace exact-command and quality-job parsing

**Files:**
- Modify: `crates/flpdf-cli/tests/ci_workflow_contract.rs`

**Interfaces:**
- Consumes: `parse_workflow`, `Yaml::as_hash`, `Yaml::as_vec`,
  `Yaml::as_str`
- Produces:
  - `fn mapping_get<'a>(&'a Yaml, &str) -> Option<&'a Yaml>`
  - `fn mapping_contains_key(&Yaml, &str) -> bool`
  - `fn has_default_run_shell(&Yaml, &str) -> ContractResult<bool>`
  - `fn job_contains_exact_command(&Yaml, bool, &str) -> ContractResult<bool>`
  - `fn workflow_contains_exact_command(&str, &str) -> ContractResult<bool>`
  - `fn quality_workflow_contains_exact_command(&str, &str) -> ContractResult<bool>`

- [ ] **Step 1: Add crate-discriminating regression tests**

Add these tests and change the old
`workflow_exact_command_match_rejects_unparsed_flow_style_default_run` case
into the positive working-directory case below:

```rust
#[test]
fn yaml_parser_handles_quoted_quality_job_and_flow_style_steps() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
jobs:
  'quality': {{'steps': [{{'run': '{command}'}}]}}
"
    );

    assert!(
        quality_workflow_contains_exact_command(&workflow, command)
            .expect("quoted and flow-style workflow must parse")
    );
}

#[test]
fn yaml_parser_resolves_run_scalar_alias() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
check: &check {command}
steps:
  - run: *check
"
    );

    assert!(
        workflow_contains_exact_command(&workflow, command)
            .expect("run alias workflow must parse")
    );
}

#[test]
fn flow_style_default_working_directory_does_not_disable_gate() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
defaults: {{run: {{working-directory: scripts}}}}
steps:
  - run: {command}
"
    );

    assert!(
        workflow_contains_exact_command(&workflow, command)
            .expect("flow-style defaults must parse")
    );
}
```

Also wrap the shell-suffix input in a parsed step:

```rust
#[test]
fn workflow_exact_command_match_rejects_shell_suffix() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
steps:
  - run: {command} || true
"
    );

    assert!(
        !workflow_contains_exact_command(&workflow, command)
            .expect("synthetic job workflow must be valid")
    );
}
```

- [ ] **Step 2: Run the new cases against the hand-written parser**

Run:

```bash
cargo test -p flpdf-cli --test ci_workflow_contract yaml_parser_ -- --nocapture
cargo test -p flpdf-cli --test ci_workflow_contract flow_style_default_working_directory_does_not_disable_gate -- --nocapture
```

Expected: the quoted/flow, alias, and working-directory cases fail or panic
under the old text parser.

- [ ] **Step 3: Add YAML mapping and policy helpers**

Replace `yaml_mapping_value`, `yaml_value_is_block_declaration`, and
`job_has_default_run_shell` with:

```rust
fn mapping_get<'a>(mapping: &'a Yaml, key: &str) -> Option<&'a Yaml> {
    mapping
        .as_hash()?
        .get(&Yaml::String(key.to_owned()))
}

fn mapping_contains_key(mapping: &Yaml, key: &str) -> bool {
    mapping_get(mapping, key).is_some()
}

fn require_mapping<'a>(value: &'a Yaml, context: &str) -> ContractResult<&'a Yaml> {
    value
        .as_hash()
        .map(|_| value)
        .ok_or_else(|| format!("{context} must be a mapping"))
}

fn has_default_run_shell(mapping: &Yaml, context: &str) -> ContractResult<bool> {
    let Some(defaults) = mapping_get(mapping, "defaults") else {
        return Ok(false);
    };
    let defaults = require_mapping(defaults, &format!("{context}.defaults"))?;
    let Some(run) = mapping_get(defaults, "run") else {
        return Ok(false);
    };
    let run = require_mapping(run, &format!("{context}.defaults.run"))?;
    Ok(mapping_contains_key(run, "shell"))
}

fn continue_on_error_is_gating(mapping: &Yaml) -> bool {
    matches!(
        mapping_get(mapping, "continue-on-error"),
        None | Some(Yaml::Boolean(false))
    )
}

fn step_is_gating(step: &Yaml) -> bool {
    !mapping_contains_key(step, "if")
        && !mapping_contains_key(step, "shell")
        && continue_on_error_is_gating(step)
}
```

- [ ] **Step 4: Implement parsed job traversal and exact matching**

Replace the indentation/block-scalar state machine with:

```rust
fn job_contains_exact_command(
    job: &Yaml,
    inherited_default_shell: bool,
    command: &str,
) -> ContractResult<bool> {
    let job = require_mapping(job, "job")?;
    if inherited_default_shell
        || has_default_run_shell(job, "job")?
        || mapping_contains_key(job, "if")
        || !continue_on_error_is_gating(job)
    {
        return Ok(false);
    }

    let Some(steps) = mapping_get(job, "steps") else {
        return Ok(false);
    };
    let steps = steps
        .as_vec()
        .ok_or_else(|| "job.steps must be a sequence".to_owned())?;

    Ok(steps.iter().any(|step| {
        step.as_hash().is_some()
            && step_is_gating(step)
            && mapping_get(step, "run")
                .and_then(Yaml::as_str)
                .is_some_and(|run| run.trim() == command)
    }))
}

fn workflow_contains_exact_command(
    workflow: &str,
    command: &str,
) -> ContractResult<bool> {
    let job = parse_workflow(workflow)?;
    job_contains_exact_command(&job, false, command)
}

fn quality_workflow_contains_exact_command(
    workflow: &str,
    command: &str,
) -> ContractResult<bool> {
    let workflow = parse_workflow(workflow)?;
    let inherited_default_shell = has_default_run_shell(&workflow, "workflow")?;
    let jobs = mapping_get(&workflow, "jobs")
        .ok_or_else(|| "ci workflow must define jobs".to_owned())?;
    let jobs = require_mapping(jobs, "workflow.jobs")?;
    let quality = mapping_get(jobs, "quality")
        .ok_or_else(|| "ci workflow must define the quality job".to_owned())?;

    job_contains_exact_command(quality, inherited_default_shell, command)
}
```

- [ ] **Step 5: Update existing assertions to unwrap structural errors**

For every boolean assertion around the two exact-command helpers, use:

```rust
assert!(
    workflow_contains_exact_command(&workflow, command)
        .expect("synthetic job workflow must be valid")
);
```

or:

```rust
assert!(
    !quality_workflow_contains_exact_command(&workflow, command)
        .expect("synthetic complete workflow must be valid")
);
```

Keep each test's existing positive or negative expectation. Replace the three
`quality_job_body` scope tests with these parsed-tree checks:

```rust
#[test]
fn quality_job_scope_excludes_commands_from_other_jobs() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
jobs:
  quality:
    steps:
      - run: echo quality
  lint:
    steps:
      - run: {command}
"
    );

    assert!(
        !quality_workflow_contains_exact_command(&workflow, command)
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn quality_job_scope_excludes_comment_suffixed_job() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
jobs:
  quality:
    steps:
      - run: echo quality
  lint: # separate job
    steps:
      - run: {command}
"
    );

    assert!(
        !quality_workflow_contains_exact_command(&workflow, command)
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn quality_job_scope_handles_crlf() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "jobs:\r\n  quality:\r\n    steps:\r\n      - run: {command}\r\n  lint:\r\n    steps:\r\n      - run: echo lint\r\n"
    );

    assert!(
        quality_workflow_contains_exact_command(&workflow, command)
            .expect("CRLF workflow must be valid")
    );
}
```

- [ ] **Step 6: Run the exact-command contract suite**

```bash
cargo test -p flpdf-cli --test ci_workflow_contract
```

Expected: exact-command, gating, quoted-key, flow-style, anchor/alias, and CRLF
tests pass. The whole-file qpdf-zlib test may still use its old raw scan until
Task 3.

- [ ] **Step 7: Commit the parsed quality contract**

```bash
git add crates/flpdf-cli/tests/ci_workflow_contract.rs
git commit -m "refactor: parse quality workflow contract from YAML"
```

---

### Task 3: Restrict compatibility-test discovery to parsed run scalars

**Files:**
- Modify: `crates/flpdf-cli/tests/ci_workflow_contract.rs`

**Interfaces:**
- Consumes: `parse_workflow`, `mapping_get`, `require_mapping`
- Produces:
  - `fn run_contains_test_command(&str, &str) -> bool`
  - `fn job_contains_test_command(&Yaml, &str) -> ContractResult<bool>`
  - `fn workflow_contains_test_command(&str, &str) -> ContractResult<bool>`

- [ ] **Step 1: Add positive and negative parsed-run cases**

Replace the old bare-string boundary test and add the metadata rejection:

```rust
#[test]
fn workflow_command_match_does_not_accept_longer_test_name() {
    let command =
        "cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_byte_identical";
    let workflow = format!(
        "\
jobs:
  test:
    steps:
      - run: {command}_overlay
"
    );

    assert!(
        !workflow_contains_test_command(&workflow, command)
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn workflow_test_command_ignores_non_run_metadata() {
    let command =
        "cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_byte_identical";
    let workflow = format!(
        "\
jobs:
  test:
    env:
      NOTE: {command}
    steps:
      - name: \"{command}\"
        run: echo unrelated
"
    );

    assert!(
        !workflow_contains_test_command(&workflow, command)
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn workflow_test_command_accepts_conditional_multiline_run() {
    let command =
        "cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_byte_identical";
    let workflow = format!(
        "\
jobs:
  test:
    steps:
      - if: runner.os == 'Linux'
        run: |
          set -euo pipefail
          {command}
"
    );

    assert!(
        workflow_contains_test_command(&workflow, command)
            .expect("synthetic complete workflow must be valid")
    );
}
```

- [ ] **Step 2: Run the new metadata case against the raw scan**

```bash
cargo test -p flpdf-cli --test ci_workflow_contract workflow_test_command_ -- --nocapture
```

Expected: the metadata case fails because the old helper scans every workflow
line; the conditional multiline case passes and pins the intended zlib policy.

- [ ] **Step 3: Implement all-job parsed-run traversal**

Replace the raw workflow line scan with:

```rust
fn run_contains_test_command(run: &str, command: &str) -> bool {
    run.lines().map(str::trim).any(|line| {
        let Some(suffix) = line.strip_prefix(command) else {
            return false;
        };
        suffix.is_empty() || suffix.chars().next().is_some_and(char::is_whitespace)
    })
}

fn job_contains_test_command(job: &Yaml, command: &str) -> ContractResult<bool> {
    let job = require_mapping(job, "job")?;
    let Some(steps) = mapping_get(job, "steps") else {
        return Ok(false);
    };
    let steps = steps
        .as_vec()
        .ok_or_else(|| "job.steps must be a sequence".to_owned())?;

    Ok(steps.iter().any(|step| {
        step.as_hash().is_some()
            && mapping_get(step, "run")
                .and_then(Yaml::as_str)
                .is_some_and(|run| run_contains_test_command(run, command))
    }))
}

fn workflow_contains_test_command(
    workflow: &str,
    command: &str,
) -> ContractResult<bool> {
    let workflow = parse_workflow(workflow)?;
    let jobs = mapping_get(&workflow, "jobs")
        .ok_or_else(|| "ci workflow must define jobs".to_owned())?;
    let jobs = require_mapping(jobs, "workflow.jobs")?
        .as_hash()
        .expect("required mapping must remain a mapping");

    for job in jobs.values() {
        if job_contains_test_command(job, command)? {
            return Ok(true);
        }
    }
    Ok(false)
}
```

- [ ] **Step 4: Update the discovered-test contract call**

Use:

```rust
if !workflow_contains_test_command(CI_WORKFLOW, &command)
    .expect("ci.yml must be valid and define mapping jobs")
{
    missing.push(format!("{crate_name}/{test_name}"));
}
```

- [ ] **Step 5: Prove that no YAML lexical parser remains**

Run:

```bash
rg -n "yaml_mapping_value|yaml_value_is_block_declaration|job_property_indent|literal_run_block|folded_run_block|workflow_job_key|quality_job_body" crates/flpdf-cli/tests/ci_workflow_contract.rs
```

Expected: no matches.

The only remaining `.lines()` command search must be inside
`run_contains_test_command`; it receives a decoded YAML scalar rather than the
raw workflow.

- [ ] **Step 6: Run the complete focused test**

```bash
cargo fmt --all
cargo test -p flpdf-cli --test ci_workflow_contract
```

Expected: every workflow contract test passes against the real `ci.yml`.

- [ ] **Step 7: Commit the compatibility contract migration**

```bash
git add crates/flpdf-cli/tests/ci_workflow_contract.rs
git commit -m "refactor: inspect parsed CI run scalars"
```

---

### Task 4: Verify the dependency and workspace boundaries

**Files:**
- Verify: `Cargo.toml`
- Verify: `Cargo.lock`
- Verify: `crates/flpdf-cli/Cargo.toml`
- Verify: `crates/flpdf-cli/tests/ci_workflow_contract.rs`

**Interfaces:**
- Consumes: final parsed workflow contracts from Tasks 1–3
- Produces: a clean, fully tested implementation tree ready for history
  reconstruction

- [ ] **Step 1: Run formatting and focused tests**

```bash
cargo fmt --all -- --check
cargo test -p flpdf-cli --test ci_workflow_contract
```

Expected: both commands succeed.

- [ ] **Step 2: Run crate and workspace tests**

```bash
cargo test -p flpdf-cli
cargo test
```

Expected: all tests pass.

- [ ] **Step 3: Verify published-package exclusion**

```bash
cargo package -p flpdf-cli --allow-dirty
```

Expected: packaging succeeds, and the package verification does not attempt to
compile the excluded workspace-only workflow contract.

- [ ] **Step 4: Check dependency shape and patch cleanliness**

```bash
cargo tree -p flpdf-cli --edges dev | rg "yaml-rust2 v0\\.11"
git diff --check
git status --short
```

Expected: `yaml-rust2 v0.11.x` appears only through development edges; no
whitespace errors or uncommitted files remain.

---

### Task 5: Rebuild PR history without the hand-written parser and publish

**Files:**
- Re-group all files in `origin/main..HEAD`; do not change the final tree
- Preserve the pre-rebase branch:
  `backup/flpdf-qxba-3-before-main-rebase`
- Preserve a local recovery branch:
  `backup/flpdf-qxba-3-before-history-cleanup`

**Interfaces:**
- Consumes: clean verified implementation tree
- Produces: four logical PR commits whose CI commit introduces only the
  `yaml-rust2` parser

- [ ] **Prerequisite: Integrate the latest `origin/main` safely**

The feature branch may have an older merge base than the current PR base.
Preserve its verified tree, then rebase before reconstructing history:

```bash
git status --short
git branch backup/flpdf-qxba-3-before-main-rebase
git fetch origin
git rebase origin/main
cargo fmt --all -- --check
cargo test -p flpdf-cli --test ci_workflow_contract
cargo test
```

Expected: the rebase succeeds without dropping either mainline changes or the
PR changes, and the rebased tree passes the same formatting, focused, and
workspace gates. Resolve conflicts according to the already verified feature
tree and the current mainline APIs; never preserve the discarded hand-written
YAML parser merely to make an old commit apply.

- [ ] **Step 1: Record the verified tree and create a recovery reference**

```bash
git status --short
git branch backup/flpdf-qxba-3-before-history-cleanup
git rev-parse HEAD
git rev-parse HEAD^{tree}
```

Expected: the worktree is clean and the backup branch points to the verified
pre-rewrite history.

- [ ] **Step 2: Reconstruct the branch from `origin/main` without changing files**

```bash
git reset --soft origin/main
git restore --staged :/
```

This is the user-requested history rewrite. `--soft` preserves the complete
working tree, and the backup branch makes the old commits recoverable.

- [ ] **Step 3: Commit the design and plan documents**

```bash
git add docs/plans/2026-07-26-qpdf-module-doc-correspondence-design.md docs/plans/2026-07-26-qpdf-module-doc-correspondence.md docs/superpowers/specs/2026-07-27-ci-workflow-yaml-parser-design.md docs/superpowers/plans/2026-07-27-ci-workflow-yaml-parser.md
git commit -m "docs: design qpdf module correspondence checks"
```

- [ ] **Step 4: Commit the checker and its unit tests**

```bash
git add scripts/qpdf-module-docs.py scripts/tests/test_qpdf_module_docs.py
git commit -m "test: enforce qpdf module correspondence"
```

- [ ] **Step 5: Commit annotations and generated correspondence docs**

```bash
git add .gitattributes crates/flpdf/src crates/flpdf/tests/nntree_tests.rs crates/flpdf/tests/outline_document_helper_tests.rs docs/qpdf-correspondence.md docs/qpdf-module-doc-index.md
git commit -m "docs: classify flpdf modules by qpdf correspondence"
```

- [ ] **Step 6: Commit workflow integration with the crate parser**

```bash
git add .github/workflows/ci.yml Cargo.toml Cargo.lock crates/flpdf-cli/Cargo.toml crates/flpdf-cli/tests/ci_workflow_contract.rs
git commit -m "ci: enforce qpdf module correspondence"
```

At this point the CI contract appears for the first time in the branch already
using `yaml-rust2`; none of the discarded indentation or block-scalar parser
states exist in commit history.

- [ ] **Step 7: Prove the rewritten tree is identical**

```bash
git diff --exit-code backup/flpdf-qxba-3-before-history-cleanup HEAD
git status --short
git log --reverse --oneline origin/main..HEAD
```

Expected: the tree diff and status are empty, and the log contains exactly the
four logical commits from Steps 3–6.

- [ ] **Step 8: Re-run post-rewrite verification**

```bash
cargo fmt --all -- --check
cargo test -p flpdf-cli --test ci_workflow_contract
cargo test
cargo package -p flpdf-cli --allow-dirty
git diff --check
```

Expected: all commands succeed on the rewritten commit history.

- [ ] **Step 9: Push Beads and force-update the already published PR branch**

```bash
bd show flpdf-qxba.3
bd dolt push
git push --force-with-lease origin feature/flpdf-qxba-3-qpdf-module-docs
```

Before the force push, report that PR #555's remote commit IDs will be replaced
and that `--force-with-lease` protects against overwriting an unexpected remote
update.

- [ ] **Step 10: Verify PR #555 after publication**

```bash
gh pr view 555 --json url,headRefName,headRefOid,mergeable,reviewDecision
gh pr checks 555
git status --short --branch
```

Expected: PR #555 points to the rewritten head, the branch tracks the updated
remote, and the worktree is clean. Retain the local backup branch until the
rewritten PR checks pass.
