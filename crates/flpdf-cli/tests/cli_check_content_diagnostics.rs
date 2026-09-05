//! qpdf 11.9.0 differential tests for `--check` content-parser diagnostics.

use assert_cmd::Command;
use std::io::Write;
use std::process::{Command as ProcessCommand, Output};

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

fn qpdf_available() -> bool {
    let output = match ProcessCommand::new("qpdf").arg("--version").output() {
        Ok(output) => output,
        Err(error) => {
            if std::env::var_os("CI").is_some() {
                panic!("qpdf 11.9.0 is required on CI: {error}");
            }
            eprintln!("skipping: qpdf 11.9.0 is unavailable: {error}");
            return false;
        }
    };
    let version = String::from_utf8_lossy(&output.stdout);
    if output.status.success() && version.lines().next() == Some(EXPECTED_QPDF_VERSION) {
        return true;
    }
    if std::env::var_os("CI").is_some() {
        panic!(
            "qpdf 11.9.0 is required on CI; found {:?}",
            version.lines().next()
        );
    }
    eprintln!(
        "skipping: qpdf 11.9.0 is required; found {:?}",
        version.lines().next()
    );
    false
}

fn single_page_content_pdf(content: &[u8]) -> Vec<u8> {
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let off1 = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let off2 = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    let off3 = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n",
    );
    let off4 = pdf.len();
    pdf.extend_from_slice(format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len()).as_bytes());
    pdf.extend_from_slice(content);
    pdf.extend_from_slice(b"\nendstream\nendobj\n");

    let xref_start = pdf.len();
    pdf.extend_from_slice(
        format!(
            "xref\n0 5\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n{off4:010} 00000 n \n"
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    pdf
}

fn run_qpdf(args: &[&str]) -> Output {
    ProcessCommand::new("qpdf")
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

fn assert_check_matches_qpdf(content: &[u8], extra_args: &[&str], expected_warning: Option<&str>) {
    if !qpdf_available() {
        return;
    }

    let mut input = tempfile::NamedTempFile::new().expect("temporary PDF");
    input
        .write_all(&single_page_content_pdf(content))
        .expect("write temporary PDF");
    let path = input.path().to_str().expect("temporary path is UTF-8");
    let mut args = extra_args.to_vec();
    args.extend(["--check", path]);

    let qpdf = run_qpdf(&args);
    let flpdf = run_flpdf(&args);
    assert_eq!(
        flpdf.status.code(),
        qpdf.status.code(),
        "--check exit code must match qpdf; qpdf={qpdf:?}, flpdf={flpdf:?}"
    );
    assert_eq!(
        normalize_text_newlines(&flpdf.stdout),
        normalize_text_newlines(&qpdf.stdout),
        "--check stdout must match qpdf"
    );
    assert_eq!(
        normalize_text_newlines(&flpdf.stderr),
        normalize_text_newlines(&qpdf.stderr),
        "--check stderr must match qpdf"
    );
    let normalized_stderr = normalize_text_newlines(&qpdf.stderr);
    let stderr = String::from_utf8_lossy(&normalized_stderr);
    if let Some(expected_warning) = expected_warning {
        assert!(
            stderr.contains(expected_warning),
            "qpdf fixture must exercise the requested warning: {stderr:?}"
        );
    } else {
        assert!(
            stderr.is_empty(),
            "--no-warn must suppress qpdf warnings: {stderr:?}"
        );
    }
}

#[test]
fn check_invalid_hexstring_warning_matches_qpdf() {
    assert_check_matches_qpdf(b"\r<0g", &[], Some("invalid character (g) in hexstring"));
}

#[test]
fn check_unexpected_brace_warning_matches_qpdf() {
    assert_check_matches_qpdf(b"{", &[], Some("treating unexpected brace token as null"));
}

#[test]
fn check_unterminated_inline_image_warning_matches_qpdf() {
    assert_check_matches_qpdf(
        b"BI /W 1 ID \0",
        &[],
        Some("EOF found while reading inline image"),
    );
}

#[test]
fn check_content_warning_no_warn_matches_qpdf() {
    assert_check_matches_qpdf(b"\r<0g", &["--no-warn"], None);
}
