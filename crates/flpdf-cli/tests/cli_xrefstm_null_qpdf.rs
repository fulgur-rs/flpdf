//! Differential coverage for null-valued classic-trailer /XRefStm keys.

use assert_cmd::Command;
use std::process::Command as ProcessCommand;

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

fn qpdf_available() -> bool {
    ProcessCommand::new("qpdf")
        .arg("--version")
        .output()
        .is_ok_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .is_some_and(|line| line.trim() == EXPECTED_QPDF_VERSION)
        })
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

fn xrefstm_null_pdf(indirect: bool) -> Vec<u8> {
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = [0usize; 5];
    for (number, body) in [
        b"<< /Type /Catalog /Pages 2 0 R >>".as_slice(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".as_slice(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>".as_slice(),
        b"null".as_slice(),
    ]
    .into_iter()
    .enumerate()
    {
        let number = number + 1;
        offsets[number] = pdf.len();
        pdf.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        pdf.extend_from_slice(body);
        pdf.extend_from_slice(b"\nendobj\n");
    }

    let xref_offset = pdf.len();
    pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for offset in &offsets[1..] {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    let xrefstm = if indirect { "4 0 R" } else { "null" };
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size 5 /Root 1 0 R /XRefStm {xrefstm} >>\nstartxref\n{xref_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );
    pdf
}

fn assert_show_npages_matches_qpdf(pdf_bytes: &[u8], file_name: &str) {
    if skip_if_qpdf_missing() {
        return;
    }

    let temp = tempfile::tempdir().expect("create xref fixture directory");
    let input = temp.path().join(file_name);
    std::fs::write(&input, pdf_bytes).expect("write xref fixture");
    let qpdf = ProcessCommand::new("qpdf")
        .arg("--show-npages")
        .arg(&input)
        .output()
        .expect("run qpdf 11.9.0");
    let flpdf = Command::new(assert_cmd::cargo::cargo_bin!("flpdf"))
        .env("FLPDF_PROGNAME", "qpdf")
        .arg("--show-npages")
        .arg(&input)
        .output()
        .expect("run flpdf");

    assert_eq!(qpdf.status.code(), Some(0), "qpdf fixture must be accepted");
    assert_eq!(qpdf.stdout, b"1\n");
    assert!(qpdf.stderr.is_empty(), "qpdf stderr: {:?}", qpdf.stderr);
    assert_eq!(
        flpdf.status.code(),
        qpdf.status.code(),
        "{file_name} status"
    );
    assert_eq!(flpdf.stdout, qpdf.stdout, "{file_name} stdout");
    assert_eq!(flpdf.stderr, qpdf.stderr, "{file_name} stderr");
}

#[test]
fn direct_null_xrefstm_is_absent_like_qpdf() {
    assert_show_npages_matches_qpdf(&xrefstm_null_pdf(false), "direct-null-xrefstm.pdf");
}

#[test]
fn indirect_null_xrefstm_is_absent_like_qpdf() {
    assert_show_npages_matches_qpdf(&xrefstm_null_pdf(true), "indirect-null-xrefstm.pdf");
}
