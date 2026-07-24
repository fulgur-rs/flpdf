use assert_cmd::Command;
use predicates::str::{contains, is_empty};

#[test]
fn missing_actual_file_reports_error_no_panic() {
    Command::cargo_bin("flpdf-test-compare")
        .unwrap()
        .args(["/no/such/actual.pdf", "/no/such/expected.pdf"])
        .assert()
        .failure()
        .code(2)
        .stdout(is_empty())
        .stderr(contains("flpdf-test-compare:"));
}
