#[test]
fn page_merge_copies_selected_pages_through_the_canonical_foreign_route() {
    let source = include_str!("../src/job/page_merge.rs");
    let canonical = source
        .find("target.copy_foreign_object(&source_page)")
        .expect("page merge must copy each selected page through copyForeignObject");

    assert!(canonical < source.len());
}

#[test]
fn page_merge_metadata_uses_the_persistent_handle_copy_route() {
    let production = include_str!("../src/job/page_merge.rs")
        .split_once("#[cfg(test)]")
        .expect("page_merge test module marker")
        .0;

    for legacy in [
        "copy_objects_with_seed",
        "extend_object_closure",
        "extend_page_object_closure",
        "page_object_closure",
        "remap_refs_in_object",
    ] {
        assert!(
            !production.contains(legacy),
            "page_merge production must not retain the raw closure bridge: {legacy}"
        );
    }
    assert!(
        production.contains("copy_foreign_value"),
        "page_merge metadata must use the ObjectHandle foreign-value copier"
    );
}
