// Rewrite page extraction is configured on one QPDFJob and completed by the
// shared CLI runner. QPDFJob::create_qpdf owns source opening, page selection,
// and in-place page completion; QPDFJob::run owns the single create/write
// lifecycle for ordinary, multi-source, and empty-primary extraction.

fn function_body<'a>(source: &'a str, signature: &str, next_signature: &str) -> &'a str {
    source
        .split_once(signature)
        .expect("named production function")
        .1
        .split_once(next_signature)
        .expect("following production function")
        .0
}

fn assert_shared_job_runner(route: &str, label: &str) {
    assert!(
        route.contains("run_page_operation_job("),
        "{label} must use the shared page-operation Job runner"
    );
    for legacy_route in [
        "job.create_qpdf()",
        "job.write_qpdf(",
        "complete_in_place_page_selection(",
    ] {
        assert!(
            !route.contains(legacy_route),
            "{label} must not own {legacy_route}"
        );
    }
}

#[test]
fn single_source_pages_use_the_shared_job_runner() {
    let source = include_str!("../../flpdf-cli/src/main.rs");
    let route = function_body(
        source,
        "fn run_page_extraction(",
        "\nfn run_empty_page_extraction",
    );
    assert_shared_job_runner(route, "single-source --pages");
}

#[test]
fn empty_and_multi_source_pages_use_the_shared_job_runner() {
    let source = include_str!("../../flpdf-cli/src/main.rs");
    let route = function_body(
        source,
        "fn run_empty_page_extraction(",
        "\nfn split_pages_active",
    );
    assert_shared_job_runner(route, "empty-primary and multi-source --pages");
}

#[test]
fn in_place_page_specs_share_the_qpdf_completion_boundary() {
    let cli_source = include_str!("../../flpdf-cli/src/main.rs");
    let runner = function_body(
        cli_source,
        "fn run_page_operation_job(",
        "\nfn run_rewrite_with_qpdf_job",
    );
    assert_eq!(
        runner.matches("job.run()?").count(),
        1,
        "all page-source forms must complete through one QPDFJob::run()"
    );
    assert!(runner.contains("configure_page_selection_job(&mut job, page_ops, remove_unref)"));

    let lifecycle_source = include_str!("../src/job/lifecycle.rs");
    let lifecycle_body = lifecycle_source
        .split_once("fn prepare_document(")
        .expect("shared page completion caller must have a named Job function")
        .1;
    let lifecycle_completion = lifecycle_body
        .find("complete_in_place_page_selection(")
        .expect("QPDFJob in-place page path must call the shared completion boundary");
    let lifecycle_rotation = lifecycle_body[lifecycle_completion..]
        .find("self.apply_configured_rotations(")
        .map(|offset| lifecycle_completion + offset)
        .expect("QPDFJob in-place page path must apply rotation");
    assert!(
        lifecycle_completion < lifecycle_rotation,
        "QPDFJob in-place page path must apply rotation after shared completion"
    );
    assert!(
        !lifecycle_body.contains("remap_outline_and_dests(pdf, &result)")
            && !lifecycle_body.contains("QPDFJob::prune_after_subset(pdf, prune_mode)")
            && !lifecycle_body.contains("QPDFJob::prune_acroform_after_subset(pdf, &result)"),
        "QPDFJob in-place page path must not duplicate page-completion calls"
    );
}
