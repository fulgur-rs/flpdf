//! Contract tests for the qtest-driver 34-41 ObjectHandle cutover slice.

#[test]
fn test_34_41_driver_has_no_terminal_resolution_bridge() {
    let source = include_str!("../src/driver/test_34_41.rs");

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
            "test_34_41.rs still contains the qtest raw-route marker {forbidden:?}"
        );
    }
    assert!(
        source.contains("ObjectHandle"),
        "test_34_41.rs must retain the canonical ObjectHandle route"
    );
}
