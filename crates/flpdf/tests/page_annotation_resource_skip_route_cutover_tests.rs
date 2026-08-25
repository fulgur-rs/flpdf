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
fn malformed_destination_resource_fixture_uses_live_handles() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/page_annotation_flatten.rs"))
        .unwrap()
        .replace("\r\n", "\n");
    let block = source
        .split_once("fn qpdf_flatten_ignores_a_malformed_destination_category_absent_from_dr")
        .expect("malformed destination resource test must remain")
        .1
        .split_once("fn qpdf_flatten_appends_an_indirect_scalar_item_from_dr")
        .expect("malformed destination resource test boundary must remain")
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
            "malformed destination resource fixture must not keep raw route marker {marker:?}"
        );
    }
    for marker in [
        "get_object_handle(",
        "ObjectHandle::stream(",
        "ObjectHandle::dictionary(",
        "ObjectHandle::name(",
        "replace_object_handle(",
        "as_stream_dict()",
        "try_get_key(",
    ] {
        assert!(
            block.contains(marker),
            "malformed destination resource fixture must use live handle accessor {marker:?}"
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
        .split_once("fn qpdf_flatten_marks_an_indirect_array_category_dirty_after_merge")
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

#[test]
fn indirect_array_category_dirty_fixture_uses_live_handles() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/page_annotation_flatten.rs"))
        .unwrap()
        .replace("\r\n", "\n");
    let block = source
        .split_once("fn qpdf_flatten_marks_an_indirect_array_category_dirty_after_merge")
        .expect("indirect array category dirty test must remain")
        .1
        .split_once(
            "fn qpdf_flatten_keeps_an_earlier_indirect_array_merge_dirty_after_a_later_category_fails",
        )
        .expect("indirect array category dirty test boundary must remain")
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
            "indirect array category dirty fixture must not keep raw route marker {marker:?}"
        );
    }
    for marker in [
        "get_object_handle(",
        "ObjectHandle::array(",
        "ObjectHandle::dictionary(",
        "ObjectHandle::stream(",
        "replace_object_handle(",
        "resolve(",
        "as_array()",
        "object_ref()",
    ] {
        assert!(
            block.contains(marker),
            "indirect array category dirty fixture must use live handle accessor {marker:?}"
        );
    }
}

#[test]
fn earlier_indirect_array_merge_failure_fixture_uses_live_handles() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/page_annotation_flatten.rs"))
        .unwrap()
        .replace("\r\n", "\n");
    let block = source
        .split_once(
            "fn qpdf_flatten_keeps_an_earlier_indirect_array_merge_dirty_after_a_later_category_fails",
        )
        .expect("earlier indirect array merge failure test must remain")
        .1
        .split_once("fn qpdf_flatten_installs_an_array_default_resource_category_absent_from_the_appearance")
        .expect("earlier indirect array merge failure test boundary must remain")
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
            "earlier indirect array merge failure fixture must not keep raw route marker {marker:?}"
        );
    }
    for marker in [
        "get_object_handle(",
        "ObjectHandle::array(",
        "ObjectHandle::dictionary(",
        "ObjectHandle::stream(",
        "replace_object_handle(",
        "resolve(",
        "as_array()",
        "object_ref()",
        "expect_err(",
        "Error::System",
    ] {
        assert!(
            block.contains(marker),
            "earlier indirect array merge failure fixture must use live handle accessor {marker:?}"
        );
    }
}

#[test]
fn missing_array_resource_category_fixture_uses_live_handles() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/page_annotation_flatten.rs"))
        .unwrap()
        .replace("\r\n", "\n");
    let block = source
        .split_once(
            "fn qpdf_flatten_installs_an_array_default_resource_category_absent_from_the_appearance",
        )
        .expect("missing array resource category test must remain")
        .1
        .split_once(
            "fn qpdf_flatten_merges_an_array_default_resource_category_deduping_existing_scalars",
        )
        .expect("missing array resource category test boundary must remain")
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
            "missing array resource category fixture must not keep raw route marker {marker:?}"
        );
    }
    for marker in [
        "get_object_handle(",
        "ObjectHandle::array(",
        "ObjectHandle::dictionary(",
        "ObjectHandle::stream(",
        "replace_object_handle(",
        "resolve(",
        "as_stream_dict()",
        "try_get_key(",
        "as_array()",
    ] {
        assert!(
            block.contains(marker),
            "missing array resource category fixture must use live handle accessor {marker:?}"
        );
    }
}

