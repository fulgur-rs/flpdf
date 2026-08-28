//! Contract test for the bounded resolver EOF stream-framing cutover.

fn selected_tests(source: &str) -> String {
    [
        "a_stream_keyword_at_end_of_input_resolves_to_null_with_a_warning",
        "a_stream_ending_the_input_after_endstream_warns_and_resolves_to_null",
    ]
    .into_iter()
    .map(|name| {
        let start = source
            .find(&format!("fn {name}"))
            .expect("selected EOF stream-framing test must exist");
        let rest = &source[start..];
        let end = [
            rest.find("\n    #[test]"),
            rest.find("\n    fn "),
            rest.find("\n    ///"),
        ]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(rest.len());
        rest[..end].to_owned()
    })
    .collect()
}

#[test]
fn selected_eof_stream_framing_tests_use_canonical_handles() {
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
            "selected EOF stream-framing test still uses legacy route marker {forbidden:?}"
        );
    }
    for required in ["ObjectHandle", "get_object_handle(", "pdf.resolve("] {
        assert!(
            tests.contains(required),
            "selected EOF stream-framing tests must retain canonical marker {required:?}"
        );
    }
}
