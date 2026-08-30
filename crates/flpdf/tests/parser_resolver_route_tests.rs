//! Route guards for the qpdf-shaped parser and resolver cutover.

#[test]
fn parser_has_no_detached_live_object_projection() {
    let source = include_str!("../src/parser.rs");

    assert!(!source.contains("fn materialize_live_handle"));
    assert!(!source.contains("pub(crate) fn parse_qpdf_file_object("));
}

#[test]
fn xref_stream_production_parses_through_the_handle_route_once() {
    let source = include_str!("../src/xref.rs");
    let parse_xref_stream = source
        .split("fn parse_xref_stream(")
        .nth(1)
        .and_then(|tail| tail.split("fn xref_file_object_diagnostic").next())
        .expect("xref stream parser exists");

    assert!(parse_xref_stream.contains("read_file_object_handle("));
    assert!(!parse_xref_stream.contains("read_file_object("));
}

#[test]
fn object_stream_legacy_test_entrypoint_is_removed() {
    let source = include_str!("../src/reader.rs");
    assert!(!source.contains("pub(crate) fn parse_object_stream_entry("));
    assert!(source.contains("parse_qpdf_file_object_handle_with_diagnostics"));
    assert!(!source.contains("parse_qpdf_file_object("));
    assert!(!source.contains(".materialize("));
}
