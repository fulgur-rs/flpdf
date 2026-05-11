//! Smoke test for the compat matrix harness.
//!
//! Runs a small 2×2 matrix (fixtures: one-page.pdf, two-page.pdf;
//! flag-sets: plain, static-id) through the harness end-to-end.
//!
//! Asserts:
//! - The harness completes without panicking.
//! - The report contains the expected number of tuples.
//! - JSON and Markdown output are well-formed (non-empty, contain headers).
//!
//! Does NOT assert byte equality between qpdf and flpdf outputs — that is
//! the responsibility of later subtasks (.3+).

#[path = "support/mod.rs"]
mod support;

use support::{
    is_qpdf_available, run_matrix, ByteComparator, Comparator, Fixture, FlagSet, Verdict,
};

#[test]
fn smoke_matrix_runs_end_to_end() {
    if !is_qpdf_available() {
        // Smooth early return: harness already records Skipped when qpdf is
        // absent, but we can also skip the smoke entirely to keep the output
        // clean.
        eprintln!("smoke_matrix_runs_end_to_end: qpdf not available, skipping");
        return;
    }

    let fixtures = vec![Fixture::new("one-page.pdf"), Fixture::new("two-page.pdf")];

    let flag_sets = vec![FlagSet::Plain, FlagSet::StaticId];

    let comparators: Vec<&dyn Comparator> = vec![&ByteComparator];

    let report = run_matrix(&fixtures, &flag_sets, &comparators);

    // --- Structural assertions -------------------------------------------------

    // 2 fixtures × 2 flag-sets = 4 tuples.
    assert_eq!(
        report.tuple_reports.len(),
        4,
        "expected 4 tuple reports, got {}",
        report.tuple_reports.len()
    );

    // Every tuple report must have exactly one finding (one comparator).
    for tuple_report in &report.tuple_reports {
        assert_eq!(
            tuple_report.findings.len(),
            1,
            "tuple ({}, {}) should have 1 finding, got {}",
            tuple_report.fixture,
            tuple_report.flag_set,
            tuple_report.findings.len()
        );
    }

    // The verdict must be one of the three valid values for each tuple.
    // If a new Verdict variant is added later and a tuple ends up carrying
    // it, this assertion catches the gap (matches! on its own would silently
    // accept anything once a new variant slips through).
    for tuple_report in &report.tuple_reports {
        assert!(
            matches!(
                tuple_report.verdict,
                Verdict::Pass | Verdict::Fail | Verdict::Skipped
            ),
            "tuple ({}, {}) has unexpected verdict: {:?}",
            tuple_report.fixture,
            tuple_report.flag_set,
            tuple_report.verdict,
        );
    }

    // --- Format assertions -------------------------------------------------------

    let json = report.to_json();
    assert!(!json.is_empty(), "JSON output must not be empty");
    // Must be valid JSON-like structure.
    assert!(
        json.starts_with('{') && json.ends_with('}'),
        "JSON must start with '{{' and end with '}}'"
    );
    // Must contain known fixture names.
    assert!(
        json.contains("one-page.pdf"),
        "JSON must mention one-page.pdf"
    );
    assert!(
        json.contains("two-page.pdf"),
        "JSON must mention two-page.pdf"
    );
    // Must contain comparator name.
    assert!(
        json.contains("byte-equal"),
        "JSON must mention the byte-equal comparator"
    );

    let md = report.to_markdown();
    assert!(!md.is_empty(), "Markdown output must not be empty");
    assert!(
        md.contains("# Compat Matrix Report"),
        "Markdown must contain heading"
    );
    assert!(
        md.contains("one-page.pdf"),
        "Markdown must mention one-page.pdf"
    );
    assert!(
        md.contains("two-page.pdf"),
        "Markdown must mention two-page.pdf"
    );

    // Print the report for diagnostic visibility in `cargo test -- --nocapture`.
    println!("{}", md);
}
