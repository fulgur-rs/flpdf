//! Round-trip linearization tests using qpdf as an external oracle.
//!
//! # Test matrix
//!
//! - **(a)** `flpdf rewrite --linearize` on plain fixtures → `qpdf --check-linearization`
//!   must accept the output.  qpdf currently reports a "hint table length mismatch"
//!   warning on every fixture; this specific message — and only this message — is
//!   classified as a known issue.  *Any other* qpdf warning text causes the test to
//!   fail, so the oracle keeps catching new regressions.
//! - **(b)** `flpdf rewrite` (without `--linearize`) of an already-linearized PDF must
//!   produce a *non-linearized* output.  **This sub-case MUST PASS.**
//! - **(c)** `flpdf check-linearization`'s Pass-vs-Fail verdict must agree with qpdf's.
//!   Verdict mismatches now hard-fail (the previous lenient behaviour was tracked as
//!   flpdf-0dl, which has been fixed).
//!
//! # Active known fingerprint
//! - **`hint table length mismatch`** — flpdf's writer / hint stream encoder produces
//!   `/H` values that disagree with qpdf's recomputed table length.  Tracked as a
//!   follow-up to flpdf-k8h (which addressed the per-page object_count placeholder
//!   but not the surrounding length encoding).
//!
//! Tests requiring qpdf are skipped silently in environments where qpdf is not installed.

use assert_cmd::Command as CargoCommand;
use std::path::{Path, PathBuf};
use std::process::Command as ShellCommand;
use tempfile::tempdir;

// ---------------------------------------------------------------------------
// Fixture directory (same as compat_matrix_tests)
// ---------------------------------------------------------------------------

const COMPAT_FIXTURE_DIR: &str = "../../tests/fixtures/compat";

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(COMPAT_FIXTURE_DIR)
        .join(name)
}

// ---------------------------------------------------------------------------
// Verdict enum
// ---------------------------------------------------------------------------

/// Normalised outcome for a single linearization check.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Verdict {
    /// Unambiguously valid linearized PDF.
    Pass,
    /// Valid but with recoverable warnings (e.g. hint table mismatch).
    Warn(String),
    /// Hard failure or malformed.
    Fail(String),
    /// File is not linearized at all.
    NotLinearized,
}

// ---------------------------------------------------------------------------
// Guards
// ---------------------------------------------------------------------------

fn qpdf_available() -> bool {
    ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// qpdf helpers
// ---------------------------------------------------------------------------

/// Run `qpdf --check-linearization <path>` and return `(exit_code, combined_output)`.
fn run_qpdf_check(path: &Path) -> (i32, String) {
    let out = ShellCommand::new("qpdf")
        .args(["--check-linearization", path.to_str().unwrap()])
        .output()
        .expect("failed to spawn qpdf");

    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let combined = format!("{stdout}{stderr}");
    (code, combined)
}

/// Classify a `(exit_code, combined_output)` pair from qpdf.
///
/// qpdf exit codes:
///   0  → OK (may still print "is not linearized")
///   2  → hard error
///   3  → warnings only (operation succeeded with warnings)
fn qpdf_verdict(exit: i32, output: &str) -> Verdict {
    if output.contains("is not linearized") {
        return Verdict::NotLinearized;
    }
    match exit {
        0 => {
            if output.contains("no linearization errors") || output.trim().is_empty() {
                Verdict::Pass
            } else {
                // unexpected content on exit 0
                Verdict::Warn(output.to_string())
            }
        }
        3 => Verdict::Warn(output.to_string()),
        _ => Verdict::Fail(output.to_string()),
    }
}

/// Classify a `qpdf --check-linearization` result for a linearized file.
///
/// Returns `None` when the file is simply "not linearized".
fn qpdf_linearization_verdict(path: &Path) -> Verdict {
    let (code, output) = run_qpdf_check(path);
    qpdf_verdict(code, &output)
}

/// Run `qpdf --linearize <input> <output>` to produce a qpdf-linearized fixture.
fn qpdf_linearize(input: &Path, output: &Path) {
    let status = ShellCommand::new("qpdf")
        .args([
            "--linearize",
            input.to_str().unwrap(),
            output.to_str().unwrap(),
        ])
        .status()
        .expect("failed to spawn qpdf --linearize");
    assert!(status.success(), "qpdf --linearize failed");
}

// ---------------------------------------------------------------------------
// flpdf helpers
// ---------------------------------------------------------------------------

/// Run `flpdf rewrite --linearize <input> <output>`.
///
/// Returns `true` if the command succeeded (exit 0).
fn linearize_via_flpdf(input: &Path, output: &Path) -> bool {
    CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args([
            "rewrite",
            "--linearize",
            input.to_str().unwrap(),
            output.to_str().unwrap(),
        ])
        .output()
        .map(|o| o.status.success())
        .expect("failed to spawn flpdf")
}

/// Run `flpdf rewrite <input> <output>` (without --linearize).
///
/// Returns `true` if the command succeeded (exit 0).
fn rewrite_via_flpdf(input: &Path, output: &Path) -> bool {
    CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args(["rewrite", input.to_str().unwrap(), output.to_str().unwrap()])
        .output()
        .map(|o| o.status.success())
        .expect("failed to spawn flpdf")
}

/// Run `flpdf check-linearization <path>` and return `(exit_code, combined_output)`.
fn flpdf_check(path: &Path) -> (i32, String) {
    let out = CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args(["check-linearization", path.to_str().unwrap()])
        .output()
        .expect("failed to spawn flpdf check-linearization");

    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let combined = format!("{stdout}{stderr}");
    (code, combined)
}

/// Classify flpdf's check-linearization result.
fn flpdf_verdict(path: &Path) -> Verdict {
    let (code, output) = flpdf_check(path);
    match code {
        0 => Verdict::Pass,
        1 => {
            if output.contains("not a linearized PDF") {
                Verdict::NotLinearized
            } else {
                Verdict::Fail(output)
            }
        }
        _ => Verdict::Fail(output),
    }
}

// ---------------------------------------------------------------------------
// Categorised result tracking
// ---------------------------------------------------------------------------

/// A single categorised test result.
struct CaseResult {
    label: String,
    status: CaseStatus,
    reason: String,
}

enum CaseStatus {
    Pass,
    Fail,
    KnownIssue,
}

impl CaseResult {
    fn pass(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            status: CaseStatus::Pass,
            reason: String::new(),
        }
    }

    fn fail(label: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            status: CaseStatus::Fail,
            reason: reason.into(),
        }
    }

    fn known(label: impl Into<String>, issue: &str, detail: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            status: CaseStatus::KnownIssue,
            reason: format!("known: {issue} — {}", detail.into()),
        }
    }
}

