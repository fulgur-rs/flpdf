//! End-to-end CLI coverage for the match path: identical files must exit 0
//! with the *expected* file's bytes dumped verbatim to stdout (no re-parse,
//! no re-serialize). This is qpdf's `qpdf-test-compare` "difference.empty()
//! -> to_output = expected" branch.

use assert_cmd::Command;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

fn fixture_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

#[test]
fn identical_files_emit_expected_and_exit_zero() {
    // Copy the fixture into a temp dir so `actual` and `expected` are two
    // distinct paths (mirrors real usage: the harness passes two files).
    let dir = TempDir::new().unwrap();
    let src = fixture_path("tests/fixtures/minimal.pdf");
    let src_bytes = fs::read(&src).expect("read minimal.pdf fixture");
    let a = dir.path().join("a.pdf");
    let b = dir.path().join("b.pdf");
    fs::write(&a, &src_bytes).unwrap();
    fs::write(&b, &src_bytes).unwrap();

    let output = Command::cargo_bin("qpdf-test-compare")
        .unwrap()
        .args([a.to_str().unwrap(), b.to_str().unwrap()])
        .output()
        .expect("spawn qpdf-test-compare");

    assert!(
        output.status.success(),
        "expected exit 0, got status={:?} stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    // The critical assertion: stdout is the *expected* file (arg 2) copied
    // byte-for-byte. If the pipeline ever re-parsed and re-serialized, the
    // bytes would differ and this assert_eq! would fail loudly — that's the
    // whole point of the test.
    assert_eq!(
        output.stdout, src_bytes,
        "expected stdout == expected file bytes (no re-serialize)",
    );
    assert!(
        output.stderr.is_empty(),
        "expected empty stderr, got {:?}",
        String::from_utf8_lossy(&output.stderr),
    );
}
