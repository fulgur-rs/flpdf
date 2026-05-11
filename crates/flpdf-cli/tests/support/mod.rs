//! Shared test harness for compat matrix tests.
//!
//! Provides types and helpers to run (fixture, flag-set) tuples through both
//! `qpdf` and `flpdf`, route outputs through [`Comparator`]s, and collect
//! structured [`MatrixReport`]s.

use std::path::{Path, PathBuf};
use std::process::Command as ShellCommand;

use assert_cmd::Command as CargoCommand;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// A named PDF fixture under `tests/fixtures/compat/`.
#[derive(Debug, Clone)]
pub struct Fixture {
    /// Short name used in reports (e.g. `"one-page.pdf"`).
    pub name: String,
    /// Absolute path to the fixture file.
    pub path: PathBuf,
}

impl Fixture {
    /// Construct a fixture from its file name, resolved relative to
    /// `CARGO_MANIFEST_DIR/../../tests/fixtures/compat/`.
    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into();
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat")
            .join(&name);
        Self { name, path }
    }
}

// ---------------------------------------------------------------------------
// FlagSet
// ---------------------------------------------------------------------------

/// A named group of flags to pass to both tools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlagSet {
    /// No extra flags — plain rewrite.
    Plain,
    /// `--static-id` (deterministic ID for testing).
    StaticId,
    /// `--linearize` (fast web view).
    Linearize,
}

impl FlagSet {
    /// Human-readable identifier used in reports.
    pub fn name(&self) -> &'static str {
        match self {
            FlagSet::Plain => "plain",
            FlagSet::StaticId => "static-id",
            FlagSet::Linearize => "linearize",
        }
    }

    /// Extra CLI args to append to `flpdf rewrite <in> <out>`.
    pub fn flpdf_args(&self) -> Vec<&'static str> {
        match self {
            FlagSet::Plain => vec![],
            FlagSet::StaticId => vec!["--static-id"],
            FlagSet::Linearize => vec!["--linearize"],
        }
    }

    /// Extra CLI args to append to `qpdf <in> <out>`.
    pub fn qpdf_args(&self) -> Vec<&'static str> {
        match self {
            FlagSet::Plain => vec![],
            FlagSet::StaticId => vec!["--static-id"],
            FlagSet::Linearize => vec!["--linearize"],
        }
    }
}

// ---------------------------------------------------------------------------
// Tuple
// ---------------------------------------------------------------------------

/// A single (fixture, flag-set) pair that forms one row in the matrix.
#[derive(Debug, Clone)]
pub struct Tuple {
    pub fixture: Fixture,
    pub flag_set: FlagSet,
}

// ---------------------------------------------------------------------------
// RunOutputs
// ---------------------------------------------------------------------------

/// The output produced when running one tool on one tuple.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    /// Whether the process exited successfully.
    #[allow(dead_code)]
    pub success: bool,
    /// The process exit code (None if the process could not be spawned).
    #[allow(dead_code)]
    pub exit_code: Option<i32>,
    /// stdout captured from the process.
    #[allow(dead_code)]
    pub stdout: Vec<u8>,
    /// stderr captured from the process.
    #[allow(dead_code)]
    pub stderr: Vec<u8>,
    /// Bytes of the output file, if the process succeeded and produced one.
    pub output_bytes: Option<Vec<u8>>,
}

/// Both tools' outputs for one tuple.
#[derive(Debug, Clone)]
pub struct RunOutputs {
    pub qpdf: ToolOutput,
    pub flpdf: ToolOutput,
}

// ---------------------------------------------------------------------------
// Comparator trait
// ---------------------------------------------------------------------------

/// The outcome of one comparator's evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComparatorResult {
    /// The outputs matched according to this comparator's criterion.
    Match,
    /// The outputs differed.  `reason` describes the divergence.
    Diverge { reason: String },
    /// The comparator could not run (e.g. one or both tools failed).
    Skipped { reason: String },
}

impl ComparatorResult {
    #[allow(dead_code)]
    pub fn is_pass(&self) -> bool {
        matches!(self, Self::Match)
    }
}

/// A named comparator that compares `RunOutputs` for a single tuple.
pub trait Comparator {
    /// Stable, human-readable identifier (e.g. `"byte-equal"`).
    fn name(&self) -> &str;
    /// Evaluate the outputs and return a result.
    fn compare(&self, outputs: &RunOutputs) -> ComparatorResult;
}

