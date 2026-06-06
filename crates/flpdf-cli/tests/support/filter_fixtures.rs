//! Shared fixtures for stream-filter CLI tests (flpdf-9hc.7.6 / .7.7).
//!
//! These helpers build minimal PDFs whose stream data is supplied **pre-encoded**,
//! bypassing `filters::encode_stream_data` — required for codecs flpdf cannot
//! encode (LZW and the image/binary passthrough codecs DCT/JBIG2/JPX/CCITT).
//!
//! Used by both `cli_multi_filter_chain.rs` and `cli_stream_data.rs` via
//! `#[path = "support/mod.rs"] mod support;` + `support::filter_fixtures::*`.

// Known LZW-encoded vector (copied from crates/flpdf/tests/qdf_tests.rs).
//
// `LZW_ABABABABABABAB_EC1` encodes "ABABABABABABAB" with EarlyChange=1 (PDF
// default). Generated and verified by an independent Python implementation.
pub const LZW_ABABABABABABAB_EC1: &[u8] = &[
    0x80, 0x10, 0x48, 0x50, 0x28, 0x24, 0x0e, 0x0d, 0x02, 0x80, 0x80,
];

/// Decoded payload of [`LZW_ABABABABABABAB_EC1`].
pub const LZW_ABABABABABABAB_PLAIN: &[u8] = b"ABABABABABABAB";

/// Build a minimal PDF whose obj-4 stream data is supplied pre-encoded (no
/// `encode_stream_data`).
///
/// Object layout:
///   1 0 obj  /Catalog  -> /Pages 2 0 R
///   2 0 obj  /Pages    -> /Kids [3 0 R]
///   3 0 obj  /Page     -> /Contents 4 0 R
///   4 0 obj  stream    <- caller-supplied `encoded` bytes verbatim
///
/// - `filter_array_literal` e.g. `"/DCTDecode"` or `"[/ASCII85Decode /LZWDecode]"`
/// - `decode_parms_literal` optional, e.g. `"<< /K -1 /Columns 8 >>"`
pub fn build_pdf_with_prefiltered_stream(
    encoded: &[u8],
    filter_array_literal: &str,
    decode_parms_literal: Option<&str>,
) -> Vec<u8> {
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
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Resources << >> /Contents 4 0 R >>\nendobj\n",
    );

    // Object 4: stream with the pre-encoded data.
    offsets.push(pdf_bytes.len());
    let mut stream_header = format!(
        "4 0 obj\n<< /Length {} /Filter {}",
        encoded.len(),
        filter_array_literal
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

    // xref + trailer
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
