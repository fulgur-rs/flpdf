//! Baseline byte-comparison test: `flpdf rewrite --static-id` vs golden files.
//!
//! For each fixture in the curated corpus:
//! - If no golden file exists (linearized-one-page, encrypted-r4-three-page) → **skip**
//! - If `flpdf rewrite --static-id` exits non-zero → **fail**
//! - Otherwise compare output bytes to golden bytes via `ByteComparator`.
//!
//! Results are rendered as a Markdown table and either:
//! - compared byte-for-byte against `tests/golden/baseline-static-id.md`, or
//! - written there when the environment variable `BLESS=1` is set.
//!
//! Run initial generation with:
//!   `BLESS=1 cargo test --test compat_baseline_static_id`

#[allow(dead_code, unused_imports)]
#[path = "support/mod.rs"]
mod support;

use std::path::{Path, PathBuf};

use assert_cmd::Command as CargoCommand;
use support::{ByteComparator, Comparator, ComparatorResult, RunOutputs, ToolOutput};

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .to_path_buf()
}

fn golden_references_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references")
        .to_path_buf()
}

fn baseline_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/baseline-static-id.md")
        .to_path_buf()
}

// ---------------------------------------------------------------------------
// Fixture descriptor
// ---------------------------------------------------------------------------

struct FixtureEntry {
    name: &'static str,
    has_golden: bool,
}

const FIXTURES: &[FixtureEntry] = &[
    FixtureEntry {
        name: "one-page.pdf",
        has_golden: true,
    },
    FixtureEntry {
        name: "two-page.pdf",
        has_golden: true,
    },
    FixtureEntry {
        name: "three-page.pdf",
        has_golden: true,
    },
    FixtureEntry {
        name: "attachment-two-page.pdf",
        has_golden: true,
    },
    FixtureEntry {
        name: "linearized-one-page.pdf",
        has_golden: false,
    },
    FixtureEntry {
        name: "encrypted-r4-three-page.pdf",
        has_golden: false,
    },
];

// ---------------------------------------------------------------------------
// Row type
// ---------------------------------------------------------------------------

struct Row {
    fixture: &'static str,
    flpdf_bytes: Option<usize>,
    golden_bytes: Option<usize>,
    verdict: RowVerdict,
    first_diff: String,
}

enum RowVerdict {
    Match,
    Diverge,
    Skip,
    Fail,
}

impl RowVerdict {
    fn as_str(&self) -> &'static str {
        match self {
            RowVerdict::Match => "match",
            RowVerdict::Diverge => "diverge",
            RowVerdict::Skip => "skip",
            RowVerdict::Fail => "fail",
        }
    }
}

// ---------------------------------------------------------------------------
// Markdown renderer
// ---------------------------------------------------------------------------

