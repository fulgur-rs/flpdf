use std::fs;
use std::path::Path;

#[test]
fn rotate_pagebox_helper_uses_the_canonical_handle_route() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/job/rotate.rs"))
        .unwrap()
        .replace("\r\n", "\n");
    let tests = source
        .split_once("mod tests {")
        .expect("rotate must keep its test module")
        .1;
    let pagebox_slice = tests
        .split_once("// -----------------------------------------------------------------------\n    // PDF builder helpers")
        .expect("rotate pagebox helper must precede the PDF builders")
        .0;

    for marker in [
        "use crate::Object",
        "Object::",
        "resolve_object(",
        "resolve_borrowed(",
        "set_object(",
        "materialize(",
    ] {
        assert!(
            !pagebox_slice.contains(marker),
            "rotate pagebox helper must not keep raw route marker {marker:?}"
        );
    }
    assert!(
        pagebox_slice.contains("ObjectHandle"),
        "rotate pagebox helper must use ObjectHandle"
    );
    assert!(
        pagebox_slice.contains("as_array"),
        "rotate pagebox helper must inspect arrays through typed handle access"
    );
    assert!(
        !source.contains("object_to_pagebox("),
        "rotate must not retain the removed raw pagebox helper"
    );
}
