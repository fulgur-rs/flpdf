//! Route contract for the AcroForm appearance renderer A6/A7 cutover.

fn production_source(source: &str) -> &str {
    source
        .split_once("\n#[cfg(test)]\nmod tests")
        .map_or(source, |(production, _)| production)
}

#[test]
fn appearance_renderer_production_uses_canonical_handle_access() {
    let source = include_str!("../src/form_field_object_helper/rendering.rs").replace("\r\n", "\n");
    let production = production_source(&source);
    for forbidden in [
        ".resolve(",
        "resolve_canonical(",
        ".get_key(",
        ".as_dictionary(",
        ".as_array(",
        ".as_name(",
        ".is_null(",
    ] {
        assert!(
            !production.contains(forbidden),
            "appearance renderer production retains legacy route {forbidden}"
        );
    }
    assert!(
        production.contains(".try_dereference()?"),
        "appearance renderer must use the canonical resolver"
    );
}
