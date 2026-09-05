//! End-to-end CLI test for `--verbose` "wrote file" completion line.
//!
//! qpdf `--verbose` prints `qpdf: wrote file <output-path>` after a successful
//! rewrite. flpdf-cli emits `flpdf: wrote file <path>` through logger info; the
//! flpdf-qtest shim normalizes the prefix. This completes the verbose-output
//! parity check for qpdf's uo-1..uo-5 and uo-7 goldens.

use std::path::{Path, PathBuf};
use std::process::Command as ShellCommand;

use assert_cmd::Command;
use predicates::prelude::*;

#[path = "support/eol.rs"]
mod eol;
use eol::EOL;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .to_path_buf()
}

fn fixture(name: &str) -> String {
    fixtures_dir().join(name).to_str().unwrap().to_string()
}

fn qpdf_available() -> bool {
    ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .is_some_and(|line| line.trim() == "qpdf version 11.9.0")
        })
        .unwrap_or(false)
}

#[test]
fn verbose_prints_wrote_file_line() {
    let input = fixture("one-page.pdf");
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = tmp.path().join("out.pdf");
    let out_path = out.to_str().unwrap().to_string();
    Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(["rewrite", "--static-id", "--verbose", &input, &out_path])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "flpdf: wrote file {}{EOL}",
            out_path
        )))
        .stderr(predicate::str::is_empty());
}

#[test]
fn verbose_prints_wrote_file_line_after_linearized_rewrite() {
    // rewrite --linearize takes a separate write path (write_linearized +
    // std::fs::write) that would otherwise skip the wrote-file completion
    // line; regression-guard the branch keeps parity with qpdf --verbose.
    let input = fixture("one-page.pdf");
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = tmp.path().join("out.pdf");
    let out_path = out.to_str().unwrap().to_string();
    Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args([
            "rewrite",
            "--static-id",
            "--linearize",
            "--verbose",
            &input,
            &out_path,
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "flpdf: wrote file {}{EOL}",
            out_path
        )))
        .stderr(predicate::str::is_empty());
}

#[test]
fn no_verbose_does_not_print_wrote_file() {
    let input = fixture("one-page.pdf");
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = tmp.path().join("out.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(["rewrite", "--static-id", &input, out.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("wrote file").not())
        .stderr(predicate::str::contains("wrote file").not());
}

#[test]
fn remove_attachment_verbose_diagnostics_match_qpdf() {
    if !qpdf_available() {
        eprintln!("[skip] qpdf 11.9.0 is not available");
        return;
    }

    let input = fixture("attachment-two-page.pdf");
    let tmp = tempfile::tempdir().expect("tempdir");
    let output = tmp.path().join("out.pdf");

    let qpdf = ShellCommand::new("qpdf")
        .args(["--verbose", "--remove-attachment=attachment.txt"])
        .arg(&input)
        .arg(&output)
        .output()
        .expect("qpdf invocation");
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["--verbose", "--remove-attachment=attachment.txt"])
        .arg(&input)
        .arg(&output)
        .output()
        .expect("flpdf invocation");

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    assert!(output.exists(), "successful removal must write output");
}

#[test]
fn remove_attachment_missing_key_diagnostic_matches_qpdf() {
    if !qpdf_available() {
        eprintln!("[skip] qpdf 11.9.0 is not available");
        return;
    }

    let input = fixture("one-page.pdf");
    let tmp = tempfile::tempdir().expect("tempdir");
    let output = tmp.path().join("out.pdf");

    let qpdf = ShellCommand::new("qpdf")
        .arg("--remove-attachment=missing-key")
        .arg(&input)
        .arg(&output)
        .output()
        .expect("qpdf invocation");
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_PROGNAME", "qpdf")
        .arg("--remove-attachment=missing-key")
        .arg(&input)
        .arg(&output)
        .output()
        .expect("flpdf invocation");

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    assert!(!output.exists(), "missing removal must not write output");
}

