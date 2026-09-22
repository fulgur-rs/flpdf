//! qpdf 11.9.0 parity for a self-referential stream-length resolution loop.

use assert_cmd::Command;
use std::process::Command as ProcessCommand;

#[path = "support/eol.rs"]
mod eol;
use eol::EOL;
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
        panic!("{EXPECTED_QPDF_VERSION} is required for issue-117 parity");
    }
    available
}

fn issue_117_fixture() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = [0usize; 5];

    offsets[1] = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 3 0 R >>\nendobj\n");
    offsets[2] = bytes.len();
    bytes.extend_from_slice(
        b"2 0 obj\n<< /Length 2 0 R >>\nstream\nself-referential\nendstream\nendobj\n",
    );
    offsets[3] = bytes.len();
    bytes.extend_from_slice(b"3 0 obj\n<< /Type /Pages /Kids [4 0 R] /Count 1 >>\nendobj\n");
    offsets[4] = bytes.len();
    bytes.extend_from_slice(
        b"4 0 obj\n<< /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] /Contents 2 0 R >>\nendobj\n",
    );

    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for offset in offsets.iter().skip(1) {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
    bytes
}

#[test]
fn self_referential_stream_resolves_to_null_like_qpdf() {
    if !qpdf_available() {
        eprintln!("skipping issue-117 differential: qpdf 11.9.0 is not available");
        return;
    }

    let directory = tempfile::tempdir().expect("temporary directory");
    let input = directory.path().join("issue-117.pdf");
    std::fs::write(&input, issue_117_fixture()).expect("write issue-117 fixture");

    let qpdf = ProcessCommand::new("qpdf")
        .args(["--show-object=2"])
        .arg(&input)
        .output()
        .expect("qpdf must inspect issue-117 fixture");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["--show-object=2"])
        .arg(&input)
        .output()
        .expect("flpdf must inspect issue-117 fixture");

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(
        normalize_text_newlines(&flpdf.stdout),
        normalize_text_newlines(&qpdf.stdout),
        "object value differs from qpdf"
    );
    assert_eq!(
        normalize_text_newlines(&flpdf.stderr),
        normalize_text_newlines(&qpdf.stderr),
        "diagnostics differ from qpdf"
    );
    assert_eq!(qpdf.stdout, format!("null{EOL}").into_bytes());
}

#[test]
fn self_referential_stream_linearization_matches_qpdf() {
    if !qpdf_available() {
        eprintln!("skipping issue-117 linearization differential: qpdf 11.9.0 is not available");
        return;
    }

    let directory = tempfile::tempdir().expect("temporary directory");
    let input = directory.path().join("issue-117.pdf");
    let qpdf_output = directory.path().join("qpdf.pdf");
    let flpdf_output = directory.path().join("flpdf.pdf");
    std::fs::write(&input, issue_117_fixture()).expect("write issue-117 fixture");

    let qpdf = ProcessCommand::new("qpdf")
        .args(["--deterministic-id", "--linearize"])
        .arg(&input)
        .arg(&qpdf_output)
        .output()
        .expect("qpdf must linearize issue-117 fixture");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["--deterministic-id", "--linearize"])
        .arg(&input)
        .arg(&flpdf_output)
        .output()
        .expect("flpdf must linearize issue-117 fixture");

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    // qpdf's redirected text-mode stderr can carry CRLF on Windows while flpdf
    // emits LF, so compare the same way the preceding differential does.
    assert_eq!(
        normalize_text_newlines(&flpdf.stderr),
        normalize_text_newlines(&qpdf.stderr),
        "linearization diagnostics differ"
    );
    assert!(qpdf_output.exists(), "qpdf must write output");
    assert!(flpdf_output.exists(), "flpdf must write output");

    #[cfg(feature = "qpdf-zlib-compat")]
    assert_eq!(
        std::fs::read(&flpdf_output).expect("read flpdf output"),
        std::fs::read(&qpdf_output).expect("read qpdf output"),
        "linearized output must be byte-identical to qpdf"
    );
}
