//! qpdf 11.9.0 `--no-warn` parity for ordinary and output-producing opens.
//!
//! qpdf applies `noWarn` to the QPDF object before it reads the input. These
//! tests compare the complete observable result of that boundary: stdout,
//! stderr, exit status, and (for rewrite) the existence of the output file.

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::process::{Command as ShellCommand, Output};

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

fn repairable_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/test_driver/repairable_input.pdf")
}

fn qpdf_available() -> bool {
    ShellCommand::new("qpdf")
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

fn qpdf_or_skip() -> bool {
    if qpdf_available() {
        return true;
    }
    if std::env::var_os("CI").is_some() {
        panic!("qpdf 11.9.0 is required for --no-warn parity tests on CI");
    }
    eprintln!("skipping: qpdf 11.9.0 is not available");
    false
}

fn run_qpdf(args: &[&str]) -> Output {
    ShellCommand::new("qpdf")
        .args(args)
        .output()
        .expect("qpdf should spawn")
}

fn corrupt_xref_with_page_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let objects = [
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".as_slice(),
        b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n".as_slice(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n".as_slice(),
        b"4 0 obj\n<< /Length 0 >>\nstream\nendstream\nendobj\n".as_slice(),
    ];
    let mut offsets = Vec::new();
    for object in objects {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    }
    let xref_start = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    let xref = bytes
        .windows(4)
        .position(|window| window == b"xref")
        .expect("xref keyword");
    bytes[xref + 2] = b'z';
    bytes
}

fn write_corrupt_xref_with_page(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, corrupt_xref_with_page_pdf()).expect("write corrupt PDF");
    path
}

fn corrupt_startxref_copy(input: &Path, output: &Path) {
    let mut bytes = std::fs::read(input).expect("read source PDF");
    let marker = b"startxref\n";
    let start = bytes
        .windows(marker.len())
        .rposition(|window| window == marker)
        .expect("startxref marker");
    let value_start = start + marker.len();
    let value_end = value_start
        + bytes[value_start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .expect("startxref value newline");
    bytes[value_start..value_end].fill(b'0');
    std::fs::write(output, bytes).expect("write damaged source PDF");
}

fn json_input_with_warning() -> &'static [u8] {
    br#"{
        "qpdf": [
            {"jsonversion": 2, "pdfversion": "bad"},
            {
                "obj:1 0 R": {"value": {"/Type": "/Catalog", "/Pages": "2 0 R"}},
                "obj:2 0 R": {"value": {"/Type": "/Pages", "/Count": 1, "/Kids": ["3 0 R"]}},
                "obj:3 0 R": {"value": {"/Type": "/Page", "/Parent": "2 0 R", "/MediaBox": [0, 0, 612, 792]}},
                "trailer": {"value": {"/Root": "1 0 R", "/Size": 4}}
            }
        ]
    }"#
}

#[test]
fn show_npages_no_warn_matches_qpdf_before_open_diagnostics_are_delivered() {
    if !qpdf_or_skip() {
        return;
    }

    let input = repairable_fixture();
    let input = input.to_str().expect("fixture path is UTF-8");
    let qpdf = run_qpdf(&["--no-warn", "--show-npages", input]);
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["--no-warn", "--show-npages", input])
        .output()
        .expect("flpdf invocation");

    assert_eq!(qpdf.status.code(), Some(3));
    assert!(!qpdf.stdout.is_empty());
    assert!(qpdf.stderr.is_empty());
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[test]
fn is_encrypted_no_warn_matches_qpdf_before_open_diagnostics_are_delivered() {
    if !qpdf_or_skip() {
        return;
    }

    let input = repairable_fixture();
    let input = input.to_str().expect("fixture path is UTF-8");
    let qpdf = run_qpdf(&["--no-warn", "--is-encrypted", input]);
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["--no-warn", "--is-encrypted", input])
        .output()
        .expect("flpdf invocation");

    assert_eq!(qpdf.status.code(), Some(2));
    assert!(qpdf.stderr.is_empty());
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[test]
fn top_level_rewrite_no_warn_matches_qpdf_before_open_diagnostics_are_delivered() {
    if !qpdf_or_skip() {
        return;
    }

    let input = repairable_fixture();
    let temp = tempfile::tempdir().expect("tempdir");
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");
    let input = input.to_str().expect("fixture path is UTF-8");
    let qpdf_output_str = qpdf_output.to_str().expect("qpdf output path is UTF-8");

    let qpdf = run_qpdf(&["--no-warn", input, qpdf_output_str]);
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["--no-warn", input])
        .arg(&flpdf_output)
        .output()
        .expect("flpdf invocation");

    assert_eq!(qpdf.status.code(), Some(3));
    assert!(qpdf.stdout.is_empty());
    assert!(qpdf.stderr.is_empty());
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    assert!(qpdf_output.exists());
    assert!(flpdf_output.exists());
}

