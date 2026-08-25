//! Differential coverage for qpdf's top-level `--check-linearization` route.

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Output};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(name)
}

fn root_fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

fn qpdf_available() -> bool {
    ProcessCommand::new("/usr/bin/qpdf")
        .arg("--version")
        .output()
        .is_ok()
}

fn run_qpdf(args: &[&str]) -> Output {
    ProcessCommand::new("/usr/bin/qpdf")
        .args(args)
        .output()
        .expect("qpdf 11.9.0 should spawn")
}

fn run_flpdf(args: &[&str]) -> Output {
    Command::cargo_bin("flpdf")
        .expect("flpdf binary should exist")
        .env("FLPDF_PROGNAME", "qpdf")
        .args(args)
        .output()
        .expect("flpdf should spawn")
}

fn assert_output_matches(actual: &Output, expected: &Output) {
    assert_eq!(actual.status.code(), expected.status.code());
    assert_eq!(actual.stdout, expected.stdout);
    assert_eq!(actual.stderr, expected.stderr);
}

#[test]
fn top_level_check_linearization_clean_matches_qpdf() {
    if !qpdf_available() {
        return;
    }
    let input = fixture("linearized-one-page.pdf");
    let input = input.to_str().expect("fixture path should be UTF-8");

    let expected = run_qpdf(&["--check-linearization", input]);
    let actual = run_flpdf(&["--check-linearization", input]);

    assert_output_matches(&actual, &expected);
}

#[test]
fn top_level_check_linearization_non_linearized_matches_qpdf() {
    if !qpdf_available() {
        return;
    }
    let input = root_fixture("minimal.pdf");
    let input = input.to_str().expect("fixture path should be UTF-8");

    let expected = run_qpdf(&["--check-linearization", input]);
    let actual = run_flpdf(&["--check-linearization", input]);

    assert_output_matches(&actual, &expected);
}

#[test]
fn top_level_check_linearization_warning_matches_qpdf() {
    if !qpdf_available() {
        return;
    }
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let input = temp.path().join("o-mismatch.pdf");
    let mut bytes = std::fs::read(fixture("linearized-one-page.pdf")).unwrap();
    let marker = b"/O 6 /E";
    let replacement = b"/O 7 /E";
    let offset = bytes
        .windows(marker.len())
        .position(|window| window == marker)
        .expect("linearization fixture should contain /O");
    bytes[offset..offset + marker.len()].copy_from_slice(replacement);
    std::fs::write(&input, bytes).expect("malformed fixture should be written");
    let input = input.to_str().expect("temporary path should be UTF-8");

    let expected = run_qpdf(&["--check-linearization", input]);
    let actual = run_flpdf(&["--check-linearization", input]);

    assert_eq!(expected.status.code(), Some(3));
    assert_output_matches(&actual, &expected);
    assert!(String::from_utf8_lossy(&actual.stderr).contains("first page object (/O) mismatch"));
    assert!(String::from_utf8_lossy(&actual.stderr).contains("qpdf: operation succeeded with warnings"));
}

#[test]
fn check_linearization_subcommand_uses_the_same_canonical_route() {
    let input = fixture("linearized-one-page.pdf");
    let input = input.to_str().expect("fixture path should be UTF-8");

    let top_level = run_flpdf(&["--check-linearization", input]);
    let subcommand = run_flpdf(&["check-linearization", input]);

    assert_output_matches(&subcommand, &top_level);
}
