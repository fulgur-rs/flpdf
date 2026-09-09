//! Route contracts for the page-label A6/A7 accessor cutover.

use std::fs;
use std::path::PathBuf;

fn production_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/page_label_document_helper.rs");
    let source = fs::read_to_string(path)
        .expect("page_label_document_helper.rs must be readable")
        .replace("\r\n", "\n");
    source
        .split_once("\n#[cfg(test)]")
        .map_or(source.clone(), |(production, _)| production.to_owned())
}

#[test]
fn production_page_labels_use_canonical_resolving_routes() {
    let production = production_source();
    for forbidden in [".resolve(", ".resolve_handle(", ".resolve_handle_ref("] {
        assert!(
            !production.contains(forbidden),
            "page_label_document_helper production retains non-canonical route {forbidden}"
        );
    }
    assert!(production.contains(".try_dereference()?"));
    assert!(production.contains(".try_get_string_value()?"));
}
