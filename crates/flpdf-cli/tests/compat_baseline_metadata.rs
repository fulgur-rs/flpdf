//! Metadata-preservation baseline test: Info dict byte-equality between
//! `flpdf rewrite --full-rewrite` and `qpdf <in> <out>`.
//!
//! For each unencrypted fixture:
//! 1. Run `qpdf <input> qpdf-out.pdf` (plain, no flags).
//! 2. Run `flpdf rewrite --full-rewrite --static-id <input> flpdf-out.pdf`.
//! 3. Open each output with `flpdf::Pdf::open`, resolve the `/Info` object
//!    referenced from the trailer, serialize it with `Object::write_pdf`, and
//!    assert the two serialized forms are byte-equal.
//!
//! The comparison is parse-equivalence (round-trip through flpdf's serializer),
//! not raw on-disk byte comparison. This is intentional: object numbers and
//! offsets legitimately differ between qpdf and flpdf output; only the Info
//! dict *contents* matter for this policy check. A stronger byte-exact
//! comparison of the raw `N 0 obj…endobj` block is not required here because
//! the orchestrator already confirmed on-disk equality empirically.
//!
//! Fixtures excluded:
//! - `encrypted-r4-three-page.pdf` — full_rewrite rejects encrypted documents.
//!
//! If `qpdf` is not on PATH the entire test is skipped (non-failing).

#[allow(dead_code, unused_imports)]
#[path = "support/mod.rs"]
mod support;

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::Command as ShellCommand;

use assert_cmd::Command as CargoCommand;
use flpdf::Pdf;

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .to_path_buf()
}

// ---------------------------------------------------------------------------
// Fixtures (unencrypted only)
// ---------------------------------------------------------------------------

const FIXTURES: &[&str] = &[
    "one-page.pdf",
    "two-page.pdf",
    "three-page.pdf",
    "attachment-two-page.pdf",
    "linearized-one-page.pdf",
];

// ---------------------------------------------------------------------------
// Helper: extract and serialize the Info dict from a PDF file
// ---------------------------------------------------------------------------

/// Open `path` and return the serialized bytes of the `/Info` dictionary.
///
/// Uses `flpdf::Pdf::open` → `trailer().get_ref("Info")` → `resolve()` →
/// `Object::write_pdf`. Returns `None` if the trailer has no `/Info` entry.
fn extract_info_dict_bytes(path: &Path) -> Option<Vec<u8>> {
    let file = File::open(path).unwrap_or_else(|e| panic!("failed to open {}: {e}", path.display()));
    let reader = BufReader::new(file);
    let mut pdf = Pdf::open(reader)
        .unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()));

    let info_ref = pdf.trailer().get_ref("Info")?;
    let info_obj = pdf
        .resolve(info_ref)
        .unwrap_or_else(|e| panic!("failed to resolve Info in {}: {e}", path.display()));

    let mut buf: Vec<u8> = Vec::new();
    info_obj.write_pdf(&mut buf);
    Some(buf)
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

#[test]
fn info_dict_preserved_under_full_rewrite() {
    // Skip the whole test when qpdf is not available.
    if !support::is_qpdf_available() {
        eprintln!("qpdf not found on PATH — skipping info_dict_preserved_under_full_rewrite");
        return;
    }

    let fixtures = FIXTURES;

    for fixture_name in fixtures {
        let fixture_path = fixtures_dir().join(fixture_name);

        let tmp_dir = tempfile::tempdir().expect("failed to create tempdir");
        let qpdf_out = tmp_dir.path().join("qpdf-out.pdf");
        let flpdf_out = tmp_dir.path().join("flpdf-out.pdf");

        // 1. Run qpdf (plain passthrough — preserves Info verbatim).
        let qpdf_status = ShellCommand::new("qpdf")
            .arg(fixture_path.to_str().unwrap())
            .arg(qpdf_out.to_str().unwrap())
            .output()
            .unwrap_or_else(|e| panic!("failed to spawn qpdf for {fixture_name}: {e}"));

        assert!(
            qpdf_status.status.success(),
            "qpdf failed for {fixture_name}: {}",
            String::from_utf8_lossy(&qpdf_status.stderr)
        );

        // 2. Run flpdf rewrite --full-rewrite --static-id.
        let flpdf_status = CargoCommand::cargo_bin("flpdf")
            .expect("flpdf binary must exist")
            .arg("rewrite")
            .arg("--full-rewrite")
            .arg("--static-id")
            .arg(fixture_path.to_str().unwrap())
            .arg(flpdf_out.to_str().unwrap())
            .output()
            .unwrap_or_else(|e| panic!("failed to spawn flpdf for {fixture_name}: {e}"));

        assert!(
            flpdf_status.status.success(),
            "flpdf failed for {fixture_name}: {}",
            String::from_utf8_lossy(&flpdf_status.stderr)
        );

        // 3. Extract and compare Info dict serializations.
        let qpdf_info = extract_info_dict_bytes(&qpdf_out)
            .unwrap_or_else(|| panic!("qpdf output for {fixture_name} has no /Info in trailer"));
        let flpdf_info = extract_info_dict_bytes(&flpdf_out)
            .unwrap_or_else(|| panic!("flpdf output for {fixture_name} has no /Info in trailer"));

        assert_eq!(
            qpdf_info,
            flpdf_info,
            "Info dict mismatch for {fixture_name}:\n  qpdf:  {}\n  flpdf: {}",
            String::from_utf8_lossy(&qpdf_info),
            String::from_utf8_lossy(&flpdf_info),
        );

        eprintln!("[PASS] {fixture_name}: Info dict byte-equal");
    }
}
