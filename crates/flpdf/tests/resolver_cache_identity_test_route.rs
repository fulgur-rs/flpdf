//! Contract test for the bounded resolver cache/identity test cutover.

fn selected_tests(source: &str) -> String {
    [
        "an_uncompressed_object_resolves_and_a_second_dereference_does_not_re_read",
        "a_nested_reference_resolves_to_the_documents_canonical_handle",
        "the_canonical_resolver_records_qpdf_offsets_for_plain_and_stream_objects",
    ]
    .into_iter()
    .map(|name| {
        let start = source
            .find(&format!("fn {name}"))
            .expect("selected resolver cache/identity test must exist");
        let rest = &source[start..];
        let end = [rest.find("\n    #[test]"), rest.find("\n    fn ")]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(rest.len());
        rest[..end].to_owned()
    })
    .collect()
}

#[test]
fn selected_resolver_cache_identity_tests_use_canonical_handles() {
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
            "selected resolver cache/identity tests still use legacy route marker {forbidden:?}"
        );
    }
    for required in ["ObjectHandle", "get_object_handle(", "pdf.resolve("] {
        assert!(
            tests.contains(required),
            "selected resolver cache/identity tests must retain canonical marker {required:?}"
        );
    }
}
