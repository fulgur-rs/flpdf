use std::fs;
use std::path::Path;

#[test]
fn page_split_tests_do_not_keep_a_raw_object_route() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/job/page_split.rs")).unwrap();
    let tests = source
        .split_once("#[cfg(test)]")
        .expect("page_split must keep its test module")
        .1;

    for marker in [
        "use crate::{Object,",
        "crate::Dictionary",
        "resolve_object(",
        "resolve_borrowed(",
        "Object::",
        "materialize(",
        "set_object(",
    ] {
        assert!(
            !tests.contains(marker),
            "page_split tests must not keep raw Object route marker {marker:?}"
        );
    }
    assert!(
        tests.contains("ObjectHandle"),
        "page_split tests must inspect fixtures through ObjectHandle"
    );
    assert!(
        tests.contains("root_handle()"),
        "page_split tests must start catalog inspection from the live root handle"
    );
}
