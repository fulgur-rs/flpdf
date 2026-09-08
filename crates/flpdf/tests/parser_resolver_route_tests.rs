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
    let parse_xref_stream = slice_between(
        source,
        "fn parse_xref_stream(",
        "fn xref_file_object_diagnostic",
    );

    assert!(parse_xref_stream.contains("read_file_object_handle("));
    assert!(!parse_xref_stream.contains("read_file_object("));
}

#[test]
fn xref_parsers_do_not_carry_dead_diagnostic_sink_arguments() {
    let source = include_str!("../src/xref.rs");
    let table = slice_between(source, "fn parse_xref_table(", "fn parse_xref_stream(");
    assert!(
        !table.contains("error_diagnostics_sink"),
        "classic xref table parser must return diagnostics rather than accept a dead sink"
    );

    let canonical_stream = slice_between(
        source,
        "fn parse_xref_stream_with_canonical_owner(",
        "fn is_xref_stream_handle(",
    );
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

/// The source between two function headers, with both ends verified.
///
/// `str::split(..).next()` yields the whole remainder when the separator is
/// absent, so an end marker that sits *before* the start marker silently
/// widens the slice to the end of the file instead of failing. Check both
/// ends so a stale marker is a test failure rather than a wider guard.
fn slice_between<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let tail = source
        .split_once(start)
        .unwrap_or_else(|| panic!("{start} exists"))
        .1;
    tail.split_once(end)
        .unwrap_or_else(|| panic!("{end} follows {start}"))
        .0
}
