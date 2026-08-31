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
