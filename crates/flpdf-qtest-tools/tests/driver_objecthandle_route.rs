//! Contract tests for the first `.19` qtest-driver cutover slice.

#[test]
fn test_0_1_driver_has_no_raw_resolution_or_materialization_route() {
    let handle = include_str!("../src/driver/handle.rs");
    let test_0_1 = include_str!("../src/driver/test_0_1.rs");
    let test_26_33 = include_str!("../src/driver/test_26_33.rs");

    for (name, source) in [
        ("handle.rs", handle),
        ("test_0_1.rs", test_0_1),
        ("test_26_33.rs", test_26_33),
    ] {
        for forbidden in [
            "resolve_borrowed(",
            "resolve_object(",
            "resolve_chain(",
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
    }
}
