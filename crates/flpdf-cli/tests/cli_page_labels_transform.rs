//! qpdf 11.9.0 ordinary-rewrite page-label transformation parity.

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Output};

fn qpdf_available() -> bool {
    ProcessCommand::new("qpdf")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

fn run_qpdf(args: &[&Path]) -> Output {
    ProcessCommand::new("qpdf")
        .args(args)
        .output()
        .expect("qpdf 11.9.0 is available")
}

fn show_catalog(path: &Path) -> String {
    let output = ProcessCommand::new("qpdf")
        .arg("--show-object=1")
        .arg(path)
        .output()
        .expect("show Catalog with qpdf 11.9.0");
    assert!(
        output.status.success(),
        "qpdf --show-object=1 failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("qpdf Catalog output is UTF-8")
}

#[test]
fn top_level_set_page_labels_matches_qpdf_raw_catalog_shape() {
    if !qpdf_available() {
        eprintln!("[SKIP cli_page_labels_transform] qpdf 11.9.0 is unavailable");
        return;
    }
    let tempdir = tempfile::tempdir().expect("temporary directory");
    let input = fixture("compat/one-page.pdf");
    let qpdf_output = tempdir.path().join("qpdf.pdf");
    let flpdf_output = tempdir.path().join("flpdf.pdf");
    let qpdf = run_qpdf(&[
        &input,
        &qpdf_output,
        Path::new("--set-page-labels"),
        Path::new("1:a"),
        Path::new("--"),
    ]);
    assert!(
        qpdf.status.success(),
        "qpdf set page labels failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .arg(&input)
        .arg(&flpdf_output)
        .args(["--set-page-labels", "1:a", "--"])
        .assert()
        .success();

    let qpdf_catalog = show_catalog(&qpdf_output);
    let flpdf_catalog = show_catalog(&flpdf_output);
    let expected = "/PageLabels << /Nums [ 0 << /S /a >> ] >>";
    assert!(
        qpdf_catalog.contains(expected),
        "qpdf Catalog: {qpdf_catalog}"
    );
    assert!(
        flpdf_catalog.contains(expected),
        "flpdf Catalog: {flpdf_catalog}"
    );
}

#[test]
fn top_level_remove_page_labels_removes_catalog_key() {
    if !qpdf_available() {
        eprintln!("[SKIP cli_page_labels_transform] qpdf 11.9.0 is unavailable");
        return;
    }
    let tempdir = tempfile::tempdir().expect("temporary directory");
    let input = fixture("compat/one-page.pdf");
    let labeled = tempdir.path().join("labeled.pdf");
    let qpdf_output = tempdir.path().join("qpdf.pdf");
    let flpdf_output = tempdir.path().join("flpdf.pdf");
    let qpdf = run_qpdf(&[
        &input,
        &labeled,
        Path::new("--set-page-labels"),
        Path::new("1:a"),
        Path::new("--"),
    ]);
    assert!(qpdf.status.success());

    let qpdf = run_qpdf(&[&labeled, &qpdf_output, Path::new("--remove-page-labels")]);
    assert!(qpdf.status.success());
    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .arg(&labeled)
        .arg(&flpdf_output)
        .arg("--remove-page-labels")
        .assert()
        .success();

    assert!(!show_catalog(&qpdf_output).contains("/PageLabels"));
    assert!(!show_catalog(&flpdf_output).contains("/PageLabels"));
}

#[test]
fn native_rewrite_uses_the_same_canonical_page_label_consumer() {
    if !qpdf_available() {
        eprintln!("[SKIP cli_page_labels_transform] qpdf 11.9.0 is unavailable");
        return;
    }
    let tempdir = tempfile::tempdir().expect("temporary directory");
    let input = fixture("compat/one-page.pdf");
    let output = tempdir.path().join("rewrite.pdf");
    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["rewrite"])
        .arg(&input)
        .arg(&output)
        .args(["--set-page-labels", "1:a", "--"])
        .assert()
        .success();
    assert!(show_catalog(&output).contains("/PageLabels << /Nums [ 0 << /S /a >> ] >>"));
}

#[test]
fn top_level_set_page_labels_rejects_invalid_spec_like_qpdf() {
    if !qpdf_available() {
        eprintln!("[SKIP cli_page_labels_transform] qpdf 11.9.0 is unavailable");
        return;
    }
    let tempdir = tempfile::tempdir().expect("temporary directory");
    let input = fixture("compat/one-page.pdf");
    let qpdf_output = tempdir.path().join("qpdf.pdf");
    let qpdf = run_qpdf(&[
        Path::new("--set-page-labels"),
        Path::new("quack"),
        Path::new("--"),
        &input,
        &qpdf_output,
    ]);
    assert_eq!(qpdf.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&qpdf.stderr).contains("page label spec must be"));

    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .arg("--set-page-labels")
        .arg("quack")
        .arg("--")
        .arg(&input)
        .arg(tempdir.path().join("flpdf.pdf"))
        .output()
        .expect("run flpdf");
    assert_eq!(flpdf.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&flpdf.stderr).contains("page label spec must be"),
        "flpdf stderr: {}",
        String::from_utf8_lossy(&flpdf.stderr)
    );
}
