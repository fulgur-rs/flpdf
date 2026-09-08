//! Route guards for the qpdf-shaped parser and resolver.

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
fn xref_parsers_do_not_carry_dead_diagnostic_sink_arguments() {
    let source = include_str!("../src/xref.rs");
    let table = source
        .split("fn parse_xref_table(")
        .nth(1)
        .and_then(|tail| tail.split("fn parse_xref_stream(").next())
        .expect("classic xref table parser exists");
    assert!(
        !table.contains("error_diagnostics_sink"),
        "classic xref table parser must return diagnostics rather than accept a dead sink"
    );

    let canonical_stream = source
        .split("fn parse_xref_stream_with_canonical_owner(")
        .nth(1)
        .and_then(|tail| tail.split("fn build_xref_stream(").next())
        .expect("canonical xref stream parser exists");
    assert!(
        !canonical_stream.contains("error_diagnostics_sink"),
        "canonical xref stream parser must deliver diagnostics through its owner"
    );
}

#[test]
fn object_stream_legacy_test_entrypoint_is_removed() {
    let source = include_str!("../src/reader.rs");
    assert!(!source.contains("pub(crate) fn parse_object_stream_entry("));
    assert!(!source.contains("parse_qpdf_file_object("));
    assert!(!source.contains(".materialize("));
}
