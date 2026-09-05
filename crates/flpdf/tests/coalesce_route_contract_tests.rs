use std::path::Path;

#[test]
fn page_module_has_no_eager_legacy_coalesce_route() {
    let source = include_str!("../src/pages.rs");
    assert!(!source.contains("coalesce_page_contents"));
}

#[test]
fn production_consumers_call_the_canonical_coalesce_owner() {
    let cli = include_str!("../../flpdf-cli/src/main.rs");
    let flatten = include_str!("../src/page_annotation_flatten.rs");

    assert!(!cli.contains("coalesce_page_contents"));
    assert!(!flatten.contains("coalesce_page_contents"));
    assert!(cli.contains("PageObjectHelper::new(page_ref, &mut pdf).coalesce_content_streams()?"));
    assert!(flatten.contains("PageObjectHelper::new(page_ref, pdf).coalesce_content_streams()?"));
}

#[test]
fn cli_transformation_order_matches_qpdf_job() {
    let source = include_str!("../../flpdf-cli/src/main.rs");
    let generate = source
        .find("if generate_appearances {")
        .expect("appearance generation route");
    // The linearize rewrite path flattens before handing the document to the
    // linearization writer and has no appearance-generation step.  Check the
    // ordinary rewrite pipeline here, where qpdf's combined transformation
    // order is represented by the later flatten call.
    let flatten = source
        .rfind(".flatten_annotations(required_flags, forbidden_flags)?")
        .expect("annotation flatten route");
    let coalesce = source
        .find("PageObjectHelper::new(page_ref, &mut pdf).coalesce_content_streams()?")
        .expect("coalesce route");
    // The linearized rewrite path has its own earlier flatten-rotation call;
    // select the ordinary rewrite pipeline below, whose order this contract
    // is checking.
    let rotation = source
        .rfind("flatten_rotation_on_pages(&mut pdf, &page_refs)?")
        .expect("rotation route");
    let normalize = source
        .rfind("normalize_page_contents(&mut pdf)?")
        .expect("plain rewrite normalization route");

    assert!(generate < flatten);
    assert!(flatten < coalesce);
    assert!(coalesce < rotation);
    assert!(rotation < normalize);
}

#[test]
fn qpdf_correspondence_records_the_provider_owner_and_order() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/qpdf-correspondence.md");
    let source = std::fs::read_to_string(path).expect("qpdf correspondence");
    assert!(source.contains("provider-backed stream"));
    assert!(source.contains("legacy stream write-back は削除済み"));
}
