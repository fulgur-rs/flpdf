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
//!   (l) qdf=true + no_original_object_ids=false: "%% Original object ID: N G"
//!       appears immediately before each "N G obj" line (≥2 objects verified).
//!   (m) qdf=true + no_original_object_ids=true: no "%% Original object ID:"
//!       lines; "N G obj" lines still present.
//!   (n) qdf=false: no "%% Original object ID:" lines regardless of flag.

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

// ─────────────────────────────────────────────────────────────────────────────
// (l) qdf=true + no_original_object_ids=false:
//     "%% Original object ID: N G" immediately precedes each "N G obj" line.
// ─────────────────────────────────────────────────────────────────────────────

/// QDF output with no_original_object_ids=false: for every indirect object at
/// number N generation G, the byte sequence
/// `%% Original object ID: N G\nN G obj\n` must appear contiguously.
/// We verify at least objects 1, 2, and 3 — the three objects that
/// `build_minimal_pdf_with_stream` always produces.
#[test]
fn qdf_original_object_id_comments_emitted_when_flag_false() {
    let raw = b"Original-object-id comment test payload.";
    let compressed = flate_encode(raw);

    let (source, _) = build_minimal_pdf_with_stream(b"FlateDecode", &compressed, None);
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.qdf = true;
    options.no_original_object_ids = false; // default, but set explicitly

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    // Helper: assert the comment+obj pair appears contiguously for (num, gen).
    let check_pair = |num: u32, gen: u16| {
        let comment_line = format!("%% Original object ID: {} {}\n", num, gen);
        let obj_line = format!("{} {} obj\n", num, gen);
        let pattern = format!("{}{}", comment_line, obj_line);
        let pattern_bytes = pattern.as_bytes();
        assert!(
            output
                .windows(pattern_bytes.len())
                .any(|w| w == pattern_bytes),
            "expected contiguous pattern {:?} in QDF output (obj {} {})",
            pattern,
            num,
            gen
        );
    };

    // Verify at least 3 distinct objects (1 0, 2 0, 3 0).
    check_pair(1, 0);
    check_pair(2, 0);
    check_pair(3, 0);

    // The output must still be a valid PDF (xref offsets point at "N G obj").
    let report = check_reader(Cursor::new(&output)).unwrap();
    assert!(
        report.valid,
        "QDF output with original-object-id comments must be valid; diagnostics: {:?}",
        report.diagnostics.entries()
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// (m) qdf=true + no_original_object_ids=true:
//     no "%% Original object ID:" lines; "N G obj" lines still present.
// ─────────────────────────────────────────────────────────────────────────────

/// When no_original_object_ids=true the comment lines must be absent, but the
/// "N G obj" header lines must still be present.
#[test]
fn qdf_original_object_id_comments_suppressed_when_flag_true() {
    let raw = b"Suppress-original-id-comment test.";
    let compressed = flate_encode(raw);

    let (source, _) = build_minimal_pdf_with_stream(b"FlateDecode", &compressed, None);
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.qdf = true;
    options.no_original_object_ids = true;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    let comment_marker = b"%% Original object ID:";
    let comment_count = output
        .windows(comment_marker.len())
        .filter(|w| *w == comment_marker)
        .count();
    assert_eq!(
        comment_count, 0,
        "qdf=true + no_original_object_ids=true must emit zero '%% Original object ID:' lines"
    );

    // "N G obj\n" lines for objects 1, 2, 3 must still be present.
    for (num, gen) in [(1u32, 0u16), (2, 0), (3, 0)] {
        let obj_line = format!("{} {} obj\n", num, gen);
        assert!(
            output
                .windows(obj_line.len())
                .any(|w| w == obj_line.as_bytes()),
            "'{num} {gen} obj' line must still be present when no_original_object_ids=true"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// (n) qdf=false: no "%% Original object ID:" lines regardless of flag value.
// ─────────────────────────────────────────────────────────────────────────────

// ─────────────────────────────────────────────────────────────────────────────
// (o) qdf=true with xref-stream source → output must use classic xref table
//     (flpdf-9hc.6.6)
// ─────────────────────────────────────────────────────────────────────────────

/// QDF mode must force a classic xref table even when the source PDF used an
/// xref stream.  The output must:
///   - contain "\nxref\n" (classic table marker)
///   - contain "\ntrailer <<\n" (classic trailer keyword; qdf format since 6.3)
///   - NOT contain "/Type /XRef" (no xref stream)
#[test]
fn qdf_mode_forces_xref_table_when_source_has_xref_stream() {
    // build_pdf_with_objstm_for_qdf() produces a PDF-1.5 xref-stream document.
    let source = build_pdf_with_objstm_for_qdf();

    // Verify the source really has an xref stream (sanity guard for this test).
    assert!(
        source
            .windows(b"/Type /XRef".len())
            .any(|w| w == b"/Type /XRef"),
        "test setup error: source fixture must use an xref stream"
    );

    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.qdf = true;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    // Classic xref table marker (leading newline avoids matching "startxref\n").
    assert!(
        output.windows(b"\nxref\n".len()).any(|w| w == b"\nxref\n"),
        "qdf=true must emit a classic xref table (\\nxref\\n) even for an xref-stream source"
    );

    // Classic (table-form) trailer keyword. Since flpdf-9hc.6.3 the qdf path
    // formats it as "trailer <<\n" (qpdf --qdf convention) rather than the
    // compact "trailer\n<<"; either way it is a classic trailer, NOT an xref
    // stream, which is what this 6.6 test asserts.
    assert!(
        output
            .windows(b"\ntrailer\n".len())
            .any(|w| w == b"\ntrailer\n"),
        "qdf=true must emit a classic trailer dict (\\ntrailer\\n) even for an xref-stream source"
    );

    // No xref stream must remain.
    assert!(
        !output
            .windows(b"/Type /XRef".len())
            .any(|w| w == b"/Type /XRef"),
        "qdf=true must not emit any /Type /XRef stream"
    );

    // Output must be structurally valid.
    let report = check_reader(Cursor::new(&output)).unwrap();
    assert!(
        report.valid,
        "qdf xref-stream→table output must be valid; diagnostics: {:?}",
        report.diagnostics.entries()
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// (p) qdf=true with classic-xref-table source → stays classic (regression guard)
//     (flpdf-9hc.6.6)
// ─────────────────────────────────────────────────────────────────────────────

/// When the source PDF already uses a classic xref table, qdf=true must keep it
/// that way — classic table in, classic table out.
#[test]
fn qdf_mode_keeps_xref_table_when_source_has_classic_table() {
    let raw = b"Classic-xref regression guard payload.";
    let compressed = flate_encode(raw);

    // build_minimal_pdf_with_stream always produces a classic xref-table PDF.
    let (source, _) = build_minimal_pdf_with_stream(b"FlateDecode", &compressed, None);

    // Sanity: source must not have an xref stream.
    assert!(
        !source
            .windows(b"/Type /XRef".len())
            .any(|w| w == b"/Type /XRef"),
        "test setup error: source fixture must use a classic xref table"
    );

    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.qdf = true;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    assert!(
        output.windows(b"\nxref\n".len()).any(|w| w == b"\nxref\n"),
        "qdf=true on a classic-table source must still emit a classic xref table"
    );
    assert!(
        output
            .windows(b"\ntrailer\n".len())
            .any(|w| w == b"\ntrailer\n"),
        "qdf=true on a classic-table source must still emit a classic trailer"
    );
    assert!(
        !output
            .windows(b"/Type /XRef".len())
            .any(|w| w == b"/Type /XRef"),
        "qdf=true on a classic-table source must not emit /Type /XRef"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// (q) qdf=true + object_streams=Generate → classic xref table, no /ObjStm
//     (flpdf-9hc.6.6 override holds even with Generate requested)
// ─────────────────────────────────────────────────────────────────────────────

/// When qdf=true and object_streams=Generate are both set, QDF wins: the output
/// must use a classic xref table and must not contain any /Type /ObjStm.
#[test]
fn qdf_mode_forces_xref_table_with_generate_override() {
    let source = build_pdf_with_objstm_for_qdf();
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.qdf = true;
    options.object_streams = ObjectStreamMode::Generate;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    // Classic xref table must be present.
    assert!(
        output.windows(b"\nxref\n".len()).any(|w| w == b"\nxref\n"),
        "qdf=true + Generate must still emit a classic xref table"
    );
    assert!(
        output
            .windows(b"\ntrailer\n".len())
            .any(|w| w == b"\ntrailer\n"),
        "qdf=true + Generate must still emit a classic trailer"
    );

    // No xref stream.
    assert!(
        !output
            .windows(b"/Type /XRef".len())
            .any(|w| w == b"/Type /XRef"),
        "qdf=true + Generate must not emit /Type /XRef"
    );

    // No ObjStm (6.2 regression guard still holds).
    let mut reopened = Pdf::open(Cursor::new(&output)).unwrap();
    for obj_ref in reopened.object_refs() {
        if let Ok(Object::Stream(s)) = reopened.resolve(obj_ref) {
            let is_objstm = matches!(
                s.dict.get("Type"),
                Some(Object::Name(n)) if n.as_slice() == b"ObjStm"
            );
            assert!(
                !is_objstm,
                "qdf=true + Generate must not emit /Type /ObjStm; found at obj {}",
                obj_ref.number
            );
        }
    }

    // Output must be valid.
    let report = check_reader(Cursor::new(&output)).unwrap();
    assert!(
        report.valid,
        "qdf+Generate xref-table output must be valid; diagnostics: {:?}",
        report.diagnostics.entries()
    );
}

/// Non-QDF output must never contain "%% Original object ID:" lines,
/// whether no_original_object_ids is true or false.
#[test]
fn non_qdf_never_emits_original_object_id_comments() {
    let raw = b"Non-QDF original-id absence test.";
    let compressed = flate_encode(raw);

    let comment_marker = b"%% Original object ID:";

    for flag in [false, true] {
        let (source, _) = build_minimal_pdf_with_stream(b"FlateDecode", &compressed, None);
        let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

        let mut options = WriteOptions::default();
        options.full_rewrite = true;
        options.qdf = false;
        options.no_original_object_ids = flag;

        let mut output = Vec::new();
        write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

        let count = output
            .windows(comment_marker.len())
            .filter(|w| *w == comment_marker)
            .count();
        assert_eq!(
            count, 0,
            "qdf=false must emit zero '%% Original object ID:' lines (no_original_object_ids={flag})"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// flpdf-9hc.6.3 — QDF output formatting normalization
//
// (o) Golden normalized-diff: flpdf qdf full-rewrite of minimal.pdf matches the
//     committed qpdf 11.9.0 --qdf reference byte-for-byte, modulo three KNOWN
//     pre-existing divergences (all outside 6.3's body/trailer-serialization
//     scope, confirmed by stashing the 6.3 diff):
//       - object 0 (`%% Original object ID: 0 65535` / `0 65535 obj` / `null`
//         / `endobj`) is emitted by flpdf but not by qpdf (object-loop /
//         framing concern — tracked as a 6.4/6.5 follow-up),
//       - the blank line qpdf inserts BETWEEN indirect objects (framing),
//       - the random `/ID` hex (qpdf emits a fresh id; flpdf static-id is the
//         pi constant — id strategy is 6.8/6.9 scope).
//     After normalizing those, the dict multi-line layout, alphabetical key
//     sort, one-element-per-line arrays, empty-container shape, and the
//     `trailer <<` block (with /ID last and inline) are byte-identical.
// (p) Property assertions on the qdf body region.
// (q) Idempotence: qdf output re-fed through qdf full-rewrite is byte-identical.
// (r) Non-qdf regression guard: qdf=false output keeps the compact
//     `<< /K v >>` single-line form (this layer changes nothing off the qdf
//     path).
// ─────────────────────────────────────────────────────────────────────────────

fn qdf_rewrite(source: &[u8]) -> Vec<u8> {
    let mut pdf = Pdf::open(Cursor::new(source.to_vec())).unwrap();
    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.qdf = true;
    options.static_id = true;
    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();
    output
}

/// Drop the pre-existing framing/id divergences so the comparison isolates
/// what flpdf-9hc.6.3 actually owns (body + trailer serialization). The
/// removed shapes are tracked by a separate bd follow-up (object-0 body +
/// inter-object blank line) and by 6.8/6.9 (/ID strategy).
fn normalize_known_divergences(bytes: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        // Strip the pre-existing object-0 null body block.
        if line == "%% Original object ID: 0 65535" {
            // skip: comment, "0 65535 obj", "null", "endobj"
            i += 4;
            continue;
        }
        // Drop blank separator lines (qpdf has them between objects, flpdf
        // does not — framing follow-up).
        if line.is_empty() {
            i += 1;
            continue;
        }
        // Normalize the random /ID array (id strategy is 6.8/6.9 scope).
        if let Some(idx) = line.find("/ID [<") {
            out.push(format!("{}/ID <NORM>", &line[..idx]));
            i += 1;
            continue;
        }
        // xref in-use/free entry: "0000000052 00000 n " — the OFFSET value
        // differs only because flpdf still emits the object-0 block (framing
        // follow-up), which shifts every byte position. 6.3 owns the body
        // serialization, not the offsets, so collapse the whole entry to a
        // single marker; the body bytes themselves are compared above.
        // Form: 10 digits, space, 5 digits, space, 'n'|'f', trailing space
        // ("0000000052 00000 n ").
        let bytes_line = line.as_bytes();
        if bytes_line.len() == 19
            && bytes_line[10] == b' '
            && bytes_line[16] == b' '
            && matches!(bytes_line[17], b'n' | b'f')
            && bytes_line[18] == b' '
            && line[..10].bytes().all(|b| b.is_ascii_digit())
            && line[11..16].bytes().all(|b| b.is_ascii_digit())
        {
            out.push("<XREF-ENTRY>".to_string());
            i += 1;
            continue;
        }
        if out.last().map(String::as_str) == Some("startxref") {
            out.push("<OFFSET>".to_string());
            i += 1;
            continue;
        }
        out.push(line.to_string());
        i += 1;
    }
    out
}

#[test]
fn qdf_golden_minimal_matches_qpdf_modulo_known_divergences() {
    let source = std::fs::read("../../tests/fixtures/minimal.pdf").unwrap();
    let golden = std::fs::read("../../tests/fixtures/qdf-golden/minimal.qdf").unwrap();

    let produced = qdf_rewrite(&source);

    let want = normalize_known_divergences(&golden);
    let got = normalize_known_divergences(&produced);

    assert_eq!(
        got,
        want,
        "flpdf qdf output diverges from the qpdf --qdf golden beyond the \
         documented pre-existing framing/id gaps.\n--- produced ---\n{}\n",
        String::from_utf8_lossy(&produced)
    );
}

// NOTE: there is intentionally no full-byte golden test for three-page.pdf.
// qpdf --qdf RENUMBERS objects sequentially; flpdf preserves the source
// object numbers (renumbering is not in flpdf-9hc.6.3's body/trailer
// serialization scope). minimal.pdf's numbering happens to align with qpdf's,
// so it is the byte-parity golden; three-page is exercised only via the
// structural property + idempotence tests below.

#[test]
fn qdf_body_formatting_properties() {
    let source = std::fs::read("../../tests/fixtures/compat/three-page.pdf").unwrap();
    let out = qdf_rewrite(&source);
    let text = String::from_utf8_lossy(&out);

    // Every dictionary opens multi-line ("<<\n", never the compact "<< /").
    assert!(
        text.contains("<<\n"),
        "qdf output must contain multi-line dictionaries"
    );
    assert!(
        !text.contains("<< /"),
        "qdf output must not contain the compact '<< /' single-line dict form"
    );
    assert!(
        !text.contains("[ "),
        "qdf output must not contain the compact '[ ' inline-array form"
    );

    // Locate the Catalog by content signature (the Catalog dict contains
    // "/Type /Catalog"), independent of which object number it received.
    // Walk back from that marker to the enclosing "<<\n", then collect the
    // entry lines up to the matching ">>".
    let type_pos = text.find("/Type /Catalog").expect("a Catalog object");
    let open = text[..type_pos].rfind("<<\n").expect("catalog dict open") + "<<\n".len();
    let close_rel = text[open..].find("\n>>").expect("catalog dict close");
    let cat_body = &text[open..open + close_rel];
    let mut keys: Vec<&str> = Vec::new();
    for line in cat_body.lines() {
        assert!(
            line.starts_with("  /"),
            "catalog entry line must be '  /'-indented, got {line:?}"
        );
        let key = line[3..].split(' ').next().unwrap();
        keys.push(key);
    }
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(
        keys, sorted,
        "catalog dict keys must be alphabetically sorted"
    );
    assert_eq!(keys, vec!["PageMode", "Pages", "Type"]);

    // Array element on its own line: every /Kids indirect-ref array must put
    // each element on its own +2-indented line and close at the dict indent.
    let kids = text.find("/Kids [\n").expect("a /Kids array");
    let kids_close = text[kids..].find("  ]\n").expect("/Kids array close") + kids;
    let kids_body = &text[kids + "/Kids [\n".len()..kids_close];
    let mut elem_lines = 0;
    for line in kids_body.lines() {
        assert!(
            line.starts_with("    ") && line.trim().ends_with(" R"),
            "each /Kids element must be a '    N G R' line, got {line:?}"
        );
        elem_lines += 1;
    }
    assert_eq!(elem_lines, 3, "three-page /Pages has three /Kids entries");

    // Trailer: "trailer <<" on one line, /ID last and INLINE.
    let tr = text.find("trailer <<\n").expect("trailer block");
    let tr_rel_end = text[tr..].find("\n>>\n").unwrap();
    let tr_block = &text[tr..tr + tr_rel_end];
    assert!(
        tr_block.contains("\n  /ID [<") && tr_block.contains(">]"),
        "trailer /ID must stay inline ([<hex><hex>]), got: {tr_block:?}"
    );
    let id_pos = tr_block.find("/ID").unwrap();
    assert!(
        tr_block[..id_pos].contains("/Root") && tr_block[..id_pos].contains("/Size"),
        "trailer /ID must be emitted last (after the sorted keys)"
    );
}

#[test]
fn qdf_empty_container_shape() {
    let source = std::fs::read("../../tests/fixtures/minimal.pdf").unwrap();
    let out = qdf_rewrite(&source);
    let text = String::from_utf8_lossy(&out);
    // minimal.pdf's /Pages has an empty /Kids array.
    assert!(
        text.contains("/Kids [\n  ]\n"),
        "empty array must render as '[\\n<indent>]' (got: {text})"
    );
}

#[test]
fn qdf_output_is_idempotent() {
    let source = std::fs::read("../../tests/fixtures/compat/three-page.pdf").unwrap();
    let once = qdf_rewrite(&source);
    let twice = qdf_rewrite(&once);
    assert_eq!(
        once, twice,
        "feeding qdf full-rewrite output back through qdf full-rewrite must \
         be byte-identical"
    );
}

#[test]
fn non_qdf_output_keeps_compact_dict_form() {
    // Regression guard: this layer must not change any non-qdf serialization.
    let source = std::fs::read("../../tests/fixtures/minimal.pdf").unwrap();
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();
    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.qdf = false;
    options.static_id = true;
    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();
    let text = String::from_utf8_lossy(&output);

    assert!(
        text.contains("<< /Pages 2 0 R /Type /Catalog >>"),
        "non-qdf full-rewrite must keep the compact single-line dict form"
    );
    assert!(
        text.contains("<< /Count 0 /Kids [ ] /Type /Pages >>"),
        "non-qdf full-rewrite must keep compact dicts and inline empty arrays"
    );
    assert!(
        text.contains("trailer\n<<"),
        "non-qdf trailer must keep the 'trailer\\n<<' compact form"
    );
}
