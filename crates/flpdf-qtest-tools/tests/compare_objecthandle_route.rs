//! Contract tests for the qtest compare/cleanup cutover slice.

#[test]
fn compare_and_cleanup_use_only_canonical_object_handles() {
    let clean = include_str!("../src/clean.rs");
    let compare = include_str!("../src/compare.rs");

    for (name, source) in [("clean.rs", clean), ("compare.rs", compare)] {
        for forbidden in [
            "resolve_borrowed(",
            "resolve_object(",
            "resolve_to_terminal(",
            "resolve_to_terminal_ref(",
            "materialize(",
            "Object::",
        ] {
            assert!(
                !source.contains(forbidden),
                "{name} still contains the qtest raw-route marker {forbidden:?}"
            );
        }
        assert!(
            source.contains("ObjectHandle"),
            "{name} must retain the canonical ObjectHandle route"
        );
    }
}
