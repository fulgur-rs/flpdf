//! Differential coverage for the exact offset qpdf records on its
//! "accepting invalid xref table entry" warning (`libqpdf/QPDF.cc:836`).
//!
//! flpdf-sy8i tracked a 1-byte divergence in this warning's recorded offset:
//! qpdf's `parse_xrefEntry` warns using `InputSource::readLine`'s
//! `last_offset`, which is the file position at the *start* of the entry
//! line, before it was read (`libqpdf/InputSource.cc:20-41`). This
//! divergence no longer reproduces on `main` (fixed alongside
//! eaf81385a's classic-table start-position alignment, verified
//! independently of this test), but nothing pinned the exact offset value
//! -- the existing unit coverage in `crates/flpdf/src/xref.rs` only asserts
//! the warning's message text, which would still pass with a wrong offset.
//! This locks in full stdout+stderr byte-equality against real qpdf for
//! three independent malformations (leading/double whitespace at object 0,
//! at a later object, and a short digit-field width at a later object).

use assert_cmd::Command;
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

/// A minimal three-page-object PDF whose classic xref table's free-list-head
/// entry (object 0) has a leading space and double-space field separators:
/// `" 0000000000  65535  f "`. Both the leading and doubled whitespace are
/// individually sufficient to trip qpdf's `invalid` flag
/// (`libqpdf/QPDF.cc:770-836`).
fn entry0_leading_and_double_space_pdf() -> Vec<u8> {
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
    pdf.extend_from_slice(b"xref\n0 4\n");
    pdf.extend_from_slice(b" 0000000000  65535  f \n");
    pdf.extend_from_slice(format!("{catalog:010} 00000 n \n").as_bytes());
    pdf.extend_from_slice(format!("{pages:010} 00000 n \n").as_bytes());
    pdf.extend_from_slice(format!("{page:010} 00000 n \n").as_bytes());
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    pdf
}

/// As above, but the malformed leading/double-space entry is a later object
/// (index 2) rather than the free-list head, so the recorded offset depends
/// on the cursor position accumulated by two preceding, well-formed reads.
fn entry2_leading_and_double_space_pdf() -> Vec<u8> {
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
    pdf.extend_from_slice(b"xref\n0 4\n");
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    pdf.extend_from_slice(format!("{catalog:010} 00000 n \n").as_bytes());
    pdf.extend_from_slice(format!(" {pages:010}  00000  n \n").as_bytes());
    pdf.extend_from_slice(format!("{page:010} 00000 n \n").as_bytes());
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    pdf
}

/// As above, but the anomaly is a short digit-field width (9 digits instead
/// of the required 10) rather than any extra whitespace -- qpdf's
/// `invalid` flag has a third, independent trigger for this
/// (`libqpdf/QPDF.cc:829-831`).
fn entry2_short_offset_field_pdf() -> Vec<u8> {
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
    pdf.extend_from_slice(b"xref\n0 4\n");
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    pdf.extend_from_slice(format!("{catalog:010} 00000 n \n").as_bytes());
    pdf.extend_from_slice(format!("{pages:09} 00000 n \n").as_bytes());
    pdf.extend_from_slice(format!("{page:010} 00000 n \n").as_bytes());
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    pdf
}

fn assert_check_matches_qpdf(pdf_bytes: &[u8], tag: &str) {
    if skip_if_qpdf_missing() {
        return;
    }
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join(format!("{tag}.pdf"));
    std::fs::write(&input, pdf_bytes).unwrap();

    let qpdf = ProcessCommand::new("qpdf")
        .arg("--check")
        .arg(&input)
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_PROGNAME", "qpdf")
        .arg("--check")
        .arg(&input)
        .output()
        .unwrap();

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stderr, qpdf.stderr, "{tag}: stderr must match qpdf exactly, including the accepting-invalid-xref-table-entry warning's offset");
    assert_eq!(
        flpdf.stdout, qpdf.stdout,
        "{tag}: stdout must match qpdf exactly"
    );
    assert!(
        String::from_utf8_lossy(&qpdf.stderr).contains("accepting invalid xref table entry"),
        "{tag}: fixture must actually exercise the warning under test: {:?}",
        qpdf.stderr
    );
}

#[test]
fn check_matches_qpdf_invalid_entry_offset_at_the_free_list_head() {
    assert_check_matches_qpdf(&entry0_leading_and_double_space_pdf(), "entry0-malformed");
}

#[test]
fn check_matches_qpdf_invalid_entry_offset_at_a_later_object() {
    assert_check_matches_qpdf(&entry2_leading_and_double_space_pdf(), "entry2-malformed");
}

#[test]
fn check_matches_qpdf_invalid_entry_offset_for_a_short_digit_field() {
    assert_check_matches_qpdf(&entry2_short_offset_field_pdf(), "entry2-short-field");
}
