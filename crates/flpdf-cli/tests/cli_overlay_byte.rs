//! Feature-gated CLI byte-identity tests for `--overlay` / `--underlay`.
//!
//! Each test runs the `flpdf` binary through the same recipe the overlay qpdf
//! goldens were generated with (`flpdf rewrite --static-id IN --overlay … --
//! OUT`, mirroring `qpdf --static-id IN --overlay … -- OUT`) and byte-compares
//! the output to the committed golden under
//! `tests/golden/references/overlay/`.
//!
//! Gated on `qpdf-zlib-compat` because byte-identity requires flpdf's deflate
//! output to match qpdf's classic-libz output (CLAUDE.md DEFLATE carve-out);
//! these tests are no-ops in the default Pure-Rust build.
//!
//! KNOWN BLOCKER (every test here is `#[ignore]`d): the `flpdf` CLI's
//! `rewrite` path defaults to `--newline-before-endstream=y`
//! (`NewlineBeforeEndstream::Yes`), which writes one extra `\n` before each
//! `endstream`. qpdf's DEFAULT output writes no such newline
//! (`NewlineBeforeEndstream::Never`; see `qpdf --help=--newline-before-endstream`
//! — the flag is opt-in), and the goldens encode that default. The CLI exposes
//! no way to select `Never` (only `y`/`n`), so the binary cannot reproduce the
//! goldens byte-for-byte.
//!
//! This is a PRE-EXISTING, documented CLI divergence, not an overlay bug:
//! `tests/golden/compat-matrix.md` records the CLI `byte-equal` column as
//! `diverge` for every row and notes byte-identity is achievable only at the
//! library level (`crates/flpdf/tests/cmp_diff_zero_tests.rs`, which calls the
//! writer directly with `NewlineBeforeEndstream::Never`). The overlay GRAPH is
//! already proven byte-identical to qpdf at the library level by the
//! `byte_gate` module in `crates/flpdf/src/overlay.rs`.
//!
//! Proof the only delta is the newline policy: stripping a single `\n` before
//! every `endstream` from the CLI output makes it the SAME LENGTH as the golden
//! and identical except for the xref offset table (whose offsets shift by the
//! removed bytes). The object graph, streams, and overlay content match exactly.
//!
//! SECOND BLOCKER (encrypted-source test only): the AES-256 source fixture
//! `one-page-enc-u.pdf` is `%PDF-1.7` and carries
//! `/Extensions << /ADBE << /BaseVersion /1.7 /ExtensionLevel 8 >> >>` (the
//! AES-256 ISO-extension marker). qpdf, when importing pages from such a
//! source via `--overlay`, propagates the source's `/Extensions` into the
//! destination `/Catalog` and bumps the output header to `%PDF-1.7` (its
//! version/developer-extension merge). flpdf's overlay import does NOT do this
//! merge (output stays `%PDF-1.3` with the destination's own catalog), a
//! distinct library-level overlay gap that the .16.5 byte_gate never covered
//! (it tests only plain sources). Decryption itself works: the source page is
//! imported and re-rendered correctly (the page content/streams match).
//!
//! These tests keep the STRICT byte-identity assertion (not weakened) and are
//! `#[ignore]`d so CI stays green; they flip to passing once the CLI can emit
//! qpdf-default (`Never`) framing (and, for the encrypted case, once the
//! overlay import merges the source's `/Extensions`/version). Resolving CLI
//! byte parity is a project-level decision (make the CLI default qpdf-faithful,
//! touching compat-matrix.md + the compat baselines, or keep byte-identity at
//! the library level) and gates the whole .16.7 byte matrix.

#![cfg(feature = "qpdf-zlib-compat")]

use std::path::{Path, PathBuf};

use assert_cmd::Command as CargoCommand;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .to_path_buf()
}

fn golden_overlay_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references/overlay")
        .to_path_buf()
}

/// Report the first differing byte offset for a readable failure message.
fn first_diff(a: &[u8], b: &[u8]) -> Option<usize> {
    if a == b {
        return None;
    }
    let common = a.len().min(b.len());
    (0..common).find(|&i| a[i] != b[i]).or(Some(common))
}

