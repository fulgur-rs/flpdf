use flpdf::filters::{decode_stream_data, decode_stream_data_recovering, StreamDecodeEvent};
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
