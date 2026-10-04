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
    let page_extraction = cli
        .split_once("fn run_page_extraction(")
        .expect("rewrite page extraction route should remain named")
        .1
        .split_once("\nfn run_rewrite_with_page_ops")
        .expect("plain rewrite page-operation route should follow extraction")
        .0;
    assert!(page_extraction.contains("run_page_operation_job(job, page_ops, remove_unref)"));
    assert!(!page_extraction.contains("apply_transformations_for_cli("));
    assert!(cli.contains("configuration.coalesce_contents();"));
    assert!(!cli.contains("match job.apply_transformations(pdf)"));
    assert!(!cli.contains("job.write_qpdf(&mut pdf)"));
    assert!(cli.contains("finish_job_exit_status(job.run()?)"));
    assert!(!cli.contains("fn run_rewrite_opened<"));
    let empty_rewrite = cli
        .split_once("fn run_rewrite(")
        .and_then(|(_, tail)| tail.split_once("fn run_page_operations_with_qpdf_job("))
        .map(|(body, _)| body)
        .expect("empty rewrite route remains on run_rewrite");
    let empty_rewrite = empty_rewrite.split_whitespace().collect::<String>();
    assert!(empty_rewrite.contains("run_rewrite_with_qpdf_job(None,"));
    assert!(lifecycle
        .contains("PageObjectHelper::from_object_handle(page, pdf).coalesce_content_streams()?"));
    assert!(flatten.contains(
        "PageObjectHelper::from_object_handle(page.clone(), pdf).coalesce_content_streams()?"
    ));
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
        .find("PageObjectHelper::from_object_handle(page, pdf).coalesce_content_streams()?")
        .expect("coalesce route");
    let rotation_branch = transformations
        .find("if configuration.flatten_rotation {")
        .expect("rotation configuration branch");
    let rotation_transform = &transformations[rotation_branch..];
    let rotation = rotation_transform
        .find("flatten_rotation_on_document(pdf)?")
        .expect("rotation route");
    let rotation_module = include_str!("../src/job/rotate.rs");
    let acroform = rotation_module
        .find("AcroFormDocumentHelper::new(pdf)?")
        .expect("eager AcroForm analysis");
    let page_enumeration = rotation_module
        .find("PageDocumentHelper::new(pdf).get_all_pages()?")
        .expect("raw page enumeration");
    let page_iteration = rotation_module
        .find("flatten_rotation_on_page_handles(pdf, &pages)")
        .expect("raw page handle iteration");
    let labels = transformations
        .find("self.apply_page_label_transformations(pdf, configuration)?")
        .expect("page-label route");

    assert!(generate < flatten);
    assert!(flatten < coalesce);
    assert!(coalesce < rotation_branch);
    let rotation_call = rotation_branch + "if configuration.flatten_rotation {".len() + rotation;
    assert!(rotation_call < labels);
    assert!(acroform < page_enumeration);
    assert!(page_enumeration < page_iteration);
}

#[test]
fn qpdf_correspondence_records_the_provider_owner_and_order() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/qpdf-correspondence.md");
    let source = std::fs::read_to_string(path).expect("qpdf correspondence");
    assert!(source.contains("provider-backed stream"));
    assert!(source.contains("legacy stream write-back は削除済み"));
}