/// Classify a qpdf `Warn` message in the (a) matrix into a [`CaseResult`].
///
/// Only **explicitly enumerated** warning fingerprints are accepted as known
/// issues; any other warning text surfaces as a `Fail` so the oracle keeps
/// catching new regressions instead of silently absorbing them under a
/// previously-tracked label.
fn classify_qpdf_warning(label: &str, msg: &str) -> CaseResult {
    if msg.contains("hint table length mismatch") {
        CaseResult::known(
            label.to_string(),
            "linearization hint table /H mismatch (post-k8h: writer/encoder length)",
            "qpdf exit 3 with warnings",
        )
    } else {
        CaseResult::fail(label.to_string(), format!("unexpected qpdf warning: {msg}"))
    }
}

fn print_summary(results: &[CaseResult]) {
    println!();
    println!("=== cli_linearize_qpdf test summary ===");
    for r in results {
        let tag = match r.status {
            CaseStatus::Pass => "OK         ",
            CaseStatus::Fail => "FAIL       ",
            CaseStatus::KnownIssue => "KNOWN-ISSUE",
        };
        if r.reason.is_empty() {
            println!("  [{tag}] {}", r.label);
        } else {
            println!("  [{tag}] {} — {}", r.label, r.reason);
        }
    }
    println!("=======================================");
    println!();
}

// ---------------------------------------------------------------------------
// (a) flpdf --linearize → qpdf --check-linearization
// ---------------------------------------------------------------------------

