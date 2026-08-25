use std::fs;
use std::path::Path;

#[test]
fn orphan_widget_resource_skip_fixture_uses_live_handles() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/page_annotation_flatten.rs"))
        .unwrap()
        .replace("\r\n", "\n");
    let block = source
        .split_once("fn qpdf_flatten_skips_default_resources_for_widget_without_acroform_fields")
        .expect("resource skip test must remain")
        .1
        .split_once(
            "fn qpdf_flatten_rejects_a_direct_stream_when_installing_a_missing_resource_category",
        )
        .expect("resource skip test boundary must remain")
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
            "resource skip fixture must not keep raw route marker {marker:?}"
        );
    }
    for marker in [
        "get_object_handle(",
        "ObjectHandle::stream(",
        "ObjectHandle::dictionary(",
        "replace_object_handle(",
        "replace_key(",
        "mark_object_handle_dirty(",
        "as_stream_dict()",
        "try_get_keys()",
    ] {
        assert!(
            block.contains(marker),
            "resource skip fixture must use live handle accessor {marker:?}"
        );
    }
}
