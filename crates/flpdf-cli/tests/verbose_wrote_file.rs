//! End-to-end CLI test for `--verbose` "wrote file" completion line.
//!
//! qpdf `--verbose` prints `qpdf: wrote file <output-path>` after a successful
//! rewrite. flpdf-cli emits `flpdf: wrote file <path>` through logger info; the
//! flpdf-qtest shim normalizes the prefix. This completes the verbose-output
//! parity check for qpdf's uo-1..uo-5 and uo-7 goldens.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;

#[path = "support/eol.rs"]
mod eol;
use eol::EOL;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .to_path_buf()
}

fn fixture(name: &str) -> String {
    fixtures_dir().join(name).to_str().unwrap().to_string()
}

#[test]
fn verbose_prints_wrote_file_line() {
    let input = fixture("one-page.pdf");
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = tmp.path().join("out.pdf");
    let out_path = out.to_str().unwrap().to_string();
    Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(["rewrite", "--static-id", "--verbose", &input, &out_path])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "flpdf: wrote file {}{EOL}",
            out_path
        )))
        .stderr(predicate::str::is_empty());
}

#[test]
fn verbose_prints_wrote_file_line_after_linearized_rewrite() {
    // rewrite --linearize takes a separate write path (write_linearized +
    // std::fs::write) that would otherwise skip the wrote-file completion
    // line; regression-guard the branch keeps parity with qpdf --verbose.
    let input = fixture("one-page.pdf");
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = tmp.path().join("out.pdf");
    let out_path = out.to_str().unwrap().to_string();
    Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args([
            "rewrite",
            "--static-id",
            "--linearize",
            "--verbose",
            &input,
            &out_path,
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "flpdf: wrote file {}{EOL}",
            out_path
        )))
        .stderr(predicate::str::is_empty());
}

#[test]
fn no_verbose_does_not_print_wrote_file() {
    let input = fixture("one-page.pdf");
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = tmp.path().join("out.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(["rewrite", "--static-id", &input, out.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("wrote file").not())
        .stderr(predicate::str::contains("wrote file").not());
}
