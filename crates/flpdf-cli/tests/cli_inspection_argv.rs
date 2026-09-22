//! qpdf 11.9.0 inspection argv surface parity.

use assert_cmd::Command;
use std::process::{Command as ProcessCommand, Output};

#[path = "support/text_newlines.rs"]
mod text_newlines;
use text_newlines::normalize_text_newlines;

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";
const INPUT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/compat/attachment-two-page.pdf"
);

fn qpdf_available() -> bool {
    ProcessCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .is_some_and(|line| line.trim() == EXPECTED_QPDF_VERSION)
        })
        .unwrap_or(false)
}

fn run_qpdf(args: &[&str]) -> Output {
    ProcessCommand::new("qpdf")
        .args(args)
        .output()
        .expect("qpdf 11.9.0 should spawn")
}

fn run_flpdf(args: &[&str]) -> Output {
    Command::cargo_bin("flpdf")
        .expect("flpdf binary should build")
        .env("FLPDF_PROGNAME", "qpdf")
        .args(args)
        .output()
        .expect("flpdf should spawn")
}
fn assert_matches_qpdf(args: &[&str]) {
    let expected = run_qpdf(args);
    let actual = run_flpdf(args);
    assert_eq!(
        actual.status.code(),
        expected.status.code(),
        "exit mismatch for {args:?}: qpdf={expected:?} flpdf={actual:?}"
    );
    assert_eq!(
        normalize_text_newlines(&actual.stdout),
        normalize_text_newlines(&expected.stdout),
        "stdout mismatch for {args:?}"
    );
    assert_eq!(
        normalize_text_newlines(&actual.stderr),
        normalize_text_newlines(&expected.stderr),
        "stderr mismatch for {args:?}"
    );
}

#[test]
fn repeated_inspection_flags_match_qpdf() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{EXPECTED_QPDF_VERSION} is required for this parity test on CI");
        }
        eprintln!("skipping: {EXPECTED_QPDF_VERSION} is not available");
        return;
    }

    for args in [
        ["--check", "--check", INPUT],
        ["--show-npages", "--show-npages", INPUT],
        ["--show-pages", "--show-pages", INPUT],
        ["--show-xref", "--show-xref", INPUT],
        ["--check-linearization", "--check-linearization", INPUT],
        ["--show-linearization", "--show-linearization", INPUT],
        ["--show-encryption", "--show-encryption", INPUT],
        ["--list-attachments", "--list-attachments", INPUT],
    ] {
        assert_matches_qpdf(&args);
    }
}

#[test]
fn repeated_selector_inspection_flags_use_qpdf_last_value() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{EXPECTED_QPDF_VERSION} is required for this parity test on CI");
        }
        eprintln!("skipping: {EXPECTED_QPDF_VERSION} is not available");
        return;
    }

    assert_matches_qpdf(&["--show-object=1", "--show-object=trailer", INPUT]);
    assert_matches_qpdf(&[
        "--show-attachment=missing",
        "--show-attachment=attachment.txt",
        INPUT,
    ]);
}

#[test]
fn list_and_show_attachment_run_independently_in_qpdf_order() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{EXPECTED_QPDF_VERSION} is required for this parity test on CI");
        }
        eprintln!("skipping: {EXPECTED_QPDF_VERSION} is not available");
        return;
    }

    assert_matches_qpdf(&[
        "--list-attachments",
        "--show-attachment=attachment.txt",
        INPUT,
    ]);
}
