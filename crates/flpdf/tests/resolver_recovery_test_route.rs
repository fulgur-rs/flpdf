//! Contract test for the bounded resolver recovery test handle cutover.

fn selected_tests(source: &str) -> String {
    [
        "public_resolve_retries_a_recovered_header_mismatch",
        "public_resolve_preserves_absent_recovery_as_null",
        "public_resolve_returns_null_for_unindexed_objstm_member",
        "public_resolve_recovers_a_malformed_stream_after_xref_reconstruction",
    ]
    .into_iter()
    .map(|name| {
        let start = source
            .find(&format!("fn {name}"))
            .expect("selected resolver recovery test must exist");
        let rest = &source[start..];
        let end = rest.find("\n    #[test]").unwrap_or(rest.len());
        rest[..end].to_owned()
    })
    .collect()
}

#[test]
fn selected_resolver_recovery_tests_use_canonical_handles() {
    let source = include_str!("../src/reader/resolver.rs");
    let tests = selected_tests(source);

    for forbidden in [
        "resolve_borrowed(",
        "resolve_object(",
        "Object::",
        ".as_dict()",
        ".into_dict()",
    ] {
        assert!(
            !tests.contains(forbidden),
            "selected resolver recovery tests still use legacy route marker {forbidden:?}"
        );
    }
    for required in ["ObjectHandle", "get_object_handle(", "pdf.resolve("] {
        assert!(
            tests.contains(required),
            "selected resolver recovery tests must retain canonical marker {required:?}"
        );
    }
}
