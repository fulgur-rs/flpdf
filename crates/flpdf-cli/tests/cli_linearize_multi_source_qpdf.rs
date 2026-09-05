//! qpdf 11.9.0 byte parity for linearized multi-source page selection.

#![cfg(feature = "qpdf-zlib-compat")]

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::process::{Command as ShellCommand, Output};

const COMPAT: &str = "../../tests/fixtures/compat";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(COMPAT)
        .join(name)
}

fn skip_if_qpdf_missing() -> bool {
    let version = ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .ok()
        .and_then(|output| {
            output
                .status
                .success()
                .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
        });
    if version
        .as_deref()
        .is_some_and(|stdout| stdout.lines().next() == Some("qpdf version 11.9.0"))
    {
        return false;
    }
    if std::env::var_os("CI").is_some() {
        panic!("qpdf 11.9.0 is required for multi-source linearization parity: {version:?}");
    }
    eprintln!("skipping: qpdf 11.9.0 is not available: {version:?}");
    true
}

fn run_qpdf(args: &[&str]) -> Output {
    ShellCommand::new("qpdf")
        .args(args)
        .output()
        .expect("qpdf should spawn")
}

fn run_flpdf(args: &[&str]) -> Output {
    Command::cargo_bin("flpdf")
        .expect("flpdf binary should build")
        .args(args)
        .output()
        .expect("flpdf should spawn")
}

fn assert_success(output: &Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn linearize_multi_source_pages_matches_qpdf_bytes() {
    if skip_if_qpdf_missing() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let primary = fixture("three-page.pdf");
    let secondary = fixture("one-page.pdf");
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");
    let primary = primary.to_str().unwrap();
    let secondary = secondary.to_str().unwrap();

    let qpdf = run_qpdf(&[
        "--static-id",
        "--stream-data=uncompress",
        "--linearize",
        primary,
        "--pages",
        primary,
        "1",
        secondary,
        "--",
        qpdf_output.to_str().unwrap(),
    ]);
    assert_success(&qpdf, "qpdf multi-source linearization");

    let flpdf = run_flpdf(&[
        "--static-id",
        "--stream-data=uncompress",
        "--linearize",
        primary,
        "--pages",
        primary,
        "1",
        secondary,
        "--",
        flpdf_output.to_str().unwrap(),
    ]);
    assert_success(&flpdf, "flpdf multi-source linearization");

    assert_eq!(
        std::fs::read(&flpdf_output).unwrap(),
        std::fs::read(&qpdf_output).unwrap(),
        "linearized multi-source --pages output must match qpdf"
    );
}
