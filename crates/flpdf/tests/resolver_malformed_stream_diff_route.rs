//! Contract test for the bounded resolver malformed-stream differential cutover.

fn selected_test(source: &str) -> String {
    let name = "canonical_stream_malformed_framing_matches_pinned_qpdf";
    let start = source
        .find(&format!("fn {name}"))
        .expect("selected malformed-stream differential test must exist");
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
fn selected_malformed_stream_differential_test_uses_canonical_handle() {
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
            "selected malformed-stream differential test still uses legacy route marker {forbidden:?}"
        );
    }
    for required in ["ObjectHandle", "get_object_handle(", "pdf.resolve("] {
        assert!(
            test.contains(required),
            "selected malformed-stream differential test must retain canonical marker {required:?}"
        );
    }
}
