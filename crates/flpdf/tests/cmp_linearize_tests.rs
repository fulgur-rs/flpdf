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
use flpdf::{write_pdf_with_options, NewlineBeforeEndstream, Pdf, WriteOptions};
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
    // qpdf's default output writes no newline before endstream; the linearized
    // body content streams honour this option (see the plain-path sibling in
    // `cmp_diff_zero_tests`).
    opts.newline_before_endstream = NewlineBeforeEndstream::Never;

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

// --------------------------------------------------------------------------
// Structural byte-parity (flpdf-9hc.13.10): the full-file byte-identity tests
// above now subsume these — flpdf reproduces qpdf's deterministic `/ID[1]` by
// digesting a byte-identical reconstruction of qpdf's *first* write pass (empty
// parameter dict, no hint stream, unresolved first-page xref; see
// `QPDFWriter::writeLinearized` → `computeDeterministicIDData`). These tests are
// kept as a narrower diagnostic: they zero the `/ID[1]` hex run on both sides
// before comparing, so a regression in the structural layout (object numbering,
// physical order, trailers, framing) is isolated from a regression in the
// `/ID[1]` digest, giving a more targeted failure than the strict tests.
// --------------------------------------------------------------------------

/// Replace the 32 hex bytes of every `/ID [<id0><id1>]` array's *second*
/// element with ASCII `'0'`, leaving the permanent identifier `/ID[0]` and all
/// surrounding bytes intact. Used to compare structural layout independently of
/// the deterministic changing identifier.
fn mask_id1(buf: &[u8]) -> Vec<u8> {
    let mut out = buf.to_vec();
    let needle = b" /ID [<";
    let mut i = 0usize;
    while let Some(rel) = find(&out[i..], needle) {
        let arr = i + rel + needle.len();
        // arr -> first hex digit of id0; id0 is 32 hex then '>', then '<', then id1.
        let id0_end = arr + 32;
        // Expect '>' then '<' then 32 id1 hex.
        if id0_end + 2 + 32 <= out.len() && out[id0_end] == b'>' && out[id0_end + 1] == b'<' {
            let id1 = id0_end + 2;
            for b in &mut out[id1..id1 + 32] {
                *b = b'0';
            }
            i = id1 + 32;
        } else {
            i = arr;
        }
    }
    out
}

fn assert_linearize_structurally_byte_identical(fixture: &str, stem: &str) {
    let actual = mask_id1(&flpdf_linearized(fixture));
    let expected = mask_id1(&golden(stem));
    if let Some(off) = first_diff(&actual, &expected) {
        let lo = off.saturating_sub(16);
        panic!(
            "{fixture}: structural layout diverged from qpdf golden (ignoring /ID[1])              (flpdf={} bytes, golden={} bytes, first diff at byte {off})\n             flpdf : {:?}\ngolden: {:?}",
            actual.len(),
            expected.len(),
            &actual[lo..(off + 16).min(actual.len())],
            &expected[lo..(off + 16).min(expected.len())],
        );
    }
}

#[test]
fn one_page_linearized_structurally_byte_identical_to_qpdf() {
    assert_linearize_structurally_byte_identical("one-page.pdf", "one-page");
}

#[test]
fn two_page_linearized_structurally_byte_identical_to_qpdf() {
    assert_linearize_structurally_byte_identical("two-page.pdf", "two-page");
}

#[test]
fn three_page_linearized_structurally_byte_identical_to_qpdf() {
    assert_linearize_structurally_byte_identical("three-page.pdf", "three-page");
}

// --------------------------------------------------------------------------
// Body content-stream recompression parity (flpdf-9hc.13.10, divergence A).
//
// The linearized writer used to clone each body stream's dict + raw data
// verbatim, preserving e.g. an `[/ASCII85Decode /FlateDecode]` source chain,
// whereas qpdf (and flpdf's plain rewrite) decode the chain and re-encode to a
// single `/FlateDecode`. These tests pin the recompression independently of
// object NUMBERING and physical PLACEMENT (separate divergences), so they pass
// even while the full-file byte-parity tests above are still red.
// --------------------------------------------------------------------------

