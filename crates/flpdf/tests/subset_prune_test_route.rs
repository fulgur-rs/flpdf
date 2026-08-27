//! Contract test for the subset resource-pruning test handle cutover.

#[test]
fn subset_prune_tests_use_canonical_handles() {
    let source = include_str!("../src/subset_prune.rs");
    let tests = source
        .split_once("#[cfg(test)]")
        .expect("subset_prune must have a test module")
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
            "subset_prune tests still use legacy route marker {forbidden:?}"
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
            "subset_prune tests must retain canonical marker {required:?}"
        );
    }
}
