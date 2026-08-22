use std::path::Path;

#[test]
fn tree_compatibility_route_is_removed_after_cutover() {
    let module_name = ["name", "number", "tree"].join("_");
    let old_functions = [
        ["read", "name", "tree"].join("_"),
        ["read", "number", "tree"].join("_"),
        ["build", "name", "tree"].join("_"),
        ["build", "number", "tree"].join("_"),
    ];
    let source_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join(format!("{module_name}.rs"));
    assert!(
        !source_path.exists(),
        "the raw tree compatibility source must be deleted"
    );

    for (label, source) in [
        ("flpdf::lib", include_str!("../src/lib.rs")),
        (
            "page_label_document_helper",
            include_str!("../src/page_label_document_helper.rs"),
        ),
        ("embedded_files", include_str!("../src/embedded_files.rs")),
    ] {
        assert!(
            !source.contains(&module_name),
            "{label} still imports the old tree module"
        );
        for function in &old_functions {
            assert!(
                !source.contains(function),
                "{label} still references {function}"
            );
        }
    }

    let nntree = include_str!("../src/nntree.rs");
    assert!(nntree.contains("pub struct NameTree"));
    assert!(nntree.contains("pub struct NumberTree"));
}
