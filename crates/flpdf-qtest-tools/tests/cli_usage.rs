use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn zero_args_prints_usage_and_exits_two() {
    Command::cargo_bin("flpdf-test-compare")
        .unwrap()
        .assert()
        .failure()
        .code(2)
        .stderr(contains("Usage:"))
        .stderr(contains("actual expected"));
}

#[test]
fn too_many_args_prints_usage_and_exits_two() {
    Command::cargo_bin("flpdf-test-compare")
        .unwrap()
        .args(["a", "b", "c", "d"])
        .assert()
        .failure()
        .code(2)
        .stderr(contains("Usage:"));
}
