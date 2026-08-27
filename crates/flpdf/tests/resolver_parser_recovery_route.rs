//! Contract test for the bounded resolver parser/recovery cutover.

fn selected_tests(source: &str) -> String {
    [
        "a_direct_value_ending_the_input_warns_and_resolves_to_null",
        "a_value_ending_on_a_buffer_boundary_still_finds_its_endobj",
        "a_recovered_malformed_body_reports_its_warning_at_the_file_offset",
        "a_caught_parse_failure_preserves_its_warning_offset",
    ]
    .into_iter()
    .map(|name| {
        let start = source
            .find(&format!("fn {name}"))
            .expect("selected resolver parser/recovery test must exist");
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
fn selected_parser_recovery_tests_use_canonical_handles() {
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
            "selected parser/recovery test still uses legacy route marker {forbidden:?}"
        );
    }
    for required in ["ObjectHandle", "get_object_handle(", "pdf.resolve("] {
        assert!(
            tests.contains(required),
            "selected parser/recovery tests must retain canonical marker {required:?}"
        );
    }
}
