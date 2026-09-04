//! qpdf parity for inline direct-container depth during linearization.

use flpdf::{Pdf, PdfWriter};
use std::fs;
use std::io::Cursor;
use std::process::Command;

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

fn qpdf_available() -> bool {
    Command::new("qpdf")
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

fn deep_resource_pdf(depth: usize) -> Vec<u8> {
    let objects = [
        b"<< /Type /Catalog /Pages 2 0 R /Outlines << /Count 0 >> >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
    ];
    let mut page = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources ".to_vec();
    page.extend(std::iter::repeat_n(b'[', depth));
    page.extend(std::iter::repeat_n(b']', depth));
    page.extend_from_slice(b" >>");

    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (number, body) in objects.into_iter().chain(std::iter::once(page)).enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", number + 1).as_bytes());
        pdf.extend_from_slice(&body);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref = pdf.len();
    pdf.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    pdf
}

#[test]
fn linearization_accepts_direct_nesting_within_the_parser_limit() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("qpdf 11.9.0 is required for this parity test on CI");
        }
        eprintln!("skipping: qpdf 11.9.0 is not available");
        return;
    }

    let bytes = deep_resource_pdf(257);
    let temp = tempfile::tempdir().expect("temporary directory");
    let input = temp.path().join("deep257.pdf");
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");
    fs::write(&input, &bytes).expect("write input fixture");

    let qpdf = Command::new("qpdf")
        .args(["--static-id", "--linearize"])
        .arg(&input)
        .arg(&qpdf_output)
        .status()
        .expect("run qpdf");
    assert!(qpdf.success(), "qpdf linearization failed: {qpdf:?}");
    let qpdf_check = Command::new("qpdf")
        .arg("--check")
        .arg(&qpdf_output)
        .status()
        .expect("check qpdf output");
    assert!(
        qpdf_check.success(),
        "qpdf output is invalid: {qpdf_check:?}"
    );

    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("open input fixture");
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_output_memory().expect("configure memory output");
    writer.set_linearization(true);
    writer.set_static_id(true);
    writer
        .write()
        .expect("flpdf must linearize parser-depth input");
    let output = writer.get_buffer().expect("take flpdf output");
    fs::write(&flpdf_output, output).expect("write flpdf output");

    let flpdf_check = Command::new("qpdf")
        .arg("--check")
        .arg(&flpdf_output)
        .status()
        .expect("check flpdf output");
    assert!(
        flpdf_check.success(),
        "flpdf output is invalid: {flpdf_check:?}"
    );
}
