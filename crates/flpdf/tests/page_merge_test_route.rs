//! Contract test for the page-merge test handle cutover.

#[test]
fn page_merge_tests_use_canonical_handles() {
    let source = include_str!("../src/job/page_merge.rs");
    let tests = source
        .split_once("#[cfg(test)]")
        .expect("page_merge must have a test module")
        .1;

    for forbidden in [
        "resolve_borrowed(",
        "resolve_object(",
        "Object::",
        ".as_dict()",
        ".into_dict()",
    ] {
        assert!(
            !tests.contains(forbidden),
            "page_merge tests still use legacy route marker {forbidden:?}"
        );
    }
    for required in [
        "ObjectHandle",
        "get_object_handle(",
        "pdf.resolve(",
        ".get_key(",
    ] {
        assert!(
            tests.contains(required),
            "page_merge tests must retain canonical marker {required:?}"
        );
    }
}
