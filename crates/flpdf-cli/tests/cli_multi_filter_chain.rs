//! CLI E2E tests for show-stream / rewrite consistency on multi-filter chains (flpdf-jcd.7).
//!
//! Exercises PDFs whose content streams use filter arrays such as:
//!   [/ASCII85Decode /FlateDecode]
//!   [/FlateDecode /ASCIIHexDecode]
//!
//! For each variant the tests verify:
//!   1. `show-stream` decodes the multi-filter content and emits the raw bytes.
//!   2. After a plain `rewrite`, `show-stream` still produces the same raw bytes.
//!   3. `rewrite --stream-data=uncompress` strips all filters and writes raw bytes.
//!   4. (when qpdf is available) qpdf can also parse the fixture without error.
//!
//! flpdf-9hc.7.6 additions:
//!   - ASCII85Decode + LZWDecode chain: decode → correct payload.
//!   - DCTDecode standalone: passthrough marker + byte-identical after rewrite.
//!   - CCITTFaxDecode standalone: passthrough marker + byte-identical after rewrite.
//!   - JBIG2Decode with /JBIG2Globals indirect ref: passthrough + ref preserved.

use assert_cmd::Command;
use flpdf::{filters, Dictionary, Object, ObjectRef, Pdf, Stream};
use std::io::Cursor;
use std::process::Command as ShellCommand;
use tempfile::tempdir;

// ---------------------------------------------------------------------------
// qpdf availability
// ---------------------------------------------------------------------------

fn is_qpdf_available() -> bool {
    ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Build a minimal PDF whose content stream uses a filter array.
//
// Object layout:
//   1 0 obj  /Catalog  -> /Pages 2 0 R
//   2 0 obj  /Pages    -> /Kids [3 0 R]
//   3 0 obj  /Page     -> /Contents 4 0 R
//   4 0 obj  stream    <- encoded with `filter_array` and optional `decode_parms`
// ---------------------------------------------------------------------------

fn build_pdf_with_filter_array(
    raw_content: &[u8],
    filter_names: &[&[u8]],
    decode_parms: Option<Object>,
) -> Vec<u8> {
    // Build dictionary describing how the stream is encoded.
    let mut stream_dict = Dictionary::new();
    stream_dict.insert(
        "Filter",
        Object::Array(
            filter_names
                .iter()
                .map(|n| Object::Name(n.to_vec()))
                .collect(),
        ),
    );
    if let Some(parms) = decode_parms {
        stream_dict.insert("DecodeParms", parms);
    }

    // encode_stream_data applies filters in reverse (encode direction).
    let encoded =
        filters::encode_stream_data(&stream_dict, raw_content).expect("encode multi-filter stream");

    // Build the filter dict text for the stream header.
    let filter_names_str: Vec<&str> = filter_names
        .iter()
        .map(|n| std::str::from_utf8(n).unwrap())
        .collect();
    let filter_array_str = format!(
        "[{}]",
        filter_names_str
            .iter()
            .map(|n| format!("/{n}"))
            .collect::<Vec<_>>()
            .join(" ")
    );

    // Build PDF bytes manually.
    let mut pdf_bytes: Vec<u8> = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::<usize>::new();

    // Object 1: Catalog
    offsets.push(pdf_bytes.len());
    pdf_bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    // Object 2: Pages
    offsets.push(pdf_bytes.len());
    pdf_bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    // Object 3: Page.
    //
    // `/Resources` is a required (inheritable) Page attribute. qpdf 11.x
    // tolerated its absence silently, but qpdf 12.x emits a `Resources is
    // missing or invalid; repairing` warning that bumps `qpdf --check` to
    // exit 3 — so the empty (but valid) resources dict is spelled out here.
    offsets.push(pdf_bytes.len());
    pdf_bytes.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Resources << >> /Contents 4 0 R >>\nendobj\n",
    );

    // Object 4: stream with the multi-filter chain.
    //
    // /DecodeParms is serialised into the stream header via Object::write_pdf
    // (rather than left only in the in-memory dict consumed by encode) so the
    // parameters are also visible to a subsequent reader/show-stream pass.
    offsets.push(pdf_bytes.len());
    let mut stream_header = format!(
        "4 0 obj\n<< /Length {} /Filter {}",
        encoded.len(),
        filter_array_str
    )
    .into_bytes();
    if let Some(parms) = stream_dict.get("DecodeParms") {
        stream_header.extend_from_slice(b" /DecodeParms ");
        parms.write_pdf(&mut stream_header);
    }
    stream_header.extend_from_slice(b" >>\nstream\n");
    pdf_bytes.extend_from_slice(&stream_header);
    pdf_bytes.extend_from_slice(&encoded);
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

// ---------------------------------------------------------------------------
// Helper: run `show-stream 4 0` against a PDF file and return stdout bytes.
// ---------------------------------------------------------------------------

fn show_stream_decoded(pdf_path: &str) -> Vec<u8> {
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["show-stream", "4 0", pdf_path])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone()
}

