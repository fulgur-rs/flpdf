//! Contract test for the bounded resolver tombstone/recovery test cutover.

fn selected_tests(source: &str) -> String {
    [
        "reconstruction_reregisters_privately_removed_unindexed_object_like_qpdf",
        "reconstruction_discards_loaded_free_object_tombstones",
    ]
    .into_iter()
    .map(|name| {
        let start = source
            .find(&format!("fn {name}"))
            .expect("selected resolver tombstone test must exist");
        let rest = &source[start..];
        let end = [rest.find("\n    #[test]"), rest.find("\n    fn ")]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(rest.len());
        rest[..end].to_owned()
    })
    .collect()
}

#[test]
fn selected_resolver_tombstone_tests_use_canonical_handles() {
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
            "selected resolver tombstone tests still use legacy route marker {forbidden:?}"
        );
    }
    for required in ["ObjectHandle", "get_object_handle(", "pdf.resolve("] {
        assert!(
            tests.contains(required),
            "selected resolver tombstone tests must retain canonical marker {required:?}"
        );
    }
}
