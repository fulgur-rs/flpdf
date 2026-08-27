//! Contract tests for the qtest-driver 88-90 ObjectHandle cutover slice.

fn function_region<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let start = source
        .find(start)
        .unwrap_or_else(|| panic!("missing function marker {start:?}"));
    let body = &source[start..];
    let end = body
        .find(end)
        .unwrap_or_else(|| panic!("missing end marker {end:?}"));
    &body[..end]
}

#[test]
fn test_88_89_driver_use_only_canonical_resolution() {
    let source = include_str!("../src/driver/test_88_98.rs");
    let source = function_region(
        source,
        "pub(crate) fn run_test_88",
        "pub(crate) fn run_test_91",
    );

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
            "test_88-89 still contains the qtest raw-route marker {forbidden:?}"
        );
    }
    assert!(
        source.contains("pdf.resolve"),
        "test_88-89 must retain the canonical Pdf::resolve route"
    );
}
