use std::fs;
use std::path::PathBuf;

fn source_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn function_body<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let start_at = source.find(start).expect("target function must exist");
    let body = &source[start_at..];
    let end_at = body.find(end).expect("next function boundary must exist");
    &body[..end_at]
}

fn assert_canonical_route(body: &str, function_name: &str, receiver: &str) {
    for forbidden in [
        "pdf.resolve(",
        ".resolve_handle(",
        ".resolve_handle_ref(",
        ".get_key(",
        ".has_key(",
    ] {
        assert!(
            !body.contains(forbidden),
            "{function_name} retains non-canonical route {forbidden}"
        );
    }
    assert!(
        body.contains(&format!("{receiver}.try_dereference()?")),
        "{function_name} must dereference the live handle at its receiver boundary"
    );
}

#[test]
fn struct_tree_and_objr_drop_routes_use_canonical_handles() {
    let struct_tree = fs::read_to_string(source_root().join("struct_tree_pg.rs"))
        .expect("struct_tree_pg.rs must be readable")
        .replace("\r\n", "\n");
    let struct_body = function_body(
        &struct_tree,
        "pub fn drop_struct_elem_dangling_pg_with_max_depth",
        "/// Walk a `/K` value",
    );
    assert_canonical_route(
        struct_body,
        "drop_struct_elem_dangling_pg_with_max_depth",
        "catalog",
    );
    assert!(struct_body.contains("catalog.try_as_dictionary()?"));

    let objr = fs::read_to_string(source_root().join("objr_obj_annot_p.rs"))
        .expect("objr_obj_annot_p.rs must be readable")
        .replace("\r\n", "\n");
    let objr_body = function_body(
        &objr,
        "pub fn drop_objr_obj_annot_dangling_p",
        "/// Remap-or-drop the `/P`",
    );
    assert_canonical_route(objr_body, "drop_objr_obj_annot_dangling_p", "annot");
    assert!(objr_body.contains("annot.try_as_dictionary()?"));
}
