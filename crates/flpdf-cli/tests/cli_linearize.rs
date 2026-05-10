//! Integration tests for `--linearize` and `check-linearization` CLI surface.
//!
//! Happy-path: linearize a known fixture, then verify with check-linearization.
//! Malformed-path: tamper with /L in the output; check-linearization must exit 1.
//! Regression: plain `rewrite` (no --linearize) still works unchanged.

use assert_cmd::Command;
use predicates::prelude::*;
use std::io::Write as _;

// ---------------------------------------------------------------------------
// Helper: build a minimal single-page PDF fixture in memory
// ---------------------------------------------------------------------------

fn minimal_pdf_bytes() -> Vec<u8> {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n");

    let off1 = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let off2 = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    let off3 = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
    );

    let xref_start = pdf.len();
    let xref = format!(
        "xref\n0 4\n0000000000 65535 f \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \n",
        off1, off2, off3,
    );
    pdf.extend_from_slice(xref.as_bytes());
    let trailer = format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
    pdf.extend_from_slice(trailer.as_bytes());
    pdf
}

fn write_temp(bytes: &[u8]) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(bytes).unwrap();
    f
}

// ---------------------------------------------------------------------------
// 1. Happy path: rewrite --linearize produces a file, check-linearization passes
// ---------------------------------------------------------------------------

#[test]
fn rewrite_linearize_then_check_passes() {
    let input = write_temp(&minimal_pdf_bytes());
    let outdir = tempfile::tempdir().unwrap();
    let output = outdir.path().join("linearized.pdf");

    // --linearize
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--linearize",
            input.path().to_str().unwrap(),
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    assert!(output.exists(), "linearized output file must be created");
    assert!(
        std::fs::metadata(&output).unwrap().len() > 0,
        "linearized output must not be empty"
    );

    // check-linearization must exit 0
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["check-linearization", output.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("linearization OK"));
}

// ---------------------------------------------------------------------------
// 2. check-linearization on a malformed file exits 1 with actionable stderr
// ---------------------------------------------------------------------------

#[test]
fn check_linearization_tampered_l_exits_1() {
    // First produce a valid linearized file.
    let input = write_temp(&minimal_pdf_bytes());
    let outdir = tempfile::tempdir().unwrap();
    let linearized_path = outdir.path().join("linearized.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--linearize",
            input.path().to_str().unwrap(),
            linearized_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Read and tamper: find "/L 0" and bump one digit so /L != file_length.
    let mut bytes = std::fs::read(&linearized_path).unwrap();
    let needle = b"/L 0";
    if let Some(pos) = bytes.windows(needle.len()).position(|w| w == needle) {
        // The 10-digit value starts at pos + 3 (after "/L ").
        let val_start = pos + 3;
        let val_end = val_start + 10;
        // Change the last digit to make /L wrong.
        bytes[val_end - 1] = if bytes[val_end - 1] == b'9' {
            b'0'
        } else {
            bytes[val_end - 1] + 1
        };
    } else {
        panic!("could not find '/L 0' in linearized output for tampering");
    }

    let tampered_path = outdir.path().join("tampered.pdf");
    std::fs::write(&tampered_path, &bytes).unwrap();

    // check-linearization must exit 1 with an actionable message.
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["check-linearization", tampered_path.to_str().unwrap()])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("linearization check failed"));
}

// ---------------------------------------------------------------------------
// 3. check-linearization on a non-linearized PDF exits 1
// ---------------------------------------------------------------------------

#[test]
fn check_linearization_non_linearized_exits_1() {
    let input = write_temp(&minimal_pdf_bytes());

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["check-linearization", input.path().to_str().unwrap()])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("not a linearized PDF"));
}

// ---------------------------------------------------------------------------
// 4. Regression: plain `rewrite` (no --linearize) still works
// ---------------------------------------------------------------------------

#[test]
fn rewrite_without_linearize_flag_still_works() {
    let outdir = tempfile::tempdir().unwrap();
    let output = outdir.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "../../tests/fixtures/minimal.pdf",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    assert!(output.exists());
    assert!(std::fs::metadata(&output).unwrap().len() > 0);
}

// ---------------------------------------------------------------------------
// 5. check-linearization on a missing file exits 2 (I/O error)
// ---------------------------------------------------------------------------

#[test]
fn check_linearization_missing_file_exits_2() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "check-linearization",
            "/tmp/this_file_does_not_exist_flpdf_test.pdf",
        ])
        .assert()
        .failure()
        .code(2);
}
