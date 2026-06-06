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
//! flpdf-9hc.7.7 additions:
//!   LZWDecode ×3 modes, DCTDecode/JBIG2Decode/JPXDecode/CCITTFaxDecode passthrough ×3 modes,
//!   LZWDecode show-stream text-readable test.
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

// ===========================================================================
// flpdf-9hc.7.7: LZWDecode × 3 modes + passthrough × 3 modes
// ===========================================================================

// ---------------------------------------------------------------------------
// Known LZW-encoded vector (copied from crates/flpdf/tests/qdf_tests.rs and
// crates/flpdf-cli/tests/cli_multi_filter_chain.rs — Rust integration tests
// are crate-isolated, so we must duplicate the constant here).
//
// LZW_ABABABABABABAB_EC1 encodes "ABABABABABABAB" with EarlyChange=1 (PDF default).
// ---------------------------------------------------------------------------

const LZW_ABABABABABABAB_EC1: &[u8] = &[
    0x80, 0x10, 0x48, 0x50, 0x28, 0x24, 0x0e, 0x0d, 0x02, 0x80, 0x80,
];

const LZW_ABABABABABABAB_PLAIN: &[u8] = b"ABABABABABABAB";

// ---------------------------------------------------------------------------
// Helper: build a minimal PDF whose stream data is supplied pre-encoded (no
// encode_stream_data applied).  Mirrors build_pdf_with_prefiltered_stream in
// cli_multi_filter_chain.rs.
//
// Object layout:
//   1 0 obj  /Catalog  -> /Pages 2 0 R
//   2 0 obj  /Pages    -> /Kids [3 0 R]
//   3 0 obj  /Page     -> /Contents 4 0 R
//   4 0 obj  stream    <- caller-supplied `encoded` bytes verbatim
//
// `filter_literal`       e.g. "/LZWDecode" or "/DCTDecode"
// `decode_parms_literal` optional, e.g. "<< /K -1 /Columns 8 >>"
// ---------------------------------------------------------------------------

fn build_pdf_with_prefiltered_stream(
    encoded: &[u8],
    filter_literal: &str,
    decode_parms_literal: Option<&str>,
) -> Vec<u8> {
    let mut pdf_bytes: Vec<u8> = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::<usize>::new();

    offsets.push(pdf_bytes.len());
    pdf_bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    offsets.push(pdf_bytes.len());
    pdf_bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    offsets.push(pdf_bytes.len());
    pdf_bytes.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Resources << >> /Contents 4 0 R >>\nendobj\n",
    );

    offsets.push(pdf_bytes.len());
    let mut stream_header = format!(
        "4 0 obj\n<< /Length {} /Filter {}",
        encoded.len(),
        filter_literal
    )
    .into_bytes();
    if let Some(parms) = decode_parms_literal {
        stream_header.extend_from_slice(b" /DecodeParms ");
        stream_header.extend_from_slice(parms.as_bytes());
    }
    stream_header.extend_from_slice(b" >>\nstream\n");
    pdf_bytes.extend_from_slice(&stream_header);
    pdf_bytes.extend_from_slice(encoded);
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
// LZWDecode × 3 modes
// ---------------------------------------------------------------------------

/// --stream-data=preserve: /Filter /LZWDecode kept, decoded payload round-trips.
#[test]
fn cli_stream_data_lzw_preserve() {
    let src = build_pdf_with_prefiltered_stream(LZW_ABABABABABABAB_EC1, "/LZWDecode", None);
    let out = rewrite_with_args(&src, &["--stream-data=preserve"]);

    let stream = extract_obj4(&out);
    assert_eq!(
        stream.dict.get("Filter"),
        Some(&Object::Name(b"LZWDecode".to_vec())),
        "--stream-data=preserve must keep /Filter /LZWDecode"
    );
    let decoded = filters::decode_stream_data(&stream.dict, &stream.data).expect("lzw decode");
    assert_eq!(
        decoded.as_slice(),
        LZW_ABABABABABABAB_PLAIN,
        "preserve: decoded payload must match the original"
    );
}

/// --stream-data=uncompress: /Filter stripped, raw LZW payload written directly.
#[test]
fn cli_stream_data_lzw_uncompress() {
    let src = build_pdf_with_prefiltered_stream(LZW_ABABABABABABAB_EC1, "/LZWDecode", None);
    let out = rewrite_with_args(&src, &["--stream-data=uncompress"]);

    let stream = extract_obj4(&out);
    assert!(
        stream.dict.get("Filter").is_none(),
        "--stream-data=uncompress must strip /Filter; got {:?}",
        stream.dict.get("Filter")
    );
    assert_eq!(
        stream.data.as_slice(),
        LZW_ABABABABABABAB_PLAIN,
        "uncompress: stream data must be the decoded LZW payload"
    );
}

