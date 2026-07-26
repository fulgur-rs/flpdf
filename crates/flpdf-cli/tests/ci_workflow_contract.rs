use std::fs;
use std::path::Path;
use yaml_rust2::{Yaml, YamlLoader};

const CI_WORKFLOW: &str = include_str!("../../../.github/workflows/ci.yml");
const WHOLE_FILE_ZLIB_GATE: &str = "#![cfg(feature = \"qpdf-zlib-compat\")]";

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

fn workflow_contains_test_command(workflow: &str, command: &str) -> ContractResult<bool> {
    let workflow = parse_workflow(workflow)?;
    let jobs =
        mapping_get(&workflow, "jobs").ok_or_else(|| "ci workflow must define jobs".to_owned())?;
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

fn mapping_get<'a>(mapping: &'a Yaml, key: &str) -> Option<&'a Yaml> {
    mapping.as_hash()?.get(&Yaml::String(key.to_owned()))
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

fn job_contains_exact_command(
    job: &Yaml,
    inherited_default_shell: bool,
    command: &str,
) -> ContractResult<bool> {
    let job = require_mapping(job, "job")?;
    let job_has_default_shell = has_default_run_shell(job, "job")?;
    let steps = match mapping_get(job, "steps") {
        Some(steps) => Some(
            steps
                .as_vec()
                .ok_or_else(|| "job.steps must be a sequence".to_owned())?,
        ),
        None => None,
    };

    if inherited_default_shell
        || job_has_default_shell
        || mapping_contains_key(job, "if")
        || !continue_on_error_is_gating(job)
    {
        return Ok(false);
    }

    let Some(steps) = steps else {
        return Ok(false);
    };

    Ok(steps.iter().any(|step| {
        step.as_hash().is_some()
            && step_is_gating(step)
            && mapping_get(step, "run")
                .and_then(Yaml::as_str)
                .is_some_and(|run| run.trim() == command)
    }))
}

fn workflow_contains_exact_command(workflow: &str, command: &str) -> ContractResult<bool> {
    let job = parse_workflow(workflow)?;
    job_contains_exact_command(&job, false, command)
}

fn quality_workflow_contains_exact_command(workflow: &str, command: &str) -> ContractResult<bool> {
    let workflow = parse_workflow(workflow)?;
    let inherited_default_shell = has_default_run_shell(&workflow, "workflow")?;
    let jobs =
        mapping_get(&workflow, "jobs").ok_or_else(|| "ci workflow must define jobs".to_owned())?;
    let jobs = require_mapping(jobs, "workflow.jobs")?;
    let quality = mapping_get(jobs, "quality")
        .ok_or_else(|| "ci workflow must define the quality job".to_owned())?;

    job_contains_exact_command(quality, inherited_default_shell, command)
}

#[test]
fn workflow_command_match_does_not_accept_longer_test_name() {
    let command = "cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_byte_identical";
    let workflow = format!(
        "\
jobs:
  test:
    steps:
      - run: {command}_overlay
"
    );

    assert!(!workflow_contains_test_command(&workflow, command)
        .expect("synthetic complete workflow must be valid"));
}

#[test]
fn workflow_test_command_ignores_non_run_metadata() {
    let command = "cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_byte_identical";
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

    assert!(!workflow_contains_test_command(&workflow, command)
        .expect("synthetic complete workflow must be valid"));
}

#[test]
fn workflow_test_command_ignores_non_run_block_scalar() {
    let command = "cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_byte_identical";
    let workflow = format!(
        "\
jobs:
  test:
    env:
      NOTE: |
        {command}
    steps:
      - run: echo unrelated
"
    );

    assert!(!workflow_contains_test_command(&workflow, command)
        .expect("synthetic complete workflow must be valid"));
}

#[test]
fn workflow_test_command_accepts_conditional_multiline_run() {
    let command = "cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_byte_identical";
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

    assert!(workflow_contains_test_command(&workflow, command)
        .expect("synthetic complete workflow must be valid"));
}

#[test]
fn workflow_exact_command_match_rejects_shell_suffix() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
steps:
  - run: {command} || true
"
    );

    assert!(!workflow_contains_exact_command(&workflow, command)
        .expect("synthetic job workflow must be valid"));
}

#[test]
fn yaml_parser_handles_quoted_quality_job_and_flow_style_steps() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
jobs:
  'quality': {{'steps': [{{'run': '{command}'}}]}}
"
    );

    assert!(quality_workflow_contains_exact_command(&workflow, command)
        .expect("quoted and flow-style workflow must parse"));
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
        workflow_contains_exact_command(&workflow, command).expect("run alias workflow must parse")
    );
}

#[test]
fn quality_command_contract_reports_malformed_job_defaults_despite_inherited_shell() {
    let workflow = "\
defaults:
  run:
    shell: bash
jobs:
  quality:
    defaults: []
    steps: []
";

    let error = quality_workflow_contains_exact_command(workflow, "echo quality")
        .expect_err("malformed job defaults must be reported");

    assert_eq!(error, "job.defaults must be a mapping");
}

