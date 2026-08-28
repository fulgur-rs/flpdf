//! Contract test for the file-level json_inspect legacy-route cutover.

const JSON_VIEW_FUNCTIONS: &[&str] = &[
    "qpdf_preparation_collects_refs_from_a_freed_old_xref_stream",
    "qpdf_preparation_keeps_old_xref_streams_freed_at_the_same_generation",
];

fn selected_function(source: &str, name: &str) -> String {
    let start = source
        .find(&format!("fn {name}"))
        .unwrap_or_else(|| panic!("selected json_inspect function must exist: {name}"));
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
fn json_inspect_has_no_unowned_legacy_resolution_calls() {
    let source = include_str!("../src/json_inspect.rs");
    let mut canonical_source = source.to_owned();
    for name in JSON_VIEW_FUNCTIONS {
        let function = selected_function(source, name);
        assert!(
            function.contains("get_object_handle("),
            "JSON-view function {name} must obtain its canonical handle"
        );
        assert!(
            function.contains("pdf.resolve("),
            "JSON-view function {name} must resolve through the canonical resolver"
        );
        canonical_source = canonical_source.replacen(&function, "", 1);
    }

    for forbidden in [
        "resolve_object(",
        "resolve_borrowed(",
        "resolve_to_terminal(",
        "resolve_chain(",
        "materialize(",
    ] {
        assert!(
            !canonical_source.contains(forbidden),
            "json_inspect.rs still contains an unowned legacy route marker {forbidden:?}"
        );
    }
}
