//! qpdf 11.9.0 differential tests for verbose `--pages` diagnostics.

use assert_cmd::Command;
use std::ffi::OsString;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Output};

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(name)
}

fn qpdf_available() -> bool {
    let output = match ProcessCommand::new("qpdf").arg("--version").output() {
        Ok(output) => output,
        Err(error) => {
            if std::env::var_os("CI").is_some() {
                panic!("qpdf 11.9.0 is required on CI: {error}");
            }
            eprintln!("skipping: qpdf 11.9.0 is unavailable: {error}");
            return false;
        }
    };
    let version = String::from_utf8_lossy(&output.stdout);
    if output.status.success() && version.lines().next() == Some(EXPECTED_QPDF_VERSION) {
        return true;
    }
    if std::env::var_os("CI").is_some() {
        panic!(
            "qpdf 11.9.0 is required on CI; found {:?}",
            version.lines().next()
        );
    }
    eprintln!(
        "skipping: qpdf 11.9.0 is required; found {:?}",
        version.lines().next()
    );
    false
}

fn run_qpdf(args: &[String]) -> Output {
    ProcessCommand::new("qpdf")
        .args(args)
        .output()
        .expect("qpdf should spawn")
}

fn run_flpdf(args: &[String]) -> Output {
    Command::cargo_bin("flpdf")
        .expect("flpdf binary should build")
        .env("FLPDF_PROGNAME", "qpdf")
        .args(args)
        .output()
        .expect("flpdf should spawn")
}

#[cfg(unix)]
fn run_qpdf_os(args: &[OsString]) -> Output {
    ProcessCommand::new("qpdf")
        .args(args)
        .output()
        .expect("qpdf should spawn")
}

