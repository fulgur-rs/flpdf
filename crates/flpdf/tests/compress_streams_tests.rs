//! Tests for the `CompressStreams` toggle (flpdf-9hc.12.5).
//!
//! Covers:
//!   (a) `CompressStreams::Yes`  — raw stream (no /Filter) gets FlateDecode added;
//!       round-trip decodes back to original bytes.
//!   (b) `CompressStreams::No`   — FlateDecode stream gets decoded to raw;
//!       /Filter is absent in the output; round-trip matches original.
//!   (c) `CompressStreams::No`   — DCTDecode stream (not decodable by flpdf)
//!       is passed through verbatim; /Filter is still DCTDecode.
//!   (d) `CompressStreams::Yes`  — stream already carrying /FlateDecode is
//!       NOT double-compressed; the output has exactly one /FlateDecode level.
//!
//! These tests operate at the `apply_stream_compress_policy` API level.
//! End-to-end / CLI round-trip tests are the responsibility of flpdf-9hc.12.8.

use flpdf::{apply_stream_compress_policy, filters, CompressStreams, Dictionary, Object, Stream};

/// Helper: build a `Stream` with the given dict entries and raw data.
fn make_stream(filter: Option<&[u8]>, data: Vec<u8>) -> Stream {
    let mut dict = Dictionary::new();
    if let Some(f) = filter {
        dict.insert("Filter", Object::Name(f.to_vec()));
    }
    dict.insert(
        "Length",
        Object::Integer(i64::try_from(data.len()).unwrap()),
    );
    Stream::new(dict, data)
}

/// Helper: FlateDecode-encode `raw` bytes.
fn flate_encode(raw: &[u8]) -> Vec<u8> {
    let mut d = Dictionary::new();
    d.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
    filters::encode_stream_data(&d, raw).expect("flate encode")
}

// ---------------------------------------------------------------------------
// (a) Yes: raw stream (no /Filter) → FlateDecode added, round-trip correct
// ---------------------------------------------------------------------------

#[test]
fn compress_yes_adds_flate_to_unfiltered_stream() {
    let raw = b"Hello, flpdf compress streams test! This is raw payload data.";
    let stream = make_stream(None, raw.to_vec());

    let result = apply_stream_compress_policy(&stream, CompressStreams::Yes);
    let Object::Stream(out) = result else {
        panic!("expected Object::Stream");
    };

    // /Filter must be /FlateDecode.
    assert_eq!(
        out.dict.get("Filter"),
        Some(&Object::Name(b"FlateDecode".to_vec())),
        "Yes mode must add /FlateDecode to a raw stream"
    );

    // /Length must be consistent with the actual data length.
    assert_eq!(
        out.dict.get("Length"),
        Some(&Object::Integer(out.data.len() as i64)),
        "/Length must match actual compressed data length"
    );

    // Round-trip: decode must recover the original bytes.
    let decoded =
        filters::decode_stream_data(&out.dict, &out.data).expect("FlateDecode round-trip decode");
    assert_eq!(
        decoded, raw,
        "Yes mode: decoded output must equal original raw bytes"
    );
}

// ---------------------------------------------------------------------------
// (b) No: FlateDecode stream → raw output, /Filter absent, data correct
// ---------------------------------------------------------------------------

#[test]
fn compress_no_removes_filter_from_flate_stream() {
    let raw = b"Round-trip me through FlateDecode, then strip it back.";
    let compressed = flate_encode(raw);
    let stream = make_stream(Some(b"FlateDecode"), compressed);

    let result = apply_stream_compress_policy(&stream, CompressStreams::No);
    let Object::Stream(out) = result else {
        panic!("expected Object::Stream");
    };

    // /Filter must be absent.
    assert_eq!(
        out.dict.get("Filter"),
        None,
        "No mode must strip /FlateDecode"
    );

    // /DecodeParms must be absent.
    assert_eq!(
        out.dict.get("DecodeParms"),
        None,
        "No mode must strip /DecodeParms"
    );

    // /Length must match the raw data length.
    assert_eq!(
        out.dict.get("Length"),
        Some(&Object::Integer(raw.len() as i64)),
        "/Length must match raw decoded data length"
    );

    // The data bytes must equal the original raw content.
    assert_eq!(
        out.data.as_slice(),
        raw,
        "No mode: output data must equal the originally compressed payload"
    );

    // Round-trip: a PDF reader with no /Filter sees data as-is.
    let decoded = filters::decode_stream_data(&out.dict, &out.data).expect("unfiltered decode");
    assert_eq!(
        decoded, raw,
        "No mode: round-trip must recover original data"
    );
}

