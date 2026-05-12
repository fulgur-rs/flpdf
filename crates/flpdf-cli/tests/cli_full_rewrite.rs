use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn full_rewrite_flag_produces_output() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "rewrite",
        "--full-rewrite",
        "../../tests/fixtures/minimal.pdf",
        output.to_str().unwrap(),
    ])
    .assert()
    .success();

    assert!(output.exists());
    assert!(std::fs::metadata(&output).unwrap().len() > 0);
}

#[test]
fn full_rewrite_output_is_valid_pdf() {
    use flpdf::{check_reader, Pdf};
    use std::io::Cursor;

    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "rewrite",
        "--full-rewrite",
        "../../tests/fixtures/minimal.pdf",
        output.to_str().unwrap(),
    ])
    .assert()
    .success();

    let bytes = std::fs::read(&output).unwrap();
    let report = check_reader(Cursor::new(&bytes)).unwrap();
    assert!(
        report.valid,
        "full-rewrite CLI output should be valid; diagnostics: {:?}",
        report.diagnostics.entries()
    );

    // Trailer must not have /Prev.
    let pdf = Pdf::open(Cursor::new(&bytes)).unwrap();
    assert!(
        pdf.trailer().get("Prev").is_none(),
        "full-rewrite output must not have /Prev"
    );
}

#[test]
fn full_rewrite_and_linearize_are_mutually_exclusive() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "rewrite",
        "--full-rewrite",
        "--linearize",
        "../../tests/fixtures/minimal.pdf",
        output.to_str().unwrap(),
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("cannot be used together"));
}
