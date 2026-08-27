//! Contract test for the bounded resolver re-entrant stream-length cutover.

fn selected_tests(source: &str) -> String {
    [
        "a_self_referential_length_takes_the_loop_branch_instead_of_recursing_forever",
        "nested_stream_length_resolutions_each_restore_their_own_offset",
    ]
    .into_iter()
    .map(|name| {
        let start = source
            .find(&format!("fn {name}"))
            .expect("selected resolver re-entrant test must exist");
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
fn selected_resolver_reentrant_length_tests_use_canonical_handles() {
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
            "selected resolver re-entrant tests still use legacy route marker {forbidden:?}"
        );
    }
    for required in ["ObjectHandle", "get_object_handle(", "pdf.resolve("] {
        assert!(
            tests.contains(required),
            "selected resolver re-entrant tests must retain canonical marker {required:?}"
        );
    }
}
