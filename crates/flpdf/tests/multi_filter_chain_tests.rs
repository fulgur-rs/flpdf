//! Integration tests for multi-filter chain encoding/decoding (flpdf-jcd.7).
//!
//! Tests the following filter array combinations:
//!   (a) [/ASCII85Decode /FlateDecode] — round-trip, no DecodeParms
//!   (b) [/FlateDecode /ASCIIHexDecode] — round-trip, no DecodeParms
//!   (c) [/ASCII85Decode /FlateDecode] — with DecodeParms array [null <<Predictor 12...>>]
//!   (d) [/FlateDecode /RunLengthDecode] — round-trip, no DecodeParms
//!   (e) DecodeParms as a bare null Object — must not panic; filters apply without predictor

use flpdf::{filters, Dictionary, Object};

// ---------------------------------------------------------------------------
// (a) [/ASCII85Decode /FlateDecode] — no DecodeParms
// ---------------------------------------------------------------------------

#[test]
fn chain_ascii85_then_flate_round_trip() {
    let raw = b"Hello from the ASCII85+FlateDecode filter chain round-trip test!";

    // Build a dict with Filter = [/ASCII85Decode /FlateDecode]
    let mut dict = Dictionary::new();
    dict.insert(
        "Filter",
        Object::Array(vec![
            Object::Name(b"ASCII85Decode".to_vec()),
            Object::Name(b"FlateDecode".to_vec()),
        ]),
    );

    // encode_stream_data applies filters in *reverse* order (encode direction):
    //   raw → FlateDecode-encode → ASCII85-encode
    let encoded = filters::encode_stream_data(&dict, raw).expect("encode chain");

    // decode_stream_data applies filters in declared order:
    //   encoded → ASCII85Decode → FlateDecode → raw
    let decoded = filters::decode_stream_data(&dict, &encoded).expect("decode chain");

    assert_eq!(
        decoded.as_slice(),
        raw,
        "ASCII85+FlateDecode round-trip must produce the original bytes"
    );
}

// ---------------------------------------------------------------------------
// (b) [/FlateDecode /ASCIIHexDecode] — no DecodeParms
// ---------------------------------------------------------------------------

#[test]
fn chain_flate_then_ascii_hex_round_trip() {
    let raw = b"Testing FlateDecode + ASCIIHexDecode chain: both encode and decode directions.";

    // Filter = [/FlateDecode /ASCIIHexDecode]
    let mut dict = Dictionary::new();
    dict.insert(
        "Filter",
        Object::Array(vec![
            Object::Name(b"FlateDecode".to_vec()),
            Object::Name(b"ASCIIHexDecode".to_vec()),
        ]),
    );

    // encode: raw → ASCIIHex-encode → FlateDecode-encode
    let encoded = filters::encode_stream_data(&dict, raw).expect("encode chain");

    // decode: encoded → FlateDecode → ASCIIHex-decode → raw
    let decoded = filters::decode_stream_data(&dict, &encoded).expect("decode chain");

    assert_eq!(
        decoded.as_slice(),
        raw,
        "FlateDecode+ASCIIHexDecode round-trip must produce the original bytes"
    );
}

// ---------------------------------------------------------------------------
// (c) [/ASCII85Decode /FlateDecode] with DecodeParms = [null <<Predictor 12 ...>>]
//
// The "null" in position 0 means "no DecodeParms for ASCII85Decode".
// The dictionary in position 1 means FlateDecode uses Predictor 12 (Up filter).
// ---------------------------------------------------------------------------