/// Full-rewrite (non-linearized) one-page.pdf under qpdf-equivalent options:
/// the plain path that already byte-matches `qpdf --static-id`.
fn plain_rewrite_one_page() -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join("one-page.pdf");
    let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(file)).unwrap();

    let mut opts = WriteOptions::default();
    opts.full_rewrite = true;
    opts.deterministic_id = true;
    // Same framing the linearized path uses, so the content-stream bytes line up.
    opts.newline_before_endstream = NewlineBeforeEndstream::Never;

    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, &opts).unwrap();
    out
}

/// Extract the page content-stream object body — the single-`/FlateDecode`
/// stream whose dict has neither `/Type` (excludes xref/ObjStm/metadata) nor
/// `/S` (excludes the linearization hint stream) — returning
/// `(dict_bytes, payload_bytes)`. The payload is the `/Length` bytes between
/// the `stream` EOL and `endstream`.
fn content_stream_object(data: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let needle = b"stream\n";
    let mut found: Option<(Vec<u8>, Vec<u8>)> = None;
    let mut i = 0usize;
    while let Some(rel) = find(&data[i..], needle) {
        let stream_kw = i + rel; // position of "stream\n"
                                 // Walk back to the dict opener "<<" on a preceding `N G obj\n` line.
        let dict_open = rfind(&data[..stream_kw], b"<<").expect("stream must follow a dict");
        let dict_close = stream_kw - 1; // the byte before "stream\n" is '\n' after ">>"
        let dict = &data[dict_open..dict_close];
        let payload_start = stream_kw + needle.len();
        let es =
            find(&data[payload_start..], b"endstream").expect("endstream must follow a stream");
        let payload_end = payload_start + es;
        let payload = &data[payload_start..payload_end];
        let is_lone_flate = find(dict, b"/Filter /FlateDecode").is_some();
        let has_type = find(dict, b"/Type").is_some();
        let is_hint = find(dict, b"/S ").is_some();
        if is_lone_flate && !has_type && !is_hint {
            assert!(
                found.is_none(),
                "expected exactly one page content stream in one-page output"
            );
            found = Some((dict.to_vec(), payload.to_vec()));
        }
        i = payload_end + b"endstream".len();
    }
    found.expect("a single-FlateDecode page content stream must be present")
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).rposition(|w| w == needle)
}

#[test]
fn one_page_linearized_content_stream_is_recompressed_single_flate() {
    let lin = flpdf_linearized("one-page.pdf");
    let (dict, payload) = content_stream_object(&lin);

    // Recompressed: a single `/FlateDecode`, NOT the source `[/ASCII85Decode
    // /FlateDecode]` chain, and serialized in qpdf's stream-dict key order
    // (`/Length` then a regenerated `/Filter`).
    assert_eq!(
        dict,
        b"<< /Length 82 /Filter /FlateDecode >>",
        "linearized page content stream must be re-encoded to a single \
         /FlateDecode in qpdf key order; got {:?}",
        String::from_utf8_lossy(&dict)
    );
    assert!(
        find(&dict, b"ASCII85").is_none(),
        "the ASCII85 source filter must be dropped on recompression"
    );
    assert_eq!(payload.len(), 82, "/Length must equal the on-disk payload");
}

#[test]
fn one_page_linearized_content_stream_equals_plain_and_qpdf_golden() {
    let lin = flpdf_linearized("one-page.pdf");
    let plain = plain_rewrite_one_page();
    let qpdf = golden("one-page");

    let (lin_dict, lin_payload) = content_stream_object(&lin);
    let (plain_dict, plain_payload) = content_stream_object(&plain);
    let (qpdf_dict, qpdf_payload) = content_stream_object(&qpdf);

    // The recompressed linearized content-stream object must be byte-identical
    // to the plain-path object and to qpdf's golden obj9 — dict AND payload.
    assert_eq!(
        (&lin_dict, &lin_payload),
        (&plain_dict, &plain_payload),
        "linearized content stream must equal the plain-path content stream"
    );
    assert_eq!(
        (&lin_dict, &lin_payload),
        (&qpdf_dict, &qpdf_payload),
        "linearized content stream must equal qpdf golden obj9"
    );
}
