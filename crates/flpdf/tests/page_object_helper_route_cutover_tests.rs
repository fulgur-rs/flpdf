//! Route contract for the PageObjectHelper integration-test cutover.

#[test]
fn page_object_helper_tests_use_canonical_handle_inspection() {
    let source = include_str!("page_object_helper_tests.rs");

    for forbidden in [
        "resolve_object(",
        "resolve_borrowed(",
        "resolve_to_terminal(",
        "resolve_chain(",
        "materialize(",
        "set_object(",
        "Object::Dictionary",
        ".into_dict(",
        ".as_dict(",
        ".as_ref_id(",
    ] {
        assert!(
            !source.contains(forbidden),
            "page_object_helper_tests.rs retains legacy route marker: {forbidden}"
        );
    }

    for required in [
        "ObjectHandle",
        "get_object_handle(",
        "resolve(&",
        "get_key(",
        "object_ref(",
    ] {
        assert!(
            source.contains(required),
            "page_object_helper_tests.rs must use canonical route marker: {required}"
        );
    }
}
