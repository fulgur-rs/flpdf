//! Route contracts for the page-form-xobject A6/A7 accessor cutover.

use std::fs;
use std::path::PathBuf;

fn production_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/page_form_xobject.rs");
    let source = fs::read_to_string(path)
        .expect("page_form_xobject.rs must be readable")
        .replace("\r\n", "\n");
    source
        .split_once("\n#[cfg(test)]\nmod tests")
        .map_or(source.clone(), |(production, _)| production.to_owned())
}

#[test]
fn production_page_form_xobject_uses_canonical_resolving_routes() {
    let production = production_source();
    for forbidden in [".resolve(", ".resolve_handle(", ".resolve_handle_ref("] {
        assert!(
            !production.contains(forbidden),
            "page_form_xobject production retains non-canonical route {forbidden}"
        );
    }
    assert!(
        production.contains("PageObjectHelper::new("),
        "the production wrapper must construct the canonical PageObjectHelper"
    );
    assert!(
        production.contains(".get_form_xobject_for_page(true)?"),
        "the production wrapper must delegate to the canonical \
         PageObjectHelper::get_form_xobject_for_page instead of resolving handles itself"
    );
}
