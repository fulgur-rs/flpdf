//! Contract test for the structure-tree extraction test handle cutover.

#[test]
fn struct_tree_extraction_test_uses_canonical_handles() {
    let source = include_str!("page_extract_structtree_pg_tests.rs");

    for forbidden in [
        "resolve_borrowed(",
        "resolve_object(",
        "Object::",
        ".as_dict()",
    ] {
        assert!(
            !source.contains(forbidden),
            "struct-tree extraction test still uses legacy route marker {forbidden:?}"
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
            "struct-tree extraction test must retain canonical marker {required:?}"
        );
    }
}
