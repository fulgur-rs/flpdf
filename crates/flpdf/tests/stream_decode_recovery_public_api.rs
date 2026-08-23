use flpdf::filters::{
    decode_stream_data, decode_stream_data_recovering, decode_stream_data_recovering_with_limits,
    encode_stream_data, DecodeLimits, StreamDecodeEvent,
};
use flpdf::{Dictionary, Object};

fn asciihex_dictionary(stages: usize) -> Dictionary {
    let mut dictionary = Dictionary::new();
    dictionary.insert(
        "Filter",
        Object::Array(vec![Object::Name(b"ASCIIHexDecode".to_vec()); stages]),
    );
    dictionary
}

fn error_then_finish_warning_dictionary() -> Dictionary {
    let mut dictionary = Dictionary::new();
    dictionary.insert(
        "Filter",
        Object::Array(vec![
            Object::Name(b"ASCIIHexDecode".to_vec()),
            Object::Name(b"FlateDecode".to_vec()),
        ]),
    );
    dictionary
}

fn odd_nibble_error_dictionary() -> Dictionary {
    let mut dictionary = Dictionary::new();
    dictionary.insert("Filter", Object::Name(b"AHx".to_vec()));
    dictionary
}

#[test]
fn recovering_limits_keep_default_chain_cap_but_allow_explicit_unlimited_chain() {
    let dictionary = asciihex_dictionary(17);
    let original = b"A";
    let mut encoded = original.to_vec();
    for _ in 0..17 {
        encoded = asciihex_encode(&encoded);
    }

    assert_eq!(
        decode_stream_data_recovering(&filter_handles::dictionary(&dictionary), &encoded)
            .unwrap_err()
            .to_string(),
        "unsupported PDF feature: filter chain length 17 exceeds maximum of 16"
    );

    let outcome = decode_stream_data_recovering_with_limits(
        &filter_handles::dictionary(&dictionary),
        &encoded,
        DecodeLimits {
            max_output: None,
            max_filter_chain: None,
        },
    )
    .unwrap();

    assert_eq!(outcome.data, original);
    assert!(matches!(
        &outcome.events[..],
        [StreamDecodeEvent::Data(data)] if data == original
    ));
}

#[test]
fn recovering_final_flate_warning_precedes_predictor_finish_data() {
    let mut flate = Dictionary::new();
    flate.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
    let mut encoded = encode_stream_data(&filter_handles::dictionary(&flate), b"\0A").unwrap();
    encoded.truncate(encoded.len() - 4);

    let mut decode_params = Dictionary::new();
    decode_params.insert("Predictor", Object::Integer(12));
    decode_params.insert("Columns", Object::Integer(2));
    let mut dictionary = Dictionary::new();
    dictionary.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
    dictionary.insert("DecodeParms", Object::Dictionary(decode_params));

    let outcome =
        decode_stream_data_recovering(&filter_handles::dictionary(&dictionary), &encoded).unwrap();

    assert_eq!(outcome.data, b"A\0");
    assert!(matches!(
        &outcome.events[..],
        [
            StreamDecodeEvent::Warning(warning),
            StreamDecodeEvent::Data(data),
        ] if warning.message == "input stream is complete but output may still be valid"
            && warning.code == -5
            && data == b"A\0"
    ));
}

fn downstream_data_before_upstream_error_dictionary() -> Dictionary {
    let mut dictionary = Dictionary::new();
    dictionary.insert(
        "Filter",
        Object::Array(vec![
            Object::Name(b"AHx".to_vec()),
            Object::Name(b"AHx".to_vec()),
        ]),
    );
    dictionary
}

fn asciihex_then_flate_dictionary() -> Dictionary {
    let mut dictionary = Dictionary::new();
    dictionary.insert(
        "Filter",
        Object::Array(vec![
            Object::Name(b"AHx".to_vec()),
            Object::Name(b"FlateDecode".to_vec()),
        ]),
    );
    dictionary
}

fn flate_then_asciihex_dictionary() -> Dictionary {
    let mut dictionary = Dictionary::new();
    dictionary.insert(
        "Filter",
        Object::Array(vec![
            Object::Name(b"FlateDecode".to_vec()),
            Object::Name(b"AHx".to_vec()),
        ]),
    );
    dictionary
}

fn asciihex_encode(data: &[u8]) -> Vec<u8> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = Vec::with_capacity(data.len() * 2);
    for &byte in data {
        encoded.push(HEX[usize::from(byte >> 4)]);
        encoded.push(HEX[usize::from(byte & 0x0f)]);
    }
    encoded
}

#[test]
fn recovery_events_and_strict_error_share_pipeline_order() {
    let dictionary = error_then_finish_warning_dictionary();

    let outcome =
        decode_stream_data_recovering(&filter_handles::dictionary(&dictionary), b"78G").unwrap();

    assert!(outcome.data.is_empty());
    assert_eq!(outcome.events.len(), 2);
    assert!(matches!(
        &outcome.events[0],
        StreamDecodeEvent::Error(error)
            if error.to_string()
                == "unsupported PDF feature: character out of range during base Hex decode: G"
    ));
    assert!(matches!(
        &outcome.events[1],
        StreamDecodeEvent::Warning(warning)
            if warning.message == "input stream is complete but output may still be valid"
                && warning.code == -5
    ));

    assert_eq!(
        decode_stream_data(&filter_handles::dictionary(&dictionary), b"78G")
            .unwrap_err()
            .to_string(),
        "unsupported PDF feature: character out of range during base Hex decode: G"
    );
}

