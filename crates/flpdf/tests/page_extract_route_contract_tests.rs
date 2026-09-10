//! Route contract for the page-extraction A6/A7 accessor cutover.

fn production_source(source: &str) -> &str {
    source
        .split_once("\n#[cfg(test)]")
        .map_or(source, |(production, _)| production)
}

#[test]
fn page_extract_production_uses_canonical_page_mutations() {
    let production = production_source(include_str!("../src/page_extract.rs"));
    for forbidden in [".resolve(", ".resolve_handle(", ".resolve_handle_ref("] {
        assert!(
            !production.contains(forbidden),
            "page_extract production retains legacy route {forbidden}"
        );
    }
    assert!(
        production.contains("page.try_dereference()?;"),
        "duplicate page copying must resolve through the canonical handle"
    );
}
