use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn version_prints_and_exits_zero() {
    Command::cargo_bin("flpdf-test-compare")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(contains("flpdf-test-compare from flpdf version "));
}
