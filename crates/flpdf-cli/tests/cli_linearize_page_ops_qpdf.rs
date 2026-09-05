//! qpdf 11.9.0 parity for linearized page-operation output.
//!
//! qpdf applies page selection/rotation/transformation before its canonical
//! writer settings, and applies linearization to every split chunk. These
//! cases guard that ordering at the CLI boundary.

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::process::{Command as ShellCommand, Output};

const COMPAT: &str = "../../tests/fixtures/compat";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(COMPAT)
        .join(name)
}

fn skip_if_qpdf_missing() -> bool {
    if ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        return false;
    }
    if std::env::var_os("CI").is_some() {
        panic!("qpdf is required for linearize page-operation parity tests on CI");
    }
    eprintln!("skipping: qpdf is not available");
    true
}

fn run_qpdf(args: &[&str]) -> Output {
    ShellCommand::new("qpdf")
        .args(args)
        .output()
        .expect("qpdf should spawn")
}

fn run_flpdf(args: &[&str]) -> Output {
    Command::cargo_bin("flpdf")
        .expect("flpdf binary should build")
        .args(args)
        .output()
        .expect("flpdf should spawn")
}

fn assert_success(output: &Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_linearized(path: &Path, label: &str) {
    let output = run_qpdf(&["--check-linearization", path.to_str().unwrap()]);
    assert_success(&output, label);
    let report = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        report.contains("no linearization errors"),
        "{label} is not cleanly linearized: {report}"
    );
}

#[test]
fn top_level_pages_linearize_matches_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let input = fixture("three-page.pdf");
    let qpdf_output = temp.path().join("qpdf-pages.pdf");
    let flpdf_output = temp.path().join("flpdf-pages.pdf");
    let input = input.to_str().unwrap();

    let qpdf = run_qpdf(&[
        "--static-id",
        "--stream-data=uncompress",
        "--linearize",
        input,
        "--pages",
        input,
        "1-2",
        "--",
        qpdf_output.to_str().unwrap(),
    ]);
    assert_success(&qpdf, "qpdf --linearize --pages");

    let flpdf = run_flpdf(&[
        "--static-id",
        "--stream-data=uncompress",
        "--linearize",
        input,
        "--pages",
        input,
        "1-2",
        "--",
        flpdf_output.to_str().unwrap(),
    ]);
    assert_success(&flpdf, "flpdf --linearize --pages");
    assert_linearized(&qpdf_output, "qpdf --pages output");
    assert_linearized(&flpdf_output, "flpdf --pages output");
    assert_eq!(
        std::fs::read(&flpdf_output).unwrap(),
        std::fs::read(&qpdf_output).unwrap(),
        "linearized --pages output must match qpdf"
    );
}

#[test]
fn top_level_rotate_linearize_matches_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let input = fixture("three-page.pdf");
    let qpdf_output = temp.path().join("qpdf-rotate.pdf");
    let flpdf_output = temp.path().join("flpdf-rotate.pdf");
    let input = input.to_str().unwrap();

    let qpdf = run_qpdf(&[
        "--static-id",
        "--stream-data=uncompress",
        "--linearize",
        "--rotate=+90",
        input,
        qpdf_output.to_str().unwrap(),
    ]);
    assert_success(&qpdf, "qpdf --linearize --rotate");

    let flpdf = run_flpdf(&[
        "--static-id",
        "--stream-data=uncompress",
        "--linearize",
        "--rotate=+90",
        input,
        flpdf_output.to_str().unwrap(),
    ]);
    assert_success(&flpdf, "flpdf --linearize --rotate");
    assert_linearized(&qpdf_output, "qpdf --rotate output");
    assert_linearized(&flpdf_output, "flpdf --rotate output");
    assert_eq!(
        std::fs::read(&flpdf_output).unwrap(),
        std::fs::read(&qpdf_output).unwrap(),
        "linearized --rotate output must match qpdf"
    );
}

#[test]
fn rewrite_flatten_rotation_linearize_matches_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let input = fixture("one-page-r90.pdf");
    let qpdf_output = temp.path().join("qpdf-flatten.pdf");
    let flpdf_output = temp.path().join("flpdf-flatten.pdf");
    let input = input.to_str().unwrap();

    let qpdf = run_qpdf(&[
        "--static-id",
        "--stream-data=uncompress",
        "--linearize",
        "--flatten-rotation",
        input,
        qpdf_output.to_str().unwrap(),
    ]);
    assert_success(&qpdf, "qpdf --linearize --flatten-rotation");

    let flpdf = run_flpdf(&[
        "rewrite",
        "--static-id",
        "--stream-data=uncompress",
        "--linearize",
        "--flatten-rotation",
        input,
        flpdf_output.to_str().unwrap(),
    ]);
    assert_success(&flpdf, "flpdf rewrite --linearize --flatten-rotation");
    assert_linearized(&qpdf_output, "qpdf --flatten-rotation output");
    assert_linearized(&flpdf_output, "flpdf --flatten-rotation output");
    assert_eq!(
        std::fs::read(&flpdf_output).unwrap(),
        std::fs::read(&qpdf_output).unwrap(),
        "linearized --flatten-rotation output must match qpdf"
    );
}

#[test]
fn top_level_split_pages_linearizes_every_chunk_like_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let qpdf_dir = temp.path().join("qpdf");
    let flpdf_dir = temp.path().join("flpdf");
    std::fs::create_dir(&qpdf_dir).unwrap();
    std::fs::create_dir(&flpdf_dir).unwrap();
    let input = fixture("three-page.pdf");
    let input = input.to_str().unwrap();
    let qpdf_template = qpdf_dir.join("out.pdf");
    let flpdf_template = flpdf_dir.join("out.pdf");

    let qpdf = run_qpdf(&[
        "--static-id",
        "--stream-data=uncompress",
        "--linearize",
        "--split-pages=1",
        input,
        qpdf_template.to_str().unwrap(),
    ]);
    assert_success(&qpdf, "qpdf --linearize --split-pages");

    let flpdf = run_flpdf(&[
        "--static-id",
        "--stream-data=uncompress",
        "--linearize",
        "--split-pages=1",
        input,
        flpdf_template.to_str().unwrap(),
    ]);
    assert_success(&flpdf, "flpdf --linearize --split-pages");

    for page in 1..=3 {
        let qpdf_chunk = qpdf_dir.join(format!("out-{page}.pdf"));
        let flpdf_chunk = flpdf_dir.join(format!("out-{page}.pdf"));
        assert!(qpdf_chunk.exists(), "qpdf chunk {page} must exist");
        assert!(flpdf_chunk.exists(), "flpdf chunk {page} must exist");
        assert_linearized(&qpdf_chunk, &format!("qpdf split chunk {page}"));
        assert_linearized(&flpdf_chunk, &format!("flpdf split chunk {page}"));
        assert_eq!(
            std::fs::read(&flpdf_chunk).unwrap(),
            std::fs::read(&qpdf_chunk).unwrap(),
            "linearized split chunk {page} must match qpdf"
        );
    }
}
