use std::path::Path;

#[test]
fn public_name_tree_surface_is_object_handle_native() {
    let source_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("nntree.rs");
    let source = std::fs::read_to_string(source_path).expect("read nntree source");
    let name_start = source
        .find("pub struct NameTree")
        .expect("NameTree declaration");
    let number_start = source
        .find("pub struct NumberTree")
        .expect("NumberTree declaration");
    let name_surface = &source[name_start..number_start];

    assert!(
        name_surface.contains("ObjectHandle"),
        "NameTree must expose canonical ObjectHandle values"
    );
    for forbidden in [
        "pub fn new(root: Object,",
        "pub fn root(&self) -> &Object",
        "value: Object,",
        "Result<Option<Object>>",
        "Option<(Vec<u8>, Object)>",
    ] {
        assert!(
            !name_surface.contains(forbidden),
            "NameTree still exposes the raw Object route: {forbidden}"
        );
    }
}
