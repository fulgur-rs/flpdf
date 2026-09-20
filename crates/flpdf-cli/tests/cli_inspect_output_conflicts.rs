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
    // qpdf's checkConfiguration rejects an output file for these
    // `require_outfile = false` options with a fixed message
    // (`QPDFJob.cc:593-594`), confirmed against live qpdf 11.9.0 for every
    // flag exercised below. This is not clap's generic "cannot be used
    // with" conflict text.
    command
        .assert()
        .failure()
        .code(2)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains(
            "no output file may be given for this option",
        ));
    assert!(!output.exists(), "inspection must not create output");
}

#[test]
fn check_rejects_output_file() {
    assert_rejects_output(&["--check"]);
}

#[test]
fn show_object_rejects_output_file() {
    assert_rejects_output(&["--show-object=1 0"]);
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
    assert_rejects_output(&["--show-attachment=missing"]);
}

#[test]
fn show_encryption_rejects_output_file() {
    assert_rejects_output(&["--show-encryption"]);
}

/// `--requires-password` and `--is-encrypted` are a separate
/// `checkConfiguration` check (`QPDFJob.cc:597-599`), independent of the
/// output-file rejection above, so it gets its own assertion rather than
/// `assert_rejects_output`'s message and output-arg shape.
#[test]
fn requires_password_and_is_encrypted_reject_each_other() {
    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args([
            "--requires-password",
            "--is-encrypted",
            "../../tests/fixtures/minimal.pdf",
        ])
        .assert()
        .failure()
        .code(2)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains(
            "--requires-password and --is-encrypted may not be given together",
        ));
}
