//! flpdf preserves an already-lone-`/FlateDecode` stream verbatim under the
//! default compress policy, matching qpdf (which does not recompress a lone
//! Flate stream unless `--recompress-flate` is given). These assert behavior
//! against the SOURCE bytes, so they need no deflate-backend feature: a preserve
//! is a verbatim copy, and a re-encode (the pre-fix behavior) produces different
//! bytes at flpdf's compression level than the level-9 source.

use flpdf::linearization::{write_linearized, LinearizationPlan, RenumberMap};
use flpdf::{write_pdf_with_options, CompressStreams, NewlineBeforeEndstream, Pdf, WriteOptions};
use std::path::Path;

const FIXTURE: &str = "lone-flate-l9.pdf";

fn fixture_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(FIXTURE)
}

/// Return the bytes of the largest `stream ... endstream` payload — the page
/// content stream in this single-page fixture.
fn largest_stream_payload(data: &[u8]) -> Vec<u8> {
    let needle = b"stream\n";
    let mut best: Vec<u8> = Vec::new();
    let mut i = 0usize;
    while let Some(rel) = data[i..].windows(needle.len()).position(|w| w == needle) {
        let s = i + rel + needle.len();
        let e = s + data[s..]
            .windows(b"endstream".len())
            .position(|w| w == b"endstream")
            .expect("endstream must follow stream");
        if e - s > best.len() {
            best = data[s..e].to_vec();
        }
        i = e + b"endstream".len();
    }
    best
}

fn source_payload() -> Vec<u8> {
    largest_stream_payload(&std::fs::read(fixture_path()).unwrap())
}

fn plain_rewrite(opts: WriteOptions) -> Vec<u8> {
    let mut pdf = Pdf::open(std::io::BufReader::new(
        std::fs::File::open(fixture_path()).unwrap(),
    ))
    .unwrap();
    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, &opts).unwrap();
    out
}

fn base_opts() -> WriteOptions {
    let mut opts = WriteOptions::default();
    opts.full_rewrite = true;
    opts.static_id = true;
    opts.newline_before_endstream = NewlineBeforeEndstream::Never;
    opts
}

#[test]
fn plain_full_rewrite_preserves_lone_flate_verbatim() {
    let out = plain_rewrite(base_opts());
    assert_eq!(
        largest_stream_payload(&out),
        source_payload(),
        "default compress policy must preserve a lone /FlateDecode stream verbatim"
    );
}

#[test]
fn linearized_preserves_lone_flate_verbatim() {
    let mut pdf = Pdf::open(std::io::BufReader::new(
        std::fs::File::open(fixture_path()).unwrap(),
    ))
    .unwrap();
    let plan = LinearizationPlan::from_pdf(&mut pdf, false).unwrap();
    let renumber = RenumberMap::from_plan(&plan);
    let mut pdf2 = Pdf::open(std::io::BufReader::new(
        std::fs::File::open(fixture_path()).unwrap(),
    ))
    .unwrap();
    let mut opts = WriteOptions::default();
    opts.deterministic_id = true;
    opts.newline_before_endstream = NewlineBeforeEndstream::Never;
    let mut doc = write_linearized(&plan, &renumber, &mut pdf2, &opts).unwrap();
    doc.back_patch().unwrap();
    assert_eq!(
        largest_stream_payload(&doc.bytes),
        source_payload(),
        "linearized output must preserve a lone /FlateDecode stream verbatim"
    );
}

#[test]
fn uncompress_policy_decodes_lone_flate() {
    // The preserve gate is CompressStreams::Yes-specific: under Uncompress the
    // lone /FlateDecode must be decoded (no /Filter), not preserved.
    let mut opts = base_opts();
    opts.compress_streams = CompressStreams::No;
    let out = plain_rewrite(opts);
    let payload = largest_stream_payload(&out);
    // Decoded content is the raw rectangle operators ("re f" ops), much larger
    // than the ~1974-byte compressed source and not equal to it.
    assert_ne!(payload, source_payload());
    assert!(
        payload.windows(4).any(|w| w == b"re f"),
        "Uncompress must emit decoded raw content, not preserved compressed bytes"
    );
}

#[test]
fn recompress_flate_reencodes_lone_flate() {
    let mut opts = base_opts();
    opts.recompress_flate = true;
    let out = plain_rewrite(opts);
    let payload = largest_stream_payload(&out);
    assert_ne!(
        payload,
        source_payload(),
        "recompress_flate=true must re-encode the lone /FlateDecode stream"
    );
    // It must still be a single /FlateDecode (re-encoded), not raw bytes.
    assert!(
        out.windows(b"/Filter /FlateDecode".len())
            .any(|w| w == b"/Filter /FlateDecode"),
        "re-encoded stream must still declare a single /FlateDecode filter"
    );
}
