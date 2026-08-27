//! Contract tests for the qtest-driver 24 reserved-object cutover.

#[test]
fn test_24_uses_the_canonical_reserved_object_route() {
    let source = include_str!("../src/driver/test_18_25.rs");
    let start = source
        .find("pub(crate) fn run_test_24")
        .expect("test_24 implementation must exist");
    let body = &source[start..];
    let end = body
        .find("pub(crate) fn run_test_25")
        .expect("test_25 marker must delimit test_24");
    let source = &body[..end];

    for forbidden in [
        "resolve_borrowed",
        "resolve_object",
        "resolve_to_terminal",
        "materialize(",
        "set_object(",
        "Object::",
    ] {
        assert!(
            !source.contains(forbidden),
            "test_24 still contains the qtest raw-route marker {forbidden:?}"
        );
    }
    for required in ["new_reserved", "replace_reserved", "make_direct"] {
        assert!(
            source.contains(required),
            "test_24 must retain canonical marker {required:?}"
        );
    }
}
