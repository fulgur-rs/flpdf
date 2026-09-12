use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn dump_object_is_absent_from_help() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("dump-object").not());
}

#[test]
fn dump_object_invocation_is_not_dispatched_as_a_native_command() {
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["dump-object", "../../tests/fixtures/minimal.pdf"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("open dump-object"))
        .stderr(predicate::str::contains("Usage: flpdf dump-object").not());
}
