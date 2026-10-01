use assert_cmd::Command;
use std::process::{Command as ProcessCommand, Output};

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";
const QPDF_HEADER_WARNING: &str =
    "object stream 3 (object 3 0, offset 7): expected integer in object stream header";
const FLPDF_HEADER_WARNING: &str =
    "(object 3 0, offset 7): expected integer in object stream header";

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

fn run_qpdf_check(input: &std::path::Path, suppress_recovery: bool) -> Output {
    let mut command = ProcessCommand::new("qpdf");
    if suppress_recovery {
        command.arg("--suppress-recovery");
    }
    command
        .arg("--check")
        .arg(input)
        .output()
        .expect("run qpdf --check")
}

fn run_flpdf_check(input: &std::path::Path, suppress_recovery: bool) -> Output {
    let mut command = Command::cargo_bin("flpdf").expect("flpdf binary");
    if suppress_recovery {
        command.arg("--suppress-recovery");
    }
    command
        .arg("--check")
        .arg(input)
        .output()
        .expect("run flpdf --check")
}

fn assert_objstm_warning_precedes_member_followups(stderr: &str, header_warning: &str) {
    let header = stderr
        .find(header_warning)
        .unwrap_or_else(|| panic!("missing ObjStm warning in stderr: {stderr}"));
    let followup = stderr
        .find("operation for dictionary attempted on object of type null")
        .unwrap_or_else(|| panic!("missing null-member follow-up warning in stderr: {stderr}"));
    assert!(
        header < followup,
        "ObjStm header warning must precede member follow-ups: {stderr}"
    );
}

#[test]
fn objstm_header_warning_offset_matches_qpdf_with_and_without_recovery() {
    if !qpdf_available() {
        eprintln!("skipping: qpdf 11.9.0 is not available");
        return;
    }

    let fixture =
        include_bytes!("../../../tests/fixtures/compat/objstm-header-token-underflow.pdf");
    let directory = tempfile::tempdir().expect("create malformed ObjStm fixture directory");
    let input = directory.path().join("input.pdf");
    std::fs::write(&input, fixture).expect("write malformed ObjStm fixture");

    for suppress_recovery in [false, true] {
        let qpdf = run_qpdf_check(&input, suppress_recovery);
        let flpdf = run_flpdf_check(&input, suppress_recovery);

        assert_eq!(
            qpdf.status.code(),
            Some(3),
            "qpdf stderr: {:?}",
            qpdf.stderr
        );
        assert_eq!(
            flpdf.status.code(),
            qpdf.status.code(),
            "flpdf stderr: {:?}",
            flpdf.stderr
        );
        assert!(
            String::from_utf8_lossy(&qpdf.stderr).contains(QPDF_HEADER_WARNING),
            "qpdf stderr: {}",
            String::from_utf8_lossy(&qpdf.stderr)
        );
        assert_objstm_warning_precedes_member_followups(
            &String::from_utf8_lossy(&qpdf.stderr),
            QPDF_HEADER_WARNING,
        );
        assert!(
            String::from_utf8_lossy(&flpdf.stderr).contains(FLPDF_HEADER_WARNING),
            "flpdf stderr: {}",
            String::from_utf8_lossy(&flpdf.stderr)
        );
        assert_objstm_warning_precedes_member_followups(
            &String::from_utf8_lossy(&flpdf.stderr),
            FLPDF_HEADER_WARNING,
        );
    }
}
