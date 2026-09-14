fn production_source(path: &str) -> String {
    let source = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {path}: {error}"))
        .replace("\r\n", "\n");
    source
        .split("#[cfg(test)]\nmod tests")
        .next()
        .unwrap_or_else(|| panic!("{path} test module marker"))
        .to_owned()
}

#[test]
fn filter_public_boundaries_are_object_handle_native() {
    let filters = production_source(concat!(env!("CARGO_MANIFEST_DIR"), "/src/filters.rs"));
    for forbidden in [
        "pub fn decode_stream_data(dict: &Dictionary",
        "pub fn decode_stream_data_recovering(\n    dict: &Dictionary",
        "pub fn decode_stream_data_with_limits(\n    dict: &Dictionary",
        "pub fn encode_stream_data(dict: &Dictionary",
    ] {
        assert!(
            !filters.contains(forbidden),
            "filters.rs still exposes legacy Dictionary boundary: {forbidden}"
        );
    }
}

#[test]
fn passthrough_codec_label_has_no_public_filters_bridge() {
    let filters = production_source(concat!(env!("CARGO_MANIFEST_DIR"), "/src/filters.rs"));
    assert!(
        !filters.contains("pub fn passthrough_codec_label("),
        "filters.rs still exposes the removed public passthrough label bridge"
    );

    let stream_filter =
        production_source(concat!(env!("CARGO_MANIFEST_DIR"), "/src/stream_filter.rs"));
    assert!(
        stream_filter.contains("pub(crate) fn passthrough_codec_label("),
        "stream_filter.rs must retain the canonical internal label owner"
    );
}

#[test]
fn object_shape_filter_reader_is_test_only() {
    let source =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/stream_filter.rs"))
            .unwrap()
            .replace("\r\n", "\n");

    for declaration in [
        "pub(crate) fn decode_filter_specs_from_object",
        "fn decode_params_from_object(",
        "fn param_value_from_object(",
        "fn clamped_int_param(",
    ] {
        assert!(
            !source.contains(declaration),
            "stream_filter.rs still contains the removed object-shaped helper: {declaration}"
        );
    }
}
