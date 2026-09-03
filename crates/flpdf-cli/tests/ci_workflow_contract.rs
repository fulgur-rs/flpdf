use std::fs;
use std::path::Path;
use yaml_rust2::{Yaml, YamlLoader};

const CI_WORKFLOW: &str = include_str!("../../../.github/workflows/ci.yml");
const WHOLE_FILE_ZLIB_GATE: &str = "#![cfg(feature = \"qpdf-zlib-compat\")]";
const REQUIRED_TEST_MATRIX_OS: [&str; 4] = [
    "ubuntu-latest",
    "ubuntu-24.04-arm",
    "macos-latest",
    "windows-latest",
];
const TEST_JOB_RUNS_ON: &str = "${{ matrix.os }}";
const RELEASE_JOB_NAME: &str = "release";
const RELEASE_JOB_RUNS_ON: &str = "ubuntu-latest";
const RELEASE_TEST_COMMAND: &str = "cargo test --workspace --release";
const BASH_CONTROL_FLOW_KEYWORDS: [&str; 11] = [
    "if", "then", "else", "fi", "case", "esac", "for", "while", "until", "do", "done",
];

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

fn run_exact_command_line_count(run: &str, command: &str) -> usize {
    let run = shell_script_without_comments(run);
    run.lines()
        .map(str::trim)
        .filter(|line| *line == command)
        .count()
}

