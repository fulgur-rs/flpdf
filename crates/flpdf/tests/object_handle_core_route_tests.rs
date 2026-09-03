//! Route guards for the qpdf-shaped ObjectHandle core.

#[test]
fn object_handle_unparse_production_has_no_raw_materialization_helper() {
    // Git's default autocrlf=true checkout converts this source to CRLF on
    // Windows; keep the structural guard independent of checkout line endings.
    let source = include_str!("../src/object_handle.rs").replace("\r\n", "\n");
    let unparse_resolved = source
        .split("pub fn unparse_resolved")
        .nth(1)
        .and_then(|tail| tail.split("pub fn try_unparse_resolved").next())
        .expect("ObjectHandle has an unparse_resolved method");
    let body = unparse_resolved
        .split_once('{')
        .and_then(|(_, tail)| tail.split_once("\n    }\n\n").map(|(body, _)| body))
        .expect("unparse_resolved has a function body");

    assert!(!unparse_resolved.contains("unparse_materialize"));
    assert!(!unparse_resolved.contains("unparse_drop_iteratively"));
    assert!(!body.contains(".materialize("));
    assert!(!body.contains("Object::"));
}
