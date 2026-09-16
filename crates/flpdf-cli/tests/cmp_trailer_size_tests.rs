//! qpdf 11.9.0 parity for a malformed trailer with a misspelled `/Size` key.

use assert_cmd::Command;
use std::process::Command as ProcessCommand;

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
        panic!("{EXPECTED_QPDF_VERSION} is required for trailer-size parity");
    }
    available
}

fn malformed_trailer_fixture() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n%\xbf\xf7\xa2\xfe\n".to_vec();
    let mut offsets = [0usize; 4];
    offsets[1] = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offsets[2] = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    offsets[3] = bytes.len();
    bytes.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>\nendobj\n",
    );

    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
    for offset in offsets.iter().skip(1) {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Root 1 0 R /Siqe 7 >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
    bytes
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

fn contains(bytes: &[u8], needle: &[u8]) -> bool {
    bytes.windows(needle.len()).any(|window| window == needle)
}

#[test]
fn misspelled_trailer_size_key_is_preserved_like_qpdf() {
    if !qpdf_available() {
        eprintln!("skipping qpdf differential: qpdf 11.9.0 is not available");
        return;
    }

    let directory = tempfile::tempdir().expect("temporary directory");
    let input = directory.path().join("bad9.pdf");
    let qpdf_output = directory.path().join("qpdf.pdf");
    let flpdf_output = directory.path().join("flpdf.pdf");
    std::fs::write(&input, malformed_trailer_fixture()).expect("write malformed trailer fixture");

    let qpdf = ProcessCommand::new("qpdf")
        .args(["--static-id"])
        .arg(&input)
        .arg(&qpdf_output)
        .output()
        .expect("qpdf must rewrite malformed trailer fixture");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["--static-id"])
        .arg(&input)
        .arg(&flpdf_output)
        .output()
        .expect("flpdf must rewrite malformed trailer fixture");

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(
        normalize_text_newlines(&flpdf.stderr),
        normalize_text_newlines(&qpdf.stderr),
        "diagnostics differ from qpdf"
    );

    let qpdf_bytes = std::fs::read(&qpdf_output).expect("read qpdf output");
    let flpdf_bytes = std::fs::read(&flpdf_output).expect("read flpdf output");
    assert!(
        !contains(&qpdf_bytes, b"/Size"),
        "qpdf oracle must omit /Size"
    );
    assert!(
        contains(&qpdf_bytes, b"/Siqe 7"),
        "qpdf oracle must preserve /Siqe"
    );
    assert_eq!(flpdf_bytes, qpdf_bytes, "output differs from qpdf");
}

#[test]
fn misspelled_trailer_size_key_is_not_added_to_generated_xref_stream() {
    if !qpdf_available() {
        eprintln!("skipping qpdf differential: qpdf 11.9.0 is not available");
        return;
    }

    let directory = tempfile::tempdir().expect("temporary directory");
    let input = directory.path().join("bad9.pdf");
    let qpdf_output = directory.path().join("qpdf.pdf");
    let flpdf_output = directory.path().join("flpdf.pdf");
    std::fs::write(&input, malformed_trailer_fixture()).expect("write malformed trailer fixture");

    let qpdf = ProcessCommand::new("qpdf")
        .args(["--static-id", "--object-streams=generate"])
        .arg(&input)
        .arg(&qpdf_output)
        .output()
        .expect("qpdf must generate an xref stream");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["--static-id", "--object-streams=generate"])
        .arg(&input)
        .arg(&flpdf_output)
        .output()
        .expect("flpdf must generate an xref stream");

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(
        normalize_text_newlines(&flpdf.stderr),
        normalize_text_newlines(&qpdf.stderr),
        "diagnostics differ from qpdf"
    );

    let qpdf_bytes = std::fs::read(&qpdf_output).expect("read qpdf output");
    let flpdf_bytes = std::fs::read(&flpdf_output).expect("read flpdf output");
    assert!(
        !contains(&qpdf_bytes, b"/Size") && !contains(&flpdf_bytes, b"/Size"),
        "neither normal trailer nor generated xref stream may add /Size"
    );
    assert!(contains(&flpdf_bytes, b"/Siqe 7"));
}
