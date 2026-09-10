//! Route contract for the FormField field-tree A6/A7/A8 cutover.

fn production_source(source: &str) -> &str {
    source
        .split_once("\n#[cfg(test)]\nmod tests")
        .map_or(source, |(production, _)| production)
}

#[test]
fn form_field_production_uses_canonical_resolving_accessors() {
    let source = include_str!("../src/form_field_object_helper.rs").replace("\r\n", "\n");
    let production = production_source(&source);
    for forbidden in [
        ".resolve(",
        "fn resolved(",
        ".get_key(",
        ".as_dictionary(",
        ".as_array(",
        ".as_name(",
        ".is_null(",
        ".as_integer(",
    ] {
        assert!(
            !production.contains(forbidden),
            "FormField production retains legacy route {forbidden}"
        );
    }
    assert!(
        production.contains("try_dereference"),
        "FormField production must use the canonical resolver"
    );
}