#[test]
fn linearize_qpdf_check_matrix() {
    if !qpdf_available() {
        return;
    }

    let fixtures: &[(&str, &str)] = &[
        ("one-page.pdf", "(a) single-page"),
        ("two-page.pdf", "(a) multi-page (2 pages)"),
        ("three-page.pdf", "(a) multi-page (3 pages)"),
    ];

    let tmp = tempdir().unwrap();
    let mut results: Vec<CaseResult> = Vec::new();

    for (fixture_name, label) in fixtures {
        let input = fixture_path(fixture_name);
        let output = tmp.path().join(format!("linearized-{fixture_name}"));

        let success = linearize_via_flpdf(&input, &output);
        if !success {
            results.push(CaseResult::fail(
                *label,
                "flpdf --linearize exited with non-zero status",
            ));
            continue;
        }

        let verdict = qpdf_linearization_verdict(&output);
        match &verdict {
            Verdict::Pass => {
                results.push(CaseResult::pass(*label));
            }
            Verdict::Warn(msg) => results.push(classify_qpdf_warning(*label, msg)),
            Verdict::Fail(msg) => {
                results.push(CaseResult::fail(*label, format!("qpdf hard fail: {msg}")));
            }
            Verdict::NotLinearized => {
                results.push(CaseResult::fail(
                    *label,
                    "flpdf output is not recognized as linearized",
                ));
            }
        }
    }

    print_summary(&results);

    // Acceptance: all (a) cases must produce linearized output (even if warnings).
    // Hard failures in (a) are tracked; they must not be silently NotLinearized.
    let hard_failures: Vec<&CaseResult> = results
        .iter()
        .filter(|r| matches!(r.status, CaseStatus::Fail))
        .collect();

    if !hard_failures.is_empty() {
        let msgs: Vec<_> = hard_failures.iter().map(|r| r.label.as_str()).collect();
        panic!(
            "(a) hard failures that are not categorised as known issues: {:?}",
            msgs
        );
    }
}

// ---------------------------------------------------------------------------
// (b) Non-linear collapse: flpdf rewrite (no --linearize) of qpdf-linearized
//     PDF must produce a NON-linearized output.  MUST PASS.
// ---------------------------------------------------------------------------

#[test]
fn non_linear_collapse_from_qpdf_linearized() {
    if !qpdf_available() {
        return;
    }

    let tmp = tempdir().unwrap();
    let source = fixture_path("one-page.pdf");

    // Step 1: produce a qpdf-linearized intermediate.
    let qpdf_linearized = tmp.path().join("qpdf-linearized.pdf");
    qpdf_linearize(&source, &qpdf_linearized);

    // Sanity: qpdf's own output must be valid.
    let (check_exit, check_output) = run_qpdf_check(&qpdf_linearized);
    assert_eq!(
        check_exit, 0,
        "(b) pre-condition: qpdf --linearize output should be valid, got: {check_output}"
    );
    assert!(
        !check_output.contains("is not linearized"),
        "(b) pre-condition: qpdf --linearize output must be linearized"
    );

    // Step 2: flpdf rewrite WITHOUT --linearize.
    let flpdf_out = tmp.path().join("flpdf-collapsed.pdf");
    let rewrite_ok = rewrite_via_flpdf(&qpdf_linearized, &flpdf_out);
    assert!(rewrite_ok, "(b) flpdf rewrite exited with non-zero status");

    // Step 3: qpdf must say "is not linearized".
    let (qpdf_exit, qpdf_output) = run_qpdf_check(&flpdf_out);
    let verdict = qpdf_verdict(qpdf_exit, &qpdf_output);

    assert_eq!(
        verdict,
        Verdict::NotLinearized,
        "(b) MUST PASS: flpdf rewrite (no --linearize) of qpdf-linearized PDF \
         must produce a non-linearized output.\n\
         qpdf exit={qpdf_exit}, output={qpdf_output}"
    );

    println!("(b) non-linear collapse from qpdf-linearized: OK");

    // Note: flpdf rewrite uses incremental updates, so the *original* bytes (including any
    // /Linearized dictionary) are preserved as a prefix; only the incremental update section
    // appended at the end changes the effective structure.  qpdf correctly reads the most
    // recent update and reports "is not linearized".  A raw-byte scan of the prefix would
    // produce a false positive, so we rely on qpdf's semantic check only.
}

// ---------------------------------------------------------------------------
// (c) Verdict match: flpdf check-linearization vs qpdf --check-linearization
// ---------------------------------------------------------------------------

