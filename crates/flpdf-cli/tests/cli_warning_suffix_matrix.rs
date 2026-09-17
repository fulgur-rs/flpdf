//! qpdf 11.9.0 parity for `QPDFJob::writeQPDF`'s warning-summary suffix.
//!
//! `QPDFJob::writeQPDF` emits exactly one completion summary when a job
//! finished with warnings, and picks between two spellings through
//! `createsOutput()` (`libqpdf/QPDFJob.cc:493-503`):
//!
//! * `<prefix>: operation succeeded with warnings; resulting file may have
//!   some problems` when the job created output, and
//! * `<prefix>: operation succeeded with warnings` when it did not.
//!
//! `createsOutput()` itself is a query over mutable state — `return
//! ((m->outfilename != nullptr) || m->replace_input)`
//! (`libqpdf/QPDFJob.cc:528-531`) — and `m->outfilename` is rewritten while
//! the job runs:
//!
//! * `checkConfiguration` defaults the JSON destination to `-` when no output
//!   file was given (`libqpdf/QPDFJob.cc:582-585`), so an implicit JSON job
//!   *does* create output for dispatch purposes;
//! * `writeOutfile` replaces the name with a temporary path for
//!   `--replace-input`, and clears it outright when the destination is `-`
//!   (`libqpdf/QPDFJob.cc:3033-3040`);
//! * `writeOutfile` clears it again after the replace-input rename
//!   (`libqpdf/QPDFJob.cc:3062-3065`).
//!
//! Because of those rewrites the same predicate answers differently at
//! `writeQPDF`'s dispatch (`:486`) and at its warning summary (`:497`).
//! Writing to standard output therefore selects the *bare* spelling — not
//! because stdout is special-cased, but because `m->outfilename` has already
//! been cleared by the time the summary is emitted. `--replace-input` keeps
//! the suffixed spelling because `m->replace_input` is still set even after
//! the name is cleared, and `--split-pages` keeps it because `doSplitPages`
//! never touches the name at all.
//!
//! The matrix below pins each observable spelling so that any future
//! reorganisation of the predicate's ownership has to preserve it. The
//! expectations are absolute (fixed against qpdf 11.9.0's observed output) and
//! are additionally cross-checked against the live `qpdf` binary when one is
//! installed.

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::process::{Command as ShellCommand, Output};

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

/// The summary spelling `createsOutput()` selects when the job created output.
const SUFFIXED_SUMMARY: &str =
    ": operation succeeded with warnings; resulting file may have some problems";

/// The summary spelling selected when the job created no output. Note that
/// this is a prefix of [`SUFFIXED_SUMMARY`], so every assertion below compares
/// the complete summary line rather than testing for containment.
const BARE_SUMMARY: &str = ": operation succeeded with warnings";

/// Name the fixture is copied to inside each case's working directory.
///
/// `--replace-input` reports the backup path it kept
/// (`libqpdf/QPDFJob.cc:3076-3079`), so both binaries must be handed the same
/// spelling of the input for their diagnostics to be comparable. Running each
/// invocation in its own directory with a fixed relative name achieves that
/// without depending on the temporary directory's own name.
const INPUT_NAME: &str = "in.pdf";

/// A single-page input whose damaged cross-reference table raises warnings on
/// open, so every route below reaches `writeQPDF`'s warning summary.
///
/// `tests/fixtures/test_driver/repairable_input.pdf` also warns but recovers
/// zero pages, which makes the `--pages` routes fail before the summary.
fn warning_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/xref-whitespace-broken-table.pdf")
}

fn qpdf_available() -> bool {
    ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .is_some_and(|line| line.trim() == EXPECTED_QPDF_VERSION)
        })
        .unwrap_or(false)
}

fn qpdf_or_skip() -> bool {
    if qpdf_available() {
        return true;
    }
    if std::env::var_os("CI").is_some() {
        panic!("qpdf 11.9.0 is required for warning-summary parity tests on CI");
    }
    eprintln!("skipping: qpdf 11.9.0 is not available");
    false
}

/// One command shape and the summary spelling qpdf 11.9.0 selects for it.
struct Case {
    label: &'static str,
    args: &'static [&'static str],
    creates_output: bool,
}

