//! Contract tests for the qtest-driver 21 stream shallow-copy cutover.

#[test]
fn test_21_resolves_the_contents_handle_before_shallow_copy() {
    let source = include_str!("../src/driver/test_18_25.rs");
    let start = source
        .find("pub(crate) fn run_test_21")
        .expect("test_21 implementation must exist");
    let body = &source[start..];
    let end = body
        .find("pub(crate) fn run_test_22")
        .expect("test_22 marker must delimit test_21");
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
            "test_21 still contains the qtest raw-route marker {forbidden:?}"
        );
    }
    for required in ["pdf.resolve", "shallow_copy"] {
        assert!(
            source.contains(required),
            "test_21 must retain canonical marker {required:?}"
        );
    }
}