#[test]
fn remove_attachment_repeated_keys_match_qpdf() {
    if !qpdf_available() {
        eprintln!("[skip] qpdf 11.9.0 is not available");
        return;
    }

    let input = fixture("attachment-two-page.pdf");
    let tmp = tempfile::tempdir().expect("tempdir");
    let input_with_two = tmp.path().join("with-two.pdf");
    let add = ShellCommand::new("qpdf")
        .args(["--static-id", "--add-attachment"])
        .arg(&input)
        .arg("--key=second")
        .arg("--")
        .arg(&input)
        .arg(&input_with_two)
        .output()
        .expect("qpdf attachment setup");
    assert!(
        add.status.success(),
        "qpdf attachment setup failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );

    let output = tmp.path().join("out.pdf");
    let args = [
        "--static-id",
        "--verbose",
        "--remove-attachment=attachment.txt",
        "--remove-attachment=second",
    ];
    let qpdf = ShellCommand::new("qpdf")
        .args(args)
        .arg(&input_with_two)
        .arg(&output)
        .output()
        .expect("qpdf repeated remove invocation");
    assert_eq!(qpdf.status.code(), Some(0));
    let qpdf_bytes = std::fs::read(&output).expect("qpdf repeated remove output");

    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_PROGNAME", "qpdf")
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(args)
        .arg(&input_with_two)
        .arg(&output)
        .output()
        .expect("flpdf repeated remove invocation");

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    assert_eq!(
        std::fs::read(&output).expect("flpdf repeated remove output"),
        qpdf_bytes,
        "repeated attachment removal must produce qpdf-identical bytes"
    );
}

#[test]
fn remove_attachment_duplicate_key_failure_matches_qpdf() {
    if !qpdf_available() {
        eprintln!("[skip] qpdf 11.9.0 is not available");
        return;
    }

    let input = fixture("attachment-two-page.pdf");
    let tmp = tempfile::tempdir().expect("tempdir");
    let output = tmp.path().join("out.pdf");
    let args = [
        "--static-id",
        "--verbose",
        "--remove-attachment=attachment.txt",
        "--remove-attachment=attachment.txt",
    ];

    let qpdf = ShellCommand::new("qpdf")
        .args(args)
        .arg(&input)
        .arg(&output)
        .output()
        .expect("qpdf duplicate remove invocation");
    assert_eq!(qpdf.status.code(), Some(2));
    assert!(
        !output.exists(),
        "qpdf duplicate removal must not write output"
    );

    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_PROGNAME", "qpdf")
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(args)
        .arg(&input)
        .arg(&output)
        .output()
        .expect("flpdf duplicate remove invocation");

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    assert!(!output.exists(), "duplicate removal must not write output");
}

#[test]
fn remove_attachment_verbose_to_stdout_matches_qpdf() {
    if !qpdf_available() {
        eprintln!("[skip] qpdf 11.9.0 is not available");
        return;
    }

    // qpdf switches the logger to standard-output-save mode before any
    // transformation runs (`QPDFJob.cc:625`), so the `removed attachment`
    // info line goes to stderr, the PDF alone goes to stdout, and no
    // `wrote file` line is printed.
    let input = fixture("attachment-two-page.pdf");
    let args = [
        "--verbose",
        "--static-id",
        "--stream-data=uncompress",
        "--remove-attachment=attachment.txt",
    ];

    let qpdf = ShellCommand::new("qpdf")
        .args(args)
        .arg(&input)
        .arg("-")
        .output()
        .expect("qpdf invocation");
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_PROGNAME", "qpdf")
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(args)
        .arg(&input)
        .arg("-")
        .output()
        .expect("flpdf invocation");

    assert_eq!(qpdf.status.code(), Some(0));
    assert!(qpdf.stdout.starts_with(b"%PDF-"));
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stderr, qpdf.stderr);
    assert_eq!(flpdf.stdout, qpdf.stdout);
}
