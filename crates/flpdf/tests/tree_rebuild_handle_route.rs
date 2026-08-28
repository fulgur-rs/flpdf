//! Contract test for the tree-rebuild test-module handle cutover.

#[test]
fn tree_rebuild_tests_use_canonical_handles() {
    let source = include_str!("../src/pages/tree_rebuild.rs");
    let tests = source
        .split_once("#[cfg(test)]\nmod tests {")
        .map(|(_, tests)| tests)
        .expect("tree_rebuild test module");

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
            !tests.contains(forbidden),
            "tree_rebuild test module still uses legacy route marker {forbidden:?}"
        );
    }

    for required in [
        "ObjectHandle",
        "get_object_handle(",
        "resolve(&",
        "object_ref(",
    ] {
        assert!(
            tests.contains(required),
            "tree_rebuild test module must retain canonical marker {required:?}"
        );
    }
}