#[test]
fn dedup_existing_scalars_array_resource_fixture_uses_live_handles() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/page_annotation_flatten.rs"))
        .unwrap()
        .replace("\r\n", "\n");
    let block = source
        .split_once(
            "fn qpdf_flatten_merges_an_array_default_resource_category_deduping_existing_scalars",
        )
        .expect("dedup existing scalars test must remain")
        .1
        .split_once("fn qpdf_flatten_leaves_a_type_mismatched_default_resource_category_untouched")
        .expect("dedup existing scalars test boundary must remain")
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
            "dedup existing scalars fixture must not keep raw route marker {marker:?}"
        );
    }
    for marker in [
        "get_object_handle(",
        "ObjectHandle::array(",
        "ObjectHandle::dictionary(",
        "ObjectHandle::stream(",
        "replace_object_handle(",
        "resolve(",
        "as_stream_dict()",
        "try_get_key(",
        "as_array()",
    ] {
        assert!(
            block.contains(marker),
            "dedup existing scalars fixture must use live handle accessor {marker:?}"
        );
    }
}

#[test]
fn type_mismatched_resource_category_fixture_uses_live_handles() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/page_annotation_flatten.rs"))
        .unwrap()
        .replace("\r\n", "\n");
    let block = source
        .split_once("fn qpdf_flatten_leaves_a_type_mismatched_default_resource_category_untouched")
        .expect("type mismatched resource category test must remain")
        .1
        .split_once("fn qpdf_flatten_array_merge_excludes_non_scalar_items")
        .expect("type mismatched resource category test boundary must remain")
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
            "type mismatched resource category fixture must not keep raw route marker {marker:?}"
        );
    }
    for marker in [
        "get_object_handle(",
        "ObjectHandle::array(",
        "ObjectHandle::dictionary(",
        "ObjectHandle::stream(",
        "ObjectHandle::integer(",
        "replace_object_handle(",
        "resolve(",
        "as_stream_dict()",
        "try_get_key(",
        "as_integer()",
    ] {
        assert!(
            block.contains(marker),
            "type mismatched resource category fixture must use live handle accessor {marker:?}"
        );
    }
}

#[test]
fn array_non_scalar_resource_fixture_uses_live_handles() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/page_annotation_flatten.rs"))
        .unwrap()
        .replace("\r\n", "\n");
    let block = source
        .split_once("fn qpdf_flatten_array_merge_excludes_non_scalar_items")
        .expect("array non-scalar merge test must remain")
        .1
        .split_once("fn qpdf_flatten_ignores_direct_widget_inline_appearance_for_resource_merge")
        .expect("array non-scalar merge test boundary must remain")
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
            "array non-scalar fixture must not keep raw route marker {marker:?}"
        );
    }
    for marker in [
        "get_object_handle(",
        "ObjectHandle::array(",
        "ObjectHandle::dictionary(",
        "ObjectHandle::stream(",
        "ObjectHandle::integer(",
        "replace_object_handle(",
        "resolve(",
        "as_stream_dict()",
        "try_get_key(",
        "as_array()",
        "as_integer()",
    ] {
        assert!(
            block.contains(marker),
            "array non-scalar fixture must use live handle accessor {marker:?}"
        );
    }
}

#[test]
fn direct_inline_widget_resource_merge_fixture_uses_live_handles() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/page_annotation_flatten.rs"))
        .unwrap()
        .replace("\r\n", "\n");
    let block = source
        .split_once("fn qpdf_flatten_ignores_direct_widget_inline_appearance_for_resource_merge")
        .expect("direct inline widget appearance test must remain")
        .1
        .split_once("fn qpdf_flatten_wraps_content_when_dropping_an_unselected_appearance")
        .expect("direct inline widget appearance test boundary must remain")
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
            "direct inline widget appearance fixture must not keep raw route marker {marker:?}"
        );
    }
    for marker in [
        "get_object_handle(",
        "ObjectHandle::array(",
        "ObjectHandle::dictionary(",
        "ObjectHandle::stream(",
        "ObjectHandle::name(",
        "ObjectHandle::integer(",
        "replace_key(",
        "mark_object_handle_dirty(",
        "resolve(",
        "try_get_key(",
        "as_array()",
    ] {
        assert!(
            block.contains(marker),
            "direct inline widget appearance fixture must use live handle accessor {marker:?}"
        );
    }
}
