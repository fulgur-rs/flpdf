//! Route contract for the PageDocumentHelper get-all-pages test cutover.

const TARGETS: [&str; 13] = [
    "get_all_pages_repairs_catalog_pages_pointer",
    "get_all_pages_follows_parent_from_direct_catalog_page_value",
    "get_all_pages_traverses_a_direct_catalog_pages_root",
    "get_all_pages_marks_qpdf_json_observation",
    "get_all_pages_returns_empty_when_catalog_has_no_pages",
    "get_all_pages_returns_empty_when_catalog_pages_is_not_a_dictionary",
    "get_all_pages_rejects_a_pages_tree_cycle",
    "get_all_pages_rejects_a_revisited_pages_subtree_like_qpdf",
    "get_all_pages_traverses_a_direct_intermediate_pages_node",
    "get_all_pages_resolves_an_indirect_kids_holder_under_a_direct_pages_node",
    "get_all_pages_rejects_an_overdeep_direct_pages_tree",
    "get_all_pages_ignores_a_direct_pages_node_with_non_array_kids",
    "get_all_pages_errors_on_a_non_dictionary_root",
];

fn test_block<'a>(source: &'a str, name: &str) -> &'a str {
    let marker = format!("fn {name}(");
    let start = source
        .find(&marker)
        .unwrap_or_else(|| panic!("missing PageDocumentHelper test {name}"));
    let rest = &source[start..];
    let end = [rest.find("\n#[test]\n"), rest.find("\nfn ")]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(rest.len());
    &rest[..end]
}

#[test]
fn get_all_pages_tests_use_only_canonical_handle_routes() {
    let source = include_str!("page_document_helper_tests.rs").replace("\r\n", "\n");
    let blocks = TARGETS
        .iter()
        .map(|name| test_block(&source, name))
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
        ".into_dict(",
        ".as_dict(",
        ".as_ref_id(",
        "mark_object_dirty(",
    ] {
        assert!(
            !selected.contains(forbidden),
            "get_all_pages test retains legacy route marker: {forbidden}"
        );
    }

    for required in [
        "ObjectHandle",
        "get_object_handle(",
        "resolve(&",
        "get_key(",
        "object_ref(",
        "replace_key(",
        "mark_object_handle_dirty(",
    ] {
        assert!(
            selected.contains(required),
            "get_all_pages tests must use canonical route marker: {required}"
        );
    }
}
