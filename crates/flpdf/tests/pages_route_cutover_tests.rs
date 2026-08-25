use std::fs;
use std::path::Path;

#[test]
fn page_walk_production_uses_live_handle_traversal() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/pages.rs"))
        .unwrap()
        .replace("\r\n", "\n");
    let production = source
        .split("\n#[cfg(test)]\nmod tests")
        .next()
        .expect("pages.rs must keep its test boundary");

    for marker in [
        "resolve_borrowed(",
        "resolve_object(",
        "materialize(",
        "Object::",
    ] {
        assert!(
            !production.contains(marker),
            "PageWalk production must not keep raw route marker {marker:?}"
        );
    }
    for marker in [
        "get_object_handle(",
        "resolve(",
        "try_get_key(",
        "as_dictionary()",
        "as_array()",
    ] {
        assert!(
            production.contains(marker),
            "PageWalk production must use live handle accessor {marker:?}"
        );
    }
}
