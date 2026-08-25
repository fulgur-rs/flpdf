use std::fs;
use std::path::Path;

#[test]
fn indirect_page_rotate_fixture_uses_live_handle_mutation() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/page_annotation_flatten.rs"))
        .unwrap()
        .replace("\r\n", "\n");
    let block = source
        .split_once("fn direct_page_rotate_resolves_an_indirect_integer")
        .expect("direct page rotate test must remain")
        .1
        .split_once("fn acroform_need_appearances_reads_a_direct_boolean")
        .expect("direct page rotate test boundary must remain")
        .0;

    for marker in [
        "resolve_object(",
        "resolve_borrowed(",
        "Object::",
        "set_object(",
        "materialize(",
    ] {
        assert!(
            !block.contains(marker),
            "direct rotate fixture must not keep raw route marker {marker:?}"
        );
    }
    for marker in [
        "get_object_handle(",
        "replace_key(",
        "replace_object_handle(",
        "mark_object_handle_dirty(",
    ] {
        assert!(
            block.contains(marker),
            "direct rotate fixture must use live mutation accessor {marker:?}"
        );
    }
}
