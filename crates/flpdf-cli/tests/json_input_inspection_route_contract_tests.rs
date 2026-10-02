//! Source-route contract for JSON-input and update-from-JSON inspection.

#[test]
fn json_input_inspection_completes_through_one_job_run() {
    let source = include_str!("../src/main.rs");
    let route = source
        .split_once("fn run_json_input_inspection(")
        .expect("JSON-input inspection route should remain named")
        .1
        .split_once("\nfn run_command(")
        .expect("command dispatch should follow the route")
        .0;

    assert_eq!(
        route.matches("job.run()?").count(),
        1,
        "JSON-input inspection must complete through exactly one QPDFJob::run()"
    );
    assert!(
        route.contains("finish_job_exit_status(job.run()?)"),
        "QPDFJob::run() must own inspection, warning completion, and status"
    );
    for configured_stage in [
        "let mut job = new_cli_job(cli.no_warn)",
        "pdf_open_options(cli.repair, &cli.password)?",
        "job.set_password(input_options.password)",
        "job.set_password_mode(cli.password.password_mode.into())",
        "configure_top_level_inspection_job(&mut job, cli)?",
        "configure_cli_overlay_specs(&mut job, overlay_specs)?",
        "configure_top_level_inspection_transformations(",
        "configure_top_level_attachment_mutations(&mut job, cli, attachment_segments)?",
        "job.config().empty_input()?",
        "job.set_input_file(input.clone())?",
        "configuration.json_input()",
        "configuration.update_from_json(update_from_json.to_path_buf())",
    ] {
        assert!(
            route.contains(configured_stage),
            "JSON-input inspection must configure {configured_stage} on its Job"
        );
    }

    for staged_route in [
        "File::open(",
        "create_from_json_document(",
        "open_with_description(",
        "apply_json_update_with_job(",
        "apply_transformations(",
        "inspect_configured(",
        "run_job_inspection_on_pdf(",
    ] {
        assert!(
            !route.contains(staged_route),
            "JSON-input inspection must not split the Job lifecycle at {staged_route}"
        );
    }
}
