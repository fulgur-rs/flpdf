use std::fs;

#[test]
fn reader_tests_use_canonical_object_handle_routes() {
    let path = format!("{}/tests/reader_tests.rs", env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(path).expect("reader_tests source");

    for forbidden in [
        "resolve_borrowed(",
        "resolve_object(",
        "resolve_to_cache(",
        "materialize(",
        "Object::",
        "Dictionary::",
        ".as_dict(",
        ".into_dict(",
    ] {
        assert!(
            !source.contains(forbidden),
            "reader_tests retains legacy route marker: {forbidden}"
        );
    }

    for required in [
        "ObjectHandle",
        "get_object_handle(",
        "resolve(",
        "get_raw_stream_data(",
        "get_stream_data(",
    ] {
        assert!(
            source.contains(required),
            "reader_tests must use canonical route marker: {required}"
        );
    }
}
