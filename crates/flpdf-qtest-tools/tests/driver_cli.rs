use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;

fn driver() -> Command {
    Command::cargo_bin("flpdf-test-driver").expect("flpdf-test-driver binary")
}

fn minimal_pdf() -> &'static str {
    concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/minimal.pdf"
    )
}

#[test]
fn too_few_arguments_print_exact_usage_and_exit_two() {
    driver()
        .assert()
        .code(2)
        .stdout("")
        .stderr("Usage: flpdf-test-driver n filename1 [arg2]\n");
}

#[test]
fn too_many_arguments_print_exact_usage_and_exit_two() {
    driver()
        .args(["1", minimal_pdf(), "arg2", "extra"])
        .assert()
        .code(2)
        .stdout("")
        .stderr("Usage: flpdf-test-driver n filename1 [arg2]\n");
}

#[test]
fn unsupported_test_reads_valid_pdf_then_fails_loud() {
    driver()
        .args(["50", minimal_pdf()])
        .assert()
        .code(2)
        .stdout("")
        .stderr("invalid test 50\n");
}

#[test]
fn malformed_pdf_error_precedes_unsupported_test_lookup() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let malformed = directory.path().join("malformed.pdf");
    fs::write(&malformed, b"not a PDF").expect("write malformed fixture");

    driver()
        .args(["50", malformed.to_str().expect("utf-8 temp path")])
        .assert()
        .code(2)
        .stdout("")
        .stderr("parse error at byte 0: missing PDF header\n")
        .stderr(predicate::str::contains("invalid test").not());
}

#[test]
fn fourth_argument_is_accepted_but_not_used_by_id_one_family() {
    driver()
        .args(["50", minimal_pdf(), "unused"])
        .assert()
        .code(2)
        .stdout("")
        .stderr("invalid test 50\n");
}

#[test]
fn id_zero_is_an_explicit_fail_loud_scope_boundary() {
    driver()
        .args(["0", minimal_pdf()])
        .assert()
        .code(2)
        .stdout("")
        .stderr("invalid test 0\n");
}
