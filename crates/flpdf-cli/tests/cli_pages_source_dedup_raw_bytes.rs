//! qpdf 11.9.0 byte parity for `--pages` source deduplication.
//!
//! qpdf keys its page-spec source map by the raw filename string
//! (`page_spec_qpdfs[page_spec.filename]`, `QPDFJob.cc:2366-2367,2393-2401`)
//! and never canonicalizes it (`QPDFJob.cc:2397`: "Do not canonicalize the
//! file name"). Two page specs whose paths are lexically distinct but refer
//! to the same file on disk (`dir/f.pdf` and `dir/./f.pdf`) are therefore
//! two *different* sources to qpdf, each opened and copied independently.

#![cfg(feature = "qpdf-zlib-compat")]

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::process::{Command as ShellCommand, Output};

const COMPAT: &str = "../../tests/fixtures/compat";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(COMPAT)
        .join(name)
}

fn skip_if_qpdf_missing() -> bool {
    let version = ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .ok()
        .and_then(|output| {
            output
                .status
                .success()
                .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
        });
    if version
        .as_deref()
        .is_some_and(|stdout| stdout.lines().next() == Some("qpdf version 11.9.0"))
    {
        return false;
    }
    if std::env::var_os("CI").is_some() {
        panic!("qpdf 11.9.0 is required for source-dedup parity: {version:?}");
    }
    eprintln!("skipping: qpdf 11.9.0 is not available: {version:?}");
    true
}

fn run_qpdf(args: &[&str]) -> Output {
    ShellCommand::new("qpdf")
        .args(args)
        .output()
        .expect("qpdf should spawn")
}

fn run_flpdf(args: &[&str]) -> Output {
    Command::cargo_bin("flpdf")
        .expect("flpdf binary should build")
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

/// `dir/f.pdf` and `dir/./f.pdf` refer to the same file but are lexically
/// distinct raw strings, so qpdf opens and copies each occurrence as an
/// independent source rather than deduplicating by filesystem identity.
/// Only an annotation-bearing page surfaces the divergence: a bare page
/// clone alone produces the same bytes either way, which is why the
/// original repro for this issue came up empty.
#[test]
fn dotted_relative_path_is_not_deduplicated_with_plain_spelling() {
    if skip_if_qpdf_missing() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("dir");
    std::fs::create_dir(&dir).unwrap();
    let source = dir.join("f.pdf");
    std::fs::copy(fixture("link-annot-no-acroform.pdf"), &source).unwrap();
    let plain = source.to_str().unwrap().to_owned();
    let dotted = dir.join("./f.pdf").to_str().unwrap().to_owned();
    let output = source.to_str().unwrap().to_owned();
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");

    let qpdf = run_qpdf(&[
        "--static-id",
        "--pages",
        &plain,
        "1",
        &dotted,
        "1",
        "--",
        &output,
        qpdf_output.to_str().unwrap(),
    ]);
    assert_success(&qpdf, "qpdf dotted-path source dedup");

    let flpdf = run_flpdf(&[
        "--static-id",
        "--pages",
        &plain,
        "1",
        &dotted,
        "1",
        "--",
        &output,
        flpdf_output.to_str().unwrap(),
    ]);
    assert_success(&flpdf, "flpdf dotted-path source dedup");

    assert_eq!(
        std::fs::read(&flpdf_output).unwrap(),
        std::fs::read(&qpdf_output).unwrap(),
        "dotted vs. plain relative-path specs must not be deduplicated, matching qpdf's raw-string source map"
    );
}

/// The primary input under its own literal path spelling is a distinct
/// source from an explicit `.`-normalized page spec, mirroring the same
/// raw-string identity as the dedup case above.
#[test]
fn explicit_primary_path_spelling_is_not_deduplicated_with_dot() {
    if skip_if_qpdf_missing() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("dir");
    std::fs::create_dir(&dir).unwrap();
    let source = dir.join("f.pdf");
    std::fs::copy(fixture("link-annot-no-acroform.pdf"), &source).unwrap();
    let dotted = dir.join("./f.pdf").to_str().unwrap().to_owned();
    let output = source.to_str().unwrap().to_owned();
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");

    let qpdf = run_qpdf(&[
        "--static-id",
        "--pages",
        ".",
        "1",
        &dotted,
        "1",
        "--",
        &output,
        qpdf_output.to_str().unwrap(),
    ]);
    assert_success(&qpdf, "qpdf primary vs. dotted-path source dedup");

    let flpdf = run_flpdf(&[
        "--static-id",
        "--pages",
        ".",
        "1",
        &dotted,
        "1",
        "--",
        &output,
        flpdf_output.to_str().unwrap(),
    ]);
    assert_success(&flpdf, "flpdf primary vs. dotted-path source dedup");

    assert_eq!(
        std::fs::read(&flpdf_output).unwrap(),
        std::fs::read(&qpdf_output).unwrap(),
        "explicit primary path spelling vs. '.' must not be deduplicated, matching qpdf's raw-string source map"
    );
}
