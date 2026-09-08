//! Route contracts for the bounded page-object helper A6/A7/A8 cutover.

use std::fs;
use std::path::PathBuf;

fn production_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/page_object_helper.rs");
    let source = fs::read_to_string(path).expect("page_object_helper.rs must be readable");
    source
        .split_once("\n#[cfg(test)]")
        .map_or(source.clone(), |(production, _)| production.to_owned())
}

#[test]
fn production_page_object_helper_uses_resolving_accessor_routes() {
    let production = production_source();
    for forbidden in [
        ".resolve(",
        ".resolve_handle(",
        ".resolve_handle_ref(",
        ".get_key(",
        ".has_key(",
    ] {
        assert!(
            !production.contains(forbidden),
            "page_object_helper production retains non-canonical route {forbidden}"
        );
    }
}
