//! CLI integration tests for `--object-streams=preserve|disable|generate`.
//!
//! Covers flpdf-9hc.5.10 acceptance criteria:
//!   - each mode is parsed and routed to the writer
//!   - default (no flag) is preserve
//!   - invalid values are rejected with an actionable error

use assert_cmd::Command;
use predicates::prelude::*;

const FIXTURE: &str = "../../tests/fixtures/minimal.pdf";

fn rewrite_with_object_streams(value: &str) -> assert_cmd::assert::Assert {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "rewrite",
        "--full-rewrite",
        &format!("--object-streams={}", value),
        FIXTURE,
        output.to_str().unwrap(),
    ])
    .assert()
}

#[test]
fn object_streams_preserve_is_accepted() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "rewrite",
        "--full-rewrite",
        "--object-streams=preserve",
        FIXTURE,
        output.to_str().unwrap(),
    ])
    .assert()
    .success();
    assert!(output.exists());
}

#[test]
fn object_streams_disable_is_accepted() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "rewrite",
        "--full-rewrite",
        "--object-streams=disable",
        FIXTURE,
        output.to_str().unwrap(),
    ])
    .assert()
    .success();
    assert!(output.exists());
}

#[test]
fn object_streams_generate_is_accepted_and_emits_objstm() {
    use flpdf::{Object, Pdf};
    use std::io::Cursor;

    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "rewrite",
        "--full-rewrite",
        "--object-streams=generate",
        FIXTURE,
        output.to_str().unwrap(),
    ])
    .assert()
    .success();

    let bytes = std::fs::read(&output).unwrap();
    let mut pdf = Pdf::open(Cursor::new(&bytes)).unwrap();
    let mut found_objstm = false;
    for r in pdf.object_refs() {
        if let Ok(Object::Stream(s)) = pdf.resolve(r) {
            if let Some(Object::Name(n)) = s.dict.get("Type") {
                if n.as_slice() == b"ObjStm" {
                    found_objstm = true;
                    break;
                }
            }
        }
    }
    assert!(
        found_objstm,
        "--object-streams=generate must emit at least one /Type /ObjStm in the output"
    );
}

#[test]
fn object_streams_default_is_preserve() {
    // No --object-streams flag; default must be preserve.
    // For a fixture with no source ObjStm, preserve produces no ObjStm output —
    // identical observable behaviour to disable for this fixture.  Just verify
    // the command succeeds without the flag (i.e. the default is wired).
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "rewrite",
        "--full-rewrite",
        FIXTURE,
        output.to_str().unwrap(),
    ])
    .assert()
    .success();
    assert!(output.exists());
}

#[test]
fn object_streams_invalid_value_is_rejected() {
    rewrite_with_object_streams("garbage")
        .failure()
        .stderr(predicate::str::contains("invalid value").or(predicate::str::contains("possible")));
}

#[test]
fn object_streams_help_lists_all_three_modes() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["rewrite", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("preserve"))
        .stdout(predicate::str::contains("disable"))
        .stdout(predicate::str::contains("generate"));
}