#[test]
fn workflow_command_contract_reports_malformed_steps_despite_conditional_job() {
    let workflow = "\
if: false
steps: {}
";

    let error = workflow_contains_exact_command(workflow, "echo quality")
        .expect_err("malformed job steps must be reported");

    assert_eq!(error, "job.steps must be a sequence");
}

#[test]
fn workflow_exact_command_match_rejects_non_run_block_scalar() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
steps:
  - env:
      NOTE: |
        {command}
  - run: echo unrelated
"
    );

    assert!(!workflow_contains_exact_command(&workflow, command)
        .expect("synthetic job workflow must be valid"));
}

#[test]
fn workflow_exact_command_match_rejects_env_run_key() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
steps:
  - env:
      run: |
        {command}
    run: echo unrelated
"
    );

    assert!(!workflow_contains_exact_command(&workflow, command)
        .expect("synthetic job workflow must be valid"));
}

#[test]
fn workflow_exact_command_match_rejects_folded_run_scalar_prefix() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
steps:
  - run: >
      echo unrelated
      {command}
"
    );

    assert!(!workflow_contains_exact_command(&workflow, command)
        .expect("synthetic job workflow must be valid"));
}

#[test]
fn workflow_exact_command_match_accepts_single_line_folded_run_scalar() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
steps:
  - run: >
      {command}
"
    );

    assert!(workflow_contains_exact_command(&workflow, command)
        .expect("synthetic job workflow must be valid"));
}

#[test]
fn workflow_exact_command_match_accepts_single_line_literal_run_scalar() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
steps:
  - run: |
      {command}
"
    );

    assert!(workflow_contains_exact_command(&workflow, command)
        .expect("synthetic job workflow must be valid"));
}

#[test]
fn workflow_exact_command_match_rejects_inactive_shell_branch() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
steps:
  - run: |
      if false; then
        {command}
      fi
"
    );

    assert!(!workflow_contains_exact_command(&workflow, command)
        .expect("synthetic job workflow must be valid"));
}

#[test]
fn workflow_exact_command_match_rejects_heredoc_body() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
steps:
  - run: |
      cat <<'EOF'
      {command}
      EOF
"
    );

    assert!(!workflow_contains_exact_command(&workflow, command)
        .expect("synthetic job workflow must be valid"));
}

#[test]
fn workflow_exact_command_match_rejects_second_heredoc_body() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
steps:
  - run: |
      cat <<FIRST <<SECOND
      unrelated
      FIRST
      {command}
      SECOND
"
    );

    assert!(!workflow_contains_exact_command(&workflow, command)
        .expect("synthetic job workflow must be valid"));
}

#[test]
fn workflow_exact_command_match_rejects_run_item_outside_steps() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
strategy:
  matrix:
    include:
      - run: {command}
steps:
  - run: echo unrelated
"
    );

    assert!(!workflow_contains_exact_command(&workflow, command)
        .expect("synthetic job workflow must be valid"));
}

#[test]
fn workflow_exact_command_match_rejects_continue_on_error_step() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
steps:
  - run: {command}
    continue-on-error: true
"
    );

    assert!(!workflow_contains_exact_command(&workflow, command)
        .expect("synthetic job workflow must be valid"));
}

#[test]
fn workflow_exact_command_match_rejects_conditional_step() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
steps:
  - if: false
    run: {command}
"
    );

    assert!(!workflow_contains_exact_command(&workflow, command)
        .expect("synthetic job workflow must be valid"));
}

#[test]
fn workflow_exact_command_match_rejects_custom_step_shell() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
steps:
  - run: {command}
    shell: echo {{0}}
"
    );

    assert!(!workflow_contains_exact_command(&workflow, command)
        .expect("synthetic job workflow must be valid"));
}

#[test]
fn workflow_exact_command_match_rejects_quoted_custom_step_shell_key() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
steps:
  - run: {command}
    'shell': echo {{0}}
"
    );

    assert!(!workflow_contains_exact_command(&workflow, command)
        .expect("synthetic job workflow must be valid"));
}

#[test]
fn workflow_exact_command_match_rejects_spaced_custom_step_shell_key() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
steps:
  - run: {command}
    shell : echo {{0}}
"
    );

    assert!(!workflow_contains_exact_command(&workflow, command)
        .expect("synthetic job workflow must be valid"));
}

#[test]
fn workflow_exact_command_match_rejects_custom_job_default_shell() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
defaults:
  run:
    shell: echo {{0}}
steps:
  - run: {command}
"
    );

    assert!(!workflow_contains_exact_command(&workflow, command)
        .expect("synthetic job workflow must be valid"));
}

#[test]
fn quality_command_contract_rejects_workflow_default_shell() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
defaults:
  run:
    shell: echo {{0}}
jobs:
  quality:
    steps:
      - run: {command}
"
    );

    assert!(!quality_workflow_contains_exact_command(&workflow, command)
        .expect("synthetic complete workflow must be valid"));
}

#[test]
fn quality_command_contract_rejects_commented_workflow_defaults_shell() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
defaults: # inherited by every job
  run:
    shell: echo {{0}}