fn render_markdown(rows: &[Row]) -> String {
    let mut out = String::new();
    out.push_str("# Static-ID Baseline (flpdf vs qpdf --static-id)\n\n");
    out.push_str("| fixture | flpdf bytes | golden bytes | verdict | first-diff |\n");
    out.push_str("|---|---|---|---|---|\n");
    for row in rows {
        let flpdf_col = row
            .flpdf_bytes
            .map(|n| n.to_string())
            .unwrap_or_else(|| "-".to_string());
        let golden_col = row
            .golden_bytes
            .map(|n| n.to_string())
            .unwrap_or_else(|| "-".to_string());
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            row.fixture,
            flpdf_col,
            golden_col,
            row.verdict.as_str(),
            row.first_diff
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// BLESS helper
// ---------------------------------------------------------------------------

fn check_or_bless(actual: &str) {
    let path = baseline_path();
    if std::env::var("BLESS").is_ok() {
        std::fs::write(&path, actual).expect("failed to write baseline");
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .expect("baseline file must exist; run with BLESS=1 to create it");
    if actual != expected {
        panic!(
            "baseline drift detected\n\n--- expected ---\n{expected}\n\n--- actual ---\n{actual}\n\nRun with BLESS=1 to update."
        );
    }
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

#[test]
fn static_id_baseline() {
    let cmp = ByteComparator;
    let mut rows: Vec<Row> = Vec::new();

    for entry in FIXTURES {
        // --- no golden → skip ---
        if !entry.has_golden {
            rows.push(Row {
                fixture: entry.name,
                flpdf_bytes: None,
                golden_bytes: None,
                verdict: RowVerdict::Skip,
                first_diff: "no golden for --static-id".to_string(),
            });
            continue;
        }

        let fixture_path = fixtures_dir().join(entry.name);
        let stem = entry.name.strip_suffix(".pdf").unwrap_or(entry.name);
        let golden_path = golden_references_dir().join(stem).join("static-id.pdf");

        // Read golden bytes.
        let golden_bytes = std::fs::read(&golden_path)
            .unwrap_or_else(|e| panic!("failed to read golden {}: {e}", golden_path.display()));

        // Run flpdf rewrite --static-id.
        let tmp_dir = tempfile::tempdir().expect("failed to create tempdir");
        let out_path = tmp_dir.path().join("flpdf-out.pdf");

        let result = CargoCommand::cargo_bin("flpdf")
            .expect("flpdf binary must exist")
            .arg("rewrite")
            .arg("--static-id")
            .arg(fixture_path.to_str().unwrap())
            .arg(out_path.to_str().unwrap())
            .output()
            .expect("failed to spawn flpdf");

        if !result.status.success() {
            rows.push(Row {
                fixture: entry.name,
                flpdf_bytes: None,
                golden_bytes: Some(golden_bytes.len()),
                verdict: RowVerdict::Fail,
                first_diff: format!("flpdf exited {:?}", result.status.code()),
            });
            continue;
        }

        let flpdf_bytes =
            std::fs::read(&out_path).expect("flpdf output file missing after success");

        // Use ByteComparator via synthetic RunOutputs.
        let run_outputs = RunOutputs {
            qpdf: ToolOutput {
                success: true,
                exit_code: Some(0),
                stdout: vec![],
                stderr: vec![],
                output_bytes: Some(golden_bytes.clone()),
            },
            flpdf: ToolOutput {
                success: true,
                exit_code: Some(0),
                stdout: vec![],
                stderr: vec![],
                output_bytes: Some(flpdf_bytes.clone()),
            },
        };

        let cmp_result = cmp.compare(&run_outputs);

        let (verdict, first_diff) = match cmp_result {
            ComparatorResult::Match => (RowVerdict::Match, "-".to_string()),
            ComparatorResult::Diverge { reason } => {
                // Re-format for baseline table: extract first-diff info.
                let first_diff_col = if reason.contains("byte lengths differ") {
                    format!(
                        "length mismatch (flpdf={} golden={})",
                        flpdf_bytes.len(),
                        golden_bytes.len()
                    )
                } else {
                    // "bytes differ at offset N (len=L): qpdf=0xAA flpdf=0xBB"
                    // Re-emit as "offset N (0xAA vs 0xBB)" (golden vs flpdf).
                    extract_first_diff_display(&golden_bytes, &flpdf_bytes)
                };
                (RowVerdict::Diverge, first_diff_col)
            }
            ComparatorResult::Skipped { reason } => (RowVerdict::Skip, reason),
        };

        rows.push(Row {
            fixture: entry.name,
            flpdf_bytes: Some(flpdf_bytes.len()),
            golden_bytes: Some(golden_bytes.len()),
            verdict,
            first_diff,
        });
    }

    let markdown = render_markdown(&rows);
    println!("{markdown}");
    check_or_bless(&markdown);
}

// ---------------------------------------------------------------------------
// Helper: produce "offset N (golden=0xAA flpdf=0xBB)" string
// ---------------------------------------------------------------------------

fn extract_first_diff_display(golden: &[u8], flpdf: &[u8]) -> String {
    let offset = golden
        .iter()
        .zip(flpdf.iter())
        .position(|(g, f)| g != f)
        .unwrap_or(0);
    format!(
        "offset {} (golden=0x{:02x} flpdf=0x{:02x})",
        offset, golden[offset], flpdf[offset]
    )
}
