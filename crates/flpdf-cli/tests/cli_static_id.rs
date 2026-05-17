//! `flpdf rewrite --static-id` regression tests.
//!
//! Pins two invariants:
//! 1. Two `--static-id` runs over the same input are byte-identical (cheap
//!    determinism check that runs on every host).
//! 2. The trailer `/ID` array values match what qpdf emits under `--static-id`
//!    on the same input: ID[0] is preserved from the input trailer when
//!    present, and ID[1] is qpdf's static constant (the first 32 hex digits
//!    of π). The byte-level comparison is value-only — the surrounding array
//!    syntax (whitespace between `<...>` elements) still differs and is
//!    deferred to the writer-whitespace task in epic 9hc.20.
//!
//! Sub-test (2) needs qpdf as an external oracle and follows the same skip
//! rules as `cli_linearize_qpdf.rs`: hard-fail on Linux CI, soft-skip locally
//! and on Windows.

use assert_cmd::Command as CargoCommand;
use std::path::{Path, PathBuf};
use std::process::Command as ShellCommand;
use tempfile::tempdir;

const COMPAT_FIXTURE_DIR: &str = "../../tests/fixtures/compat";

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(COMPAT_FIXTURE_DIR)
        .join(name)
}

fn qpdf_available() -> bool {
    ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[must_use]
fn skip_if_qpdf_missing() -> bool {
    if qpdf_available() {
        return false;
    }
    let on_ci = std::env::var_os("CI").is_some();
    // Windows (choco) and macOS (Homebrew) both hit the separately-tracked
    // qpdf vector::_M_range_check defect (flpdf-d4k), so CI deliberately does
    // not install qpdf there — treat both as skip-allowed, not a hard-fail.
    let qpdf_install_skipped = cfg!(any(target_os = "windows", target_os = "macos"));
    if on_ci && !qpdf_install_skipped {
        panic!(
            "qpdf is required for cli_static_id tests on CI (Linux); \
             install qpdf in the workflow before running this test suite"
        );
    }
    eprintln!(
        "skipping: qpdf not available (target_os={}, CI={})",
        std::env::consts::OS,
        on_ci
    );
    true
}

fn run_flpdf_static_id(input: &Path, output: &Path) {
    CargoCommand::cargo_bin("flpdf")
        .expect("flpdf binary")
        .arg("rewrite")
        .arg("--static-id")
        .arg(input)
        .arg(output)
        .assert()
        .success();
}

fn run_qpdf_static_id(input: &Path, output: &Path) {
    let status = ShellCommand::new("qpdf")
        .arg("--static-id")
        .arg(input)
        .arg(output)
        .status()
        .expect("invoking qpdf");
    assert!(status.success(), "qpdf --static-id failed");
}

/// Locate the last `/ID` array in a PDF and return its two hex-string element
/// payloads (without the angle brackets). Limited to the shapes the fixtures
/// emit — `<hex0> <hex1>` or `<hex0><hex1>`, possibly preceded by whitespace.
fn extract_id_array(pdf_bytes: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let idx = pdf_bytes
        .windows(3)
        .enumerate()
        .filter(|(_, w)| *w == b"/ID")
        .map(|(i, _)| i)
        .next_back()
        .expect("/ID not found");
    let after = &pdf_bytes[idx + 3..];

    // Find the opening `[`.
    let open = after.iter().position(|&b| b == b'[').expect("expected [");
    let body = &after[open + 1..];

    let lt0 = body.iter().position(|&b| b == b'<').expect("expected <");
    let gt0 = body[lt0 + 1..]
        .iter()
        .position(|&b| b == b'>')
        .expect("expected >");
    let first = body[lt0 + 1..lt0 + 1 + gt0].to_vec();

    let rest = &body[lt0 + gt0 + 2..];
    let lt1 = rest.iter().position(|&b| b == b'<').expect("expected <");
    let gt1 = rest[lt1 + 1..]
        .iter()
        .position(|&b| b == b'>')
        .expect("expected >");
    let second = rest[lt1 + 1..lt1 + 1 + gt1].to_vec();

    (first, second)
}

#[test]
fn static_id_is_deterministic_across_runs() {
    let tmp = tempdir().expect("tempdir");
    let input = fixture_path("one-page.pdf");
    let a = tmp.path().join("a.pdf");
    let b = tmp.path().join("b.pdf");

    run_flpdf_static_id(&input, &a);
    run_flpdf_static_id(&input, &b);

    let bytes_a = std::fs::read(&a).expect("read a");
    let bytes_b = std::fs::read(&b).expect("read b");
    assert_eq!(
        bytes_a, bytes_b,
        "two --static-id runs produced different bytes"
    );
}

#[test]
fn static_id_trailer_id_matches_qpdf_oracle() {
    if skip_if_qpdf_missing() {
        return;
    }
    let tmp = tempdir().expect("tempdir");

    for name in ["one-page.pdf", "two-page.pdf", "three-page.pdf"] {
        let input = fixture_path(name);
        let flpdf_out = tmp.path().join(format!("flpdf_{name}"));
        let qpdf_out = tmp.path().join(format!("qpdf_{name}"));

        run_flpdf_static_id(&input, &flpdf_out);
        run_qpdf_static_id(&input, &qpdf_out);

        let flpdf_bytes = std::fs::read(&flpdf_out).expect("read flpdf");
        let qpdf_bytes = std::fs::read(&qpdf_out).expect("read qpdf");

        let (flpdf_id0, flpdf_id1) = extract_id_array(&flpdf_bytes);
        let (qpdf_id0, qpdf_id1) = extract_id_array(&qpdf_bytes);

        assert_eq!(
            flpdf_id0, qpdf_id0,
            "{name}: /ID[0] (permanent identifier) diverged from qpdf"
        );
        assert_eq!(
            flpdf_id1, qpdf_id1,
            "{name}: /ID[1] (changing identifier) diverged from qpdf"
        );
        // Spot-check ID[1] equals π's first 32 hex digits as a literal
        // sanity check on the qpdf reference.
        assert_eq!(
            flpdf_id1, b"31415926535897932384626433832795",
            "/ID[1] is not the qpdf static-id constant"
        );
    }
}

#[test]
fn static_id_with_linearize_succeeds() {
    // --static-id with --linearize is now supported (epic 9hc.20 sub-task .19).
    let tmp = tempdir().expect("tempdir");
    let input = fixture_path("one-page.pdf");
    let output = tmp.path().join("out.pdf");

    CargoCommand::cargo_bin("flpdf")
        .expect("flpdf binary")
        .arg("rewrite")
        .arg("--linearize")
        .arg("--static-id")
        .arg(&input)
        .arg(&output)
        .assert()
        .success();

    assert!(
        output.exists(),
        "linearized --static-id output must be created"
    );
}
