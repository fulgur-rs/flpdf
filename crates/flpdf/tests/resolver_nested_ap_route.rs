//! Contract test for the bounded resolver nested-AP cutover.

fn selected_test(source: &str) -> String {
    let name = "a_nested_ap_n_reference_resolves_through_the_owning_document";
    let start = source
        .find(&format!("fn {name}"))
        .expect("selected nested-AP test must exist");
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
fn selected_nested_ap_test_uses_canonical_handles() {
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
            "selected nested-AP test still uses legacy route marker {forbidden:?}"
        );
    }
    for required in ["ObjectHandle", "get_object_handle(", "pdf.resolve("] {
        assert!(
            normalized.contains(required),
            "selected nested-AP test must retain canonical marker {required:?}"
        );
    }
}