#[cfg(unix)]
fn run_flpdf_os(args: &[OsString]) -> Output {
    ProcessCommand::new(assert_cmd::cargo::cargo_bin!("flpdf"))
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

#[test]
fn verbose_pages_multi_source_diagnostics_match_qpdf() {
    if !qpdf_available() {
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let primary = fixture("three-page.pdf");
    let secondary = fixture("inherited-resources-one-page.pdf");
    let output = temp.path().join("pages.pdf");
    let args = vec![
        "--verbose".to_owned(),
        "--static-id".to_owned(),
        primary.to_str().unwrap().to_owned(),
        "--pages".to_owned(),
        primary.to_str().unwrap().to_owned(),
        "1".to_owned(),
        secondary.to_str().unwrap().to_owned(),
        "--".to_owned(),
        output.to_str().unwrap().to_owned(),
    ];

    let qpdf = run_qpdf(&args);
    assert_success(&qpdf, "qpdf verbose multi-source pages");
    let flpdf = run_flpdf(&args);
    assert_success(&flpdf, "flpdf verbose multi-source pages");

    assert_eq!(
        normalize_text_newlines(&flpdf.stdout),
        normalize_text_newlines(&qpdf.stdout),
        "verbose multi-source --pages stdout must match qpdf"
    );
    assert_eq!(
        normalize_text_newlines(&flpdf.stderr),
        normalize_text_newlines(&qpdf.stderr),
        "verbose multi-source --pages stderr must match qpdf"
    );

    let stdout = String::from_utf8_lossy(&qpdf.stdout);
    let secondary_path = secondary.to_str().unwrap();
    assert!(
        stdout.contains(&format!(
            "qpdf: {secondary_path}: checking for shared resources\n  found resources in non-leaf page node"
        )),
        "qpdf must report the real shared-resource finding: {stdout:?}"
    );
    assert!(
        stdout.contains(&format!(
            "qpdf: {primary_path}: checking for shared resources\nqpdf: no shared resources found",
            primary_path = primary.to_str().unwrap()
        )),
        "qpdf must report the primary preflight: {stdout:?}"
    );
}

#[test]
fn verbose_pages_single_source_reports_the_real_preflight() {
    if !qpdf_available() {
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let input = fixture("inherited-resources-one-page.pdf");
    let output = temp.path().join("single.pdf");
    let input = input.to_str().unwrap().to_owned();
    let output = output.to_str().unwrap().to_owned();
    let args = vec![
        "--verbose".to_owned(),
        "--static-id".to_owned(),
        input.clone(),
        "--pages".to_owned(),
        input.clone(),
        "1".to_owned(),
        "--".to_owned(),
        output,
    ];

    let qpdf = run_qpdf(&args);
    assert_success(&qpdf, "qpdf verbose single-source pages");
    let flpdf = run_flpdf(&args);
    assert_success(&flpdf, "flpdf verbose single-source pages");

    assert_eq!(
        normalize_text_newlines(&flpdf.stdout),
        normalize_text_newlines(&qpdf.stdout),
        "verbose single-source --pages stdout must match qpdf"
    );
    assert_eq!(
        normalize_text_newlines(&flpdf.stderr),
        normalize_text_newlines(&qpdf.stderr),
        "verbose single-source --pages stderr must match qpdf"
    );
}

#[test]
fn verbose_pages_split_reports_merge_and_split_preflights_like_qpdf() {
    if !qpdf_available() {
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let primary = fixture("three-page.pdf");
    let secondary = fixture("inherited-resources-one-page.pdf");
    let output = temp.path().join("pages-%d.pdf");
    let args = vec![
        "--verbose".to_owned(),
        "--static-id".to_owned(),
        primary.to_str().unwrap().to_owned(),
        "--pages".to_owned(),
        primary.to_str().unwrap().to_owned(),
        "1".to_owned(),
        secondary.to_str().unwrap().to_owned(),
        "--".to_owned(),
        "--split-pages=1".to_owned(),
        output.to_str().unwrap().to_owned(),
    ];

    let qpdf = run_qpdf(&args);
    assert_success(&qpdf, "qpdf verbose pages then split-pages");
    let flpdf = run_flpdf(&args);
    assert_success(&flpdf, "flpdf verbose pages then split-pages");

    assert_eq!(
        normalize_text_newlines(&flpdf.stdout),
        normalize_text_newlines(&qpdf.stdout),
        "verbose --pages --split-pages stdout must match qpdf"
    );
    assert_eq!(
        normalize_text_newlines(&flpdf.stderr),
        normalize_text_newlines(&qpdf.stderr),
        "verbose --pages --split-pages stderr must match qpdf"
    );
}

#[cfg(unix)]
#[test]
fn verbose_pages_preserves_non_utf8_source_and_output_path_bytes() {
    if !qpdf_available() {
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let primary = temp
        .path()
        .join(OsString::from_vec(b"primary-\xff.pdf".to_vec()));
    let secondary = temp
        .path()
        .join(OsString::from_vec(b"secondary-\xfe.pdf".to_vec()));
    let output = temp
        .path()
        .join(OsString::from_vec(b"output-\xfd.pdf".to_vec()));
    std::fs::copy(fixture("three-page.pdf"), &primary).expect("copy primary fixture");
    std::fs::copy(fixture("inherited-resources-one-page.pdf"), &secondary)
        .expect("copy secondary fixture");

    let args = vec![
        OsString::from("--verbose"),
        OsString::from("--static-id"),
        primary.as_os_str().to_os_string(),
        OsString::from("--pages"),
        primary.as_os_str().to_os_string(),
        OsString::from("1"),
        secondary.as_os_str().to_os_string(),
        OsString::from("--"),
        output.as_os_str().to_os_string(),
    ];

    let qpdf = run_qpdf_os(&args);
    assert_success(&qpdf, "qpdf verbose non-UTF-8 pages");
    let flpdf = run_flpdf_os(&args);
    assert_success(&flpdf, "flpdf verbose non-UTF-8 pages");

    assert_eq!(
        normalize_text_newlines(&flpdf.stdout),
        normalize_text_newlines(&qpdf.stdout),
        "verbose non-UTF-8 --pages stdout must preserve raw path bytes"
    );
    assert_eq!(
        normalize_text_newlines(&flpdf.stderr),
        normalize_text_newlines(&qpdf.stderr),
        "verbose non-UTF-8 --pages stderr must match qpdf"
    );
    assert!(
        qpdf.stdout
            .windows(b"output-\xfd.pdf".len())
            .any(|window| window == b"output-\xfd.pdf"),
        "qpdf output must contain the raw output path bytes: {:?}",
        qpdf.stdout
    );
}

#[test]
fn verbose_empty_pages_source_preflights_match_qpdf() {
    if !qpdf_available() {
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let first = fixture("three-page.pdf");
    let second = fixture("inherited-resources-one-page.pdf");
    let output = temp.path().join("empty-pages.pdf");
    let args = vec![
        "--verbose".to_owned(),
        "--static-id".to_owned(),
        "--empty".to_owned(),
        "--pages".to_owned(),
        first.to_str().unwrap().to_owned(),
        "1".to_owned(),
        second.to_str().unwrap().to_owned(),
        "1".to_owned(),
        "--".to_owned(),
        output.to_str().unwrap().to_owned(),
    ];

    let qpdf = run_qpdf(&args);
    assert_success(&qpdf, "qpdf verbose empty pages");
    let flpdf = run_flpdf(&args);
    assert_success(&flpdf, "flpdf verbose empty pages");

    assert_eq!(
        normalize_text_newlines(&flpdf.stdout),
        normalize_text_newlines(&qpdf.stdout),
        "verbose --empty --pages stdout must match qpdf"
    );
    assert_eq!(
        normalize_text_newlines(&flpdf.stderr),
        normalize_text_newlines(&qpdf.stderr),
        "verbose --empty --pages stderr must match qpdf"
    );
}
