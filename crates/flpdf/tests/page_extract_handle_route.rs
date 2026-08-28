//! Contract test for the page-extraction integration-test handle cutover.

#[test]
fn page_extract_tests_use_canonical_handles() {
    let source = include_str!("page_extract_tests.rs");

    for forbidden in [
        "resolve_borrowed(",
        "resolve_object(",
        "resolve_to_terminal(",
        "resolve_chain(",
        "materialize(",
        "set_object(",
        "Object::",
        "Dictionary::",
        ".as_dict(",
        ".into_dict(",
        ".as_ref_id(",
    ] {
        assert!(
            !source.contains(forbidden),
            "page extraction test still uses legacy route marker {forbidden:?}"
        );
    }

    for required in [
        "ObjectHandle",
        "get_object_handle(",
        "resolve(&",
        ".get_key(",
        ".object_ref(",
    ] {
        assert!(
            source.contains(required),
            "page extraction test must retain canonical marker {required:?}"
        );
    }
}
