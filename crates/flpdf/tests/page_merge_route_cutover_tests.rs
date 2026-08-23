#[test]
fn page_merge_copies_selected_pages_through_the_canonical_foreign_route() {
    let source = include_str!("../src/job/page_merge.rs");
    let canonical = source
        .find("target.copy_foreign_object(&source_page)")
        .expect("page merge must copy each selected page through copyForeignObject");
    let legacy_metadata_bridge = source
        .rfind("copy_objects_with_seed")
        .expect("the bounded legacy bridge must be isolated after page copying");

    assert!(
        canonical < legacy_metadata_bridge,
        "canonical selected-page copy must precede the legacy metadata bridge"
    );
}
