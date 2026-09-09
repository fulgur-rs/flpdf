use assert_cmd::Command;
use std::fs;
use std::path::Path;
use std::process::{Command as ProcessCommand, Output};

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

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

fn run_qpdf(directory: &Path, first: &str, second: &str) -> Output {
    ProcessCommand::new("qpdf")
        .current_dir(directory)
        .args([
            format!("--job-json-file={first}"),
            format!("--job-json-file={second}"),
        ])
        .output()
        .expect("qpdf should spawn")
}

fn run_flpdf(directory: &Path, first: &str, second: &str) -> Output {
    Command::cargo_bin("flpdf")
        .expect("flpdf should build")
        .env("FLPDF_PROGNAME", "qpdf")
        .current_dir(directory)
        .args([
            format!("--job-json-file={first}"),
            format!("--job-json-file={second}"),
        ])
        .output()
        .expect("flpdf should spawn")
}

#[test]
fn repeated_job_json_files_follow_qpdf_partial_initialize_order() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{EXPECTED_QPDF_VERSION} is required for this parity test on CI");
        }
        eprintln!("skipping: {EXPECTED_QPDF_VERSION} is not available");
        return;
    }

    let directory = tempfile::tempdir().expect("temporary directory");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf"),
        directory.path().join("minimal.pdf"),
    )
    .expect("copy fixture");
    fs::write(
        directory.path().join("input.json"),
        br#"{"inputFile":"minimal.pdf","staticId":""}"#,
    )
    .expect("write input JSON");
    fs::write(
        directory.path().join("output.json"),
        br#"{"outputFile":"output.pdf"}"#,
    )
    .expect("write output JSON");

    let qpdf = run_qpdf(directory.path(), "input.json", "output.json");
    assert!(qpdf.status.success(), "qpdf failed: {qpdf:?}");
    let qpdf_bytes = fs::read(directory.path().join("output.pdf")).expect("qpdf output PDF");
    fs::remove_file(directory.path().join("output.pdf")).expect("remove qpdf output");
    let flpdf = run_flpdf(directory.path(), "input.json", "output.json");

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    assert!(flpdf.status.success(), "flpdf failed: {flpdf:?}");
    let flpdf_bytes = fs::read(directory.path().join("output.pdf")).expect("flpdf output PDF");
    assert_eq!(flpdf_bytes, qpdf_bytes);
}

#[test]
fn repeated_job_json_files_keep_qpdf_duplicate_output_error() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{EXPECTED_QPDF_VERSION} is required for this parity test on CI");
        }
        eprintln!("skipping: {EXPECTED_QPDF_VERSION} is not available");
        return;
    }

    let directory = tempfile::tempdir().expect("temporary directory");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf"),
        directory.path().join("minimal.pdf"),
    )
    .expect("copy fixture");
    fs::write(
        directory.path().join("first.json"),
        br#"{"inputFile":"minimal.pdf","outputFile":"first.pdf","staticId":""}"#,
    )
    .expect("write first JSON");
    fs::write(
        directory.path().join("second.json"),
        br#"{"outputFile":"second.pdf"}"#,
    )
    .expect("write second JSON");

    let qpdf = run_qpdf(directory.path(), "first.json", "second.json");
    let flpdf = run_flpdf(directory.path(), "first.json", "second.json");

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    assert_eq!(qpdf.status.code(), Some(2));
}
