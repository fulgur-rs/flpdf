//! Route contract for the PageDocumentHelper page-mutation test cutover.

const TARGETS: [&str; 13] = [
    "make_catalog_pages_root_direct",
    "assert_direct_catalog_pages_root",
    "remove_page_allows_an_empty_document",
    "remove_page_allows_an_empty_direct_catalog_pages_root",
    "remove_page_removes_the_selected_page",
    "remove_page_preserves_direct_catalog_pages_root",
    "remove_page_rejects_a_non_member",
    "add_page_first_prepends_page",
    "add_page_last_appends_page",
    "add_page_preserves_direct_catalog_pages_root",
    "add_page_materializes_attributes_from_a_direct_parent",
    "add_page_at_after_reference_inserts_after_that_page",
    "add_page_at_rejects_reference_outside_document",
];

fn source_block<'a>(source: &'a str, name: &str) -> &'a str {
    let marker = format!("fn {name}(");
    let start = source
        .find(&marker)
        .unwrap_or_else(|| panic!("missing PageDocumentHelper route {name}"));
    let rest = &source[start..];
    let end = [rest.find("\n#[test]\n"), rest.find("\nfn ")]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(rest.len());
    &rest[..end]
}

#[test]
fn page_mutation_tests_use_only_canonical_handle_routes() {
    let source = include_str!("page_document_helper_tests.rs").replace("\r\n", "\n");
    let blocks = TARGETS
        .iter()
        .map(|name| source_block(&source, name))
        .collect::<Vec<_>>();
    let selected = blocks.join("\n");

    for forbidden in [
        "resolve_object(",
        "resolve_borrowed(",
        "resolve_to_terminal(",
        "resolve_chain(",
        "materialize(",
        "set_object(",
        "Object::",
        "Dictionary::",
        "get_ref(",
        ".into_dict(",
        ".as_dict(",
        ".as_ref_id(",
        "mark_object_dirty(",
    ] {
        assert!(
            !selected.contains(forbidden),
            "page-mutation route retains legacy marker: {forbidden}"
        );
    }

    for required in [
        "ObjectHandle",
        "get_object_handle(",
        "resolve(&",
        "get_key(",
        "object_ref(",
        "replace_key(",
        "remove_key(",
        "mark_object_handle_dirty(",
    ] {
        assert!(
            selected.contains(required),
            "page-mutation routes must use canonical marker: {required}"
        );
    }
}
