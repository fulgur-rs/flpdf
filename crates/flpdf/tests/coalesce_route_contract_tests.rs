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
    let lifecycle = include_str!("../src/job/lifecycle.rs");

    assert!(!cli.contains("coalesce_page_contents"));
    assert!(!flatten.contains("coalesce_page_contents"));
    assert!(cli.contains("job.apply_transformations(&mut pdf)?"));
    assert!(cli.contains("job.write_qpdf(&mut pdf)"));
    assert!(lifecycle.contains("PageObjectHelper::new(page_ref, pdf).coalesce_content_streams()?"));
    assert!(flatten.contains("PageObjectHelper::new(page_ref, pdf).coalesce_content_streams()?"));
}

#[test]
fn cli_transformation_order_matches_qpdf_job() {
    let source = include_str!("../src/job/lifecycle.rs");
    let transformations = source
        .split_once("fn prepare_document_transformations")
        .map(|(_, body)| body)
        .expect("canonical job transformation route");
    let generate = transformations
        .find("if configuration.generate_appearances {")
        .expect("appearance generation route");
    let flatten = transformations
        .find("if let Some(mode) = configuration.flatten_annotations {")
        .expect("annotation flatten route");
    let coalesce = transformations
        .find("PageObjectHelper::new(page_ref, pdf).coalesce_content_streams()?")
        .expect("coalesce route");
    let rotation = transformations
        .find("flatten_rotation_on_pages(pdf, &page_refs)?")
        .expect("rotation route");
    let labels = transformations
        .find("self.apply_page_label_transformations(pdf, configuration)?")
        .expect("page-label route");

    assert!(generate < flatten);
    assert!(flatten < coalesce);
    assert!(coalesce < rotation);
    assert!(rotation < labels);
}

#[test]
fn qpdf_correspondence_records_the_provider_owner_and_order() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/qpdf-correspondence.md");
    let source = std::fs::read_to_string(path).expect("qpdf correspondence");
    assert!(source.contains("provider-backed stream"));
    assert!(source.contains("legacy stream write-back は削除済み"));
}
