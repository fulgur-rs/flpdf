//! Route contracts for the page-annotation flattening A6/A7 accessor cutover.

use std::fs;
use std::path::PathBuf;

fn production_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/page_annotation_flatten.rs");
    let source = fs::read_to_string(path)
        .expect("page_annotation_flatten.rs must be readable")
        .replace("\r\n", "\n");
    source
        .split_once("\n#[cfg(test)]\nmod tests")
        .map_or(source.clone(), |(production, _)| production.to_owned())
}

#[test]
fn production_page_annotation_flatten_uses_canonical_resolving_routes() {
    let production = production_source();
    for forbidden in [".resolve(", ".resolve_handle(", ".resolve_handle_ref("] {
        assert!(
            !production.contains(forbidden),
            "page_annotation_flatten production retains non-canonical route {forbidden}"
        );
    }
    assert!(
        production.contains(".try_dereference()?"),
        "stream dictionary inspection must use the canonical handle resolver"
    );
    assert!(production.contains(".try_is_dictionary()?"));
    assert!(production.contains(".try_is_null()?"));
}
