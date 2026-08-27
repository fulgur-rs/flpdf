//! Contract test for the bounded resolver framing-boundary cutover.

fn selected_test(source: &str) -> String {
    let name = "a_framing_keyword_split_across_an_input_chunk_is_not_read_as_a_shorter_word";
    let start = source
        .find(&format!("fn {name}"))
        .expect("selected framing-boundary test must exist");
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
}

#[test]
fn selected_framing_boundary_test_uses_canonical_handle() {
    let source = include_str!("../src/reader/resolver.rs");
    let test = selected_test(source);

    for forbidden in [
        "try_dereference(",
        "resolve_borrowed(",
        "resolve_object(",
        "Object::",
        ".as_dict()",
        ".into_dict()",
    ] {
        assert!(
            !test.contains(forbidden),
            "selected framing-boundary test still uses legacy route marker {forbidden:?}"
        );
    }
    for required in ["ObjectHandle", "get_object_handle(", "pdf.resolve("] {
        assert!(
            test.contains(required),
            "selected framing-boundary test must retain canonical marker {required:?}"
        );
    }
}
