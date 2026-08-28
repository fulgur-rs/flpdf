//! Route contract for the file-level embedded-files test cutover.

fn test_module(source: &str) -> &str {
    source
        .split_once("#[cfg(test)]")
        .map(|(_, tests)| tests)
        .unwrap_or_else(|| panic!("embedded_files.rs must keep its test module"))
}

#[test]
fn embedded_files_tests_use_only_canonical_fixture_routes() {
    let source = include_str!("embedded_files_tests.rs");
    for forbidden in [
        "resolve_object(",
        "resolve_borrowed(",
        "resolve_to_terminal(",
        "resolve_chain(",
        "materialize(",
        "set_object(",
        "Object::",
        "Dictionary::",
    ] {
        assert!(
            !source.contains(forbidden),
            "embedded_files_tests.rs retains legacy fixture route {forbidden:?}"
        );
    }
    assert!(source.contains("ObjectHandle"));
    assert!(source.contains("pdf.resolve("));
    assert!(source.contains("replace_key"));
}

#[test]
fn embedded_files_module_tests_use_only_canonical_fixture_routes() {
    let source = include_str!("../src/embedded_files.rs");
    let tests = test_module(source);
    for forbidden in [
        "resolve_object(",
        "resolve_borrowed(",
        "resolve_to_terminal(",
        "resolve_chain(",
        "materialize(",
        "set_object(",
        "Object::",
        "Dictionary::",
    ] {
        assert!(
            !tests.contains(forbidden),
            "embedded_files.rs test module retains legacy fixture route {forbidden:?}"
        );
    }
    assert!(tests.contains("ObjectHandle"));
    assert!(tests.contains("pdf.resolve("));
}
