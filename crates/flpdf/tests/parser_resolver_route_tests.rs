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
fn object_stream_test_entrypoint_keeps_the_parsed_value_as_a_handle() {
    let source = include_str!("../src/reader.rs");
    let entrypoint = source
        .split("pub(crate) fn parse_object_stream_entry(")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) struct ParsedObjectStreamEntry")
                .next()
        })
        .expect("object-stream test entrypoint exists");

    assert!(entrypoint.contains("parse_qpdf_file_object_handle_with_diagnostics"));
    assert!(!entrypoint.contains("parse_qpdf_file_object("));
    assert!(!entrypoint.contains(".materialize("));
}