// ---------------------------------------------------------------------------
// Built-in comparators
// ---------------------------------------------------------------------------

/// Checks that both tools produced exactly the same output bytes.
pub struct ByteComparator;

impl Comparator for ByteComparator {
    fn name(&self) -> &str {
        "byte-equal"
    }

    fn compare(&self, outputs: &RunOutputs) -> ComparatorResult {
        let (Some(qpdf_bytes), Some(flpdf_bytes)) = (
            outputs.qpdf.output_bytes.as_ref(),
            outputs.flpdf.output_bytes.as_ref(),
        ) else {
            let reason =
                if outputs.qpdf.output_bytes.is_none() && outputs.flpdf.output_bytes.is_none() {
                    "both tools produced no output".to_string()
                } else if outputs.qpdf.output_bytes.is_none() {
                    "qpdf produced no output".to_string()
                } else {
                    "flpdf produced no output".to_string()
                };
            return ComparatorResult::Skipped { reason };
        };

        if qpdf_bytes == flpdf_bytes {
            return ComparatorResult::Match;
        }

        let reason = if qpdf_bytes.len() != flpdf_bytes.len() {
            format!(
                "byte lengths differ: qpdf={} flpdf={}",
                qpdf_bytes.len(),
                flpdf_bytes.len()
            )
        } else {
            let first_diff = qpdf_bytes
                .iter()
                .zip(flpdf_bytes.iter())
                .position(|(q, f)| q != f)
                .expect("byte slices are unequal but no differing position found");
            format!(
                "bytes differ at offset {} (len={}): qpdf=0x{:02x} flpdf=0x{:02x}",
                first_diff,
                qpdf_bytes.len(),
                qpdf_bytes[first_diff],
                flpdf_bytes[first_diff]
            )
        };
        ComparatorResult::Diverge { reason }
    }
}

// ---------------------------------------------------------------------------
// Verdict
// ---------------------------------------------------------------------------

/// Overall verdict for one tuple (across all comparators).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// All comparators returned `Match`.
    Pass,
    /// At least one comparator returned `Diverge`.
    Fail,
    /// All comparators were `Skipped` (e.g. one tool did not produce output).
    Skipped,
}

// ---------------------------------------------------------------------------
// TupleReport
// ---------------------------------------------------------------------------

/// Per-comparator finding for one tuple.
#[derive(Debug, Clone)]
pub struct ComparatorFinding {
    /// Name of the comparator (mirrors [`Comparator::name`]).
    pub comparator: String,
    /// This comparator's result.
    pub result: ComparatorResult,
}

/// The full report for a single (fixture, flag-set) tuple.
#[derive(Debug, Clone)]
pub struct TupleReport {
    pub fixture: String,
    pub flag_set: String,
    pub verdict: Verdict,
    pub findings: Vec<ComparatorFinding>,
}

// ---------------------------------------------------------------------------
// MatrixReport
// ---------------------------------------------------------------------------

/// The aggregated report over all (fixture, flag-set) tuples.
#[derive(Debug, Clone)]
pub struct MatrixReport {
    pub tuple_reports: Vec<TupleReport>,
}

impl MatrixReport {
    /// Number of tuples that passed.
    pub fn pass_count(&self) -> usize {
        self.tuple_reports
            .iter()
            .filter(|r| r.verdict == Verdict::Pass)
            .count()
    }

    /// Number of tuples that failed.
    pub fn fail_count(&self) -> usize {
        self.tuple_reports
            .iter()
            .filter(|r| r.verdict == Verdict::Fail)
            .count()
    }

    /// Number of tuples that were skipped.
    pub fn skip_count(&self) -> usize {
        self.tuple_reports
            .iter()
            .filter(|r| r.verdict == Verdict::Skipped)
            .count()
    }

    // -----------------------------------------------------------------------
    // Formatters
    // -----------------------------------------------------------------------

    /// Render the report as a Markdown table.
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# Compat Matrix Report\n\n");
        out.push_str(&format!(
            "pass={} fail={} skip={}\n\n",
            self.pass_count(),
            self.fail_count(),
            self.skip_count()
        ));
        out.push_str("| fixture | flag_set | verdict | details |\n");
        out.push_str("|---------|----------|---------|----------|\n");

