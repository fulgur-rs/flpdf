//! Byte-identity: flpdf plain full-rewrite emits qpdf's Catalog /Extensions
//! /ADBE removal (QPDFWriter.cc L1408 whole /Extensions removal and L1432
//! /ADBE-only removal) byte-for-byte.
//!
//! Proves flpdf's broadened strip trigger (`catalog_has_extensions_adbe`)
//! matches qpdf's `have_extensions_adbe = keys.count("/ADBE") > 0`
//! (QPDFWriter.cc L1387) on inputs whose source /ADBE dict lacks a valid
//! `/ExtensionLevel` — the case the previous `adobe_extension_level() > 0`
//! gate silently passed through.
//!
//! Fixtures are content-stream-free, so byte-identity is independent of the
//! deflate backend — this file is NOT gated on `qpdf-zlib-compat`.

use flpdf::{write_pdf_with_options, NewlineBeforeEndstream, Pdf, WriteOptions};
use std::path::Path;

/// Plain full-rewrite of `fixture` with qpdf-matching option set; return bytes.
fn adbe_removal_qpdf_equivalent(fixture: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(file)).unwrap();

    let mut opts = WriteOptions::default();
    opts.full_rewrite = true;
    opts.static_id = true;
    opts.newline_before_endstream = NewlineBeforeEndstream::Never;

    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, &opts).unwrap();
    out
}

fn golden(stem: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references")
        .join(stem)
        .join("adbe-strip.pdf");
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

fn assert_parity(fixture: &str, stem: &str) {
    let actual = adbe_removal_qpdf_equivalent(fixture);
    let expected = golden(stem);
    if let Some(off) = first_diff(&actual, &expected) {
        let lo = off.saturating_sub(16);
        panic!(
            "{fixture}: not byte-identical to qpdf adbe-strip golden \
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
        "one-page-stale-adbe-no-ext",
    );
}

#[test]
fn non_adbe_prefix_preserved_when_source_adbe_lacks_extension_level_byte_identical_to_qpdf() {
    // qpdf L1432: /Extensions has /ADBE + non-ADBE prefix and we don't want
    // /ADBE → remove /ADBE key only, keep /Extensions with other keys.
    assert_parity(
        "one-page-stale-adbe-no-ext-vendor.pdf",
        "one-page-stale-adbe-no-ext-vendor",
    );
}
