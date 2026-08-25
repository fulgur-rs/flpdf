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

#[test]
fn direct_stream_resource_error_fixture_uses_live_handles() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/page_annotation_flatten.rs"))
        .unwrap()
        .replace("\r\n", "\n");
    let block = source
        .split_once(
            "fn qpdf_flatten_rejects_a_direct_stream_when_installing_a_missing_resource_category",
        )
        .expect("direct-stream resource error test must remain")
        .1
        .split_once("fn qpdf_document_flatten_propagates_default_resource_merge_error")
        .expect("direct-stream resource error test boundary must remain")
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
            "direct-stream resource error fixture must not keep raw route marker {marker:?}"
        );
    }
    for marker in [
        "get_object_handle(",
        "ObjectHandle::stream(",
        "ObjectHandle::dictionary(",
        "replace_object_handle(",
    ] {
        assert!(
            block.contains(marker),
            "direct-stream resource error fixture must use live handle accessor {marker:?}"
        );
    }
}

#[test]
fn document_flatten_resource_error_fixture_uses_live_handles() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/page_annotation_flatten.rs"))
        .unwrap()
        .replace("\r\n", "\n");
    let block = source
        .split_once("fn qpdf_document_flatten_propagates_default_resource_merge_error")
        .expect("document flatten resource error test must remain")
        .1
        .split_once("fn qpdf_flatten_privatizes_an_indirect_appearance_resources_before_merging")
        .expect("document flatten resource error test boundary must remain")
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
            "document flatten resource error fixture must not keep raw route marker {marker:?}"
        );
    }
    for marker in [
        "get_object_handle(",
        "ObjectHandle::stream(",
        "ObjectHandle::dictionary(",
        "replace_object_handle(",
        "replace_key(",
        "mark_object_handle_dirty(",
    ] {
        assert!(
            block.contains(marker),
            "document flatten resource error fixture must use live handle accessor {marker:?}"
        );
    }
}

#[test]
fn indirect_appearance_resources_fixture_uses_live_handles() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/page_annotation_flatten.rs"))
        .unwrap()
        .replace("\r\n", "\n");
    let block = source
        .split_once("fn qpdf_flatten_privatizes_an_indirect_appearance_resources_before_merging")
        .expect("indirect appearance resource test must remain")
        .1
        .split_once("fn qpdf_flatten_ignores_a_malformed_destination_category_absent_from_dr")
        .expect("indirect appearance resource test boundary must remain")
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
            "indirect appearance resource fixture must not keep raw route marker {marker:?}"
        );
    }
    for marker in [
        "get_object_handle(",
        "ObjectHandle::stream(",
        "ObjectHandle::dictionary(",
        "ObjectHandle::integer(",
        "replace_object_handle(",
        "as_stream_dict()",
        "try_get_key(",
    ] {
        assert!(
            block.contains(marker),
            "indirect appearance resource fixture must use live handle accessor {marker:?}"
        );
    }
}

#[test]
fn indirect_scalar_resource_fixture_uses_live_handles() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/page_annotation_flatten.rs"))
        .unwrap()
        .replace("\r\n", "\n");
    let block = source
        .split_once("fn qpdf_flatten_appends_an_indirect_scalar_item_from_dr")
        .expect("indirect scalar resource test must remain")
        .1
        .split_once("fn qpdf_flatten_terminal_chases_a_holder_redirect_category_and_array_item")
        .expect("indirect scalar resource test boundary must remain")
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
            "indirect scalar resource fixture must not keep raw route marker {marker:?}"
        );
    }
    for marker in [
        "get_object_handle(",
        "ObjectHandle::stream(",
        "ObjectHandle::dictionary(",
        "ObjectHandle::array(",
        "ObjectHandle::name(",
        "replace_object_handle(",
        "as_stream_dict()",
        "try_get_key(",
    ] {
        assert!(
            block.contains(marker),
            "indirect scalar resource fixture must use live handle accessor {marker:?}"
        );
    }
}