jobs:
  quality:
    steps:
      - run: {command}
"
    );

    assert!(!quality_workflow_contains_exact_command(&workflow, command)
        .expect("synthetic complete workflow must be valid"));
}

#[test]
fn workflow_exact_command_match_rejects_commented_default_run_shell() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
defaults:
  run: # shell applies to every step
    shell: echo {{0}}
steps:
  - run: {command}
"
    );

    assert!(!workflow_contains_exact_command(&workflow, command)
        .expect("synthetic job workflow must be valid"));
}

#[test]
fn workflow_exact_command_match_rejects_flow_style_default_run_shell() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
defaults: {{run: {{shell: 'echo {{0}}'}}}}
steps:
  - run: {command}
"
    );

    assert!(!workflow_contains_exact_command(&workflow, command)
        .expect("synthetic job workflow must be valid"));
}

#[test]
fn workflow_exact_command_match_rejects_anchored_flow_style_default_run_shell() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
defaults: &d {{run: {{shell: 'echo {{0}}'}}}}
steps:
  - run: {command}
"
    );

    assert!(!workflow_contains_exact_command(&workflow, command)
        .expect("synthetic job workflow must be valid"));
}

#[test]
fn workflow_exact_command_match_rejects_quoted_flow_style_default_run_shell() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
defaults: {{'run': {{'shell': 'echo {{0}}'}}}}
steps:
  - run: {command}
"
    );

    assert!(!workflow_contains_exact_command(&workflow, command)
        .expect("synthetic job workflow must be valid"));
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

    assert!(workflow_contains_exact_command(&workflow, command)
        .expect("flow-style defaults must parse"));
}

#[test]
fn workflow_exact_command_match_rejects_continue_on_error_job() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
continue-on-error: true
steps:
  - run: {command}
"
    );

    assert!(!workflow_contains_exact_command(&workflow, command)
        .expect("synthetic job workflow must be valid"));
}

#[test]
fn workflow_exact_command_match_rejects_conditional_job() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
if: false
steps:
  - run: {command}
"
    );

    assert!(!workflow_contains_exact_command(&workflow, command)
        .expect("synthetic job workflow must be valid"));
}

#[test]
fn ci_runs_every_whole_file_qpdf_zlib_compat_test() {
    let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("flpdf-cli must live under the workspace crates directory");
    let mut missing = Vec::new();

    for crate_entry in
        fs::read_dir(crates_dir).expect("workspace crates directory must be readable")
    {
        let crate_entry = crate_entry.expect("workspace crate entry must be readable");
        let tests_dir = crate_entry.path().join("tests");
        if !tests_dir.is_dir() {
            continue;
        }

        let crate_name = crate_entry.file_name();
        let crate_name = crate_name.to_string_lossy();
        for test_entry in fs::read_dir(tests_dir).expect("crate tests directory must be readable") {
            let test_entry = test_entry.expect("test entry must be readable");
            let path = test_entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
                continue;
            }

            let source = fs::read_to_string(&path).expect("integration test must be readable");
            if !source.lines().any(|line| line == WHOLE_FILE_ZLIB_GATE) {
                continue;
            }

            let test_name = path
                .file_stem()
                .expect("integration test must have a file stem")
                .to_string_lossy();
            let command = format!(
                "cargo test -p {crate_name} --features qpdf-zlib-compat --test {test_name}"
            );
            if !workflow_contains_test_command(CI_WORKFLOW, &command)
                .expect("ci.yml must be valid and define mapping jobs")
            {
                missing.push(format!("{crate_name}/{test_name}"));
            }
        }
    }

    missing.sort();
    assert!(
        missing.is_empty(),
        "ci.yml does not run whole-file qpdf-zlib-compat tests: {}",
        missing.join(", ")
    );
}

#[test]
fn quality_checks_qpdf_module_correspondence() {
    assert!(
        quality_workflow_contains_exact_command(
            CI_WORKFLOW,
            "python3 -m unittest scripts/tests/test_qpdf_module_docs.py"
        )
        .expect("ci workflow must be valid"),
        "quality job must run qpdf module checker tests"
    );
    assert!(
        quality_workflow_contains_exact_command(
            CI_WORKFLOW,
            "python3 scripts/qpdf-module-docs.py --check"
        )
        .expect("ci workflow must be valid"),
        "quality job must reject missing annotations and stale generated output"
    );
}

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

    assert!(!quality_workflow_contains_exact_command(&workflow, command)
        .expect("synthetic complete workflow must be valid"));
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

    assert!(!quality_workflow_contains_exact_command(&workflow, command)
        .expect("synthetic complete workflow must be valid"));
}

#[test]
fn quality_job_scope_handles_crlf() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "jobs:\r\n  quality:\r\n    steps:\r\n      - run: {command}\r\n  lint:\r\n    steps:\r\n      - run: echo lint\r\n"
    );

    assert!(quality_workflow_contains_exact_command(&workflow, command)
        .expect("CRLF workflow must be valid"));
}
