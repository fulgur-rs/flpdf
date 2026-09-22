//! qpdf 11.9.0 object-cache parity after multi-source `--pages` selection.

use assert_cmd::Command;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command as ShellCommand, Output};

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

fn qpdf_available() -> bool {
    ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .is_some_and(|line| line.trim() == EXPECTED_QPDF_VERSION)
        })
        .unwrap_or(false)
}

fn require_qpdf() -> bool {
    if qpdf_available() {
        return true;
    }
    if std::env::var_os("CI").is_some() {
        panic!("qpdf 11.9.0 is required for multi-source JSON parity tests");
    }
    eprintln!("skipping: qpdf 11.9.0 is not available");
    false
}

fn assert_success(label: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{label} failed ({:?}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_qpdf_document_object_cache_matches_flpdf(qpdf_output: &Path, flpdf_output: &Path) {
    let qpdf_json: Value =
        serde_json::from_slice(&std::fs::read(qpdf_output).expect("read qpdf JSON output"))
            .expect("parse qpdf JSON output");
    let flpdf_json: Value =
        serde_json::from_slice(&std::fs::read(flpdf_output).expect("read flpdf JSON output"))
            .expect("parse flpdf JSON output");

    assert_eq!(
        flpdf_json["qpdf"][0], qpdf_json["qpdf"][0],
        "the qpdf document metadata, including maxobjectid, must match"
    );
    assert_eq!(
        flpdf_json["qpdf"][1], qpdf_json["qpdf"][1],
        "the complete qpdf object map and trailer must match"
    );
}

#[test]
fn multi_source_page_json_retains_the_primary_object_cache() {
    if !require_qpdf() {
        return;
    }

    let primary = fixture("compat/one-page.pdf");
    let secondary = fixture("compat/multi-contents-one-page.pdf");
    let directory = tempfile::tempdir().expect("temporary output directory");
    let qpdf_output = directory.path().join("qpdf.json");
    let flpdf_output = directory.path().join("flpdf.json");

    let qpdf = ShellCommand::new("qpdf")
        .args(["--json=2", "--pages"])
        .arg(&primary)
        .arg("1")
        .arg(&secondary)
        .arg("1")
        .arg("--")
        .arg(&primary)
        .arg(&qpdf_output)
        .output()
        .expect("qpdf should spawn");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary should build")
        .args(["--json=2", "--pages"])
        .arg(&primary)
        .arg("1")
        .arg(&secondary)
        .arg("1")
        .arg("--")
        .arg(&primary)
        .arg(&flpdf_output)
        .output()
        .expect("flpdf should spawn");

    assert_success("qpdf multi-source JSON", &qpdf);
    assert_success("flpdf multi-source JSON", &flpdf);
    assert_qpdf_document_object_cache_matches_flpdf(&qpdf_output, &flpdf_output);
}

#[test]
fn empty_primary_multi_source_page_json_matches_qpdf() {
    if !require_qpdf() {
        return;
    }

    let primary = fixture("compat/one-page.pdf");
    let secondary = fixture("compat/multi-contents-one-page.pdf");
    let directory = tempfile::tempdir().expect("temporary output directory");
    let qpdf_output = directory.path().join("qpdf-empty.json");
    let flpdf_output = directory.path().join("flpdf-empty.json");

    let qpdf = ShellCommand::new("qpdf")
        .args(["--empty", "--json=2", "--pages"])
        .arg(&primary)
        .arg("1")
        .arg(&secondary)
        .arg("1")
        .arg("--")
        .arg(&qpdf_output)
        .output()
        .expect("qpdf should spawn");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary should build")
        .args(["--empty", "--json=2", "--pages"])
        .arg(&primary)
        .arg("1")
        .arg(&secondary)
        .arg("1")
        .arg("--")
        .arg(&flpdf_output)
        .output()
        .expect("flpdf should spawn");

    assert_success("qpdf empty-primary multi-source JSON", &qpdf);
    assert_success("flpdf empty-primary multi-source JSON", &flpdf);
    assert_qpdf_document_object_cache_matches_flpdf(&qpdf_output, &flpdf_output);
}

#[cfg(feature = "qpdf-zlib-compat")]
#[test]
fn multi_source_pdf_write_remains_byte_identical_to_qpdf() {
    if !require_qpdf() {
        return;
    }

    let primary = fixture("compat/one-page.pdf");
    let secondary = fixture("compat/multi-contents-one-page.pdf");
    let directory = tempfile::tempdir().expect("temporary output directory");
    let qpdf_output = directory.path().join("qpdf.pdf");
    let flpdf_output = directory.path().join("flpdf.pdf");

    let qpdf = ShellCommand::new("qpdf")
        .args(["--deterministic-id", "--pages"])
        .arg(&primary)
        .arg("1")
        .arg(&secondary)
        .arg("1")
        .arg("--")
        .arg(&primary)
        .arg(&qpdf_output)
        .output()
        .expect("qpdf should spawn");
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary should build")
        .args(["--deterministic-id", "--pages"])
        .arg(&primary)
        .arg("1")
        .arg(&secondary)
        .arg("1")
        .arg("--")
        .arg(&primary)
        .arg(&flpdf_output)
        .output()
        .expect("flpdf should spawn");

    assert_success("qpdf multi-source PDF write", &qpdf);
    assert_success("flpdf multi-source PDF write", &flpdf);
    assert_eq!(
        std::fs::read(&flpdf_output).expect("read flpdf PDF output"),
        std::fs::read(&qpdf_output).expect("read qpdf PDF output"),
        "the page-selection cache fix must retain qpdf byte parity for PDF writes"
    );
}
