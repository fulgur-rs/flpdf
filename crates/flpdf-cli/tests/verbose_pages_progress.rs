//! End-to-end CLI tests for `--verbose --pages` progress lines.
//!
//! qpdf emits five progress lines around the `--pages` pipeline:
//!
//! ```text
//! qpdf: selecting --keep-open-files=y
//! qpdf: <file>: checking for shared resources
//! qpdf: no shared resources found
//! qpdf: removing unreferenced pages from primary input
//! qpdf: adding pages from <file>
//! ```
//!
//! Reference: `libqpdf/QPDFJob.cc` L2250 / L2312 / L2425 / L2539 / L2594.
//!
//! flpdf-cli emits the same lines with a `flpdf:` prefix; the qtest shim
//! rewrites `^flpdf:` → `qpdf:` for golden comparison. Byte-parity of the
//! block against `uo-6.out` / `uo-8.out` is verified via that harness under
//! `qpdf-zlib-compat`. These tests assert emission and ordering so
//! regressions surface without the harness dependency.

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
fn verbose_pages_alone_emits_qpdf_parity_progress_block() {
    let src = fixture("two-page.pdf");
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = tmp.path().join("out.pdf");

    Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(["--verbose", "--static-id"])
        .arg(&src)
        .args(["--pages", ".", "1", "--"])
        .arg(out.to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "flpdf: selecting --keep-open-files=y{EOL}"
        )))
        .stdout(predicate::str::contains(format!(
            "two-page.pdf: checking for shared resources{EOL}"
        )))
        .stdout(predicate::str::contains(format!(
            "flpdf: no shared resources found{EOL}"
        )))
        .stdout(predicate::str::contains(format!(
            "flpdf: removing unreferenced pages from primary input{EOL}"
        )))
        .stdout(predicate::str::contains(format!(
            "flpdf: adding pages from two-page.pdf{EOL}"
        )))
        .stdout(predicate::str::contains("flpdf: wrote file"))
        .stderr(predicate::str::is_empty());
}

#[test]
fn verbose_pages_progress_lines_are_ordered_matching_qpdf() {
    let src = fixture("two-page.pdf");
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = tmp.path().join("out.pdf");

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(["--verbose", "--static-id"])
        .arg(&src)
        .args(["--pages", ".", "1", "--"])
        .arg(out.to_str().unwrap())
        .output()
        .expect("flpdf invocation");
    assert!(output.status.success(), "flpdf failed: {:?}", output);
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");

    let i_kfo = stdout
        .find("selecting --keep-open-files")
        .expect("kfo line present");
    let i_check = stdout
        .find("checking for shared resources")
        .expect("checking line present");
    let i_nosh = stdout
        .find("no shared resources found")
        .expect("no-shared line present");
    let i_rm = stdout
        .find("removing unreferenced pages")
        .expect("removing line present");
    let i_add = stdout
        .find("adding pages from")
        .expect("adding-from line present");
    let i_wrote = stdout.find("wrote file").expect("wrote-file line present");

    assert!(i_kfo < i_check, "keep-open-files must precede checking");
    assert!(i_check < i_nosh, "checking must precede no-shared");
    assert!(i_nosh < i_rm, "no-shared must precede removing");
    assert!(i_rm < i_add, "removing must precede adding");
    assert!(i_add < i_wrote, "adding must precede wrote-file");
}

#[test]
fn verbose_pages_plus_overlay_emits_page_block_before_overlay_block() {
    let src = fixture("two-page.pdf");
    let overlay = fixture("one-page.pdf");
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = tmp.path().join("out.pdf");

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(["--verbose", "--static-id"])
        .arg("--overlay")
        .arg(&overlay)
        .args(["--to=1", "--from=1", "--"])
        .arg(&src)
        .args(["--pages", ".", "1-2", "--"])
        .arg(out.to_str().unwrap())
        .output()
        .expect("flpdf invocation");
    assert!(
        output.status.success(),
        "flpdf failed: stderr={:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");

    let i_pages = stdout
        .find("removing unreferenced pages")
        .expect("page-selection block present");
    let i_overlay = stdout
        .find("processing underlay/overlay")
        .expect("overlay block present");
    let i_wrote = stdout.find("wrote file").expect("wrote-file present");

    assert!(
        i_pages < i_overlay,
        "page-selection block must precede overlay block"
    );
    assert!(
        i_overlay < i_wrote,
        "overlay block must precede wrote-file line"
    );
}

#[test]
fn verbose_rotate_alone_emits_only_wrote_file_line() {
    // --rotate/--split-pages without --pages runs the smaller
    // run_rewrite_with_page_ops path. qpdf's page-selection progress
    // block does not fire in this branch — only the terminal
    // "wrote file" line does.
    let src = fixture("two-page.pdf");
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = tmp.path().join("out.pdf");

    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(["--verbose", "--static-id"])
        .arg(&src)
        .args(["--rotate=+90:1"])
        .arg(out.to_str().unwrap())
        .output()
        .expect("flpdf invocation");
    assert!(output.status.success(), "flpdf failed: {:?}", output);
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    assert!(
        stdout.contains("wrote file"),
        "wrote-file line must appear, got: {stdout:?}"
    );
    assert!(
        !stdout.contains("checking for shared resources"),
        "no --pages → no shared-resources block, got: {stdout:?}"
    );
    assert!(
        !stdout.contains("removing unreferenced pages"),
        "no --pages → no removing-unreferenced block, got: {stdout:?}"
    );
}
