use std::fs;
use std::path::Path;

#[test]
fn indirect_default_resource_category_fixture_uses_live_handles() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/page_annotation_flatten.rs"))
        .unwrap()
        .replace("\r\n", "\n");
    let block = source
        .split_once("fn qpdf_flatten_merges_an_indirect_default_resource_category")
        .expect("resource merge test must remain")
        .1
        .split_once("fn qpdf_flatten_skips_default_resources_for_widget_without_acroform_fields")
        .expect("resource merge test boundary must remain")
        .0;

    for marker in [
        "resolve_object(",
        "resolve_borrowed(",
        "Object::",
        "set_object(",
        "materialize(",
        "lift_object_to_handle(",
    ] {
        assert!(
            !block.contains(marker),
            "resource merge fixture must not keep raw route marker {marker:?}"
        );
    }
    for marker in [
        "get_object_handle(",
        "ObjectHandle::stream(",
        "ObjectHandle::dictionary(",
        "replace_object_handle(",
        "replace_key(",
        "mark_object_handle_dirty(",
        "try_get_key(",
        "as_stream_dict()",
        "object_ref()",
    ] {
        assert!(
            block.contains(marker),
            "resource merge fixture must use live handle accessor {marker:?}"
        );
    }
}
