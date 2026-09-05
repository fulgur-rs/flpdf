//! qpdf 11.9.0 verbose progress parity for split-pages resource preflight.

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::process::{Command as ShellCommand, Output};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(name)
}

fn qpdf_11_9_available() -> bool {
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
        return true;
    }
    if std::env::var_os("CI").is_some() {
        panic!("qpdf 11.9.0 is required for split-pages verbose parity: {version:?}");
    }
    eprintln!("skipping: qpdf 11.9.0 is not available: {version:?}");
    false
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
        .env("FLPDF_PROGNAME", "qpdf")
        .args(args)
        .output()
        .expect("flpdf should spawn")
}

/// qpdf's Windows C-runtime output uses CRLF while Rust's output is LF.
/// Compare the diagnostic content after collapsing only CRLF pairs.
fn normalize_text_newlines(bytes: &[u8]) -> Vec<u8> {
    let mut normalized = Vec::with_capacity(bytes.len());
    let mut remaining = bytes;

    while let Some((&byte, rest)) = remaining.split_first() {
        if byte == b'\r' && rest.first() == Some(&b'\n') {
            normalized.push(b'\n');
            remaining = &rest[1..];
        } else {
            normalized.push(byte);
            remaining = rest;
        }
    }

    normalized
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
fn split_pages_verbose_reports_resource_preflight_before_each_chunk() {
    if !qpdf_11_9_available() {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let input = fixture("three-page.pdf");
    let output = temp.path().join("out-%d.pdf");
    let input = input.to_str().unwrap();
    let output = output.to_str().unwrap();
    let args = ["--verbose", "--static-id", "--split-pages=1", input, output];

    let qpdf = run_qpdf(&args);
    assert_success(&qpdf, "qpdf split-pages verbose");
    let flpdf = run_flpdf(&args);
    assert_success(&flpdf, "flpdf split-pages verbose");

    let qpdf_stdout = normalize_text_newlines(&qpdf.stdout);
    let qpdf_stdout_text = String::from_utf8_lossy(&qpdf_stdout);
    assert!(
        qpdf_stdout_text.contains(&format!(
            "qpdf: {input}: checking for shared resources\nqpdf: no shared resources found\n"
        )),
        "qpdf must report the Auto resource preflight: {qpdf_stdout_text:?}"
    );
    assert_eq!(
        normalize_text_newlines(&flpdf.stdout),
        qpdf_stdout,
        "split-pages verbose stdout must match qpdf's resource preflight and chunk reports"
    );
    assert_eq!(
        normalize_text_newlines(&flpdf.stderr),
        normalize_text_newlines(&qpdf.stderr),
        "split-pages verbose stderr must match qpdf"
    );
}

#[test]
fn split_pages_verbose_reports_the_first_shared_resource_finding() {
    if !qpdf_11_9_available() {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let input = fixture("inherited-resources-one-page.pdf");
    let output = temp.path().join("out-%d.pdf");
    let input = input.to_str().unwrap();
    let output = output.to_str().unwrap();
    let args = ["--verbose", "--static-id", "--split-pages=1", input, output];

    let qpdf = run_qpdf(&args);
    assert_success(&qpdf, "qpdf shared-resource split-pages verbose");
    let flpdf = run_flpdf(&args);
    assert_success(&flpdf, "flpdf shared-resource split-pages verbose");

    let qpdf_stdout = normalize_text_newlines(&qpdf.stdout);
    let qpdf_stdout_text = String::from_utf8_lossy(&qpdf_stdout);
    assert!(
        qpdf_stdout_text.contains("found resources in non-leaf page node 2 0\n"),
        "qpdf must report its first shared-resource finding: {qpdf_stdout_text:?}"
    );
    assert_eq!(
        normalize_text_newlines(&flpdf.stdout),
        qpdf_stdout,
        "split-pages verbose finding output must match qpdf"
    );
    assert_eq!(
        normalize_text_newlines(&flpdf.stderr),
        normalize_text_newlines(&qpdf.stderr),
        "split-pages verbose finding stderr must match qpdf"
    );
}
