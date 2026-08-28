//! Route contract for the direct name-tree fixture slice.

const SELECTED_TESTS: &[&str] = &[
    "insert_does_not_allocate_for_direct_names_dictionary",
    "inserting_into_direct_embedded_files_root_preserves_it",
    "helper_replace_keeps_direct_names_dictionary_direct",
];

fn selected_function(source: &str, name: &str) -> String {
    let start = source
        .find(&format!("fn {name}"))
        .unwrap_or_else(|| panic!("selected test must exist: {name}"));
    let rest = &source[start..];
    let end = [
        rest.find("\n#[test]"),
        rest.find("\nfn "),
        rest.find("\n///"),
    ]
    .into_iter()
    .flatten()
    .min()
    .unwrap_or(rest.len());
    rest[..end].to_owned()
}

#[test]
fn selected_embedded_files_tests_use_only_canonical_fixture_routes() {
    let source = include_str!("embedded_files_tests.rs");

    for name in SELECTED_TESTS {
        let function = selected_function(source, name);
        for forbidden in [
            "resolve_object(",
            "resolve_borrowed(",
            "resolve_to_terminal(",
            "resolve_chain(",
            "materialize(",
            ".set_object(",
        ] {
            assert!(
                !function.contains(forbidden),
                "selected test {name} retains legacy fixture route {forbidden:?}"
            );
        }
        assert!(
            function.contains("ObjectHandle"),
            "selected test {name} must use typed handles"
        );
    }

    let selected = SELECTED_TESTS
        .iter()
        .map(|name| selected_function(source, name))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        selected.contains("Pdf::resolve") || selected.contains("pdf.resolve("),
        "selected tests must resolve live canonical handles"
    );
    assert!(
        selected.contains("replace_key"),
        "selected tests must mutate Catalog values through handle APIs"
    );
}
