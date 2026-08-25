use std::fs;
use std::path::Path;

#[test]
fn flatten_annotation_removal_assertion_uses_the_live_handle_route() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/page_annotation_flatten.rs"))
        .unwrap()
        .replace("\r\n", "\n");
    let block = source
        .split_once("fn flatten_annotations_preserves_direct_annotation_handle_until_removal")
        .expect("direct annotation flatten test must remain")
        .1
        .split_once("fn flatten_widget_all_mode")
        .expect("direct annotation flatten test boundary must remain")
        .0;

    for marker in [
        "resolve_object(",
        "resolve_borrowed(",
        "Object::",
        "set_object(",
    ] {
        assert!(
            !block.contains(marker),
            "annotation removal inspection must not keep raw route marker {marker:?}"
        );
    }
    assert!(
        block.contains("get_annotation_handles("),
        "annotation removal inspection must use qpdf-shaped annotation handles"
    );
}
