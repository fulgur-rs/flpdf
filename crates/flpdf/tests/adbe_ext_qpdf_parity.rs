//! Byte-identity: flpdf plain full-rewrite emits qpdf's Catalog /Extensions
//! /ADBE mutations byte-for-byte.
//!
//! Currently covers REMOVAL (QPDFWriter.cc L1408 whole /Extensions removal,
//! L1432 /ADBE-only removal): proves `catalog_has_extensions_adbe` broadened
//! trigger matches qpdf's `have_extensions_adbe = keys.count("/ADBE") > 0`
//! (L1387) on inputs whose source /ADBE dict lacks a valid `/ExtensionLevel`.
//!
//! This file will also host INJECTION cases (`inject_adbe_extension` fired by
//! `WriteOptions::min_extension_level`, qpdf `--min-version=<v>.<ext>`); the
//! parametrised helpers below (`strip_options`, `inject_options`,
//! `write_qpdf_equivalent`, `assert_parity`) are shared between the two.
//!
//! Fixtures are content-stream-free, so byte-identity is independent of the
//! deflate backend — this file is NOT gated on `qpdf-zlib-compat`.

use flpdf::{write_pdf_with_options, NewlineBeforeEndstream, Pdf, WriteOptions};
use std::path::Path;

/// STRIP-side WriteOptions (plain full rewrite, qpdf-matching newline/id).
///
/// Field-mutation form (not struct-literal) because `WriteOptions` is
/// `#[non_exhaustive]` and E0639 blocks struct-literal construction from
/// outside the crate even with functional update.
fn strip_options() -> WriteOptions {
    let mut opts = WriteOptions::default();
    opts.full_rewrite = true;
    opts.static_id = true;
    opts.newline_before_endstream = NewlineBeforeEndstream::Never;
    opts
}

/// INJECT-side WriteOptions: strip_options() + min-version 1.7 with extension
/// level 8 (mirrors `qpdf --min-version=1.7.8`).
#[allow(dead_code)] // wired up by follow-up INJECT tests
fn inject_options() -> WriteOptions {
    let mut opts = strip_options();
    opts.min_version = Some("1.7".into());
    opts.min_extension_level = Some(8);
    opts
}

/// Plain full-rewrite of `fixture` with the given options; return bytes.
fn write_qpdf_equivalent(fixture: &str, options: &WriteOptions) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(file)).unwrap();

    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, options).unwrap();
    out
}

/// Read golden `references/<stem>/<golden_name>`.
fn golden(stem: &str, golden_name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references")
        .join(stem)
        .join(golden_name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read golden {path:?}: {e}"))
}

fn first_diff(a: &[u8], b: &[u8]) -> Option<usize> {
    if a == b {
        return None;
    }
    let common = a.len().min(b.len());
    for i in 0..common {
        if a[i] != b[i] {
            return Some(i);
        }
    }
    Some(common)
}

fn assert_parity(fixture: &str, golden_name: &str, options: &WriteOptions) {
    let stem = fixture
        .strip_suffix(".pdf")
        .expect("fixture must end in .pdf");
    let actual = write_qpdf_equivalent(fixture, options);
    let expected = golden(stem, golden_name);
    if let Some(off) = first_diff(&actual, &expected) {
        let lo = off.saturating_sub(16);
        panic!(
            "{fixture}: not byte-identical to qpdf golden {stem}/{golden_name} \
             (flpdf={} bytes, golden={} bytes, first diff at byte {off})\n\
             flpdf : {:?}\ngolden: {:?}",
            actual.len(),
            expected.len(),
            &actual[lo..(off + 16).min(actual.len())],
            &expected[lo..(off + 16).min(expected.len())],
        );
    }
}

#[test]
fn whole_extensions_removed_when_source_adbe_lacks_extension_level_byte_identical_to_qpdf() {
    // qpdf L1408: /Extensions has only /ADBE and we don't want /ADBE → drop
    // whole /Extensions from Catalog.
    assert_parity(
        "one-page-stale-adbe-no-ext.pdf",
        "adbe-strip.pdf",
        &strip_options(),
    );
}

#[test]
fn non_adbe_prefix_preserved_when_source_adbe_lacks_extension_level_byte_identical_to_qpdf() {
    // qpdf L1432: /Extensions has /ADBE + non-ADBE prefix and we don't want
    // /ADBE → remove /ADBE key only, keep /Extensions with other keys.
    assert_parity(
        "one-page-stale-adbe-no-ext-vendor.pdf",
        "adbe-strip.pdf",
        &strip_options(),
    );
}
