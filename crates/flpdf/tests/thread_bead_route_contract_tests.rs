//! Route contracts for the thread-bead A6/A7 accessor cutover.

fn production_source(source: &str) -> &str {
    source
        .split_once("\n#[cfg(test)]")
        .map_or(source, |(production, _)| production)
}

#[test]
fn thread_bead_production_uses_canonical_resolving_accessors() {
    let production = production_source(include_str!("../src/thread_bead_p.rs"));
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
            "thread_bead_p production retains legacy route {forbidden}"
        );
    }
}