// ---------------------------------------------------------------------------
// (c) No: DCTDecode stream (not decodable) → verbatim pass-through
// ---------------------------------------------------------------------------

#[test]
fn compress_no_passthrough_dct_stream_verbatim() {
    // Fabricate a stream that declares /DCTDecode but whose data is not
    // actually valid JPEG.  flpdf cannot decode DCTDecode, so
    // `decode_stream_data` will return Err, and the stream must be returned
    // unchanged (dict + data verbatim).
    let fake_jpeg = b"\xff\xd8\xff\xe0this-is-not-real-jpeg-data";
    let stream = make_stream(Some(b"DCTDecode"), fake_jpeg.to_vec());

    let result = apply_stream_compress_policy(&stream, CompressStreams::No);
    let Object::Stream(out) = result else {
        panic!("expected Object::Stream");
    };

    // /Filter must still be /DCTDecode (verbatim pass-through).
    assert_eq!(
        out.dict.get("Filter"),
        Some(&Object::Name(b"DCTDecode".to_vec())),
        "No mode must pass DCTDecode streams through verbatim"
    );

    // Data bytes must be unchanged.
    assert_eq!(
        out.data.as_slice(),
        fake_jpeg,
        "No mode: DCTDecode data must be bit-for-bit unchanged"
    );
}

/// Same check for Yes mode: unsupported codec → verbatim pass-through even
/// in compress mode.
#[test]
fn compress_yes_passthrough_dct_stream_verbatim() {
    let fake_jpeg = b"\xff\xd8\xff\xe0not-real-jpeg";
    let stream = make_stream(Some(b"DCTDecode"), fake_jpeg.to_vec());

    let result = apply_stream_compress_policy(&stream, CompressStreams::Yes);
    let Object::Stream(out) = result else {
        panic!("expected Object::Stream");
    };

    // /Filter is preserved — we cannot re-compress what we cannot decode.
    assert_eq!(
        out.dict.get("Filter"),
        Some(&Object::Name(b"DCTDecode".to_vec())),
        "Yes mode must pass DCTDecode streams through verbatim on decode failure"
    );
    assert_eq!(
        out.data.as_slice(),
        fake_jpeg,
        "Yes mode: DCTDecode data must be bit-for-bit unchanged"
    );
}

// ---------------------------------------------------------------------------
// (d) Yes: stream already carrying /FlateDecode → NOT double-compressed
// ---------------------------------------------------------------------------

#[test]
fn compress_yes_does_not_double_compress_flate_stream() {
    let raw = b"Already compressed once - must not become [/FlateDecode /FlateDecode].";
    let compressed = flate_encode(raw);
    let stream = make_stream(Some(b"FlateDecode"), compressed);

    let result = apply_stream_compress_policy(&stream, CompressStreams::Yes);
    let Object::Stream(out) = result else {
        panic!("expected Object::Stream");
    };

    // /Filter must be a Name (not an Array) and must be exactly /FlateDecode.
    match out.dict.get("Filter") {
        Some(Object::Name(name)) => {
            assert_eq!(
                name.as_slice(),
                b"FlateDecode",
                "Yes mode must produce exactly one /FlateDecode, not a chain"
            );
        }
        Some(Object::Array(arr)) => {
            // Single-element array [/FlateDecode] is also acceptable,
            // but must not contain more than one element.
            assert_eq!(
                arr.len(),
                1,
                "Yes mode must not produce a multi-element filter array; got {arr:?}"
            );
            let Object::Name(name) = &arr[0] else {
                panic!(
                    "Yes mode single-element filter array must be a Name; got {:?}",
                    arr[0]
                );
            };
            assert_eq!(name.as_slice(), b"FlateDecode");
        }
        other => panic!("unexpected /Filter value: {other:?}"),
    }

    // Round-trip: must still decode back to the original raw bytes.
    let decoded = filters::decode_stream_data(&out.dict, &out.data)
        .expect("decode should succeed after Yes mode");
    assert_eq!(
        decoded, raw,
        "Yes mode: double-compress guard — round-trip must recover original data"
    );
}

