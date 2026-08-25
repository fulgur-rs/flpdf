use std::fs;
use std::path::Path;

#[test]
fn page_specs_tests_do_not_keep_a_raw_object_route() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let module = fs::read_to_string(root.join("src/job/page_specs.rs")).unwrap();
    let tests = module
        .split_once("#[cfg(test)]")
        .expect("page_specs must keep its test module")
        .1;

    for marker in [
        "use crate::Object;",
        "resolve_borrowed(",
        "resolve_object(",
        "Object::",
    ] {
        assert!(
            !tests.contains(marker),
            "page_specs tests must not keep raw Object route marker {marker:?}"
        );
    }
    assert!(
        tests.contains("ObjectHandle"),
        "page_specs tests must inspect fixtures through ObjectHandle"
    );
    assert!(
        tests.contains("mark_object_handle_dirty"),
        "page_specs tests must use the live handle mutation boundary"
    );
}
