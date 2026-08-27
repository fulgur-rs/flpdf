//! Contract tests for the qtest-driver 25 foreign-copy cutover.

#[test]
fn test_25_uses_the_canonical_foreign_copy_and_writer_route() {
    let source = include_str!("../src/driver/test_18_25.rs");
    let start = source
        .find("pub(crate) fn run_test_25")
        .expect("test_25 implementation must exist");
    let source = &source[start..];

    for forbidden in [
        "resolve_borrowed",
        "resolve_object",
        "resolve_to_terminal",
        "materialize(",
        "Object::",
        "GAP(test_25",
    ] {
        assert!(
            !source.contains(forbidden),
            "test_25 still contains the qtest raw-route marker {forbidden:?}"
        );
    }
    for required in [
        "copy_foreign_object",
        "replace_key",
        "trailer_key_handle",
        "resolve",
        "PdfWriter",
        "set_static_id",
        "StreamDataMode::Preserve",
    ] {
        assert!(
            source.contains(required),
            "test_25 must retain canonical marker {required:?}"
        );
    }
}
