use std::fs;
use std::path::Path;

#[test]
fn flatten_widget_resource_assertions_use_live_handle_accessors() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/page_annotation_flatten.rs"))
        .unwrap()
        .replace("\r\n", "\n");
    let block = source
        .split_once("fn flatten_widget_all_mode")
        .expect("widget flatten test must remain")
        .1
        .split_once("fn placement_matrix_values")
        .expect("widget flatten test boundary must remain")
        .0;

    for marker in [
        "resolve_object(",
        "resolve_borrowed(",
        "Object::",
        "set_object(",
    ] {
        assert!(
            !block.contains(marker),
            "widget resource inspection must not keep raw route marker {marker:?}"
        );
    }
    for marker in ["get_key(", "as_dictionary(", "object_ref(", "has_key("] {
        assert!(
            block.contains(marker),
            "widget resource inspection must use live handle accessor {marker:?}"
        );
    }
}
