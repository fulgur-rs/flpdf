//! Overlay/underlay page-tree repair parity against qpdf 11.9.0.
//!
//! qpdf obtains all source and destination pages before overlay placement. That
//! repair pass supplies the Letter MediaBox for a boxless leaf and emits the
//! corresponding warning. These tests lock both the destination and source
//! sides of that contract before the overlay consumer is switched to the
//! canonical `PageDocumentHelper` route.

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::process::Command as Shell;

const EXPECTED_QPDF_VERSION: &str = "11.9.0";

fn qpdf_command() -> String {
    std::env::var("QPDF").unwrap_or_else(|_| "qpdf".to_owned())
}

fn qpdf_available() -> bool {
    match Shell::new(qpdf_command()).arg("--version").output() {
        Ok(output) => {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .map(str::trim)
                == Some(&format!("qpdf version {EXPECTED_QPDF_VERSION}"))
        }
        Err(_) => false,
    }
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(name)
}

fn warning_lines(stderr: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(stderr)
        .lines()
        .filter(|line| line.starts_with("WARNING: "))
        .map(str::to_owned)
        .collect()
}

fn qdf_text(path: &Path) -> String {
    let output = Shell::new(qpdf_command())
        .args([
            "--qdf",
            "--object-streams=disable",
            path.to_str().expect("fixture path is UTF-8"),
            "-",
        ])
        .output()
        .expect("qpdf --qdf should spawn");
    assert!(
        output.status.success(),
        "qpdf --qdf failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn letter_bbox_count(qdf: &str) -> usize {
    qdf.matches("/BBox [\n    0\n    0\n    612\n    792")
        .count()
}

#[test]
fn overlay_repairs_boxless_destination_like_qpdf() {
    if !qpdf_available() {
        eprintln!("[SKIP] qpdf {EXPECTED_QPDF_VERSION} not on PATH");
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let destination = fixture("missing-mediabox-leaf.pdf");
    let source = fixture("one-page.pdf");
    let qpdf_output_path = tmp.path().join("qpdf.pdf");
    let flpdf_output_path = tmp.path().join("flpdf.pdf");

    let qpdf = Shell::new(qpdf_command())
        .args([
            destination.to_str().unwrap(),
            "--overlay",
            source.to_str().unwrap(),
            "--",
            qpdf_output_path.to_str().unwrap(),
        ])
        .output()
        .expect("qpdf overlay should spawn");
    assert_eq!(qpdf.status.code(), Some(3), "qpdf stderr: {:?}", qpdf);

    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args([
            "rewrite",
            destination.to_str().unwrap(),
            "--overlay",
            source.to_str().unwrap(),
            "--",
            flpdf_output_path.to_str().unwrap(),
        ])
        .output()
        .expect("flpdf overlay should spawn");
    assert_eq!(
        flpdf.status.code(),
        Some(3),
        "flpdf stderr: {}",
        String::from_utf8_lossy(&flpdf.stderr)
    );
    assert_eq!(
        warning_lines(&flpdf.stderr),
        warning_lines(&qpdf.stderr),
        "destination repair warnings must match qpdf"
    );
    assert!(String::from_utf8_lossy(&flpdf.stderr)
        .contains("MediaBox is undefined; setting to letter / ANSI A"));
    assert!(
        !String::from_utf8_lossy(&flpdf.stderr).contains("no usable placement box"),
        "the repaired destination must reach overlay placement"
    );
    assert!(flpdf_output_path.is_file());
}

#[test]
fn overlay_repairs_boxless_source_before_form_conversion_like_qpdf() {
    if !qpdf_available() {
        eprintln!("[SKIP] qpdf {EXPECTED_QPDF_VERSION} not on PATH");
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let destination = fixture("one-page.pdf");
    let source = fixture("missing-mediabox-leaf.pdf");
    let qpdf_output_path = tmp.path().join("qpdf.pdf");
    let flpdf_output_path = tmp.path().join("flpdf.pdf");

    let qpdf = Shell::new(qpdf_command())
        .args([
            destination.to_str().unwrap(),
            "--overlay",
            source.to_str().unwrap(),
            "--",
            qpdf_output_path.to_str().unwrap(),
        ])
        .output()
        .expect("qpdf overlay should spawn");
    assert!(qpdf.status.success(), "qpdf stderr: {:?}", qpdf);

    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args([
            "rewrite",
            destination.to_str().unwrap(),
            "--overlay",
            source.to_str().unwrap(),
            "--",
            flpdf_output_path.to_str().unwrap(),
        ])
        .output()
        .expect("flpdf overlay should spawn");
    assert!(
        flpdf.status.success(),
        "flpdf stderr: {}",
        String::from_utf8_lossy(&flpdf.stderr)
    );
    assert_eq!(
        warning_lines(&flpdf.stderr),
        warning_lines(&qpdf.stderr),
        "source repair warnings must match qpdf"
    );
    assert!(
        !String::from_utf8_lossy(&flpdf.stderr).contains("bounding box is invalid"),
        "the repaired source must not reach Form conversion with an invalid BBox"
    );

    let qpdf_qdf = qdf_text(&qpdf_output_path);
    let flpdf_qdf = qdf_text(&flpdf_output_path);
    assert_eq!(
        letter_bbox_count(&flpdf_qdf),
        letter_bbox_count(&qpdf_qdf),
        "source and destination Form XObjects must receive qpdf's Letter BBox"
    );
    assert!(letter_bbox_count(&qpdf_qdf) >= 2);
}

#[test]
fn overlay_qdf_original_object_ids_match_qpdf() {
    if !qpdf_available() {
        eprintln!("[SKIP] qpdf {EXPECTED_QPDF_VERSION} not on PATH");
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let destination = fixture("two-page.pdf");
    let source = fixture("fxo-red.pdf");
    let qpdf_output = temp.path().join("qpdf-qdf.pdf");
    let flpdf_output = temp.path().join("flpdf-qdf.pdf");

    let qpdf = Shell::new(qpdf_command())
        .args([
            destination.to_str().unwrap(),
            "--overlay",
            source.to_str().unwrap(),
            "--",
            "--static-id",
            "--qdf",
            qpdf_output.to_str().unwrap(),
        ])
        .output()
        .expect("qpdf overlay should spawn");
    assert!(qpdf.status.success(), "qpdf stderr: {:?}", qpdf);

    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args([
            "rewrite",
            "--static-id",
            "--qdf",
            destination.to_str().unwrap(),
            "--overlay",
            source.to_str().unwrap(),
            "--",
            flpdf_output.to_str().unwrap(),
        ])
        .output()
        .expect("flpdf overlay should spawn");
    assert!(
        flpdf.status.success(),
        "flpdf stderr: {}",
        String::from_utf8_lossy(&flpdf.stderr)
    );
    assert_eq!(
        std::fs::read(&flpdf_output).expect("flpdf qdf output"),
        std::fs::read(&qpdf_output).expect("qpdf qdf output"),
        "qdf Original object ID comments and allocation order must match qpdf"
    );
}