// ---------------------------------------------------------------------------
// Helper: rewrite a PDF with optional extra args, write to a temp file, return bytes.
// ---------------------------------------------------------------------------

fn rewrite_pdf(pdf_path: &str, extra_args: &[&str]) -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempdir().unwrap();
    let out_path = tmp.path().join("out.pdf");

    let mut args = vec!["rewrite", "--full-rewrite", pdf_path];
    args.extend_from_slice(extra_args);
    args.push(out_path.to_str().unwrap());

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(&args)
        .assert()
        .success();

    (tmp, out_path)
}

// ---------------------------------------------------------------------------
// Known LZW-encoded vector (copied from crates/flpdf/tests/qdf_tests.rs).
//
// LZW_ABABABABABABAB_EC1 encodes "ABABABABABABAB" with EarlyChange=1 (PDF default).
// Generated and verified by an independent Python implementation.
// ---------------------------------------------------------------------------

const LZW_ABABABABABABAB_EC1: &[u8] = &[
    0x80, 0x10, 0x48, 0x50, 0x28, 0x24, 0x0e, 0x0d, 0x02, 0x80, 0x80,
];

const LZW_ABABABABABABAB_PLAIN: &[u8] = b"ABABABABABABAB";

// ---------------------------------------------------------------------------
// Build a minimal PDF whose stream data is supplied pre-encoded (no encode_stream_data).
//
// Object layout:
//   1 0 obj  /Catalog  -> /Pages 2 0 R
//   2 0 obj  /Pages    -> /Kids [3 0 R]
//   3 0 obj  /Page     -> /Contents 4 0 R
//   4 0 obj  stream    <- caller-supplied `encoded` bytes verbatim
//
// `filter_array_literal`  e.g. "/DCTDecode" or "[/ASCII85Decode /LZWDecode]"
// `decode_parms_literal`  optional, e.g. "<< /K -1 /Columns 8 >>"
// ---------------------------------------------------------------------------

