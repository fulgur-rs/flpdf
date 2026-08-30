//! qpdf 11.9.0 progress-reporting CLI contracts.

use assert_cmd::Command;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(name)
}

fn progress_lines(output_name: &str) -> String {
    format!(
        "flpdf: {output_name}: write progress: 0%\n\
         flpdf: {output_name}: write progress: 29%\n\
         flpdf: {output_name}: write progress: 43%\n\
         flpdf: {output_name}: write progress: 58%\n\
         flpdf: {output_name}: write progress: 72%\n\
         flpdf: {output_name}: write progress: 86%\n\
         flpdf: {output_name}: write progress: 99%\n\
         flpdf: {output_name}: write progress: 100%\n"
    )
}

#[test]
fn progress_reports_file_output_on_info_stream() {
    let input = fixture("one-page.pdf");
    let tempdir = tempfile::tempdir().expect("tempdir");
    let output_path = tempdir.path().join("out.pdf");

    let output = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(["--progress", "--deterministic-id"])
        .arg(&input)
        .arg(&output_path)
        .output()
        .expect("flpdf invocation");

    assert!(
        output.status.success(),
        "flpdf failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).expect("progress output is UTF-8"),
        progress_lines(&output_path.display().to_string())
    );
    assert!(output.stderr.is_empty());
    assert!(output_path.exists(), "progress write must create the PDF");
}

#[test]
fn progress_keeps_pdf_on_stdout_and_reports_on_stderr() {
    let input = fixture("one-page.pdf");

    let output = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(["--progress", "--deterministic-id"])
        .arg(&input)
        .arg("-")
        .output()
        .expect("flpdf invocation");

    assert!(
        output.status.success(),
        "flpdf failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.starts_with(b"%PDF-1.3\n"));
    assert_eq!(
        String::from_utf8(output.stderr).expect("progress output is UTF-8"),
        progress_lines("standard output")
    );
}

#[test]
fn native_rewrite_progress_uses_the_same_reporter_route() {
    let input = fixture("one-page.pdf");
    let tempdir = tempfile::tempdir().expect("tempdir");
    let output_path = tempdir.path().join("rewrite-out.pdf");

    let output = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(["rewrite", "--progress", "--deterministic-id"])
        .arg(&input)
        .arg(&output_path)
        .output()
        .expect("flpdf invocation");

    assert!(
        output.status.success(),
        "flpdf failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).expect("progress output is UTF-8"),
        progress_lines(&output_path.display().to_string())
    );
    assert!(output.stderr.is_empty());
    assert!(output_path.exists(), "progress rewrite must create the PDF");
}
