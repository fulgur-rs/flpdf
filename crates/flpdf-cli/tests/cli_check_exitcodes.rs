//! CLI integration tests for qpdf-compatible `--check` / `check` exit codes.
//!
//! # Exit-code semantics (flpdf-9hc.23.2)
//!
//! Source: qpdf manual §"Exit Status"
//!   <https://qpdf.readthedocs.io/en/stable/cli.html#exit-status>
//! Confirmed by `qpdf/include/qpdf/Constants.h`:
//!   qpdf_exit_success = 0  (no errors or warnings)
//!   qpdf_exit_error   = 2  (errors found)
//!   qpdf_exit_warning = 3  (warnings found, no errors)
//!
//! Three fixture classes are exercised:
//!   1. clean PDF            → exit 0
//!   2. warnings-only PDF    → exit 3
//!   3. corrupt/error PDF    → exit 2

use assert_cmd::Command;
use predicates::prelude::*;
use std::io::Write;

// ---------------------------------------------------------------------------
// Fixture builders
// ---------------------------------------------------------------------------

/// Minimal but fully valid single-page PDF — produces exit 0.
fn clean_pdf_bytes() -> Vec<u8> {
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
    pdf.extend_from_slice(
        format!(
            "xref\n0 4\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n"
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    pdf
}

/// PDF with a deliberately corrupt xref table.  When opened with `--repair`
/// the parser recovers via linear scan and emits a "xref repaired" warning
/// (no errors) → exit 3.
fn warnings_only_corrupt_xref_bytes() -> Vec<u8> {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n");
    let offsets: Vec<usize> = {
        let objects: &[&[u8]] = &[
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        ];
        let mut offs = Vec::new();
        for obj in objects {
            offs.push(pdf.len());
            pdf.extend_from_slice(obj);
        }
        offs
    };
    let xref_start = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for off in &offsets {
        pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n",
            offsets.len() + 1
        )
        .as_bytes(),
    );

    // Corrupt the xref keyword so the parser must repair.
    let xref_pos = pdf.windows(4).position(|w| w == b"xref").unwrap();
    pdf[xref_pos + 2] = b'z'; // "xref" → "xrzf"

    pdf
}

/// PDF that is irrecoverably corrupt — no valid objects reachable, causing
/// the check to report errors → exit 2.
fn corrupt_pdf_bytes() -> Vec<u8> {
    b"%PDF-1.4\nthis is not a valid pdf at all\n%%EOF\n".to_vec()
}

/// Valid xref but the trailer lacks /Root — opens fine, check reports an
/// error-severity diagnostic → exit 2.
fn missing_root_pdf_bytes() -> Vec<u8> {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n");
    let off1 = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let off2 = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");
    let xref_start = pdf.len();
    pdf.extend_from_slice(
        format!("xref\n0 3\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n")
            .as_bytes(),
    );
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 3 >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    pdf
}

// ---------------------------------------------------------------------------
// Tests: exit 0 — clean PDF
// ---------------------------------------------------------------------------

#[test]
fn check_clean_pdf_exits_0() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&clean_pdf_bytes()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--check", f.path().to_str().unwrap()])
        .assert()
        .code(0)
        .stdout(predicate::str::contains("PDF check succeeded"))
        .stderr(predicate::str::is_empty());
}

#[test]
fn check_subcommand_clean_pdf_exits_0() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&clean_pdf_bytes()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["check", f.path().to_str().unwrap()])
        .assert()
        .code(0)
        .stdout(predicate::str::contains("PDF check succeeded"))
        .stderr(predicate::str::is_empty());
}

// ---------------------------------------------------------------------------
// Tests: exit 3 — warnings without errors (corrupt xref, repaired)
// ---------------------------------------------------------------------------

#[test]
fn check_warnings_only_pdf_exits_3() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&warnings_only_corrupt_xref_bytes()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    // --repair triggers the recovery heuristic; the resulting "xref repaired"
    // diagnostic is a warning (no error) → exit 3.
    cmd.args(["--check", "--repair", f.path().to_str().unwrap()])
        .assert()
        .code(3)
        .stdout(predicate::str::contains("PDF check succeeded"))
        .stderr(predicate::str::contains("WARNING: "));
}

#[test]
fn check_subcommand_warnings_only_pdf_exits_3() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&warnings_only_corrupt_xref_bytes()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["check", "--repair", f.path().to_str().unwrap()])
        .assert()
        .code(3)
        .stdout(predicate::str::contains("PDF check succeeded"))
        .stderr(predicate::str::contains("WARNING: "));
}

#[test]
fn check_warnings_use_qpdf_stderr_format() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&warnings_only_corrupt_xref_bytes()).unwrap();
    let path = f.path().to_str().unwrap().to_string();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--check", "--repair", &path])
        .assert()
        .code(3)
        .stdout(predicate::str::contains("PDF check succeeded"))
        // qpdf shape: WARNING: <file>: <msg>, surrounding warnings without
        // offset, then the trailing summary line.
        .stderr(predicate::str::contains(format!(
            "WARNING: {path}: file is damaged\n"
        )))
        .stderr(predicate::str::contains(
            "Attempting to reconstruct cross-reference table\n",
        ))
        .stderr(predicate::str::contains(
            "flpdf: operation succeeded with warnings\n",
        ))
        // The old lowercase `warning: <msg>` prefix must be gone.
        .stderr(predicate::str::contains("warning: ").not());
}

/// The trigger warning (and only the trigger warning) carries `(offset N)`.
#[test]
fn check_trigger_warning_carries_offset() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&warnings_only_corrupt_xref_bytes()).unwrap();
    let path = f.path().to_str().unwrap().to_string();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--check", "--repair", &path])
        .assert()
        .code(3)
        .stderr(
            predicate::str::is_match(format!(
                "WARNING: {} \\(offset \\d+\\): ",
                regex::escape(&path)
            ))
            .unwrap(),
        )
        .stderr(predicate::str::contains(format!("WARNING: {path} (offset")).count(1));
}

// ---------------------------------------------------------------------------
// Tests: exit 2 — errors / corrupt PDF
// ---------------------------------------------------------------------------

#[test]
fn check_corrupt_pdf_exits_2() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&corrupt_pdf_bytes()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--check", f.path().to_str().unwrap()])
        .assert()
        .code(2);
}

/// qpdf prints check errors as a single `<progname>: <file>: <msg>` line and
/// no extra "check failed" summary (observed with qpdf 11.9.0 on the same
/// fixture: `qpdf: noroot.pdf: unable to find /Root dictionary`).
#[test]
fn check_error_diagnostics_use_qpdf_stderr_format() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&missing_root_pdf_bytes()).unwrap();
    let path = f.path().to_str().unwrap().to_string();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.env_remove("FLPDF_PROGNAME")
        .args(["--check", &path])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(format!(
            "flpdf: {path}: trailer is missing /Root\n"
        )))
        .stderr(predicate::str::contains("PDF check failed").not())
        .stderr(predicate::str::contains("error: ").not());
}

#[test]
fn check_subcommand_corrupt_pdf_exits_2() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&corrupt_pdf_bytes()).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["check", f.path().to_str().unwrap()])
        .assert()
        .code(2);
}
