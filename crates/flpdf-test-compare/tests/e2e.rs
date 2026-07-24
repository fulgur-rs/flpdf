//! End-to-end scenarios for the `flpdf-test-compare` binary.
//!
//! The unit tests in `src/clean.rs`, `src/compare.rs`, and the integration
//! tests in `tests/orchestrator.rs` cover the algorithm at the library
//! level. This file complements them by exercising **CLI semantics** —
//! argv parsing, password wiring, exit codes, and byte-verbatim stdout —
//! for pairs where the library-level tests can't reach `main.rs`.
//!
//! Every match test asserts `stdout == expected file's raw bytes`, and the
//! differ test asserts `stdout == actual file's raw bytes`. Any accidental
//! re-serialization surfaces as an `assert_eq!` mismatch — that's what
//! makes these tests load-bearing for the "no re-serialize" invariant of
//! qpdf's oracle.

use assert_cmd::Command;
use std::fs;
use std::path::PathBuf;

/// Resolve a path relative to the workspace root (two levels up from this
/// crate's manifest directory). Matches the pattern used elsewhere in this
/// crate's tests (`tests/cli_match_path.rs`, `tests/cli_compare_why.rs`).
fn fixture_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

/// Match-path scenarios: identical (after cleanup) inputs must exit 0 and
/// dump the **expected** file's raw bytes to stdout.
mod match_paths {
    use super::*;

    /// Trailer `/ID[1]` differs between the two files; `clean_trailer`
    /// blanks the second half on both sides, and the cleaned trailers plus
    /// per-object walk then compare equal. Proves the `/ID`-half masking
    /// reaches the wire.
    #[test]
    fn id_differs_still_matches() {
        let a = fixture_path("tests/fixtures/compare_for_test/id_differs_a.pdf");
        let b = fixture_path("tests/fixtures/compare_for_test/id_differs_b.pdf");
        let expected_bytes = fs::read(&b).expect("read id_differs_b.pdf");

        let output = Command::cargo_bin("flpdf-test-compare")
            .unwrap()
            .args([a.to_str().unwrap(), b.to_str().unwrap()])
            .output()
            .expect("spawn flpdf-test-compare");

        assert!(
            output.status.success(),
            "expected exit 0 for /ID-diff match; stderr={:?}",
            String::from_utf8_lossy(&output.stderr),
        );
        assert_eq!(
            output.stdout, expected_bytes,
            "match path must cat the expected file byte-verbatim",
        );
        assert!(
            output.stderr.is_empty(),
            "match path emits nothing on stderr; got {:?}",
            String::from_utf8_lossy(&output.stderr),
        );
    }

    /// Object 3 is a `/Filter /FlateDecode` stream whose compressed bytes
    /// and `/Length` differ across the pair but whose decoded payload is
    /// identical. `compare_streams` must strip `/Length` and then compare
    /// the decoded bodies, so the pair matches → exit 0 and stdout ==
    /// expected file's raw bytes.
    ///
    /// Gated on `qpdf-zlib-compat` per the plan: the CLI-level check for
    /// "compression variance is tolerated" belongs to the compat matrix
    /// job. The unit-level equivalent (`compare.rs::
    /// flate_same_decoded_different_compressed_matches`) already runs under
    /// default features.
    #[cfg(feature = "qpdf-zlib-compat")]
    #[test]
    fn flate_compressed_bytes_differ_decoded_matches() {
        let a = fixture_path("tests/fixtures/compare_for_test/flate_miniz.pdf");
        let b = fixture_path("tests/fixtures/compare_for_test/flate_zlib.pdf");
        let expected_bytes = fs::read(&b).expect("read flate_zlib.pdf");

        // Premise: the two fixtures really do differ on disk (otherwise the
        // test is vacuous). The fixture generator asserts this too, but the
        // test asserts it on the checked-in bytes.
        let actual_bytes = fs::read(&a).expect("read flate_miniz.pdf");
        assert_ne!(
            actual_bytes, expected_bytes,
            "premise: fixture pair must differ on disk",
        );

        let output = Command::cargo_bin("flpdf-test-compare")
            .unwrap()
            .args([a.to_str().unwrap(), b.to_str().unwrap()])
            .output()
            .expect("spawn flpdf-test-compare");

        assert!(
            output.status.success(),
            "expected exit 0 for flate variance match; stderr={:?}",
            String::from_utf8_lossy(&output.stderr),
        );
        assert_eq!(
            output.stdout, expected_bytes,
            "match path cats expected file byte-verbatim",
        );
    }

