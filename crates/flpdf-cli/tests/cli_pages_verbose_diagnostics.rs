//! qpdf 11.9.0 differential tests for verbose `--pages` diagnostics.

use assert_cmd::Command;
#[cfg(target_os = "linux")]
use std::ffi::OsString;
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Output};

#[path = "support/text_newlines.rs"]
mod text_newlines;
use text_newlines::normalize_text_newlines;

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

#[cfg(target_os = "linux")]
fn run_qpdf_os(args: &[OsString]) -> Output {
    ProcessCommand::new("qpdf")
        .args(args)
        .output()
        .expect("qpdf should spawn")
}

#[cfg(target_os = "linux")]
fn run_flpdf_os(args: &[OsString]) -> Output {
    ProcessCommand::new(assert_cmd::cargo::cargo_bin!("flpdf"))
        .env("FLPDF_PROGNAME", "qpdf")
        .args(args)
        .output()
        .expect("flpdf should spawn")
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

    let normalized_qpdf_stdout = normalize_text_newlines(&qpdf.stdout);
    let stdout = String::from_utf8_lossy(&normalized_qpdf_stdout);
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
fn verbose_pages_overlay_diagnostics_match_qpdf() {
    if !qpdf_available() {
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let primary = fixture("three-page.pdf");
    let overlay = fixture("one-page.pdf");
    let output = temp.path().join("pages-overlay.pdf");
    let args = vec![
        "--verbose".to_owned(),
        "--static-id".to_owned(),
        primary.to_str().unwrap().to_owned(),
        "--overlay".to_owned(),
        overlay.to_str().unwrap().to_owned(),
        "--".to_owned(),
        "--pages".to_owned(),
        ".".to_owned(),
        "1-2".to_owned(),
        "--".to_owned(),
        output.to_str().unwrap().to_owned(),
    ];

    let qpdf = run_qpdf(&args);
    assert_success(&qpdf, "qpdf verbose pages overlay");
    let flpdf = run_flpdf(&args);
    assert_success(&flpdf, "flpdf verbose pages overlay");

    assert_eq!(
        normalize_text_newlines(&flpdf.stdout),
        normalize_text_newlines(&qpdf.stdout),
        "verbose --pages --overlay stdout must match qpdf"
    );
    assert_eq!(
        normalize_text_newlines(&flpdf.stderr),
        normalize_text_newlines(&qpdf.stderr),
        "verbose --pages --overlay stderr must match qpdf"
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

#[test]
fn rewrite_pages_split_keeps_write_time_password_notice_after_split_preflight() {
    if !qpdf_available() {
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let primary = fixture("three-page.pdf");
    let output = temp.path().join("rewrite-pages-%d.pdf");
    let primary = primary.to_str().unwrap().to_owned();
    let output = output.to_str().unwrap().to_owned();
    let qpdf_args = vec![
        "--verbose".to_owned(),
        "--allow-weak-crypto".to_owned(),
        "--encrypt".to_owned(),
        "café".to_owned(),
        "owner".to_owned(),
        "128".to_owned(),
        "--".to_owned(),
        primary.clone(),
        "--pages".to_owned(),
        primary.clone(),
        "1".to_owned(),
        "--".to_owned(),
        "--split-pages=1".to_owned(),
        output.clone(),
    ];
    let mut flpdf_args = vec!["rewrite".to_owned()];
    flpdf_args.extend(qpdf_args.iter().cloned());

    let qpdf = run_qpdf(&qpdf_args);
    assert_success(&qpdf, "qpdf rewrite-equivalent pages then split-pages");
    let flpdf = run_flpdf(&flpdf_args);
    assert_success(&flpdf, "flpdf rewrite pages then split-pages");

    let without_destination = |bytes: &[u8]| {
        String::from_utf8_lossy(&normalize_text_newlines(bytes))
            .lines()
            .filter(|line| !line.contains(": wrote file "))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        without_destination(&flpdf.stdout),
        without_destination(&qpdf.stdout),
        "rewrite --pages --split-pages stdout must keep qpdf's write-time notice order"
    );
    for stdout in [&qpdf.stdout, &flpdf.stdout] {
        assert!(
            String::from_utf8_lossy(stdout).contains("rewrite-pages-1.pdf"),
            "split output report must name the generated chunk: {:?}",
            String::from_utf8_lossy(stdout)
        );
    }
    assert_eq!(
        normalize_text_newlines(&flpdf.stderr),
        normalize_text_newlines(&qpdf.stderr),
        "rewrite --pages --split-pages stderr must match qpdf"
    );
}

#[test]
fn rewrite_pages_split_opens_output_before_weak_crypto_validation() {
    if !qpdf_available() {
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let input = fixture("three-page.pdf");
    let output = temp.path().join("missing-directory").join("pages-%d.pdf");
    let input = input.to_str().unwrap().to_owned();
    let output = output.to_str().unwrap().to_owned();
    let qpdf_args = vec![
        "--encrypt".to_owned(),
        "user".to_owned(),
        "owner".to_owned(),
        "128".to_owned(),
        "--".to_owned(),
        input.clone(),
        "--pages".to_owned(),
        input.clone(),
        "1".to_owned(),
        "--".to_owned(),
        "--split-pages=1".to_owned(),
        output,
    ];
    let mut flpdf_args = vec!["rewrite".to_owned()];
    flpdf_args.extend(qpdf_args.iter().cloned());

    let qpdf = run_qpdf(&qpdf_args);
    let flpdf = run_flpdf(&flpdf_args);

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(
        normalize_text_newlines(&flpdf.stdout),
        normalize_text_newlines(&qpdf.stdout),
        "output-open failure must not emit split diagnostics"
    );
    assert_eq!(
        normalize_text_newlines(&flpdf.stderr),
        normalize_text_newlines(&qpdf.stderr),
        "output-open failure must precede qpdf's weak-crypto validation"
    );
    assert!(!String::from_utf8_lossy(&flpdf.stderr).contains("weak cryptographic algorithm"));
}

#[test]
fn rewrite_pages_ordinary_output_opens_before_weak_crypto_validation() {
    if !qpdf_available() {
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let input = fixture("three-page.pdf");
    let output = temp.path().join("missing-directory").join("ordinary.pdf");
    let input = input.to_str().unwrap().to_owned();
    let output = output.to_str().unwrap().to_owned();
    let qpdf_args = vec![
        "--encrypt".to_owned(),
        "user".to_owned(),
        "owner".to_owned(),
        "128".to_owned(),
        "--".to_owned(),
        input.clone(),
        "--pages".to_owned(),
        input.clone(),
        "1".to_owned(),
        "--".to_owned(),
        output,
    ];
    let mut flpdf_args = vec!["rewrite".to_owned()];
    flpdf_args.extend(qpdf_args.iter().cloned());

    let qpdf = run_qpdf(&qpdf_args);
    let flpdf = run_flpdf(&flpdf_args);

    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(
        normalize_text_newlines(&flpdf.stdout),
        normalize_text_newlines(&qpdf.stdout),
        "ordinary output-open failure must not emit writer diagnostics"
    );
    assert_eq!(
        normalize_text_newlines(&flpdf.stderr),
        normalize_text_newlines(&qpdf.stderr),
        "ordinary output-open failure must precede qpdf's weak-crypto validation"
    );
    assert!(!String::from_utf8_lossy(&flpdf.stderr).contains("weak cryptographic algorithm"));
}

#[test]
fn rewrite_pages_ordinary_output_uses_the_canonical_job_writer_route() {
    let source = include_str!("../src/main.rs");
    let start = source
        .find("fn run_page_extraction_after_plan")
        .expect("page extraction completion route should remain named");
    let body = source[start..]
        .split_once("\n/// Parse `--split-pages")
        .expect("split parser should follow the page extraction route")
        .0;

    assert!(
        body.matches("write_qpdf(").count() >= 2,
        "ordinary and split page outputs must use the Job writer boundary"
    );
    assert!(
        !body.contains("write_with_pdf_writer("),
        "page extraction must not retain a direct PdfWriter output route"
    );
}

#[cfg(target_os = "linux")]
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

#[test]
fn rewrite_empty_pages_repeated_collated_sources_match_qpdf() {
    if !qpdf_available() {
        return;
    }

    let temp = tempfile::tempdir().expect("temporary empty-pages directory");
    let first = fixture("three-page.pdf");
    let second = fixture("inherited-resources-one-page.pdf");
    let output = temp.path().join("empty-pages-qdf.pdf");
    let first = first.to_str().unwrap().to_owned();
    let second = second.to_str().unwrap().to_owned();
    let output = output.to_str().unwrap().to_owned();

    // The repeated first source exercises qpdf's literal source identity and
    // the collate grouping; verbose source-preflight stdout/stderr parity for
    // the inherited-resource source is covered by
    // `verbose_empty_pages_source_preflights_match_qpdf`.
    // --qdf and --static-id exercise writer configuration without comparing
    // serializer-specific bytes.
    let qpdf_args = vec![
        "--verbose".to_owned(),
        "--static-id".to_owned(),
        "--qdf".to_owned(),
        "--collate=2".to_owned(),
        "--empty".to_owned(),
        "--pages".to_owned(),
        first.clone(),
        "1".to_owned(),
        second.clone(),
        "1".to_owned(),
        first.clone(),
        "2".to_owned(),
        "--".to_owned(),
        output.clone(),
    ];
    let qpdf = run_qpdf(&qpdf_args);
    assert_success(&qpdf, "qpdf repeated/collated empty pages");
    let qpdf_pages = run_qpdf(&["--show-pages".to_owned(), output.clone()]);
    assert_success(&qpdf_pages, "qpdf repeated/collated empty-pages output");

    let flpdf_args = vec![
        "rewrite".to_owned(),
        temp.path().join("unused-input.pdf").display().to_string(),
        output.clone(),
        "--verbose".to_owned(),
        "--static-id".to_owned(),
        "--qdf".to_owned(),
        "--collate=2".to_owned(),
        "--empty".to_owned(),
        "--pages".to_owned(),
        first,
        "1".to_owned(),
        second,
        "1".to_owned(),
        fixture("three-page.pdf").to_str().unwrap().to_owned(),
        "2".to_owned(),
        "--".to_owned(),
    ];
    let flpdf = run_flpdf(&flpdf_args);
    assert_success(&flpdf, "flpdf rewrite repeated/collated empty pages");
    let flpdf_pages = run_qpdf(&["--show-pages".to_owned(), output]);
    assert_success(&flpdf_pages, "flpdf repeated/collated empty-pages output");

    assert_eq!(
        normalize_text_newlines(&flpdf_pages.stdout),
        normalize_text_newlines(&qpdf_pages.stdout),
        "rewrite --empty --pages repeated/collated page layout must match qpdf"
    );
}