#[test]
fn chain_ascii85_then_flate_with_predictor_decode_params() {
    // 5 rows × 4 columns of raw image-like data — must be divisible by Columns (4)
    let columns: usize = 4;
    let row_count: usize = 5;
    // Generate deterministic bytes that wrap at 256. Using a usize range with
    // `% 256` keeps the test robust if these dimensions are ever raised above
    // the point where columns * row_count exceeds 255 (where a plain
    // `(0u8..(columns * row_count) as u8)` range would silently truncate).
    let raw: Vec<u8> = (0..columns * row_count).map(|i| (i % 256) as u8).collect();
    assert_eq!(raw.len(), columns * row_count);

    // DecodeParms = [null, << /Predictor 12 /Columns 4 /Colors 1 /BitsPerComponent 8 >>]
    let mut flate_params = Dictionary::new();
    flate_params.insert("Predictor", Object::Integer(12));
    flate_params.insert("Columns", Object::Integer(columns as i64));
    flate_params.insert("Colors", Object::Integer(1));
    flate_params.insert("BitsPerComponent", Object::Integer(8));

    let mut dict = Dictionary::new();
    dict.insert(
        "Filter",
        Object::Array(vec![
            Object::Name(b"ASCII85Decode".to_vec()),
            Object::Name(b"FlateDecode".to_vec()),
        ]),
    );
    dict.insert(
        "DecodeParms",
        Object::Array(vec![Object::Null, Object::Dictionary(flate_params)]),
    );

    // encode: apply predictor to raw data, then FlateDecode-encode, then ASCII85-encode
    let encoded = filters::encode_stream_data(&dict, &raw).expect("encode with predictor chain");

    // decode: ASCII85Decode, then FlateDecode + undo predictor → original raw
    let decoded =
        filters::decode_stream_data(&dict, &encoded).expect("decode with predictor chain");

    assert_eq!(
        decoded.as_slice(),
        raw.as_slice(),
        "ASCII85+FlateDecode with Predictor 12 DecodeParms array must round-trip correctly"
    );
}

// ---------------------------------------------------------------------------
// (d) [/FlateDecode /RunLengthDecode] — no DecodeParms
// ---------------------------------------------------------------------------

#[test]
fn chain_flate_then_run_length_round_trip() {
    // RLE-friendly payload: some repeated runs and some unique bytes
    let raw = b"AAAAAABBBBBBCCCCCC plain text mixed in DDDDDDEEEEEE";

    // Filter = [/FlateDecode /RunLengthDecode]
    let mut dict = Dictionary::new();
    dict.insert(
        "Filter",
        Object::Array(vec![
            Object::Name(b"FlateDecode".to_vec()),
            Object::Name(b"RunLengthDecode".to_vec()),
        ]),
    );

    // encode: raw → RunLength-encode → FlateDecode-encode
    let encoded = filters::encode_stream_data(&dict, raw).expect("encode chain");

    // decode: FlateDecode → RunLength-decode → raw
    let decoded = filters::decode_stream_data(&dict, &encoded).expect("decode chain");

    assert_eq!(
        decoded.as_slice(),
        raw,
        "FlateDecode+RunLengthDecode round-trip must produce the original bytes"
    );
}

// ---------------------------------------------------------------------------
// (e) DecodeParms as a bare Object::Null — must not error; filters apply without predictor
// ---------------------------------------------------------------------------

#[test]
fn chain_null_decode_parms_is_ignored() {
    // When DecodeParms is a bare Null (not an array), get_decode_params returns None,
    // so no predictor is applied. The filter still runs normally.
    let raw = b"bare null DecodeParms should be silently ignored";

    let mut dict = Dictionary::new();
    dict.insert(
        "Filter",
        Object::Array(vec![
            Object::Name(b"FlateDecode".to_vec()),
            Object::Name(b"ASCIIHexDecode".to_vec()),
        ]),
    );
    // Set DecodeParms to a bare Null (edge case: malformed but should not crash)
    dict.insert("DecodeParms", Object::Null);

    let encoded = filters::encode_stream_data(&dict, raw).expect("encode with null DecodeParms");
    let decoded =
        filters::decode_stream_data(&dict, &encoded).expect("decode with null DecodeParms");

    assert_eq!(
        decoded.as_slice(),
        raw,
        "bare null DecodeParms must be treated as no-op predictor"
    );
}