#[test]
fn verdict_match_flpdf_vs_qpdf() {
    if !qpdf_available() {
        return;
    }

    let fixtures = &["one-page.pdf", "two-page.pdf", "three-page.pdf"];

    let tmp = tempdir().unwrap();
    let mut results: Vec<CaseResult> = Vec::new();

    for fixture_name in fixtures {
        let input = fixture_path(fixture_name);
        let linearized = tmp.path().join(format!("lin-{fixture_name}"));

        // produce flpdf-linearized file
        let success = linearize_via_flpdf(&input, &linearized);
        if !success {
            results.push(CaseResult::fail(
                format!("(c) {fixture_name}"),
                "flpdf linearize failed, cannot compare verdicts",
            ));
            continue;
        }

        let fv = flpdf_verdict(&linearized);
        let qv = qpdf_linearization_verdict(&linearized);

        // Normalise to a simple Pass/Fail binary for comparison.
        let fv_pass = matches!(fv, Verdict::Pass | Verdict::Warn(_));
        let qv_pass = matches!(qv, Verdict::Pass | Verdict::Warn(_));

        let label = format!("(c) verdict match — {fixture_name}");
        if fv_pass == qv_pass {
            results.push(CaseResult::pass(label));
        } else {
            // flpdf-0dl (validator over-leniency) is closed.  A verdict
            // mismatch is now a real regression — neither checker should
            // accept what the other rejects.
            let detail = format!(
                "flpdf={} qpdf={}",
                if fv_pass { "Pass/Warn" } else { "Fail" },
                if qv_pass { "Pass/Warn" } else { "Fail" }
            );
            results.push(CaseResult::fail(
                label,
                format!("verdict mismatch between flpdf and qpdf: {detail}"),
            ));
        }
    }

    print_summary(&results);

    // After flpdf-0dl was fixed, mismatches must surface as failures so the
    // oracle catches divergence between the two implementations.
    let mismatches: Vec<&CaseResult> = results
        .iter()
        .filter(|r| matches!(r.status, CaseStatus::Fail))
        .collect();
    if !mismatches.is_empty() {
        let msgs: Vec<_> = mismatches.iter().map(|r| r.label.as_str()).collect();
        panic!(
            "(c) verdict mismatches between flpdf and qpdf (regressions): {:?}",
            msgs
        );
    }
}

// ---------------------------------------------------------------------------
// (b) Additional: two-page and three-page collapse
// ---------------------------------------------------------------------------

#[test]
fn non_linear_collapse_multi_page() {
    if !qpdf_available() {
        return;
    }

    let tmp = tempdir().unwrap();
    let mut results: Vec<CaseResult> = Vec::new();

    let fixtures = &["two-page.pdf", "three-page.pdf"];

    for fixture_name in fixtures {
        let source = fixture_path(fixture_name);
        let qpdf_linearized = tmp.path().join(format!("qpdf-lin-{fixture_name}"));
        let flpdf_out = tmp.path().join(format!("flpdf-collapsed-{fixture_name}"));

        qpdf_linearize(&source, &qpdf_linearized);
        let success = rewrite_via_flpdf(&qpdf_linearized, &flpdf_out);

        if !success {
            results.push(CaseResult::fail(
                format!("(b) multi-page collapse — {fixture_name}"),
                "flpdf rewrite exited with non-zero status",
            ));
            continue;
        }

        let (qpdf_exit, qpdf_output) = run_qpdf_check(&flpdf_out);
        let verdict = qpdf_verdict(qpdf_exit, &qpdf_output);

        let label = format!("(b) non-linear collapse — {fixture_name}");
        if verdict == Verdict::NotLinearized {
            results.push(CaseResult::pass(label));
        } else {
            results.push(CaseResult::fail(
                label,
                format!("expected NotLinearized, got {:?}", verdict),
            ));
        }
    }

    print_summary(&results);

    // (b) multi-page collapse MUST PASS as well.
    let failures: Vec<_> = results
        .iter()
        .filter(|r| matches!(r.status, CaseStatus::Fail))
        .map(|r| r.label.as_str())
        .collect();

    assert!(
        failures.is_empty(),
        "(b) non-linear collapse MUST PASS for all fixtures, failing: {:?}",
        failures
    );
}

// ---------------------------------------------------------------------------
// Test-of-test: classification of qpdf warnings
// ---------------------------------------------------------------------------

/// Asserts that an *unknown* qpdf warning is classified as Fail, not as a
/// known issue.  Without this discipline the oracle silently swallows
/// regressions — the original concern in flpdf-23w.
#[test]
fn classify_unknown_warning_fails() {
    let r = classify_qpdf_warning("(a) test", "some unrelated qpdf warning text");
    assert!(
        matches!(r.status, CaseStatus::Fail),
        "unknown qpdf warning must be CaseStatus::Fail, got {:?}",
        r.reason
    );
    assert!(
        r.reason.contains("unexpected qpdf warning"),
        "reason text should name it as unexpected, got: {}",
        r.reason
    );
}

/// Asserts that the only currently-tracked warning fingerprint maps to
/// CaseStatus::KnownIssue.
#[test]
fn classify_hint_table_warning_is_known() {
    let r = classify_qpdf_warning(
        "(a) test",
        "WARNING: file: hint table length mismatch detected",
    );
    assert!(
        matches!(r.status, CaseStatus::KnownIssue),
        "the hint-table-length-mismatch fingerprint must remain known"
    );
}
