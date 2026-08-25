use std::fs;
use std::path::Path;

#[test]
fn holder_redirect_fixture_is_not_retained() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/page_annotation_flatten.rs"))
        .unwrap()
        .replace("\r\n", "\n");

    assert!(
        !source.contains("fn qpdf_flatten_terminal_chases_a_holder_redirect_category_and_array_item"),
        "the bare indirect-object holder redirect fixture is a qpdf-deviation test and must be removed"
    );
}
