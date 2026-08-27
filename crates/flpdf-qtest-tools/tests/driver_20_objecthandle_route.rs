//! Contract tests for the qtest-driver 20 shallow-copy cutover.

#[test]
fn test_20_uses_the_live_handle_trailer_and_writer_route() {
    let source = include_str!("../src/driver/test_18_25.rs");
    let start = source
        .find("pub(crate) fn run_test_20")
        .expect("test_20 implementation must exist");
    let body = &source[start..];
    let end = body
        .find("pub(crate) fn run_test_21")
        .expect("test_21 marker must delimit test_20");
    let source = &body[..end];

    for forbidden in [
        "resolve_borrowed",
        "resolve_object",
        "resolve_to_terminal",
        "materialize(",
        "set_object(",
        "Object::",
        "GAP(test_20",
    ] {
        assert!(
            !source.contains(forbidden),
            "test_20 still contains the qtest raw-route marker {forbidden:?}"
        );
    }
    for required in [
        "shallow_copy",
        "append_array_item",
        "replace_key",
        "PdfWriter",
        "set_static_id",
        "StreamDataMode::Preserve",
    ] {
        assert!(
            source.contains(required),
            "test_20 must retain canonical marker {required:?}"
        );
    }
}
