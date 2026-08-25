use std::fs;
use std::path::Path;

#[test]
fn empty_document_flatten_resources_assertion_uses_live_handles() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/page_annotation_flatten.rs"))
        .unwrap()
        .replace("\r\n", "\n");
    let block = source
        .split_once("fn qpdf_document_flatten_empty_page_exercises_public_contract")
        .expect("empty flatten test must remain")
        .1
        .split_once("fn direct_page_rotate_resolves_an_indirect_integer")
        .expect("empty flatten test boundary must remain")
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
            "empty flatten inspection must not keep raw route marker {marker:?}"
        );
    }
    for marker in ["get_object_handle(", "get_key(", "as_dictionary("] {
        assert!(
            block.contains(marker),
            "empty flatten inspection must use live handle accessor {marker:?}"
        );
    }
}
