//! End-to-end byte-identity: the `flpdf` CLI binary + `--overlay`/`--underlay`
//! == qpdf 11.9.0 output.
//!
//! Mirrors the library-layer `overlay::byte_gate` (in `crates/flpdf/src/overlay.rs`)
//! but runs the actual `flpdf` binary through `rewrite --static-id [--qdf
//! --no-original-object-ids] DEST --overlay SRC [--from=..] [--to=..] [--repeat=..]
//! -- OUT`. This exercises the whole CLI path — raw-argv pre-split (`extract_overlay_groups`),
//! `WriteOptions` assembly (incl. the `overlay-presence ⇒ full_rewrite=true` promotion),
//! CLI defaults (`NewlineBeforeEndstream::Never`), and the write pipeline — so a
//! divergence introduced by the CLI layer (not just the library) is caught.
//!
//! Gated on `qpdf-zlib-compat`: byte identity requires flpdf's DEFLATE to match
//! qpdf's classic-zlib output. The default (miniz_oxide) build compiles these
//! out — the only sanctioned byte deviation per the project's mimicry policy.

#![cfg(feature = "qpdf-zlib-compat")]

use assert_cmd::Command;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(name)
}

fn overlay_golden(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references/overlay")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read golden {path:?}: {e}"))
}

/// Run `flpdf rewrite --static-id [extra_head...] <dest> <argv...> <out>` and
/// return the written bytes. `argv` should terminate each overlay/underlay
/// group with `--`, mirroring the qpdf CLI shape captured in
/// `tests/golden/regenerate.sh`.
fn run_cli(extra_head: &[&str], dest: &str, argv: &[&str]) -> Vec<u8> {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = tmp.path().join("out.pdf");
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.env("FLPDF_STATIC_ID_QUIET", "1");
    cmd.arg("rewrite").arg("--static-id");
    for a in extra_head {
        cmd.arg(a);
    }
    cmd.arg(fixture(dest));
    for a in argv {
        cmd.arg(a);
    }
    cmd.arg(&out);
    cmd.assert().success();
    std::fs::read(&out).unwrap_or_else(|e| panic!("read out: {e}"))
}

fn assert_bytes(actual: &[u8], golden_name: &str) {
    let expected = overlay_golden(golden_name);
    if actual == expected {
        return;
    }
    let common = actual.len().min(expected.len());
    let off = (0..common)
        .find(|&i| actual[i] != expected[i])
        .unwrap_or(common);
    let lo = off.saturating_sub(24);
    panic!(
        "{golden_name}: CLI overlay output diverged from qpdf golden \
         (flpdf={} bytes, golden={} bytes, first diff at byte {off})\n\
         flpdf : {:?}\ngolden: {:?}",
        actual.len(),
        expected.len(),
        String::from_utf8_lossy(&actual[lo..(off + 24).min(actual.len())]),
        String::from_utf8_lossy(&expected[lo..(off + 24).min(expected.len())]),
    );
}

// ── Plain static-id: three-page dest × one-page source (identity cm) ─────────

#[test]
fn cli_three_page_overlay_one_page_is_byte_identical() {
    let src = fixture("one-page.pdf");
    let bytes = run_cli(
        &[],
        "three-page.pdf",
        &["--overlay", src.to_str().unwrap(), "--"],
    );
    assert_bytes(&bytes, "three-page-overlay-one-page.pdf");
}
