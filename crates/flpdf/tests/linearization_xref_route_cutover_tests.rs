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
    assert!(writer_source.contains("prepare_xref_stream"));
    assert!(!writer_source.contains("encode_payload_uncompressed(&entries"));
    assert!(!writer_source.contains("encode_payload_raw(&entries"));

    let plain_source = include_str!("../src/writer/plain/xref.rs");
    assert!(plain_source.contains("prepare_xref_stream"));
    assert!(!plain_source.contains("field3 = u64::from(generation)"));

    let module_index = include_str!("../../../docs/qpdf-module-doc-index.md");
    assert!(!module_index.contains(&alias_path));
}

#[test]
fn linearized_classic_xref_rows_use_the_shared_qpdf_owner() {
    let writer_source = include_str!("../src/linearization/writer.rs");
    let plain_xref_source = include_str!("../src/writer/plain/xref.rs");

    assert!(
        plain_xref_source.contains("write_xref_table_from_offsets"),
        "the canonical xref owner must expose the linearized offset-map consumer"
    );
    assert!(
        writer_source.contains("write_xref_table_from_offsets"),
        "linearized classic xref rows must call the shared owner"
    );
    assert!(
        !writer_source.contains("for number in 1..param_slot"),
        "the linearized main xref must not retain a private row loop"
    );
}

#[test]
fn shared_classic_xref_rows_keep_the_stack_buffer_fast_path() {
    let plain_xref_source = include_str!("../src/writer/plain/xref.rs");

    assert!(
        plain_xref_source.contains("fn write_fixed_xref_entry"),
        "classic xref rows must have a shared stack-buffer encoder"
    );
    assert!(
        plain_xref_source.contains("write_fixed_xref_entry(out, offset)"),
        "the shared row owner must use the stack-buffer encoder"
    );
    assert!(
        !plain_xref_source.contains(r#"format!("{offset:010} 00000 n \n")"#),
        "classic xref rows must not allocate a temporary formatted string"
    );
}
