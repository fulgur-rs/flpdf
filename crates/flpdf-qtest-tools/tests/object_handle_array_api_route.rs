//! Contract tests for the external canonical array API consumer.

#[test]
fn external_array_consumer_uses_only_object_handles() {
    let source = include_str!("object_handle_array_api.rs");

    for forbidden in [
        "Object::",
        "resolve_object(",
        "resolve_borrowed(",
        "resolve_to_terminal(",
        "materialize(",
        "set_object(",
    ] {
        assert!(
            !source.contains(forbidden),
            "external array consumer still contains raw-route marker {forbidden:?}"
        );
    }
    assert!(
        source.contains("ObjectHandle"),
        "external array consumer must retain the canonical ObjectHandle route"
    );
    assert!(
        source.contains("pdf.resolve("),
        "external array consumer must resolve through Pdf::resolve"
    );
}
