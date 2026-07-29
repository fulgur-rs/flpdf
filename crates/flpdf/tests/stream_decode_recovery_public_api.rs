use flpdf::filters::{
    decode_stream_data, decode_stream_data_recovering, encode_stream_data, StreamDecodeEvent,
};
use flpdf::{Dictionary, Object};

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

    let outcome = decode_stream_data_recovering(&dictionary, b"78G").unwrap();

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
        decode_stream_data(&dictionary, b"78G")
            .unwrap_err()
            .to_string(),
        "unsupported PDF feature: character out of range during base Hex decode: G"
    );
}

#[test]
fn recovery_events_keep_odd_nibble_cleanup_after_write_error() {
    let dictionary = odd_nibble_error_dictionary();

    let outcome = decode_stream_data_recovering(&dictionary, b"4G ").unwrap();

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
        decode_stream_data(&dictionary, b"4G ")
            .unwrap_err()
            .to_string(),
        "unsupported PDF feature: character out of range during base Hex decode: G"
    );
}

#[test]
fn recovery_events_keep_downstream_data_before_upstream_write_error() {
    let dictionary = downstream_data_before_upstream_error_dictionary();

    let outcome = decode_stream_data_recovering(&dictionary, b"3431G").unwrap();

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
        decode_stream_data(&dictionary, b"3431G")
            .unwrap_err()
            .to_string(),
        "unsupported PDF feature: character out of range during base Hex decode: G"
    );
}

#[test]
fn recovery_events_keep_downstream_cleanup_after_upstream_write_error() {
    let dictionary = downstream_data_before_upstream_error_dictionary();

    let outcome = decode_stream_data_recovering(&dictionary, b"343G").unwrap();

    assert_eq!(outcome.data, b"@");
    assert!(matches!(
        &outcome.events[..],
        [StreamDecodeEvent::Error(error), StreamDecodeEvent::Data(data)]
            if error.to_string()
                == "unsupported PDF feature: character out of range during base Hex decode: G"
                && data == b"@"
    ));
    assert_eq!(
        decode_stream_data(&dictionary, b"343G")
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
    let compressed = encode_stream_data(&flate_dictionary, b"partial flate output").unwrap();

    // Dropping the zlib checksum leaves the decoded bytes available but makes
    // Flate warn at finish. The trailing invalid hex digit separately makes
    // the preceding ASCIIHex stage report its write-time error.
    let mut encoded = asciihex_encode(&compressed[..compressed.len() - 4]);
    encoded.push(b'G');

    let outcome = decode_stream_data_recovering(&dictionary, &encoded).unwrap();

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
