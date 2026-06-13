//! Byte-identity demonstration: flpdf linearized output ==
//! `qpdf --linearize --deterministic-id`.
//!
//! This pins flpdf's linearized + deterministic-`/ID` writer to the committed
//! qpdf golden references at `tests/golden/references/<stem>/linearize.pdf`
//! (produced by qpdf 11.9.0; see `tests/golden/regenerate.sh`). It is gated on
//! the `qpdf-zlib-compat` feature because byte-identity requires flpdf's deflate
//! output to match qpdf's classic-libz output (the Pure-Rust miniz_oxide default
//! produces equivalent but not byte-identical compression).
//!
//! The public-API sequence mirrors the CLI's `--linearize` path: build the
//! [`LinearizationPlan`] and [`RenumberMap`] from one handle, then re-open the
//! file so [`write_linearized`] can seek/read objects independently, write with
//! `deterministic_id` set, and `back_patch` the param-dict placeholders, `/Prev`,
//! and `/ID`.
//!
//! CAVEAT: byte-identity pins to the linked libz version (captured with zlib1g
//! 1:1.3.dfsg-3.1ubuntu2.1 / qpdf 11.9.0); a different libz may shift the deflate
//! bytes and require re-blessing the goldens.

#![cfg(feature = "qpdf-zlib-compat")]

use flpdf::linearization::{write_linearized, LinearizationPlan, RenumberMap};
use flpdf::{Pdf, WriteOptions};
use std::path::Path;

/// Linearize `fixture` via the public API (mirroring the CLI `--linearize`
/// path) and return the complete back-patched bytes.
fn flpdf_linearized(fixture: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);

    // Build the plan + renumber map from one handle.
    let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(file)).unwrap();
    let plan = LinearizationPlan::from_pdf(&mut pdf).unwrap();
    let renumber = RenumberMap::from_plan(&plan);

    // Re-open so `write_linearized` can seek/read objects independently.
    let file2 = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf2 = Pdf::open(std::io::BufReader::new(file2)).unwrap();

    let mut opts = WriteOptions::default();
    // Linearization is implied by calling `write_linearized`; this only opts in
    // to the qpdf-matching deterministic trailer `/ID`.
    opts.deterministic_id = true;

    let mut doc = write_linearized(&plan, &renumber, &mut pdf2, &opts).unwrap();
    // Back-patches the param-dict placeholders (/L, /H, /O, /E, /T, /N), /Prev,
    // and /ID with their final values.
    doc.back_patch().unwrap();
    doc.bytes
}

fn golden(fixture_stem: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references")
        .join(fixture_stem)
        .join("linearize.pdf");
    std::fs::read(&path).unwrap_or_else(|e| panic!("read golden {path:?}: {e}"))
}

/// Report the first differing byte offset for a readable failure message.
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

fn assert_linearize_byte_identical(fixture: &str, stem: &str) {
    let actual = flpdf_linearized(fixture);
    let expected = golden(stem);
    if let Some(off) = first_diff(&actual, &expected) {
        let lo = off.saturating_sub(16);
        panic!(
            "{fixture}: not byte-identical to qpdf --linearize --deterministic-id golden \
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
fn one_page_linearized_is_byte_identical_to_qpdf() {
    assert_linearize_byte_identical("one-page.pdf", "one-page");
}

#[test]
fn two_page_linearized_is_byte_identical_to_qpdf() {
    assert_linearize_byte_identical("two-page.pdf", "two-page");
}

#[test]
fn three_page_linearized_is_byte_identical_to_qpdf() {
    assert_linearize_byte_identical("three-page.pdf", "three-page");
}
