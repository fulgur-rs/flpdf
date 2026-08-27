//! Contract test for the bounded resolver stream-recovery scan cutover.

fn selected_tests(source: &str) -> String {
    [
        "canonical_stream_recovery_respects_qpdf_candidate_token_limit",
        "canonical_stream_recovery_does_not_seek_for_each_non_prefix_candidate",
    ]
    .into_iter()
    .map(|name| {
        let start = source
            .find(&format!("fn {name}"))
            .expect("selected stream-recovery scan test must exist");
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
fn selected_stream_recovery_scan_tests_use_canonical_handles() {
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
            "selected stream-recovery scan test still uses legacy route marker {forbidden:?}"
        );
    }
    for required in ["ObjectHandle", "get_object_handle(", "pdf.resolve("] {
        assert!(
            tests.contains(required),
            "selected stream-recovery scan tests must retain canonical marker {required:?}"
        );
    }
}
