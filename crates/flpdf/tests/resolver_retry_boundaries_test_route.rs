//! Contract test for the bounded resolver reconstruction-retry cutover.

fn selected_tests(source: &str) -> String {
    [
        "second_reconstruction_attempt_rethrows_error_to_prevent_infinite_loop",
        "reconstruct_xref_and_retry_with_non_parse_trigger_error",
        "reconstruct_xref_and_retry_when_read_object_at_offset_fails",
    ]
    .into_iter()
    .map(|name| {
        let start = source
            .find(&format!("fn {name}"))
            .expect("selected resolver retry test must exist");
        let rest = &source[start..];
        let end = rest.find("\n    #[test]").unwrap_or(rest.len());
        rest[..end].to_owned()
    })
    .collect()
}

#[test]
fn selected_resolver_retry_tests_use_canonical_handles() {
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
            "selected resolver retry tests still use legacy route marker {forbidden:?}"
        );
    }
    for required in ["ObjectHandle", "get_object_handle(", "pdf.resolve("] {
        assert!(
            tests.contains(required),
            "selected resolver retry tests must retain canonical marker {required:?}"
        );
    }
}