fn build_pdf_with_prefiltered_stream(
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

// ---------------------------------------------------------------------------
// Build a minimal PDF with a JBIG2Decode stream that has /JBIG2Globals as an
// indirect reference.
//
// Object layout:
//   1 0 obj  /Catalog
//   2 0 obj  /Pages
//   3 0 obj  /Page    -> /Contents 4 0 R
//   4 0 obj  stream   /Filter /JBIG2Decode, /DecodeParms << /JBIG2Globals 5 0 R >>
//   5 0 obj  stream   (the JBIG2 globals)
// ---------------------------------------------------------------------------

fn build_pdf_with_jbig2_globals_ref(
    jbig2_stream_data: &[u8],
    jbig2_globals_data: &[u8],
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

    // Object 4: JBIG2 stream with /JBIG2Globals 5 0 R
    offsets.push(pdf_bytes.len());
    pdf_bytes.extend_from_slice(
        format!(
            "4 0 obj\n<< /Length {} /Filter /JBIG2Decode /DecodeParms << /JBIG2Globals 5 0 R >> >>\nstream\n",
            jbig2_stream_data.len()
        )
        .as_bytes(),
    );
    pdf_bytes.extend_from_slice(jbig2_stream_data);
    pdf_bytes.extend_from_slice(b"\nendstream\nendobj\n");

    // Object 5: JBIG2 globals stream (plain, no filter)
    offsets.push(pdf_bytes.len());
    pdf_bytes.extend_from_slice(
        format!(
            "5 0 obj\n<< /Length {} >>\nstream\n",
            jbig2_globals_data.len()
        )
        .as_bytes(),
    );
    pdf_bytes.extend_from_slice(jbig2_globals_data);
    pdf_bytes.extend_from_slice(b"\nendstream\nendobj\n");

    // xref + trailer (6 entries: 0 free + objects 1-5)
    let xref_offset = pdf_bytes.len();
    let n = offsets.len() + 1; // = 6
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
// Test: ASCII85Decode + FlateDecode chain — show-stream consistency
// ---------------------------------------------------------------------------

#[test]
fn cli_show_stream_ascii85_flate_chain() {
    let raw = b"ASCII85+FlateDecode content stream for CLI consistency test";
    let pdf_bytes = build_pdf_with_filter_array(raw, &[b"ASCII85Decode", b"FlateDecode"], None);

    let tmp = tempdir().unwrap();
    let pdf_path = tmp.path().join("ascii85-flate.pdf");
    std::fs::write(&pdf_path, &pdf_bytes).unwrap();
    let pdf_path_str = pdf_path.to_str().unwrap();

    // 1. show-stream should decode and return raw bytes.
    let decoded = show_stream_decoded(pdf_path_str);
    assert_eq!(
        decoded.as_slice(),
        raw,
        "show-stream must decode ASCII85+FlateDecode chain to original bytes"
    );

    // 2. After plain rewrite, show-stream still returns the same raw bytes.
    let (_tmp2, out_path) = rewrite_pdf(pdf_path_str, &[]);
    let decoded_after_rewrite = show_stream_decoded(out_path.to_str().unwrap());
    assert_eq!(
        decoded_after_rewrite.as_slice(),
        raw,
        "show-stream after plain rewrite must still decode to original bytes"
    );

    // 3. After --stream-data=uncompress, the stream is stored raw (no filter).
    let (_tmp3, uncomp_path) = rewrite_pdf(pdf_path_str, &["--stream-data=uncompress"]);
    let decoded_uncomp = show_stream_decoded(uncomp_path.to_str().unwrap());
    assert_eq!(
        decoded_uncomp.as_slice(),
        raw,
        "show-stream after uncompress rewrite must return raw bytes"
    );

    // 4. (optional) qpdf can parse the original fixture.
    if is_qpdf_available() {
        let out = ShellCommand::new("qpdf")
            .args(["--check", pdf_path_str])
            .output()
            .expect("spawn qpdf");
        assert!(
            out.status.success(),
            "qpdf --check must pass for ASCII85+FlateDecode fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

// ---------------------------------------------------------------------------
// Test: FlateDecode + ASCIIHexDecode chain — show-stream consistency
// ---------------------------------------------------------------------------

#[test]
fn cli_show_stream_flate_ascii_hex_chain() {
    let raw = b"FlateDecode+ASCIIHexDecode content for CLI consistency test";
    let pdf_bytes = build_pdf_with_filter_array(raw, &[b"FlateDecode", b"ASCIIHexDecode"], None);

    let tmp = tempdir().unwrap();
    let pdf_path = tmp.path().join("flate-asciihex.pdf");
    std::fs::write(&pdf_path, &pdf_bytes).unwrap();
    let pdf_path_str = pdf_path.to_str().unwrap();

    // 1. show-stream decodes the chain to original raw bytes.
    let decoded = show_stream_decoded(pdf_path_str);
    assert_eq!(
        decoded.as_slice(),
        raw,
        "show-stream must decode FlateDecode+ASCIIHexDecode chain to original bytes"
    );

    // 2. After plain rewrite, show-stream still returns same raw bytes.
    let (_tmp2, out_path) = rewrite_pdf(pdf_path_str, &[]);
    let decoded_after_rewrite = show_stream_decoded(out_path.to_str().unwrap());
    assert_eq!(
        decoded_after_rewrite.as_slice(),
        raw,
        "show-stream after plain rewrite must still return original bytes"
    );

    // 3. After --stream-data=uncompress, raw bytes are directly in the stream.
    let (_tmp3, uncomp_path) = rewrite_pdf(pdf_path_str, &["--stream-data=uncompress"]);
    let decoded_uncomp = show_stream_decoded(uncomp_path.to_str().unwrap());
    assert_eq!(
        decoded_uncomp.as_slice(),
        raw,
        "show-stream after uncompress must return raw bytes"
    );

    // 4. (optional) qpdf spot-check.
    if is_qpdf_available() {
        let out = ShellCommand::new("qpdf")
            .args(["--check", pdf_path_str])
            .output()
            .expect("spawn qpdf");
        assert!(
            out.status.success(),
            "qpdf --check must pass for FlateDecode+ASCIIHexDecode fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

// ---------------------------------------------------------------------------
// Test: ASCII85Decode + LZWDecode chain — show-stream decodes to correct payload
// (flpdf-9hc.7.6)
//
// Strategy:
//   - Use the known LZW vector for "ABABABABABABAB" (EarlyChange=1, PDF default).
//   - Wrap it in ASCII85 using encode_stream_data with /Filter /ASCII85Decode
//     (encode applies the inverse = ascii85::encode).
//   - Build PDF with /Filter [/ASCII85Decode /LZWDecode] and the double-encoded bytes.
//   - show-stream must decode both layers and return the LZW payload.
// ---------------------------------------------------------------------------

#[test]
fn cli_show_stream_ascii85_lzw_chain() {
    // The LZW bytes encode "ABABABABABABAB" with EarlyChange=1 (PDF default).
    let lzw_bytes = LZW_ABABABABABABAB_EC1;
    let expected_payload = LZW_ABABABABABABAB_PLAIN;

    // ASCII85-encode the LZW bytes so we can store them as [/ASCII85Decode /LZWDecode].
    // encode_stream_data with /Filter /ASCII85Decode applies ascii85::encode internally.
    let mut ascii85_dict = Dictionary::new();
    ascii85_dict.insert("Filter", Object::Name(b"ASCII85Decode".to_vec()));
    let stored =
        filters::encode_stream_data(&ascii85_dict, lzw_bytes).expect("ascii85 encode of LZW bytes");

    // /Filter [/ASCII85Decode /LZWDecode]: decode order is ASCII85 first, then LZW.
    let pdf_bytes = build_pdf_with_prefiltered_stream(
        &stored,
        "[/ASCII85Decode /LZWDecode]",
        None, // EarlyChange=1 is the PDF default; no DecodeParms needed.
    );

    let tmp = tempdir().unwrap();
    let pdf_path = tmp.path().join("ascii85-lzw.pdf");
    std::fs::write(&pdf_path, &pdf_bytes).unwrap();
    let pdf_path_str = pdf_path.to_str().unwrap();

    // show-stream must decode ASCII85 then LZW and return the LZW payload.
    let decoded = show_stream_decoded(pdf_path_str);
    assert_eq!(
        decoded.as_slice(),
        expected_payload,
        "show-stream must decode [/ASCII85Decode /LZWDecode] chain to the original LZW payload"
    );

    // After plain rewrite, show-stream still returns the same payload.
    let (_tmp2, out_path) = rewrite_pdf(pdf_path_str, &[]);
    let decoded_after_rewrite = show_stream_decoded(out_path.to_str().unwrap());
    assert_eq!(
        decoded_after_rewrite.as_slice(),
        expected_payload,
        "show-stream after plain rewrite must still decode [/ASCII85Decode /LZWDecode] to correct payload"
    );
}

// ---------------------------------------------------------------------------
// Test: DCTDecode standalone — passthrough marker + byte-identical after rewrite
// (flpdf-9hc.7.6)
// ---------------------------------------------------------------------------

#[test]
fn cli_show_stream_dct_passthrough_marker_and_rewrite() {
    // Fake JPEG-like bytes (arbitrary binary — DCTDecode is a passthrough codec).
    let fake_jpeg: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0xAA, 0xBB, 0xCC, 0xDD];

    let pdf_bytes = build_pdf_with_prefiltered_stream(fake_jpeg, "/DCTDecode", None);

    let tmp = tempdir().unwrap();
    let pdf_path = tmp.path().join("dct.pdf");
    std::fs::write(&pdf_path, &pdf_bytes).unwrap();
    let pdf_path_str = pdf_path.to_str().unwrap();

    // (a) show-stream must emit the passthrough marker (not raw binary).
    let stdout = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["show-stream", "4 0", pdf_path_str])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let marker_str = String::from_utf8_lossy(&stdout);
    let expected_marker = format!("<binary, {} bytes, codec DCTDecode>", fake_jpeg.len());
    assert!(
        marker_str.trim() == expected_marker,
        "show-stream DCTDecode must emit passthrough marker; got: {marker_str:?}"
    );

    // (b) After full-rewrite, stream data must be byte-identical.
    // Use --remove-unreferenced-resources=no to prevent the resources scanner
    // from attempting to decode the passthrough codec content stream, which
    // would fail because DCTDecode is a passthrough codec (not decoded by flpdf).
    let (_tmp2, out_path) = rewrite_pdf(pdf_path_str, &["--remove-unreferenced-resources=no"]);
    let out_bytes = std::fs::read(&out_path).unwrap();
    let mut pdf = Pdf::open(Cursor::new(out_bytes)).unwrap();
    let obj = pdf.resolve(ObjectRef::new(4, 0)).unwrap();
    let Object::Stream(stream) = obj else {
        panic!("object 4 should be a stream after rewrite");
    };
    assert_eq!(
        stream.data.as_slice(),
        fake_jpeg,
        "DCTDecode passthrough: stream data must be byte-identical after full-rewrite"
    );
    // /Filter must still be /DCTDecode.
    assert!(
        matches!(stream.dict.get("Filter"), Some(Object::Name(n)) if n.as_slice() == b"DCTDecode"),
        "DCTDecode passthrough: /Filter must be preserved after full-rewrite"
    );
}

// ---------------------------------------------------------------------------
// Test: CCITTFaxDecode standalone — passthrough marker + byte-identical after rewrite
// (flpdf-9hc.7.6)
// ---------------------------------------------------------------------------

#[test]
fn cli_show_stream_ccitt_passthrough_marker_and_rewrite() {
    // Fake CCITT bitstream (arbitrary binary bytes).
    let fake_ccitt: &[u8] = &[0x00, 0x01, 0x02, 0x03, 0xFF, 0xFE, 0xFD, 0xFC];

    // Minimal /DecodeParms for CCITTFaxDecode: K=-1 (Group 4), Columns=8.
    let pdf_bytes = build_pdf_with_prefiltered_stream(
        fake_ccitt,
        "/CCITTFaxDecode",
        Some("<< /K -1 /Columns 8 >>"),
    );

    let tmp = tempdir().unwrap();
    let pdf_path = tmp.path().join("ccitt.pdf");
    std::fs::write(&pdf_path, &pdf_bytes).unwrap();
    let pdf_path_str = pdf_path.to_str().unwrap();

    // (a) show-stream must emit the passthrough marker.
    let stdout = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["show-stream", "4 0", pdf_path_str])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let marker_str = String::from_utf8_lossy(&stdout);
    let expected_marker = format!("<binary, {} bytes, codec CCITTFaxDecode>", fake_ccitt.len());
    assert!(
        marker_str.trim() == expected_marker,
        "show-stream CCITTFaxDecode must emit passthrough marker; got: {marker_str:?}"
    );

    // (b) After full-rewrite, stream data must be byte-identical.
    // Use --remove-unreferenced-resources=no to prevent the resources scanner
    // from attempting to decode the passthrough codec content stream.
    let (_tmp2, out_path) = rewrite_pdf(pdf_path_str, &["--remove-unreferenced-resources=no"]);
    let out_bytes = std::fs::read(&out_path).unwrap();
    let mut pdf = Pdf::open(Cursor::new(out_bytes)).unwrap();
    let obj = pdf.resolve(ObjectRef::new(4, 0)).unwrap();
    let Object::Stream(stream) = obj else {
        panic!("object 4 should be a stream after rewrite");
    };
    assert_eq!(
        stream.data.as_slice(),
        fake_ccitt,
        "CCITTFaxDecode passthrough: stream data must be byte-identical after full-rewrite"
    );
    // /Filter must still be /CCITTFaxDecode.
    assert!(
        matches!(stream.dict.get("Filter"), Some(Object::Name(n)) if n.as_slice() == b"CCITTFaxDecode"),
        "CCITTFaxDecode passthrough: /Filter must be preserved after full-rewrite"
    );
}

// ---------------------------------------------------------------------------
// Test: JBIG2Decode + /JBIG2Globals indirect reference — passthrough + ref preserved
// (flpdf-9hc.7.6)
//
// Validates review-pattern #2 (indirect reference preservation):
//   - The JBIG2 stream body is preserved verbatim (passthrough codec).
//   - /DecodeParms /JBIG2Globals is an indirect reference (5 0 R) that must
//     remain a reference in the rewritten output — not inlined or dropped.
//   - The globals stream (obj 5) must remain resolvable.
// ---------------------------------------------------------------------------

#[test]
fn cli_show_stream_jbig2_globals_indirect_ref_preserved_after_rewrite() {
    let fake_jbig2: &[u8] = &[0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
    let fake_globals: &[u8] = &[0x10, 0x20, 0x30];

    let pdf_bytes = build_pdf_with_jbig2_globals_ref(fake_jbig2, fake_globals);

    let tmp = tempdir().unwrap();
    let pdf_path = tmp.path().join("jbig2.pdf");
    std::fs::write(&pdf_path, &pdf_bytes).unwrap();
    let pdf_path_str = pdf_path.to_str().unwrap();

    // (a) show-stream must emit the passthrough marker for JBIG2Decode.
    let stdout = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["show-stream", "4 0", pdf_path_str])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let marker_str = String::from_utf8_lossy(&stdout);
    let expected_marker = format!("<binary, {} bytes, codec JBIG2Decode>", fake_jbig2.len());
    assert!(
        marker_str.trim() == expected_marker,
        "show-stream JBIG2Decode must emit passthrough marker; got: {marker_str:?}"
    );

    // (b) After full-rewrite, stream body must be verbatim and /JBIG2Globals ref preserved.
    // Use --remove-unreferenced-resources=no to prevent the resources scanner
    // from attempting to decode the passthrough codec content stream.
    let (_tmp2, out_path) = rewrite_pdf(pdf_path_str, &["--remove-unreferenced-resources=no"]);
    let out_bytes = std::fs::read(&out_path).unwrap();

    // Re-open and resolve obj 4 (the JBIG2 stream).
    let mut pdf = Pdf::open(Cursor::new(&out_bytes)).unwrap();
    let obj = pdf.resolve(ObjectRef::new(4, 0)).unwrap();
    let Object::Stream(stream) = obj else {
        panic!("object 4 should be a stream after rewrite");
    };

    // Payload byte-identical.
    assert_eq!(
        stream.data.as_slice(),
        fake_jbig2,
        "JBIG2Decode passthrough: stream data must be byte-identical after full-rewrite"
    );

    // /Filter must still be /JBIG2Decode.
    assert!(
        matches!(stream.dict.get("Filter"), Some(Object::Name(n)) if n.as_slice() == b"JBIG2Decode"),
        "JBIG2Decode passthrough: /Filter must be preserved after full-rewrite"
    );

    // /DecodeParms /JBIG2Globals must be an indirect reference (Object::Reference),
    // not inlined as a stream value. This is review-pattern #2 validation.
    let decode_parms = stream
        .dict
        .get("DecodeParms")
        .unwrap_or_else(|| panic!("JBIG2Decode: /DecodeParms must be present after full-rewrite"));
    let parms_dict = match decode_parms {
        Object::Dictionary(d) => d,
        other => panic!(
            "JBIG2Decode: /DecodeParms must be a dictionary, got {:?}",
            other
        ),
    };
    let globals_val = parms_dict.get("JBIG2Globals").unwrap_or_else(|| {
        panic!("JBIG2Decode: /DecodeParms /JBIG2Globals must be present after full-rewrite")
    });
    assert!(
        matches!(globals_val, Object::Reference(_)),
        "JBIG2Decode: /JBIG2Globals must remain an indirect reference (not inlined) after full-rewrite; \
         got: {:?}",
        globals_val
    );

    // The globals stream (obj 5) must still be resolvable (reference is not dangling).
    let mut pdf2 = Pdf::open(Cursor::new(&out_bytes)).unwrap();
    let globals_obj = pdf2
        .resolve(ObjectRef::new(5, 0))
        .unwrap_or_else(|e| panic!("JBIG2Globals obj 5 must be resolvable after rewrite: {e}"));
    assert!(
        matches!(globals_obj, Object::Stream(_)),
        "JBIG2Globals obj 5 must be a stream after full-rewrite; got: {:?}",
        globals_obj
    );
}

// ---------------------------------------------------------------------------
// Suppress unused-import warning for Stream (used in JBIG2 test via type check).
// ---------------------------------------------------------------------------
fn _use_stream_type(_: Stream) {}
