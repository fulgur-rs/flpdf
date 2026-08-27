//! Contract test for the bounded resolver enumeration/null test cutover.

fn selected_tests(source: &str) -> String {
    [
        "handle_recovery_updates_public_object_enumeration",
        "reconstruction_returns_null_for_unindexed_objstm_member",
    ]
    .into_iter()
    .map(|name| {
        let start = source
            .find(&format!("fn {name}"))
            .expect("selected resolver enumeration test must exist");
        let rest = &source[start..];
        let end = rest.find("\n    #[test]").unwrap_or(rest.len());
        rest[..end].to_owned()
    })
    .collect()
}

#[test]
fn selected_resolver_enumeration_tests_use_canonical_handles() {
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
            "selected resolver enumeration tests still use legacy route marker {forbidden:?}"
        );
    }
    for required in ["ObjectHandle", "get_object_handle(", "pdf.resolve("] {
        assert!(
            tests.contains(required),
            "selected resolver enumeration tests must retain canonical marker {required:?}"
        );
    }
}
