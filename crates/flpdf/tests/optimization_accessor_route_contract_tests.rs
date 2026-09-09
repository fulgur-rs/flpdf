//! Route contracts for the optimization A6/A7 accessor cutover.

fn production_source(source: &str) -> &str {
    source
        .split_once("\n#[cfg(test)]")
        .map_or(source, |(production, _)| production)
}

#[test]
fn optimization_consumers_use_canonical_resolving_accessors() {
    for (path, source) in [
        (
            "src/optimization.rs",
            include_str!("../src/optimization.rs"),
        ),
        (
            "src/optimization/inherited_attrs.rs",
            include_str!("../src/optimization/inherited_attrs.rs"),
        ),
    ] {
        let production = production_source(source);
        for forbidden in [
            ".resolve(",
            ".resolve_handle(",
            ".resolve_handle_ref(",
            ".get_key(",
            ".has_key(",
        ] {
            assert!(
                !production.contains(forbidden),
                "{path} retains the legacy optimization route {forbidden}"
            );
        }
    }
}
