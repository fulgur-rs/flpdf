use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn show_stream_is_absent_from_help() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("show-stream").not());
}

#[test]
fn show_stream_invocation_is_not_dispatched_as_a_native_command() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["show-stream", "../../tests/fixtures/minimal.pdf"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("open show-stream"))
        .stderr(predicate::str::contains("Usage: flpdf show-stream").not());
}
