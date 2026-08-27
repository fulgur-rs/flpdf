//! Contract test for the bounded resolver shared-helper cutover.

fn selected_helpers(source: &str) -> String {
    ["with_second_object", "with_second_object_strict"]
        .into_iter()
        .map(|name| {
            let start = source
                .find(&format!("fn {name}"))
                .expect("selected resolver helper must exist");
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
fn resolver_shared_helpers_use_canonical_resolution() {
    let source = include_str!("../src/reader/resolver.rs");
    let helpers = selected_helpers(source);

    for forbidden in [
        "try_dereference(",
        "resolve_borrowed(",
        "resolve_object(",
        "Object::",
        ".as_dict()",
        ".into_dict()",
    ] {
        assert!(
            !helpers.contains(forbidden),
            "resolver shared helper still uses legacy route marker {forbidden:?}"
        );
    }
    for required in ["ObjectHandle", "pdf.resolve("] {
        assert!(
            helpers.contains(required),
            "resolver shared helper must retain canonical marker {required:?}"
        );
    }
}
