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

#[test]
fn top_level_preserve_unreferenced_resources_synonym_matches_qpdf_for_pages() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("qpdf 11.9.0 is required for this parity test on CI");
        }
        eprintln!("skipping: qpdf 11.9.0 is not available");
        return;
    }

    let temp = tempfile::tempdir().expect("temporary directory");
    let input = fixture("inherited-resources-one-page.pdf");
    let output = temp.path().join("output.pdf");
    // qpdf's argument grammar accepts a single leading dash for any long
    // option, and this synonym is no exception.
    for spelling in [
        "--preserve-unreferenced-resources",
        "-preserve-unreferenced-resources",
    ] {
        let args = vec![
            OsString::from("--static-id"),
            OsString::from("--stream-data=uncompress"),
            OsString::from("--newline-before-endstream=y"),
            OsString::from(spelling),
            input.as_os_str().to_owned(),
            OsString::from("--pages"),
            OsString::from("."),
            OsString::from("1"),
            OsString::from("--"),
            output.as_os_str().to_owned(),
        ];

        let qpdf = run_qpdf(&args);
        assert_success(&qpdf, spelling);
        let qpdf_bytes = fs::read(&output).expect("read qpdf pages output");

        let flpdf = run_flpdf(&args);
        assert_success(&flpdf, spelling);
        assert_eq!(
            normalize_text_newlines(&flpdf.stdout),
            normalize_text_newlines(&qpdf.stdout),
            "{spelling}: stdout must match qpdf"
        );
        assert_eq!(
            normalize_text_newlines(&flpdf.stderr),
            normalize_text_newlines(&qpdf.stderr),
            "{spelling}: stderr must match qpdf"
        );
        assert_eq!(
            fs::read(&output).expect("read flpdf pages output"),
            qpdf_bytes,
            "{spelling}: output bytes must match qpdf's --remove-unreferenced-resources=no"
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

/// A resource dictionary whose entry resolves to null.
///
/// `QPDF_Dictionary::getKeys` omits null-valued keys
/// (`libqpdf/QPDF_Dictionary.cc:117-127`), and
/// `removeUnreferencedResourcesHelper` builds both `known_names` and its
/// removal candidate set from `dict.getKeys()`
/// (`libqpdf/QPDFPageObjectHelper.cc:581-583,590-593`). A null-valued
/// `/Font` entry is therefore never a removal candidate on either side, and
/// the content stream's reference to it never counts as resolved.
///
/// No compat fixture carries this shape, so this is the only coverage for
/// that boundary.
fn null_valued_font_entry_pdf() -> Vec<u8> {
    let objects: &[(u32, &str)] = &[
        (1, "<< /Type /Catalog /Pages 2 0 R >>"),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
             /Resources << /Font << /F1 null /F2 5 0 R /F3 6 0 R >> >> >>",
        ),
        (
            4,
            "<< /Length 33 >>\nstream\nBT /F1 12 Tf 10 10 Td (hi) Tj ET\nendstream",
        ),
        (5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
        (6, "<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>"),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = std::collections::BTreeMap::new();
    for (number, body) in objects {
        offsets.insert(*number, out.len());
        out.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let xref_start = out.len();
    out.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
    for offset in offsets.values() {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    out
}

#[test]
fn remove_unreferenced_resources_matches_qpdf_for_a_null_valued_entry() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("qpdf 11.9.0 is required for this parity test on CI");
        }
        eprintln!("skipping: qpdf 11.9.0 is not available");
        return;
    }

    for mode in ["auto", "no", "yes"] {
        let temp = tempfile::tempdir().expect("temporary directory");
        let input = temp.path().join("null-font-entry.pdf");
        fs::write(&input, null_valued_font_entry_pdf()).expect("write input");

        let qpdf_output = temp.path().join("qpdf.pdf");
        let mut args = common_flags(mode);
        args.extend([
            input.as_os_str().to_owned(),
            qpdf_output.as_os_str().to_owned(),
        ]);
        let qpdf = run_qpdf(&args);

        let flpdf_output = temp.path().join("flpdf.pdf");
        let mut args = common_flags(mode);
        args.extend([
            input.as_os_str().to_owned(),
            flpdf_output.as_os_str().to_owned(),
        ]);
        let flpdf = run_flpdf(&args);

        assert_eq!(
            flpdf.status.code(),
            qpdf.status.code(),
            "exit status must match qpdf for --remove-unreferenced-resources={mode}"
        );
        assert_eq!(
            normalize_text_newlines(&flpdf.stderr),
            normalize_text_newlines(&qpdf.stderr),
            "stderr must match qpdf for --remove-unreferenced-resources={mode}"
        );
        assert_eq!(
            fs::read(&flpdf_output).expect("flpdf output"),
            fs::read(&qpdf_output).expect("qpdf output"),
            "output must match qpdf for --remove-unreferenced-resources={mode}"
        );
    }
}
