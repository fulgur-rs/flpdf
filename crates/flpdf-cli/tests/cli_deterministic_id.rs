//! `flpdf rewrite --deterministic-id` CLI tests.
//!
//! Pins the wiring of qpdf's `--deterministic-id`:
//! - two runs over the same input are byte-identical (self-stable /ID);
//! - the /ID depends on the input content and is not the `--static-id`
//!   constant;
//! - the qpdf-incompatible combinations are rejected (+ `--static-id`,
//!   + `--encrypt`, + `--linearize`);
//! - unlike `--static-id`, the flag is production-safe and emits no
//!   testing-only warning.
//!
//! The exact digest algorithm (/ID == MD5 over the body up to the xref table)
//! is verified by the library test
//! `writer::tests::deterministic_id_is_stable_and_equals_md5_over_body`. Byte
//! parity with qpdf's `--deterministic-id` holds only under the
//! `qpdf-zlib-compat` build feature (the default Pure-Rust build diverges in
//! compressed-stream bytes), so no qpdf `/ID`-equality oracle runs here.

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
fn deterministic_id_is_content_dependent_and_not_static_constant() {
    let tmp = tempdir().expect("tempdir");
    let out1 = tmp.path().join("one.pdf");
    let out2 = tmp.path().join("two.pdf");

    run_rewrite_det_id(&fixture_path("one-page.pdf"), &out1);
    run_rewrite_det_id(&fixture_path("two-page.pdf"), &out2);

    let (id0_1, id1_1) = extract_id_array(&std::fs::read(&out1).expect("read one"));
    let (id0_2, _) = extract_id_array(&std::fs::read(&out2).expect("read two"));

    assert_eq!(id0_1, id1_1, "both /ID elements must equal the digest");
    assert_ne!(
        id0_1, id0_2,
        "different input content must yield a different deterministic /ID"
    );
    assert_ne!(
        id1_1.as_slice(),
        b"31415926535897932384626433832795",
        "deterministic /ID must not be the --static-id constant"
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
