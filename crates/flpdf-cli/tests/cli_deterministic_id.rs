//! `flpdf rewrite --deterministic-id` CLI tests.
//!
//! Pins the wiring of qpdf's `--deterministic-id`:
//! - two runs over the same input are byte-identical (self-stable /ID);
//! - /ID[0] (permanent identifier) is preserved from the input, while /ID[1]
//!   (changing identifier) is content-derived and is not the `--static-id`
//!   constant;
//! - the qpdf-incompatible combinations are rejected (+ `--static-id`,
//!   + `--encrypt`, + `--linearize`);
//! - unlike `--static-id`, the flag is production-safe and emits no
//!   testing-only warning.
//!
//! The /ID[1] body-digest algorithm is verified by the library test
//! `writer::tests::deterministic_id_is_stable_and_equals_md5_over_body`. flpdf's
//! digest is its own scheme and is NOT byte-identical to qpdf's /ID (qpdf seeds a
//! second MD5 with the body digest plus the `/Info` strings), so no qpdf
//! `/ID`-equality oracle runs here; the `--deterministic-id` equivalence is
//! behavioural (deterministic, content-derived, permanent-ID-preserving).

use assert_cmd::Command as CargoCommand;
use predicates::prelude::*;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

const COMPAT_FIXTURE_DIR: &str = "../../tests/fixtures/compat";

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(COMPAT_FIXTURE_DIR)
        .join(name)
}

fn run_rewrite_det_id(input: &Path, output: &Path) {
    CargoCommand::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["rewrite", "--deterministic-id"])
        .arg(input)
        .arg(output)
        .assert()
        .success();
}

/// Locate the last `/ID` array in a PDF and return its two hex-string element
/// payloads (without the angle brackets) — same shape handling as
/// `cli_static_id::extract_id_array`.
fn extract_id_array(pdf_bytes: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let idx = pdf_bytes
        .windows(3)
        .enumerate()
        .filter(|(_, w)| *w == b"/ID")
        .map(|(i, _)| i)
        .next_back()
        .expect("/ID not found");
    let after = &pdf_bytes[idx + 3..];

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
fn deterministic_id_is_stable_across_runs() {
    let tmp = tempdir().expect("tempdir");
    let input = fixture_path("one-page.pdf");
    let a = tmp.path().join("a.pdf");
    let b = tmp.path().join("b.pdf");

    run_rewrite_det_id(&input, &a);
    run_rewrite_det_id(&input, &b);

    assert_eq!(
        std::fs::read(&a).expect("read a"),
        std::fs::read(&b).expect("read b"),
        "two --deterministic-id runs must be byte-identical"
    );
}

#[test]
fn deterministic_id_preserves_permanent_id_and_is_content_dependent() {
    let tmp = tempdir().expect("tempdir");
    let out1 = tmp.path().join("one.pdf");
    let out2 = tmp.path().join("two.pdf");

    run_rewrite_det_id(&fixture_path("one-page.pdf"), &out1);
    run_rewrite_det_id(&fixture_path("two-page.pdf"), &out2);

    let (id0_1, id1_1) = extract_id_array(&std::fs::read(&out1).expect("read one"));
    let (_id0_2, id1_2) = extract_id_array(&std::fs::read(&out2).expect("read two"));

    // /ID[0] (permanent identifier) is preserved from one-page.pdf's input /ID.
    assert_eq!(
        id0_1.as_slice(),
        b"2dc780102304e5176780e8127fa6438c",
        "/ID[0] must be preserved from the source permanent identifier"
    );
    assert_ne!(
        id0_1, id1_1,
        "permanent and changing identifiers must differ"
    );
    // /ID[1] (changing identifier) is the content digest → content-dependent.
    assert_ne!(
        id1_1, id1_2,
        "different input content must yield a different /ID[1]"
    );
    assert_ne!(
        id1_1.as_slice(),
        b"31415926535897932384626433832795",
        "deterministic /ID[1] must not be the --static-id constant"
    );
}

#[test]
fn deterministic_id_conflicts_with_static_id() {
    let tmp = tempdir().expect("tempdir");
    let input = fixture_path("one-page.pdf");
    let output = tmp.path().join("out.pdf");

    CargoCommand::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["rewrite", "--deterministic-id", "--static-id"])
        .arg(&input)
        .arg(&output)
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn deterministic_id_incompatible_with_encryption() {
    let tmp = tempdir().expect("tempdir");
    let input = fixture_path("one-page.pdf");
    let output = tmp.path().join("out.pdf");

    // Top-level alias path: `flpdf --deterministic-id --encrypt U O 256 -- IN OUT`.
    // The writer rejects the combination with qpdf's wording before any
    // encryption work happens.
    CargoCommand::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["--deterministic-id", "--encrypt", "u", "o", "256", "--"])
        .arg(&input)
        .arg(&output)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "incompatible with encrypted output files",
        ));
}