/// --stream-data=compress: LZW decoded and re-encoded as FlateDecode.
#[test]
fn cli_stream_data_lzw_compress() {
    let src = build_pdf_with_prefiltered_stream(LZW_ABABABABABABAB_EC1, "/LZWDecode", None);
    let out = rewrite_with_args(&src, &["--stream-data=compress"]);

    let stream = extract_obj4(&out);
    assert_eq!(
        stream.dict.get("Filter"),
        Some(&Object::Name(b"FlateDecode".to_vec())),
        "--stream-data=compress must change /Filter to /FlateDecode"
    );
    let decoded = filters::decode_stream_data(&stream.dict, &stream.data).expect("flate decode");
    assert_eq!(
        decoded.as_slice(),
        LZW_ABABABABABABAB_PLAIN,
        "compress: FlateDecode-wrapped stream must round-trip to original LZW payload"
    );
}

// ---------------------------------------------------------------------------
// DCTDecode passthrough × 3 modes
//
// All three --stream-data modes must leave a passthrough codec intact:
// /Filter preserved, stream data byte-identical.  This is guaranteed by the
// decode-failure early-return in apply_stream_compress_policy.
//
// --remove-unreferenced-resources=no is used to prevent the resources scanner
// from trying to decode the passthrough content stream. (flpdf-s9s)
// ---------------------------------------------------------------------------

/// DCTDecode × preserve: /Filter preserved, data byte-identical.
#[test]
fn cli_stream_data_dct_preserve() {
    let fake_jpeg: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0xAA, 0xBB];
    let src = build_pdf_with_prefiltered_stream(fake_jpeg, "/DCTDecode", None);
    let out = rewrite_with_args(
        &src,
        &[
            "--stream-data=preserve",
            "--full-rewrite",
            "--remove-unreferenced-resources=no", // flpdf-s9s
        ],
    );

    let stream = extract_obj4(&out);
    assert!(
        matches!(stream.dict.get("Filter"), Some(Object::Name(n)) if n.as_slice() == b"DCTDecode"),
        "DCTDecode preserve: /Filter must remain /DCTDecode; got {:?}",
        stream.dict.get("Filter")
    );
    assert_eq!(
        stream.data.as_slice(),
        fake_jpeg,
        "DCTDecode preserve: stream data must be byte-identical"
    );
}

/// DCTDecode × uncompress: passthrough codec cannot be decoded — /Filter and
/// data must be left intact (decode-failure verbatim path in writer).
#[test]
fn cli_stream_data_dct_uncompress() {
    let fake_jpeg: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0xAA, 0xBB];
    let src = build_pdf_with_prefiltered_stream(fake_jpeg, "/DCTDecode", None);
    let out = rewrite_with_args(
        &src,
        &[
            "--stream-data=uncompress",
            "--full-rewrite",
            "--remove-unreferenced-resources=no", // flpdf-s9s
        ],
    );

    let stream = extract_obj4(&out);
    assert!(
        matches!(stream.dict.get("Filter"), Some(Object::Name(n)) if n.as_slice() == b"DCTDecode"),
        "DCTDecode uncompress: passthrough must keep /Filter /DCTDecode; got {:?}",
        stream.dict.get("Filter")
    );
    assert_eq!(
        stream.data.as_slice(),
        fake_jpeg,
        "DCTDecode uncompress: stream data must be byte-identical (passthrough intact)"
    );
}

/// DCTDecode × compress: passthrough codec cannot be decoded — /Filter and
/// data must remain intact.
#[test]
fn cli_stream_data_dct_compress() {
    let fake_jpeg: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0xAA, 0xBB];
    let src = build_pdf_with_prefiltered_stream(fake_jpeg, "/DCTDecode", None);
    let out = rewrite_with_args(
        &src,
        &[
            "--stream-data=compress",
            "--full-rewrite",
            "--remove-unreferenced-resources=no", // flpdf-s9s
        ],
    );

    let stream = extract_obj4(&out);
    assert!(
        matches!(stream.dict.get("Filter"), Some(Object::Name(n)) if n.as_slice() == b"DCTDecode"),
        "DCTDecode compress: passthrough must keep /Filter /DCTDecode; got {:?}",
        stream.dict.get("Filter")
    );
    assert_eq!(
        stream.data.as_slice(),
        fake_jpeg,
        "DCTDecode compress: stream data must be byte-identical (passthrough intact)"
    );
}

