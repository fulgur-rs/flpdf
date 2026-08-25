use std::path::Path;

#[test]
fn public_number_tree_surface_is_object_handle_native() {
    let source_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("nntree.rs");
    let source = std::fs::read_to_string(source_path).expect("read nntree source");
    let number_start = source
        .find("pub struct NumberTree")
        .expect("NumberTree declaration");
    let implementation_end = source
        .find("impl<K: TreeKey> NNTree<K>")
        .expect("NNTree implementation");
    let number_surface = &source[number_start..implementation_end];

    assert!(
        number_surface.contains("ObjectHandle"),
        "NumberTree must expose canonical ObjectHandle values"
    );
    for forbidden in [
        "pub fn new(root: Object,",
        "pub fn root(&self) -> &Object",
        "value: Object,",
        "Result<Option<Object>>",
        "Option<(i64, Object)>",
    ] {
        assert!(
            !number_surface.contains(forbidden),
            "NumberTree still exposes the raw Object route: {forbidden}"
        );
    }
}
