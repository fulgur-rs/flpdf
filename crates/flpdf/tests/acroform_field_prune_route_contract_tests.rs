//! Route contracts for the AcroForm field-prune A6/A7 cutover.

fn production_source(source: &str) -> &str {
    source
        .split_once("\n#[cfg(test)]")
        .map_or(source, |(production, _)| production)
}

#[test]
fn acroform_field_prune_uses_canonical_resolving_accessors() {
    let production = production_source(include_str!("../src/job/acroform_field_prune.rs"));
    for forbidden in [
        ".resolve(",
        ".resolve_handle(",
        ".resolve_handle_ref(",
        ".as_dictionary()",
        ".as_array()",
        ".as_name()",
        ".is_null()",
        ".get_key(",
        ".has_key(",
    ] {
        assert!(
            !production.contains(forbidden),
            "acroform_field_prune production retains legacy route {forbidden}"
        );
    }
}
