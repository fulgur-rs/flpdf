use std::fs;
use std::path::Path;

#[test]
fn rotate_post_apply_inspection_uses_the_live_handle_route() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/job/rotate.rs"))
        .unwrap()
        .replace("\r\n", "\n");
    for test_name in [
        "fn apply_to_empty_slice_is_noop",
        "fn rejects_pages_tree_node_target",
        "fn round_trip_rotate_preserved_after_write_reopen",
        "fn round_trip_inherited_rotate_materialized_on_leaf",
        "fn apply_to_multiple_pages_each_updated_independently",
    ] {
        assert!(
            source.contains(test_name),
            "post-apply rotate test {test_name:?} must remain in the source"
        );
    }

    let tests = source
        .split_once("fn apply_to_empty_slice_is_noop")
        .expect("post-apply rotate tests must remain")
        .1
        .split_once("// flatten_rotation_on_pages")
        .expect("post-apply rotate test block must remain before flatten tests")
        .0;

    for marker in [
        "resolve_object(",
        "resolve_borrowed(",
        "Object::",
        "set_object(",
    ] {
        assert!(
            !tests.contains(marker),
            "post-apply rotate inspection must not keep raw route marker {marker:?}"
        );
    }
    assert!(
        tests.contains("rotate_value("),
        "post-apply rotate inspection must use the live rotate_value helper"
    );
}
