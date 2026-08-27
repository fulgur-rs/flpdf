//! Contract test for the bounded resolver stream-recovery source-seek cutover.

fn selected_test(source: &str) -> String {
    let name = "canonical_stream_recovery_does_not_rewind_the_whole_source";
    let start = source
        .find(&format!("fn {name}"))
        .expect("selected source-seek recovery test must exist");
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
fn selected_source_seek_recovery_test_uses_canonical_handle() {
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
            "selected source-seek recovery test still uses legacy route marker {forbidden:?}"
        );
    }
    for required in ["ObjectHandle", "get_object_handle(", "pdf.resolve("] {
        assert!(
            test.contains(required),
            "selected source-seek recovery test must retain canonical marker {required:?}"
        );
    }
}
