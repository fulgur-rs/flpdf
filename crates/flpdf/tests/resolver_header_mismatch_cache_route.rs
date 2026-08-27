//! Contract test for the bounded resolver header-mismatch cache cutover.

fn selected_test(source: &str) -> String {
    let name = "a_header_mismatch_caches_the_object_under_its_actual_objgen";
    let start = source
        .find(&format!("fn {name}"))
        .expect("selected header-mismatch test must exist");
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
fn selected_header_mismatch_test_uses_canonical_handle() {
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
            "selected header-mismatch test still uses legacy route marker {forbidden:?}"
        );
    }
    for required in ["ObjectHandle", "get_object_handle(", "pdf.resolve("] {
        assert!(
            test.contains(required),
            "selected header-mismatch test must retain canonical marker {required:?}"
        );
    }
}
