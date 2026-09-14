fn normalize_source(source: &str) -> String {
    source.replace("\r\n", "\n")
}

#[test]
fn normalize_source_handles_windows_line_endings() {
    assert_eq!(normalize_source("first\r\nsecond\r\n"), "first\nsecond\n");
}

#[test]
fn qpdf_less_page_plan_adapters_are_not_publicly_reexported() {
    let job_module = normalize_source(include_str!("../src/job/mod.rs"));
    let crate_root = normalize_source(include_str!("../src/lib.rs"));
    let cli = normalize_source(include_str!("../../flpdf-cli/src/main.rs"));
    let example = normalize_source(include_str!("../examples/extract_pages.rs"));

    assert!(job_module.contains("#[cfg(test)]\nmod page_combine;"));
    assert!(!job_module.contains("pub use page_combine::{CombinedPage, CombinedPlan, InputSpec}"));
    assert!(!job_module.contains("pub use page_plan::{PagePlan, SelectedPage}"));
    for symbol in [
        "CombinedPage",
        "CombinedPlan",
        "InputSpec",
        "PagePlan",
        "SelectedPage",
    ] {
        assert!(
            !crate_root.contains(symbol),
            "crate root must not re-export qpdf-less page-plan adapter {symbol}"
        );
    }
    assert!(!cli.contains("flpdf::SelectedPage"));
    assert!(!cli.contains("Vec<CombinedPage>"));
    assert!(!cli.contains("\n            CombinedPage {"));
    assert!(!cli.contains("struct CliCombinedPage"));
    assert!(!cli.contains("Vec<CliCombinedPage>"));
    assert!(!cli.contains("Vec<InputSpec>"));
    assert!(!cli.contains("\n        out.push(InputSpec::new("));
    assert!(!example.contains("PagePlan"));

    let page_plan = normalize_source(include_str!("../src/job/page_plan.rs"));
    assert!(page_plan.contains("#[cfg(test)]\n    pub(crate) fn from_1based_indices"));
    assert!(page_plan.contains("#[cfg(test)]\n    pub(crate) fn source_page_count"));
    assert!(page_plan.contains("#[cfg(test)]\n    pub(crate) fn len"));
}
