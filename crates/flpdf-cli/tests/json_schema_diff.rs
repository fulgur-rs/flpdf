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

const CORPUS: &[FixtureSpec] = &[
    FixtureSpec {
        label: "minimal.pdf",
        relative_path: "minimal.pdf",
        password: None,
    },
    FixtureSpec {
        label: "compat/one-page.pdf",
        relative_path: "compat/one-page.pdf",
        password: None,
    },
    FixtureSpec {
        label: "compat/two-page.pdf",
        relative_path: "compat/two-page.pdf",
        password: None,
    },
    FixtureSpec {
        label: "compat/three-page.pdf",
        relative_path: "compat/three-page.pdf",
        password: None,
    },
    FixtureSpec {
        label: "compat/linearized-one-page.pdf",
        relative_path: "compat/linearized-one-page.pdf",
        password: None,
    },
    FixtureSpec {
        label: "compat/attachment-two-page.pdf",
        relative_path: "compat/attachment-two-page.pdf",
        password: None,
    },
    FixtureSpec {
        label: "compat/multi-contents-one-page.pdf",
        relative_path: "compat/multi-contents-one-page.pdf",
        password: None,
    },
    FixtureSpec {
        label: "compat/unref-resources-one-page.pdf",
        relative_path: "compat/unref-resources-one-page.pdf",
        password: None,
    },
    // qdf-fix/qdf-golden/qdf-roundtrip: QDF-form PDFs (qpdf's debug output
    // format, re-parsed as PDFs). Cover diverse content trees and xref shapes.
    FixtureSpec {
        label: "qdf-fix/one-page-clean.qdf",
        relative_path: "qdf-fix/one-page-clean.qdf",
        password: None,
    },
    FixtureSpec {
        label: "qdf-golden/minimal.qdf",
        relative_path: "qdf-golden/minimal.qdf",
        password: None,
    },
    FixtureSpec {
        label: "qdf-roundtrip/three-page-clean.qdf",
        relative_path: "qdf-roundtrip/three-page-clean.qdf",
        password: None,
    },
    // SKIP: qdf-roundtrip/three-page-edited-payload.qdf — intentionally damaged
    // (qpdf reconstructs xref with warnings, flpdf rejects with "parse error
    // at byte 3: expected integer"). Schema-diff is not the right test
    // surface for repair-path divergence.
    // SKIP: qdf-fix/corrupt-*.qdf — all intentionally corrupted variants used
    // by the qdf-fix repair test; same reason as above.
    // Encrypted: AES-128 (V=4 R=4) and AES-256 (V=5 R=6). RC4 fixtures
    // require flpdf's --allow-weak-crypto flag, which the support module's
    // run_flpdf_json helper does not currently pass, so they are excluded.
    FixtureSpec {
        label: "encrypted/v4-aes-128-r4.pdf",
        relative_path: "encrypted/v4-aes-128-r4.pdf",
        password: Some("user-v4-aes"),
    },
    FixtureSpec {
        label: "encrypted/v5-aes-256-r6.pdf",
        relative_path: "encrypted/v5-aes-256-r6.pdf",
        password: Some("user-v5-r6"),
    },
];

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

    let out_dir = report_dir();
    std::fs::create_dir_all(&out_dir).expect("create target/json-diff");
    std::fs::write(out_dir.join("report.md"), report.to_markdown())
        .expect("write target/json-diff/report.md");
    std::fs::write(out_dir.join("report.json"), report.to_json())
        .expect("write target/json-diff/report.json");

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
