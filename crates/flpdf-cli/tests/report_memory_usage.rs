//! qpdf 11.9.0 `--report-memory-usage` CLI contract
//! (`libqpdf/qpdf/auto_job_init.hh:73`, `libqpdf/QPDFJob.cc:505-509`).

use assert_cmd::Command;
use regex::Regex;
use std::path::{Path, PathBuf};

#[path = "support/eol.rs"]
mod eol;
use eol::EOL;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(name)
}

fn memory_usage_line_pattern() -> Regex {
    Regex::new(&format!(r"^qpdf-max-memory-usage \d+{EOL}$")).expect("valid regex")
}

#[test]
fn report_memory_usage_writes_the_line_to_stderr_on_the_top_level_route() {
    let input = fixture("one-page.pdf");
    let tempdir = tempfile::tempdir().expect("tempdir");
    let output_path = tempdir.path().join("out.pdf");

    let output = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["--report-memory-usage"])
        .arg(&input)
        .arg(&output_path)
        .output()
        .expect("flpdf invocation");

    assert!(
        output.status.success(),
        "flpdf failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert!(
        memory_usage_line_pattern().is_match(&stderr),
        "unexpected stderr: {stderr:?}"
    );
    assert!(output_path.exists(), "the report must not suppress output");
}

#[test]
fn report_memory_usage_reaches_the_rewrite_subcommand() {
    let input = fixture("one-page.pdf");
    let tempdir = tempfile::tempdir().expect("tempdir");
    let output_path = tempdir.path().join("out.pdf");

    let output = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["rewrite", "--report-memory-usage"])
        .arg(&input)
        .arg(&output_path)
        .output()
        .expect("flpdf invocation");

    assert!(
        output.status.success(),
        "flpdf failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert!(
        memory_usage_line_pattern().is_match(&stderr),
        "unexpected stderr: {stderr:?}"
    );
}

#[test]
fn report_memory_usage_reaches_the_job_json_file_route() {
    let input = fixture("one-page.pdf");
    let tempdir = tempfile::tempdir().expect("tempdir");
    let output_path = tempdir.path().join("out.pdf");
    let job_json_path = tempdir.path().join("job.json");
    std::fs::write(&job_json_path, b"{}").expect("job json");

    let output = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .arg(&input)
        .arg("--report-memory-usage")
        .arg(format!("--job-json-file={}", job_json_path.display()))
        .arg(&output_path)
        .output()
        .expect("flpdf invocation");

    assert!(
        output.status.success(),
        "flpdf failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert!(
        memory_usage_line_pattern().is_match(&stderr),
        "unexpected stderr: {stderr:?}"
    );
}

#[test]
fn without_the_flag_no_memory_usage_line_is_printed() {
    let input = fixture("one-page.pdf");
    let tempdir = tempfile::tempdir().expect("tempdir");
    let output_path = tempdir.path().join("out.pdf");

    let output = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .arg(&input)
        .arg(&output_path)
        .output()
        .expect("flpdf invocation");

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
}

/// qpdf prints the line from whichever job finished the run
/// (`QPDFJob.cc:505-509`), so inspection routes report it just like writes.
/// Measured against pinned qpdf 11.9.0: each of these emits exactly one
/// `qpdf-max-memory-usage` line on stderr.
#[test]
fn report_memory_usage_reaches_the_inspection_routes() {
    let input = fixture("one-page.pdf");

    for flag in [
        "--show-npages",
        "--check",
        "--show-xref",
        "--show-pages",
        "--show-encryption",
        "--list-attachments",
        "--json",
    ] {
        let output = Command::cargo_bin("flpdf")
            .expect("flpdf binary")
            .arg("--report-memory-usage")
            .arg(flag)
            .arg(&input)
            .output()
            .unwrap_or_else(|error| panic!("flpdf invocation for {flag}: {error}"));

        let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
        assert!(
            memory_usage_line_pattern().is_match(&stderr),
            "{flag} must report memory usage like qpdf; stderr was {stderr:?}"
        );
    }
}

/// The same routes stay silent without the flag, so the line is attributable
/// to `--report-memory-usage` rather than to inspection output in general.
#[test]
fn inspection_routes_stay_silent_without_the_flag() {
    let input = fixture("one-page.pdf");

    for flag in ["--show-npages", "--check", "--show-xref", "--json"] {
        let output = Command::cargo_bin("flpdf")
            .expect("flpdf binary")
            .arg(flag)
            .arg(&input)
            .output()
            .unwrap_or_else(|error| panic!("flpdf invocation for {flag}: {error}"));

        let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
        assert!(
            !memory_usage_line_pattern().is_match(&stderr),
            "{flag} must not report memory usage without the flag; stderr was {stderr:?}"
        );
    }
}