// ---------------------------------------------------------------------------
// CCITTFaxDecode passthrough × 3 modes
// ---------------------------------------------------------------------------

/// CCITTFaxDecode × preserve: intact.
#[test]
fn cli_stream_data_ccitt_preserve() {
    let fake_ccitt: &[u8] = &[0x00, 0x01, 0x02, 0x03, 0xFF, 0xFE, 0xFD, 0xFC];
    let src = build_pdf_with_prefiltered_stream(
        fake_ccitt,
        "/CCITTFaxDecode",
        Some("<< /K -1 /Columns 8 >>"),
    );
    let out = rewrite_with_args(
        &src,
        &[
            "--stream-data=preserve",
            "--full-rewrite",
            "--remove-unreferenced-resources=no", // flpdf-s9s
        ],
    );

    let stream = extract_obj4(&out);
    assert!(
        matches!(stream.dict.get("Filter"), Some(Object::Name(n)) if n.as_slice() == b"CCITTFaxDecode"),
        "CCITTFaxDecode preserve: /Filter must remain /CCITTFaxDecode; got {:?}",
        stream.dict.get("Filter")
    );
    assert_eq!(
        stream.data.as_slice(),
        fake_ccitt,
        "CCITTFaxDecode preserve: stream data must be byte-identical"
    );
}

/// CCITTFaxDecode × uncompress: passthrough intact.
#[test]
fn cli_stream_data_ccitt_uncompress() {
    let fake_ccitt: &[u8] = &[0x00, 0x01, 0x02, 0x03, 0xFF, 0xFE, 0xFD, 0xFC];
    let src = build_pdf_with_prefiltered_stream(
        fake_ccitt,
        "/CCITTFaxDecode",
        Some("<< /K -1 /Columns 8 >>"),
    );
    let out = rewrite_with_args(
        &src,
        &[
            "--stream-data=uncompress",
            "--full-rewrite",
            "--remove-unreferenced-resources=no", // flpdf-s9s
        ],
    );

    let stream = extract_obj4(&out);
    assert!(
        matches!(stream.dict.get("Filter"), Some(Object::Name(n)) if n.as_slice() == b"CCITTFaxDecode"),
        "CCITTFaxDecode uncompress: passthrough must keep /Filter; got {:?}",
        stream.dict.get("Filter")
    );
    assert_eq!(
        stream.data.as_slice(),
        fake_ccitt,
        "CCITTFaxDecode uncompress: stream data must be byte-identical"
    );
}

/// CCITTFaxDecode × compress: passthrough intact.
#[test]
fn cli_stream_data_ccitt_compress() {
    let fake_ccitt: &[u8] = &[0x00, 0x01, 0x02, 0x03, 0xFF, 0xFE, 0xFD, 0xFC];
    let src = build_pdf_with_prefiltered_stream(
        fake_ccitt,
        "/CCITTFaxDecode",
        Some("<< /K -1 /Columns 8 >>"),
    );
    let out = rewrite_with_args(
        &src,
        &[
            "--stream-data=compress",
            "--full-rewrite",
            "--remove-unreferenced-resources=no", // flpdf-s9s
        ],
    );

    let stream = extract_obj4(&out);
    assert!(
        matches!(stream.dict.get("Filter"), Some(Object::Name(n)) if n.as_slice() == b"CCITTFaxDecode"),
        "CCITTFaxDecode compress: passthrough must keep /Filter; got {:?}",
        stream.dict.get("Filter")
    );
    assert_eq!(
        stream.data.as_slice(),
        fake_ccitt,
        "CCITTFaxDecode compress: stream data must be byte-identical"
    );
}

// ---------------------------------------------------------------------------
// JBIG2Decode passthrough × 2 representative modes (uncompress + compress)
// ---------------------------------------------------------------------------

