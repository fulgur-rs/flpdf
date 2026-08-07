//! QPDFLogger routing coverage for qpdf-equivalent CLI output.

use assert_cmd::Command;
use std::path::Path;
use std::process::{Command as ProcessCommand, Output};

const MINIMAL: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/minimal.pdf"
);
const MULTI_STREAM: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/compat/multi-stream-one-page.pdf"
);
const ONE_PAGE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/compat/one-page.pdf"
);
const WARNING_PDF: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/test_driver/missing_startxref.pdf"
);
const ATTACHMENT_PDF: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/compat/attachment-two-page.pdf"
);

fn flpdf() -> Command {
    Command::new(assert_cmd::cargo::cargo_bin!("flpdf"))
}

fn qpdf_available() -> bool {
    ProcessCommand::new("qpdf")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn run_qpdf(args: &[&str]) -> Output {
    ProcessCommand::new("qpdf").args(args).output().unwrap()
}

fn run_flpdf(args: &[&str]) -> Output {
    ProcessCommand::new(assert_cmd::cargo::cargo_bin!("flpdf"))
        .env("FLPDF_PROGNAME", "qpdf")
        .args(args)
        .output()
        .unwrap()
}

fn assert_observables_equal(label: &str, qpdf: &Output, flpdf: &Output) {
    assert_eq!(flpdf.status.code(), qpdf.status.code(), "{label}: status");
    assert_eq!(flpdf.stdout, qpdf.stdout, "{label}: stdout");
    assert_eq!(flpdf.stderr, qpdf.stderr, "{label}: stderr");
}

#[test]
fn binary_json_uses_stdout_without_stderr() {
    let output = flpdf().args(["--json=2", MINIMAL]).output().unwrap();

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["version"], serde_json::json!(2));
}

#[test]
fn binary_raw_stream_preserves_exact_bytes() {
    let output = flpdf()
        .args(["show-stream", "4 0 R", MULTI_STREAM, "--raw"])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(
        output.stdout,
        [
            0x78, 0x9c, 0x2b, 0x54, 0x30, 0x54, 0x30, 0x00, 0x42, 0x08, 0x99, 0x9c, 0x0b, 0x00,
            0x1a, 0x69, 0x03, 0x44,
        ]
    );
}

