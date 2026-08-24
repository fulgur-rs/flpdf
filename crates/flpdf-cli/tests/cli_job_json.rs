use assert_cmd::Command;
use std::fs;
use std::path::PathBuf;

#[test]
fn job_json_file_runs_through_the_production_qpdf_job() {
    let directory = tempfile::tempdir().unwrap();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    fs::copy(fixture, directory.path().join("minimal.pdf")).unwrap();
    fs::write(
        directory.path().join("job.json"),
        br#"{"inputFile":"minimal.pdf","outputFile":"output.pdf","staticId":""}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=job.json")
        .assert()
        .code(0)
        .stdout("");

    assert!(directory.path().join("output.pdf").is_file());
}

#[test]
fn job_json_file_usage_errors_use_the_qpdf_job_file_boundary() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("bad.json"),
        br#"{"objectStreams":"potato"}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=bad.json")
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "error with job-json file bad.json",
        ));
}

#[test]
fn job_json_file_preserves_qpdf_warning_status() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/test_driver/repairable_input.pdf");
    fs::copy(fixture, directory.path().join("repairable.pdf")).unwrap();
    fs::write(
        directory.path().join("warning.json"),
        br#"{"inputFile":"repairable.pdf","outputFile":"output.pdf","staticId":""}"#,
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=warning.json")
        .assert()
        .code(3)
        .stderr(predicates::str::contains(
            "operation succeeded with warnings",
        ));

    assert!(directory.path().join("output.pdf").is_file());
}

#[test]
fn job_json_file_rejects_same_input_and_output_without_truncating_input() {
    let directory = tempfile::tempdir().unwrap();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let input = directory.path().join("input.pdf");
    let output = directory.path().join("output.pdf");
    fs::copy(fixture, &input).unwrap();
    fs::hard_link(&input, &output).unwrap();
    let before = fs::read(&input).unwrap();
    fs::write(
        directory.path().join("same.json"),
        serde_json::json!({
            "inputFile": input,
            "outputFile": output,
        })
        .to_string(),
    )
    .unwrap();

    Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=same.json")
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "input file and output file are the same",
        ));

    assert_eq!(fs::read(&input).unwrap(), before);
}

#[test]
fn job_json_file_dash_output_is_written_to_stdout() {
    let directory = tempfile::tempdir().unwrap();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    fs::copy(fixture, directory.path().join("input.pdf")).unwrap();
    fs::write(
        directory.path().join("stdout.json"),
        br#"{"inputFile":"input.pdf","outputFile":"-"}"#,
    )
    .unwrap();

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .current_dir(directory.path())
        .arg("--job-json-file=stdout.json")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.starts_with(b"%PDF-"));
    assert!(!directory.path().join("-").exists());
}