/// JBIG2Decode × uncompress: passthrough intact.
#[test]
fn cli_stream_data_jbig2_uncompress() {
    let fake_jbig2: &[u8] = &[0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
    let src = build_pdf_with_prefiltered_stream(fake_jbig2, "/JBIG2Decode", None);
    let out = rewrite_with_args(
        &src,
        &[
            "--stream-data=uncompress",
            "--full-rewrite",
            "--remove-unreferenced-resources=no", // flpdf-s9s
        ],
    );

    let stream = extract_obj4(&out);
    assert!(
        matches!(stream.dict.get("Filter"), Some(Object::Name(n)) if n.as_slice() == b"JBIG2Decode"),
        "JBIG2Decode uncompress: passthrough must keep /Filter; got {:?}",
        stream.dict.get("Filter")
    );
    assert_eq!(
        stream.data.as_slice(),
        fake_jbig2,
        "JBIG2Decode uncompress: stream data must be byte-identical"
    );
}

/// JBIG2Decode × compress: passthrough intact.
#[test]
fn cli_stream_data_jbig2_compress() {
    let fake_jbig2: &[u8] = &[0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
    let src = build_pdf_with_prefiltered_stream(fake_jbig2, "/JBIG2Decode", None);
    let out = rewrite_with_args(
        &src,
        &[
            "--stream-data=compress",
            "--full-rewrite",
            "--remove-unreferenced-resources=no", // flpdf-s9s
        ],
    );

    let stream = extract_obj4(&out);
    assert!(
        matches!(stream.dict.get("Filter"), Some(Object::Name(n)) if n.as_slice() == b"JBIG2Decode"),
        "JBIG2Decode compress: passthrough must keep /Filter; got {:?}",
        stream.dict.get("Filter")
    );
    assert_eq!(
        stream.data.as_slice(),
        fake_jbig2,
        "JBIG2Decode compress: stream data must be byte-identical"
    );
}

// ---------------------------------------------------------------------------
// JPXDecode passthrough × 2 representative modes (uncompress + compress)
// ---------------------------------------------------------------------------

/// JPXDecode × uncompress: passthrough intact.
#[test]
fn cli_stream_data_jpx_uncompress() {
    let fake_jpx: &[u8] = &[0x00, 0x00, 0x00, 0x0C, 0x6A, 0x50, 0x20, 0x20, 0xDD, 0xEE];
    let src = build_pdf_with_prefiltered_stream(fake_jpx, "/JPXDecode", None);
    let out = rewrite_with_args(
        &src,
        &[
            "--stream-data=uncompress",
            "--full-rewrite",
            "--remove-unreferenced-resources=no", // flpdf-s9s
        ],
    );

    let stream = extract_obj4(&out);
    assert!(
        matches!(stream.dict.get("Filter"), Some(Object::Name(n)) if n.as_slice() == b"JPXDecode"),
        "JPXDecode uncompress: passthrough must keep /Filter; got {:?}",
        stream.dict.get("Filter")
    );
    assert_eq!(
        stream.data.as_slice(),
        fake_jpx,
        "JPXDecode uncompress: stream data must be byte-identical"
    );
}

/// JPXDecode × compress: passthrough intact.
#[test]
fn cli_stream_data_jpx_compress() {
    let fake_jpx: &[u8] = &[0x00, 0x00, 0x00, 0x0C, 0x6A, 0x50, 0x20, 0x20, 0xDD, 0xEE];
    let src = build_pdf_with_prefiltered_stream(fake_jpx, "/JPXDecode", None);
    let out = rewrite_with_args(
        &src,
        &[
            "--stream-data=compress",
            "--full-rewrite",
            "--remove-unreferenced-resources=no", // flpdf-s9s
        ],
    );

    let stream = extract_obj4(&out);
    assert!(
        matches!(stream.dict.get("Filter"), Some(Object::Name(n)) if n.as_slice() == b"JPXDecode"),
        "JPXDecode compress: passthrough must keep /Filter; got {:?}",
        stream.dict.get("Filter")
    );
    assert_eq!(
        stream.data.as_slice(),
        fake_jpx,
        "JPXDecode compress: stream data must be byte-identical"
    );
}

// ---------------------------------------------------------------------------
// show-stream: LZWDecode → text-readable (flpdf-9hc.7.7)
//
// show-stream must decode a standalone /LZWDecode stream and emit the raw
// payload bytes (text-readable ASCII in this case).
// cli_show_stream.rs already covers the passthrough marker cases for all four
// image codecs; this adds the missing LZW-decode case.
// ---------------------------------------------------------------------------

/// show-stream decodes a /LZWDecode stream and emits text-readable bytes.
#[test]
fn cli_show_stream_lzw_text_readable() {
    let src = build_pdf_with_prefiltered_stream(LZW_ABABABABABABAB_EC1, "/LZWDecode", None);

    let temp = tempfile::tempdir().unwrap();
    let pdf_path = temp.path().join("lzw.pdf");
    std::fs::write(&pdf_path, &src).unwrap();

    let stdout = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["show-stream", "4 0", pdf_path.to_str().unwrap()])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    assert_eq!(
        stdout.as_slice(),
        LZW_ABABABABABABAB_PLAIN,
        "show-stream must decode /LZWDecode and emit text-readable payload"
    );
}