/// Every route that reaches `writeQPDF`'s warning summary, with the spelling
/// observed from qpdf 11.9.0.
const CASES: &[Case] = &[
    // Named output file: `m->outfilename` survives `writeOutfile` untouched.
    Case {
        label: "named output file",
        args: &[INPUT_NAME, "out.pdf"],
        creates_output: true,
    },
    // Standard output: `writeOutfile` clears `m->outfilename`
    // (`QPDFJob.cc:3039-3040`) before the summary is chosen.
    Case {
        label: "standard output",
        args: &[INPUT_NAME, "-"],
        creates_output: false,
    },
    // `m->replace_input` remains set after the name is cleared
    // (`QPDFJob.cc:3062-3065`), so the suffixed spelling still wins.
    Case {
        label: "replace input",
        args: &["--replace-input", INPUT_NAME],
        creates_output: true,
    },
    // Implicit JSON destination: `checkConfiguration` sets `-`
    // (`QPDFJob.cc:582-585`), which `writeOutfile` then clears again.
    Case {
        label: "json to implicit standard output",
        args: &["--json", INPUT_NAME],
        creates_output: false,
    },
    // Explicit JSON destination: the name is neither `-` nor replaced.
    Case {
        label: "json to named file",
        args: &["--json", INPUT_NAME, "out.json"],
        creates_output: true,
    },
    Case {
        label: "json-output to named file",
        args: &["--json-output", INPUT_NAME, "out.json"],
        creates_output: true,
    },
    // `doSplitPages` never rewrites `m->outfilename` (`QPDFJob.cc:486-489`).
    Case {
        label: "split pages",
        args: &["--split-pages", INPUT_NAME, "out.pdf"],
        creates_output: true,
    },
    // No output at all: `writeQPDF` dispatches to `doInspection`.
    Case {
        label: "check inspection",
        args: &["--check", INPUT_NAME],
        creates_output: false,
    },
    Case {
        label: "show-npages inspection",
        args: &["--show-npages", INPUT_NAME],
        creates_output: false,
    },
    // Page-specification routes reach the same summary through
    // `handlePageSpecs` (`QPDFJob.cc:466-469`).
    Case {
        label: "pages to named output file",
        args: &["--pages", ".", "1", "--", INPUT_NAME, "out.pdf"],
        creates_output: true,
    },
    Case {
        label: "pages to standard output",
        args: &["--pages", ".", "1", "--", INPUT_NAME, "-"],
        creates_output: false,
    },
    // The discriminating case for any predicate derived from the output
    // destination alone: no name is given, so only `m->replace_input`
    // distinguishes this from an inspection.
    Case {
        label: "replace input with pages",
        args: &["--replace-input", "--pages", ".", "1", "--", INPUT_NAME],
        creates_output: true,
    },
    Case {
        label: "empty primary with pages to named output file",
        args: &["--empty", "--pages", INPUT_NAME, "1", "--", "out.pdf"],
        creates_output: true,
    },
    Case {
        label: "empty primary with pages to standard output",
        args: &["--empty", "--pages", INPUT_NAME, "1", "--", "-"],
        creates_output: false,
    },
    Case {
        label: "rotate to named output file",
        args: &["--rotate=90", INPUT_NAME, "out.pdf"],
        creates_output: true,
    },
    Case {
        label: "rotate to standard output",
        args: &["--rotate=90", INPUT_NAME, "-"],
        creates_output: false,
    },
];

impl Case {
    /// The complete summary line this case must end with, for `prefix`.
    fn expected_summary(&self, prefix: &str) -> String {
        let spelling = if self.creates_output {
            SUFFIXED_SUMMARY
        } else {
            BARE_SUMMARY
        };
        format!("{prefix}{spelling}")
    }
}

/// A private working directory holding a fresh copy of the fixture.
fn case_directory() -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("tempdir");
    std::fs::copy(warning_fixture(), directory.path().join(INPUT_NAME)).expect("copy fixture");
    directory
}

fn run_flpdf(case: &Case) -> (Output, tempfile::TempDir) {
    let directory = case_directory();
    let output = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .current_dir(directory.path())
        .args(case.args)
        .output()
        .expect("flpdf invocation");
    (output, directory)
}

fn run_qpdf(case: &Case) -> (Output, tempfile::TempDir) {
    let directory = case_directory();
    let output = ShellCommand::new("qpdf")
        .current_dir(directory.path())
        .args(case.args)
        .output()
        .expect("qpdf should spawn");
    (output, directory)
}

/// The final diagnostic line, which `writeQPDF` emits last for every route
/// (the replace-input backup notice precedes it, `QPDFJob.cc:3076-3079`).
fn summary_line(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    text.lines()
        .next_back()
        .unwrap_or_default()
        .trim_end()
        .to_owned()
}

/// Replace a leading program name with a stable marker so qpdf's and flpdf's
/// diagnostics can be compared byte for byte.
fn normalize_prefix(stderr: &[u8], prefix: &str) -> String {
    let needle = format!("{prefix}: ");
    String::from_utf8_lossy(stderr)
        .lines()
        .map(|line| match line.strip_prefix(needle.as_str()) {
            Some(rest) => format!("PROG: {rest}"),
            None => line.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn warning_summary_suffix_matches_qpdf_11_9_0_for_every_output_shape() {
    for case in CASES {
        let (output, _directory) = run_flpdf(case);
        assert_eq!(
            output.status.code(),
            Some(3),
            "{}: a job that finished with warnings exits 3",
            case.label
        );
        assert_eq!(
            summary_line(&output.stderr),
            case.expected_summary("flpdf"),
            "{}: wrong warning-summary spelling",
            case.label
        );
    }
}

#[test]
fn warning_summary_suffix_matrix_covers_both_spellings() {
    // Guard against a future edit that silently drops every case of one
    // spelling and leaves the matrix passing for the wrong reason.
    assert!(
        CASES.iter().any(|case| case.creates_output),
        "matrix must exercise the output-creating spelling"
    );
    assert!(
        CASES.iter().any(|case| !case.creates_output),
        "matrix must exercise the bare spelling"
    );
}

#[test]
fn warning_summary_stderr_matches_live_qpdf_for_every_output_shape() {
    if !qpdf_or_skip() {
        return;
    }

    for case in CASES {
        let (expected, _expected_directory) = run_qpdf(case);
        let (actual, _actual_directory) = run_flpdf(case);

        assert_eq!(
            expected.status.code(),
            actual.status.code(),
            "{}: exit status",
            case.label
        );
        assert_eq!(
            normalize_prefix(&expected.stderr, "qpdf"),
            normalize_prefix(&actual.stderr, "flpdf"),
            "{}: diagnostics",
            case.label
        );
        assert_eq!(
            summary_line(&expected.stderr),
            case.expected_summary("qpdf"),
            "{}: qpdf 11.9.0 no longer selects the pinned spelling",
            case.label
        );
    }
}
