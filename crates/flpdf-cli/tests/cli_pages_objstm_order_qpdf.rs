//! qpdf 11.9.0 byte parity for multi-source pages with generated ObjStms.

#![cfg(feature = "qpdf-zlib-compat")]

use assert_cmd::Command;
use std::path::Path;
use std::process::{Command as ProcessCommand, Output};

const PRIMARY: &str = "../../tests/fixtures/compat/three-page.pdf";
const FOREIGN: &str = "../../tests/fixtures/compat/one-page.pdf";

fn run_qpdf(output: &Path) -> Output {
    ProcessCommand::new("qpdf")
        .args([
            "--static-id",
            "--object-streams=generate",
            PRIMARY,
            "--pages",
            PRIMARY,
            "1",
            FOREIGN,
            "--",
        ])
        .arg(output)
        .output()
        .expect("qpdf should spawn")
}

#[test]
fn multi_source_pages_generated_objstm_members_match_qpdf() {
    let temp = tempfile::tempdir().unwrap();
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");

    let qpdf = run_qpdf(&qpdf_output);
    assert!(
        qpdf.status.success(),
        "qpdf probe failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--static-id",
            "--object-streams=generate",
            PRIMARY,
            "--pages",
            PRIMARY,
            "1",
            FOREIGN,
            "--",
        ])
        .arg(&flpdf_output)
        .assert()
        .success();

    assert_eq!(
        std::fs::read(&flpdf_output).unwrap(),
        std::fs::read(&qpdf_output).unwrap(),
        "multi-source generated ObjStm member order must match qpdf"
    );
}
