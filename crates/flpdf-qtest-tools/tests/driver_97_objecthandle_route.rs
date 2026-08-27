//! Contract tests for the qtest-driver 97 ObjectHandle cutover slice.

#[test]
fn test_97_uses_only_canonical_resolution() {
    let source = include_str!("../src/driver/test_88_98.rs");
    let start = source
        .find("pub(crate) fn run_test_97")
        .expect("test_97 implementation must exist");
    let end = source[start..]
        .find("// test_98")
        .map(|offset| start + offset)
        .expect("test_98 marker must delimit test_97");
    let source = &source[start..end];

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
            "test_97 still contains the qtest raw-route marker {forbidden:?}"
        );
    }
    assert!(
        source.contains("pdf.resolve"),
        "test_97 must retain the canonical Pdf::resolve route"
    );
}
