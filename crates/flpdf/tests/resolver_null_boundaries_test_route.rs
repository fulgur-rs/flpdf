//! Contract test for the bounded resolver null/warning test cutover.

fn selected_tests(source: &str) -> String {
    [
        "mismatch_without_recovery_warns_and_resolves_to_null",
        "absent_after_rebuild_warns_and_resolves_to_null",
        "absent_from_xref_table_resolves_to_null_without_reconstruction",
        "zero_offset_xref_entry_warns_and_resolves_to_null_without_reconstruction",
    ]
    .into_iter()
    .map(|name| {
        let start = source
            .find(&format!("fn {name}"))
            .expect("selected resolver null-boundary test must exist");
        let rest = &source[start..];
        let end = rest.find("\n    #[test]").unwrap_or(rest.len());
        rest[..end].to_owned()
    })
    .collect()
}

#[test]
fn selected_resolver_null_boundary_tests_use_canonical_handles() {
    let source = include_str!("../src/reader/resolver.rs");
    let tests = selected_tests(source);

    for forbidden in [
        "try_dereference(",
        "resolve_borrowed(",
        "resolve_object(",
        "Object::",
        ".as_dict()",
        ".into_dict()",
    ] {
        assert!(
            !tests.contains(forbidden),
            "selected resolver null-boundary tests still use legacy route marker {forbidden:?}"
        );
    }
    for required in ["ObjectHandle", "get_object_handle(", "pdf.resolve("] {
        assert!(
            tests.contains(required),
            "selected resolver null-boundary tests must retain canonical marker {required:?}"
        );
    }
}
