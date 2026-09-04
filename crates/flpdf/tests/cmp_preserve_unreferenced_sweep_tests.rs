//! Single-source `--pages` preserve-unreferenced parity against qpdf 11.9.0.
//!
//! The page-selection mutation must leave unselected source objects available
//! to the writer. qpdf's writer then preserves them when
//! `preserveUnreferenced` is enabled, even when page-local resource pruning is
//! requested. This is the regression test for removing flpdf's pre-write
//! document-wide sweep.

#![cfg(feature = "qpdf-zlib-compat")]

use flpdf::job::{JobExitCode, QPDFJob};
use std::path::{Path, PathBuf};
use std::process::Command;

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(name)
}

fn qpdf_available() -> bool {
    Command::new("qpdf")
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

#[test]
fn single_source_pages_preserve_unreferenced_objects_with_resource_pruning() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("qpdf 11.9.0 is required for this parity test on CI");
        }
        eprintln!("skipping: qpdf 11.9.0 is not available");
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let input = fixture("d27-two-page-distinct-resources.pdf");
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");

    let qpdf_result = Command::new("qpdf")
        .args([
            "--static-id",
            "--preserve-unreferenced",
            "--remove-unreferenced-resources=yes",
            "--pages",
            ".",
            "1",
            "--",
        ])
        .arg(&input)
        .arg(&qpdf_output)
        .output()
        .expect("qpdf should spawn");
    assert!(
        qpdf_result.status.success(),
        "qpdf single-source --pages failed: {}",
        String::from_utf8_lossy(&qpdf_result.stderr)
    );

    let json = serde_json::json!({
        "inputFile": input,
        "outputFile": flpdf_output,
        "staticId": "",
        "preserveUnreferenced": "",
        "removeUnreferencedResources": "yes",
        "pages": [{"file": ".", "range": "1"}]
    })
    .to_string();
    let mut job = QPDFJob::new();
    job.initialize_from_json(&json)
        .expect("initialize the qpdf-shaped page job");
    assert_eq!(
        job.run().expect("run the qpdf-shaped page job"),
        JobExitCode::Success
    );

    let qpdf_bytes = std::fs::read(&qpdf_output).expect("qpdf output should be readable");
    let flpdf_bytes = std::fs::read(&flpdf_output).expect("flpdf output should be readable");
    assert_eq!(
        flpdf_bytes, qpdf_bytes,
        "single-source --pages preserve-unreferenced output must be byte-identical"
    );
}
