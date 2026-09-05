//! Top-level `--remove-unreferenced-resources` parity against qpdf 11.9.0.

use assert_cmd::Command;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as ShellCommand, Output};

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(name)
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

fn run_qpdf(args: &[OsString]) -> Output {
    ShellCommand::new("qpdf")
        .args(args)
        .output()
        .expect("qpdf should spawn")
}

fn run_flpdf(args: &[OsString]) -> Output {
    Command::cargo_bin("flpdf")
        .expect("flpdf binary should build")
        .env("FLPDF_PROGNAME", "qpdf")
        .args(args)
        .output()
        .expect("flpdf should spawn")
}

fn normalize_text_newlines(bytes: &[u8]) -> Vec<u8> {
    let mut normalized = Vec::with_capacity(bytes.len());
    let mut remaining = bytes;

    while let Some((&byte, rest)) = remaining.split_first() {
        if byte == b'\r' && rest.first() == Some(&b'\n') {
            normalized.push(b'\n');
            remaining = &rest[1..];
        } else {
            normalized.push(byte);
            remaining = rest;
        }
    }

    normalized
}

fn assert_success(output: &Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn common_flags(mode: &str) -> Vec<OsString> {
    [
        "--static-id".to_owned(),
        "--stream-data=uncompress".to_owned(),
        "--newline-before-endstream=y".to_owned(),
        format!("--remove-unreferenced-resources={mode}"),
    ]
    .into_iter()
    .map(OsString::from)
    .collect()
}

fn split_args(mode: &str, input: &Path, output: &Path) -> Vec<OsString> {
    let mut args = common_flags(mode);
    args.extend([
        OsString::from("--verbose"),
        OsString::from("--split-pages=1"),
        input.as_os_str().to_owned(),
        output.as_os_str().to_owned(),
    ]);
    args
}

fn pages_args(mode: &str, input: &Path, output: &Path) -> Vec<OsString> {
    let mut args = common_flags(mode);
    args.extend([
        input.as_os_str().to_owned(),
        OsString::from("--pages"),
        OsString::from("."),
        OsString::from("1"),
        OsString::from("--"),
        output.as_os_str().to_owned(),
    ]);
    args
}

fn split_chunks(directory: &Path) -> Vec<Vec<u8>> {
    (1..=3)
        .map(|page| {
            fs::read(directory.join(format!("out-{page}.pdf")))
                .unwrap_or_else(|error| panic!("read split chunk {page}: {error}"))
        })
        .collect()
}

#[test]
fn top_level_remove_unreferenced_resources_matches_qpdf_for_split_pages() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("qpdf 11.9.0 is required for this parity test on CI");
        }
        eprintln!("skipping: qpdf 11.9.0 is not available");
        return;
    }

    for mode in ["auto", "no", "yes"] {
        let temp = tempfile::tempdir().expect("temporary directory");
        let input = fixture("three-page.pdf");
        let output = temp.path().join("out-%d.pdf");
        let args = split_args(mode, &input, &output);

        let qpdf = run_qpdf(&args);
        assert_success(&qpdf, "qpdf top-level split-pages resource mode");
        let qpdf_chunks = split_chunks(temp.path());

        let flpdf = run_flpdf(&args);
        assert_success(&flpdf, "flpdf top-level split-pages resource mode");
        assert_eq!(
            normalize_text_newlines(&flpdf.stdout),
            normalize_text_newlines(&qpdf.stdout),
            "top-level split-pages stdout must match qpdf for --remove-unreferenced-resources={mode}"
        );
        assert_eq!(
            normalize_text_newlines(&flpdf.stderr),
            normalize_text_newlines(&qpdf.stderr),
            "top-level split-pages stderr must match qpdf for --remove-unreferenced-resources={mode}"
        );
        assert_eq!(
            split_chunks(temp.path()),
            qpdf_chunks,
            "top-level split-pages bytes must match qpdf for --remove-unreferenced-resources={mode}"
        );
    }
}

#[test]
fn top_level_remove_unreferenced_resources_matches_qpdf_for_pages() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("qpdf 11.9.0 is required for this parity test on CI");
        }
        eprintln!("skipping: qpdf 11.9.0 is not available");
        return;
    }

    for mode in ["auto", "no", "yes"] {
        let temp = tempfile::tempdir().expect("temporary directory");
        let input = fixture("inherited-resources-one-page.pdf");
        let output = temp.path().join("output.pdf");
        let args = pages_args(mode, &input, &output);

        let qpdf = run_qpdf(&args);
        assert_success(&qpdf, "qpdf top-level pages resource mode");
        let qpdf_bytes = fs::read(&output).expect("read qpdf pages output");

        let flpdf = run_flpdf(&args);
        assert_success(&flpdf, "flpdf top-level pages resource mode");
        assert_eq!(
            normalize_text_newlines(&flpdf.stdout),
            normalize_text_newlines(&qpdf.stdout),
            "top-level pages stdout must match qpdf for --remove-unreferenced-resources={mode}"
        );
        assert_eq!(
            normalize_text_newlines(&flpdf.stderr),
            normalize_text_newlines(&qpdf.stderr),
            "top-level pages stderr must match qpdf for --remove-unreferenced-resources={mode}"
        );
        assert_eq!(
            fs::read(&output).expect("read flpdf pages output"),
            qpdf_bytes,
            "top-level pages bytes must match qpdf for --remove-unreferenced-resources={mode}"
        );
    }
}

/// qpdf's `shouldRemoveUnreferencedResources` returns before any verbose
/// report for explicit `yes`/`no` (`QPDFJob.cc:2253-2258`), so the
/// `checking for shared resources` / `no shared resources found` lines must
/// only appear in `auto` mode on the `--pages` route.
#[test]
fn top_level_pages_verbose_preflight_lines_only_appear_in_auto_mode() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let input = fixture("three-page.pdf");

    for (mode, expected) in [("auto", true), ("yes", false), ("no", false)] {
        let output = temp.path().join(format!("{mode}.pdf"));
        let mut args = common_flags(mode);
        args.extend([
            OsString::from("--verbose"),
            input.as_os_str().to_owned(),
            OsString::from("--pages"),
            OsString::from("."),
            OsString::from("1-2"),
            OsString::from("--"),
            output.as_os_str().to_owned(),
        ]);
        let flpdf = run_flpdf(&args);
        assert_success(&flpdf, "flpdf verbose pages resource mode");
        let stdout = String::from_utf8_lossy(&flpdf.stdout);
        assert_eq!(
            stdout.contains("checking for shared resources"),
            expected,
            "mode {mode}: preflight report presence must follow qpdf: {stdout:?}"
        );
        assert_eq!(
            stdout.contains("no shared resources found"),
            expected,
            "mode {mode}: no-shared report presence must follow qpdf: {stdout:?}"
        );
    }
}
