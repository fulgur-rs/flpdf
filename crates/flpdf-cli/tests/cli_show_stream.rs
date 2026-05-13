use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn show_stream_decodes_filtered_content_stream() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "show-stream",
        "7 0",
        "../../tests/fixtures/compat/one-page.pdf",
    ])
    .assert()
    .success()
    .stdout(predicate::str::contains("Fixture page 1"));
}

#[test]
fn show_stream_raw_emits_stored_bytes() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "show-stream",
        "--raw",
        "7 0",
        "../../tests/fixtures/compat/one-page.pdf",
    ])
    .assert()
    .success()
    .stdout(predicate::str::starts_with("GapQh0E"));
}

#[test]
fn show_stream_rejects_non_stream_object() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "show-stream",
        "4 0",
        "../../tests/fixtures/compat/one-page.pdf",
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("is not a stream"));
}

#[test]
fn show_stream_unknown_object_reports_clear_error() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "show-stream",
        "99 0",
        "../../tests/fixtures/compat/one-page.pdf",
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("not found"));
}

#[test]
fn show_stream_writes_to_out_file() {
    let temp = tempfile::tempdir().unwrap();
    let out_path = temp.path().join("stream.txt");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "show-stream",
        "7 0",
        "../../tests/fixtures/compat/one-page.pdf",
        "--out",
    ])
    .arg(&out_path)
    .assert()
    .success();

    let contents = std::fs::read_to_string(&out_path).unwrap();
    assert!(
        contents.contains("Fixture page 1"),
        "expected 'Fixture page 1' in output file, got: {:?}",
        &contents[..contents.len().min(200)]
    );
}
