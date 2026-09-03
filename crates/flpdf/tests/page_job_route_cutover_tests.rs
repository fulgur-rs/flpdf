#[test]
fn single_source_pages_use_the_qpdf_job_page_specs_route() {
    let source = include_str!("../../flpdf-cli/src/main.rs");
    let start = source
        .find("fn run_page_extraction_from_single_source")
        .expect("single-source job route must have a named production function");
    let body = &source[start..];
    assert!(
        body.contains("handle_page_specs("),
        "single-source --pages must use QPDFJob::handle_page_specs"
    );
    assert!(
        !body.contains("CombinedPlan::build_repeated"),
        "single-source --pages must not build a duplicate CombinedPlan route"
    );
    assert!(
        !body.contains("collate(&plan"),
        "single-source --pages must not call the standalone collate bridge"
    );
}

#[test]
fn in_place_page_specs_share_the_qpdf_completion_boundary() {
    let cli_source = include_str!("../../flpdf-cli/src/main.rs");
    let cli_start = cli_source
        .find("fn run_page_extraction_after_plan")
        .expect("shared page completion caller must have a named CLI function");
    let cli_body = &cli_source[cli_start..];
    assert!(
        cli_body.contains("complete_in_place_page_selection("),
        "CLI InPlace page path must call the shared completion boundary"
    );
    assert!(
        !cli_body.contains("remap_outline_and_dests(pdf, &result)")
            && !cli_body.contains("QPDFJob::prune_after_subset(pdf, prune_mode)")
            && !cli_body.contains("QPDFJob::prune_acroform_after_subset(pdf, &result)"),
        "CLI InPlace page path must not duplicate page completion calls"
    );

    let lifecycle_source = include_str!("../src/job/lifecycle.rs");
    let lifecycle_start = lifecycle_source
        .find("fn run_document_erased")
        .expect("shared page completion caller must have a named job function");
    let lifecycle_body = &lifecycle_source[lifecycle_start..];
    assert!(
        lifecycle_body.contains("complete_in_place_page_selection("),
        "QPDFJob InPlace page path must call the shared completion boundary"
    );
    assert!(
        !lifecycle_body.contains("remap_outline_and_dests(pdf, &result)")
            && !lifecycle_body.contains("QPDFJob::prune_after_subset(pdf, prune_mode)")
            && !lifecycle_body.contains("QPDFJob::prune_acroform_after_subset(pdf, &result)"),
        "QPDFJob InPlace page path must not duplicate page completion calls"
    );
}
