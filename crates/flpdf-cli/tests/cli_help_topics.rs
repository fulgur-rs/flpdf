// Golden stdout is captured from qpdf 11.9.0 at the pinned source revision.
use assert_cmd::Command;
use std::process::Output;

const QPDF_HELP_ALL: &str = include_str!("fixtures/qpdf-help-all.txt");
const QPDF_HELP_TOP: &str = include_str!("fixtures/qpdf-help-top.txt");

fn run_flpdf(args: &[&str]) -> Output {
    run_flpdf_named(args, "qpdf")
}

fn run_flpdf_named(args: &[&str], program_name: &str) -> Output {
    let mut command = Command::cargo_bin("flpdf").unwrap();
    command
        .env("FLPDF_PROGNAME", program_name)
        .args(args)
        .output()
        .unwrap()
}

fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n")
}

fn normalized_stdout(output: &[u8]) -> String {
    normalize_newlines(std::str::from_utf8(output).unwrap())
}

#[test]
fn help_golden_comparison_normalizes_windows_console_newlines() {
    assert_eq!(normalized_stdout(b"qpdf\r\nhelp\r\n"), "qpdf\nhelp\n");
}

fn help_sections() -> Vec<(String, String)> {
    let all_help = normalize_newlines(QPDF_HELP_ALL);
    let mut sections = Vec::new();
    let mut active: Option<(String, usize)> = None;
    let mut offset = 0;

    for line in all_help.split_inclusive('\n') {
        let line_without_newline = line.strip_suffix('\n').unwrap_or(line);
        let header = line_without_newline
            .strip_prefix("== ")
            .and_then(|header| header.strip_suffix(" =="))
            .filter(|header| header.contains(" ("));

        if let Some(header) = header {
            if let Some((name, body_start)) = active.take() {
                let mut body_end = offset;
                if all_help[..body_end].ends_with('\n') {
                    body_end -= 1;
                }
                sections.push((name, all_help[body_start..body_end].to_owned()));
            }

            let (name, _) = header.split_once(" (").unwrap();
            active = Some((name.to_owned(), offset + line.len() + 1));
        } else if line_without_newline == "====" {
            if let Some((name, body_start)) = active.take() {
                let mut body_end = offset;
                if all_help[..body_end].ends_with('\n') {
                    body_end -= 1;
                }
                sections.push((name, all_help[body_start..body_end].to_owned()));
            }
            break;
        }

        offset += line.len();
    }

    sections
}

#[test]
fn qpdf_help_golden_has_all_registered_topic_and_option_sections() {
    let sections = help_sections();
    assert_eq!(sections.len(), 146);
    assert_eq!(
        sections
            .iter()
            .filter(|(name, _)| name.starts_with("--"))
            .count(),
        127
    );
    assert_eq!(
        sections
            .iter()
            .filter(|(name, _)| !name.starts_with("--"))
            .count(),
        19
    );
}

#[test]
fn bare_qpdf_help_matches_the_qpdf_11_9_top_level_golden() {
    let output = run_flpdf(&["--help"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    assert_eq!(
        normalized_stdout(&output.stdout),
        normalize_newlines(QPDF_HELP_TOP)
    );
}

#[test]
fn usage_topic_keeps_the_literal_program_name_from_the_qpdf_source_table() {
    let output = run_flpdf_named(&["--help=usage"], "flpdf");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Usage: qpdf [infile] [options] [outfile]"));
    assert!(stdout.contains("   OR  qpdf --help[={topic|--option}]"));
    assert!(!stdout.contains("Usage: flpdf [infile]"));
}

#[test]
fn qpdf_help_all_matches_the_qpdf_11_9_golden() {
    let output = run_flpdf(&["--help=all"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    assert_eq!(
        normalized_stdout(&output.stdout),
        normalize_newlines(QPDF_HELP_ALL)
    );
}

#[test]
fn every_qpdf_help_topic_and_option_matches_its_help_all_section() {
    let all_help = normalize_newlines(QPDF_HELP_ALL);
    let footer = all_help.split_once("====\n").unwrap().1;
    for (name, body) in help_sections() {
        let argument = format!("--help={name}");
        let output = run_flpdf(&[&argument]);
        let expected = format!("{body}{footer}");

        assert_eq!(
            output.status.code(),
            Some(0),
            "valid qpdf help entry {name:?} must succeed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty(), "help entry {name:?}");
        assert_eq!(
            normalized_stdout(&output.stdout),
            expected,
            "qpdf help entry {name:?} must match its all-help section"
        );
    }
}
