//! qpdf 11.9.0 parity for linearization stop-on-error source locations.

use assert_cmd::Command;
use std::process::{Command as ProcessCommand, Output};

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

fn qpdf_available() -> bool {
    let output = match ProcessCommand::new("qpdf").arg("--version").output() {
        Ok(output) => output,
        Err(error) => {
            if std::env::var_os("CI").is_some() {
                panic!("qpdf 11.9.0 is required: {error}");
            }
            eprintln!("skipping qlwe5 differential: qpdf unavailable: {error}");
            return false;
        }
    };
    let version = String::from_utf8_lossy(&output.stdout);
    if output.status.success() && version.lines().next() == Some(EXPECTED_QPDF_VERSION) {
        true
    } else if std::env::var_os("CI").is_some() {
        panic!(
            "qpdf 11.9.0 is required; found {:?}",
            version.lines().next()
        );
    } else {
        eprintln!(
            "skipping qlwe5 differential: expected {EXPECTED_QPDF_VERSION}, found {:?}",
            version.lines().next()
        );
        false
    }
}

fn run_qpdf(input: &std::path::Path, output: &std::path::Path) -> Output {
    ProcessCommand::new("qpdf")
        .args(["--deterministic-id", "--linearize"])
        .arg(input)
        .arg(output)
        .output()
        .expect("qpdf should spawn")
}

fn run_flpdf(input: &std::path::Path, output: &std::path::Path) -> Output {
    Command::cargo_bin("flpdf")
        .expect("flpdf should build")
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["--deterministic-id", "--linearize"])
        .arg(input)
        .arg(output)
        .output()
        .expect("flpdf should spawn")
}

fn pdf_with_objects(objects: &[(u32, &[u8])], trailer: &[u8]) -> Vec<u8> {
    let mut pdf = b"%PDF-1.4\n%\xbf\xf7\xa2\xfe\n".to_vec();
    let mut offsets = Vec::new();
    for &(number, body) in objects {
        offsets.push((number, pdf.len()));
        pdf.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        pdf.extend_from_slice(body);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref = pdf.len();
    let size = offsets.iter().map(|(number, _)| *number).max().unwrap_or(0) + 1;
    pdf.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    let mut next = 1u32;
    for (number, offset) in offsets {
        while next < number {
            pdf.extend_from_slice(b"0000000000 65535 f \n");
            next += 1;
        }
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        next += 1;
    }
    while next < size {
        pdf.extend_from_slice(b"0000000000 65535 f \n");
        next += 1;
    }
    pdf.extend_from_slice(b"trailer\n");
    pdf.extend_from_slice(trailer);
    pdf.extend_from_slice(format!("\nstartxref\n{xref}\n%%EOF\n").as_bytes());
    pdf
}

fn filter_on_write_empty_pages_pdf() -> Vec<u8> {
    pdf_with_objects(
        &[
            (1, b"<< /Pages 6 0 R /Type /Catalog >>"),
            (2, b"<< /Filter /RunLengthDecode /Length 5 >>\nstream\n\x81w\xb9w\x80endstream"),
            (3, b"<< /Length 6 >>\nstream\npotatoendstream"),
            (4, b"<< /Length 12 /Filter /FlateDecode >>\nstream\nx\x9c+/\x1f\x1e\0\0\x82@\\\xf9endstream"),
            (5, b"<< /Length 13 /Filter /FlateDecode >>\nstream\nx\x9c+N\xccIL\x01\0\x062\x02\x06endstream"),
            (6, b"<< /Count 0 /Kids [ ] /Type /Pages >>"),
        ],
        b"<< /Root 1 0 R /S1 2 0 R /S2 3 0 R /S3 4 0 R /S4 5 0 R /Size 7 /ID [<5ab7a0329a828e2f46377e16247bc367><31415926535897932384626433832795>] >>",
    )
}

fn pages_loop_pdf() -> Vec<u8> {
    pdf_with_objects(
        &[
            (1, b"<< /Pages 2 0 R /Type /Catalog >>"),
            (2, b"<< /Count 1 /Kids [ 3 0 R 2 0 R ] /Type /Pages >>"),
            (3, b"<< /Contents 4 0 R /MediaBox [ 0 0 612 792 ] /Parent 2 0 R /Resources << /Font << /F1 6 0 R >> /ProcSet 7 0 R >> /Type /Page >>"),
            (4, b"<< /Length 5 >>\nstream\nPotatoendstream"),
            (5, b"44"),
            (6, b"<< /BaseFont /Helvetica /Encoding /WinAnsiEncoding /Name /F1 /Subtype /Type1 /Type /Font >>"),
            (7, b"[ /PDF /Text ]"),
        ],
        b"<< /ID [<395875d4235973eebbade9c7e9e7f857><395875d4235973eebbade9c7e9e7f857>] /Root 1 0 R /Size 8 >>",
    )
}

fn assert_linearization_diagnostics_match(name: &str, bytes: Vec<u8>) {
    if !qpdf_available() {
        return;
    }
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = directory.path().join(format!("{name}.pdf"));
    let qpdf_output = directory.path().join("qpdf.pdf");
    let flpdf_output = directory.path().join("flpdf.pdf");
    std::fs::write(&input, bytes).expect("write fixture");

    let qpdf = run_qpdf(&input, &qpdf_output);
    let flpdf = run_flpdf(&input, &flpdf_output);
    assert_eq!(flpdf.status.code(), qpdf.status.code(), "{name}: status");
    assert_eq!(flpdf.stderr, qpdf.stderr, "{name}: stderr");
    assert_eq!(
        std::fs::read(&flpdf_output).unwrap_or_default(),
        std::fs::read(&qpdf_output).unwrap_or_default(),
        "{name}: failed output bytes"
    );
    assert_eq!(qpdf.status.code(), Some(2), "{name}: qpdf must stop");
    assert_eq!(std::fs::metadata(&qpdf_output).unwrap().len(), 0);
    assert_eq!(std::fs::metadata(&flpdf_output).unwrap().len(), 0);
}

#[test]
fn filter_on_write_empty_pages_preserves_qpdf_stop_offset() {
    assert_linearization_diagnostics_match(
        "filter-on-write-out",
        filter_on_write_empty_pages_pdf(),
    );
}

#[test]
fn pages_loop_preserves_qpdf_last_object_description() {
    assert_linearization_diagnostics_match("pages-loop", pages_loop_pdf());
}
