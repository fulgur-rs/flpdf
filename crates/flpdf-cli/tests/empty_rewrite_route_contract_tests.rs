//! Route contracts for empty-primary rewrites.

use std::fs;
use std::path::PathBuf;

fn main_source() -> String {
    fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/main.rs"))
        .expect("read CLI production source")
        .replace("\r\n", "\n")
}

fn function_body<'a>(source: &'a str, signature: &str, next: &str) -> &'a str {
    source
        .split_once(signature)
        .and_then(|(_, tail)| tail.split_once(next))
        .map(|(body, _)| body)
        .unwrap_or_else(|| panic!("production function {signature:?} must exist"))
}

#[test]
fn empty_rewrite_uses_one_job_empty_input_and_run_boundary() {
    let source = main_source();
    let rewrite = function_body(
        &source,
        "fn run_rewrite(",
        "fn run_page_operations_with_qpdf_job(",
    );
    let empty_route = rewrite
        .split_once("if empty {")
        .and_then(|(_, tail)| tail.split_once("\n    let input = input.ok_or_else"))
        .map(|(body, _)| body)
        .expect("ordinary empty rewrite route");
    let empty_route = empty_route.split_whitespace().collect::<String>();

    assert!(
        empty_route.contains("run_rewrite_with_qpdf_job(None,"),
        "ordinary --empty rewrite must configure the empty input on the canonical writer Job"
    );
    for preopened_route in ["create_empty_primary_document(", "run_rewrite_opened("] {
        assert!(
            !empty_route.contains(preopened_route),
            "ordinary --empty rewrite must not use {preopened_route}"
        );
    }
    assert!(
        !source.contains("fn run_rewrite_opened<"),
        "the dedicated pre-opened writer route should be removed"
    );

    let writer_job = function_body(
        &source,
        "fn run_rewrite_with_qpdf_job(",
        "fn configure_rewrite_job(",
    );
    assert!(writer_job.contains("input: Option<&Path>"));
    assert!(writer_job.contains("job.config().empty_input()?"));
    assert!(writer_job.contains("finish_job_exit_status(job.run()?)"));
    assert!(!writer_job.contains("job.write_qpdf("));
    assert!(!writer_job.contains("job.get_exit_code()"));
}
