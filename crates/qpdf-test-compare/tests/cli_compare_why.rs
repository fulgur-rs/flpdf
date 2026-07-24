//! End-to-end coverage for qpdf's `QPDF_COMPARE_WHY` env: when set, the diff
//! reason goes to stderr and the actual-file stdout dump is skipped. The env
//! must NOT affect the match branch — identical files still emit the
//! expected file on stdout and exit 0.
//!
//! Fixture bytes are declared inline (same style as `tests/orchestrator.rs`)
//! so this test is fully self-contained — no external tool needed to
//! regenerate PDFs and no vendored qpdf goldens. `DIFFER_A_PDF` and
//! `DIFFER_B_PDF` mirror the `MINIMAL_PDF` / `MINIMAL_PDF_COUNT1` pair used
//! by the orchestrator test: both have the same object count and order and
//! the same trailer, and differ solely in obj 2's `/Count` value. That
//! forces the per-object content-diff branch (label "2 0"), which is exactly
//! the qpdf branch the WHY test needs to exercise.

use assert_cmd::Command;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

fn fixture_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

/// Baseline: 2 in-use objects (Catalog + Pages with `/Count 0`). Identical
/// to `tests/fixtures/minimal.pdf`. Duplicated inline so the test doesn't
/// depend on the fixture file — the bytes below are what the WHY branch is
/// actually asked to diff.
const DIFFER_A_PDF: &[u8] = b"\
%PDF-1.7
1 0 obj
<< /Type /Catalog /Pages 2 0 R >>
endobj
2 0 obj
<< /Type /Pages /Count 0 /Kids [] >>
endobj
xref
0 3
0000000000 65535 f\x20
0000000009 00000 n\x20
0000000058 00000 n\x20
trailer
<< /Size 3 /Root 1 0 R >>
startxref
110
%%EOF
";

/// Same shape as `DIFFER_A_PDF` but obj 2's `/Count` is `1` (both digits are
/// one byte so xref offsets and startxref stay valid). Trailer is unchanged
/// so the trailer compare passes and the diff surfaces on obj `2 0` — the
/// per-object-content-diff branch, which is the one WHY mode has to print.
const DIFFER_B_PDF: &[u8] = b"\
%PDF-1.7
1 0 obj
<< /Type /Catalog /Pages 2 0 R >>
endobj
2 0 obj
<< /Type /Pages /Count 1 /Kids [] >>
endobj
xref
0 3
0000000000 65535 f\x20
0000000009 00000 n\x20
0000000058 00000 n\x20
trailer
<< /Size 3 /Root 1 0 R >>
startxref
110
%%EOF
";

#[test]
fn compare_why_prints_reason_and_skips_output() {
    let dir = TempDir::new().unwrap();
    let a = dir.path().join("a.pdf");
    let b = dir.path().join("b.pdf");
    fs::write(&a, DIFFER_A_PDF).unwrap();
    fs::write(&b, DIFFER_B_PDF).unwrap();

    let output = Command::cargo_bin("qpdf-test-compare")
        .unwrap()
        .env("QPDF_COMPARE_WHY", "1")
        .args([a.to_str().unwrap(), b.to_str().unwrap()])
        .output()
        .expect("spawn qpdf-test-compare");

    assert_eq!(
        output.status.code(),
        Some(2),
        "diff must exit 2; stderr={:?}",
        String::from_utf8_lossy(&output.stderr),
    );
    // The whole point of WHY mode: no cat of the actual file on stdout.
    assert!(
        output.stdout.is_empty(),
        "WHY mode must skip stdout dump; got {} bytes on stdout",
        output.stdout.len(),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    // The reason is one of the fixed qpdf strings; this fixture pair
    // triggers the object-content-diff branch ("N G: object contents
    // differ"). Accept the sibling stream-branch messages too so this
    // stays a per-branch behavior check rather than a per-byte check.
    assert!(
        stderr.contains("object contents differ")
            || stderr.contains("stream data differs")
            || stderr.contains("stream dictionaries differ"),
        "unexpected stderr: {stderr:?}",
    );
    // Bonus: the label prefix must be qpdf's "N G" (no trailing R). If the
    // orchestrator ever regressed to formatting via ObjectRef::Display,
    // we'd see "2 0 R:" here instead.
    assert!(
        stderr.contains("2 0:"),
        "expected 'N G:' label prefix (no trailing R); got {stderr:?}",
    );
}

#[test]
fn compare_why_does_not_affect_match_path() {
    // Positive control: WHY=1 must not perturb the match branch. Two
    // identical files still exit 0 with the expected-file bytes on stdout
    // and no stderr — proving the show_why check only fires inside the
    // "Some(reason)" arm of the compare result.
    let dir = TempDir::new().unwrap();
    let src = fixture_path("tests/fixtures/minimal.pdf");
    let src_bytes = fs::read(&src).expect("read minimal.pdf fixture");
    let a = dir.path().join("a.pdf");
    let b = dir.path().join("b.pdf");
    fs::write(&a, &src_bytes).unwrap();
    fs::write(&b, &src_bytes).unwrap();

    let output = Command::cargo_bin("qpdf-test-compare")
        .unwrap()
        .env("QPDF_COMPARE_WHY", "1")
        .args([a.to_str().unwrap(), b.to_str().unwrap()])
        .output()
        .expect("spawn qpdf-test-compare");

    assert!(
        output.status.success(),
        "match path must still exit 0 with WHY=1; stderr={:?}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(
        output.stdout, src_bytes,
        "match path still cats expected file verbatim under WHY=1",
    );
    assert!(
        output.stderr.is_empty(),
        "match path must not print to stderr under WHY=1; got {:?}",
        String::from_utf8_lossy(&output.stderr),
    );
}
