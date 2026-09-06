//! Differential coverage for qpdf's top-level `--empty` primary input.

use assert_cmd::Command;
use std::fs;
use std::path::Path;
use std::process::{Command as ProcessCommand, Output};

fn qpdf_available() -> bool {
    ProcessCommand::new("qpdf")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn run_qpdf(args: &[&str]) -> Output {
    ProcessCommand::new("qpdf")
        .args(args)
        .output()
        .expect("qpdf is available")
}

fn run_flpdf(args: &[&str]) -> Output {
    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_PROGNAME", "qpdf")
        .args(args)
        .output()
        .expect("flpdf process")
}

fn assert_same_process_result(args: &[&str]) {
    let qpdf = run_qpdf(args);
    let flpdf = run_flpdf(args);
    assert_eq!(
        flpdf.status.code(),
        qpdf.status.code(),
        "status differs for {args:?}: qpdf stderr={:?}, flpdf stderr={:?}",
        String::from_utf8_lossy(&qpdf.stderr),
        String::from_utf8_lossy(&flpdf.stderr),
    );
    assert_eq!(flpdf.stdout, qpdf.stdout, "stdout differs for {args:?}");
    assert_eq!(flpdf.stderr, qpdf.stderr, "stderr differs for {args:?}");
}

#[test]
fn empty_primary_plain_write_matches_qpdf() {
    if !qpdf_available() {
        return;
    }
    let temp = tempfile::tempdir().expect("temporary directory");
    let qpdf_output = temp.path().join("qpdf-empty.pdf");
    let flpdf_output = temp.path().join("flpdf-empty.pdf");

    let qpdf = run_qpdf(&[
        "--empty",
        "--static-id",
        qpdf_output.to_str().expect("UTF-8 path"),
    ]);
    let flpdf = run_flpdf(&[
        "--empty",
        "--static-id",
        flpdf_output.to_str().expect("UTF-8 path"),
    ]);

    assert!(qpdf.status.success(), "qpdf stderr: {:?}", qpdf.stderr);
    assert!(flpdf.status.success(), "flpdf stderr: {:?}", flpdf.stderr);
    assert_eq!(
        fs::read(&flpdf_output).expect("flpdf output"),
        fs::read(&qpdf_output).expect("qpdf output"),
        "standalone --empty output must be byte-identical",
    );
}

#[test]
fn empty_primary_check_matches_qpdf() {
    if !qpdf_available() {
        return;
    }
    assert_same_process_result(&["--empty", "--check"]);
}

#[test]
fn empty_primary_json_output_file_matches_qpdf() {
    if !qpdf_available() {
        return;
    }
    let temp = tempfile::tempdir().expect("temporary directory");
    let qpdf_output = temp.path().join("qpdf-empty.json");
    let flpdf_output = temp.path().join("flpdf-empty.json");

    let qpdf = run_qpdf(&[
        "--empty",
        "--json",
        qpdf_output.to_str().expect("UTF-8 path"),
    ]);
    let flpdf = run_flpdf(&[
        "--empty",
        "--json",
        flpdf_output.to_str().expect("UTF-8 path"),
    ]);

    assert!(qpdf.status.success(), "qpdf stderr: {:?}", qpdf.stderr);
    assert!(flpdf.status.success(), "flpdf stderr: {:?}", flpdf.stderr);
    assert_eq!(
        fs::read(&flpdf_output).expect("flpdf JSON output"),
        fs::read(&qpdf_output).expect("qpdf JSON output"),
    );
}

#[test]
fn empty_primary_update_from_json_check_matches_qpdf() {
    if !qpdf_available() {
        return;
    }
    let temp = tempfile::tempdir().expect("temporary directory");
    let update = temp.path().join("update.json");
    fs::write(
        &update,
        r#"{"qpdf":[{"jsonversion":2},{"obj:1 0 R":{"value":{"/Type":"/Catalog","/Pages":"2 0 R","/Custom":"/UPDATED"}}}]}"#,
    )
    .expect("write JSON update");
    let update_arg = format!("--update-from-json={}", update.display());
    assert_same_process_result(&["--empty", update_arg.as_str(), "--check"]);
}

#[test]
fn empty_primary_inspections_match_qpdf() {
    if !qpdf_available() {
        return;
    }
    for args in [
        ["--empty", "--show-npages"],
        ["--empty", "--show-pages"],
        ["--empty", "--show-xref"],
        ["--empty", "--show-linearization"],
        ["--empty", "--check-linearization"],
        ["--empty", "--show-encryption"],
        ["--empty", "--show-object=trailer"],
        ["--empty", "--list-attachments"],
        ["--empty", "--json"],
    ] {
        assert_same_process_result(&args);
    }
}

#[test]
fn empty_primary_inspection_rejects_an_output_like_qpdf() {
    if !qpdf_available() {
        return;
    }
    let output = Path::new("/tmp/flpdf-buca-inspection-output.pdf");
    assert_same_process_result(&["--empty", "--check", output.to_str().expect("UTF-8 path")]);
}