#[test]
fn overlay_no_warn_matches_qpdf_for_a_warning_bearing_source() {
    if !qpdf_or_skip() {
        return;
    }

    for (operation, source_name) in [
        ("--overlay", "overlay-source.pdf"),
        ("--underlay", "underlay-source.pdf"),
    ] {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = write_corrupt_xref_with_page(temp.path(), source_name);
        let destination =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
        let qpdf_output = temp.path().join("qpdf.pdf");
        let flpdf_output = temp.path().join("flpdf.pdf");
        let source = source.to_str().expect("source path is UTF-8");
        let destination = destination.to_str().expect("destination path is UTF-8");
        let qpdf_output_str = qpdf_output.to_str().expect("qpdf output path is UTF-8");

        let qpdf = run_qpdf(&[
            "--no-warn",
            operation,
            source,
            "--",
            destination,
            qpdf_output_str,
        ]);
        let flpdf = Command::cargo_bin("flpdf")
            .expect("flpdf binary")
            .args(["--no-warn", destination, operation, source, "--"])
            .arg(&flpdf_output)
            .output()
            .expect("flpdf invocation");

        assert!(
            qpdf.status.success(),
            "qpdf {operation} invocation failed: {}",
            String::from_utf8_lossy(&qpdf.stderr)
        );
        assert!(qpdf.stderr.is_empty());
        assert_eq!(flpdf.status.code(), qpdf.status.code());
        assert_eq!(flpdf.stdout, qpdf.stdout);
        assert_eq!(flpdf.stderr, qpdf.stderr);
        assert!(flpdf_output.exists());
    }
}

#[test]
fn page_source_no_warn_matches_qpdf_for_a_warning_bearing_source() {
    if !qpdf_or_skip() {
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let source_path = write_corrupt_xref_with_page(temp.path(), "page-source.pdf");
    let destination =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");
    let source = source_path.to_str().expect("source path is UTF-8");
    let destination = destination.to_str().expect("destination path is UTF-8");
    let qpdf_output_str = qpdf_output.to_str().expect("qpdf output path is UTF-8");

    let qpdf = run_qpdf(&[
        "--no-warn",
        "--pages",
        source,
        "1",
        "--",
        destination,
        qpdf_output_str,
    ]);
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["--no-warn", "--pages", source, "1", "--", destination])
        .arg(&flpdf_output)
        .output()
        .expect("flpdf invocation");

    assert!(
        qpdf.status.code() == Some(3),
        "qpdf page-source invocation returned {:?}: {}",
        qpdf.status,
        String::from_utf8_lossy(&qpdf.stderr)
    );
    assert!(qpdf.stderr.is_empty());
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    assert!(flpdf_output.exists());
}