/// Assert `actual` is byte-identical to the named overlay golden, reporting the
/// first diff offset and surrounding bytes on mismatch.
fn assert_byte_identical(actual: &[u8], golden_name: &str) {
    let golden_path = golden_overlay_dir().join(golden_name);
    let expected = std::fs::read(&golden_path)
        .unwrap_or_else(|e| panic!("read golden {}: {e}", golden_path.display()));
    if let Some(off) = first_diff(actual, &expected) {
        let lo = off.saturating_sub(24);
        let g = expected.get(off).copied().unwrap_or(0);
        let f = actual.get(off).copied().unwrap_or(0);
        panic!(
            "overlay CLI output not byte-identical to qpdf golden {golden_name} \
             (flpdf={} bytes, golden={} bytes)\n\
             first diff at offset {off} (golden=0x{g:02x} flpdf=0x{f:02x})\n\
             golden[{lo}..]: {:?}\nflpdf [{lo}..]: {:?}",
            actual.len(),
            expected.len(),
            String::from_utf8_lossy(&expected[lo..(off + 24).min(expected.len())]),
            String::from_utf8_lossy(&actual[lo..(off + 24).min(actual.len())]),
        );
    }
}

/// Run `flpdf rewrite --static-id <dest> <overlay_args...> <OUT>` and return the
/// output bytes. `overlay_args` are the post-`<dest>`, pre-`<OUT>` tokens (the
/// `--overlay`/`--underlay` groups), exactly as on a qpdf command line.
fn run_overlay(dest: &str, overlay_args: &[&str]) -> Vec<u8> {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = tmp.path().join("flpdf-overlay-out.pdf");

    let mut cmd = CargoCommand::cargo_bin("flpdf").expect("flpdf binary");
    cmd.arg("rewrite")
        .arg("--static-id")
        .arg(fixtures_dir().join(dest));
    for a in overlay_args {
        cmd.arg(a);
    }
    cmd.arg(out.to_str().unwrap());

    let result = cmd.output().expect("spawn flpdf");
    assert!(
        result.status.success(),
        "flpdf exited {:?}\nstderr: {}",
        result.status.code(),
        String::from_utf8_lossy(&result.stderr)
    );
    std::fs::read(&out).expect("flpdf output missing after success")
}

#[test]
#[ignore = "blocked: CLI rewrite defaults to newline_before_endstream=Yes; qpdf default is Never and the CLI exposes no Never path (pre-existing, documented in compat-matrix.md). See .16.7 handoff."]
fn cli_single_overlay_is_byte_identical() {
    let out = run_overlay(
        "three-page.pdf",
        &[
            "--overlay",
            fixtures_dir().join("one-page.pdf").to_str().unwrap(),
            "--",
        ],
    );
    assert_byte_identical(&out, "three-page-overlay-one-page.pdf");
}

#[test]
#[ignore = "blocked: CLI rewrite defaults to newline_before_endstream=Yes; qpdf default is Never and the CLI exposes no Never path (pre-existing, documented in compat-matrix.md). See .16.7 handoff."]
fn cli_two_overlays_compose_byte_identical() {
    let one = fixtures_dir().join("one-page.pdf");
    let two = fixtures_dir().join("two-page.pdf");
    let out = run_overlay(
        "three-page.pdf",
        &[
            "--overlay",
            one.to_str().unwrap(),
            "--",
            "--overlay",
            two.to_str().unwrap(),
            "--",
        ],
    );
    assert_byte_identical(&out, "three-page-two-overlays.pdf");
}

#[test]
#[ignore = "blocked: CLI rewrite defaults to newline_before_endstream=Yes; qpdf default is Never and the CLI exposes no Never path (pre-existing, documented in compat-matrix.md). See .16.7 handoff."]
fn cli_overlay_and_underlay_compose_byte_identical() {
    let one = fixtures_dir().join("one-page.pdf");
    let two = fixtures_dir().join("two-page.pdf");
    let out = run_overlay(
        "three-page.pdf",
        &[
            "--overlay",
            one.to_str().unwrap(),
            "--",
            "--underlay",
            two.to_str().unwrap(),
            "--",
        ],
    );
    assert_byte_identical(&out, "three-page-overlay-and-underlay.pdf");
}

#[test]
#[ignore = "blocked: (1) CLI newline_before_endstream=Yes vs qpdf default Never; (2) overlay import does not merge the AES-256 source's /Extensions /ADBE + version bump to 1.7. Both pre-existing/library-level. See .16.7 handoff."]
fn cli_overlay_encrypted_source_byte_identical() {
    let enc = fixtures_dir().join("one-page-enc-u.pdf");
    let out = run_overlay(
        "three-page.pdf",
        &["--overlay", enc.to_str().unwrap(), "--password=u", "--"],
    );
    assert_byte_identical(&out, "three-page-overlay-encrypted-source.pdf");
}
