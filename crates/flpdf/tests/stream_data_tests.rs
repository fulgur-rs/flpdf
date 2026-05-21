//! Tests for the `StreamDataMode` toggle (flpdf-jcd.6).
//!
//! Covers qpdf `--stream-data={preserve,uncompress,compress}` semantics on the
//! full-rewrite path.
//!
//! Covered cases:
//!   (a) `Preserve` — FlateDecode-wrapped stream keeps its /Filter verbatim.
//!   (b) `Uncompress` — FlateDecode-wrapped stream is decoded; /Filter absent.
//!   (c) `Compress` — uncompressed stream gains /FlateDecode and shrinks.
//!   (d) `Preserve` — output passes `check_reader` (structural validity check).
//!   (e) `Preserve` — round-trip stability: second rewrite matches first rewrite.
//!   (f) Backward compat: `stream_data = None` behaves identically to
//!       the existing `compress_streams` path (no regression).

use flpdf::{
    check_reader, filters, write_pdf_with_options, CompressStreams, Dictionary, Object, ObjectRef,
    Pdf, Stream, StreamDataMode, WriteOptions,
};
use std::io::Cursor;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a minimal but valid PDF with a single content stream carrying
/// the given `raw` bytes under the given `filter` name (or no filter when `None`).
///
/// Object layout:
///   1 0 obj  /Catalog  → /Pages 2 0 R
///   2 0 obj  /Pages    → /Kids [3 0 R]
///   3 0 obj  /Page     → /Contents 4 0 R
///   4 0 obj  stream    ← our test payload
fn make_pdf_with_stream(raw: &[u8], filter: Option<&[u8]>) -> Vec<u8> {
    // Optionally encode the data.
    let (stream_bytes, filter_entry) = match filter {
        Some(f) => {
            let mut d = Dictionary::new();
            d.insert("Filter", Object::Name(f.to_vec()));
            let encoded = filters::encode_stream_data(&d, raw).expect("encode");
            (encoded, Some(f))
        }
        None => (raw.to_vec(), None),
    };

    let mut pdf_bytes: Vec<u8> = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::<usize>::new();

    // Object 1: Catalog
    offsets.push(pdf_bytes.len());
    pdf_bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    // Object 2: Pages
    offsets.push(pdf_bytes.len());
    pdf_bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    // Object 3: Page
    offsets.push(pdf_bytes.len());
    pdf_bytes.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n",
    );

    // Object 4: stream
    offsets.push(pdf_bytes.len());
    let mut stream_dict = format!("4 0 obj\n<< /Length {}", stream_bytes.len());
    if let Some(f) = filter_entry {
        stream_dict.push_str(" /Filter /");
        stream_dict.push_str(std::str::from_utf8(f).unwrap());
    }
    stream_dict.push_str(" >>\nstream\n");
    pdf_bytes.extend_from_slice(stream_dict.as_bytes());
    pdf_bytes.extend_from_slice(&stream_bytes);
    pdf_bytes.extend_from_slice(b"\nendstream\nendobj\n");

    // xref + trailer
    let xref_offset = pdf_bytes.len();
    let n = offsets.len() + 1; // include free object 0
    pdf_bytes.extend_from_slice(format!("xref\n0 {}\n", n).as_bytes());
    pdf_bytes.extend_from_slice(b"0000000000 65535 f \n");
    for o in &offsets {
        pdf_bytes.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    pdf_bytes.extend_from_slice(
        format!("trailer\n<< /Size {n} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
            .as_bytes(),
    );

    pdf_bytes
}

/// Decode stream data from `object 4 0 R` in the given PDF bytes.
fn extract_stream_obj(pdf_bytes: &[u8]) -> Stream {
    let mut pdf = Pdf::open(Cursor::new(pdf_bytes.to_vec())).expect("open");
    let obj = pdf.resolve(ObjectRef::new(4, 0)).expect("resolve 4 0 R");
    match obj {
        Object::Stream(s) => s,
        other => panic!("expected stream, got {other:?}"),
    }
}

/// Run a full rewrite with the given options.
fn full_rewrite(src: &[u8], opts: &WriteOptions) -> Vec<u8> {
    let mut pdf = Pdf::open(Cursor::new(src.to_vec())).expect("open for rewrite");
    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, opts).expect("write");
    out
}

fn base_opts() -> WriteOptions {
    let mut opts = WriteOptions::default();
    opts.full_rewrite = true;
    opts.static_id = true;
    opts
}

// ---------------------------------------------------------------------------
// (a) Preserve keeps FlateDecode filter verbatim
// ---------------------------------------------------------------------------

#[test]
fn rewrite_stream_data_preserve_keeps_filter() {
    let raw = b"Hello, preserve mode! This data will be FlateDecode-wrapped in the source.";
    let src = make_pdf_with_stream(raw, Some(b"FlateDecode"));

    let mut opts = base_opts();
    opts.stream_data = Some(StreamDataMode::Preserve);
    let out = full_rewrite(&src, &opts);

    // Object 4 0 R in the output should still carry /Filter /FlateDecode.
    let s = extract_stream_obj(&out);
    assert_eq!(
        s.dict.get("Filter"),
        Some(&Object::Name(b"FlateDecode".to_vec())),
        "Preserve mode must keep /Filter /FlateDecode intact"
    );

    // The decoded bytes must equal the original raw content.
    let decoded = filters::decode_stream_data(&s.dict, &s.data).expect("decode in preserve output");
    assert_eq!(decoded.as_slice(), raw, "decoded bytes must match original");
}

// ---------------------------------------------------------------------------
// (b) Uncompress strips FlateDecode and emits raw bytes
// ---------------------------------------------------------------------------

