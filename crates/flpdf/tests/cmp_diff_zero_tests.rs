//! Byte-identity demonstration: flpdf plain rewrite == `qpdf --static-id`.
//!
//! This is the capstone of the qpdf byte-identical roadmap. It is gated on the
//! `qpdf-zlib-compat` feature because byte-identity requires flpdf's deflate
//! output to match qpdf's classic-libz output (the Pure-Rust miniz_oxide default
//! produces equivalent but not byte-identical compression). Three independent
//! pieces must all line up:
//!
//!   1. Stream-dictionary key order — `/Length` pulled out, `/Filter` last on
//!      re-filtered streams (matches `QPDFWriter::unparseObject`).
//!   2. Trailer on the `trailer ` line with keys sorted and `/ID` last.
//!   3. No newline before `endstream` ([`NewlineBeforeEndstream::Never`]) —
//!      qpdf's default output writes exactly `/Length` bytes then `endstream`.
//!
//! plus deflate parity (this feature) and the deterministic `--static-id` trailer
//! `/ID`. With all of these, flpdf's full rewrite is `cmp`-diff-0 against the
//! committed `qpdf --static-id` golden references.
//!
//! CAVEAT: byte-identity pins to the linked libz version (captured with zlib1g
//! 1:1.3.dfsg-3.1ubuntu2.1 / qpdf 11.9.0); a different libz may shift the deflate
//! bytes and require re-blessing the goldens.

#![cfg(feature = "qpdf-zlib-compat")]

use flpdf::{write_pdf_with_options, NewlineBeforeEndstream, Pdf, WriteOptions};
use std::path::Path;

/// Full-rewrite `fixture` with the qpdf-matching option set and return the bytes.
fn rewrite_qpdf_equivalent(fixture: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(file)).unwrap();

    let mut opts = WriteOptions::default();
    opts.full_rewrite = true;
    opts.static_id = true;
    // qpdf's default output writes no newline before endstream.
    opts.newline_before_endstream = NewlineBeforeEndstream::Never;
    // compress_streams defaults to Yes (decode + re-encode to single FlateDecode).

    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, &opts).unwrap();
    out
}

fn golden(fixture_stem: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references")
        .join(fixture_stem)
        .join("static-id.pdf");
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

fn assert_cmp_diff_zero(fixture: &str, stem: &str) {
    let actual = rewrite_qpdf_equivalent(fixture);
    let expected = golden(stem);
    if let Some(off) = first_diff(&actual, &expected) {
        let lo = off.saturating_sub(16);
        panic!(
            "{fixture}: not byte-identical to qpdf --static-id golden \
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
fn one_page_plain_rewrite_is_byte_identical_to_qpdf_static_id() {
    assert_cmp_diff_zero("one-page.pdf", "one-page");
}

#[test]
fn two_page_plain_rewrite_is_byte_identical_to_qpdf_static_id() {
    assert_cmp_diff_zero("two-page.pdf", "two-page");
}

#[test]
fn three_page_plain_rewrite_is_byte_identical_to_qpdf_static_id() {
    assert_cmp_diff_zero("three-page.pdf", "three-page");
}
