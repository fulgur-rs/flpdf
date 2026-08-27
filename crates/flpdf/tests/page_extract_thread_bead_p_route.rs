//! Contract test for the thread-bead extraction test handle cutover.

#[test]
fn thread_bead_extraction_test_uses_canonical_handles() {
    let source = include_str!("page_extract_thread_bead_p_tests.rs");

    for forbidden in [
        "resolve_borrowed(",
        "resolve_object(",
        "Object::",
        ".as_dict()",
        ".into_dict()",
        ".as_ref_id()",
    ] {
        assert!(
            !source.contains(forbidden),
            "thread-bead extraction test still uses legacy route marker {forbidden:?}"
        );
    }
    for required in [
        "ObjectHandle",
        "get_object_handle(",
        "pdf.resolve(",
        ".get_key(",
    ] {
        assert!(
            source.contains(required),
            "thread-bead extraction test must retain canonical marker {required:?}"
        );
    }
}