        for report in &self.tuple_reports {
            let verdict_str = match report.verdict {
                Verdict::Pass => "PASS",
                Verdict::Fail => "FAIL",
                Verdict::Skipped => "SKIP",
            };
            let details = report
                .findings
                .iter()
                .map(|f| {
                    let tag = match &f.result {
                        ComparatorResult::Match => format!("{}:match", f.comparator),
                        ComparatorResult::Diverge { reason } => {
                            format!("{}:diverge({})", f.comparator, reason)
                        }
                        ComparatorResult::Skipped { reason } => {
                            format!("{}:skip({})", f.comparator, reason)
                        }
                    };
                    tag
                })
                .collect::<Vec<_>>()
                .join("; ");
            out.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                report.fixture, report.flag_set, verdict_str, details
            ));
        }

        out
    }

    /// Render the report as a JSON string (no external dependencies).
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        out.push_str("{\n");
        out.push_str(&format!("  \"pass\": {},\n", self.pass_count()));
        out.push_str(&format!("  \"fail\": {},\n", self.fail_count()));
        out.push_str(&format!("  \"skip\": {},\n", self.skip_count()));
        out.push_str("  \"tuples\": [\n");

        for (idx, report) in self.tuple_reports.iter().enumerate() {
            let verdict_str = match report.verdict {
                Verdict::Pass => "pass",
                Verdict::Fail => "fail",
                Verdict::Skipped => "skip",
            };
            out.push_str("    {\n");
            out.push_str(&format!(
                "      \"fixture\": {},\n",
                json_string(&report.fixture)
            ));
            out.push_str(&format!(
                "      \"flag_set\": {},\n",
                json_string(&report.flag_set)
            ));
            out.push_str(&format!(
                "      \"verdict\": {},\n",
                json_string(verdict_str)
            ));
            out.push_str("      \"findings\": [\n");

            for (fidx, finding) in report.findings.iter().enumerate() {
                let (result_tag, reason_field) = match &finding.result {
                    ComparatorResult::Match => ("match", None),
                    ComparatorResult::Diverge { reason } => ("diverge", Some(reason.as_str())),
                    ComparatorResult::Skipped { reason } => ("skip", Some(reason.as_str())),
                };
                out.push_str("        {\n");
                out.push_str(&format!(
                    "          \"comparator\": {},\n",
                    json_string(&finding.comparator)
                ));
                out.push_str(&format!(
                    "          \"result\": {}",
                    json_string(result_tag)
                ));
                if let Some(r) = reason_field {
                    out.push_str(",\n");
                    out.push_str(&format!("          \"reason\": {}", json_string(r)));
                }
                out.push('\n');
                out.push_str("        }");
                if fidx + 1 < report.findings.len() {
                    out.push(',');
                }
                out.push('\n');
            }

            out.push_str("      ]\n");
            out.push_str("    }");
            if idx + 1 < self.tuple_reports.len() {
                out.push(',');
            }
            out.push('\n');
        }

        out.push_str("  ]\n");
        out.push('}');
        out
    }
}

// ---------------------------------------------------------------------------
// JSON helper
// ---------------------------------------------------------------------------

/// Minimal JSON string escaper (no external deps).
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------------------
// qpdf availability
// ---------------------------------------------------------------------------

