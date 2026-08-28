//! Route contract for the PageDocumentHelper inherited-attributes test cutover.

const TARGETS: [&str; 4] = [
    "push_inherited_attributes_materializes_rotate_on_leaf",
    "push_inherited_attributes_traverses_a_direct_catalog_pages_root",
    "push_inherited_attributes_traverses_direct_pages_descendants",
    "push_inherited_attributes_ignores_non_dictionary_direct_kids",
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
fn inherited_attributes_tests_use_only_canonical_handle_routes() {
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
            "inherited-attributes test retains legacy route marker: {forbidden}"
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
            "inherited-attributes tests must use canonical route marker: {required}"
        );
    }
}
