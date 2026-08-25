use std::fs;
use std::path::Path;

#[test]
fn attachment_job_tests_do_not_keep_a_raw_object_route() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let module = fs::read_to_string(root.join("src/job/attachments.rs")).unwrap();
    let tests = module
        .split_once("#[cfg(test)]")
        .expect("attachments must keep its test module")
        .1;

    for marker in [
        "use crate::Object;",
        "resolve_borrowed(",
        "resolve_object(",
        "set_object(",
        "Object::",
    ] {
        assert!(
            !tests.contains(marker),
            "attachment job tests must not keep raw Object route marker {marker:?}"
        );
    }
    assert!(
        tests.contains("ObjectHandle"),
        "attachment job tests must inspect fixtures through ObjectHandle"
    );
    assert!(
        tests.contains("replace_key"),
        "attachment job tests must mutate fixtures through the handle API"
    );
    assert!(
        tests.contains("mark_object_handle_dirty"),
        "attachment job tests must mark live handle mutations dirty"
    );
}
