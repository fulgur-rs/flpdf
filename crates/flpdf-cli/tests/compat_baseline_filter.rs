//! Filter-policy baseline test: verify that `flpdf rewrite --full-rewrite`
//! produces streams with the same `/Filter` policy as `qpdf <in> <out>`.
//!
//! For each unencrypted, non-linearized fixture:
//! 1. Run `qpdf <input> qpdf-out.pdf` (plain passthrough, no flags).
//! 2. Run `flpdf rewrite --full-rewrite --static-id <input> flpdf-out.pdf`.
//! 3. Open each output with `flpdf::Pdf::open`, resolve all known object refs,
//!    filter for `Object::Stream`, and extract the `/Filter` field from each.
//! 4. Assert that every stream's `/Filter` resolves to exactly `[FlateDecode]`
//!    in both the qpdf and flpdf outputs.
//!
//! Object numbers legitimately differ between qpdf and flpdf outputs (renumber
//! is handled separately), so we compare the *per-stream filter invariant*
//! independently on each side rather than zipping by object number.
//!
//! A PDF `/Filter` entry may be a `Name` (`/FlateDecode`) or a single-element
//! `Array` (`[/FlateDecode]`); the helper `filter_names` normalizes both forms
//! to `Vec<Vec<u8>>` so the assertion is form-agnostic.
//!
//! Fixtures excluded:
//! - `linearized-one-page.pdf`    — linearize path is independent of full_rewrite (separate issue).
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
use flpdf::{Object, Pdf};

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .to_path_buf()
}

// ---------------------------------------------------------------------------
// Fixtures (unencrypted and non-linearized only)
// ---------------------------------------------------------------------------

const FIXTURES: &[&str] = &[
    "one-page.pdf",
    "two-page.pdf",
    "three-page.pdf",
    "attachment-two-page.pdf",
];

// ---------------------------------------------------------------------------
// Helper: normalize /Filter entry to a list of filter name bytes
// ---------------------------------------------------------------------------

/// Extract the list of filter names from a stream dictionary's `/Filter` value.
///
/// PDF allows `/Filter /FlateDecode` (Name) or `/Filter [/FlateDecode ...]`
/// (Array).  This function normalizes both forms to `Vec<Vec<u8>>`.
///
/// Returns an empty `Vec` when the stream has no `/Filter` entry.
fn filter_names(stream: &flpdf::Stream) -> Vec<Vec<u8>> {
    match stream.dict.get("Filter") {
        None => vec![],
        Some(Object::Name(name)) => vec![name.clone()],
        Some(Object::Array(arr)) => arr
            .iter()
            .filter_map(|item| {
                if let Object::Name(n) = item {
                    Some(n.clone())
                } else {
                    None
                }
            })
            .collect(),
        _ => vec![],
    }
}

// ---------------------------------------------------------------------------
// Helper: collect filter name lists from all streams in a PDF file
// ---------------------------------------------------------------------------

/// Open `path` and return the `/Filter` names for every stream object found.
///
/// Objects are iterated in `object_refs()` order (which is stable for a given
/// PDF).  `Object::Null` (freed / missing entries) and non-stream objects are
/// ignored.
fn collect_stream_filters(path: &Path) -> Vec<Vec<Vec<u8>>> {
    let file =
        File::open(path).unwrap_or_else(|e| panic!("failed to open {}: {e}", path.display()));
    let reader = BufReader::new(file);
    let mut pdf =
        Pdf::open(reader).unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()));

    let mut refs = pdf.object_refs();
    refs.sort(); // stable order

    let mut filters: Vec<Vec<Vec<u8>>> = Vec::new();
    for obj_ref in refs {
        let obj = pdf.resolve(obj_ref).unwrap_or_else(|e| {
            panic!("failed to resolve {:?} in {}: {e}", obj_ref, path.display())
        });
        if let Object::Stream(stream) = obj {
            filters.push(filter_names(&stream));
        }
    }
    filters
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

#[test]
fn stream_filter_policy_matches_qpdf() {
    // Skip the whole test when qpdf is not available.
    if !support::is_qpdf_available() {
        eprintln!("qpdf not found on PATH — skipping stream_filter_policy_matches_qpdf");
        return;
    }

    for fixture_name in FIXTURES {
        let fixture_path = fixtures_dir().join(fixture_name);

        let tmp_dir = tempfile::tempdir().expect("failed to create tempdir");
        let qpdf_out = tmp_dir.path().join("qpdf-out.pdf");
        let flpdf_out = tmp_dir.path().join("flpdf-out.pdf");

        // 1. Run qpdf (plain passthrough).
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

        // 3. Collect stream filters from each output.
        let qpdf_filters = collect_stream_filters(&qpdf_out);
        let flpdf_filters = collect_stream_filters(&flpdf_out);

        // 4a. Assert that every stream in the qpdf output uses /FlateDecode only.
        for (idx, names) in qpdf_filters.iter().enumerate() {
            assert_eq!(
                names,
                &vec![b"FlateDecode".to_vec()],
                "qpdf output for {fixture_name}: stream {idx} has unexpected /Filter: {:?}",
                names
                    .iter()
                    .map(|n| String::from_utf8_lossy(n).into_owned())
                    .collect::<Vec<_>>()
            );
        }

        // 4b. Assert that every stream in the flpdf output uses /FlateDecode only.
        for (idx, names) in flpdf_filters.iter().enumerate() {
            assert_eq!(
                names,
                &vec![b"FlateDecode".to_vec()],
                "flpdf output for {fixture_name}: stream {idx} has unexpected /Filter: {:?}",
                names
                    .iter()
                    .map(|n| String::from_utf8_lossy(n).into_owned())
                    .collect::<Vec<_>>()
            );
        }

        eprintln!(
            "[PASS] {fixture_name}: qpdf={} streams, flpdf={} streams — all /FlateDecode",
            qpdf_filters.len(),
            flpdf_filters.len()
        );
    }
}