#[test]
fn binary_pdf_dash_writes_stdout_without_creating_a_dash_file() {
    let directory = tempfile::tempdir().unwrap();
    let output = flpdf()
        .current_dir(directory.path())
        .args([MINIMAL, "-"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert!(output.stdout.starts_with(b"%PDF-"));
    assert!(!Path::new(directory.path()).join("-").exists());
}

#[test]
fn binary_linearized_pdf_dash_uses_the_same_save_route() {
    let output = flpdf()
        .args(["--linearize", ONE_PAGE, "-"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.starts_with(b"%PDF-"));
    assert!(output.stderr.is_empty());
}

#[test]
fn binary_qdf_dash_uses_the_same_save_route() {
    let output = flpdf().args(["qdf", MINIMAL, "-"]).output().unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.starts_with(b"%PDF-"));
    assert!(output.stdout.windows(5).any(|window| window == b"%QDF-"));
    assert!(output.stderr.is_empty());
}

#[test]
fn binary_page_extraction_dash_uses_the_save_route_without_creating_a_dash_file() {
    let directory = tempfile::tempdir().unwrap();
    let output = flpdf()
        .current_dir(directory.path())
        .args([ONE_PAGE, "--pages", ".", "1", "--", "-"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.starts_with(b"%PDF-"));
    assert!(output.stderr.is_empty());
    assert!(!directory.path().join("-").exists());
}

#[test]
fn binary_rotate_dash_uses_the_save_route_without_creating_a_dash_file() {
    let directory = tempfile::tempdir().unwrap();
    let output = flpdf()
        .current_dir(directory.path())
        .args([ONE_PAGE, "-", "--rotate=+90:1"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.starts_with(b"%PDF-"));
    assert!(output.stderr.is_empty());
    assert!(!directory.path().join("-").exists());
}

#[test]
fn split_pages_dash_is_rejected_before_output_is_created() {
    let directory = tempfile::tempdir().unwrap();
    let output = flpdf()
        .current_dir(directory.path())
        .args([ONE_PAGE, "-", "--split-pages=1"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("--split-pages may not be used when writing to standard output"));
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn verbose_page_extraction_dash_keeps_pdf_and_info_on_separate_routes() {
    let output = flpdf()
        .args(["--verbose", ONE_PAGE, "--pages", ".", "1", "--", "-"])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(output.stdout.starts_with(b"%PDF-"));
    assert!(!output
        .stdout
        .windows(b"flpdf:".len())
        .any(|window| window == b"flpdf:"));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("flpdf: selecting --keep-open-files=y")
    );
}

#[test]
fn text_rewrite_verbose_uses_info_route_for_file_output() {
    let directory = tempfile::tempdir().unwrap();
    let output_path = directory.path().join("out.pdf");
    let output = flpdf()
        .args(["--verbose", MINIMAL, output_path.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("flpdf: wrote file {}\n", output_path.display())
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn text_rewrite_verbose_does_not_announce_standard_output() {
    let output = flpdf().args(["--verbose", MINIMAL, "-"]).output().unwrap();

    assert!(output.status.success());
    assert!(output.stdout.starts_with(b"%PDF-"));
    assert!(output.stderr.is_empty());
}

#[test]
fn text_check_success_stays_on_info_route() {
    let output = flpdf().args(["--check", MINIMAL]).output().unwrap();

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.starts_with(&format!("checking {MINIMAL}\n")));
    assert!(stdout.ends_with("errors that flpdf cannot detect\n"));
}

#[test]
fn qpdf_differential_matches_routed_output_matrix() {
    if !qpdf_available() {
        eprintln!("qpdf not available; skipping logger routing differential");
        return;
    }

    let cases: &[(&str, &[&str], &[&str])] = &[
        ("clean check", &["--check", MINIMAL], &["--check", MINIMAL]),
        (
            "warning check",
            &["--check", WARNING_PDF],
            &["--repair", "--check", WARNING_PDF],
        ),
        (
            "JSON stdout",
            &["--json=2", MINIMAL],
            &["--json=2", MINIMAL],
        ),
        (
            "raw stream",
            &["--show-object=4", "--raw-stream-data", MULTI_STREAM],
            &["show-stream", "4 0 R", MULTI_STREAM, "--raw"],
        ),
        (
            "filtered stream",
            &["--show-object=4", "--filtered-stream-data", MULTI_STREAM],
            &["show-stream", "4 0 R", MULTI_STREAM],
        ),
        (
            "attachment",
            &["--show-attachment=attachment.txt", ATTACHMENT_PDF],
            &["--show-attachment=attachment.txt", ATTACHMENT_PDF],
        ),
    ];

    for (label, qpdf_args, flpdf_args) in cases {
        assert_observables_equal(label, &run_qpdf(qpdf_args), &run_flpdf(flpdf_args));
    }
}

#[test]
fn qpdf_differential_classifies_existing_native_open_error_text_gap() {
    if !qpdf_available() {
        eprintln!("qpdf not available; skipping logger routing differential");
        return;
    }

    let missing = "/tmp/flpdf-qynx4-cli-logger-missing.pdf";
    let qpdf = run_qpdf(&["--check", missing]);
    let flpdf = run_flpdf(&["--check", missing]);

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert!(String::from_utf8_lossy(&qpdf.stderr).contains("No such file or directory"));
    assert!(String::from_utf8_lossy(&flpdf.stderr).contains("No such file or directory"));
    assert_ne!(
        flpdf.stderr, qpdf.stderr,
        "native I/O error formatting is an existing oracle mismatch, not a logger route mismatch"
    );
}