#[test]
fn recovery_events_keep_odd_nibble_cleanup_after_write_error() {
    let dictionary = odd_nibble_error_dictionary();

    let outcome =
        decode_stream_data_recovering(&filter_handles::dictionary(&dictionary), b"4G ").unwrap();

    assert_eq!(outcome.data, b"@");
    assert_eq!(outcome.events.len(), 2);
    assert!(matches!(
        &outcome.events[0],
        StreamDecodeEvent::Error(error)
            if error.to_string()
                == "unsupported PDF feature: character out of range during base Hex decode: G"
    ));
    assert!(matches!(
        &outcome.events[1],
        StreamDecodeEvent::Data(data) if data == b"@"
    ));
    assert_eq!(
        decode_stream_data(&filter_handles::dictionary(&dictionary), b"4G ")
            .unwrap_err()
            .to_string(),
        "unsupported PDF feature: character out of range during base Hex decode: G"
    );
}

#[test]
fn recovery_events_keep_downstream_data_before_upstream_write_error() {
    let dictionary = downstream_data_before_upstream_error_dictionary();

    let outcome =
        decode_stream_data_recovering(&filter_handles::dictionary(&dictionary), b"3431G").unwrap();

    assert_eq!(outcome.data, b"A");
    assert_eq!(outcome.events.len(), 2);
    assert!(matches!(
        &outcome.events[0],
        StreamDecodeEvent::Data(data) if data == b"A"
    ));
    assert!(matches!(
        &outcome.events[1],
        StreamDecodeEvent::Error(error)
            if error.to_string()
                == "unsupported PDF feature: character out of range during base Hex decode: G"
    ));
    assert_eq!(
        decode_stream_data(&filter_handles::dictionary(&dictionary), b"3431G")
            .unwrap_err()
            .to_string(),
        "unsupported PDF feature: character out of range during base Hex decode: G"
    );
}

#[test]
fn recovery_events_keep_downstream_cleanup_after_upstream_write_error() {
    let dictionary = downstream_data_before_upstream_error_dictionary();

    let outcome =
        decode_stream_data_recovering(&filter_handles::dictionary(&dictionary), b"343G").unwrap();

    assert_eq!(outcome.data, b"@");
    assert!(matches!(
        &outcome.events[..],
        [StreamDecodeEvent::Error(error), StreamDecodeEvent::Data(data)]
            if error.to_string()
                == "unsupported PDF feature: character out of range during base Hex decode: G"
                && data == b"@"
    ));
    assert_eq!(
        decode_stream_data(&filter_handles::dictionary(&dictionary), b"343G")
            .unwrap_err()
            .to_string(),
        "unsupported PDF feature: character out of range during base Hex decode: G"
    );
}

#[test]
fn recovery_events_keep_final_data_and_warning_after_prior_write_error() {
    let dictionary = asciihex_then_flate_dictionary();
    let mut flate_dictionary = Dictionary::new();
    flate_dictionary.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
    let compressed = encode_stream_data(
        &filter_handles::dictionary(&flate_dictionary),
        b"partial flate output",
    )
    .unwrap();

    // Dropping the zlib checksum leaves the decoded bytes available but makes
    // Flate warn at finish. The trailing invalid hex digit separately makes
    // the preceding ASCIIHex stage report its write-time error.
    let mut encoded = asciihex_encode(&compressed[..compressed.len() - 4]);
    encoded.push(b'G');

    let outcome =
        decode_stream_data_recovering(&filter_handles::dictionary(&dictionary), &encoded).unwrap();

    assert!(matches!(
        &outcome.events[..],
        [
            StreamDecodeEvent::Data(data),
            StreamDecodeEvent::Error(error),
            StreamDecodeEvent::Warning(warning),
        ] if data == b"partial flate output"
            && error.to_string()
                == "unsupported PDF feature: character out of range during base Hex decode: G"
            && warning.message == "input stream is complete but output may still be valid"
            && warning.code == -5
    ));
}

#[test]
fn recovery_events_keep_nonfinal_warning_between_data_and_cleanup() {
    let dictionary = flate_then_asciihex_dictionary();
    let mut flate_dictionary = Dictionary::new();
    flate_dictionary.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
    let compressed =
        encode_stream_data(&filter_handles::dictionary(&flate_dictionary), b"414").unwrap();

    let outcome = decode_stream_data_recovering(
        &filter_handles::dictionary(&dictionary),
        &compressed[..compressed.len() - 4],
    )
    .unwrap();

    assert!(matches!(
        &outcome.events[..],
        [
            StreamDecodeEvent::Data(data),
            StreamDecodeEvent::Warning(warning),
            StreamDecodeEvent::Data(cleanup),
        ] if data == b"A"
            && warning.message == "input stream is complete but output may still be valid"
            && warning.code == -5
            && cleanup == b"@"
    ));
}

#[test]
fn recovery_events_keep_final_cleanup_after_a_nonfinal_warning_and_write_error() {
    let dictionary = flate_then_asciihex_dictionary();
    let mut flate_dictionary = Dictionary::new();
    flate_dictionary.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
    let compressed =
        encode_stream_data(&filter_handles::dictionary(&flate_dictionary), b"4G ").unwrap();

    let outcome = decode_stream_data_recovering(
        &filter_handles::dictionary(&dictionary),
        &compressed[..compressed.len() - 4],
    )
    .unwrap();

    assert!(matches!(
        &outcome.events[..],
        [
            StreamDecodeEvent::Warning(warning),
            StreamDecodeEvent::Error(error),
            StreamDecodeEvent::Data(cleanup),
        ] if warning.message == "input stream is complete but output may still be valid"
            && warning.code == -5
            && error.to_string()
                == "unsupported PDF feature: character out of range during base Hex decode: G"
            && cleanup == b"@"
    ));
}
#[path = "support/filter_handles.rs"]
mod filter_handles;
