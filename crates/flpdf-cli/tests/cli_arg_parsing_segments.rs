use assert_cmd::Command;
use std::process::{Command as ProcessCommand, Output};

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

fn qpdf_available() -> bool {
    ProcessCommand::new("qpdf")
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

fn run_qpdf(args: &[&str]) -> Output {
    ProcessCommand::new("qpdf")
        .args(args)
        .output()
        .expect("qpdf should spawn")
}

fn run_flpdf(args: &[&str]) -> Output {
    Command::cargo_bin("flpdf")
        .expect("flpdf binary should build")
        .env("FLPDF_PROGNAME", "qpdf")
        .args(args)
        .output()
        .expect("flpdf should spawn")
}

fn assert_matches_qpdf(args: &[&str], label: &str) {
    let qpdf = run_qpdf(args);
    assert_eq!(qpdf.status.code(), Some(2), "qpdf should reject {label}");

    let flpdf = run_flpdf(args);
    assert_eq!(
        flpdf.status.code(),
        qpdf.status.code(),
        "{label}: exit status; qpdf={:?}, flpdf={:?}",
        qpdf.status,
        flpdf.status
    );
    assert_eq!(flpdf.stdout, qpdf.stdout, "{label}: stdout");
    assert_eq!(
        flpdf.stderr,
        qpdf.stderr,
        "{label}: stderr; got {:?}, expected {:?}",
        String::from_utf8_lossy(&flpdf.stderr),
        String::from_utf8_lossy(&qpdf.stderr)
    );
}

#[test]
fn overlay_and_underlay_segment_errors_follow_qpdf_callbacks() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{EXPECTED_QPDF_VERSION} is required for this parity test on CI");
        }
        eprintln!("skipping: {EXPECTED_QPDF_VERSION} is not available");
        return;
    }

    assert_matches_qpdf(&["--underlay", "--"], "missing underlay filename");
    assert_matches_qpdf(&["--overlay", "x", "x", "--"], "duplicate overlay filename");
}

#[test]
fn pages_segment_errors_follow_qpdf_callbacks_before_final_checks() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{EXPECTED_QPDF_VERSION} is required for this parity test on CI");
        }
        eprintln!("skipping: {EXPECTED_QPDF_VERSION} is not available");
        return;
    }

    let cases = [
        (
            &["--pages", ".", "--pages", ".", "--"][..],
            "unclosed pages after a second pages option",
        ),
        (
            &["--pages", ".", "1", "--xyz", "out"][..],
            "unknown pages option",
        ),
        (
            &["--pages", "--range=1", ".", "--password=z", "--"][..],
            "pages range without a file",
        ),
        (
            &[
                "--pages",
                "--file=.",
                "--range=1",
                "--range=2",
                ".",
                "--password=z",
                "--",
            ][..],
            "duplicate pages range",
        ),
        (
            &["--pages", "--password=z", ".", "1", "--"][..],
            "pages password without a file",
        ),
        (
            &["--pages", ".", "--password=z", "--password=z", "--"][..],
            "duplicate pages password",
        ),
    ];

    for (args, label) in cases {
        assert_matches_qpdf(args, label);
    }
}

#[test]
fn encryption_segment_errors_follow_qpdf_callbacks_before_termination() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{EXPECTED_QPDF_VERSION} is required for this parity test on CI");
        }
        eprintln!("skipping: {EXPECTED_QPDF_VERSION} is not available");
        return;
    }

    let cases = [
        (&["--encrypt", "--"][..], "missing encryption key length"),
        (
            &["--encrypt", "u", "--owner-password=x"][..],
            "mixed positional and dashed encryption arguments",
        ),
        (
            &["--encrypt", "u", "o", "--bits=128"][..],
            "mixed positional and dashed encryption arguments after owner password",
        ),
        (
            &["--encrypt", "--user-password=u", "o"][..],
            "mixed dashed and positional encryption arguments",
        ),
        (
            &[
                "--encrypt",
                "--user-password=u",
                "--owner-password=o",
                "256",
            ][..],
            "mixed dashed and positional encryption key length",
        ),
    ];

    for (args, label) in cases {
        assert_matches_qpdf(args, label);
    }
}

#[test]
fn segment_callbacks_do_not_overtake_earlier_argv_errors() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{EXPECTED_QPDF_VERSION} is required for this parity test on CI");
        }
        eprintln!("skipping: {EXPECTED_QPDF_VERSION} is not available");
        return;
    }

    assert_matches_qpdf(
        &["--bad", "--pages", "--"],
        "unknown top-level option before pages callback",
    );
    assert_matches_qpdf(
        &[
            "--encrypt",
            "u",
            "o",
            "256",
            "--use-aes=y",
            "input.pdf",
            "output.pdf",
        ],
        "unknown option after encryption key-length table switch",
    );
    assert_matches_qpdf(
        &["--empty", "--empty", "--pages", "--"],
        "repeated top-level selector before pages callback",
    );
    assert_matches_qpdf(
        &["--help", "--pages", "--"],
        "help-table error before pages callback",
    );
}
