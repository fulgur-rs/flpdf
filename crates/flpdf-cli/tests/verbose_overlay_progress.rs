//! End-to-end CLI test for `--verbose` overlay/underlay progress lines.
//!
//! Verifies the `flpdf: processing underlay/overlay` header and per-page
//! `  page N\n    <file> overlay <src>\n` mapping are emitted to stderr
//! in the exact format qpdf `--verbose` uses (the flpdf-qtest shim
//! normalizes the `flpdf:` prefix to `qpdf:`).

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .to_path_buf()
}

fn fixture(name: &str) -> String {
    fixtures_dir().join(name).to_str().unwrap().to_string()
}

#[test]
fn verbose_overlay_prints_processing_header_and_per_page_mapping() {
    let dest = fixture("two-page.pdf");
    let src = fixture("one-page.pdf");
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = tmp.path().join("out.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args([
            "rewrite",
            "--static-id",
            "--verbose",
            &dest,
            out.to_str().unwrap(),
            "--overlay",
            &src,
            "--",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "flpdf: processing underlay/overlay\n",
        ))
        .stderr(predicate::str::contains("  page 1\n"))
        // The src fixture path is absolute; qpdf/flpdf verbose emits the raw
        // CLI-supplied filename. Assert the raw path + " overlay 1" appears
        // under page 1.
        .stderr(predicate::str::contains(format!("    {} overlay 1\n", src)))
        .stderr(predicate::str::contains("  page 2\n"));
}

#[test]
fn verbose_without_overlay_does_not_print_processing_header() {
    let input = fixture("one-page.pdf");
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = tmp.path().join("out.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args([
            "rewrite",
            "--static-id",
            "--verbose",
            &input,
            out.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("processing underlay/overlay").not());
}
