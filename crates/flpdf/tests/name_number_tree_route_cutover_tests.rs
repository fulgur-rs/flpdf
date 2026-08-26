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
    let name_start = nntree.find("pub struct NameTree").expect("NameTree");
    let number_start = nntree.find("pub struct NumberTree").expect("NumberTree");
    let name_surface = &nntree[name_start..number_start];
    assert!(name_surface.contains("pub fn new(root: ObjectHandle"));
    assert!(!name_surface.contains("pub fn new(root: Object,"));
    assert!(!name_surface.contains("value: Object,"));
    assert!(!name_surface.contains("Result<Option<Object>>"));

    let number_start = nntree.find("pub struct NumberTree").expect("NumberTree");
    let implementation_end = nntree
        .find("impl<K: TreeKey> NNTree<K>")
        .expect("NNTree implementation");
    let number_surface = &nntree[number_start..implementation_end];
    assert!(number_surface.contains("pub fn new(root: ObjectHandle"));
    assert!(!number_surface.contains("pub fn new(root: Object,"));
    assert!(!number_surface.contains("value: Object,"));
    assert!(!number_surface.contains("Result<Option<Object>>"));
}

#[test]
fn generic_nntree_uses_only_the_canonical_handle_route() {
    let source = include_str!("../src/nntree.rs");
    for forbidden in [
        "fn from_object(",
        "fn to_object(",
        "pub(crate) fn new(root: Object,",
        "materialize_cursor_value",
        "legacy_root_snapshot",
        "legacy_projection",
        "sync_legacy_root",
        "finish_mutation",
        "lift_value",
        "legacy_terminal_handle",
        "qpdf-deviation",
        "raw: Option<(Object",
        "current: Option<(K::Key, Object)>",
        "use crate::{Dictionary",
        "Object::",
        "resolve_to_terminal(",
        "Result<Option<Object>>",
    ] {
        assert!(
            !source.contains(forbidden),
            "nntree.rs still contains the raw route marker {forbidden:?}"
        );
    }
    for canonical in ["ObjectHandle", "cloned_current", "set_array_items"] {
        assert!(
            source.contains(canonical),
            "nntree.rs must retain the canonical route marker {canonical:?}"
        );
    }
}
