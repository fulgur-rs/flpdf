//! The JSON v2 object-map key must use the allocation-free qpdf-shaped key
//! writer rather than formatting an owned Rust `String` per object.

use std::fs;
use std::path::Path;

fn document_json_source() -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/document_json.rs"))
        .expect("document_json.rs")
}

#[test]
fn v2_object_entries_use_the_stack_object_key_writer() {
    let source = document_json_source();
    assert!(
        source.contains("write_qpdf_object_key"),
        "JSON v2 object entries must use the stack object-key formatter"
    );
    assert!(
        !source.contains("let key = format!(\"obj:{} {} R\""),
        "JSON v2 object entries must not allocate a formatted String per object"
    );
}
