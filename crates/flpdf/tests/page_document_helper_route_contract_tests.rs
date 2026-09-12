//! Route contracts for the page-document A6/A7 accessor cutover.

use std::fs;
use std::path::PathBuf;

fn source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/page_document_helper.rs");
    fs::read_to_string(path)
        .expect("page_document_helper.rs must be readable")
        .replace("\r\n", "\n")
}

#[test]
fn page_document_helper_has_no_explicit_resolve_bridge() {
    let source = source();
    for forbidden in [".resolve(", ".resolve_handle(", ".resolve_handle_ref("] {
        assert!(
            !source.contains(forbidden),
            "page_document_helper retains non-canonical route {forbidden}"
        );
    }
    assert!(source.contains(".try_is_dictionary()?"));
}
