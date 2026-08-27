//! Contract test for the bounded resolver rewrite/provenance test cutover.

fn selected_tests(source: &str) -> String {
    [
        "reconstruction_keeps_canonical_resolution_on_the_rebuilt_xref",
        "full_rewrite_synchronizes_recovered_compressed_parent_provenance",
    ]
    .into_iter()
    .map(|name| {
        let start = source
            .find(&format!("fn {name}"))
            .expect("selected resolver rewrite test must exist");
        let rest = &source[start..];
        let end = rest.find("\n    #[test]").unwrap_or(rest.len());
        rest[..end].to_owned()
    })
    .collect()
}

#[test]
fn selected_resolver_rewrite_tests_use_canonical_handles() {
    let source = include_str!("../src/reader/resolver.rs");
    let tests = selected_tests(source);

    for forbidden in [
        "resolve_borrowed(",
        "resolve_object(",
        "Object::",
        ".as_dict()",
        ".into_dict()",
        "set_object(",
    ] {
        assert!(
            !tests.contains(forbidden),
            "selected resolver rewrite tests still use legacy route marker {forbidden:?}"
        );
    }
    for required in [
        "ObjectHandle",
        "get_object_handle(",
        "pdf.resolve(",
        "set_object_handle(",
    ] {
        assert!(
            tests.contains(required),
            "selected resolver rewrite tests must retain canonical marker {required:?}"
        );
    }
}