    /// Same encrypted file on both sides, with the correct password passed
    /// as `argv[3]`. Proves `argv[3] → PdfOpenOptions::password` plumbing
    /// in `main.rs` — an unencrypted-file test can't reach this branch.
    #[test]
    fn password_plumbing_encrypted_pair() {
        let path = fixture_path("tests/fixtures/encrypted/v5-aes-256-r6.pdf");
        let expected_bytes = fs::read(&path).expect("read v5-aes-256-r6.pdf");

        let output = Command::cargo_bin("flpdf-test-compare")
            .unwrap()
            .args([path.to_str().unwrap(), path.to_str().unwrap(), "user-v5-r6"])
            .output()
            .expect("spawn flpdf-test-compare");

        assert!(
            output.status.success(),
            "expected exit 0 with correct password; stderr={:?}",
            String::from_utf8_lossy(&output.stderr),
        );
        assert_eq!(
            output.stdout, expected_bytes,
            "match path cats the encrypted file byte-verbatim",
        );
        assert!(
            output.stderr.is_empty(),
            "match path emits nothing on stderr; got {:?}",
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

/// Differ-path scenarios: mismatched or unopenable inputs must exit 2 with
/// either the **actual** file's bytes on stdout (real diff) or nothing on
/// stdout (open/parse error → stderr-only).
mod differ_paths {
    use super::*;

    /// Object 2's body differs between the pair (same shape everywhere
    /// else). The compare reports `"2 0: object contents differ"`, and
    /// main dumps the **actual** file's raw bytes to stdout before exiting
    /// 2. Proves the "diff → cat actual" branch is byte-verbatim.
    #[test]
    fn object_body_diff_emits_actual_bytes() {
        let a = fixture_path("tests/fixtures/compare_for_test/differ_body_a.pdf");
        let b = fixture_path("tests/fixtures/compare_for_test/differ_body_b.pdf");
        let actual_bytes = fs::read(&a).expect("read differ_body_a.pdf");

        let output = Command::cargo_bin("flpdf-test-compare")
            .unwrap()
            .args([a.to_str().unwrap(), b.to_str().unwrap()])
            .output()
            .expect("spawn flpdf-test-compare");

        assert_eq!(
            output.status.code(),
            Some(2),
            "diff must exit 2; stderr={:?}",
            String::from_utf8_lossy(&output.stderr),
        );
        assert_eq!(
            output.stdout, actual_bytes,
            "diff path must cat the actual file byte-verbatim",
        );
        assert!(
            output.stderr.is_empty(),
            "diff path emits nothing on stderr (that's WHY mode's job); got {:?}",
            String::from_utf8_lossy(&output.stderr),
        );
    }

    /// Wrong password → `Pdf::open_mem_owned_with_options` returns Err →
    /// `main()`'s Err branch prints `"flpdf-test-compare: <err>"` to stderr,
    /// dumps nothing to stdout, and exits 2. Proves the compare-files Err
    /// arm reaches the stderr-only exit and does NOT fall through to the
    /// actual-file dump.
    #[test]
    fn wrong_password_reports_stderr_no_stdout() {
        let path = fixture_path("tests/fixtures/encrypted/v5-aes-256-r6.pdf");

        let output = Command::cargo_bin("flpdf-test-compare")
            .unwrap()
            .args([
                path.to_str().unwrap(),
                path.to_str().unwrap(),
                "wrong-password",
            ])
            .output()
            .expect("spawn flpdf-test-compare");

        assert_eq!(
            output.status.code(),
            Some(2),
            "wrong password must exit 2; stderr={:?}",
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(
            output.stdout.is_empty(),
            "Err path must skip stdout dump; got {} bytes",
            output.stdout.len(),
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("flpdf-test-compare:"),
            "expected 'flpdf-test-compare:' prefix on stderr; got {stderr:?}",
        );
    }
}
