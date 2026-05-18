//! Tests for QDF stream decompression (flpdf-9hc.6.1) and ObjStm
//! decomposition (flpdf-9hc.6.2).
//!
//! Covers:
//!   (a) LZWDecode unit test — known-vector decode with EarlyChange=1 (default).
//!   (b) LZWDecode unit test — known-vector decode with EarlyChange=0.
//!   (c) LZWDecode of a ClearCode-only / empty-body stream.
//!   (d) qdf=true full-rewrite: text stream (FlateDecode) decoded and /Filter
//!       absent in the output.
//!   (e) qdf=true full-rewrite: image stream (DCTDecode) passed through verbatim.
//!   (f) qdf=true full-rewrite: /Length matches the decoded byte count.
//!   (g) qdf=true full-rewrite: round-trip — re-writing the QDF output via
//!       full-rewrite recovers byte-identical decoded content.
//!   (h) qdf=true full-rewrite: LZWDecode stream decoded and /Filter absent.
//!   (j) qdf=true: ObjStm decomposition — output has no /Type /ObjStm,
//!       formerly-compressed objects appear as plain indirect.
//!   (k) qdf=true + object_streams=Generate: qdf overrides Generate, no ObjStm.

use flpdf::{
    check_reader, filters, write_pdf_with_options, CompressStreams, Dictionary, Object, ObjectRef,
    ObjectStreamMode, Pdf, Stream, WriteOptions,
};
use std::io::Cursor;

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

fn lzw_decode_raw(data: &[u8], early_change: bool) -> Vec<u8> {
    let mut dict = Dictionary::new();
    dict.insert("Filter", Object::Name(b"LZWDecode".to_vec()));
    if !early_change {
        let mut params = Dictionary::new();
        params.insert("EarlyChange", Object::Integer(0));
        dict.insert("DecodeParms", Object::Dictionary(params));
    }
    filters::decode_stream_data(&dict, data).expect("LZWDecode should succeed")
}

