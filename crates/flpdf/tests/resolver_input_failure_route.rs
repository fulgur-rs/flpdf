//! Contract test for the bounded resolver input-failure cutover.

fn selected_test(source: &str) -> String {
    let name = "an_input_source_that_fails_mid_resolution_propagates_the_error";
    let start = source
        .find(&format!("fn {name}"))
        .expect("selected input-failure test must exist");
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
fn selected_input_failure_test_uses_canonical_handle() {
    let source = include_str!("../src/reader/resolver.rs");
    let test = selected_test(source);
    let normalized = test.split_whitespace().collect::<String>();

    for forbidden in [
        "try_dereference(",
        "resolve_borrowed(",
        "resolve_object(",
        "Object::",
        ".as_dict()",
        ".into_dict()",
    ] {
        assert!(
            !normalized.contains(forbidden),
            "selected input-failure test still uses legacy route marker {forbidden:?}"
        );
    }
    for required in ["ObjectHandle", "get_object_handle(", "pdf.resolve("] {
        assert!(
            normalized.contains(required),
            "selected input-failure test must retain canonical marker {required:?}"
        );
    }
}
