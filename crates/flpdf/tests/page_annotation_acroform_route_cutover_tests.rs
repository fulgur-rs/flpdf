use std::fs;
use std::path::Path;

#[test]
fn direct_need_appearances_fixture_uses_live_handle_mutation() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/page_annotation_flatten.rs"))
        .unwrap()
        .replace("\r\n", "\n");
    let block = source
        .split_once("fn acroform_need_appearances_reads_a_direct_boolean")
        .expect("direct NeedAppearances test must remain")
        .1
        .split_once("fn qpdf_flatten_expands_a_multihop_contents_array")
        .expect("direct NeedAppearances test boundary must remain")
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
            "direct NeedAppearances fixture must not keep raw route marker {marker:?}"
        );
    }
    for marker in [
        "root_ref()",
        "get_object_handle(",
        "ObjectHandle::dictionary(",
        "ObjectHandle::boolean(",
        "replace_key(",
        "mark_object_handle_dirty(",
    ] {
        assert!(
            block.contains(marker),
            "direct NeedAppearances fixture must use live mutation accessor {marker:?}"
        );
    }
}
