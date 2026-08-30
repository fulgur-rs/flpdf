//! Route guards for the qpdf-shaped ObjectHandle core cutover.

#[test]
fn object_handle_unparse_production_has_no_raw_materialization_helper() {
    let source = include_str!("../src/object_handle.rs");
    let unparse_resolved = source
        .split("pub fn unparse_resolved")
        .nth(1)
        .and_then(|tail| tail.split("pub fn try_unparse_resolved").next())
        .expect("ObjectHandle has an unparse_resolved method");

    assert!(!unparse_resolved.contains("unparse_materialize"));
    assert!(!unparse_resolved.contains("unparse_drop_iteratively"));
}
