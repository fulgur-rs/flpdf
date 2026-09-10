use assert_cmd::Command;
use predicates::prelude::*;

#[path = "support/eol.rs"]
mod eol;
use eol::EOL;

#[test]
fn pages_zero_reports_qpdf_numeric_range_error_instead_of_opening_a_file() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "../../tests/fixtures/compat/three-page.pdf",
            output.to_str().unwrap(),
            "--pages",
            ".",
            "0",
            "--",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "error at * in numeric range *0: number 0 out of range",
        ))
        .stderr(predicate::str::contains("parsing numeric range for"))
        .stderr(predicate::str::contains("For help").not())
        .stderr(predicate::str::contains("unsupported PDF feature").not())
        .stderr(predicate::str::contains("No such file").not());
}

#[test]
fn pages_invalid_range_reports_qpdf_syntax_error_instead_of_opening_a_file() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "../../tests/fixtures/compat/three-page.pdf",
            output.to_str().unwrap(),
            "--pages",
            ".",
            "1-3:odd:even",
            "--",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "error at * in numeric range 1-3*:odd:even: expected :even or :odd",
        ))
        .stderr(predicate::str::contains("No such file").not());
}

#[test]
fn pages_explicit_range_reports_qpdf_source_framing() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "../../tests/fixtures/compat/three-page.pdf",
            output.to_str().unwrap(),
            "--pages",
            "--file=.",
            "--range=abc",
            "--",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("parsing numeric range for"))
        .stderr(predicate::str::contains(
            "error at * in numeric range *abc: invalid range syntax",
        ))
        .stderr(predicate::str::contains("unsupported PDF feature").not())
        .stderr(predicate::str::contains("For help").not());
}

#[test]
fn pages_named_range_then_positional_range_matches_qpdf_duplicate_error() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "../../tests/fixtures/compat/three-page.pdf",
            output.to_str().unwrap(),
            "--pages",
            ".",
            "--range=1",
            "2",
            "--",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "--range already specified for this file",
        ))
        .stderr(predicate::str::contains("No such file").not());
}

#[test]
fn pages_named_file_keeps_the_next_positional_token_as_a_file() {
    let temp = tempfile::tempdir().unwrap();
    let input = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/three-page.pdf");
    std::fs::copy(&input, temp.path().join("1")).unwrap();
    let output = temp.path().join("out.pdf");
    let named_file = format!("--file={}", input.display());

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(temp.path())
        .args([
            "rewrite",
            input.to_str().unwrap(),
            output.to_str().unwrap(),
            "--pages",
            &named_file,
            "1",
            "--",
        ])
        .assert()
        .success();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(temp.path())
        .args(["--show-npages", output.to_str().unwrap()])
        .assert()
        .success()
        .stdout(format!("6{EOL}"));
}

#[test]
fn pages_positional_file_before_named_file_keeps_named_file_range_heuristic() {
    let temp = tempfile::tempdir().unwrap();
    let input = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/three-page.pdf");
    let named_file = temp.path().join("named.pdf");
    std::fs::copy(&input, &named_file).unwrap();
    let output = temp.path().join("out.pdf");
    let named_arg = format!("--file={}", named_file.display());

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(temp.path())
        .args([
            "rewrite",
            input.to_str().unwrap(),
            output.to_str().unwrap(),
            "--pages",
            input.to_str().unwrap(),
            &named_arg,
            "1",
            "--",
        ])
        .assert()
        .success();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(temp.path())
        .args(["--show-npages", output.to_str().unwrap()])
        .assert()
        .success()
        .stdout(format!("4{EOL}"));
}
