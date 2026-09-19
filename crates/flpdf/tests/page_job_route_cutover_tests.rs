// flpdf-3yn9.48.187 moved the single-source `--pages` route from a direct
// `QPDFJob::handle_page_specs` call to `QPDFJob::create_qpdf`'s canonical
// page-spec lifecycle -- the same job/CLI boundary `run_empty_page_extraction`
// and the top-level `--pages` route already use, matching qpdf's own
// `createQPDF` -> `handlePageSpecs` call order. This test now pins that
// boundary instead of the direct call it replaced.
#[test]
fn single_source_pages_use_the_qpdf_job_create_qpdf_route() {
    let source = include_str!("../../flpdf-cli/src/main.rs");
    let start = source
        .find("fn run_page_extraction_from_single_source")
        .expect("single-source job route must have a named production function");
    let rest = &source[start..];
    // Bound the scan to this one function: stop at the next top-level `fn`
    // (this function has no nested `fn`, so the first one after its own
    // signature always belongs to the next item).
    let end = rest[1..]
        .find("\nfn ")
        .map(|offset| offset + 1)
        .unwrap_or(rest.len());
    let body = &rest[..end];
    assert!(
        body.contains("create_qpdf()"),
        "single-source --pages must use QPDFJob::create_qpdf, the same job/CLI \
         boundary run_empty_page_extraction and the top-level --pages route use"
    );
    assert!(
        !body.contains(".handle_page_specs("),
        "single-source --pages must not call QPDFJob::handle_page_specs directly \
         any more; create_qpdf's prepare_document owns that call internally"
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

/// flpdf-3yn9.48.192 moved the multi-source `--pages` route from a direct
/// `QPDFJob::handle_page_specs` call (which manually opened every source and
/// built its own `PageSpecInput` vector) to `QPDFJob::create_qpdf`'s
/// canonical page-spec lifecycle -- the same job/CLI boundary
/// `run_empty_page_extraction` and the top-level `--pages` route already
/// use, matching qpdf's own `createQPDF` -> `handlePageSpecs` call order.
#[test]
fn multi_source_pages_use_the_qpdf_job_create_qpdf_route() {
    let source = include_str!("../../flpdf-cli/src/main.rs");
    let start = source
        .find("fn run_page_extraction_from_multiple_sources")
        .expect("multi-source job route must have a named production function");
    let rest = &source[start..];
    // Bound the scan to this one function: stop at the next top-level `fn`
    // (this function has no nested `fn`, so the first one after its own
    // signature always belongs to the next item).
    let end = rest[1..]
        .find("\nfn ")
        .map(|offset| offset + 1)
        .unwrap_or(rest.len());
    let body = &rest[..end];
    assert!(
        body.contains("create_qpdf()"),
        "multi-source --pages must use QPDFJob::create_qpdf, the same job/CLI \
         boundary run_empty_page_extraction and the top-level --pages route use"
    );
    assert!(
        !body.contains(".handle_page_specs("),
        "multi-source --pages must not call QPDFJob::handle_page_specs directly \
         any more; create_qpdf's prepare_document owns that call internally"
    );
}

#[test]
fn in_place_page_specs_share_the_qpdf_completion_boundary() {
    let cli_source = include_str!("../../flpdf-cli/src/main.rs");
    let cli_start = cli_source
        .find("fn run_page_extraction_after_plan")
        .expect("shared page completion caller must have a named CLI function");
    let cli_body = &cli_source[cli_start..];
    let cli_completion = cli_body
        .find("complete_in_place_page_selection(")
        .expect("CLI InPlace page path must call the shared completion boundary");
    let cli_rotation = cli_body
        .find("configuration.rotate(")
        .expect("CLI InPlace page path must queue rotation on QPDFJob");
    assert!(
        cli_completion < cli_rotation,
        "CLI InPlace page path must apply rotation after shared completion"
    );
    assert!(
        !cli_body.contains("remap_outline_and_dests(pdf, &result)")
            && !cli_body.contains("QPDFJob::prune_after_subset(pdf, prune_mode)")
            && !cli_body.contains("QPDFJob::prune_acroform_after_subset(pdf, &result)"),
        "CLI InPlace page path must not duplicate page completion calls"
    );

    let lifecycle_source = include_str!("../src/job/lifecycle.rs");
    let lifecycle_start = lifecycle_source
        .find("fn prepare_document(")
        .expect("shared page completion caller must have a named job function");
    let lifecycle_body = &lifecycle_source[lifecycle_start..];
    let lifecycle_completion = lifecycle_body
        .find("complete_in_place_page_selection(")
        .expect("QPDFJob InPlace page path must call the shared completion boundary");
    let lifecycle_rotation = lifecycle_body[lifecycle_completion..]
        .find("self.apply_configured_rotations(")
        .map(|offset| lifecycle_completion + offset)
        .expect("QPDFJob InPlace page path must apply rotation");
    assert!(
        lifecycle_completion < lifecycle_rotation,
        "QPDFJob InPlace page path must apply rotation after shared completion"
    );
    assert!(
        !lifecycle_body.contains("remap_outline_and_dests(pdf, &result)")
            && !lifecycle_body.contains("QPDFJob::prune_after_subset(pdf, prune_mode)")
            && !lifecycle_body.contains("QPDFJob::prune_acroform_after_subset(pdf, &result)"),
        "QPDFJob InPlace page path must not duplicate page completion calls"
    );
}
