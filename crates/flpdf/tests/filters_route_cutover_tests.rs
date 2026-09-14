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
    assert!(
        !std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/filters.rs")
            .exists(),
        "filters.rs still exposes a legacy public boundary"
    );
}

#[test]
fn passthrough_codec_label_bridge_is_removed() {
    let stream_filter =
        production_source(concat!(env!("CARGO_MANIFEST_DIR"), "/src/stream_filter.rs"));
    for forbidden in [
        "pub(crate) fn passthrough_codec_label(",
        "passthrough codec {label}: image/binary stream data is not decoded by flpdf",
    ] {
        assert!(
            !stream_filter.contains(forbidden),
            "stream_filter.rs still contains C43 bridge text: {forbidden}"
        );
    }
}

#[test]
fn qpdf_less_recovering_filter_api_is_removed() {
    let crate_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(
        !crate_root.join("src/filters.rs").exists(),
        "qpdf-less filters.rs module remains"
    );
    assert!(
        !crate_root
            .join("tests/tiff_predictor_memory_tests.rs")
            .exists(),
        "qpdf-less TIFF hardening test remains"
    );
    let lib = std::fs::read_to_string(crate_root.join("src/lib.rs")).unwrap();
    assert!(!lib.contains("pub mod filters;"));
    for (relative, forbidden) in [
        ("src/stream_filter.rs", "pipe_decode_recovering"),
        ("src/stream_filter.rs", "FilterDecodeOutcome"),
        ("src/stream_filter.rs", "FilterDecodePhase"),
        ("src/stream_filter.rs", "decode_filter_specs_from_handle"),
        ("src/stream_filter.rs", "set_tiff_memory_limit"),
        ("src/pipeline/dct.rs", "DecodeLimits"),
        ("src/pipeline/dct.rs", "with_max_output"),
        ("src/pipeline/tiff_predictor.rs", "new_with_memory_limit"),
        ("src/pipeline/tiff_predictor.rs", "max_memory"),
    ] {
        let source = std::fs::read_to_string(crate_root.join(relative)).unwrap();
        assert!(
            !source.contains(forbidden),
            "{relative} retains qpdf-less C28 API/implementation: {forbidden}"
        );
    }
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

#[test]
fn runtime_stream_filter_registry_is_public_and_single_canonical_lookup() {
    let crate_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let lib = std::fs::read_to_string(crate_root.join("src/lib.rs")).unwrap();
    assert!(lib.contains("pub mod stream_filter;"));
    assert!(lib.contains(
        "pub use stream_filter::{register_stream_filter, OwnedDecodePipeline, StreamFilter};"
    ));
    assert!(std::fs::read_to_string(crate_root.join("src/pipeline.rs"))
        .unwrap()
        .contains("pub enum PipelineRef"));

    let stream_filter =
        production_source(concat!(env!("CARGO_MANIFEST_DIR"), "/src/stream_filter.rs"));
    assert!(stream_filter.contains("pub trait StreamFilter: 'static"));
    assert!(stream_filter.contains("static FILTER_FACTORIES"));
    assert!(stream_filter.contains("pub fn register_stream_filter"));
    assert!(stream_filter.contains("fn get_decode_pipeline<'a>"));
    assert!(!stream_filter.contains("decode_pipeline_owned"));
    assert!(!stream_filter.contains("match filter_name"));
}
