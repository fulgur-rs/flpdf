use std::path::Path;

#[test]
fn flpdf_name_tree_destination_adapter_is_removed() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let module_file = ["src/name_tree", "_dests.rs"].concat();
    assert!(
        !manifest.join(&module_file).exists(),
        "the flpdf-only name-tree destination adapter must be deleted"
    );

    let lib = include_str!("../src/lib.rs");
    assert!(!lib.contains(&["pub mod name_tree", "_dests"].concat()));
    assert!(!lib.contains(&["insert_name_tree_", "dest"].concat()));
    assert!(!lib.contains(&["delete_name_tree_", "dest"].concat()));
    assert!(!lib.contains(&["DEFAULT_MAX_NAME_TREE_", "DESTS_DEPTH"].concat()));

    let module_index = include_str!("../../../docs/qpdf-module-doc-index.md");
    assert!(!module_index.contains(&module_file));
}
