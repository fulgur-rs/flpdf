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
fn verbose_underlay_prints_underlay_kind_string() {
    let dest = fixture("one-page.pdf");
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
            "--underlay",
            &src,
            "--",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(format!(
            "    {} underlay 1\n",
            src
        )));
}

#[test]
fn verbose_overlay_repeated_to_slots_emit_one_line_per_slot() {
    // Regression for the `--to=N,N,...` repeated-slot collapse. Before the
    // PageRange::resolve dedup was removed, `--to=1,1` was reduced to `[1]`
    // and overlay applied only page 1 of the source, so only one
    // `<src> overlay 1\n` line appeared under `page 1`. With qpdf-parity
    // (no dedup), both slots survive and pair with `--from=1-2` in order,
    // yielding overlay lines 1 AND 2 under page 1 — matching the qpdf
    // overlay/underlay 6 (uo-6) shape in miniature.
    let dest = fixture("one-page.pdf");
    let src = fixture("two-page.pdf");
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
            "--to=1,1",
            "--from=1-2",
            "--",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("  page 1\n"))
        .stderr(predicate::str::contains(format!("    {} overlay 1\n", src)))
        .stderr(predicate::str::contains(format!("    {} overlay 2\n", src)));
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
