//! qpdf 11.9.0 parity for warnings from a present, malformed trailer `/ID` value.

use assert_cmd::Command;
use std::process::Command as ProcessCommand;

#[path = "support/text_newlines.rs"]
mod text_newlines;
use text_newlines::normalize_text_newlines;

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

fn qpdf_available() -> bool {
    let available = ProcessCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .is_some_and(|line| line.trim() == EXPECTED_QPDF_VERSION)
        })
        .unwrap_or(false);
    if !available && std::env::var_os("CI").is_some() {
        panic!("{EXPECTED_QPDF_VERSION} is required for trailer-value parity");
    }
    available
}

fn malformed_id_trailer_fixture() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n%\xbf\xf7\xa2\xfe\n".to_vec();
    let mut offsets = [0usize; 5];
    offsets[1] = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offsets[2] = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    offsets[3] = bytes.len();
    bytes.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>\nendobj\n",
    );
    offsets[4] = bytes.len();
    bytes.extend_from_slice(b"4 0 obj\n<< >>\nendobj\n");

    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for offset in offsets.iter().skip(1) {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend(std::iter::repeat_n(b' ', 38));
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R /ID 7 >>\nstartxref\n{xref_offset}\n%%EOF\n")
            .as_bytes(),
    );
    bytes
}
#[test]
fn malformed_nested_trailer_value_preserves_qpdf_description_and_offset() {
    if !qpdf_available() {
        eprintln!("skipping qpdf differential: qpdf 11.9.0 is not available");
        return;
    }

    let directory = tempfile::tempdir().expect("temporary directory");
    let input = directory.path().join("in.pdf");
    let qpdf_output = directory.path().join("qpdf.pdf");
    let flpdf_output = directory.path().join("flpdf.pdf");
    std::fs::write(&input, malformed_id_trailer_fixture()).expect("write trailer fixture");

    let qpdf = ProcessCommand::new("qpdf")
        .args(["--static-id"])
        .arg(&input)
        .arg(&qpdf_output)
        .output()
        .expect("qpdf must rewrite trailer fixture");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["--static-id"])
        .arg(&input)
        .arg(&flpdf_output)
        .output()
        .expect("flpdf must rewrite trailer fixture");

    assert_eq!(qpdf.status.code(), Some(3), "qpdf warning status");
    assert_eq!(flpdf.status.code(), qpdf.status.code(), "exit codes differ");
    assert!(
        String::from_utf8_lossy(&qpdf.stderr).contains("in.pdf, trailer at offset"),
        "qpdf stderr: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );
    assert!(
        String::from_utf8_lossy(&qpdf.stderr).contains("offset 393"),
        "qpdf stderr: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );
    assert_eq!(
        normalize_text_newlines(&flpdf.stderr),
        normalize_text_newlines(&qpdf.stderr),
        "diagnostics differ from qpdf"
    );
    assert_eq!(
        std::fs::read(&flpdf_output).expect("read flpdf output"),
        std::fs::read(&qpdf_output).expect("read qpdf output"),
        "output differs from qpdf"
    );
}
