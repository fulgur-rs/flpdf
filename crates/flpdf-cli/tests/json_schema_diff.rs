//! Integration test: qpdf --json=2 vs flpdf --json=2 schema-diff over a curated
//! fixture corpus (beads flpdf-9hc.11.14).

#[allow(dead_code, unused_imports)]
mod support;

use std::path::{Path, PathBuf};

use support::json_diff::{
    compute_matrix, run_flpdf_json, run_qpdf_json, Allowlist, FixtureResult, Report,
};

struct FixtureSpec {
    label: &'static str,
    relative_path: &'static str,
    password: Option<&'static str>,
}

const CORPUS: &[FixtureSpec] = &[FixtureSpec {
    label: "minimal.pdf",
    relative_path: "minimal.pdf",
    password: None,
}];

fn workspace_fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

fn allowlist_path() -> PathBuf {
    workspace_fixtures_root().join("json-diff/allowed-divergences.json")
}

fn report_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/json-diff")
}

#[test]
fn json_schema_diff_corpus() {
    if !support::is_qpdf_available() {
        eprintln!("skipping json_schema_diff_corpus: qpdf not on PATH");
        return;
    }

    let root = workspace_fixtures_root();
    let mut allowlist = Allowlist::from_path(&allowlist_path()).expect("allowlist load");

    let mut fixtures = Vec::new();
    for spec in CORPUS {
        let path = root.join(spec.relative_path);
        let qpdf_out = run_qpdf_json(&path, spec.password);
        let flpdf_out = run_flpdf_json(&path, spec.password);

        let (cells, qpdf_error, flpdf_error) = match (qpdf_out, flpdf_out) {
            (Ok(qv), Ok(fv)) => (
                compute_matrix(spec.label, &qv, &fv, &mut allowlist),
                None,
                None,
            ),
            (Err(qe), Ok(_)) => (vec![], Some(qe), None),
            (Ok(_), Err(fe)) => (vec![], None, Some(fe)),
            (Err(qe), Err(fe)) => (vec![], Some(qe), Some(fe)),
        };

        fixtures.push(FixtureResult {
            fixture: spec.label.to_string(),
            cells,
            qpdf_error,
            flpdf_error,
        });
    }

    let stale = allowlist
        .stale_entries()
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    let report = Report {
        fixtures,
        stale_allowlist: stale,
    };

    std::fs::create_dir_all(report_dir()).ok();
    std::fs::write(report_dir().join("report.md"), report.to_markdown()).ok();
    std::fs::write(report_dir().join("report.json"), report.to_json()).ok();

    let invocation_errors: Vec<String> = report
        .fixtures
        .iter()
        .flat_map(|f| {
            let mut v = vec![];
            if let Some(e) = &f.qpdf_error {
                v.push(format!("{}: qpdf: {e}", f.fixture));
            }
            if let Some(e) = &f.flpdf_error {
                v.push(format!("{}: flpdf: {e}", f.fixture));
            }
            v
        })
        .collect();
    assert!(
        invocation_errors.is_empty(),
        "tool errors:\n{}",
        invocation_errors.join("\n")
    );

    let unknown = report.unknown_divergences();
    assert!(
        unknown.is_empty(),
        "unknown divergences ({}):\n{}\n\nFull report at {}",
        unknown.len(),
        report.unknown_summary(),
        report_dir().join("report.md").display(),
    );

    assert!(
        report.stale_allowlist.is_empty(),
        "stale allowlist entries:\n{}",
        report.stale_summary(),
    );
}
