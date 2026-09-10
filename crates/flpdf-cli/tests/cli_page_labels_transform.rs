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
fn pages_remove_page_labels_runs_after_page_selection() {
    if !qpdf_available() {
        eprintln!("[SKIP cli_page_labels_transform] qpdf 11.9.0 is unavailable");
        return;
    }
    let tempdir = tempfile::tempdir().expect("temporary directory");
    let input = fixture("compat/one-page.pdf");
    let labeled = tempdir.path().join("labeled.pdf");
    let qpdf_output = tempdir.path().join("qpdf-pages.pdf");
    let flpdf_output = tempdir.path().join("flpdf-pages.pdf");

    let qpdf = run_qpdf(&[
        &input,
        &labeled,
        Path::new("--set-page-labels"),
        Path::new("1:a"),
        Path::new("--"),
    ]);
    assert!(qpdf.status.success());

    let qpdf = run_qpdf(&[
        &labeled,
        Path::new("--remove-page-labels"),
        Path::new("--pages"),
        Path::new("."),
        Path::new("1"),
        Path::new("--"),
        &qpdf_output,
    ]);
    assert!(
        qpdf.status.success(),
        "qpdf page selection with label removal failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .arg(&labeled)
        .arg("--remove-page-labels")
        .args(["--pages", ".", "1", "--"])
        .arg(&flpdf_output)
        .output()
        .expect("run flpdf");
    assert_eq!(
        flpdf.status.code(),
        Some(0),
        "flpdf page selection with label removal failed: {}",
        String::from_utf8_lossy(&flpdf.stderr)
    );

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

#[test]
fn page_label_option_table_diagnostics_match_qpdf() {
    if !qpdf_available() {
        eprintln!("[SKIP cli_page_labels_transform] qpdf 11.9.0 is unavailable");
        return;
    }

    let tempdir = tempfile::tempdir().expect("temporary directory");
    let input = fixture("compat/one-page.pdf");

    let qpdf_unterminated = run_qpdf(&[
        Path::new("--set-page-labels"),
        Path::new("1:D"),
        &input,
        &tempdir.path().join("qpdf-unterm.pdf"),
    ]);
    let flpdf_unterminated = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["--set-page-labels", "1:D"])
        .arg(&input)
        .arg(tempdir.path().join("flpdf-unterm.pdf"))
        .output()
        .expect("run flpdf");
    assert_eq!(qpdf_unterminated.status.code(), Some(2));
    assert_eq!(flpdf_unterminated.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&qpdf_unterminated.stderr)
        .contains("missing -- at end of set page labels options"));
    assert!(
        String::from_utf8_lossy(&flpdf_unterminated.stderr)
            .contains("missing -- at end of set page labels options"),
        "flpdf stderr: {}",
        String::from_utf8_lossy(&flpdf_unterminated.stderr)
    );

    let qpdf_unknown = run_qpdf(&[
        Path::new("--bogus"),
        Path::new("--set-page-labels"),
        Path::new("1:D"),
        &input,
        &tempdir.path().join("qpdf-unknown.pdf"),
    ]);
    let flpdf_unknown = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["--bogus", "--set-page-labels", "1:D"])
        .arg(&input)
        .arg(tempdir.path().join("flpdf-unknown.pdf"))
        .output()
        .expect("run flpdf");
    assert_eq!(qpdf_unknown.status.code(), Some(2));
    assert_eq!(flpdf_unknown.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&qpdf_unknown.stderr).contains("unrecognized argument --bogus"));
    assert!(
        String::from_utf8_lossy(&flpdf_unknown.stderr).contains("unrecognized argument --bogus"),
        "flpdf stderr: {}",
        String::from_utf8_lossy(&flpdf_unknown.stderr)
    );

    for (option, stem) in [("--verbose", "verbose"), ("--remove-page-labels", "remove")] {
        let qpdf_output = tempdir.path().join(format!("qpdf-{stem}.pdf"));
        let flpdf_output = tempdir.path().join(format!("flpdf-{stem}.pdf"));
        let qpdf = run_qpdf(&[
            Path::new("--set-page-labels"),
            Path::new("1:D"),
            Path::new(option),
            Path::new("--"),
            &input,
            &qpdf_output,
        ]);
        let flpdf = Command::cargo_bin("flpdf")
            .expect("flpdf binary")
            .args(["--set-page-labels", "1:D", option, "--"])
            .arg(&input)
            .arg(&flpdf_output)
            .output()
            .expect("run flpdf");
        let expected = format!(
            "unrecognized argument {option} (set page labels options must be terminated with --)"
        );
        assert_eq!(
            qpdf.status.code(),
            Some(2),
            "qpdf stderr: {:?}",
            qpdf.stderr
        );
        assert_eq!(
            flpdf.status.code(),
            Some(2),
            "flpdf stderr: {:?}",
            flpdf.stderr
        );
        assert!(
            String::from_utf8_lossy(&qpdf.stderr).contains(&expected),
            "qpdf stderr: {}",
            String::from_utf8_lossy(&qpdf.stderr)
        );
        assert!(
            String::from_utf8_lossy(&flpdf.stderr).contains(&expected),
            "flpdf stderr: {}",
            String::from_utf8_lossy(&flpdf.stderr)
        );
    }
}

#[test]
fn page_label_spec_validation_precedes_extra_positional_like_qpdf() {
    if !qpdf_available() {
        eprintln!("[SKIP cli_page_labels_transform] qpdf 11.9.0 is unavailable");
        return;
    }

    let tempdir = tempfile::tempdir().expect("temporary directory");
    let input = fixture("compat/one-page.pdf");
    let qpdf_output = tempdir.path().join("qpdf.pdf");
    let flpdf_output = tempdir.path().join("flpdf.pdf");

    let qpdf = run_qpdf(&[
        Path::new("--set-page-labels"),
        Path::new("quack"),
        Path::new("--"),
        &input,
        &qpdf_output,
        Path::new("extra.pdf"),
    ]);
    let flpdf = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["--set-page-labels", "quack", "--"])
        .arg(&input)
        .arg(&flpdf_output)
        .arg("extra.pdf")
        .output()
        .expect("run flpdf");

    assert_eq!(qpdf.status.code(), Some(2));
    assert_eq!(flpdf.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&qpdf.stderr).contains("page label spec must be"),
        "qpdf stderr: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );
    assert!(
        String::from_utf8_lossy(&flpdf.stderr).contains("page label spec must be"),
        "flpdf stderr: {}",
        String::from_utf8_lossy(&flpdf.stderr)
    );
}
