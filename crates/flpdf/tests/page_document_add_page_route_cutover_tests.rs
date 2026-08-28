//! Route contract for the PageDocumentHelper add-page test cutover.

const TARGETS: [&str; 7] = [
    "add_page_indirects_a_direct_page_input",
    "add_page_duplicate_does_not_overwrite_a_handle_only_object",
    "add_page_copies_a_foreign_page_after_materializing_source_inheritance",
    "add_page_uses_qpdf_copy_foreign_object_null_key_filtering",
    "add_page_recopies_a_page_left_as_a_nested_boundary_placeholder",
    "add_page_reuses_foreign_resources_from_the_same_source",
    "add_page_does_not_copy_a_second_page_referenced_by_a_foreign_page",
];

fn test_block<'a>(source: &'a str, name: &str) -> &'a str {
    let marker = format!("fn {name}(");
    let start = source
        .find(&marker)
        .unwrap_or_else(|| panic!("missing PageDocumentHelper test {name}"));
    let rest = &source[start..];
    let end = rest.find("\n#[test]\n").unwrap_or(rest.len());
    &rest[..end]
}

#[test]
fn add_page_tests_use_only_canonical_handle_routes() {
    let source = include_str!("page_document_helper_tests.rs");
    let blocks = TARGETS
        .iter()
        .map(|name| test_block(source, name))
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
    ] {
        assert!(
            !selected.contains(forbidden),
            "add_page test retains legacy route marker: {forbidden}"
        );
    }

    for required in [
        "ObjectHandle",
        "get_object_handle(",
        "resolve(&",
        "replace_key(",
        "mark_object_handle_dirty(",
        "object_ref(",
        "is_null(",
    ] {
        assert!(
            selected.contains(required),
            "add_page tests must use canonical route marker: {required}"
        );
    }
}