fn shell_script_without_comments(script: &str) -> String {
    let mut uncommented = String::with_capacity(script.len());
    let mut quote = None;
    let mut escaped = false;
    let mut in_comment = false;

    for character in script.chars() {
        if in_comment {
            if character == '\n' {
                in_comment = false;
                uncommented.push(character);
            }
            continue;
        }

        match quote {
            Some('\'') => {
                uncommented.push(character);
                if character == '\'' {
                    quote = None;
                }
            }
            Some('"') => {
                uncommented.push(character);
                if escaped {
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == '"' {
                    quote = None;
                }
            }
            None => {
                if escaped {
                    uncommented.push(character);
                    escaped = false;
                    continue;
                }

                match character {
                    '\\' => {
                        uncommented.push(character);
                        escaped = true;
                    }
                    '\'' | '"' => {
                        uncommented.push(character);
                        quote = Some(character);
                    }
                    '#' => in_comment = true,
                    _ => uncommented.push(character),
                }
            }
            Some(_) => unreachable!("shell quote must be single or double"),
        }
    }

    uncommented
}

fn shell_command_segments(script: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut quote = None;
    let mut escaped = false;
    let mut characters = script.char_indices();

    while let Some((index, character)) = characters.next() {
        match quote {
            Some('\'') => {
                if character == '\'' {
                    quote = None;
                }
            }
            Some('"') => {
                if escaped {
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == '"' {
                    quote = None;
                }
            }
            None => {
                if escaped {
                    escaped = false;
                    continue;
                }

                match character {
                    '\\' => escaped = true,
                    '\'' | '"' => quote = Some(character),
                    ';' | '\n' => {
                        segments.push(&script[start..index]);
                        start = index + 1;
                    }
                    '&' if script[index..].starts_with("&&") => {
                        segments.push(&script[start..index]);
                        start = index + 2;
                        characters.next();
                    }
                    '&' => {
                        segments.push(&script[start..index]);
                        start = index + 1;
                    }
                    '|' if script[index..].starts_with("||") => {
                        segments.push(&script[start..index]);
                        start = index + 2;
                        characters.next();
                    }
                    '|' => {
                        segments.push(&script[start..index]);
                        start = index + 1;
                    }
                    _ => {}
                }
            }
            Some(_) => unreachable!("shell quote must be single or double"),
        }
    }

    segments.push(&script[start..]);
    segments
}

fn is_plain_echo_output_segment(segment: &str) -> bool {
    let segment = segment.trim();
    segment.split_whitespace().next() == Some("echo")
        && !segment.contains("$(")
        && !segment.contains('`')
        && !segment.contains("<(")
        && !segment.contains(">(")
}

fn run_raw_command_occurrence_count(run: &str, command: &str) -> usize {
    let run = shell_script_without_comments(run);
    shell_command_segments(&run)
        .into_iter()
        .filter(|segment| !is_plain_echo_output_segment(segment))
        .map(|segment| segment.match_indices(command).count())
        .sum()
}

fn bash_run_has_unsupported_syntax(run: &str) -> bool {
    let run = shell_script_without_comments(run);
    if run.contains("$(") || run.contains('`') {
        return true;
    }

    let mut quote = None;
    let mut escaped = false;

    for (index, character) in run.char_indices() {
        match quote {
            Some('\'') => {
                if character == '\'' {
                    quote = None;
                } else if character == '\n' {
                    return true;
                }
            }
            Some('"') => {
                if escaped {
                    if character == '\n' {
                        return true;
                    }
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == '"' {
                    quote = None;
                } else if character == '\n' {
                    return true;
                }
            }
            None => {
                if escaped {
                    if character == '\n' {
                        return true;
                    }
                    escaped = false;
                    continue;
                }

                match character {
                    '\\' => escaped = true,
                    '\'' | '"' => quote = Some(character),
                    ';' | '|' | '&' | '{' | '}' | '(' | ')' | '`' => return true,
                    '<' if run[index..].starts_with("<(") => return true,
                    '>' if run[index..].starts_with(">(") => return true,
                    _ => {}
                }
            }
            Some(_) => unreachable!("shell quote must be single or double"),
        }
    }

    quote.is_some() || escaped
}

fn bash_run_has_control_flow(run: &str) -> bool {
    bash_run_has_unsupported_syntax(run)
        || shell_script_without_comments(run)
            .lines()
            .map(str::trim)
            .any(|line| {
                line.contains("<<")
                    || line.split_whitespace().next().is_some_and(|word| {
                        BASH_CONTROL_FLOW_KEYWORDS.contains(&word.trim_end_matches(';'))
                    })
            })
}

fn bash_run_has_early_success_before_command(run: &str, command: &str) -> bool {
    let run = shell_script_without_comments(run);
    for segment in shell_command_segments(&run) {
        let segment = segment.trim();
        if segment == command {
            return false;
        }
        if matches!(segment, "exit" | "exit 0" | "exec true") {
            return true;
        }
    }

    false
}

fn bash_run_disables_errexit(run: &str) -> bool {
    let run = shell_script_without_comments(run);
    shell_command_segments(&run).into_iter().any(|segment| {
        let mut words = segment.split_whitespace();
        if words.next() != Some("set") {
            return false;
        }

        let words = words.collect::<Vec<_>>();
        words.iter().enumerate().any(|(index, word)| {
            word.starts_with("+e") || (*word == "+o" && words.get(index + 1) == Some(&"errexit"))
        })
    })
}

fn bash_run_exact_command_line_count(run: &str, command: &str) -> usize {
    if bash_run_has_control_flow(run)
        || bash_run_has_early_success_before_command(run, command)
        || bash_run_disables_errexit(run)
    {
        return 0;
    }

    run_exact_command_line_count(run, command)
}

fn test_job_step_exact_command_line_count(step: &Yaml, command: &str) -> usize {
    if !(step.as_hash().is_some()
        && !mapping_contains_key(step, "if")
        && !mapping_contains_key(step, "working-directory")
        && continue_on_error_is_gating(step)
        && mapping_get(step, "shell")
            .and_then(Yaml::as_str)
            .is_some_and(|shell| shell == "bash"))
    {
        return 0;
    }

    mapping_get(step, "run")
        .and_then(Yaml::as_str)
        .map_or(0, |run| bash_run_exact_command_line_count(run, command))
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

fn test_job_contains_test_command(workflow: &str, command: &str) -> ContractResult<bool> {
    let workflow = parse_workflow(workflow)?;
    if has_default_run_override(&workflow, "workflow")? {
        return Ok(false);
    }
    let jobs =
        mapping_get(&workflow, "jobs").ok_or_else(|| "ci workflow must define jobs".to_owned())?;
    let jobs = require_mapping(jobs, "workflow.jobs")?;
    let test_job = mapping_get(jobs, "test")
        .ok_or_else(|| "ci workflow must define the test job".to_owned())?;
    let test_job = require_mapping(test_job, "test job")?;
    if has_default_run_override(test_job, "test job")?
        || mapping_contains_key(test_job, "if")
        || !continue_on_error_is_gating(test_job)
    {
        return Ok(false);
    }
    if mapping_get(test_job, "runs-on").and_then(Yaml::as_str) != Some(TEST_JOB_RUNS_ON) {
        return Ok(false);
    }
    if !test_job_has_required_os_matrix(test_job)? {
        return Ok(false);
    }
    let Some(steps) = mapping_get(test_job, "steps") else {
        return Ok(false);
    };
    let steps = steps
        .as_vec()
        .ok_or_else(|| "test job.steps must be a sequence".to_owned())?;

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

fn release_job_contains_test_command(workflow: &str, command: &str) -> ContractResult<bool> {
    let workflow = parse_workflow(workflow)?;
    if has_default_run_override(&workflow, "workflow")? {
        return Ok(false);
    }

    let jobs =
        mapping_get(&workflow, "jobs").ok_or_else(|| "ci workflow must define jobs".to_owned())?;
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
    if mapping_get(release_job, "runs-on").and_then(Yaml::as_str) != Some(RELEASE_JOB_RUNS_ON) {
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

fn test_job_has_required_os_matrix(test_job: &Yaml) -> ContractResult<bool> {
    let Some(strategy) = mapping_get(test_job, "strategy") else {
        return Ok(false);
    };
    let strategy = require_mapping(strategy, "test job.strategy")?;
    let Some(matrix) = mapping_get(strategy, "matrix") else {
        return Ok(false);
    };
    let matrix = require_mapping(matrix, "test job.strategy.matrix")?;
    let matrix_entries = matrix
        .as_hash()
        .expect("required mapping must remain a mapping");
    if matrix_entries.len() != 1 {
        return Ok(false);
    }

    if let Some(include) = mapping_get(matrix, "include") {
        let include = include
            .as_vec()
            .ok_or_else(|| "test job.strategy.matrix.include must be a sequence".to_owned())?;
        let mut os_values = Vec::with_capacity(include.len());
        for entry in include {
            let entry = require_mapping(entry, "test job.strategy.matrix.include entry")?;
            let os = mapping_get(entry, "os")
                .and_then(Yaml::as_str)
                .ok_or_else(|| {
                    "test job.strategy.matrix.include entry must define os".to_owned()
                })?;
            os_values.push(os);
        }
        return Ok(has_required_test_matrix_os_values(&os_values));
    }

    let Some(os) = mapping_get(matrix, "os") else {
        return Ok(false);
    };
    let os = os
        .as_vec()
        .ok_or_else(|| "test job.strategy.matrix.os must be a sequence".to_owned())?;
    let mut os_values = Vec::with_capacity(os.len());
    for os in os {
        let os = os
            .as_str()
            .ok_or_else(|| "test job.strategy.matrix.os must contain strings".to_owned())?;
        os_values.push(os);
    }

    Ok(has_required_test_matrix_os_values(&os_values))
}

fn has_required_test_matrix_os_values(os_values: &[&str]) -> bool {
    os_values.len() == REQUIRED_TEST_MATRIX_OS.len()
        && REQUIRED_TEST_MATRIX_OS
            .iter()
            .all(|expected| os_values.contains(expected))
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

fn has_default_run_override(mapping: &Yaml, context: &str) -> ContractResult<bool> {
    let Some(defaults) = mapping_get(mapping, "defaults") else {
        return Ok(false);
    };
    let defaults = require_mapping(defaults, &format!("{context}.defaults"))?;
    let Some(run) = mapping_get(defaults, "run") else {
        return Ok(false);
    };
    let run = require_mapping(run, &format!("{context}.defaults.run"))?;
    Ok(mapping_contains_key(run, "shell") || mapping_contains_key(run, "working-directory"))
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
        && !mapping_contains_key(step, "working-directory")
        && continue_on_error_is_gating(step)
}

fn job_contains_exact_command(
    job: &Yaml,
    inherited_default_run_override: bool,
    command: &str,
) -> ContractResult<bool> {
    let job = require_mapping(job, "job")?;
    let job_has_default_run_override = has_default_run_override(job, "job")?;
    let steps = match mapping_get(job, "steps") {
        Some(steps) => Some(
            steps
                .as_vec()
                .ok_or_else(|| "job.steps must be a sequence".to_owned())?,
        ),
        None => None,
    };

    if inherited_default_run_override
        || job_has_default_run_override
        || mapping_contains_key(job, "if")
        || mapping_contains_key(job, "needs")
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
    let inherited_default_run_override = has_default_run_override(&workflow, "workflow")?;
    let jobs =
        mapping_get(&workflow, "jobs").ok_or_else(|| "ci workflow must define jobs".to_owned())?;
    let jobs = require_mapping(jobs, "workflow.jobs")?;
    let quality = mapping_get(jobs, "quality")
        .ok_or_else(|| "ci workflow must define the quality job".to_owned())?;

    job_contains_exact_command(quality, inherited_default_run_override, command)
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
fn test_matrix_runs_default_workspace_suite() {
    assert!(
        test_job_contains_test_command(CI_WORKFLOW, "cargo test --workspace")
            .expect("ci workflow must be valid and define the test job"),
        "the four-OS test matrix must run the complete default workspace suite"
    );
}

#[test]
fn release_job_runs_gating_workspace_release_suite() {
    assert!(
        release_job_contains_test_command(CI_WORKFLOW, RELEASE_TEST_COMMAND)
            .expect("ci workflow must be valid and define the release job"),
        "release job must be an Ubuntu quality-dependent gating release test"
    );
}

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

    assert!(
        !release_job_contains_test_command(&workflow, RELEASE_TEST_COMMAND)
            .expect("synthetic release workflow must be valid")
    );
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

    assert!(
        !release_job_contains_test_command(&workflow, RELEASE_TEST_COMMAND)
            .expect("synthetic release workflow must be valid")
    );
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

    assert!(
        !release_job_contains_test_command(&workflow, RELEASE_TEST_COMMAND)
            .expect("synthetic release workflow must be valid")
    );
}

#[test]
fn raw_command_occurrence_count_ignores_comments_but_preserves_quoted_hashes() {
    let command = "cargo test --workspace";
    let run = "\
# cargo test --workspace
printf '%s' \"# cargo test --workspace\"
printf '%s' '# cargo test --workspace'
cargo test --workspace # comment
";

    assert_eq!(
        run_raw_command_occurrence_count(run, command),
        3,
        "shell comments must be ignored while quoted # text and the real command remain visible"
    );
}

#[test]
fn test_job_workspace_command_accepts_real_command_with_inline_comment() {
    let workflow = workspace_test_job_workflow(
        "\
steps:
  - shell: bash
    run: cargo test --workspace # the suite is required
",
    );

    assert!(
        test_job_contains_test_command(&workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_rejects_exit_zero_before_suite() {
    let workflow = workspace_test_job_workflow(
        "\
steps:
  - shell: bash
    run: |
      exit 0
      cargo test --workspace
",
    );

    assert!(
        !test_job_contains_test_command(&workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_rejects_bare_exit_before_suite() {
    let workflow = workspace_test_job_workflow(
        "\
steps:
  - shell: bash
    run: |
      set -euo pipefail
      exit
      cargo test --workspace
",
    );

    assert!(
        !test_job_contains_test_command(&workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_rejects_exec_true_before_suite() {
    let workflow = workspace_test_job_workflow(
        "\
steps:
  - shell: bash
    run: |
      exec true
      cargo test --workspace
",
    );

    assert!(
        !test_job_contains_test_command(&workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_rejects_disabled_errexit_before_suite() {
    let workflow = workspace_test_job_workflow(
        "\
steps:
  - shell: bash
    run: |
      set +e
      cargo test --workspace
      echo done
",
    );

    assert!(
        !test_job_contains_test_command(&workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_accepts_errexit_failure_gating() {
    let workflow = workspace_test_job_workflow(
        "\
steps:
  - shell: bash
    run: |
      set -euo pipefail
      cargo test --workspace
",
    );

    assert!(
        test_job_contains_test_command(&workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_rejects_duplicate_workspace_commands() {
    let workflow = workspace_test_job_workflow(
        "\
steps:
  - shell: bash
    run: cargo test --workspace
  - shell: bash
    run: cargo test --workspace
",
    );

    assert!(
        !test_job_contains_test_command(&workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_rejects_duplicate_workspace_command_lines() {
    let workflow = workspace_test_job_workflow(
        "\
steps:
  - shell: bash
    run: |
      cargo test --workspace
      cargo test --workspace
",
    );

    assert!(
        !test_job_contains_test_command(&workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_rejects_inline_semicolon_duplicate() {
    let workflow = workspace_test_job_workflow(
        "\
steps:
  - shell: bash
    run: cargo test --workspace; cargo test --workspace
",
    );

    assert!(
        !test_job_contains_test_command(&workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_rejects_echo_segment_duplicate() {
    let workflow = workspace_test_job_workflow(
        "\
steps:
  - shell: bash
    run: echo pre; cargo test --workspace
  - shell: bash
    run: cargo test --workspace
",
    );

    assert!(
        !test_job_contains_test_command(&workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_rejects_pipe_segment_duplicate() {
    let workflow = workspace_test_job_workflow(
        "\
steps:
  - shell: bash
    run: echo pre | cargo test --workspace
  - shell: bash
    run: cargo test --workspace
",
    );

    assert!(
        !test_job_contains_test_command(&workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_rejects_conditional_continuation() {
    let workflow = workspace_test_job_workflow(
        r#"steps:
  - shell: bash
    run: |
      true || \
      cargo test --workspace
"#,
    );

    assert!(
        !test_job_contains_test_command(&workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_rejects_uncalled_function() {
    let workflow = workspace_test_job_workflow(
        "\
steps:
  - shell: bash
    run: |
      f() {
        cargo test --workspace
      }
",
    );

    assert!(
        !test_job_contains_test_command(&workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_rejects_multiline_quoted_echo() {
    let workflow = workspace_test_job_workflow(
        "\
steps:
  - shell: bash
    run: |
      echo \"begin
      cargo test --workspace
      end\"
",
    );

    assert!(
        !test_job_contains_test_command(&workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_rejects_process_substitution() {
    let workflow = workspace_test_job_workflow(
        "\
steps:
  - shell: bash
    run: echo <(cargo test --workspace)
  - shell: bash
    run: cargo test --workspace
",
    );

    assert!(
        !test_job_contains_test_command(&workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn bash_safety_rejects_backtick_command_substitution() {
    assert!(bash_run_has_unsupported_syntax(
        "echo `cargo test --workspace`"
    ));
}

#[test]
fn bash_safety_rejects_quoted_dollar_paren_command_substitution() {
    assert!(bash_run_has_unsupported_syntax(
        "echo \"$(printf '%s' ignored)\"\ncargo test --workspace"
    ));
}

#[test]
fn test_job_workspace_command_rejects_gated_duplicate_workspace_command() {
    let workflow = workspace_test_job_workflow(
        "\
steps:
  - shell: bash
    run: cargo test --workspace
  - if: false
    shell: bash
    run: cargo test --workspace
",
    );

    assert!(
        !test_job_contains_test_command(&workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_rejects_while_control_flow() {
    let workflow = workspace_test_job_workflow(
        "\
steps:
  - shell: bash
    run: |
      while false; do
        cargo test --workspace
      done
",
    );

    assert!(
        !test_job_contains_test_command(&workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_rejects_static_runs_on() {
    let workflow = "\
jobs:
  test:
    runs-on: ubuntu-latest
    strategy:
      matrix:
        include:
          - os: ubuntu-latest
          - os: ubuntu-24.04-arm
          - os: macos-latest
          - os: windows-latest
    steps:
      - shell: bash
        run: cargo test --workspace
";

    assert!(
        !test_job_contains_test_command(workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_rejects_matrix_exclude() {
    let workflow = "\
jobs:
  test:
    runs-on: ${{ matrix.os }}
    strategy:
      matrix:
        include:
          - os: ubuntu-latest
          - os: ubuntu-24.04-arm
          - os: macos-latest
          - os: windows-latest
        exclude:
          - os: windows-latest
    steps:
      - shell: bash
        run: cargo test --workspace
";

    assert!(
        !test_job_contains_test_command(workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_accepts_direct_os_matrix() {
    let workflow = "\
jobs:
  test:
    runs-on: ${{ matrix.os }}
    strategy:
      matrix:
        os:
          - ubuntu-latest
          - ubuntu-24.04-arm
          - macos-latest
          - windows-latest
    steps:
      - shell: bash
        run: cargo test --workspace
";

    assert!(
        test_job_contains_test_command(workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_rejects_matrix_extra_axis() {
    let workflow = "\
jobs:
  test:
    runs-on: ${{ matrix.os }}
    strategy:
      matrix:
        include:
          - os: ubuntu-latest
          - os: ubuntu-24.04-arm
          - os: macos-latest
          - os: windows-latest
        arch:
          - amd64
    steps:
      - shell: bash
        run: cargo test --workspace
";

    assert!(
        !test_job_contains_test_command(workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

fn workspace_test_job_workflow(test_job_fields: &str) -> String {
    let test_job_fields = test_job_fields
        .lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "\
jobs:
  test:
    runs-on: ${{{{ matrix.os }}}}
    strategy:
      matrix:
        include:
          - os: ubuntu-latest
          - os: ubuntu-24.04-arm
          - os: macos-latest
          - os: windows-latest
{test_job_fields}
"
    )
}

#[test]
fn test_job_workspace_command_rejects_missing_matrix() {
    let workflow = "\
jobs:
  test:
    runs-on: ${{ matrix.os }}
    steps:
      - shell: bash
        run: cargo test --workspace
";

    assert!(
        !test_job_contains_test_command(workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_rejects_empty_matrix() {
    let workflow = "\
jobs:
  test:
    runs-on: ${{ matrix.os }}
    strategy:
      matrix:
        include: []
    steps:
      - shell: bash
        run: cargo test --workspace
";

    assert!(
        !test_job_contains_test_command(workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_rejects_wrong_matrix_os() {
    let workflow = "\
jobs:
  test:
    runs-on: ${{ matrix.os }}
    strategy:
      matrix:
        include:
          - os: ubuntu-latest
          - os: ubuntu-24.04-arm
          - os: macos-latest
          - os: macos-13
    steps:
      - shell: bash
        run: cargo test --workspace
";

    assert!(
        !test_job_contains_test_command(workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_rejects_inactive_shell_branch() {
    let workflow = workspace_test_job_workflow(
        "\
steps:
  - shell: bash
    run: |
      if false; then
        cargo test --workspace
      fi
",
    );

    assert!(
        !test_job_contains_test_command(&workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_rejects_heredoc_body() {
    let workflow = workspace_test_job_workflow(
        "\
steps:
  - shell: bash
    run: |
      cat <<'EOF'
      cargo test --workspace
      EOF
",
    );

    assert!(
        !test_job_contains_test_command(&workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_rejects_custom_shell() {
    let workflow = workspace_test_job_workflow(
        "\
steps:
  - shell: pwsh
    run: cargo test --workspace
",
    );

    assert!(
        !test_job_contains_test_command(&workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_rejects_job_condition() {
    let workflow = "\
jobs:
  test:
    if: runner.os == 'Linux'
    steps:
      - shell: bash
        run: cargo test --workspace
";

    assert!(
        !test_job_contains_test_command(workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_rejects_job_continue_on_error() {
    let workflow = "\
jobs:
  test:
    continue-on-error: true
    steps:
      - shell: bash
        run: cargo test --workspace
";

    assert!(
        !test_job_contains_test_command(workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_rejects_workflow_default_working_directory() {
    let workflow = "\
defaults:
  run:
    working-directory: crates/flpdf
jobs:
  test:
    steps:
      - shell: bash
        run: cargo test --workspace
";

    assert!(
        !test_job_contains_test_command(workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_rejects_job_default_working_directory() {
    let workflow = "\
jobs:
  test:
    defaults:
      run:
        working-directory: crates/flpdf
    steps:
      - shell: bash
        run: cargo test --workspace
";

    assert!(
        !test_job_contains_test_command(workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_rejects_longer_suffix() {
    let workflow = workspace_test_job_workflow(
        "\
steps:
  - shell: bash
    run: cargo test --workspace --no-run
",
    );

    assert!(
        !test_job_contains_test_command(&workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_rejects_ignored_failure_suffix() {
    let workflow = workspace_test_job_workflow(
        "\
steps:
  - shell: bash
    run: cargo test --workspace || true
",
    );

    assert!(
        !test_job_contains_test_command(&workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_rejects_conditional_step() {
    let workflow = workspace_test_job_workflow(
        "\
steps:
  - if: runner.os == 'Linux'
    shell: bash
    run: cargo test --workspace
",
    );

    assert!(
        !test_job_contains_test_command(&workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_rejects_working_directory_override() {
    let workflow = workspace_test_job_workflow(
        "\
steps:
  - shell: bash
    working-directory: crates/flpdf
    run: cargo test --workspace
",
    );

    assert!(
        !test_job_contains_test_command(&workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
}

#[test]
fn test_job_workspace_command_rejects_continue_on_error() {
    let workflow = workspace_test_job_workflow(
        "\
steps:
  - shell: bash
    continue-on-error: true
    run: cargo test --workspace
",
    );

    assert!(
        !test_job_contains_test_command(&workflow, "cargo test --workspace")
            .expect("synthetic complete workflow must be valid")
    );
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
fn workflow_exact_command_match_rejects_custom_step_working_directory() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
steps:
  - run: {command}
    working-directory: scripts
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
fn quality_command_contract_rejects_workflow_default_working_directory() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
defaults:
  run:
    working-directory: scripts
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
fn workflow_exact_command_match_rejects_flow_style_default_working_directory() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
defaults: {{run: {{working-directory: scripts}}}}
steps:
  - run: {command}
"
    );

    assert!(!workflow_contains_exact_command(&workflow, command)
        .expect("synthetic job workflow must be valid"));
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
fn quality_command_contract_rejects_dependent_quality_job() {
    let command = "python3 scripts/qpdf-module-docs.py --check";
    let workflow = format!(
        "\
jobs:
  setup:
    if: false
    steps:
      - run: echo skipped
  quality:
    needs: setup
    steps:
      - run: {command}
"
    );

    assert!(!quality_workflow_contains_exact_command(&workflow, command)
        .expect("synthetic complete workflow must be valid"));
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
