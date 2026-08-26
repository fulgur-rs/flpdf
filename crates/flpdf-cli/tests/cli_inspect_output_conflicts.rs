use assert_cmd::Command;
use predicates::prelude::*;

fn assert_rejects_output(flag_args: &[&str]) {
    let temp = tempfile::tempdir().expect("tempdir");
    let output = temp.path().join("must-not-be-created.pdf");

    let mut command = Command::cargo_bin("flpdf").expect("flpdf binary");
    command.args(flag_args).args([
        "../../tests/fixtures/minimal.pdf",
        output.to_str().expect("UTF-8 temporary path"),
    ]);
    command
        .assert()
        .failure()
        .code(2)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("cannot be used with"));
    assert!(!output.exists(), "inspection must not create output");
}

#[test]
fn check_rejects_output_file() {
    assert_rejects_output(&["--check"]);
}

#[test]
fn show_object_rejects_output_file() {
    assert_rejects_output(&["--show-object", "1 0"]);
}

#[test]
fn show_npages_rejects_output_file() {
    assert_rejects_output(&["--show-npages"]);
}

#[test]
fn show_pages_rejects_output_file() {
    assert_rejects_output(&["--show-pages"]);
}

#[test]
fn show_xref_rejects_output_file() {
    assert_rejects_output(&["--show-xref"]);
}

#[test]
fn show_linearization_rejects_output_file() {
    assert_rejects_output(&["--show-linearization"]);
}

#[test]
fn list_attachments_rejects_output_file() {
    assert_rejects_output(&["--list-attachments"]);
}

#[test]
fn show_attachment_rejects_output_file() {
    assert_rejects_output(&["--show-attachment", "missing"]);
}
