//! End-to-end byte-identity: the `flpdf` CLI binary == qpdf 11.9.0 output.
//!
//! Unlike the library-level parity tests (`flpdf`'s `cmp_linearize_objstm_tests`,
//! which call `write_linearized` directly), these run the actual `flpdf` binary
//! through `rewrite --linearize [...]` and diff the full output bytes against the
//! committed qpdf goldens. This exercises the whole CLI path — argument parsing,
//! `WriteOptions` assembly, the write pipeline, and final framing — so a
//! divergence introduced by the CLI layer (not just the library) is caught.
//!
//! Gated on `qpdf-zlib-compat`: byte identity requires flpdf's DEFLATE to match
//! qpdf's classic-zlib output. `cargo test -p flpdf-cli --features qpdf-zlib-compat`
//! builds the binary with the zlib backend, so `cargo_bin("flpdf")` runs it.
//! Under the default (miniz_oxide) feature these tests are compiled out — the
//! only sanctioned byte deviation per the project's mimicry policy.

#![cfg(feature = "qpdf-zlib-compat")]

use assert_cmd::Command;
use std::path::{Path, PathBuf};

fn fixture(stem: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(format!("{stem}.pdf"))
}

fn golden(stem: &str, kind: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references")
        .join(stem)
        .join(format!("{kind}.pdf"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("read golden {path:?}: {e}"))
}

/// Run `flpdf rewrite --linearize <extra...> --deterministic-id <fixture> <out>`
/// through the actual binary and return the written bytes.
fn run_cli(stem: &str, extra: &[&str]) -> Vec<u8> {
    let outdir = tempfile::tempdir().unwrap();
    let out = outdir.path().join("out.pdf");
    let input = fixture(stem);

    // Pass the paths through the builder API (`Path`/`PathBuf` are `AsRef<OsStr>`)
    // rather than `to_str().unwrap()`, which would panic on a non-UTF-8 temp path.
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg("rewrite")
        .arg("--linearize")
        .args(extra)
        .arg("--deterministic-id")
        .arg(&input)
        .arg(&out)
        .assert()
        .success();

    std::fs::read(&out).unwrap_or_else(|e| panic!("read flpdf output for {stem}: {e}"))
}

/// Run `flpdf rewrite --full-rewrite --static-id <fixture> <out>` through the
/// actual binary and return the written bytes. Mirrors the library-level
/// `cmp_diff_zero_tests` but goes through the CLI so a divergence in argv
/// parsing, `WriteOptions` assembly, or defaults (e.g. `--newline-before-endstream`)
/// is caught end-to-end.
fn run_cli_full_rewrite_static_id(stem: &str) -> Vec<u8> {
    let outdir = tempfile::tempdir().unwrap();
    let out = outdir.path().join("out.pdf");
    let input = fixture(stem);

    Command::cargo_bin("flpdf")
        .unwrap()
        .arg("rewrite")
        .arg("--full-rewrite")
        .arg("--static-id")
        .arg(&input)
        .arg(&out)
        .assert()
        .success();

    std::fs::read(&out).unwrap_or_else(|e| panic!("read flpdf output for {stem}: {e}"))
}

fn assert_byte_identical(stem: &str, kind: &str, extra: &[&str]) {
    let actual = run_cli(stem, extra);
    let expected = golden(stem, kind);
    if actual == expected {
        return;
    }
    let common = actual.len().min(expected.len());
    let off = (0..common)
        .find(|&i| actual[i] != expected[i])
        .unwrap_or(common);
    let lo = off.saturating_sub(24);
    panic!(
        "{stem} ({kind}): CLI output diverged from qpdf golden \
         (flpdf={} bytes, golden={} bytes, first diff at byte {off})\n\
         flpdf : {:?}\ngolden: {:?}",
        actual.len(),
        expected.len(),
        String::from_utf8_lossy(&actual[lo..(off + 24).min(actual.len())]),
        String::from_utf8_lossy(&expected[lo..(off + 24).min(expected.len())]),
    );
}

// ── Linearized + object-streams=generate (cross-reference stream path) ────────

#[test]
fn cli_two_page_linearize_objstm_byte_identical() {
    assert_byte_identical(
        "two-page",
        "linearize-objstm",
        &["--object-streams=generate"],
    );
}

#[test]
fn cli_three_page_linearize_objstm_byte_identical() {
    assert_byte_identical(
        "three-page",
        "linearize-objstm",
        &["--object-streams=generate"],
    );
}

#[test]
fn cli_shared_stream_linearize_objstm_byte_identical() {
    assert_byte_identical(
        "shared-stream-objstm",
        "linearize-objstm",
        &["--object-streams=generate"],
    );
}

// ── Classic linearized (no object streams) ────────────────────────────────────

#[test]
fn cli_one_page_linearize_byte_identical() {
    assert_byte_identical("one-page", "linearize", &[]);
}

#[test]
fn cli_two_page_linearize_byte_identical() {
    assert_byte_identical("two-page", "linearize", &[]);
}

#[test]
fn cli_three_page_linearize_byte_identical() {
    assert_byte_identical("three-page", "linearize", &[]);
}

// ── Plain full rewrite + static-id (no linearize) ─────────────────────────────
//
// These cover the plain full-rewrite path, which uses the CLI's default
// `--newline-before-endstream=never` framing to match qpdf. The linearize tests
// above force `Never` internally regardless of the CLI default, so only these
// rows would regress if the CLI default were flipped back to `y`.

fn assert_full_rewrite_static_id_byte_identical(stem: &str) {
    let actual = run_cli_full_rewrite_static_id(stem);
    let expected = golden(stem, "static-id");
    if actual == expected {
        return;
    }
    let common = actual.len().min(expected.len());
    let off = (0..common)
        .find(|&i| actual[i] != expected[i])
        .unwrap_or(common);
    let lo = off.saturating_sub(24);
    panic!(
        "{stem} (static-id via full-rewrite): CLI output diverged from qpdf golden \
         (flpdf={} bytes, golden={} bytes, first diff at byte {off})\n\
         flpdf : {:?}\ngolden: {:?}",
        actual.len(),
        expected.len(),
        String::from_utf8_lossy(&actual[lo..(off + 24).min(actual.len())]),
        String::from_utf8_lossy(&expected[lo..(off + 24).min(expected.len())]),
    );
}

#[test]
fn cli_one_page_full_rewrite_static_id_byte_identical() {
    assert_full_rewrite_static_id_byte_identical("one-page");
}

#[test]
fn cli_two_page_full_rewrite_static_id_byte_identical() {
    assert_full_rewrite_static_id_byte_identical("two-page");
}

#[test]
fn cli_three_page_full_rewrite_static_id_byte_identical() {
    assert_full_rewrite_static_id_byte_identical("three-page");
}
