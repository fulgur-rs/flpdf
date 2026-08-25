use std::fs;
use std::path::Path;

#[test]
fn malformed_multihop_contents_fixture_is_not_retained() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/page_annotation_flatten.rs"))
        .unwrap()
        .replace("\r\n", "\n");

    assert!(
        !source.contains("fn qpdf_flatten_expands_a_multihop_contents_array"),
        "the bare indirect-object contents fixture is a qpdf-deviation test and must be removed"
    );
}