/// Build a minimal in-memory PDF with an explicit stream object.
///
/// Returns `(pdf_bytes, obj3_raw_payload)` where obj3 is a stream whose filter
/// chain and compressed data are caller-supplied.
fn build_minimal_pdf_with_stream(
    filter_name: &[u8],
    stream_data: &[u8],
    decode_parms: Option<&str>,
) -> (Vec<u8>, Vec<u8>) {
    let length = stream_data.len();
    let decode_parms_entry = decode_parms.unwrap_or("");

    let mut bytes = b"%PDF-1.4\n".to_vec();

    let cat_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    let pages_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");

    let stream_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "3 0 obj\n<< /Filter /{filter} /Length {length}{parms} >>\nstream\n",
            filter = std::str::from_utf8(filter_name).unwrap(),
            parms = decode_parms_entry,
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(stream_data);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 4\n");
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{cat_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{pages_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{stream_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(b"trailer\n<< /Size 4 /Root 1 0 R >>\n");
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

    (bytes, stream_data.to_vec())
}

/// Helper: FlateDecode-compress `raw`.
fn flate_encode(raw: &[u8]) -> Vec<u8> {
    let mut d = Dictionary::new();
    d.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
    filters::encode_stream_data(&d, raw).expect("flate encode")
}

// ─────────────────────────────────────────────────────────────────────────────
// (a) LZWDecode unit tests — known vectors
// ─────────────────────────────────────────────────────────────────────────────

/// Known LZW-encoded vector for "ABABABABABABAB" with EarlyChange=1 (PDF default).
/// Generated and verified by an independent Python implementation.
const LZW_ABABABABABABAB_EC1: &[u8] = &[
    0x80, 0x10, 0x48, 0x50, 0x28, 0x24, 0x0e, 0x0d, 0x02, 0x80, 0x80,
];

/// Known LZW-encoded vector for "A" with EarlyChange=1.
const LZW_A_EC1: &[u8] = &[0x80, 0x10, 0x60, 0x20];

/// Known LZW-encoded vector for empty input (ClearCode + EOD only).
const LZW_EMPTY_EC1: &[u8] = &[0x80, 0x40, 0x40];

/// Known LZW-encoded vector for "ABABABABABAB" with EarlyChange=0.
const LZW_ABABABABAB_EC0: &[u8] = &[0x80, 0x10, 0x48, 0x50, 0x28, 0x24, 0x0e, 0x0d, 0x01];

#[test]
fn lzw_decode_abab_early_change_default() {
    let decoded = lzw_decode_raw(LZW_ABABABABABABAB_EC1, /*early_change=*/ true);
    assert_eq!(
        decoded, b"ABABABABABABAB",
        "LZWDecode (EarlyChange=1): decoded bytes must match known plaintext"
    );
}

#[test]
fn lzw_decode_single_byte_a() {
    let decoded = lzw_decode_raw(LZW_A_EC1, true);
    assert_eq!(
        decoded, b"A",
        "LZWDecode: single-byte input must decode correctly"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// (b) LZWDecode EarlyChange=0
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn lzw_decode_early_change_zero() {
    let decoded = lzw_decode_raw(LZW_ABABABABAB_EC0, /*early_change=*/ false);
    assert_eq!(
        decoded, b"ABABABABABAB",
        "LZWDecode (EarlyChange=0): decoded bytes must match known plaintext"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// (c) LZWDecode — empty body
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn lzw_decode_empty_input() {
    let decoded = lzw_decode_raw(LZW_EMPTY_EC1, true);
    assert_eq!(
        decoded, b"",
        "LZWDecode: ClearCode+EOD stream must decode to empty output"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// (d) qdf=true: FlateDecode stream decoded, /Filter absent
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn qdf_mode_strips_filter_from_flate_stream() {
    let raw = b"Human-readable QDF stream content.";
    let compressed = flate_encode(raw);

    let (source, _) = build_minimal_pdf_with_stream(b"FlateDecode", &compressed, None);
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.qdf = true;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    let mut reopened = Pdf::open(Cursor::new(&output)).unwrap();
    let obj = reopened.resolve(ObjectRef::new(3, 0)).unwrap();
    let Object::Stream(stream) = obj else {
        panic!("object 3 should be a stream");
    };

    // /Filter must be absent in QDF output.
    assert_eq!(
        stream.dict.get("Filter"),
        None,
        "qdf=true must strip /FlateDecode from text streams"
    );

    // /DecodeParms must also be absent.
    assert_eq!(
        stream.dict.get("DecodeParms"),
        None,
        "qdf=true must strip /DecodeParms"
    );

    // Data must be the decoded (raw) bytes.
    assert_eq!(
        stream.data.as_slice(),
        raw,
        "qdf=true: stream data must be the decoded (human-readable) bytes"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// (e) qdf=true: DCTDecode (image codec) passed through verbatim
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn qdf_mode_keeps_dct_stream_verbatim() {
    // Fake JPEG-like bytes: flpdf cannot decode DCTDecode, so the stream
    // must be passed through verbatim with /Filter preserved.
    let fake_jpeg: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0xAA, 0xBB, 0xCC];

    let (source, _) = build_minimal_pdf_with_stream(b"DCTDecode", fake_jpeg, None);
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.qdf = true;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    let mut reopened = Pdf::open(Cursor::new(&output)).unwrap();
    let obj = reopened.resolve(ObjectRef::new(3, 0)).unwrap();
    let Object::Stream(stream) = obj else {
        panic!("object 3 should be a stream");
    };

    // /Filter must still be /DCTDecode (verbatim pass-through).
    assert_eq!(
        stream.dict.get("Filter"),
        Some(&Object::Name(b"DCTDecode".to_vec())),
        "qdf=true must preserve /DCTDecode on image streams"
    );

    // Compressed data bytes must be unchanged.
    assert_eq!(
        stream.data.as_slice(),
        fake_jpeg,
        "qdf=true: DCTDecode image data must be bit-for-bit unchanged"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// (f) qdf=true: /Length matches decoded byte count
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn qdf_mode_length_matches_decoded_bytes() {
    let raw = b"Length-check payload for QDF mode.";
    let compressed = flate_encode(raw);

    let (source, _) = build_minimal_pdf_with_stream(b"FlateDecode", &compressed, None);
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.qdf = true;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    let mut reopened = Pdf::open(Cursor::new(&output)).unwrap();
    let obj = reopened.resolve(ObjectRef::new(3, 0)).unwrap();
    let Object::Stream(stream) = obj else {
        panic!("object 3 should be a stream");
    };

    let declared_length = match stream.dict.get("Length") {
        Some(Object::Integer(n)) => *n as usize,
        other => panic!("expected /Length integer, got {other:?}"),
    };

    assert_eq!(
        declared_length,
        raw.len(),
        "/Length must equal the decoded (raw) byte count in QDF output"
    );
    assert_eq!(
        stream.data.len(),
        raw.len(),
        "actual stream data length must also equal the decoded byte count"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// (g) qdf=true: round-trip — re-write of QDF output preserves content
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn qdf_mode_round_trip_content_preserved() {
    let raw = b"Round-trip content must survive QDF-then-rewrite.";
    let compressed = flate_encode(raw);

    let (source, _) = build_minimal_pdf_with_stream(b"FlateDecode", &compressed, None);
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    // First pass: QDF rewrite.
    let mut qdf_options = WriteOptions::default();
    qdf_options.full_rewrite = true;
    qdf_options.qdf = true;
    let mut qdf_output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut qdf_output, &qdf_options).unwrap();

    // Second pass: full-rewrite (CompressStreams::Yes) of the QDF output.
    let mut pdf2 = Pdf::open(Cursor::new(qdf_output)).unwrap();
    let mut compress_options = WriteOptions::default();
    compress_options.full_rewrite = true;
    compress_options.compress_streams = CompressStreams::Yes;
    let mut final_output = Vec::new();
    write_pdf_with_options(&mut pdf2, &mut final_output, &compress_options).unwrap();

    // Re-open and decode stream 3 — content must match original.
    let mut pdf3 = Pdf::open(Cursor::new(final_output)).unwrap();
    let obj = pdf3.resolve(ObjectRef::new(3, 0)).unwrap();
    let Object::Stream(stream) = obj else {
        panic!("object 3 should be a stream after second rewrite");
    };
    let decoded =
        filters::decode_stream_data(&stream.dict, &stream.data).expect("second-pass decode");
    assert_eq!(
        decoded.as_slice(),
        raw,
        "round-trip (QDF + rewrite) must recover original stream bytes"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// (h) qdf=true: LZWDecode stream decoded and /Filter absent
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn qdf_mode_strips_filter_from_lzw_stream() {
    // Use the known LZW-encoded vector for "ABABABABABABAB".
    let lzw_data = LZW_ABABABABABABAB_EC1;
    let expected_plain = b"ABABABABABABAB";

    let (source, _) = build_minimal_pdf_with_stream(b"LZWDecode", lzw_data, None);
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.qdf = true;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    let mut reopened = Pdf::open(Cursor::new(&output)).unwrap();
    let obj = reopened.resolve(ObjectRef::new(3, 0)).unwrap();
    let Object::Stream(stream) = obj else {
        panic!("object 3 should be a stream");
    };

    // /Filter must be absent after QDF decode.
    assert_eq!(
        stream.dict.get("Filter"),
        None,
        "qdf=true must strip /LZWDecode from text streams"
    );

    // Data must be the decoded bytes.
    assert_eq!(
        stream.data.as_slice(),
        expected_plain,
        "qdf=true: LZWDecode stream data must be the decoded bytes"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// (i) LZWDecode via apply_stream_compress_policy directly
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn apply_stream_compress_no_decodes_lzw() {
    use flpdf::apply_stream_compress_policy;

    let lzw_data = LZW_ABABABABABABAB_EC1.to_vec();
    let mut dict = Dictionary::new();
    dict.insert("Filter", Object::Name(b"LZWDecode".to_vec()));
    dict.insert("Length", Object::Integer(lzw_data.len() as i64));
    let stream = Stream::new(dict, lzw_data);

    let result = apply_stream_compress_policy(&stream, CompressStreams::No);
    let Object::Stream(out) = result else {
        panic!("expected Object::Stream from apply_stream_compress_policy");
    };

    // /Filter must be stripped.
    assert_eq!(
        out.dict.get("Filter"),
        None,
        "CompressStreams::No must strip /LZWDecode"
    );

    // Data must be decoded.
    assert_eq!(
        out.data.as_slice(),
        b"ABABABABABABAB",
        "CompressStreams::No must produce decoded bytes for LZWDecode"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// ObjStm decomposition helpers (flpdf-9hc.6.2)
// ─────────────────────────────────────────────────────────────────────────────

/// Build a zlib-compressed ObjStm payload from (object-number, raw-bytes) pairs.
fn build_objstm_payload_6_2(members: &[(u32, &[u8])]) -> (Vec<u8>, usize) {
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;

    let mut header = String::new();
    let mut body = Vec::new();
    for (index, (number, object_data)) in members.iter().enumerate() {
        let offset = body.len();
        header.push_str(&format!("{} {} ", number, offset));
        body.extend_from_slice(object_data);
        if index + 1 < members.len() {
            body.push(b'\n');
        }
    }
    let mut decoded = Vec::new();
    decoded.extend_from_slice(header.as_bytes());
    decoded.extend_from_slice(&body);

    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(&decoded).unwrap();
    let encoded = enc.finish().unwrap();
    (encoded, header.len())
}

fn append_u24_be_6_2(bytes: &mut Vec<u8>, value: u32) {
    let b = value.to_be_bytes();
    bytes.extend_from_slice(&b[1..]);
}

fn append_xref_entry_6_2(entries: &mut Vec<u8>, entry_type: u8, field1: u32, field2: u8) {
    entries.push(entry_type);
    append_u24_be_6_2(entries, field1);
    entries.push(field2);
}

/// Build a minimal xref-stream PDF that has one ObjStm containing obj 2 (Pages).
///
///   0 free
///   1 Catalog (plain indirect)
///   2 Pages   (compressed in ObjStm 3, index 0)
///   3 ObjStm
///   4 XRef stream
fn build_pdf_with_objstm_for_qdf() -> Vec<u8> {
    let objstm_num: u32 = 3;
    let xref_num: u32 = 4;
    let total_size: u32 = xref_num + 1;

    let mut bytes = b"%PDF-1.5\n".to_vec();

    let catalog_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    let pages_bytes: &[u8] = b"<< /Type /Pages /Count 0 /Kids [] >>";
    let (stream_data, first) = build_objstm_payload_6_2(&[(2, pages_bytes)]);
    let n_members: u32 = 1;

    let objstm_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "{objstm_num} 0 obj\n<< /Type /ObjStm /N {n_members} /First {first} /Length {} /Filter /FlateDecode >>\nstream\n",
            stream_data.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&stream_data);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let xref_offset = bytes.len();

    let mut xref_entries: Vec<u8> = Vec::new();
    append_xref_entry_6_2(&mut xref_entries, 0, 0, 0);
    append_xref_entry_6_2(&mut xref_entries, 1, catalog_offset as u32, 0);
    append_xref_entry_6_2(&mut xref_entries, 2, objstm_num, 0); // Pages in ObjStm
    append_xref_entry_6_2(&mut xref_entries, 1, objstm_offset as u32, 0);
    append_xref_entry_6_2(&mut xref_entries, 1, xref_offset as u32, 0);

    bytes.extend_from_slice(
        format!(
            "{xref_num} 0 obj\n<< /Type /XRef /Size {total_size} /Root 1 0 R /W [1 3 1] /Index [0 {total_size}] /Length {} >>\nstream\n",
            xref_entries.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
    bytes
}

// ─────────────────────────────────────────────────────────────────────────────
// (j) qdf=true: ObjStm decomposition
// ─────────────────────────────────────────────────────────────────────────────

/// qdf=true on an ObjStm-containing PDF must produce output with:
///   - no /Type /ObjStm objects
///   - formerly-compressed object (Pages, obj 2) present as plain indirect
///   - output is a valid PDF
#[test]
fn qdf_mode_decomposes_objstm_no_objstm_in_output() {
    let source = build_pdf_with_objstm_for_qdf();
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.qdf = true;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    // Output must be a structurally valid PDF.
    let report = check_reader(Cursor::new(&output)).unwrap();
    assert!(
        report.valid,
        "qdf ObjStm-decompose output must be valid; diagnostics: {:?}",
        report.diagnostics.entries()
    );

    // No /Type /ObjStm must exist in the output.
    let mut reopened = Pdf::open(Cursor::new(&output)).unwrap();
    for obj_ref in reopened.object_refs() {
        if let Ok(Object::Stream(s)) = reopened.resolve(obj_ref) {
            let is_objstm = matches!(
                s.dict.get("Type"),
                Some(Object::Name(n)) if n.as_slice() == b"ObjStm"
            );
            assert!(
                !is_objstm,
                "qdf=true must not emit any /Type /ObjStm; found one at obj {}",
                obj_ref.number
            );
        }
    }

    // Object 2 (originally inside the ObjStm) must be resolvable with its
    // original number and must be the Pages dict.
    let mut reopened2 = Pdf::open(Cursor::new(&output)).unwrap();
    let pages = reopened2.resolve(ObjectRef::new(2, 0)).unwrap();
    match &pages {
        Object::Dictionary(d) => {
            assert_eq!(
                d.get("Type"),
                Some(&Object::Name(b"Pages".to_vec())),
                "object 2 must be the Pages dict after ObjStm decomposition"
            );
        }
        other => panic!("object 2 should be a Dictionary, got {:?}", other),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// (l) %QDF-1.0 header marker (flpdf-9hc.6.4)
// ─────────────────────────────────────────────────────────────────────────────

/// QDF output must contain "%QDF-1.0\n" immediately after the binary marker line.
#[test]
fn qdf_header_contains_qdf_marker() {
    let raw = b"QDF header marker test payload.";
    let compressed = flate_encode(raw);

    let (source, _) = build_minimal_pdf_with_stream(b"FlateDecode", &compressed, None);
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.qdf = true;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    // Must contain "%QDF-1.0" somewhere in the output.
    assert!(
        output.windows(b"%QDF-1.0".len()).any(|w| w == b"%QDF-1.0"),
        "qdf=true must emit %QDF-1.0 in the output"
    );

    // Verify exact layout: %PDF-x.y\n  → binary marker → %QDF-1.0\n → \n
    // Split into lines to check the sequence.
    let header_area = &output[..output.len().min(128)];
    let mut lines = header_area.split(|&b| b == b'\n');
    let line1 = lines.next().expect("line 1 (%PDF-...)");
    assert!(
        line1.starts_with(b"%PDF-"),
        "line 1 must be the %PDF- version line, got: {:?}",
        line1
    );
    let line2 = lines.next().expect("line 2 (binary marker)");
    assert_eq!(
        line2, b"%\xbf\xf7\xa2\xfe",
        "line 2 must be the binary marker %BF F7 A2 FE (without the newline)"
    );
    let line3 = lines.next().expect("line 3 (%QDF-1.0)");
    assert_eq!(
        line3, b"%QDF-1.0",
        "line 3 must be %QDF-1.0 immediately after the binary marker"
    );
    let line4 = lines.next().expect("line 4 (blank line)");
    assert_eq!(line4, b"", "line 4 must be a blank line after %QDF-1.0");
}

/// Non-QDF output must NOT contain "%QDF-1.0" but still has the binary marker.
#[test]
fn non_qdf_header_has_no_qdf_marker_but_has_binary_marker() {
    let raw = b"Non-QDF header test payload.";
    let compressed = flate_encode(raw);

    let (source, _) = build_minimal_pdf_with_stream(b"FlateDecode", &compressed, None);
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.qdf = false;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    // Must NOT contain "%QDF-1.0".
    assert!(
        !output.windows(b"%QDF-1.0".len()).any(|w| w == b"%QDF-1.0"),
        "qdf=false must NOT emit %QDF-1.0"
    );

    // Must still contain the binary marker.
    let marker = b"%\xbf\xf7\xa2\xfe";
    assert!(
        output.windows(marker.len()).any(|w| w == marker),
        "non-QDF output must still contain the binary marker %BF F7 A2 FE"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// (k) qdf=true + object_streams=Generate → qdf overrides Generate, no ObjStm
// ─────────────────────────────────────────────────────────────────────────────

/// When both qdf=true and object_streams=Generate are set, qdf wins:
/// the output must not contain any /Type /ObjStm.
#[test]
fn qdf_overrides_generate_mode_no_objstm() {
    let source = build_pdf_with_objstm_for_qdf();
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.qdf = true;
    options.object_streams = ObjectStreamMode::Generate;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    // Output must be valid.
    let report = check_reader(Cursor::new(&output)).unwrap();
    assert!(
        report.valid,
        "qdf+Generate output must be valid; diagnostics: {:?}",
        report.diagnostics.entries()
    );

    // qdf must override Generate — no /Type /ObjStm in output.
    let mut reopened = Pdf::open(Cursor::new(&output)).unwrap();
    for obj_ref in reopened.object_refs() {
        if let Ok(Object::Stream(s)) = reopened.resolve(obj_ref) {
            let is_objstm = matches!(
                s.dict.get("Type"),
                Some(Object::Name(n)) if n.as_slice() == b"ObjStm"
            );
            assert!(
                !is_objstm,
                "qdf=true must override Generate and emit no /Type /ObjStm; found one at obj {}",
                obj_ref.number
            );
        }
    }

    // Object 2 must still be resolvable as the Pages dict.
    let mut reopened2 = Pdf::open(Cursor::new(&output)).unwrap();
    let pages = reopened2.resolve(ObjectRef::new(2, 0)).unwrap();
    match &pages {
        Object::Dictionary(d) => {
            assert_eq!(
                d.get("Type"),
                Some(&Object::Name(b"Pages".to_vec())),
                "object 2 (Pages) must be resolvable after qdf+Generate rewrite"
            );
        }
        other => panic!("object 2 should be a Dictionary, got {:?}", other),
    }
}