#[test]
fn deterministic_id_conflicts_with_json() {
    // --json is dispatched before any rewrite path; without the conflict the
    // flag would be silently ignored. clap must reject the combination.
    let input = fixture_path("one-page.pdf");

    CargoCommand::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["--json", "--deterministic-id"])
        .arg(&input)
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn deterministic_id_rejected_with_linearize() {
    let tmp = tempdir().expect("tempdir");
    let input = fixture_path("one-page.pdf");
    let output = tmp.path().join("out.pdf");

    CargoCommand::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["rewrite", "--linearize", "--deterministic-id"])
        .arg(&input)
        .arg(&output)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "not yet supported for linearized output",
        ));
}

#[test]
fn deterministic_id_emits_no_testing_warning() {
    // Unlike --static-id, --deterministic-id is production-safe and must not
    // print the testing-only warning.
    let tmp = tempdir().expect("tempdir");
    let input = fixture_path("one-page.pdf");
    let output = tmp.path().join("out.pdf");

    CargoCommand::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["rewrite", "--deterministic-id"])
        .arg(&input)
        .arg(&output)
        .assert()
        .success()
        .stderr(predicate::str::contains("testing only").not());
}

#[test]
fn deterministic_id_is_honored_in_page_ops_pipeline() {
    // The page-extraction pipeline rewrites through the full-rewrite writer, so
    // --deterministic-id must produce a stable /ID there too (not silently fall
    // back to a random one). Guards against a silent regression.
    let tmp = tempdir().expect("tempdir");
    let input = fixture_path("three-page.pdf");
    let a = tmp.path().join("a.pdf");
    let b = tmp.path().join("b.pdf");

    for out in [&a, &b] {
        CargoCommand::cargo_bin("flpdf")
            .expect("flpdf binary")
            .arg("--deterministic-id")
            .arg(&input)
            .args(["--pages", ".", "1-2", "--"])
            .arg(out)
            .assert()
            .success();
    }

    let (id0, id1) = extract_id_array(&std::fs::read(&a).expect("read a"));
    // three-page.pdf carries an input /ID, so /ID[0] (preserved) and /ID[1]
    // (content digest) differ.
    assert_ne!(id0, id1, "permanent /ID[0] and changing /ID[1] must differ");
    assert_eq!(
        std::fs::read(&a).expect("read a"),
        std::fs::read(&b).expect("read b"),
        "page-ops --deterministic-id must be byte-stable across runs"
    );
}

#[test]
fn deterministic_id_top_level_alias_is_stable() {
    let tmp = tempdir().expect("tempdir");
    let input = fixture_path("one-page.pdf");
    let a = tmp.path().join("a.pdf");
    let b = tmp.path().join("b.pdf");

    for out in [&a, &b] {
        CargoCommand::cargo_bin("flpdf")
            .expect("flpdf binary")
            .arg("--deterministic-id")
            .arg(&input)
            .arg(out)
            .assert()
            .success();
    }

    assert_eq!(
        std::fs::read(&a).expect("read a"),
        std::fs::read(&b).expect("read b"),
        "two top-level --deterministic-id runs must be byte-identical"
    );
}