#[test]
fn attachment_copy_no_warn_matches_qpdf_for_a_warning_bearing_source() {
    if !qpdf_or_skip() {
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let clean_source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/attachment-two-page.pdf");
    let source_path = temp.path().join("attachment-source.pdf");
    corrupt_startxref_copy(&clean_source, &source_path);
    let destination =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");
    let source = source_path.to_str().expect("source path is UTF-8");
    let destination = destination.to_str().expect("destination path is UTF-8");
    let qpdf_output_str = qpdf_output.to_str().expect("qpdf output path is UTF-8");

    let qpdf = run_qpdf(&[
        "--no-warn",
        "--copy-attachments-from",
        source,
        "--",
        destination,
        qpdf_output_str,
    ]);
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args([
            "--no-warn",
            destination,
            "--copy-attachments-from",
            source,
            "--",
        ])
        .arg(&flpdf_output)
        .output()
        .expect("flpdf invocation");

    assert_eq!(qpdf.status.code(), Some(3));
    assert!(qpdf.stderr.is_empty());
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    assert!(flpdf_output.exists());
}

#[test]
fn copy_encryption_no_warn_matches_qpdf_for_a_warning_bearing_donor() {
    if !qpdf_or_skip() {
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let clean_donor = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/encrypted-r4-three-page.pdf");
    let donor_path = temp.path().join("encryption-donor.pdf");
    corrupt_startxref_copy(&clean_donor, &donor_path);
    let destination =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");
    let donor = donor_path.to_str().expect("donor path is UTF-8");
    let destination = destination.to_str().expect("destination path is UTF-8");
    let qpdf_output_str = qpdf_output.to_str().expect("qpdf output path is UTF-8");
    let copy_option = format!("--copy-encryption={donor}");

    let qpdf = run_qpdf(&[
        "--no-warn",
        &copy_option,
        "--encryption-file-password=",
        "--",
        destination,
        qpdf_output_str,
    ]);
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args([
            "--no-warn",
            &copy_option,
            "--encryption-file-password=",
            "--",
            destination,
        ])
        .arg(&flpdf_output)
        .output()
        .expect("flpdf invocation");

    assert!(qpdf.status.success());
    assert!(qpdf.stderr.is_empty());
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    assert!(qpdf_output.exists());
    assert!(flpdf_output.exists());
}

#[test]
fn json_input_no_warn_matches_qpdf_for_an_import_warning() {
    if !qpdf_or_skip() {
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let input = temp.path().join("input.json");
    std::fs::write(&input, json_input_with_warning()).expect("write JSON input");
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");
    let input = input.to_str().expect("input path is UTF-8");
    let qpdf_output_str = qpdf_output.to_str().expect("qpdf output path is UTF-8");

    let qpdf = run_qpdf(&["--no-warn", "--json-input", input, qpdf_output_str]);
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["--no-warn", "--json-input", input])
        .arg(&flpdf_output)
        .output()
        .expect("flpdf invocation");

    assert_eq!(qpdf.status.code(), Some(2));
    assert!(!qpdf
        .stderr
        .windows(b"WARNING:".len())
        .any(|window| window == b"WARNING:"));
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert!(!flpdf
        .stderr
        .windows(b"WARNING:".len())
        .any(|window| window == b"WARNING:"));
}

/// `--split-pages` builds its own `QPDFJob` for each split output
/// (`crates/flpdf-cli/src/main.rs::split_rewritten_pdf`) rather than reusing
/// the job that opened the input, unlike qpdf's `writeQPDF`
/// (`QPDFJob.cc:483-503`), which calls `doSplitPages` on the same job that
/// applied `noWarn`. That fresh job's own suppression must be threaded
/// through explicitly or the "operation succeeded with warnings" summary
/// (and any warning raised during the split itself) leaks past `--no-warn`.
#[test]
fn split_pages_no_warn_matches_qpdf_for_a_warning_bearing_source() {
    if !qpdf_or_skip() {
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let input = write_corrupt_xref_with_page(temp.path(), "input.pdf");
    let input = input.to_str().expect("input path is UTF-8");

    let qpdf_dir = temp.path().join("qpdf-split");
    let flpdf_dir = temp.path().join("flpdf-split");
    std::fs::create_dir(&qpdf_dir).expect("qpdf split dir");
    std::fs::create_dir(&flpdf_dir).expect("flpdf split dir");
    let qpdf_template = qpdf_dir.join("out%d.pdf");
    let flpdf_template = flpdf_dir.join("out%d.pdf");

    let qpdf = run_qpdf(&[
        "--no-warn",
        input,
        "--split-pages",
        "--",
        qpdf_template.to_str().expect("qpdf template is UTF-8"),
    ]);
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["--no-warn", input, "--split-pages", "--"])
        .arg(&flpdf_template)
        .output()
        .expect("flpdf invocation");

    assert_eq!(qpdf.status.code(), Some(3));
    assert!(qpdf.stdout.is_empty());
    assert!(qpdf.stderr.is_empty());
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    assert!(qpdf_dir.join("out1.pdf").exists());
    assert!(flpdf_dir.join("out1.pdf").exists());
}

/// One page whose content stream starts with a bad token (`\r<0g`), so
/// `--normalize-content=y` records qpdf's "content normalization encountered
/// bad tokens" warning family.
fn bad_content_pdf() -> Vec<u8> {
    let content: &[u8] = b"\r<0g";
    let objects: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>".to_vec(),
        [
            format!("<< /Length {} >>\nstream\n", content.len()).into_bytes(),
            content.to_vec(),
            b"\nendstream".to_vec(),
        ]
        .concat(),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        out.extend_from_slice(object);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

#[test]
fn top_level_rewrite_no_warn_suppresses_normalization_warnings_like_qpdf() {
    if !qpdf_or_skip() {
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let input = temp.path().join("bad-content.pdf");
    std::fs::write(&input, bad_content_pdf()).expect("write bad-content PDF");
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");
    let input_str = input.to_str().expect("input path is UTF-8");
    let qpdf_output_str = qpdf_output.to_str().expect("qpdf output path is UTF-8");

    let qpdf = run_qpdf(&[
        "--no-warn",
        "--normalize-content=y",
        input_str,
        qpdf_output_str,
    ]);
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["--no-warn", "--normalize-content=y", input_str])
        .arg(&flpdf_output)
        .output()
        .expect("flpdf invocation");

    // qpdf records the normalization warning through `QPDF::warn`, which keeps
    // the warning exit status but prints nothing under `--no-warn`.
    assert_eq!(qpdf.status.code(), Some(3));
    assert!(qpdf.stderr.is_empty());
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    assert!(flpdf_output.exists());
}

#[test]
fn attachment_copy_no_warn_suppresses_normalization_warnings_like_qpdf() {
    if !qpdf_or_skip() {
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let input = temp.path().join("bad-content.pdf");
    std::fs::write(&input, bad_content_pdf()).expect("write bad-content PDF");
    let donor = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/attachment-two-page.pdf");
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");
    let input_str = input.to_str().expect("input path is UTF-8");
    let donor_str = donor.to_str().expect("donor path is UTF-8");
    let qpdf_output_str = qpdf_output.to_str().expect("qpdf output path is UTF-8");

    let qpdf = run_qpdf(&[
        "--no-warn",
        "--normalize-content=y",
        "--copy-attachments-from",
        donor_str,
        "--",
        input_str,
        qpdf_output_str,
    ]);
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args([
            "--no-warn",
            "--normalize-content=y",
            "--copy-attachments-from",
            donor_str,
            "--",
            input_str,
        ])
        .arg(&flpdf_output)
        .output()
        .expect("flpdf invocation");

    assert_eq!(qpdf.status.code(), Some(3));
    assert!(qpdf.stderr.is_empty());
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
    assert!(flpdf_output.exists());
}
