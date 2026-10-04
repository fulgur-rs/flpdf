fn production_source(source: &str) -> &str {
    source
        .split_once("\n#[cfg(test)]")
        .map_or(source, |(production, _)| production)
}

#[test]
fn page_helper_exposes_qpdf_stream_and_callback_routes_without_a_vector_collector() {
    let source = include_str!("../src/page_object_helper.rs");
    let production = production_source(source);

    assert!(production.contains("pub fn get_page_contents"));
    assert!(production.contains("pub fn parse_page_contents"));
    assert!(production.contains("pub fn parse_contents"));
    assert!(
        !production.contains("pub fn content_stream_objects"),
        "qpdf exposes stream handles and parser callbacks, not a parsed-object Vec helper"
    );
    assert!(
        !production.contains("struct ObjectRecordingCallbacks"),
        "the qpdf-less Vec collector must be removed with its public method"
    );
}
