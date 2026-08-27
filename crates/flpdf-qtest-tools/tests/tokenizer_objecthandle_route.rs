//! Contract tests for the qtest tokenizer ObjectHandle cutover slice.

#[test]
fn tokenizer_has_no_legacy_resolution_or_raw_object_route() {
    let source = include_str!("../src/tokenizer_runner.rs");

    for forbidden in [
        "resolve_borrowed",
        "resolve_object",
        "resolve_to_terminal",
        "resolve_to_terminal_ref",
        "materialize(",
        "Object::",
    ] {
        assert!(
            !source.contains(forbidden),
            "tokenizer_runner.rs still contains the qtest raw-route marker {forbidden:?}"
        );
    }
    assert!(
        source.contains("pdf.resolve"),
        "tokenizer_runner.rs must retain the canonical Pdf::resolve route"
    );
}