/// Returns `true` if `qpdf` is found on `PATH` and responds to `--version`.
pub fn is_qpdf_available() -> bool {
    ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Internal runner helpers
// ---------------------------------------------------------------------------

fn run_qpdf_for_tuple(fixture_path: &Path, flag_set: &FlagSet, out_dir: &Path) -> ToolOutput {
    let output_path = out_dir.join("qpdf-out.pdf");
    let out_path_str = output_path.to_str().unwrap_or("").to_string();
    let fixture_str = fixture_path.to_str().unwrap_or("").to_string();

    let mut cmd_args: Vec<String> = flag_set.qpdf_args().iter().map(|s| s.to_string()).collect();
    cmd_args.push(fixture_str);
    cmd_args.push(out_path_str);

    let result = ShellCommand::new("qpdf").args(&cmd_args).output();

    match result {
        Err(e) => ToolOutput {
            success: false,
            exit_code: None,
            stdout: vec![],
            stderr: e.to_string().into_bytes(),
            output_bytes: None,
        },
        Ok(out) => {
            let output_bytes = if out.status.success() {
                std::fs::read(&output_path).ok()
            } else {
                None
            };
            ToolOutput {
                success: out.status.success(),
                exit_code: out.status.code(),
                stdout: out.stdout,
                stderr: out.stderr,
                output_bytes,
            }
        }
    }
}

fn run_flpdf_for_tuple(fixture_path: &Path, flag_set: &FlagSet, out_dir: &Path) -> ToolOutput {
    let output_path = out_dir.join("flpdf-out.pdf");
    let fixture_str = fixture_path.to_str().unwrap_or("").to_string();
    let out_path_str = output_path.to_str().unwrap_or("").to_string();

    // Build args: rewrite <in> <out> [extra flags...]
    let mut cmd = CargoCommand::cargo_bin("flpdf").expect("flpdf binary must exist");
    cmd.arg("rewrite");
    for arg in flag_set.flpdf_args() {
        cmd.arg(arg);
    }
    cmd.arg(&fixture_str);
    cmd.arg(&out_path_str);

    let result = cmd.output();

    match result {
        Err(e) => ToolOutput {
            success: false,
            exit_code: None,
            stdout: vec![],
            stderr: e.to_string().into_bytes(),
            output_bytes: None,
        },
        Ok(out) => {
            let output_bytes = if out.status.success() {
                std::fs::read(&output_path).ok()
            } else {
                None
            };
            ToolOutput {
                success: out.status.success(),
                exit_code: out.status.code(),
                stdout: out.stdout,
                stderr: out.stderr,
                output_bytes,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Matrix runner
// ---------------------------------------------------------------------------

/// Run the full matrix of `(fixture × flag_set)` tuples, evaluate each with
/// all supplied `comparators`, and return a [`MatrixReport`].
///
/// If `qpdf` is not available, all tuples are recorded as `Verdict::Skipped`.
pub fn run_matrix(
    fixtures: &[Fixture],
    flag_sets: &[FlagSet],
    comparators: &[&dyn Comparator],
) -> MatrixReport {
    let qpdf_present = is_qpdf_available();

    let mut tuple_reports = Vec::new();

    for fixture in fixtures {
        for flag_set in flag_sets {
            let tuple_key = Tuple {
                fixture: fixture.clone(),
                flag_set: flag_set.clone(),
            };

            if !qpdf_present {
                let findings = comparators
                    .iter()
                    .map(|c| ComparatorFinding {
                        comparator: c.name().to_string(),
                        result: ComparatorResult::Skipped {
                            reason: "qpdf not available".to_string(),
                        },
                    })
                    .collect::<Vec<_>>();
                tuple_reports.push(TupleReport {
                    fixture: tuple_key.fixture.name.clone(),
                    flag_set: tuple_key.flag_set.name().to_string(),
                    verdict: Verdict::Skipped,
                    findings,
                });
                continue;
            }

            // Create a temporary directory for this tuple's outputs.
            let tmp: TempDir = tempfile::tempdir().expect("failed to create tempdir");

            let qpdf_output = run_qpdf_for_tuple(&fixture.path, flag_set, tmp.path());
            let flpdf_output = run_flpdf_for_tuple(&fixture.path, flag_set, tmp.path());

            let run_outputs = RunOutputs {
                qpdf: qpdf_output,
                flpdf: flpdf_output,
            };

            let findings: Vec<ComparatorFinding> = comparators
                .iter()
                .map(|c| ComparatorFinding {
                    comparator: c.name().to_string(),
                    result: c.compare(&run_outputs),
                })
                .collect();

            let verdict = if findings.iter().all(|f| f.result == ComparatorResult::Match) {
                Verdict::Pass
            } else if findings
                .iter()
                .all(|f| matches!(f.result, ComparatorResult::Skipped { .. }))
            {
                Verdict::Skipped
            } else if findings
                .iter()
                .any(|f| matches!(f.result, ComparatorResult::Diverge { .. }))
            {
                Verdict::Fail
            } else {
                // Mix of Match and Skipped: treat as Pass (skipped comparators are advisory).
                Verdict::Pass
            };

            tuple_reports.push(TupleReport {
                fixture: tuple_key.fixture.name.clone(),
                flag_set: tuple_key.flag_set.name().to_string(),
                verdict,
                findings,
            });
        }
    }

    MatrixReport { tuple_reports }
}

// ---------------------------------------------------------------------------
// Unit tests for internal helpers
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_string_escapes_special_chars() {
        assert_eq!(json_string("hello"), "\"hello\"");
        assert_eq!(json_string("a\"b"), "\"a\\\"b\"");
        assert_eq!(json_string("a\\b"), "\"a\\\\b\"");
        assert_eq!(json_string("a\nb"), "\"a\\nb\"");
        assert_eq!(json_string("a\tb"), "\"a\\tb\"");
        assert_eq!(json_string("a\x01b"), "\"a\\u0001b\"");
    }

    #[test]
    fn flag_set_names_are_stable() {
        assert_eq!(FlagSet::Plain.name(), "plain");
        assert_eq!(FlagSet::StaticId.name(), "static-id");
        assert_eq!(FlagSet::Linearize.name(), "linearize");
    }

    #[test]
    fn byte_comparator_skips_when_no_output() {
        let outputs = RunOutputs {
            qpdf: ToolOutput {
                success: false,
                exit_code: Some(1),
                stdout: vec![],
                stderr: vec![],
                output_bytes: None,
            },
            flpdf: ToolOutput {
                success: false,
                exit_code: Some(1),
                stdout: vec![],
                stderr: vec![],
                output_bytes: None,
            },
        };
        let result = ByteComparator.compare(&outputs);
        assert!(matches!(result, ComparatorResult::Skipped { .. }));
    }

    #[test]
    fn byte_comparator_matches_identical_bytes() {
        let bytes = vec![1u8, 2, 3];
        let outputs = RunOutputs {
            qpdf: ToolOutput {
                success: true,
                exit_code: Some(0),
                stdout: vec![],
                stderr: vec![],
                output_bytes: Some(bytes.clone()),
            },
            flpdf: ToolOutput {
                success: true,
                exit_code: Some(0),
                stdout: vec![],
                stderr: vec![],
                output_bytes: Some(bytes),
            },
        };
        let result = ByteComparator.compare(&outputs);
        assert_eq!(result, ComparatorResult::Match);
    }

    #[test]
    fn byte_comparator_diverges_on_different_lengths() {
        let outputs = RunOutputs {
            qpdf: ToolOutput {
                success: true,
                exit_code: Some(0),
                stdout: vec![],
                stderr: vec![],
                output_bytes: Some(vec![1, 2, 3]),
            },
            flpdf: ToolOutput {
                success: true,
                exit_code: Some(0),
                stdout: vec![],
                stderr: vec![],
                output_bytes: Some(vec![4, 5, 6, 7]),
            },
        };
        let result = ByteComparator.compare(&outputs);
        let ComparatorResult::Diverge { reason } = result else {
            panic!("expected Diverge");
        };
        assert!(
            reason.contains("byte lengths differ"),
            "expected length-diff reason, got: {reason}"
        );
    }

    #[test]
    fn byte_comparator_diverges_on_same_length_different_content() {
        let outputs = RunOutputs {
            qpdf: ToolOutput {
                success: true,
                exit_code: Some(0),
                stdout: vec![],
                stderr: vec![],
                output_bytes: Some(vec![1, 2, 3, 4]),
            },
            flpdf: ToolOutput {
                success: true,
                exit_code: Some(0),
                stdout: vec![],
                stderr: vec![],
                output_bytes: Some(vec![1, 2, 9, 4]),
            },
        };
        let result = ByteComparator.compare(&outputs);
        let ComparatorResult::Diverge { reason } = result else {
            panic!("expected Diverge");
        };
        assert!(
            reason.contains("offset 2"),
            "expected first-diff offset 2, got: {reason}"
        );
        assert!(
            reason.contains("len=4"),
            "expected length tag, got: {reason}"
        );
    }

    #[test]
    fn matrix_report_to_json_is_non_empty() {
        let report = MatrixReport {
            tuple_reports: vec![TupleReport {
                fixture: "one-page.pdf".to_string(),
                flag_set: "plain".to_string(),
                verdict: Verdict::Pass,
                findings: vec![ComparatorFinding {
                    comparator: "byte-equal".to_string(),
                    result: ComparatorResult::Match,
                }],
            }],
        };
        let json = report.to_json();
        assert!(!json.is_empty());
        assert!(json.contains("\"pass\": 1"));
        assert!(json.contains("\"fail\": 0"));
        assert!(json.contains("one-page.pdf"));
    }

    #[test]
    fn matrix_report_to_markdown_is_non_empty() {
        let report = MatrixReport {
            tuple_reports: vec![TupleReport {
                fixture: "one-page.pdf".to_string(),
                flag_set: "plain".to_string(),
                verdict: Verdict::Pass,
                findings: vec![ComparatorFinding {
                    comparator: "byte-equal".to_string(),
                    result: ComparatorResult::Match,
                }],
            }],
        };
        let md = report.to_markdown();
        assert!(!md.is_empty());
        assert!(md.contains("PASS"));
        assert!(md.contains("one-page.pdf"));
    }
}
