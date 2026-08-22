use std::path::Path;

#[test]
fn linearization_xref_stream_has_one_canonical_owner() {
    let alias_path = ["linearization", "xref_stream.rs"].join("/");
    let alias_module = ["mod", "xref_stream"].join(" ");
    let source_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join(&alias_path);
    assert!(
        !source_path.exists(),
        "the dead compatibility namespace must be deleted"
    );

    let module_source = include_str!("../src/linearization/mod.rs");
    assert!(!module_source.contains(&alias_module));

    let writer_source = include_str!("../src/linearization/writer.rs");
    assert!(writer_source.contains("serialize::xref_stream"));

    let module_index = include_str!("../../../docs/qpdf-module-doc-index.md");
    assert!(!module_index.contains(&alias_path));
}
