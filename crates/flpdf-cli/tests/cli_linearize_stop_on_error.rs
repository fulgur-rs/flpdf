//! Differential coverage for qpdf's fatal linearization-plan invariants.

use assert_cmd::Command;
use std::io::Write;
use std::process::Command as ProcessCommand;

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

fn qpdf_available() -> bool {
    ProcessCommand::new("qpdf")
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

fn skip_if_qpdf_missing() -> bool {
    if qpdf_available() {
        return false;
    }
    if std::env::var_os("CI").is_some() {
        panic!("qpdf version 11.9.0 is required for this differential test");
    }
    eprintln!("skipping: qpdf version 11.9.0 is not available");
    true
}

/// Reference the first page from a non-Root trailer key. qpdf's object-user
/// walk then gives that page both a page user and a trailer-key user, so the
/// first page is not lc_first_page_private and QPDF_linearization.cc:1194
/// stops the write.
fn first_page_shared_by_trailer_pdf() -> Vec<u8> {
    let mut pdf = b"%PDF-1.3\n".to_vec();
    let catalog = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let pages = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    let page = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>\nendobj\n",
    );
    let xref = pdf.len();
    pdf.extend_from_slice(
        format!(
            "xref\n0 4\n0000000000 65535 f \n{catalog:010} 00000 n \n{pages:010} 00000 n \n{page:010} 00000 n \ntrailer\n<< /Size 4 /Root 1 0 R /Extra 3 0 R >>\nstartxref\n{xref}\n%%EOF\n"
        )
        .as_bytes(),
    );
    pdf
}

#[test]
fn linearize_rejects_a_first_page_that_is_not_private_like_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }

    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("first-page-shared.pdf");
    let qpdf_output = tempdir.path().join("qpdf-output.pdf");
    let flpdf_output = tempdir.path().join("flpdf-output.pdf");
    let mut file = std::fs::File::create(&input).unwrap();
    file.write_all(&first_page_shared_by_trailer_pdf()).unwrap();

    let qpdf = ProcessCommand::new("qpdf")
        .args(["--deterministic-id", "--linearize"])
        .arg(&input)
        .arg(&qpdf_output)
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["--deterministic-id", "--linearize"])
        .arg(&input)
        .arg(&flpdf_output)
        .output()
        .unwrap();

    assert_eq!(
        qpdf.status.code(),
        Some(2),
        "qpdf stderr: {:?}",
        qpdf.stderr
    );
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stderr, qpdf.stderr);
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(
        std::fs::metadata(&flpdf_output).unwrap().len(),
        0,
        "a failed linearization must not leave output bytes"
    );
    assert_eq!(
        std::fs::metadata(&qpdf_output).unwrap().len(),
        0,
        "qpdf's failed linearization output is empty"
    );
}
