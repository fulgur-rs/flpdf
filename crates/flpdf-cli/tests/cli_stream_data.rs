//! CLI E2E tests for --stream-data={preserve,uncompress,compress} (flpdf-jcd.6).
//!
//! Each test builds a minimal PDF whose object `4 0 R` carries a FlateDecode
//! stream, runs `flpdf rewrite --stream-data=<mode>` against it, then re-parses
//! the output to assert the **per-mode behavioural difference** directly:
//!
//!   preserve   → /Filter /FlateDecode kept, raw bytes round-trip
//!   uncompress → /Filter absent, raw bytes appear verbatim in the stream
//!   compress   → /Filter /FlateDecode present, decoded bytes round-trip
//!   override   → --stream-data=uncompress wins over --compress-streams=y
//!
//! These assertions catch regressions where the three modes would silently
//! collapse to identical behaviour (mere "PDF is valid" would not).

use assert_cmd::Command;
use flpdf::{filters, Dictionary, Object, ObjectRef, Pdf, Stream};
use std::io::Cursor;

// ---------------------------------------------------------------------------
// Helper: build a minimal PDF with a FlateDecode-wrapped content stream.
// ---------------------------------------------------------------------------

fn build_pdf_with_flate_stream(raw: &[u8]) -> Vec<u8> {
    let mut d = Dictionary::new();
    d.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
    let encoded = filters::encode_stream_data(&d, raw).expect("encode FlateDecode stream");

    let mut pdf_bytes: Vec<u8> = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::<usize>::new();

    offsets.push(pdf_bytes.len());
    pdf_bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offsets.push(pdf_bytes.len());
    pdf_bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    offsets.push(pdf_bytes.len());
    pdf_bytes.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n",
    );

    offsets.push(pdf_bytes.len());
    let header = format!(
        "4 0 obj\n<< /Length {} /Filter /FlateDecode >>\nstream\n",
        encoded.len()
    );
    pdf_bytes.extend_from_slice(header.as_bytes());
    pdf_bytes.extend_from_slice(&encoded);
    pdf_bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let xref_offset = pdf_bytes.len();
    let n = offsets.len() + 1;
    pdf_bytes.extend_from_slice(format!("xref\n0 {n}\n").as_bytes());
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

// ---------------------------------------------------------------------------
// Helper: write `src` to a temp file, run flpdf rewrite with the given extra
// args, and return the output PDF bytes.
// ---------------------------------------------------------------------------

fn rewrite_with_args(src: &[u8], extra_args: &[&str]) -> Vec<u8> {
    let temp = tempfile::tempdir().unwrap();
    let in_path = temp.path().join("in.pdf");
    let out_path = temp.path().join("out.pdf");
    std::fs::write(&in_path, src).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    let mut args = vec!["rewrite", in_path.to_str().unwrap()];
    args.extend_from_slice(extra_args);
    args.push(out_path.to_str().unwrap());
    cmd.args(&args).assert().success();

    std::fs::read(&out_path).unwrap()
}

// ---------------------------------------------------------------------------
// Helper: extract object `4 0 R` as a Stream from an in-memory PDF.
// ---------------------------------------------------------------------------

fn extract_obj4(pdf_bytes: &[u8]) -> Stream {
    let mut pdf = Pdf::open(Cursor::new(pdf_bytes.to_vec())).expect("open output PDF");
    match pdf.resolve(ObjectRef::new(4, 0)).expect("resolve 4 0 R") {
        Object::Stream(s) => s,
        other => panic!("expected stream, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Test: --stream-data=preserve keeps /FlateDecode and original encoded bytes
// ---------------------------------------------------------------------------

#[test]
fn cli_stream_data_preserve_keeps_filter() {
    let raw = b"preserve-mode-payload: keep my /Filter /FlateDecode verbatim";
    let src = build_pdf_with_flate_stream(raw);
    let out = rewrite_with_args(&src, &["--stream-data=preserve"]);

    let stream = extract_obj4(&out);
    assert_eq!(
        stream.dict.get("Filter"),
        Some(&Object::Name(b"FlateDecode".to_vec())),
        "--stream-data=preserve must keep /Filter /FlateDecode on the output stream"
    );
    // Decode the preserved stream to confirm content is untouched (preserve
    // passes encoded bytes through verbatim; decode_stream_data round-trips).
    let decoded = filters::decode_stream_data(&stream.dict, &stream.data).expect("decode");
    assert_eq!(
        decoded.as_slice(),
        raw,
        "preserve must yield the original raw payload after decode"
    );
}

// ---------------------------------------------------------------------------
// Test: --stream-data=uncompress strips /Filter and writes raw bytes
// ---------------------------------------------------------------------------

#[test]
fn cli_stream_data_uncompress_strips_filter() {
    let raw = b"uncompress-mode-payload: my /Filter should be gone in the output";
    let src = build_pdf_with_flate_stream(raw);
    let out = rewrite_with_args(&src, &["--stream-data=uncompress"]);

    let stream = extract_obj4(&out);
    assert!(
        stream.dict.get("Filter").is_none(),
        "--stream-data=uncompress must drop /Filter; got {:?}",
        stream.dict.get("Filter")
    );
    assert_eq!(
        stream.data, raw,
        "uncompress must write the decoded raw payload directly"
    );
}

// ---------------------------------------------------------------------------
// Test: --stream-data=compress keeps /FlateDecode wrapper and round-trips data
// ---------------------------------------------------------------------------

#[test]
fn cli_stream_data_compress_wraps_with_flate() {
    let raw = b"compress-mode-payload: keep me FlateDecode-wrapped in the output";
    let src = build_pdf_with_flate_stream(raw);
    let out = rewrite_with_args(&src, &["--stream-data=compress"]);

    let stream = extract_obj4(&out);
    assert_eq!(
        stream.dict.get("Filter"),
        Some(&Object::Name(b"FlateDecode".to_vec())),
        "--stream-data=compress must keep /Filter /FlateDecode (decode→re-encode)"
    );
    let decoded = filters::decode_stream_data(&stream.dict, &stream.data).expect("decode");
    assert_eq!(
        decoded.as_slice(),
        raw,
        "compress must round-trip the original payload via FlateDecode"
    );
}

// ---------------------------------------------------------------------------
// Test: --stream-data wins over --compress-streams when both are supplied
// ---------------------------------------------------------------------------

#[test]
fn cli_stream_data_overrides_compress_streams() {
    let raw = b"override-mode-payload: --stream-data must beat --compress-streams=y";
    let src = build_pdf_with_flate_stream(raw);
    // uncompress should win over the conflicting compress-streams=y.
    let out = rewrite_with_args(&src, &["--stream-data=uncompress", "--compress-streams=y"]);

    let stream = extract_obj4(&out);
    assert!(
        stream.dict.get("Filter").is_none(),
        "--stream-data=uncompress must take precedence and strip /Filter even when \
         --compress-streams=y is also passed; got {:?}",
        stream.dict.get("Filter")
    );
    assert_eq!(
        stream.data, raw,
        "override path must still write the decoded raw payload"
    );
}
