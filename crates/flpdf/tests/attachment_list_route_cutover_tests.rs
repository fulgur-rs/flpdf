use std::fs;
use std::path::Path;

#[test]
fn attachment_list_tests_do_not_keep_a_raw_object_route() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let module = fs::read_to_string(root.join("src/job/attachment_list.rs")).unwrap();
    let tests = module
        .split_once("#[cfg(test)]")
        .expect("attachment_list must keep its test module")
        .1;

    for marker in [
        "use crate::object::{Dictionary, Object, Stream};",
        "resolve_object(",
        "resolve_borrowed(",
        "set_object(",
        "Object::",
    ] {
        assert!(
            !tests.contains(marker),
            "attachment_list tests must not keep raw Object route marker {marker:?}"
        );
    }
    assert!(
        tests.contains("ObjectHandle::"),
        "attachment_list tests must construct fixtures through ObjectHandle"
    );
    assert!(
        tests.contains("set_object_handle"),
        "attachment_list tests must write fixtures through the handle API"
    );
}