#[test]
fn rewrite_stream_data_uncompress_strips_filter() {
    let raw = b"Uncompress me from FlateDecode back to raw bytes.";
    let src = make_pdf_with_stream(raw, Some(b"FlateDecode"));

    let mut opts = base_opts();
    opts.stream_data = Some(StreamDataMode::Uncompress);
    let out = full_rewrite(&src, &opts);

    let s = extract_stream_obj(&out);
    assert_eq!(
        s.dict.get("Filter"),
        None,
        "Uncompress mode must strip /Filter"
    );
    assert_eq!(
        s.data.as_slice(),
        raw,
        "Uncompress mode must emit raw bytes"
    );
}

// ---------------------------------------------------------------------------
// (c) Compress wraps uncompressed input with FlateDecode and reduces size
// ---------------------------------------------------------------------------

#[test]
fn rewrite_stream_data_compress_wraps_uncompressed() {
    // Large, compressible payload: "hello world\n" repeated 200 times (~2.4 KB).
    let raw_unit = b"hello world this is a compressible stream payload line\n";
    let raw: Vec<u8> = raw_unit
        .iter()
        .cycle()
        .take(raw_unit.len() * 200)
        .cloned()
        .collect();
    assert!(
        raw.len() > 1000,
        "payload must be large enough to be compressible"
    );

    let src = make_pdf_with_stream(&raw, None); // no filter in source

    let mut opts = base_opts();
    opts.stream_data = Some(StreamDataMode::Compress);
    let out = full_rewrite(&src, &opts);

    let s = extract_stream_obj(&out);
    assert_eq!(
        s.dict.get("Filter"),
        Some(&Object::Name(b"FlateDecode".to_vec())),
        "Compress mode must add /FlateDecode to unfiltered stream"
    );

    // Compressed stream data must be smaller than raw payload.
    assert!(
        s.data.len() < raw.len(),
        "Compress mode must reduce size for highly compressible input \
         (compressed={}, raw={})",
        s.data.len(),
        raw.len()
    );

    // Round-trip: decoded bytes must equal the original.
    let decoded = filters::decode_stream_data(&s.dict, &s.data).expect("decode compressed output");
    assert_eq!(
        decoded.as_slice(),
        &raw[..],
        "round-trip must match original"
    );
}

// ---------------------------------------------------------------------------
// (d) Preserve output passes check_reader
// ---------------------------------------------------------------------------

#[test]
fn rewrite_stream_data_preserve_passes_check() {
    let raw = b"Structural validity check on preserve-mode output.";
    let src = make_pdf_with_stream(raw, Some(b"FlateDecode"));

    let mut opts = base_opts();
    opts.stream_data = Some(StreamDataMode::Preserve);
    let out = full_rewrite(&src, &opts);

    let report = check_reader(Cursor::new(out)).expect("check_reader");
    assert!(
        report.valid,
        "preserve-mode output must be structurally valid; diagnostics: {:?}",
        report.diagnostics.entries()
    );
}

// ---------------------------------------------------------------------------
// (e) Round-trip stability: preserve(preserve(x)) ≡ preserve(x) content-wise
// ---------------------------------------------------------------------------

#[test]
fn rewrite_stream_data_round_trip_preserve() {
    let raw = b"Round-trip stability test for preserve mode.";
    let src = make_pdf_with_stream(raw, Some(b"FlateDecode"));

    let mut opts = base_opts();
    opts.stream_data = Some(StreamDataMode::Preserve);

    // First rewrite.
    let out1 = full_rewrite(&src, &opts);
    // Second rewrite of the first output.
    let out2 = full_rewrite(&out1, &opts);

    // The content stream in both rewrites should decode to the same bytes.
    let s1 = extract_stream_obj(&out1);
    let s2 = extract_stream_obj(&out2);
    let d1 = filters::decode_stream_data(&s1.dict, &s1.data).expect("decode out1");
    let d2 = filters::decode_stream_data(&s2.dict, &s2.data).expect("decode out2");
    assert_eq!(d1, d2, "preserve mode must be stable across two rewrites");
}

// ---------------------------------------------------------------------------
// (f) Backward compatibility: stream_data = None behaves like compress_streams
// ---------------------------------------------------------------------------

#[test]
fn stream_data_none_falls_back_to_compress_streams() {
    let raw = b"Backward-compat: stream_data=None must defer to compress_streams.";
    let src = make_pdf_with_stream(raw, Some(b"FlateDecode"));

    // Fallback path: `stream_data = None` + `compress_streams = No` must
    // produce identical output to the explicit `stream_data = Some(Uncompress)`
    // path. Comparing them (rather than two identical configs) is what makes
    // this a regression test for the sentinel→Uncompress fallback.
    let mut opts_fallback = base_opts();
    opts_fallback.stream_data = None;
    opts_fallback.compress_streams = CompressStreams::No;
    let mut opts_explicit = base_opts();
    opts_explicit.stream_data = Some(StreamDataMode::Uncompress);
    // compress_streams must not affect the explicit path; set it to the opposite
    // value to prove --stream-data overrides --compress-streams.
    opts_explicit.compress_streams = CompressStreams::Yes;

    let out_fallback = full_rewrite(&src, &opts_fallback);
    let out_explicit = full_rewrite(&src, &opts_explicit);

    let s_fallback = extract_stream_obj(&out_fallback);
    let s_explicit = extract_stream_obj(&out_explicit);

    assert_eq!(
        s_fallback.dict.get("Filter"),
        s_explicit.dict.get("Filter"),
        "fallback (None + compress_streams=No) and explicit (Some(Uncompress)) \
         must agree on /Filter"
    );
    assert_eq!(
        s_fallback.data, s_explicit.data,
        "fallback (None + compress_streams=No) and explicit (Some(Uncompress)) \
         must agree on stream data"
    );
}