// ---------------------------------------------------------------------------
// Full-rewrite integration: write_pdf_with_options + CompressStreams::No
// ---------------------------------------------------------------------------

/// Build a minimal in-memory PDF whose stream uses FlateDecode.
/// Returns (pdf_bytes, original_raw_payload).
fn build_minimal_pdf_with_flate_stream() -> (Vec<u8>, Vec<u8>) {
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;

    let raw: Vec<u8> =
        b"The quick brown fox jumps over the lazy dog. Payload for compress test.".to_vec();

    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(&raw).unwrap();
    let compressed = enc.finish().unwrap();

    let mut bytes = b"%PDF-1.4\n".to_vec();

    // obj 1: Catalog
    let cat_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    // obj 2: Pages
    let pages_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");

    // obj 3: content stream with /FlateDecode
    let stream_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "3 0 obj\n<< /Filter /FlateDecode /Length {} >>\nstream\n",
            compressed.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&compressed);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 4\n");
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{cat_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{pages_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{stream_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(b"trailer\n<< /Size 4 /Root 1 0 R >>\n");
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

    (bytes, raw)
}

#[test]
fn full_rewrite_compress_no_strips_filter_from_all_streams() {
    use flpdf::{write_pdf_with_options, Object, ObjectRef, Pdf, WriteOptions};
    use std::io::Cursor;

    let (source, original_raw) = build_minimal_pdf_with_flate_stream();
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.compress_streams = CompressStreams::No;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    // Re-open the output and inspect stream 3.
    let mut reopened = Pdf::open(Cursor::new(&output)).unwrap();
    let stream_obj = reopened.resolve(ObjectRef::new(3, 0)).unwrap();
    let Object::Stream(stream) = stream_obj else {
        panic!("object 3 should be a stream");
    };

    // /Filter must be absent.
    assert_eq!(
        stream.dict.get("Filter"),
        None,
        "compress_streams=No must strip /FlateDecode from full-rewrite output"
    );

    // Round-trip: unfiltered data must equal the original payload.
    let decoded =
        filters::decode_stream_data(&stream.dict, &stream.data).expect("unfiltered read-back");
    assert_eq!(
        decoded, original_raw,
        "compress_streams=No: decoded output must equal original payload"
    );
}

#[test]
fn full_rewrite_compress_yes_applies_flate_to_all_streams() {
    use flpdf::{write_pdf_with_options, Object, ObjectRef, Pdf, WriteOptions};
    use std::io::Cursor;

    let (source, original_raw) = build_minimal_pdf_with_flate_stream();
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.compress_streams = CompressStreams::Yes;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    let mut reopened = Pdf::open(Cursor::new(&output)).unwrap();
    let stream_obj = reopened.resolve(ObjectRef::new(3, 0)).unwrap();
    let Object::Stream(stream) = stream_obj else {
        panic!("object 3 should be a stream");
    };

    // /Filter must be /FlateDecode.
    assert_eq!(
        stream.dict.get("Filter"),
        Some(&Object::Name(b"FlateDecode".to_vec())),
        "compress_streams=Yes must emit /FlateDecode"
    );

    // Round-trip must recover original payload.
    let decoded =
        filters::decode_stream_data(&stream.dict, &stream.data).expect("FlateDecode read-back");
    assert_eq!(
        decoded, original_raw,
        "compress_streams=Yes: decoded output must equal original payload"
    );
}
