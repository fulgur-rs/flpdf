//! Route contracts for the outline A6/A7 accessor cutover.

use std::fs;
use std::path::PathBuf;

fn production_source(path: &str) -> String {
    let source = fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path))
        .expect("outline source must be readable")
        .replace("\r\n", "\n");
    source
        .split_once("\n#[cfg(test)]")
        .map_or(source.clone(), |(production, _)| production.to_owned())
}

#[test]
fn outline_helpers_have_no_explicit_resolve_bridges() {
    for path in [
        "src/outline_document_helper.rs",
        "src/outline_object_helper.rs",
    ] {
        let source = production_source(path);
        for forbidden in [".resolve(", ".resolve_handle(", ".resolve_handle_ref("] {
            assert!(
                !source.contains(forbidden),
                "{path} retains non-canonical route {forbidden}"
            );
        }
    }
}
