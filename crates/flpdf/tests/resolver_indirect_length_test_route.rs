//! Contract test for the bounded resolver indirect-length stream cutover.

fn selected_tests(source: &str) -> String {
    [
        "a_streams_indirect_length_resolves_mid_parse_and_raw_read_uses_the_restored_position",
        "raw_stream_data_reports_a_short_original_source_as_unsupported",
    ]
    .into_iter()
    .map(|name| {
        let start = source
            .find(&format!("fn {name}"))
            .expect("selected resolver indirect-length test must exist");
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
fn selected_resolver_indirect_length_tests_use_canonical_handles() {
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
            "selected resolver indirect-length tests still use legacy route marker {forbidden:?}"
        );
    }
    for required in ["ObjectHandle", "get_object_handle(", "pdf.resolve("] {
        assert!(
            tests.contains(required),
            "selected resolver indirect-length tests must retain canonical marker {required:?}"
        );
    }
}
