//! Contract tests for the qtest-driver 92 ObjectHandle cutover slice.

fn region<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let start = source
        .find(start)
        .unwrap_or_else(|| panic!("missing start marker {start:?}"));
    let body = &source[start..];
    let end = body
        .find(end)
        .unwrap_or_else(|| panic!("missing end marker {end:?}"));
    &body[..end]
}

#[test]
fn test_92_uses_only_canonical_resolution() {
    let source = include_str!("../src/driver/test_88_98.rs");
    let helper = region(source, "fn resolved_key", "fn is_scalar");
    let test = region(source, "pub(crate) fn run_test_92", "// test_93");
    let source = format!("{helper}\n{test}");

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
            "test_92 still contains the qtest raw-route marker {forbidden:?}"
        );
    }
    assert!(
        source.contains("pdf.resolve"),
        "test_92 must retain the canonical Pdf::resolve route"
    );
}
